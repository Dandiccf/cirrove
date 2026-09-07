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
    fn scope(&self) -> &Scope {
        match self {
            Self::Upload(r) => &r.scope,
            Self::Mutation(r) => &r.request.scope,
        }
    }
    fn sequence(&self) -> u64 {
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
                MutationIntent::RemoveFile { .. } => None,
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
            Operation::Upload(r) => Ok((r.scope, r.intent)),
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
        if previous.scope() != &request.scope || request.scope.account != self.account {
            return Err(JournalError::Account);
        }
        let before = request.intent.before().ok_or(JournalError::Intent)?;
        if previous.kind().as_ref() != Some(&before.kind) || before.target.is_some() {
            return Err(JournalError::Intent);
        }
        // A newly created local file does not have an ETag yet. Validate the
        // local shape/destination, then replace the source from its receipt at claim.
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
        Ok(!waiting)
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
        Ok(())
    }
}
