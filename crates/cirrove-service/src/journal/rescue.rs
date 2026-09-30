//! Keep a refused current save as a new local file. Its queue entry, namespace
//! move and resolution of the refused operation publish in one transaction.
use super::*;
mod atomic;

pub(super) struct RescueCommit {
    atomic: Option<atomic::AtomicRescue>,
    original: UploadRecord,
    successors: Vec<UploadRecord>,
    object: Option<NamespaceObject>,
    working: Option<WorkingFile>,
}
impl RescueCommit {
    pub fn working_id(&self) -> Option<Uuid> {
        self.working.as_ref().map(|w| w.id)
    }
    fn latest(&self) -> &UploadRecord {
        self.successors.last().unwrap_or(&self.original)
    }
    fn saves(&self) -> impl Iterator<Item = &UploadRecord> {
        std::iter::once(&self.original).chain(&self.successors)
    }
    pub fn payload_id(&self) -> Uuid {
        self.latest().id
    }
    pub fn scope(&self) -> Scope {
        self.original.scope.clone()
    }
}

pub(super) fn prepare(
    journal: &mut UploadJournal,
    id: Uuid,
    parent: &str,
    name: &str,
) -> Result<RescueCommit> {
    let saved = prepare_saved(journal, id, parent, name)?;
    if let Some(working) = &saved.working
        && working.dirty
    {
        // Seal the newest acknowledged bytes before choosing the rescue payload.
        // Failure retains the original conflict and every completed generation.
        journal
            .seal_working(working.id)?
            .ok_or(JournalError::Stale)?;
        return prepare_saved(journal, id, parent, name);
    }
    Ok(saved)
}

fn successors(
    journal: &UploadJournal,
    original: &UploadRecord,
    cleanup: Option<Uuid>,
) -> Result<Vec<UploadRecord>> {
    let mut saves = Vec::new();
    let mut previous = original;
    loop {
        // Other-object prerequisites require their own recovery decision. A
        // resolved save must never masquerade as their provider receipt.
        let dependent: bool = journal.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM write_prerequisites WHERE predecessor=?1 AND (?2 IS NULL OR operation!=?2))",
            params![previous.id.to_string(), cleanup.map(|id| id.to_string())],
            |r| r.get(0),
        )?;
        if dependent {
            return Err(JournalError::Stale);
        }
        let next: Option<String> = journal
            .db
            .query_row(
                "SELECT successor FROM write_successors WHERE predecessor=?1",
                [previous.id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        let Some(next) = next else {
            return Ok(saves);
        };
        if saves.len() >= 10_000 {
            return Err(JournalError::Quota);
        }
        let next = journal.get(Uuid::parse_str(&next).map_err(|_| JournalError::Corrupt)?)?;
        if next.state != UploadState::Pending
            || next.sequence <= previous.sequence
            || next.scope != original.scope
            || next.intent != original.intent
            || next.working_file != original.working_file
            || next
                .base
                .as_ref()
                .is_none_or(|base| base.resolved || base.predecessor != previous.id)
            || next.attempt.is_some()
            || next.remote.is_some()
            || next.session_key.is_some()
            || next.identity_handoff.is_some()
            || next.transferred_bytes != 0
            || next.failed_attempts != 0
        {
            return Err(JournalError::Stale);
        }
        saves.push(next);
        previous = saves.last().ok_or(JournalError::Corrupt)?;
    }
}

fn prepare_saved(
    journal: &UploadJournal,
    id: Uuid,
    parent: &str,
    name: &str,
) -> Result<RescueCommit> {
    let original = journal.get(id)?;
    if !matches!(original.state, UploadState::Conflict | UploadState::Failed) {
        return Err(JournalError::Stale);
    }
    UploadIntent::Create {
        parent: parent.into(),
        name: name.into(),
    }
    .validate()
    .map_err(|_| JournalError::Intent)?;
    let object = journal.namespace_for_operation(id)?;
    let atomic = atomic::prepare(journal, &original, object.as_ref())?;
    let successors = successors(journal, &original, atomic.as_ref().map(|a| a.cleanup_id()))?;
    let latest = successors.last().unwrap_or(&original);
    let working = if let Some(object) = &object {
        if object.unlinked
            || object.follows_remote
            || !object.remote_owned
            || object.latest != Some(latest.id)
            || object.node.kind != NodeKind::File
            || object.node.package
            || object.node.target.is_some()
        {
            return Err(JournalError::Stale);
        }
        let working = journal.working_file(object.working_file.ok_or(JournalError::Stale)?)?;
        if working.unlinked
            || working.latest != Some(latest.id)
            || working.node != object.node
            || working.scope != original.scope
            || original.working_file != Some(working.id)
        {
            return Err(JournalError::Stale);
        }
        for save in &successors {
            if journal
                .namespace_for_operation(save.id)?
                .as_ref()
                .map(|o| o.id)
                != Some(object.id)
            {
                return Err(JournalError::Stale);
            }
        }
        let local_parent = directories::local_parent(&journal.db, &original.scope, parent)?;
        if namespace::entry_slot(&journal.db, &original.scope, &local_parent, name)?
            == namespace::entry_slot(
                &journal.db,
                &original.scope,
                object
                    .node
                    .parent_id
                    .as_deref()
                    .ok_or(JournalError::Intent)?,
                &object.node.name,
            )?
        {
            return Err(JournalError::Intent);
        }
        let slot = namespace::entry_slot(&journal.db, &original.scope, &local_parent, name)?;
        let occupied: bool = journal.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM namespace_entries WHERE slot=?1 AND object!=?2)",
            params![slot, object.id.to_string()],
            |r| r.get(0),
        )?;
        if occupied {
            return Err(JournalError::Stale);
        }
        Some(working)
    } else {
        if original.working_file.is_some() {
            return Err(JournalError::Corrupt);
        }
        None
    };
    Ok(RescueCommit {
        atomic,
        original,
        successors,
        object,
        working,
    })
}

