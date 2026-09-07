//! Coalesced local namespace changes, read at a committed database frontier.
//! One marker per object bounds storage independently of the number of writes.
use super::*;

#[derive(Clone, Debug)]
pub struct NamespaceSnapshot {
    pub object: NamespaceObject,
    pub working: Option<WorkingFile>,
}

#[derive(Debug)]
pub struct NamespacePublication {
    pub after: u64,
    pub through: u64,
    pub objects: Vec<NamespaceSnapshot>,
}

pub(super) fn migrate(db: &mut Connection, version: u32) -> Result<()> {
    if version < 12 {
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS namespace_clock(
                singleton INTEGER PRIMARY KEY CHECK(singleton=1),
                value INTEGER NOT NULL CHECK(typeof(value)='integer' AND value>=0));
             INSERT OR IGNORE INTO namespace_clock VALUES(1,0);
             CREATE TABLE IF NOT EXISTS namespace_changes(
                object TEXT PRIMARY KEY, sequence INTEGER NOT NULL);
             CREATE INDEX IF NOT EXISTS namespace_changes_sequence ON namespace_changes(sequence);
             DROP TRIGGER IF EXISTS namespace_publish_insert;
             DROP TRIGGER IF EXISTS namespace_publish_update;
             UPDATE namespace_clock SET value=value+1;
             DELETE FROM namespace_changes;
             INSERT INTO namespace_changes
                SELECT id,(SELECT value FROM namespace_clock) FROM namespace_objects;
             CREATE TRIGGER namespace_publish_insert AFTER INSERT ON namespace_objects BEGIN
                UPDATE namespace_clock SET value=value+1;
                INSERT INTO namespace_changes VALUES(NEW.id,(SELECT value FROM namespace_clock))
                    ON CONFLICT(object) DO UPDATE SET sequence=excluded.sequence;
             END;
             CREATE TRIGGER namespace_publish_update AFTER UPDATE ON namespace_objects BEGIN
                UPDATE namespace_clock SET value=value+1;
                INSERT INTO namespace_changes VALUES(NEW.id,(SELECT value FROM namespace_clock))
                    ON CONFLICT(object) DO UPDATE SET sequence=excluded.sequence;
             END;",
        )?;
        tx.pragma_update(None, "user_version", 12)?;
        tx.commit()?;
    }
    db.prepare("SELECT object,sequence FROM namespace_changes INDEXED BY namespace_changes_sequence LIMIT 0")?;
    let value: i64 = db.query_row(
        "SELECT value FROM namespace_clock WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    if value < 0 {
        return Err(JournalError::Corrupt);
    }
    let triggers: u32 = db.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE type='trigger' AND tbl_name='namespace_objects'
            AND name IN ('namespace_publish_insert','namespace_publish_update')",
        [],
        |r| r.get(0),
    )?;
    if triggers != 2 {
        return Err(JournalError::Corrupt);
    }
    Ok(())
}

impl UploadJournal {
    /// Return every changed object at one committed frontier, with its current
    /// working metadata. Do not paginate this result: a page could split a
    /// replacement transaction, or miss a later transfer of the same binding.
    /// Coalescing and the existing 10,000-object journal limit bound the result;
    /// the sequence index avoids rereading unchanged objects on ordinary saves.
    /// A consumer advances its cursor only after publishing the entire batch.
    pub fn namespace_publication(&self, after: u64) -> Result<NamespacePublication> {
        let after_sql = i64::try_from(after).map_err(|_| JournalError::Stale)?;
        let tx = self.db.unchecked_transaction()?;
        let through: i64 = tx.query_row(
            "SELECT value FROM namespace_clock WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        let through = u64::try_from(through).map_err(|_| JournalError::Corrupt)?;
        if after > through {
            return Err(JournalError::Stale);
        }
        let mut query = tx.prepare(
            "SELECT o.body,w.body FROM namespace_changes c INDEXED BY namespace_changes_sequence
             LEFT JOIN namespace_objects o ON o.id=c.object
             LEFT JOIN working_files w ON w.id=o.working
             WHERE c.sequence>?1 ORDER BY c.sequence LIMIT 10001",
        )?;
        let rows = query.query_map([after_sql], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        let mut objects = Vec::new();
        for row in rows {
            let (object, working) = row?;
            let object: NamespaceObject = serde_json::from_str(&object)?;
            let working = working
                .map(|s| serde_json::from_str::<WorkingFile>(&s))
                .transpose()?;
            if object.working_file != working.as_ref().map(|w| w.id) {
                return Err(JournalError::Corrupt);
            }
            objects.push(NamespaceSnapshot { object, working });
        }
        if objects.len() > 10_000 {
            return Err(JournalError::Quota);
        }
        Ok(NamespacePublication {
            after,
            through,
            objects,
        })
    }
}
