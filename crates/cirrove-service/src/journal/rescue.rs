//! Keep a refused current save as a new local file. Its queue entry, namespace
//! move and resolution of the refused operation publish in one transaction.
use super::*;

pub(super) struct RescueCommit {
    original: UploadRecord,
    object: Option<NamespaceObject>,
    working: Option<WorkingFile>,
}
impl RescueCommit {
    pub fn working_id(&self) -> Option<Uuid> {
        self.working.as_ref().map(|w| w.id)
    }
    pub fn scope(&self) -> Scope {
        self.original.scope.clone()
    }
}

pub(super) fn prepare(
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
    journal.ensure_successor_free(id)?;
    let dependent: bool = journal.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM write_prerequisites WHERE predecessor=?1)",
        [id.to_string()],
        |r| r.get(0),
    )?;
    if dependent {
        return Err(JournalError::Stale);
    }
    let object = journal.namespace_for_operation(id)?;
    let working =
        if let Some(object) = &object {
            if object.unlinked
                || object.follows_remote
                || !object.remote_owned
                || object.latest != Some(id)
                || object.node.kind != NodeKind::File
                || object.node.package
                || object.node.target.is_some()
            {
                return Err(JournalError::Stale);
            }
            let replacement: bool = journal.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM file_replacements WHERE id=?1 OR source=?2 OR victim=?2)",
            params![id.to_string(), object.id.to_string()], |r| r.get(0))?;
            if replacement {
                return Err(JournalError::Stale);
            }
            let working = journal.working_file(object.working_file.ok_or(JournalError::Stale)?)?;
            if working.dirty
                || working.unlinked
                || working.latest != Some(id)
                || working.node != object.node
                || working.scope != original.scope
                || original.working_file != Some(working.id)
            {
                return Err(JournalError::Stale);
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
            Some(working)
        } else {
            None
        };
    Ok(RescueCommit {
        original,
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
    let mut original = saved.original.clone();
    if copy.scope != original.scope || copy.sha256 != original.sha256 || copy.size != original.size
    {
        return Err(JournalError::Corrupt);
    }
    if let (Some(previous), Some(working)) = (&saved.object, &saved.working) {
        let mut object = namespace::by_id(tx, previous.id)?;
        if object.revision != previous.revision || object.latest != Some(original.id) {
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
        tx.execute(
            "DELETE FROM namespace_operations WHERE operation=?1",
            [original.id.to_string()],
        )?;
        namespace::save(tx, &object)?;
        if let Some(remote) = &previous.remote {
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
    original.state = UploadState::Resolved;
    if tx.execute("UPDATE uploads SET state='resolved',body=?2 WHERE id=?1 AND state IN ('conflict','failed')",
        params![original.id.to_string(), serde_json::to_string(&original)?])? != 1 { return Err(JournalError::Stale); }
    // prepare refused all downstream dependencies. Resolution releases this
    // queue reservation; it does not pretend there is a provider receipt.
    mutations::queue_complete(tx, original.id, true)?;
    Ok(())
}
