//! Explicit local Stage abandonment. Never calls a provider or deletes recovery bytes.
use super::*;
use cirrove_icloud::{NativeReplacementAbandonEvidence, NativeReplacementAbandonRecord};
const _: () = assert!(JOURNAL_SCHEMA >= 18);
const MAX_RECEIPT: usize = 64 * 1024;

pub(super) fn migrate(db: &Connection) -> Result<()> {
    // Additive to the unreleased schema18; opening a preexisting synthetic18
    // installs the table too. Read-only open never creates or migrates it.
    db.execute_batch("CREATE TABLE IF NOT EXISTS native_replacement_abandonments(operation TEXT PRIMARY KEY,body TEXT NOT NULL);")?;
    Ok(())
}
pub struct NativeAbandonPreparation {
    row: UploadRecord,
    owner: NamespaceObject,
    recovery: NamespaceObject,
}
impl NativeAbandonPreparation {
    pub fn operation(&self) -> Uuid {
        self.row.id
    }
    pub fn request(&self) -> cirrove_core::upload::UploadRequest {
        request(&self.row)
    }
}
fn request(row: &UploadRecord) -> cirrove_core::upload::UploadRequest {
    cirrove_core::upload::UploadRequest {
        scope: row.scope.clone(),
        intent: row.intent.clone(),
        representation: row.representation.clone(),
        size: row.size,
        sha256: row.sha256.clone(),
    }
}
fn row(db: &Connection, id: Uuid) -> Result<UploadRecord> {
    let body: String = db.query_row(
        "SELECT body FROM uploads WHERE id=?1",
        [id.to_string()],
        |r| r.get(0),
    )?;
    if body.len() > 256 * 1024 {
        return Err(JournalError::Corrupt);
    }
    Ok(serde_json::from_str(&body)?)
}
fn snapshot(db: &Connection, account: &str, id: Uuid) -> Result<NativeAbandonPreparation> {
    let row = row(db, id)?;
    package_replacement::validate(&row.scope, &row.intent, &row.representation)?;
    let before = package_replacement::original(&row.representation).ok_or(JournalError::Intent)?;
    if row.scope.account != account
        || row.state != UploadState::Conflict
        || row.attempt.is_some()
        || row.base.is_some()
        || row.working_file.is_some()
        || row.remote.is_some()
        || row.package_completion.is_some()
        || row.session_key != Some(id)
    {
        return Err(JournalError::Stale);
    }
    let owner_id: String = db.query_row(
        "SELECT object FROM namespace_operations WHERE operation=?1",
        [id.to_string()],
        |r| r.get(0),
    )?;
    let owner = namespace::by_id(
        db,
        Uuid::parse_str(&owner_id).map_err(|_| JournalError::Corrupt)?,
    )?;
    if owner.scope != row.scope
        || owner.latest != Some(id)
        || !owner.remote_owned
        || owner.unlinked
        || owner.follows_remote
        || owner.working_file.is_some()
        || owner.remote.as_ref() != Some(before)
        || owner.node.kind != NodeKind::Folder
        || !owner.node.package
        || owner.node.target.is_some()
        || owner.node.name != before.name
    {
        return Err(JournalError::Stale);
    }
    let (recovery_id, name) = row
        .identity_handoff
        .as_ref()
        .and_then(|r| r.unconfirmed_native(before))
        .ok_or(JournalError::Stale)?;
    let recovery = namespace::by_id(db, recovery_id)?;
    if recovery.scope != row.scope
        || !recovery.unlinked
        || recovery.remote_owned
        || recovery.remote.as_ref() != Some(before)
        || recovery.working_file.is_some()
        || recovery.latest.is_some()
        || recovery.follows_remote
        || recovery.node.name != name
        || recovery.node.kind != NodeKind::Folder
        || !recovery.node.package
        || recovery.node.target.is_some()
        || recovery.node.size != before.size
    {
        return Err(JournalError::Stale);
    }
    let complete: bool = db.query_row(
        "SELECT complete FROM write_queue WHERE id=?1",
        [id.to_string()],
        |r| r.get(0),
    )?;
    let blocked:bool=db.query_row("SELECT
        EXISTS(SELECT 1 FROM write_successors WHERE predecessor=?1 OR successor=?1)
        OR EXISTS(SELECT 1 FROM write_prerequisites WHERE predecessor=?1 OR operation=?1)
        OR EXISTS(SELECT 1 FROM write_destinations WHERE predecessor=?1 OR operation=?1)
        OR EXISTS(SELECT 1 FROM file_replacements WHERE id=?1 OR cleanup=?1 OR source=?2 OR victim=?2 OR source=?3 OR victim=?3)
        OR EXISTS(SELECT 1 FROM native_working_operations WHERE operation=?1 OR owner=?2 OR owner=?3)
        OR EXISTS(SELECT 1 FROM native_working_bindings WHERE source=?4)
        OR EXISTS(SELECT 1 FROM native_working_heads WHERE json_extract(body,'$.owner') IN (?2,?3))
        OR EXISTS(SELECT 1 FROM working_files WHERE json_extract(body,'$.latest')=?1 OR json_extract(body,'$.node.id')=?5 OR json_extract(body,'$.node.parent_id')=?5)
        OR EXISTS(SELECT 1 FROM write_resources a JOIN write_resources b ON b.resource=a.resource JOIN write_queue q ON q.id=b.id WHERE a.id=?1 AND b.id!=?1 AND q.complete=0)
        OR EXISTS(SELECT 1 FROM package_metadata_publication WHERE operation=?1)",
        params![id.to_string(),owner.id.to_string(),recovery.id.to_string(),serde_json::to_string(&(&row.scope,&before.id))?,owner.node.id],|r|r.get(0))?;
    if complete || blocked {
        return Err(JournalError::Stale);
    }
    Ok(NativeAbandonPreparation {
        row,
        owner,
        recovery,
    })
}
fn same(a: &NativeAbandonPreparation, b: &NativeAbandonPreparation) -> Result<bool> {
    Ok(serde_json::to_vec(&a.row)? == serde_json::to_vec(&b.row)?
        && serde_json::to_vec(&a.owner)? == serde_json::to_vec(&b.owner)?
        && serde_json::to_vec(&a.recovery)? == serde_json::to_vec(&b.recovery)?)
}
fn bound(row: &UploadRecord, record: &NativeReplacementAbandonRecord) -> Result<()> {
    record.validate().map_err(|_| JournalError::Corrupt)?;
    if record.operation != row.id || record.request != request(row) {
        return Err(JournalError::Stale);
    }
    Ok(())
}
pub(super) fn exportable(db: &Connection, row: &UploadRecord) -> Result<bool> {
    if row.state != UploadState::Resolved {
        return Ok(false);
    }
    let exists:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='native_replacement_abandonments')",[],|r|r.get(0))?;
    if !exists {
        return Ok(false);
    }
    let body: Option<String> = db
        .query_row(
            "SELECT body FROM native_replacement_abandonments WHERE operation=?1",
            [row.id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    let Some(body) = body else { return Ok(false) };
    if body.len() > MAX_RECEIPT {
        return Err(JournalError::Corrupt);
    }
    let record: NativeReplacementAbandonRecord = serde_json::from_str(&body)?;
    bound(row, &record)?;
    Ok(row.remote.is_none()
        && row.package_completion.is_none()
        && row
            .identity_handoff
            .as_ref()
            .and_then(|r| r.unconfirmed_native(&record.original))
            .is_some())
}
impl UploadJournal {
    pub fn prepare_native_stage_abandonment(&self, id: Uuid) -> Result<NativeAbandonPreparation> {
        let prepared = snapshot(&self.db, &self.account, id)?;
        // Refuse if retained recovery bytes are already unavailable. Export
        // remains the independently hashed way of reading those same bytes.
        self.local_export_source(id)?;
        Ok(prepared)
    }
    pub fn abandon_native_stage(
        &mut self,
        prepared: NativeAbandonPreparation,
        evidence: NativeReplacementAbandonEvidence,
        cancel: &cirrove_core::CancellationToken,
    ) -> Result<NativeReplacementAbandonRecord> {
        if cancel.is_cancelled() || !evidence.is_fresh() {
            return Err(JournalError::Stale);
        }
        let record = evidence.record();
        bound(&prepared.row, record)?;
        let body = serde_json::to_string(record)?;
        if body.len() > MAX_RECEIPT {
            return Err(JournalError::Quota);
        }
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current = snapshot(&tx, &self.account, prepared.row.id)?;
        if !same(&prepared, &current)? {
            return Err(JournalError::Stale);
        }
        let mut resolved = current.row;
        resolved.state = UploadState::Resolved;
        let mut owner = current.owner;
        owner.latest = None;
        owner.follows_remote = true;
        owner.revision = owner.revision.checked_add(1).ok_or(JournalError::Quota)?;
        namespace::save(&tx, &owner)?;
        tx.execute(
            "INSERT INTO native_replacement_abandonments VALUES(?1,?2)",
            params![resolved.id.to_string(), body],
        )?;
        if tx.execute(
            "UPDATE uploads SET state='resolved',body=?2 WHERE id=?1 AND state='conflict'",
            params![resolved.id.to_string(), serde_json::to_string(&resolved)?],
        )? != 1
        {
            return Err(JournalError::Stale);
        }
        mutations::queue_complete(&tx, resolved.id, true)?;
        // Preserve resource history, namespace-operation mapping, hidden unowned
        // placeholder, sealed payload and opaque checkpoint/session reference.
        if cancel.is_cancelled() {
            return Err(JournalError::Stale);
        }
        tx.commit()?;
        Ok(record.clone())
    }
    pub fn native_stage_abandonment(
        &self,
        id: Uuid,
    ) -> Result<Option<NativeReplacementAbandonRecord>> {
        let current = self.get(id)?;
        if !exportable(&self.db, &current)? {
            return Ok(None);
        }
        let body: String = self.db.query_row(
            "SELECT body FROM native_replacement_abandonments WHERE operation=?1",
            [id.to_string()],
            |r| r.get(0),
        )?;
        Ok(Some(serde_json::from_str(&body)?))
    }
}