pub(super) fn commit(
    tx: &rusqlite::Transaction<'_>,
    saved: &RescueCommit,
    copy: &UploadRecord,
) -> Result<()> {
    let UploadIntent::Create { parent, name } = &copy.intent else {
        return Err(JournalError::Intent);
    };
    let latest = saved.latest();
    if copy.scope != latest.scope || copy.sha256 != latest.sha256 || copy.size != latest.size {
        return Err(JournalError::Corrupt);
    }
    if let (Some(previous), Some(working)) = (&saved.object, &saved.working) {
        let mut object = namespace::by_id(tx, previous.id)?;
        if object.revision != previous.revision || object.latest != Some(latest.id) {
            return Err(JournalError::Stale);
        }
        let mut working = working.clone();
        working.node.parent_id = Some(directories::local_parent(tx, &copy.scope, parent)?);
        working.node.name = name.clone();
        working.intent = copy.intent.clone();
        working.initial_remote = None;
        working.latest = Some(copy.id);
        // Preserve the working identity/descriptor bytes, but release the old
        // provider binding. The cloud version can then occupy its original name.
        object.node = working.node.clone();
        object.remote = None;
        object.remote_sequence = 0;
        object.latest = Some(copy.id);
        object.revision = object.revision.checked_add(1).ok_or(JournalError::Quota)?;
        if tx.execute(
            "UPDATE working_files SET slot=?2,body=?3 WHERE id=?1",
            params![
                working.id.to_string(),
                working::slot(tx, &working)?,
                serde_json::to_string(&working)?
            ],
        )? != 1
        {
            return Err(JournalError::Missing);
        }
        for save in saved.saves() {
            tx.execute(
                "DELETE FROM namespace_operations WHERE operation=?1",
                [save.id.to_string()],
            )?;
        }
        namespace::save(tx, &object)?;
        let restored = if let Some(atomic) = &saved.atomic {
            Some(atomic::commit(tx, atomic, previous, copy)?)
        } else {
            previous.remote.clone()
        };
        if let Some(remote) = &restored {
            // Older working files may use the provider ID as their local ID.
            // Keep that identity for existing descriptors, and give the cloud
            // entry a distinct durable alias. Fresh listings own its metadata.
            let count: i64 =
                tx.query_row("SELECT count(*) FROM namespace_objects", [], |r| r.get(0))?;
            if count >= 10_000 {
                return Err(JournalError::Quota);
            }
            let id = Uuid::new_v4();
            let mut node = remote.clone();
            node.id = format!("local-{id}");
            directories::localize_parent(tx, &object.scope, &mut node)?;
            namespace::save(
                tx,
                &NamespaceObject {
                    id,
                    scope: object.scope.clone(),
                    names: object.names,
                    node,
                    remote: Some(remote.clone()),
                    remote_owned: true,
                    remote_sequence: 0,
                    working_file: None,
                    latest: None,
                    revision: 0,
                    follows_remote: true,
                    unlinked: false,
                },
            )?;
        }
    }
    for before in saved.saves() {
        let expected = match before.state {
            UploadState::Conflict => "conflict",
            UploadState::Failed => "failed",
            UploadState::Pending => "pending",
            _ => return Err(JournalError::Stale),
        };
        let mut resolved = before.clone();
        resolved.state = UploadState::Resolved;
        if tx.execute(
            "UPDATE uploads SET state='resolved',body=?2 WHERE id=?1 AND state=?3",
            params![
                resolved.id.to_string(),
                serde_json::to_string(&resolved)?,
                expected
            ],
        )? != 1
        {
            return Err(JournalError::Stale);
        }
        // All unresolved descendants belong to this rescue. Retain their
        // immutable payloads and lineage; none can be claimed or sent again.
        mutations::queue_complete(tx, resolved.id, true)?;
    }
    Ok(())
}
