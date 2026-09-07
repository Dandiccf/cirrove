//! Completion prerequisites are separate from the receipt supplying new content's base.
use super::*;
use rusqlite::Transaction;

#[derive(Default)]
pub(super) struct WriteOrder {
    pub base: Option<WriteBase>,
    pub prerequisites: Vec<Uuid>,
}
impl From<Option<WriteBase>> for WriteOrder {
    fn from(base: Option<WriteBase>) -> Self {
        Self {
            base,
            prerequisites: vec![],
        }
    }
}

pub(super) fn migrate(db: &mut Connection, version: u32) -> Result<()> {
    if version < 10 {
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS write_prerequisites(
            operation TEXT NOT NULL, predecessor TEXT NOT NULL,
            PRIMARY KEY(operation,predecessor));
            CREATE INDEX IF NOT EXISTS write_prerequisite_successors
            ON write_prerequisites(predecessor,operation);",
        )?;
        tx.pragma_update(None, "user_version", 10)?;
        tx.commit()?;
    }
    db.prepare("SELECT operation,predecessor FROM write_prerequisites LIMIT 0")?;
    Ok(())
}

pub(super) fn validate(db: &Connection, scope: &Scope, prerequisites: &[Uuid]) -> Result<()> {
    if prerequisites.len() > 16 {
        return Err(JournalError::Quota);
    }
    let mut seen = std::collections::HashSet::new();
    for id in prerequisites {
        if !seen.insert(id) {
            return Err(JournalError::Intent);
        }
        let encoded: Option<String> = db
            .query_row(
                "SELECT json_extract(body,'$.scope') FROM uploads WHERE id=?1
            UNION ALL SELECT json_extract(body,'$.request.scope') FROM mutations WHERE id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        let actual: Scope = serde_json::from_str(&encoded.ok_or(JournalError::Missing)?)?;
        if &actual != scope {
            return Err(JournalError::Account);
        }
    }
    Ok(())
}

pub(super) fn insert(
    tx: &Transaction<'_>,
    operation: Uuid,
    sequence: u64,
    scope: &Scope,
    prerequisites: &[Uuid],
) -> Result<()> {
    validate(tx, scope, prerequisites)?;
    for predecessor in prerequisites {
        let previous: Option<i64> = tx
            .query_row(
                "SELECT sequence FROM write_queue WHERE id=?1",
                [predecessor.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        // Every edge points backward in one journal. Cycles and future/self
        // prerequisites are impossible, including across operation kinds.
        if !previous.is_some_and(|p| p > 0 && (p as u64) < sequence) {
            return Err(JournalError::Stale);
        }
        tx.execute(
            "INSERT INTO write_prerequisites VALUES(?1,?2)",
            params![operation.to_string(), predecessor.to_string()],
        )?;
    }
    Ok(())
}

pub(super) fn satisfied(db: &Connection, id: Uuid) -> Result<bool> {
    Ok(!db.query_row(
        "SELECT EXISTS(SELECT 1 FROM write_prerequisites b
        LEFT JOIN write_queue p ON p.id=b.predecessor
        WHERE b.operation=?1 AND coalesce(p.complete,0)!=1)",
        [id.to_string()],
        |r| r.get::<_, bool>(0),
    )?)
}

impl UploadJournal {
    /// Completion dependencies only. These do not supply an item ID or ETag and
    /// do not consume the predecessor's linear content-successor relationship.
    pub fn operation_prerequisites(&self, id: Uuid) -> Result<Vec<Uuid>> {
        let exists: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM write_queue WHERE id=?1)",
            [id.to_string()],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(JournalError::Missing);
        }
        let mut q = self.db.prepare(
            "SELECT predecessor FROM write_prerequisites WHERE operation=?1 ORDER BY predecessor",
        )?;
        q.query_map([id.to_string()], |r| r.get::<_, String>(0))?
            .map(|row| Uuid::parse_str(&row?).map_err(|_| JournalError::Corrupt))
            .collect()
    }
}
