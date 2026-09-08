//! Cache block bookkeeping, deliberately in its own database.
//!
//! Two reasons, both measured. A commit by any connection invalidates the
//! private page cache of every other connection to the same database file: on a
//! 95 MB index with a cold operating-system cache, three hundred repeat lookups
//! cost 0.93 ms before a foreign commit and 20.63 ms after a single one. The
//! content cache commits once per published 4 MiB block, so keeping this table
//! in the metadata database made cached navigation read cold for the whole of a
//! download. Second, this index is derived state: `reconcile` rebuilds it from
//! the cache directory at every mount, so it needs neither the durability nor
//! the migration that the metadata index does.
use crate::{Result, timestamp};
use rusqlite::{Connection, params};
use std::{path::Path, time::Duration};

pub struct BlockIndex {
    db: Connection,
}
impl BlockIndex {
    /// Call from a blocking worker. Hold one open for the process lifetime so
    /// that per-operation connections never checkpoint and unlink the log.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let db = Connection::open(path)?;
        db.busy_timeout(Duration::from_secs(3))?;
        // Losing the most recent rows to a power failure costs nothing: the
        // blocks themselves are re-downloadable and `reconcile` re-indexes
        // whatever the cache directory actually holds.
        db.execute_batch("PRAGMA journal_mode=WAL;")?;
        db.execute_batch(
            "PRAGMA synchronous=NORMAL; PRAGMA journal_size_limit=16777216;
            CREATE TABLE IF NOT EXISTS cache_blocks (
                key TEXT PRIMARY KEY, size INTEGER NOT NULL, touched INTEGER NOT NULL);
            CREATE INDEX IF NOT EXISTS cache_block_age ON cache_blocks(touched);",
        )?;
        Ok(Self { db })
    }
    pub fn touch(&mut self, key: &str, size: u64) -> Result<()> {
        self.db.execute(
            "INSERT INTO cache_blocks(key,size,touched) VALUES(?1,?2,?3)
            ON CONFLICT(key) DO UPDATE SET touched=excluded.touched,size=excluded.size",
            params![key, size as i64, timestamp()],
        )?;
        Ok(())
    }
    /// Oldest first, so a caller can evict in order without sorting.
    pub fn oldest(&self) -> Result<Vec<(String, u64)>> {
        let mut query = self
            .db
            .prepare("SELECT key,size FROM cache_blocks ORDER BY touched")?;
        Ok(query
            .query_map([], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u64)))?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }
    pub fn forget(&mut self, key: &str) -> Result<()> {
        self.db
            .execute("DELETE FROM cache_blocks WHERE key=?1", [key])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
