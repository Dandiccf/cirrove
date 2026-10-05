//! A later local edit follows a confirmed receipt, including intervening renames.
use super::*;
use cirrove_core::mutation::{MutationIntent, MutationReceipt, MutationRequest};
use rusqlite::Transaction;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WriteBase {
    pub predecessor: Uuid,
    pub resolved: bool,
}
/// Kept for callers of the initial upload-only generation API.
pub type UploadBase = WriteBase;

pub(super) fn migrate(db: &mut Connection, version: u32) -> Result<()> {
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS upload_successor
        ON uploads(json_extract(body,'$.base.predecessor'))
        WHERE json_type(body,'$.base.predecessor')='text';",
    )?;
    tx.pragma_update(None, "user_version", version.max(4))?;
    tx.commit()?;
    Ok(())
}

pub(super) fn migrate_dependencies(db: &mut Connection, version: u32) -> Result<()> {
    if version >= 6 {
        // Missing lineage in an already migrated journal is corruption, not an
        // invitation to rebuild only part of its causal relationships.
        db.prepare("SELECT predecessor,successor FROM write_successors LIMIT 0")?;
        return Ok(());
    }
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS write_successors (
        predecessor TEXT PRIMARY KEY, successor TEXT NOT NULL UNIQUE);
        INSERT OR IGNORE INTO write_successors
        SELECT json_extract(body,'$.base.predecessor'),id FROM uploads
        WHERE json_type(body,'$.base.predecessor')='text';",
    )?;
    tx.pragma_update(None, "user_version", 6)?;
    tx.commit()?;
    Ok(())
}

