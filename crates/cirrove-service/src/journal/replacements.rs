//! Atomic local path takeover and conditional publication on the destination ID.
use super::*;
use cirrove_core::mutation::{MutationIntent, MutationRequest};
use rusqlite::Transaction;

#[derive(Clone, Serialize, Deserialize)]
pub struct ReplacementRecord {
    pub id: Uuid,
    pub source: Uuid,
    pub victim: Uuid,
    pub cleanup: Uuid,
    pub cleanup_object: Uuid,
    pub local_ready: bool,
    pub remote_applied: bool,
    /// A refused replacement was rescued as this independent create.
    #[serde(default)]
    pub rescued_as: Option<Uuid>,
    /// This takeover captured an unacknowledged empty Create as its source's
    /// content ancestor. The unowned cleanup reservation therefore has no
    /// remote receipt yet; only that exact later Create receipt can authorize it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_unconfirmed_create: Option<Uuid>,
}
pub(super) struct ReplacementCommit {
    pub(super) source: NamespaceObject,
    victim: NamespaceObject,
    source_working: Option<WorkingFile>,
    victim_working: Option<WorkingFile>,
    cleanup_request: MutationRequest,
    cleanup_base: Option<WriteBase>,
    preserve_readers: bool,
}
impl ReplacementCommit {
    pub fn working_id(&self) -> Option<Uuid> {
        self.source_working.as_ref().map(|w| w.id)
    }
}
pub(super) fn migrate(db: &mut Connection, version: u32) -> Result<()> {
    if version < 11 {
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS file_replacements(
            id TEXT PRIMARY KEY, source TEXT NOT NULL, victim TEXT NOT NULL,
            cleanup TEXT NOT NULL UNIQUE, body TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS replacement_readers ON file_replacements(id)
            WHERE json_extract(body,'$.local_ready')=0;
            CREATE UNIQUE INDEX IF NOT EXISTS namespace_remote_objects ON namespace_remote(object);",
        )?;
        tx.pragma_update(None, "user_version", 11)?;
        tx.commit()?;
    }
    db.prepare("SELECT id,source,victim,cleanup,body FROM file_replacements LIMIT 0")?;
    db.prepare(
        "SELECT identity,object FROM namespace_remote INDEXED BY namespace_remote_objects LIMIT 0",
    )?;
    Ok(())
}
fn load(db: &Connection, id: Uuid) -> Result<ReplacementRecord> {
    let body: Option<String> = db
        .query_row(
            "SELECT body FROM file_replacements WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    Ok(serde_json::from_str(&body.ok_or(JournalError::Missing)?)?)
}
pub(super) fn save(db: &Connection, record: &ReplacementRecord) -> Result<()> {
    if db.execute(
        "UPDATE file_replacements SET body=?2,cleanup=?3 WHERE id=?1",
        params![
            record.id.to_string(),
            serde_json::to_string(record)?,
            record.cleanup.to_string()
        ],
    )? != 1
    {
        return Err(JournalError::Missing);
    }
    Ok(())
}
fn next_revision(object: &mut NamespaceObject) -> Result<()> {
    object.revision = object.revision.checked_add(1).ok_or(JournalError::Quota)?;
    Ok(())
}
fn save_working(tx: &Transaction<'_>, file: &WorkingFile) -> Result<()> {
    let slot = if file.unlinked {
        None
    } else {
        Some(namespace::entry_slot(
            tx,
            &file.scope,
            file.node.parent_id.as_deref().ok_or(JournalError::Intent)?,
            &file.node.name,
        )?)
    };
    if tx.execute(
        "UPDATE working_files SET slot=?2,body=?3 WHERE id=?1",
        params![file.id.to_string(), slot, serde_json::to_string(file)?],
    )? != 1
    {
        return Err(JournalError::Stale);
    }
    Ok(())
}

impl UploadJournal {
    /// Atomically take the victim's name for the source's stable identity. The
    /// caller seals dirty streams first and establishes reader preservation.
    /// This queues conditional upload/cleanup; it makes no provider request.
    pub fn replace_namespace_file(
        &mut self,
        source: Uuid,
        source_revision: u64,
        victim: Uuid,
        victim_revision: u64,
        preserve_readers: bool,
    ) -> Result<ReplacementRecord> {
        let source = self.namespace_object(source)?;
        let victim = self.namespace_object(victim)?;
        if source.scope != victim.scope || source.scope.account != self.account {
            return Err(JournalError::Account);
        }
        if source.id == victim.id
            || source.revision != source_revision
            || victim.revision != victim_revision
        {
            return Err(JournalError::Stale);
        }
        for object in [&source, &victim] {
            if object.unlinked || object.follows_remote || !object.remote_owned {
                return Err(JournalError::Stale);
            }
            if object.node.kind != NodeKind::File || object.node.target.is_some() {
                return Err(JournalError::Intent);
            }
        }
        let count: i64 = self
            .db
            .query_row("SELECT count(*) FROM namespace_objects", [], |r| r.get(0))?;
        if count >= 10_000 {
            return Err(JournalError::Quota);
        }
        let source_working = source
            .working_file
            .map(|id| self.working_file(id))
            .transpose()?;
        let victim_working = victim
            .working_file
            .map(|id| self.working_file(id))
            .transpose()?;
        for (object, working) in [
            (&source, source_working.as_ref()),
            (&victim, victim_working.as_ref()),
        ] {
            if working.is_some_and(|w| {
                w.dirty || w.unlinked || w.latest != object.latest || w.node != object.node
            }) {
                return Err(JournalError::Stale);
            }
        }
        let (intent, base) = match victim.latest {
            Some(predecessor) => (
                self.upload_intent_after(predecessor)?.1,
                Some(WriteBase {
                    predecessor,
                    resolved: false,
                }),
            ),
            None => {
                let remote = victim.remote.as_ref().ok_or(JournalError::Stale)?;
                (
                    UploadIntent::Replace {
                        item: remote.id.clone(),
                        expected_etag: remote.etag.clone().ok_or(JournalError::Intent)?,
                    },
                    None,
                )
            }
        };
        let cleanup_request = MutationRequest {
            scope: source.scope.clone(),
            intent: MutationIntent::RemoveFile {
                before: if source.latest.is_some() {
                    source.node.clone()
                } else {
                    source.remote.clone().ok_or(JournalError::Stale)?
                },
            },
        };
        let cleanup_base = if let Some(predecessor) = source.latest {
            self.validate_mutation_base(predecessor, &cleanup_request)?;
            self.ensure_successor_free(predecessor)?;
            Some(WriteBase {
                predecessor,
                resolved: false,
            })
        } else {
            cleanup_request
                .validate()
                .map_err(|_| JournalError::Intent)?;
            None
        };
        let order = WriteOrder {
            base,
            prerequisites: source.latest.into_iter().collect(),
        };
        let scope = source.scope.clone();
        let plan = ReplacementCommit {
            source,
            victim,
            source_working,
            victim_working,
            cleanup_request,
            cleanup_base,
            preserve_readers,
        };
        let upload = if let Some(file) = &plan.source_working {
            let bytes = self.working_descriptor(file.id, false)?;
            if bytes.metadata()?.len() != file.node.size {
                return Err(JournalError::Corrupt);
            }
            self.enqueue_generation(
                scope,
                intent,
                order,
                Some(GenerationCommit::Replacement(Box::new(plan))),
                bytes,
            )?
        } else {
            self.enqueue_preparing_replacement(scope, intent, order, plan)?
        };
        self.replacement(upload.id)
    }
    pub fn replacement(&self, id: Uuid) -> Result<ReplacementRecord> {
        load(&self.db, id)
    }
    pub fn replacement_readers(
        &self,
        after: u64,
        limit: u32,
    ) -> Result<Vec<(u64, ReplacementRecord)>> {
        let mut q=self.db.prepare("SELECT u.sequence,r.body FROM file_replacements r JOIN uploads u ON u.id=r.id
            WHERE u.sequence>?1 AND json_extract(r.body,'$.local_ready')=0 ORDER BY u.sequence LIMIT ?2")?;
        q.query_map(
            params![after.min(i64::MAX as u64) as i64, limit.min(32)],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        )?
        .map(|row| {
            let (seq, body) = row?;
            Ok((seq as u64, serde_json::from_str(&body)?))
        })
        .collect()
    }
    pub fn release_replacement_readers(&mut self, id: Uuid) -> Result<()> {
        let mut r = self.replacement(id)?;
        if r.local_ready {
            return Ok(());
        }
        let state = self.get(id)?.state;
        if state == UploadState::Preparing
            && self.namespace_object(r.source)?.working_file.is_none()
        {
            return Err(JournalError::Stale);
        }
        if !self.namespace_object(r.victim)?.unlinked
            || !matches!(
                state,
                UploadState::Preparing
                    | UploadState::Pending
                    | UploadState::VerifyRequired
                    | UploadState::Conflict
                    | UploadState::Failed
            )
        {
            return Err(JournalError::Corrupt);
        }
        r.local_ready = true;
        save(&self.db, &r)
    }
    pub(super) fn recover_replacement_readers(&mut self) -> Result<()> {
        let mut after = 0;
        loop {
            let pending = self.replacement_readers(after, 32)?;
            if pending.is_empty() {
                return Ok(());
            }
            for (sequence, r) in pending {
                after = sequence;
                if self.get(r.id)?.sha256.len() != 64
                    && self.namespace_object(r.source)?.working_file.is_none()
                {
                    continue;
                }
                self.release_replacement_readers(r.id)?;
            }
        }
    }
}

pub(super) fn locally_ready(db: &Connection, id: Uuid) -> Result<bool> {
    Ok(!db.query_row("SELECT EXISTS(SELECT 1 FROM file_replacements WHERE id=?1 AND json_extract(body,'$.local_ready')=0)",
        [id.to_string()],|r|r.get::<_,bool>(0))?)
}

pub(super) fn receipt_name(db: &Connection, operation: Uuid) -> Result<Option<String>> {
    match load(db, operation) {
        Ok(record) => Ok(Some(namespace::by_id(db, record.source)?.node.name)),
        Err(JournalError::Missing) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Record the actual capture frontier, not a guess made after the Create ack.
/// Only the ordinary empty-Create -> sealed source generation topology gains
/// this authority; old replacement bodies retain None and are not promoted.
fn unconfirmed_source_create(db: &Connection, source: &NamespaceObject) -> Result<Option<Uuid>> {
    let (Some(source_id), Some(working_id)) = (source.latest, source.working_file) else {
        return Ok(None);
    };
    if source.remote.is_some()
        || source.remote_sequence != 0
        || source.native_archive.is_some()
        || source.unlinked
        || source.follows_remote
        || !source.remote_owned
        || source.node.kind != NodeKind::File
        || source.node.package
        || source.node.target.is_some()
    {
        return Ok(None);
    }
    let row: Option<(i64, String, String)> = db
        .query_row(
            "SELECT sequence,state,body FROM uploads WHERE id=?1",
            [source_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((sequence, state, body)) = row else {
        return Ok(None);
    };
    let upload: UploadRecord = serde_json::from_str(&body)?;
    let Some(base) = upload.base.as_ref() else {
        return Ok(None);
    };
    if upload.id != source_id
        || sequence <= 0
        || sequence as u64 != upload.sequence
        || state != "pending"
        || upload.state != UploadState::Pending
        || upload.scope != source.scope
        || !upload.representation.is_file_bytes()
        || upload.working_file != Some(working_id)
        || base.resolved
        || upload.attempt.is_some()
        || upload.remote.is_some()
        || upload.identity_handoff.is_some()
        || upload.package_completion.is_some()
        || upload.session_key.is_some()
        || upload.transferred_bytes != 0
        || upload.size != source.node.size
    {
        return Ok(None);
    }
    let row: Option<(i64, String, String)> = db
        .query_row(
            "SELECT sequence,state,body FROM uploads WHERE id=?1",
            [base.predecessor.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((create_sequence, state, body)) = row else {
        return Ok(None);
    };
    let create: UploadRecord = serde_json::from_str(&body)?;
    let phase = match create.state {
        UploadState::Pending => state == "pending" && create.attempt.is_none(),
        UploadState::Uploading => state == "uploading" && create.attempt.is_some(),
        UploadState::VerifyRequired => state == "verify_required" && create.attempt.is_none(),
        UploadState::Verifying => state == "verifying" && create.attempt.is_some(),
        _ => false,
    };
    if create.id != base.predecessor
        || create_sequence <= 0
        || create_sequence as u64 != create.sequence
        || create.sequence >= upload.sequence
        || !phase
        || create.scope != source.scope
        || !create.representation.is_file_bytes()
        || create.working_file != Some(working_id)
        || create.base.is_some()
        || create.remote.is_some()
        || create.identity_handoff.is_some()
        || create.package_completion.is_some()
        || create.size != 0
        || create.transferred_bytes != 0
        || create.intent != upload.intent
        || create.intent.validate().is_err()
        || !matches!(&create.intent, UploadIntent::Create { name, .. } if name == &source.node.name)
    {
        return Ok(None);
    }
    let body: String = db.query_row(
        "SELECT body FROM working_files WHERE id=?1",
        [working_id.to_string()],
        |row| row.get(0),
    )?;
    let working: WorkingFile = serde_json::from_str(&body)?;
    if working.id != working_id
        || working.native
        || working.unlinked
        || working.dirty
        || working.scope != source.scope
        || working.latest != Some(source_id)
        || working.node != source.node
    {
        return Ok(None);
    }
    let bound: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM write_successors WHERE predecessor=?1 AND successor=?2)
         AND (SELECT count(*) FROM namespace_operations WHERE operation IN (?1,?2) AND object=?3)=2
         AND EXISTS(SELECT 1 FROM write_queue WHERE id=?1 AND sequence=?4 AND complete=0)
         AND EXISTS(SELECT 1 FROM write_queue WHERE id=?2 AND sequence=?5 AND complete=0)
         AND NOT EXISTS(SELECT 1 FROM file_replacements WHERE id IN (?1,?2) OR cleanup IN (?1,?2)
             OR source=?3 OR victim=?3)
         AND NOT EXISTS(SELECT 1 FROM native_working_operations WHERE operation IN (?1,?2)
             OR owner=?3 OR working=?6)
         AND NOT EXISTS(SELECT 1 FROM native_working_heads WHERE working=?6
             OR json_extract(body,'$.owner')=?3)
         AND NOT EXISTS(SELECT 1 FROM native_working_bindings WHERE working=?6)",
        params![
            create.id.to_string(),
            upload.id.to_string(),
            source.id.to_string(),
            create_sequence,
            sequence,
            working_id.to_string()
        ],
        |row| row.get(0),
    )?;
    Ok(bound.then_some(create.id))
}

pub(super) fn commit(
    tx: &Transaction<'_>,
    plan: &ReplacementCommit,
    upload: &UploadRecord,
) -> Result<()> {
    let mut source = namespace::by_id(tx, plan.source.id)?;
    let mut victim = namespace::by_id(tx, plan.victim.id)?;
    if source.revision != plan.source.revision
        || victim.revision != plan.victim.revision
        || source.latest != plan.source.latest
        || victim.latest != plan.victim.latest
        || source.working_file != plan.working_id()
        || upload.size != plan.source.node.size
        || upload.base.as_ref().map(|b| b.predecessor) != victim.latest
    {
        return Err(JournalError::Stale);
    }
    let mut cleanup = MutationRecord {
        id: Uuid::new_v4(),
        sequence: 0,
        request: plan.cleanup_request.clone(),
        state: MutationState::Pending,
        attempt: None,
        receipt: None,
        verified_content: None,
        retry_at: 0,
        failed_attempts: 0,
        base: plan.cleanup_base.clone(),
        working_file: None,
        prepared_item: None,
        local_ready: true,
    };
    cleanup.sequence = mutations::queue_insert(
        tx,
        cleanup.id,
        mutations::mutation_resources(&cleanup.request)?,
    )?;
    generations::insert_dependency(tx, cleanup.id, cleanup.sequence, cleanup.base.as_ref())?;
    barriers::insert(
        tx,
        cleanup.id,
        cleanup.sequence,
        &source.scope,
        &[upload.id],
    )?;
    tx.execute(
        "INSERT INTO mutations VALUES(?1,?2,'pending',?3)",
        params![
            cleanup.sequence as i64,
            cleanup.id.to_string(),
            serde_json::to_string(&cleanup)?
        ],
    )?;
    let record = ReplacementRecord {
        id: upload.id,
        source: source.id,
        victim: victim.id,
        cleanup: cleanup.id,
        cleanup_object: Uuid::new_v4(),
        local_ready: !plan.preserve_readers,
        remote_applied: false,
        rescued_as: None,
        source_unconfirmed_create: unconfirmed_source_create(tx, &plan.source)?,
    };
    let mut source_working = plan.source_working.clone();
    let mut victim_working = plan.victim_working.clone();
    victim.unlinked = true;
    next_revision(&mut victim)?;
    if let Some(file) = &mut victim_working {
        file.unlinked = true;
        file.generation = file.generation.checked_add(1).ok_or(JournalError::Quota)?;
        save_working(tx, file)?;
    }
    // Free both pathname indexes before occupying the destination's slot.
    namespace::save(tx, &victim)?;
    source.node.name = victim.node.name.clone();
    source.node.parent_id = victim.node.parent_id.clone();
    if let Some(file) = &mut source_working {
        file.node = source.node.clone();
        file.latest = Some(upload.id);
        file.generation = file.generation.checked_add(1).ok_or(JournalError::Quota)?;
        save_working(tx, file)?;
    }
    source.latest = Some(upload.id);
    next_revision(&mut source)?;
    namespace::save(tx, &source)?;
    // Reserve metadata capacity now. A confirmed cloud upload must not later
    // fail local ownership publication merely because the object limit was hit.
    let mut cleanup_object = plan.source.clone();
    cleanup_object.id = record.cleanup_object;
    cleanup_object.node.id = format!("local-{}", record.cleanup_object);
    cleanup_object.working_file = None;
    cleanup_object.latest = Some(cleanup.id);
    cleanup_object.revision = 0;
    cleanup_object.unlinked = true;
    cleanup_object.remote_owned = false;
    namespace::save(tx, &cleanup_object)?;
    tx.execute(
        "INSERT INTO file_replacements VALUES(?1,?2,?3,?4,?5)",
        params![
            record.id.to_string(),
            record.source.to_string(),
            record.victim.to_string(),
            record.cleanup.to_string(),
            serde_json::to_string(&record)?
        ],
    )?;
    Ok(())
}

/// Acknowledgement and all three binding changes share the upload transaction.
/// Historical victim metadata remains available to its retained local stream.
/// Resolve the *victim* of a durable editor replacement. The operation's source
/// still owns the temporary upload ID until both provider identities are committed.
pub(super) fn handoff_victim(
    db: &Connection,
    upload: &UploadRecord,
    source: &NamespaceObject,
) -> Result<Option<NamespaceObject>> {
    let record = match load(db, upload.id) {
        Ok(record) => record,
        Err(JournalError::Missing) => return Ok(None),
        Err(error) => return Err(error),
    };
    let victim = namespace::by_id(db, record.victim)?;
    let old = victim.remote.as_ref().ok_or(JournalError::Stale)?;
    if record.source != source.id
        || !record.local_ready
        || record.remote_applied
        || victim.scope != upload.scope
        || source.scope != upload.scope
        || !victim.unlinked
        || !victim.remote_owned
        || old.kind != NodeKind::File
        || old.package
        || old.target.is_some()
        || source.remote.as_ref().is_none_or(|node| node.id == old.id)
        || !matches!(&upload.intent, UploadIntent::Replace { item, expected_etag }
            if item == &old.id && Some(expected_etag) == old.etag.as_ref())
    {
        return Err(JournalError::Stale);
    }
    Ok(Some(victim))
}

pub(super) fn confirm_handoff(
    tx: &Transaction<'_>,
    upload: &UploadRecord,
    remote: &Node,
    old_item: &str,
) -> Result<()> {
    if !confirm_with_victim(tx, upload.id, upload.sequence, remote, old_item)? {
        return Err(JournalError::Corrupt);
    }
    Ok(())
}

pub(super) fn confirm(
    tx: &Transaction<'_>,
    operation: Uuid,
    sequence: u64,
    remote: &Node,
) -> Result<bool> {
    confirm_with_victim(tx, operation, sequence, remote, &remote.id)
}

fn confirm_with_victim(
    tx: &Transaction<'_>,
    operation: Uuid,
    sequence: u64,
    remote: &Node,
    victim_item: &str,
) -> Result<bool> {
    let mut record = match load(tx, operation) {
        Ok(r) => r,
        Err(JournalError::Missing) => return Ok(false),
        Err(e) => return Err(e),
    };
    if record.remote_applied {
        return Ok(true);
    }
    if !record.local_ready {
        return Err(JournalError::Stale);
    }
    let mut source = namespace::by_id(tx, record.source)?;
    let mut victim = namespace::by_id(tx, record.victim)?;
    let mut cleanup = namespace::by_id(tx, record.cleanup_object)?;
    let old_source = source.remote.clone().ok_or(JournalError::Corrupt)?;
    if !source.remote_owned
        || !victim.remote_owned
        || !victim.unlinked
        || victim.remote.as_ref().is_none_or(|n| n.id != victim_item)
        || old_source.id == remote.id
        || source.scope != victim.scope
        || source.remote_sequence >= sequence
        || cleanup.remote_owned
        || !cleanup.unlinked
    {
        return Err(JournalError::Corrupt);
    }
    victim.remote_owned = false;
    next_revision(&mut victim)?;
    namespace::save(tx, &victim)?;
    let old_source_sequence = source.remote_sequence;
    source.remote = Some(remote.clone());
    source.remote_sequence = sequence;
    next_revision(&mut source)?;
    namespace::save(tx, &source)?;
    let identity = cleanup.node.id.clone();
    cleanup.node = old_source.clone();
    cleanup.node.id = identity;
    cleanup.remote = Some(old_source);
    cleanup.remote_sequence = old_source_sequence;
    cleanup.remote_owned = true;
    next_revision(&mut cleanup)?;
    namespace::save(tx, &cleanup)?;
    record.remote_applied = true;
    save(tx, &record)?;
    Ok(true)
}
