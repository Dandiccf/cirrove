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
    /// Source fields are bound to this preceding operation's confirmed receipt
    /// before a worker can claim the request. Destination fields remain unchanged.
    #[serde(default)]
    pub base: Option<WriteBase>,
    #[serde(default)]
    pub working_file: Option<Uuid>,
    /// A local reader-preservation barrier; independent of cloud receipt lineage.
    #[serde(default = "locally_ready")]
    pub local_ready: bool,
}
fn locally_ready() -> bool {
    true
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
pub(super) fn mutation_resources(request: &MutationRequest) -> Result<Vec<String>> {
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
    // Never downgrade a newer journal if the process dies before later migration.
    tx.pragma_update(None, "user_version", version.max(3))?;
    tx.commit()?;
    Ok(())
}
impl UploadJournal {
    pub fn enqueue_mutation(&mut self, request: MutationRequest) -> Result<MutationRecord> {
        request.validate().map_err(|_| JournalError::Intent)?;
        self.enqueue_bound_mutation(request, None, None)
    }
    /// Follow the confirmed identity/version of an earlier save or namespace
    /// operation. Only the requested destination is independent of that receipt.
    /// The supplied source is a local preview and may lack a remote ETag.
    pub fn enqueue_mutation_after(
        &mut self,
        predecessor: Uuid,
        request: MutationRequest,
    ) -> Result<MutationRecord> {
        self.enqueue_mutation_after_all(predecessor, &[], request)
    }
    /// A separate completion barrier can, for example, delay cleanup of a source
    /// temporary file until replacement of the destination was acknowledged.
    /// The source's own predecessor still supplies the conditional deletion base.
    pub fn enqueue_mutation_after_all(
        &mut self,
        predecessor: Uuid,
        prerequisites: &[Uuid],
        request: MutationRequest,
    ) -> Result<MutationRecord> {
        self.validate_mutation_base(predecessor, &request)?;
        self.ensure_successor_free(predecessor)?;
        self.enqueue_mutation_transaction(
            request,
            WriteOrder {
                base: Some(WriteBase {
                    predecessor,
                    resolved: false,
                }),
                prerequisites: prerequisites.to_vec(),
            },
            None,
            None,
        )
    }
    pub(super) fn enqueue_bound_mutation(
        &mut self,
        request: MutationRequest,
        base: Option<WriteBase>,
        working: Option<WorkingFile>,
    ) -> Result<MutationRecord> {
        self.enqueue_mutation_transaction(request, base.into(), working, None)
    }
    pub(super) fn enqueue_namespace_mutation(
        &mut self,
        request: MutationRequest,
        base: Option<WriteBase>,
        object: NamespaceObject,
    ) -> Result<MutationRecord> {
        self.enqueue_mutation_transaction(request, base.into(), None, Some(object))
    }
    fn enqueue_mutation_transaction(
        &mut self,
        request: MutationRequest,
        order: WriteOrder,
        working: Option<WorkingFile>,
        object: Option<NamespaceObject>,
    ) -> Result<MutationRecord> {
        if request.scope.account != self.account {
            return Err(JournalError::Account);
        }
        barriers::validate(&self.db, &request.scope, &order.prerequisites)?;
        let mut record = MutationRecord {
            id: Uuid::new_v4(),
            sequence: 0,
            request,
            state: MutationState::Pending,
            attempt: None,
            receipt: None,
            retry_at: 0,
            failed_attempts: 0,
            base: order.base,
            working_file: working.as_ref().map(|file| file.id),
            local_ready: true,
        };
        let tx = self.db.transaction()?;
        record.sequence = queue_insert(&tx, record.id, mutation_resources(&record.request)?)?;
        if let MutationIntent::CreateFolder { parent, .. }
        | MutationIntent::Relocate { parent, .. } = &record.request.intent
        {
            directories::bind(
                &tx,
                record.id,
                record.sequence,
                &record.request.scope,
                parent,
            )?;
        }
        super::generations::insert_dependency(
            &tx,
            record.id,
            record.sequence,
            record.base.as_ref(),
        )?;
        barriers::insert(
            &tx,
            record.id,
            record.sequence,
            &record.request.scope,
            &order.prerequisites,
        )?;
        tx.execute(
            "INSERT INTO mutations VALUES(?1,?2,'pending',?3)",
            params![
                record.sequence as i64,
                record.id.to_string(),
                serde_json::to_string(&record)?
            ],
        )?;
        if let Some(working) = working {
            super::working::commit_relocation(&tx, working, &record)?;
        }
        if let Some(object) = object {
            if matches!(record.request.intent, MutationIntent::CreateFolder { .. }) {
                directories::commit_creation(&tx, object, &record)?;
            } else {
                super::namespace::commit_relocation(&tx, object, &record)?;
            }
        }
        tx.commit()?;
        #[cfg(feature = "test-support")]
        crate::journal::durable::record("mutations::enqueue_mutation_transaction");
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
    /// How many namespace changes the daemon has stopped trying to apply.
    ///
    /// `Conflict`, `Failed` and `NeedsReview` are terminal: nothing retries them,
    /// and each one is a change the mount has already acted on locally that the
    /// provider never took. A folder removal that ends here leaves the directory
    /// hidden in the mount and present in the account -- which is exactly what
    /// happened to fourteen of them on a live drive, with nothing anywhere saying
    /// so. A count is the smallest honest thing status can carry; it names no
    /// paths, so it costs the user no privacy to have it always on.
    pub fn stuck_mutations(&self) -> Result<u64> {
        let count: i64 = self.db.query_row(
            "SELECT count(*) FROM mutations WHERE state IN ('conflict','failed','needs_review')",
            [],
            |r| r.get(0),
        )?;
        Ok(count.max(0) as u64)
    }
    pub(super) fn save_mutation(&mut self, record: &MutationRecord) -> Result<()> {
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
        if record.state == MutationState::Applied
            && let Some(MutationReceipt::Upsert(remote)) = &record.receipt
        {
            super::namespace::confirm(&tx, record.id, record.sequence, remote)?;
        }
        tx.commit()?;
        #[cfg(feature = "test-support")]
        crate::journal::durable::record("mutations::save_mutation");
        Ok(())
    }
    pub fn claim_mutation(&mut self) -> Result<Option<MutationRecord>> {
        if !self.resolve_ready_generations()? {
            return Ok(None);
        }
        let body: Option<String> = self.db.query_row("SELECT m.body FROM mutations m WHERE m.state IN ('pending','verify_required')
            AND NOT EXISTS(SELECT 1 FROM write_destinations d WHERE d.operation=m.id AND d.resolved=0)
            AND coalesce(json_extract(m.body,'$.local_ready'),1)=1
            AND (json_extract(m.body,'$.base') IS NULL OR json_extract(m.body,'$.base.resolved')=1)
            AND NOT EXISTS (SELECT 1 FROM write_prerequisites b LEFT JOIN write_queue p ON p.id=b.predecessor
                WHERE b.operation=m.id AND coalesce(p.complete,0)!=1)
            AND json_extract(m.body,'$.retry_at')<=?1 AND NOT EXISTS (
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
    ) -> Result<MutationState> {
        let mut record = self.mutation_attempt(id, attempt)?;
        if !record.request.accepts(&receipt) {
            return Err(JournalError::Corrupt);
        }
        record.state = reconciled_state(&record, &receipt);
        record.receipt = Some(receipt);
        record.attempt = None;
        self.save_mutation(&record)?;
        Ok(record.state)
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
    /// Abandon a removal the provider never took, and let the mount show what is
    /// really there again.
    ///
    /// A terminal mutation is a disagreement nothing retries. For a removal that
    /// is the worst shape it can have: the object is `unlinked` locally, so the
    /// mount says the file or folder is gone, and the provider still has it.
    /// Fourteen of those sat in a live account with no way to clear them --
    /// `request_mutation_retry` refuses `Conflict` by design, and nothing else
    /// touched it.
    ///
    /// Three deliberate narrownesses, for the same reason `forget` has its own.
    ///
    /// It discards rather than retries. A conflict means the remote moved under
    /// us, and re-sending a delete against whatever is there now is how a stale
    /// intent destroys someone else's change. Dropping the intent and showing
    /// reality lets the user look and decide again; the second attempt is then
    /// an ordinary `rmdir` built from current state.
    ///
    /// It takes removals only. A stuck `CreateFolder` or `Relocate` leaves the
    /// local tree depending on it -- children parented to a folder that was
    /// never created -- and unwinding that is a different problem from clearing
    /// a flag. Refused rather than half-handled.
    ///
    /// And it takes terminal states only. A change still being worked on is not
    /// stuck, and discarding one would race the worker holding it.
    /// Ids of stuck removals, oldest first. Removals only, because they are the
    /// only ones `discard_stuck_removal` will take.
    pub fn stuck_removals(&self, limit: u32) -> Result<Vec<Uuid>> {
        let mut query = self.db.prepare(
            "SELECT body FROM mutations WHERE state IN ('conflict','failed','needs_review')
             ORDER BY sequence LIMIT ?1",
        )?;
        let rows = query.query_map([limit.clamp(1, 1000)], |r| r.get::<_, String>(0))?;
        let mut ids = Vec::new();
        for row in rows {
            let record: MutationRecord = serde_json::from_str(&row?)?;
            if matches!(
                record.request.intent,
                MutationIntent::RemoveFile { .. } | MutationIntent::RemoveFolder { .. }
            ) {
                ids.push(record.id);
            }
        }
        Ok(ids)
    }
    pub fn discard_stuck_removal(&mut self, id: Uuid) -> Result<()> {
        let record = self.mutation(id)?;
        if !matches!(
            record.state,
            MutationState::Conflict | MutationState::Failed | MutationState::NeedsReview
        ) {
            return Err(JournalError::Stale);
        }
        if !matches!(
            record.request.intent,
            MutationIntent::RemoveFile { .. } | MutationIntent::RemoveFolder { .. }
        ) {
            return Err(JournalError::Intent);
        }
        let mut object = self
            .namespace_for_operation(id)?
            .ok_or(JournalError::Missing)?;
        if !object.unlinked || object.latest != Some(id) {
            // Something else has happened to this object since. Refusing is the
            // honest answer: the caller's picture of it is out of date.
            return Err(JournalError::Stale);
        }
        // Visibility only. Freshness is a separate thing and is not this
        // function's to fix: the restored object still carries whatever ETag it
        // held when the removal was built, and if the remote moved in the
        // meantime -- which is what a conflict means -- a second removal
        // attempted before the delta feed catches up is refused for the original
        // reason. Measured on a live drive: of fourteen abandoned removals, ten
        // deleted cleanly on the second attempt and four conflicted again, their
        // local copies on ",1" while the feed already held ",2". Those four
        // cleared once the feed caught up.
        //
        // Making the object follow the remote here was tried and does not help:
        // the node it would follow is the one this journal stored, which is the
        // stale one. Refreshing from the provider is the delta feed's job and
        // doing it from inside a discard would be a second, worse copy of it.
        object.unlinked = false;
        object.latest = None;
        object.revision = object.revision.checked_add(1).ok_or(JournalError::Quota)?;
        let tx = self.db.transaction()?;
        super::namespace::save(&tx, &object)?;
        tx.execute(
            "DELETE FROM namespace_operations WHERE operation=?1",
            [id.to_string()],
        )?;
        tx.execute("DELETE FROM mutations WHERE id=?1", [id.to_string()])?;
        tx.execute("DELETE FROM write_resources WHERE id=?1", [id.to_string()])?;
        tx.execute("DELETE FROM write_queue WHERE id=?1", [id.to_string()])?;
        tx.commit()?;
        Ok(())
    }
    pub fn request_mutation_retry(&mut self, id: Uuid) -> Result<()> {
        let mut record = self.mutation(id)?;
        if record.base.as_ref().is_some_and(|base| !base.resolved) {
            return Err(JournalError::Stale);
        }
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

fn reconciled_state(record: &MutationRecord, receipt: &MutationReceipt) -> MutationState {
    let (MutationIntent::Relocate { before, .. }, MutationReceipt::Upsert(after)) =
        (&record.request.intent, receipt)
    else {
        return MutationState::Applied;
    };
    if record.state != MutationState::Verifying || before.kind != NodeKind::File {
        // A conditional mutation response binds to the version we requested.
        return MutationState::Applied;
    }
    // A later lookup at the requested name can also include somebody else's
    // content edit. Never adopt that new ETag as the base for our next upload.
    if before.size != after.size {
        return MutationState::Conflict;
    }
    if before.etag == after.etag {
        return MutationState::Applied;
    }
    match (
        before.content_version.as_deref().filter(|s| !s.is_empty()),
        after.content_version.as_deref().filter(|s| !s.is_empty()),
    ) {
        (Some(old), Some(new)) if old == new => MutationState::Applied,
        (Some(_), Some(_)) => MutationState::Conflict,
        _ => MutationState::NeedsReview,
    }
}