pub(super) fn insert_dependency(
    tx: &Transaction<'_>,
    id: Uuid,
    sequence: u64,
    base: Option<&WriteBase>,
) -> Result<()> {
    let Some(base) = base else { return Ok(()) };
    let previous: Option<i64> = tx
        .query_row(
            "SELECT sequence FROM write_queue WHERE id=?1",
            [base.predecessor.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    if base.resolved
        || !previous.is_some_and(|previous| previous > 0 && (previous as u64) < sequence)
    {
        return Err(JournalError::Stale);
    }
    if tx.execute(
        "INSERT OR IGNORE INTO write_successors(predecessor,successor) VALUES(?1,?2)",
        params![base.predecessor.to_string(), id.to_string()],
    )? != 1
    {
        return Err(JournalError::Stale);
    }
    Ok(())
}

pub(super) enum Operation {
    Upload(UploadRecord),
    Mutation(MutationRecord),
}
impl Operation {
    pub(super) fn scope(&self) -> &Scope {
        match self {
            Self::Upload(r) => &r.scope,
            Self::Mutation(r) => &r.request.scope,
        }
    }
    pub(super) fn sequence(&self) -> u64 {
        match self {
            Self::Upload(r) => r.sequence,
            Self::Mutation(r) => r.sequence,
        }
    }
    fn kind(&self) -> Option<NodeKind> {
        match self {
            Self::Upload(_) => Some(NodeKind::File),
            Self::Mutation(r) => match &r.request.intent {
                MutationIntent::CreateFolder { .. } => Some(NodeKind::Folder),
                MutationIntent::Relocate { before, .. } => Some(before.kind.clone()),
                // A removal leaves no node behind, so it contributes no kind to
                // the generation, folders included.
                MutationIntent::RemoveFile { .. }
                | MutationIntent::RemoveFolder { .. }
                | MutationIntent::TrashNativeDocument { .. } => None,
            },
        }
    }
    pub(super) fn confirmed_node(&self) -> Option<Node> {
        match self {
            Self::Upload(r) if r.state == UploadState::Uploaded => r.remote.clone(),
            Self::Mutation(r) if r.state == MutationState::Applied => match &r.receipt {
                Some(receipt @ MutationReceipt::Upsert(node)) if r.request.accepts(receipt) => {
                    Some(node.clone())
                }
                _ => None,
            },
            _ => None,
        }
    }
}

impl UploadJournal {
    pub(super) fn operation(&self, id: Uuid) -> Result<Operation> {
        match self.get(id) {
            Ok(r) => Ok(Operation::Upload(r)),
            Err(JournalError::Missing) => self.mutation(id).map(Operation::Mutation),
            Err(e) => Err(e),
        }
    }
    pub(super) fn ensure_successor_free(&self, predecessor: Uuid) -> Result<()> {
        self.operation(predecessor)?;
        let exists: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM write_successors WHERE predecessor=?1)",
            [predecessor.to_string()],
            |r| r.get(0),
        )?;
        if exists {
            Err(JournalError::Stale)
        } else {
            Ok(())
        }
    }
    pub(super) fn upload_intent_after(&self, predecessor: Uuid) -> Result<(Scope, UploadIntent)> {
        match self.operation(predecessor)? {
            Operation::Upload(r) if r.representation.is_file_bytes() => Ok((r.scope, r.intent)),
            Operation::Upload(_) => Err(JournalError::Intent),
            Operation::Mutation(r) => match r.request.intent {
                MutationIntent::Relocate { before, .. } if before.kind == NodeKind::File => Ok((
                    r.request.scope,
                    // This is a pending plan. Eligibility always requires rebinding
                    // to the preceding validated receipt before any provider call.
                    UploadIntent::Replace {
                        item: before.id,
                        expected_etag: before
                            .etag
                            .unwrap_or_else(|| "cirrove-pending-receipt".into()),
                    },
                )),
                _ => Err(JournalError::Intent),
            },
        }
    }
    /// Seal a newer save after a save or file relocation. A conflict retains and
    /// blocks the linear chain; only a confirmed receipt supplies its remote base.
    pub fn enqueue_after(&mut self, predecessor: Uuid, bytes: impl Read) -> Result<UploadRecord> {
        self.enqueue_after_all(predecessor, &[], bytes)
    }
    /// Use one confirmed content base, while also waiting for operations on other
    /// objects. Atomic replacement needs both sides' earlier changes to finish;
    /// an ordering prerequisite must never substitute its identity or ETag.
    pub fn enqueue_after_all(
        &mut self,
        predecessor: Uuid,
        prerequisites: &[Uuid],
        bytes: impl Read,
    ) -> Result<UploadRecord> {
        let (scope, intent) = self.upload_intent_after(predecessor)?;
        self.ensure_successor_free(predecessor)?;
        self.enqueue_generation(
            scope,
            intent,
            WriteOrder {
                base: Some(WriteBase {
                    predecessor,
                    resolved: false,
                }),
                prerequisites: prerequisites.to_vec(),
            },
            None,
            bytes,
        )
    }

    pub(super) fn validate_mutation_base(
        &self,
        predecessor: Uuid,
        request: &MutationRequest,
    ) -> Result<()> {
        let previous = self.operation(predecessor)?;
        if matches!(&previous, Operation::Upload(row) if !row.representation.is_file_bytes()) {
            return Err(JournalError::Intent);
        }
        if previous.scope() != &request.scope || request.scope.account != self.account {
            return Err(JournalError::Account);
        }
        let before = request.intent.before().ok_or(JournalError::Intent)?;
        if previous.kind().as_ref() != Some(&before.kind) || before.target.is_some() {
            return Err(JournalError::Intent);
        }
        // A newly created local file does not have an ETag yet. Validate the
        // local shape/destination, then replace the source from its receipt at claim.
        //
        // `RemoveFolder` is deliberately absent, and this was tried. Chaining a
        // folder removal to its own creation receipt fails against Graph: the
        // eTag a folder carries in its create response is not the one it has a
        // moment later, so the conditional DELETE loses its precondition and the
        // removal lands in `Conflict` -- after `rmdir` has already told the
        // caller it succeeded. Measured on a live drive: fourteen of fourteen
        // chained folder removals conflicted, five of five unchained ones
        // applied. `Writeback::rmdir` refuses the window with `EBUSY` instead,
        // which is the honest answer, and `a_folder_removal_cannot_be_chained_to_its_own_creation`
        // holds the reason here.
        let mut preview = request.clone();
        let source = match &mut preview.intent {
            MutationIntent::Relocate { before, .. } | MutationIntent::RemoveFile { before } => {
                before
            }
            _ => return Err(JournalError::Intent),
        };
        source.etag = Some("cirrove-pending-receipt".into());
        preview.validate().map_err(|_| JournalError::Intent)
    }

    pub(super) fn resolve_ready_generations(&mut self) -> Result<bool> {
        let destinations = self.resolve_ready_destinations()?;
        // Resolve a bounded batch across both operation kinds before allowing
        // either worker to claim. A newly assigned ID must reserve its resources
        // before any younger independent-looking operation can overtake it.
        let ready = "SELECT 'upload' AS kind,u.id,u.sequence AS sequence FROM uploads u JOIN write_queue p
            ON p.id=json_extract(u.body,'$.base.predecessor')
            WHERE u.state IN ('pending','preparing') AND p.complete=1 AND json_extract(u.body,'$.base.resolved')=0
            UNION ALL
            SELECT 'mutation' AS kind,m.id,m.sequence AS sequence FROM mutations m JOIN write_queue p
            ON p.id=json_extract(m.body,'$.base.predecessor')
            WHERE m.state='pending' AND p.complete=1 AND json_extract(m.body,'$.base.resolved')=0";
        let rows = {
            let mut query = self
                .db
                .prepare(&format!("{ready} ORDER BY sequence LIMIT 256"))?;
            query
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        for (kind, id) in rows {
            let id = Uuid::parse_str(&id).map_err(|_| JournalError::Corrupt)?;
            if kind == "upload" {
                self.resolve_upload(id)?;
            } else {
                self.resolve_mutation(id)?;
            }
        }
        let waiting: bool = self
            .db
            .query_row(&format!("SELECT EXISTS({ready})"), [], |r| r.get(0))?;
        Ok(destinations && !waiting)
    }
    /// The exact completed source already used to resolve this upload intent.
    /// A provider may need it before a metadata refresh indexes a new remote ID.
    pub(crate) fn confirmed_upload_base(&self, id: Uuid) -> Result<Option<Node>> {
        let record = self.get(id)?;
        let Some(base) = record.base.as_ref() else {
            return Ok(None);
        };
        if !base.resolved {
            return Err(JournalError::Stale);
        }
        if package_replacement::original(&record.representation).is_some() {
            return working::native::successors::confirmed_base(self, &record).map(Some);
        }
        let node = self
            .base_node(base, &record.scope, record.sequence)?
            .ok_or(JournalError::Stale)?;
        if node.kind != NodeKind::File
            || node.package
            || node.target.is_some()
            || !matches!(&record.intent, UploadIntent::Replace { item, expected_etag }
                if item == &node.id && Some(expected_etag) == node.etag.as_ref())
        {
            return Err(JournalError::Stale);
        }
        Ok(Some(node))
    }

    /// An unlinked ordinary stream's acknowledged generation can authorize its
    /// exact successor removal even before an evictable metadata index sees it.
    /// This supplies identity authority only; the provider still verifies the
    /// indexed parent chain and the current remote revision independently.
    pub(crate) fn confirmed_remove_base(
        &self,
        id: Uuid,
        request: &MutationRequest,
    ) -> Result<Option<Node>> {
        let (sequence, state, body): (i64, String, String) = self
            .db
            .query_row(
                "SELECT sequence,state,body FROM mutations WHERE id=?1",
                [id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?
            .ok_or(JournalError::Missing)?;
        let remove: MutationRecord = serde_json::from_str(&body)?;
        if remove.id != id
            || sequence <= 0
            || sequence as u64 != remove.sequence
            || remove.request != *request
            || request.scope.account != self.account
            || request.validate().is_err()
        {
            return Err(JournalError::Stale);
        }
        let MutationIntent::RemoveFile { before } = &request.intent else {
            return Ok(None);
        };
        let Some(base) = &remove.base else {
            return Ok(None);
        };
        let phase_matches = match remove.state {
            MutationState::Pending => state == "pending" && remove.attempt.is_none(),
            MutationState::Applying => state == "applying" && remove.attempt.is_some(),
            _ => false,
        };
        if !phase_matches
            || !base.resolved
            || !remove.local_ready
            || remove.prepared_item.is_some()
            || remove.receipt.is_some()
            || remove.verified_content.is_some()
            || before.kind != NodeKind::File
            || before.package
            || before.target.is_some()
            || before.content_revision().is_none()
        {
            return Err(JournalError::Stale);
        }
        let (upload_sequence, upload_state, body): (i64, String, String) = self
            .db
            .query_row(
                "SELECT sequence,state,body FROM uploads WHERE id=?1",
                [base.predecessor.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?
            .ok_or(JournalError::Stale)?;
        let upload: UploadRecord = serde_json::from_str(&body)?;
        if upload.id != base.predecessor
            || upload_sequence <= 0
            || upload_sequence as u64 != upload.sequence
            || upload.sequence >= remove.sequence
            || upload_state != "uploaded"
            || upload.state != UploadState::Uploaded
            || upload.scope != request.scope
            || !upload.representation.is_file_bytes()
            || upload.intent.validate().is_err()
            || upload.base.as_ref().is_some_and(|base| !base.resolved)
            || upload.package_completion.is_some()
            || upload.attempt.is_some()
            || upload.remote.as_ref() != Some(before)
            || upload.size != before.size
            || upload.transferred_bytes != upload.size
            || (upload.identity_handoff.is_none()
                && !match &upload.intent {
                    UploadIntent::Create { parent, name } => {
                        before.parent_id.as_ref() == Some(parent) && &before.name == name
                    }
                    UploadIntent::Replace { item, .. } => &before.id == item,
                })
            || (upload.identity_handoff.is_some()
                && upload
                    .ordinary_handoff_receipt()
                    .is_none_or(|(current, _)| current != before))
        {
            return Err(JournalError::Stale);
        }
        let owner = self
            .namespace_for_operation(id)?
            .ok_or(JournalError::Stale)?;
        let working_id = upload.working_file.ok_or(JournalError::Stale)?;
        if owner.scope != request.scope
            || !owner.unlinked
            || owner.follows_remote
            || !owner.remote_owned
            || owner.native_archive.is_some()
            || owner.latest != Some(id)
            || owner.remote.as_ref() != Some(before)
            || owner.remote_sequence != upload.sequence
            || owner.working_file != Some(working_id)
            || remove.working_file != Some(working_id)
            || owner.node.kind != NodeKind::File
            || owner.node.package
            || owner.node.target.is_some()
        {
            return Err(JournalError::Stale);
        }
        let working = self.working_file(working_id)?;
        if working.id != working_id
            || working.native
            || working.scope != request.scope
            || !working.unlinked
            || working.latest != Some(id)
            || working.node != owner.node
        {
            return Err(JournalError::Stale);
        }
        // Mutable bytes may have changed after unlink. Only the sealed upload's
        // receipt, not today's dirty size or generation, authorizes this removal.
        let bound: bool = self.db.query_row(
            "SELECT
             EXISTS(SELECT 1 FROM write_successors WHERE predecessor=?1 AND successor=?2)
             AND (SELECT count(*) FROM namespace_operations
                  WHERE operation IN (?1,?2) AND object=?3)=2
             AND EXISTS(SELECT 1 FROM namespace_objects WHERE id=?3)
             AND EXISTS(SELECT 1 FROM write_queue WHERE id=?1 AND sequence=?4 AND complete=1)
             AND EXISTS(SELECT 1 FROM write_queue WHERE id=?2 AND sequence=?5 AND complete=0)
             AND NOT EXISTS(SELECT 1 FROM file_replacements
                  WHERE id IN (?1,?2) OR cleanup IN (?1,?2) OR source=?3 OR victim=?3)
             AND NOT EXISTS(SELECT 1 FROM native_working_operations
                  WHERE operation IN (?1,?2) OR owner=?3 OR working=?6)
             AND NOT EXISTS(SELECT 1 FROM native_working_heads
                  WHERE working=?6 OR json_extract(body,'$.owner')=?3)
             AND NOT EXISTS(SELECT 1 FROM native_working_bindings WHERE working=?6)
             AND NOT EXISTS(SELECT 1 FROM native_temporary_streams WHERE working=?6 OR owner=?3)
             AND NOT EXISTS(SELECT 1 FROM native_detached_streams WHERE working=?6 OR owner=?3)",
            params![
                upload.id.to_string(),
                id.to_string(),
                owner.id.to_string(),
                upload_sequence,
                sequence,
                working_id.to_string(),
            ],
            |row| row.get(0),
        )?;
        if !bound {
            return Err(JournalError::Stale);
        }
        Ok(Some(before.clone()))
    }

    fn base_node(&self, base: &WriteBase, scope: &Scope, sequence: u64) -> Result<Option<Node>> {
        let previous = self.operation(base.predecessor)?;
        if previous.scope() != scope || previous.sequence() >= sequence {
            return Ok(None);
        }
        Ok(previous.confirmed_node().filter(|n| n.target.is_none()))
    }
    fn resolve_upload(&mut self, id: Uuid) -> Result<()> {
        let mut record = self.get(id)?;
        if package_replacement::original(&record.representation).is_some() {
            return working::native::successors::resolve(self, record);
        }
        let base = record.base.as_ref().ok_or(JournalError::Corrupt)?;
        let intent = self
            .base_node(base, &record.scope, record.sequence)?
            .and_then(|node| {
                if node.kind != NodeKind::File {
                    return None;
                }
                Some(UploadIntent::Replace {
                    item: node.id,
                    expected_etag: node.etag?,
                })
            });
        if !intent.as_ref().is_some_and(|i| i.validate().is_ok()) {
            record.state = UploadState::Failed;
            return self.save(&record);
        }
        record.intent = intent.ok_or(JournalError::Corrupt)?;
        record.base.as_mut().ok_or(JournalError::Corrupt)?.resolved = true;
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        for key in mutations::upload_resources(&record.scope, &record.intent)? {
            tx.execute(
                "INSERT OR IGNORE INTO write_resources(id,resource) VALUES(?1,?2)",
                params![id.to_string(), key],
            )?;
        }
        tx.execute(
            "UPDATE uploads SET resource=?2,body=?3 WHERE id=?1",
            params![
                id.to_string(),
                resource(&record.intent, &record.scope)?,
                serde_json::to_string(&record)?
            ],
        )?;
        tx.commit()?;
        #[cfg(feature = "test-support")]
        crate::journal::durable::record("generations::resolve_upload");
        Ok(())
    }
    fn resolve_mutation(&mut self, id: Uuid) -> Result<()> {
        let mut record = self.mutation(id)?;
        let base = record.base.as_ref().ok_or(JournalError::Corrupt)?;
        let node = self.base_node(base, &record.request.scope, record.sequence)?;
        let valid = node.as_ref().is_some_and(|node| {
            record
                .request
                .intent
                .before()
                .is_some_and(|before| before.kind == node.kind)
        });
        if !valid {
            record.state = MutationState::Failed;
            return self.save_mutation(&record);
        }
        let node = node.ok_or(JournalError::Corrupt)?;
        // The confirmed node carries the real ETag, replacing the placeholder the
        // preview validated against. `record.request.validate()` below rejects
        // anything that did not actually get one, so a placeholder can never
        // reach the provider. What it cannot check is whether that ETag is still
        // current, which is why folder removals are not chained -- see
        // `validate_mutation_base`.
        match &mut record.request.intent {
            MutationIntent::Relocate { before, .. } | MutationIntent::RemoveFile { before } => {
                *before = node
            }
            _ => return Err(JournalError::Corrupt),
        }
        if record.request.validate().is_err() {
            record.state = MutationState::Failed;
            return self.save_mutation(&record);
        }
        record.base.as_mut().ok_or(JournalError::Corrupt)?.resolved = true;
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        for key in mutations::mutation_resources(&record.request)? {
            tx.execute(
                "INSERT OR IGNORE INTO write_resources(id,resource) VALUES(?1,?2)",
                params![id.to_string(), key],
            )?;
        }
        tx.execute(
            "UPDATE mutations SET body=?2 WHERE id=?1",
            params![id.to_string(), serde_json::to_string(&record)?],
        )?;
        tx.commit()?;
        #[cfg(feature = "test-support")]
        crate::journal::durable::record("generations::resolve_mutation");
        Ok(())
    }
}
