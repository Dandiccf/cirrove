//! Local native scratch streams and atomic canonical ownership transfer.
//! No mounted admission: only a validated canonical working owner grants a temp.
use super::*;

pub(crate) fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS native_temporary_streams(working TEXT PRIMARY KEY,owner TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS native_temporary_owner ON native_temporary_streams(owner);
        CREATE TABLE IF NOT EXISTS native_detached_streams(working TEXT PRIMARY KEY,owner TEXT NOT NULL,binding TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS native_working_transfers(previous_working TEXT PRIMARY KEY,next_working TEXT NOT NULL UNIQUE,owner TEXT NOT NULL,predecessor TEXT,successor TEXT NOT NULL UNIQUE);
        CREATE INDEX IF NOT EXISTS native_transfer_owner ON native_working_transfers(owner,previous_working);")?;
    Ok(())
}
pub(crate) fn local_stream(db: &Connection, id: Uuid) -> Result<bool> {
    Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM native_temporary_streams WHERE working=?1) OR EXISTS(SELECT 1 FROM native_detached_streams WHERE working=?1)", [id.to_string()], |r| r.get(0))?)
}
pub(crate) fn validate_temporary(db: &Connection, object: &NamespaceObject) -> Result<()> {
    let owner_id: String = db.query_row(
        "SELECT owner FROM native_temporary_streams WHERE working=?1",
        [object.id.to_string()],
        |r| r.get(0),
    )?;
    let owner = namespace::by_id(
        db,
        Uuid::parse_str(&owner_id).map_err(|_| JournalError::Corrupt)?,
    )?;
    let body: String = db.query_row(
        "SELECT body FROM working_files WHERE id=?1",
        [object.id.to_string()],
        |r| r.get(0),
    )?;
    let file: WorkingFile = serde_json::from_str(&body)?;
    if object.native_archive.is_some()
        || object.remote_owned
        || object.remote.is_some()
        || object.latest.is_some()
        || object.unlinked != file.unlinked
        || object.follows_remote
        || object.working_file != Some(object.id)
        || file.id != object.id
        || !file.native
        || file.latest.is_some()
        || file.scope != owner.scope
        || object.scope != owner.scope
        || file.node != object.node
        || (!object.unlinked
            && (object.node.parent_id.as_ref() != Some(&owner.node.id)
                || object.node.name == owner.node.name
                || owner.unlinked
                || !owner.remote_owned))
        || !owner.node.package
        || owner.node.kind != NodeKind::Folder
        || owner.node.target.is_some()
        || object.names != owner.names
        || object.remote_sequence != 0
        || object.node.kind != NodeKind::File
        || object.node.package
        || object.node.target.is_some()
        || object.node.id != format!("local-native-archive-{}", file.id)
    {
        return Err(JournalError::Stale);
    }
    Ok(())
}
/// Edges are created only by the same transaction as a typed replacement row.
/// Never replace immutable operation provenance with the newest working UUID.
pub(super) fn descendant(db: &Connection, owner: Uuid, from: Uuid, to: Uuid) -> Result<bool> {
    if from == to {
        return Ok(true);
    }
    Ok(db.query_row("WITH RECURSIVE chain(id,depth) AS (SELECT ?1,0 UNION ALL SELECT t.next_working,c.depth+1 FROM chain c JOIN native_working_transfers t ON t.previous_working=c.id JOIN uploads u ON u.id=t.successor JOIN native_working_operations n ON n.operation=t.successor AND n.working=t.next_working AND n.owner=t.owner WHERE t.owner=?3 AND c.depth<10000 AND json_extract(u.body,'$.representation.kind')='package_replacement_archive' AND json_extract(u.body,'$.base.predecessor') IS t.predecessor) SELECT EXISTS(SELECT 1 FROM chain WHERE id=?2)", params![from.to_string(),to.to_string(),owner.to_string()], |r| r.get(0))?)
}
pub(super) fn active(db: &Connection, owner: Uuid) -> Result<(Uuid, successors::Head)> {
    let (id, body): (String, String) = db.query_row(
        "SELECT working,body FROM native_working_heads WHERE json_extract(body,'$.owner')=?1",
        [owner.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok((
        Uuid::parse_str(&id).map_err(|_| JournalError::Corrupt)?,
        serde_json::from_str(&body)?,
    ))
}
fn equal<T: Serialize>(a: &T, b: &T) -> Result<bool> {
    Ok(serde_json::to_string(a)? == serde_json::to_string(b)?)
}

pub(super) struct Transfer {
    temporary: WorkingFile,
    victim: WorkingFile,
    temporary_object: NamespaceObject,
    victim_object: NamespaceObject,
    owner: NamespaceObject,
    head: successors::Head,
}
/// All authority is captured from a live canonical child, never from temp name.
pub struct NativeTemporaryCapture {
    transfer: Transfer,
    capture: NativeWorkingCapture,
}
pub struct CapturedNativeTemporary {
    transfer: Transfer,
    captured: CapturedNativeWorking,
}
impl NativeTemporaryCapture {
    pub fn capture(self, cancel: &CancellationToken) -> Result<CapturedNativeTemporary> {
        Ok(CapturedNativeTemporary {
            transfer: self.transfer,
            captured: self.capture.capture(cancel)?,
        })
    }
}
impl UploadJournal {
    /// Empty, local-only stream. Its native qualifier rejects ordinary sealing.
    /// Caller must separately admit the current account and ancestor route.
    pub fn create_native_temporary(
        &mut self,
        canonical: Uuid,
        name: String,
    ) -> Result<WorkingFile> {
        let child = self.namespace_object(canonical)?;
        projection::validate_child(&self.db, &child)?;
        let role = child.native_archive.as_ref().ok_or(JournalError::Stale)?;
        let owner = self.namespace_object(role.source_owner)?;
        if owner.scope.account != self.account
            || child.unlinked
            || name == child.node.name
            || name == owner.node.name
        {
            return Err(JournalError::Intent);
        }
        UploadIntent::Create {
            parent: owner.node.id.clone(),
            name: name.clone(),
        }
        .validate()
        .map_err(|_| JournalError::Intent)?;
        let id = Uuid::new_v4();
        let mut node = child.node.clone();
        node.id = format!("local-native-archive-{id}");
        node.name = name;
        node.size = 0;
        node.content_version = Some(format!("native-temporary-{id}"));
        let head = successors::head(&self.db, canonical)?;
        let file = WorkingFile {
            id,
            scope: child.scope.clone(),
            node: node.clone(),
            intent: UploadIntent::Replace {
                item: head.current.id,
                expected_etag: head.current.etag.ok_or(JournalError::Corrupt)?,
            },
            latest: None,
            dirty: true,
            generation: 0,
            initial_remote: None,
            unlinked: false,
            native: true,
        };
        let object = NamespaceObject {
            native_archive: None,
            id,
            scope: child.scope,
            names: child.names,
            node,
            remote: None,
            remote_owned: false,
            remote_sequence: 0,
            working_file: Some(id),
            latest: None,
            revision: 1,
            follows_remote: false,
            unlinked: false,
        };
        if self
            .db
            .query_row("SELECT count(*) FROM namespace_objects", [], |r| {
                r.get::<_, i64>(0)
            })?
            >= 10_000
        {
            return Err(JournalError::Quota);
        }
        let source = self.reserve_working(0)?;
        source.temporary.as_file().sync_all()?;
        source
            .temporary
            .persist_noclobber(self.working.join(id.to_string()))
            .map_err(|_| JournalError::Storage)?;
        File::open(&self.working)?.sync_all()?;
        let tx = self.db.transaction()?;
        if tx.query_row("SELECT count(*) FROM namespace_objects", [], |r| {
            r.get::<_, i64>(0)
        })? >= 10_000
        {
            return Err(JournalError::Quota);
        }
        tx.execute(
            "INSERT INTO working_files(id,identity,slot,body) VALUES(?1,?2,?3,?4)",
            params![
                id.to_string(),
                serde_json::to_string(&(&file.scope, &file.node.id))?,
                slot(&tx, &file)?,
                serde_json::to_string(&file)?
            ],
        )?;
        tx.execute(
            "INSERT INTO native_temporary_streams VALUES(?1,?2)",
            params![id.to_string(), owner.id.to_string()],
        )?;
        namespace::save(&tx, &object)?;
        tx.commit()?;
        Ok(file)
    }
    /// Local durability only. No upload, namespace operation, or cloud cleanup.
    pub fn sync_native_temporary(&self, id: Uuid) -> Result<()> {
        let exists: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM native_temporary_streams WHERE working=?1)",
            [id.to_string()],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(JournalError::Intent);
        }
        self.working_descriptor(id, false)?.sync_all()?;
        Ok(())
    }
    pub fn capture_native_temporary(
        &mut self,
        temporary: Uuid,
        canonical: Uuid,
    ) -> Result<NativeTemporaryCapture> {
        let victim = self.working_file(canonical)?;
        let temp = self.working_file(temporary)?;
        let child = self.namespace_object(canonical)?;
        let temp_object = self.namespace_object(temporary)?;
        projection::validate_child(&self.db, &child)?;
        let owner = self.namespace_object(
            child
                .native_archive
                .as_ref()
                .ok_or(JournalError::Stale)?
                .source_owner,
        )?;
        let bound: String = self.db.query_row(
            "SELECT owner FROM native_temporary_streams WHERE working=?1",
            [temporary.to_string()],
            |r| r.get(0),
        )?;
        if temporary == canonical
            || bound != owner.id.to_string()
            || temp.scope != victim.scope
            || temp.scope.account != self.account
            || temp.unlinked
            || victim.unlinked
            || !temp.native
            || !temp.dirty
            || temp.latest.is_some()
            || temp_object.remote_owned
            || temp_object.native_archive.is_some()
            || temp_object.node.parent_id != child.node.parent_id
            || temp_object.node.name == child.node.name
        {
            return Err(JournalError::Stale);
        }
        let binding = self.native_binding(canonical)?;
        let file = self.working_descriptor(temporary, false)?;
        let size = file.metadata()?.len();
        if size == 0 || size > MAX_ARCHIVE || size != temp.node.size {
            return Err(JournalError::Intent);
        }
        let staging = self.reserve_working_named(size, ".native-capture-")?;
        Ok(NativeTemporaryCapture {
            transfer: Transfer {
                temporary: temp.clone(),
                victim,
                temporary_object: temp_object,
                victim_object: child,
                owner,
                head: successors::head(&self.db, canonical)?,
            },
            capture: NativeWorkingCapture {
                binding,
                record: temp,
                file,
                staging,
            },
        })
    }
    pub fn replace_native_temporary(
        &mut self,
        ready: CapturedNativeTemporary,
        cancel: &CancellationToken,
    ) -> Result<UploadRecord> {
        self.replace_native_temporary_observed(ready, cancel, |_| {})
    }
    fn replace_native_temporary_observed(
        &mut self,
        ready: CapturedNativeTemporary,
        cancel: &CancellationToken,
        committed: impl FnOnce(&UploadRecord),
    ) -> Result<UploadRecord> {
        let CapturedNativeTemporary { transfer, captured } = ready;
        recheck(&self.db, &transfer)?;
        if cancel.is_cancelled() {
            return Err(JournalError::Stale);
        }
        let (original, original_semantic) =
            successors::provisional(&self.db, &transfer.victim, &captured.binding)?;
        let intent = UploadIntent::Replace {
            item: original.id.clone(),
            expected_etag: original.etag.clone().ok_or(JournalError::Corrupt)?,
        };
        let representation = UploadRepresentation::PackageReplacementArchive {
            expected_root: captured.binding.archive.name.clone(),
            semantic: captured.semantic,
            original: Box::new(original),
            original_semantic,
        };
        let mut record = transfer.temporary.clone();
        record.node.name = transfer.owner.node.name.clone();
        record.latest = transfer.victim.latest;
        record.intent = intent.clone();
        let order = WriteOrder {
            base: record.latest.map(|predecessor| WriteBase {
                predecessor,
                resolved: false,
            }),
            prerequisites: Vec::new(),
        };
        let saved = self.enqueue_admitted_source(
            record.scope.clone(),
            intent.clone(),
            order,
            Some(GenerationCommit::Native(Box::new(NativeCommit {
                transfer: Some(transfer),
                record,
                binding: captured.binding,
                intent,
                representation: representation.clone(),
            }))),
            representation::Admission {
                representation,
                receipt: Some(representation::BoundReceipt {
                    size: captured.size,
                    sha256: captured.sha256.clone(),
                    cancel: cancel.clone(),
                }),
            },
            std::io::empty(),
            Some(PreparedNativeObject {
                file: captured.file,
                path: captured._reservation,
                owner: captured._owner,
                size: captured.size,
                sha256: captured.sha256,
            }),
        )?;
        // Cancellation after durable publication must never hide this operation.
        committed(&saved);
        Ok(saved)
    }
}
fn recheck(db: &Connection, t: &Transfer) -> Result<()> {
    validate_temporary(db, &t.temporary_object)?;
    projection::validate_child(db, &t.victim_object)?;
    for selected in [&t.temporary, &t.victim] {
        let body: String = db.query_row(
            "SELECT body FROM working_files WHERE id=?1",
            [selected.id.to_string()],
            |r| r.get(0),
        )?;
        let actual: WorkingFile = serde_json::from_str(&body)?;
        if !equal(&actual, selected)? {
            return Err(JournalError::Stale);
        }
    }
    for selected in [&t.temporary_object, &t.victim_object, &t.owner] {
        if !equal(&namespace::by_id(db, selected.id)?, selected)? {
            return Err(JournalError::Stale);
        }
    }
    if !equal(&successors::head(db, t.victim.id)?, &t.head)? {
        return Err(JournalError::Stale);
    }
    Ok(())
}
/// Runs inside the existing immutable-upload enqueue transaction, before attach.
pub(crate) fn transfer(
    tx: &rusqlite::Transaction<'_>,
    commit: &NativeCommit,
    row: &UploadRecord,
) -> Result<()> {
    let Some(t) = &commit.transfer else {
        return Ok(());
    };
    recheck(tx, t)?;
    let actual_binding: String = tx.query_row(
        "SELECT body FROM native_working_bindings WHERE working=?1",
        [t.victim.id.to_string()],
        |r| r.get(0),
    )?;
    if !equal(
        &serde_json::from_str::<Binding>(&actual_binding)?,
        &commit.binding,
    )? || row.base.as_ref().map(|b| b.predecessor) != t.victim.latest
    {
        return Err(JournalError::Stale);
    }
    let backed_up = backup::gap(tx, t.victim.id)?;
    let mut old = t.victim.clone();
    old.unlinked = backed_up.is_none();
    let mut old_object = t.victim_object.clone();
    old_object.unlinked = backed_up.is_none();
    old_object.native_archive = None;
    old_object.revision = old_object
        .revision
        .checked_add(1)
        .ok_or(JournalError::Quota)?;
    tx.execute(
        "UPDATE working_files SET slot=NULL,body=?2 WHERE id=?1",
        params![old.id.to_string(), serde_json::to_string(&old)?],
    )?;
    tx.execute(
        "INSERT INTO native_detached_streams VALUES(?1,?2,?3)",
        params![old.id.to_string(), t.owner.id.to_string(), actual_binding],
    )?;
    if let Some(gap) = backed_up {
        backup::detach(tx, &gap)?;
    }
    tx.execute(
        "DELETE FROM native_working_bindings WHERE working=?1",
        [old.id.to_string()],
    )?;
    tx.execute(
        "INSERT INTO native_working_bindings VALUES(?1,?2,?3)",
        params![
            commit.record.id.to_string(),
            serde_json::to_string(&(&row.scope, &t.head.current.id))?,
            serde_json::to_string(&commit.binding)?
        ],
    )?;
    tx.execute(
        "UPDATE native_working_heads SET working=?2 WHERE working=?1",
        params![old.id.to_string(), commit.record.id.to_string()],
    )?;
    tx.execute(
        "INSERT INTO native_working_operations VALUES(?1,?2,?3)",
        params![
            row.id.to_string(),
            commit.record.id.to_string(),
            t.owner.id.to_string()
        ],
    )?;
    tx.execute(
        "INSERT INTO native_working_transfers VALUES(?1,?2,?3,?4,?5)",
        params![
            old.id.to_string(),
            commit.record.id.to_string(),
            t.owner.id.to_string(),
            t.victim.latest.map(|id| id.to_string()),
            row.id.to_string()
        ],
    )?;
    namespace::save(tx, &old_object)?;
    let mut child = t.temporary_object.clone();
    child.node = commit.record.node.clone();
    child.native_archive = t.victim_object.native_archive.clone();
    child
        .native_archive
        .as_mut()
        .ok_or(JournalError::Corrupt)?
        .backed_up = false;
    child
        .native_archive
        .as_mut()
        .ok_or(JournalError::Corrupt)?
        .working = commit.record.id;
    child.revision = child.revision.checked_add(1).ok_or(JournalError::Quota)?;
    tx.execute(
        "UPDATE working_files SET body=?2 WHERE id=?1",
        params![
            commit.record.id.to_string(),
            serde_json::to_string(&commit.record)?
        ],
    )?;
    namespace::save(tx, &child)?;
    tx.execute(
        "DELETE FROM native_temporary_streams WHERE working=?1",
        [commit.record.id.to_string()],
    )?;
    Ok(())
}
#[cfg(test)]
mod tests;

