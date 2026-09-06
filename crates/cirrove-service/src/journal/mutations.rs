//! Metadata intents share ordering and ownership with upload snapshots.
use super::*;
use cirrove_core::mutation::{MutationIntent, MutationReceipt, MutationRequest};
use rusqlite::Transaction;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationState {
    Pending,
    Applying,
    VerifyRequired,
    Verifying,
    Applied,
    Conflict,
    Failed,
    NeedsReview,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct MutationRecord {
    pub id: Uuid,
    pub sequence: u64,
    pub request: MutationRequest,
    pub state: MutationState,
    pub attempt: Option<Uuid>,
    pub receipt: Option<MutationReceipt>,
    pub retry_at: u64,
    pub failed_attempts: u32,
}
pub(super) fn item_key(scope: &Scope, item: &str) -> Result<String> {
    Ok(serde_json::to_string(&(scope, "item", item))?)
}
fn slot_key(scope: &Scope, parent: &str, name: &str) -> Result<String> {
    // Conservative queue exclusion only, not provider filename equivalence.
    // Case-sensitive providers may serialize extra operations; the adapter still
    // decides whether a destination really collides.
    let name = name.to_lowercase();
    Ok(serde_json::to_string(&(scope, "name", parent, name))?)
}
pub(super) fn upload_resources(scope: &Scope, intent: &UploadIntent) -> Result<Vec<String>> {
    Ok(vec![match intent {
        UploadIntent::Create { parent, name } => slot_key(scope, parent, name)?,
        UploadIntent::Replace { item, .. } => item_key(scope, item)?,
    }])
}
fn mutation_resources(request: &MutationRequest) -> Result<Vec<String>> {
    let scope = &request.scope;
    let mut keys = vec![];
    if let Some(before) = request.intent.before() {
        keys.push(item_key(scope, &before.id)?);
        keys.push(slot_key(
            scope,
            before.parent_id.as_deref().ok_or(JournalError::Intent)?,
            &before.name,
        )?);
    }
    match &request.intent {
        MutationIntent::CreateFolder { parent, name }
        | MutationIntent::Relocate { parent, name, .. } => {
            keys.push(slot_key(scope, parent, name)?)
        }
        _ => (),
    }
    keys.sort();
    keys.dedup();
    Ok(keys)
}
pub(super) fn queue_insert(tx: &Transaction<'_>, id: Uuid, keys: Vec<String>) -> Result<u64> {
    tx.execute(
        "INSERT INTO write_queue(id,complete) VALUES(?1,0)",
        [id.to_string()],
    )?;
    let sequence = tx.last_insert_rowid() as u64;
    for key in keys {
        tx.execute(
            "INSERT INTO write_resources(id,resource) VALUES(?1,?2)",
            params![id.to_string(), key],
        )?;
    }
    Ok(sequence)
}
pub(super) fn queue_complete(tx: &Transaction<'_>, id: Uuid, complete: bool) -> Result<()> {
    if tx.execute(
        "UPDATE write_queue SET complete=?2 WHERE id=?1",
        params![id.to_string(), complete],
    )? != 1
    {
        return Err(JournalError::Corrupt);
    }
    Ok(())
}
pub(super) fn migrate_queue(db: &mut Connection, version: u32) -> Result<()> {
    let tx = db.transaction()?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS write_queue(sequence INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE, complete INTEGER NOT NULL CHECK(complete IN (0,1)));
        CREATE TABLE IF NOT EXISTS write_resources(id TEXT NOT NULL, resource TEXT NOT NULL, PRIMARY KEY(id,resource));
        CREATE INDEX IF NOT EXISTS write_resource ON write_resources(resource,id);
        CREATE TABLE IF NOT EXISTS mutations(sequence INTEGER PRIMARY KEY, id TEXT NOT NULL UNIQUE, state TEXT NOT NULL, body TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS mutation_state ON mutations(state,sequence);")?;
    if version < 3 {
        let mut query = tx.prepare("SELECT body FROM uploads ORDER BY sequence")?;
        let records = query.query_map([], |r| r.get::<_, String>(0))?;
        for body in records {
            let record: UploadRecord = serde_json::from_str(&body?)?;
            tx.execute(
                "INSERT OR IGNORE INTO write_queue(sequence,id,complete) VALUES(?1,?2,?3)",
                params![
                    record.sequence as i64,
                    record.id.to_string(),
                    record.state == UploadState::Uploaded
                ],
            )?;
            for resource in upload_resources(&record.scope, &record.intent)? {
                tx.execute(
                    "INSERT OR IGNORE INTO write_resources VALUES(?1,?2)",
                    params![record.id.to_string(), resource],
                )?;
            }
        }
    }
    tx.execute("UPDATE mutations SET state='verify_required', body=json_set(body,'$.state','verify_required','$.attempt',NULL) WHERE state IN ('applying','verifying')",[])?;
    tx.pragma_update(None, "user_version", 3)?;
    tx.commit()?;
    Ok(())
}
impl UploadJournal {
    pub fn enqueue_mutation(&mut self, request: MutationRequest) -> Result<MutationRecord> {
        request.validate().map_err(|_| JournalError::Intent)?;
        if request.scope.account != self.account {
            return Err(JournalError::Account);
        }
        let mut record = MutationRecord {
            id: Uuid::new_v4(),
            sequence: 0,
            request,
            state: MutationState::Pending,
            attempt: None,
            receipt: None,
            retry_at: 0,
            failed_attempts: 0,
        };
        let tx = self.db.transaction()?;
        record.sequence = queue_insert(&tx, record.id, mutation_resources(&record.request)?)?;
        tx.execute(
            "INSERT INTO mutations VALUES(?1,?2,'pending',?3)",
            params![
                record.sequence as i64,
                record.id.to_string(),
                serde_json::to_string(&record)?
            ],
        )?;
        tx.commit()?;
        Ok(record)
    }
    pub fn mutation(&self, id: Uuid) -> Result<MutationRecord> {
        let body: String = self
            .db
            .query_row(
                "SELECT body FROM mutations WHERE id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(JournalError::Missing)?;
        Ok(serde_json::from_str(&body)?)
    }
    pub fn list_mutations(&self, after: u64, limit: u32) -> Result<Vec<MutationRecord>> {
        let Ok(after) = i64::try_from(after) else {
            return Ok(vec![]);
        };
        let mut query = self
            .db
            .prepare("SELECT body FROM mutations WHERE sequence>?1 ORDER BY sequence LIMIT ?2")?;
        let rows = query.query_map(params![after, limit.clamp(1, 1000)], |r| {
            r.get::<_, String>(0)
        })?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }
    fn save_mutation(&mut self, record: &MutationRecord) -> Result<()> {
        let state = serde_json::to_value(record.state)?;
        let tx = self.db.transaction()?;
        if tx.execute(
            "UPDATE mutations SET state=?2,body=?3 WHERE id=?1",
            params![
                record.id.to_string(),
                state.as_str().ok_or(JournalError::Corrupt)?,
                serde_json::to_string(record)?
            ],
        )? != 1
        {
            return Err(JournalError::Missing);
        }
        queue_complete(&tx, record.id, record.state == MutationState::Applied)?;
        tx.commit()?;
        Ok(())
    }
    pub fn claim_mutation(&mut self) -> Result<Option<MutationRecord>> {
        let body: Option<String> = self.db.query_row("SELECT m.body FROM mutations m WHERE m.state IN ('pending','verify_required') AND json_extract(m.body,'$.retry_at')<=?1 AND NOT EXISTS (
            SELECT 1 FROM write_queue previous JOIN write_resources a ON a.id=previous.id JOIN write_resources b ON b.resource=a.resource AND b.id=m.id WHERE previous.sequence<m.sequence AND previous.complete=0)
            ORDER BY m.sequence LIMIT 1",[now_seconds() as i64],|r|r.get(0)).optional()?;
        let Some(body) = body else {
            return Ok(None);
        };
        let mut record: MutationRecord = serde_json::from_str(&body)?;
        record.state = if record.state == MutationState::Pending {
            MutationState::Applying
        } else {
            MutationState::Verifying
        };
        record.attempt = Some(Uuid::new_v4());
        self.save_mutation(&record)?;
        Ok(Some(record))
    }
    fn mutation_attempt(&self, id: Uuid, attempt: Uuid) -> Result<MutationRecord> {
        let record = self.mutation(id)?;
        if !matches!(
            record.state,
            MutationState::Applying | MutationState::Verifying
        ) || record.attempt != Some(attempt)
        {
            return Err(JournalError::Stale);
        }
        Ok(record)
    }
    pub fn acknowledge_mutation(
        &mut self,
        id: Uuid,
        attempt: Uuid,
        receipt: MutationReceipt,
    ) -> Result<()> {
        let mut record = self.mutation_attempt(id, attempt)?;
        if !record.request.accepts(&receipt) {
            return Err(JournalError::Corrupt);
        }
        record.receipt = Some(receipt);
        record.state = MutationState::Applied;
        record.attempt = None;
        self.save_mutation(&record)
    }
    pub fn defer_mutation(
        &mut self,
        id: Uuid,
        attempt: Uuid,
        state: MutationState,
        delay: std::time::Duration,
    ) -> Result<()> {
        if !matches!(
            state,
            MutationState::VerifyRequired
                | MutationState::Failed
                | MutationState::Conflict
                | MutationState::NeedsReview
        ) {
            return Err(JournalError::Stale);
        }
        let mut record = self.mutation_attempt(id, attempt)?;
        record.state = state;
        record.attempt = None;
        record.retry_at = now_seconds()
            .saturating_add(delay.as_secs())
            .min(i64::MAX as u64);
        record.failed_attempts = record.failed_attempts.saturating_add(1);
        self.save_mutation(&record)
    }
    pub fn retry_uncommitted_mutation(&mut self, id: Uuid, attempt: Uuid) -> Result<()> {
        let mut record = self.mutation_attempt(id, attempt)?;
        if record.state != MutationState::Verifying {
            return Err(JournalError::Stale);
        }
        record.state = MutationState::Pending;
        record.attempt = None;
        record.retry_at = 0;
        self.save_mutation(&record)
    }
    pub fn request_mutation_retry(&mut self, id: Uuid) -> Result<()> {
        let mut record = self.mutation(id)?;
        if !matches!(
            record.state,
            MutationState::Failed | MutationState::VerifyRequired | MutationState::NeedsReview
        ) {
            return Err(JournalError::Stale);
        }
        record.state = MutationState::VerifyRequired;
        record.retry_at = 0;
        self.save_mutation(&record)
    }
}
