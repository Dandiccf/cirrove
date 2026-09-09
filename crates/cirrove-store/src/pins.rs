//! Durable record of what the user asked to keep available offline.
//!
//! This belongs in the metadata database and not beside the block index, which
//! is derived state that `reconcile` rebuilds from the cache directory at every
//! mount. A pin is not derived from anything: it is an instruction, and losing
//! it silently would take content offline that somebody asked to have.
//!
//! A pin reserves bytes against the same budget the cache evicts against. The
//! reservation is recorded when the pin is made rather than computed later, so
//! that a request which cannot fit is refused while the caller is still there to
//! be told, instead of being accepted and then quietly evicted.
use crate::{Result, Store, timestamp};
use rusqlite::{Connection, params};

/// A pin's own record. `reserved` is what it claims from the cache budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub scope: String,
    pub item: String,
    pub recursive: bool,
    pub reserved: u64,
    pub created: i64,
}
/// Why a pin was refused. Callers report these; they are not internal errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinRefusal {
    /// The reservation does not fit in what the budget leaves unreserved.
    WouldExceedBudget { requested: u64, available: u64 },
}
impl std::fmt::Display for PinRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WouldExceedBudget {
                requested,
                available,
            } => write!(
                f,
                "pinning needs {requested} bytes and only {available} are unreserved; \
                 unpin something or raise the cache budget"
            ),
        }
    }
}
pub(super) fn migrate(tx: &rusqlite::Transaction<'_>, version: u32) -> Result<()> {
    if version < 7 {
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS pins (
                scope TEXT NOT NULL, item TEXT NOT NULL,
                recursive INTEGER NOT NULL, reserved INTEGER NOT NULL,
                created INTEGER NOT NULL, PRIMARY KEY(scope,item));
            CREATE TABLE IF NOT EXISTS pin_blocks (
                key TEXT PRIMARY KEY, scope TEXT NOT NULL, item TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS pin_block_owner ON pin_blocks(scope,item);",
        )?;
    }
    Ok(())
}
pub(super) fn validate(db: &Connection) -> Result<()> {
    db.prepare("SELECT scope,item,recursive,reserved,created FROM pins")?;
    db.prepare("SELECT key FROM pin_blocks")?;
    Ok(())
}
impl Store {
    /// Bytes claimed by every pin, whether or not their content is cached yet.
    ///
    /// Reserved rather than resident on purpose: the point of a reservation is
    /// that the space is spoken for before the download happens, so two pins
    /// cannot each be told there is room for them.
    pub fn reserved_bytes(&self) -> Result<u64> {
        Ok(self
            .db
            .query_row("SELECT COALESCE(SUM(reserved),0) FROM pins", [], |r| {
                r.get::<_, i64>(0)
            })? as u64)
    }
    /// Record a pin, refusing one the budget cannot hold.
    ///
    /// `budget` is the cache quota. Re-pinning an item replaces its reservation,
    /// and its own current reservation does not count against it, so raising a
    /// pin from 1 GiB to 2 GiB needs only the extra gigabyte to be free.
    pub fn pin(
        &mut self,
        scope: &str,
        item: &str,
        recursive: bool,
        reserved: u64,
        budget: u64,
    ) -> Result<std::result::Result<Pin, PinRefusal>> {
        let existing: u64 = self.db.query_row(
            "SELECT COALESCE(SUM(reserved),0) FROM pins WHERE NOT (scope=?1 AND item=?2)",
            params![scope, item],
            |r| r.get::<_, i64>(0),
        )? as u64;
        let available = budget.saturating_sub(existing);
        if reserved > available {
            return Ok(Err(PinRefusal::WouldExceedBudget {
                requested: reserved,
                available,
            }));
        }
        let created = timestamp();
        self.db.execute(
            "INSERT INTO pins(scope,item,recursive,reserved,created) VALUES(?1,?2,?3,?4,?5)
            ON CONFLICT(scope,item) DO UPDATE SET
                recursive=excluded.recursive, reserved=excluded.reserved",
            params![scope, item, recursive, reserved as i64, created],
        )?;
        Ok(Ok(Pin {
            scope: scope.into(),
            item: item.into(),
            recursive,
            reserved,
            created,
        }))
    }
    /// Remove a pin and release the blocks it protected. Returns whether one existed.
    pub fn unpin(&mut self, scope: &str, item: &str) -> Result<bool> {
        let tx = self.db.transaction()?;
        tx.execute(
            "DELETE FROM pin_blocks WHERE scope=?1 AND item=?2",
            params![scope, item],
        )?;
        let removed = tx.execute(
            "DELETE FROM pins WHERE scope=?1 AND item=?2",
            params![scope, item],
        )?;
        tx.commit()?;
        Ok(removed > 0)
    }
    pub fn pins(&self) -> Result<Vec<Pin>> {
        let mut query = self
            .db
            .prepare("SELECT scope,item,recursive,reserved,created FROM pins ORDER BY created")?;
        Ok(query
            .query_map([], |r| {
                Ok(Pin {
                    scope: r.get(0)?,
                    item: r.get(1)?,
                    recursive: r.get(2)?,
                    reserved: r.get::<_, i64>(3)? as u64,
                    created: r.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }
    /// Record the cache blocks a pinned item owns, so eviction can skip them.
    pub fn protect_blocks(&mut self, scope: &str, item: &str, keys: &[String]) -> Result<()> {
        let tx = self.db.transaction()?;
        tx.execute(
            "DELETE FROM pin_blocks WHERE scope=?1 AND item=?2",
            params![scope, item],
        )?;
        {
            let mut insert =
                tx.prepare("INSERT OR REPLACE INTO pin_blocks(key,scope,item) VALUES(?1,?2,?3)")?;
            for key in keys {
                insert.execute(params![key, scope, item])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
    /// Every block eviction must leave alone.
    pub fn protected_blocks(&self) -> Result<std::collections::HashSet<String>> {
        let mut query = self.db.prepare("SELECT key FROM pin_blocks")?;
        Ok(query
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<std::collections::HashSet<_>, _>>()?)
    }
}

#[cfg(test)]
mod tests;