/// Classification is joined to the committed namespace snapshot, never guessed
/// from native=true or a local ID prefix by the filesystem consumer.
pub(crate) fn snapshot_role(
    db: &Connection,
    object: &NamespaceObject,
) -> Result<Option<crate::journal::NativeLocalStream>> {
    let temp: Option<String> = db
        .query_row(
            "SELECT owner FROM native_temporary_streams WHERE working=?1",
            [object.id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    let detached: Option<(String, String)> = db
        .query_row(
            "SELECT owner,binding FROM native_detached_streams WHERE working=?1",
            [object.id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match (temp, detached) {
        (Some(owner), None) => {
            validate_temporary(db, object)?;
            Ok(Some(crate::journal::NativeLocalStream {
                source_owner: Uuid::parse_str(&owner).map_err(|_| JournalError::Corrupt)?,
                detached: object.unlinked,
                backup: false,
            }))
        }
        (None, Some((owner, binding))) => {
            let binding: Binding = serde_json::from_str(&binding)?;
            binding.validate()?;
            if (!object.unlinked && !backup::validate_local_backup(db, object)?)
                || object.remote_owned
                || object.remote.is_some()
                || object.native_archive.is_some()
                || object.latest.is_some()
                || object.scope != binding.scope
            {
                return Err(JournalError::Corrupt);
            }
            Ok(Some(crate::journal::NativeLocalStream {
                source_owner: Uuid::parse_str(&owner).map_err(|_| JournalError::Corrupt)?,
                detached: true,
                backup: backup::validate_local_backup(db, object)?,
            }))
        }
        (None, None) => Ok(None),
        _ => Err(JournalError::Corrupt),
    }
}
impl UploadJournal {
    pub(crate) fn native_local_stream(
        &self,
        id: Uuid,
    ) -> Result<Option<crate::journal::NativeLocalStream>> {
        snapshot_role(&self.db, &self.namespace_object(id)?)
    }
    pub(crate) fn sync_native_local_stream(&self, id: Uuid) -> Result<bool> {
        if self.native_local_stream(id)?.is_none() && backup::gap(&self.db, id)?.is_none() {
            return Ok(false);
        }
        self.working_descriptor(id, false)?.sync_all()?;
        Ok(true)
    }
}
