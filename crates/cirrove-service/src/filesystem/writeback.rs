//! Experimental local edit projection. Network hydration never holds the journal
//! or namespace mutex. Ordinary daemon mounts do not construct this layer yet.
mod handoff;
mod publication;
mod unlinked;
use super::*;
use crate::journal::{
    JournalError, NamespaceCollision, NamespaceObject, UploadJournal, WorkingFile,
    project_namespace,
};
pub(super) use handoff::FileLease;
use std::sync::Weak;
pub(super) use unlinked::ReadSource;
use uuid::Uuid;

type Result<T> = std::result::Result<T, Errno>;
type EditKey = (String, String, String, String);

pub(super) struct Writeback {
    journal: Arc<Mutex<UploadJournal>>,
    pub wake: Arc<tokio::sync::Notify>,
    projection: Arc<Mutex<Projection>>,
    hydrating: Mutex<HashMap<EditKey, Weak<tokio::sync::Mutex<()>>>>,
    activity: Mutex<HashMap<EditKey, Weak<tokio::sync::RwLock<()>>>>,
    maintenance_cursor: Mutex<Option<Uuid>>,
    preserving_cursor: Mutex<u64>,
    maintenance_retries: Mutex<HashMap<Uuid, (u32, tokio::time::Instant)>>,
}
#[derive(Default)]
struct Projection {
    frontier: u64,
    objects: HashMap<Uuid, NamespaceObject>,
    local_identities: HashMap<EditKey, Uuid>,
    remote_bindings: HashMap<EditKey, Uuid>,
    files: HashMap<Uuid, WorkingFile>,
    conflicts: HashMap<EditKey, Vec<NamespaceCollision>>,
    streams: HashMap<EditKey, Vec<Weak<OpenFile>>>,
}
impl Projection {
    // A journal snapshot is published atomically, in revision order. A delayed
    // save callback cannot undo a newer rename or its confirmed remote alias.
    fn validate_snapshot(
        &self,
        object: &NamespaceObject,
        working: Option<&WorkingFile>,
    ) -> Result<bool> {
        if !object.remote_owned && !object.unlinked {
            return Err(Errno::EIO);
        }
        if object.follows_remote
            && (object.unlinked
                || object.working_file.is_some()
                || object.latest.is_some()
                || object.remote.is_none()
                || !object.remote_owned)
        {
            return Err(Errno::EIO);
        }
        let identity = key(&object.scope, &object.node.id);
        if let Some(old) = self.objects.get(&object.id) {
            if old.scope != object.scope
                || old.node.id != object.node.id
                || old.names != object.names
            {
                return Err(Errno::EIO);
            }
            if old.revision >= object.revision {
                return Ok(false);
            }
        }
        if self
            .local_identities
            .get(&identity)
            .is_some_and(|id| *id != object.id)
        {
            return Err(Errno::EIO);
        }
        match &working {
            Some(file)
                if object.working_file == Some(file.id)
                    && file.scope == object.scope
                    && file.node.id == object.node.id
                    && file.unlinked == object.unlinked
                    && file.node == object.node => {}
            None if object.working_file.is_none() => {}
            _ => return Err(Errno::EIO),
        }
        Ok(true)
    }
    fn validate_merge(
        &self,
        object: &NamespaceObject,
        working: Option<&WorkingFile>,
    ) -> Result<bool> {
        if !self.validate_snapshot(object, working)? {
            return Ok(false);
        }
        if object.remote_owned
            && object.remote.as_ref().is_some_and(|remote| {
                self.remote_bindings
                    .get(&key(&object.scope, &remote.id))
                    .is_some_and(|id| *id != object.id)
            })
        {
            return Err(Errno::EIO);
        }
        Ok(true)
    }
    fn apply(&mut self, object: NamespaceObject, working: Option<WorkingFile>) {
        let identity = key(&object.scope, &object.node.id);
        if let Some(old) = self.objects.get(&object.id)
            && let Some(old_file) = old.working_file
            && object.working_file != Some(old_file)
        {
            self.files.remove(&old_file);
        }
        // Only local identities address filesystem views and open streams.
        // Provider aliases are projected at the remote-listing boundary.
        self.local_identities.insert(identity, object.id);
        if let Some(old_remote) = self
            .objects
            .get(&object.id)
            .filter(|o| o.remote_owned)
            .and_then(|o| o.remote.as_ref())
        {
            let old_key = key(&object.scope, &old_remote.id);
            if self.remote_bindings.get(&old_key) == Some(&object.id) {
                self.remote_bindings.remove(&old_key);
            }
        }
        if object.remote_owned
            && let Some(remote) = &object.remote
        {
            self.remote_bindings
                .insert(key(&object.scope, &remote.id), object.id);
        }
        if let Some(file) = working {
            self.files.insert(file.id, file);
        }
        self.objects.insert(object.id, object);
    }
    #[cfg(test)]
    fn merge(&mut self, object: NamespaceObject, working: Option<WorkingFile>) -> Result<()> {
        if self.validate_merge(&object, working.as_ref())? {
            self.apply(object, working);
        }
        Ok(())
    }
    fn local_object(&self, scope: &Scope, item: &str) -> Option<&NamespaceObject> {
        self.local_identities
            .get(&key(scope, item))
            .and_then(|id| self.objects.get(id))
    }
}
fn key(scope: &Scope, item: &str) -> EditKey {
    (
        scope.account.clone(),
        scope.provider.clone(),
        scope.collection.clone(),
        item.to_owned(),
    )
}
fn error(error: JournalError) -> Errno {
    match error {
        JournalError::Quota => Errno::ENOSPC,
        JournalError::Missing => Errno::ENOENT,
        JournalError::Account => Errno::EACCES,
        JournalError::Intent => Errno::EINVAL,
        JournalError::Stale => Errno::ESTALE,
        _ => Errno::EIO,
    }
}
impl Writeback {
    pub async fn new(
        engine: &Engine,
        journal: Arc<Mutex<UploadJournal>>,
    ) -> std::io::Result<Arc<Self>> {
        if engine.account.access != cirrove_auth::AccessMode::ReadWrite || engine.account.enabled {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "experimental writes require a disabled, explicitly writable test account",
            ));
        }
        let owner = engine.account.id.clone();
        let load = journal.clone();
        let projection = tokio::task::spawn_blocking(move || {
            let j = load.lock().map_err(|_| JournalError::Storage)?;
            if !j.owns_account(&owner) {
                return Err(JournalError::Account);
            }
            let mut projection = Projection::default();
            projection.catch_up(&j).map_err(|_| JournalError::Corrupt)?;
            Ok(projection)
        })
        .await
        .map_err(std::io::Error::other)?
        .map_err(std::io::Error::other)?;
        Ok(Arc::new(Self {
            journal,
            wake: Arc::new(tokio::sync::Notify::new()),
            projection: Arc::new(Mutex::new(projection)),
            hydrating: Mutex::new(HashMap::new()),
            activity: Mutex::new(HashMap::new()),
            maintenance_cursor: Mutex::new(None),
            preserving_cursor: Mutex::new(0),
            maintenance_retries: Mutex::new(HashMap::new()),
        }))
    }
    async fn local<T: Send + 'static>(
        &self,
        action: impl FnOnce(&mut UploadJournal) -> crate::journal::Result<T> + Send + 'static,
    ) -> Result<T> {
        let journal = self.journal.clone();
        tokio::task::spawn_blocking(move || {
            let mut j = journal.lock().map_err(|_| JournalError::Storage)?;
            action(&mut j)
        })
        .await
        .map_err(|_| Errno::EIO)?
        .map_err(error)
    }
    async fn publish(&self, record: WorkingFile) -> Result<WorkingFile> {
        self.refresh_projection().await?;
        let projection = self.projection.lock().map_err(|_| Errno::EIO)?;
        projection
            .local_object(&record.scope, &record.node.id)
            .and_then(|o| o.working_file)
            .and_then(|id| projection.files.get(&id))
            .cloned()
            .ok_or(Errno::EIO)
    }
    pub async fn refresh_operation(&self, _id: Uuid) -> Result<()> {
        // The operation is a wake hint. Its receipt can change several objects,
        // and a later replacement may already have transferred their bindings.
        self.refresh_projection().await?;
        Ok(())
    }
    pub fn node(&self, scope: &Scope, item: &str) -> Result<Option<Node>> {
        Ok(self
            .projection
            .lock()
            .map_err(|_| Errno::EIO)?
            .local_object(scope, item)
            .filter(|o| !o.follows_remote)
            .map(|o| o.node.clone()))
    }
    pub fn working(&self, scope: &Scope, item: &str) -> Result<Option<WorkingFile>> {
        let projection = self.projection.lock().map_err(|_| Errno::EIO)?;
        Ok(projection
            .local_object(scope, item)
            .and_then(|o| o.working_file)
            .and_then(|id| projection.files.get(&id))
            .cloned())
    }
    pub fn conflicts(&self) -> Result<Vec<NamespaceCollision>> {
        Ok(self
            .projection
            .lock()
            .map_err(|_| Errno::EIO)?
            .conflicts
            .values()
            .flatten()
            .cloned()
            .collect())
    }
    pub fn overlay(&self, scope: &Scope, parent: &str, nodes: Vec<Node>) -> Result<Vec<Node>> {
        let mut projection = self.projection.lock().map_err(|_| Errno::EIO)?;
        let listing =
            project_namespace(projection.objects.values(), scope, parent, nodes).map_err(error)?;
        let identity = key(scope, parent);
        if listing.conflicts.is_empty() {
            projection.conflicts.remove(&identity);
        } else {
            projection.conflicts.insert(identity, listing.conflicts);
        }
        Ok(listing.nodes)
    }
    fn materialize(
        j: &mut UploadJournal,
        scope: Scope,
        mut node: Node,
    ) -> crate::journal::Result<NamespaceObject> {
        if let Some(object) = j.namespace_by_local(&scope, &node.id)? {
            if !object.follows_remote {
                return Ok(object);
            }
            node.id = object.remote.ok_or(JournalError::Corrupt)?.id;
        }
        j.observe_namespace_file(scope, node)
    }
    pub async fn relocate(
        &self,
        scope: Scope,
        node: Node,
        source_parent: String,
        source_name: String,
        parent: String,
        name: String,
    ) -> Result<Node> {
        let object = self
            .local(move |j| {
                let object = Self::materialize(j, scope, node)?;
                // Recheck the source at the journal's serialization point. Another
                // rename may have completed after the cached directory was read.
                if object.node.parent_id.as_ref() != Some(&source_parent)
                    || object.node.name != source_name
                {
                    return Err(JournalError::Stale);
                }
                j.relocate_namespace_file(object.id, object.revision, parent, name)?;
                j.namespace_object(object.id)
            })
            .await?;
        let scope = object.scope.clone();
        let item = object.node.id.clone();
        self.refresh_projection().await?;
        let projection = self.projection.lock().map_err(|_| Errno::EIO)?;
        let node = projection
            .local_object(&scope, &item)
            .ok_or(Errno::EIO)?
            .node
            .clone();
        self.wake.notify_waiters();
        Ok(node)
    }
    pub async fn prepare(
        &self,
        engine: &Engine,
        view: &View,
        truncate: bool,
        cancel: &CancellationToken,
    ) -> Result<WorkingFile> {
        let identity = key(&view.scope, &view.node.id);
        let gate = {
            let mut gates = self.hydrating.lock().map_err(|_| Errno::EIO)?;
            gates.retain(|_, v| v.strong_count() > 0);
            match gates.get(&identity).and_then(Weak::upgrade) {
                Some(gate) => gate,
                None => {
                    let gate = Arc::new(tokio::sync::Mutex::new(()));
                    gates.insert(identity, Arc::downgrade(&gate));
                    gate
                }
            }
        };
        let _guard = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(Errno::ENODEV),
            guard = gate.lock() => guard,
        };
        let working = match self.working(&view.scope, &view.node.id)? {
            Some(working) => working,
            None => {
                if view.reference || view.node.kind != NodeKind::File {
                    return Err(Errno::EOPNOTSUPP);
                }
                let source_node = self
                    .projection
                    .lock()
                    .map_err(|_| Errno::EIO)?
                    .local_object(&view.scope, &view.node.id)
                    .and_then(|o| {
                        if o.follows_remote {
                            o.remote.as_ref().map(|r| Node {
                                id: r.id.clone(),
                                ..view.node.clone()
                            })
                        } else {
                            o.remote.clone()
                        }
                    })
                    .unwrap_or_else(|| view.node.clone());
                if truncate {
                    let scope = view.scope.clone();
                    let node = source_node;
                    let record = self
                        .local(move |j| j.create_truncated_working(scope, node))
                        .await?;
                    return self.publish(record).await;
                }
                let node = source_node;
                let size = node.size;
                let mut source = self.local(move |j| j.reserve_working(size)).await?;
                let mut offset = 0;
                while offset < size {
                    let bytes = engine
                        .cache
                        .read(
                            engine.provider.as_ref(),
                            &view.scope,
                            &node,
                            offset,
                            (size - offset).min(u64::from(crate::content::BLOCK_SIZE)) as u32,
                            cancel,
                        )
                        .await
                        .map_err(|e| errno(&e))?;
                    if bytes.is_empty() {
                        return Err(Errno::EIO);
                    }
                    offset += bytes.len() as u64;
                    source = tokio::task::spawn_blocking(move || {
                        source.write_chunk(&bytes)?;
                        Ok::<_, JournalError>(source)
                    })
                    .await
                    .map_err(|_| Errno::EIO)?
                    .map_err(error)?;
                }
                let scope = view.scope.clone();
                let record = self
                    .local(move |j| j.publish_working(scope, node, false, source))
                    .await?;
                self.publish(record).await?
            }
        };
        if truncate {
            self.truncate(working.id, 0).await
        } else {
            Ok(working)
        }
    }
    pub async fn create(&self, scope: Scope, node: Node) -> Result<WorkingFile> {
        let record = self
            .local(move |j| j.create_working(scope, node, true, &b""[..]))
            .await?;
        self.publish(record).await
    }
    pub async fn read(&self, id: Uuid, offset: u64, count: u32) -> Result<Vec<u8>> {
        self.local(move |j| j.read_working(id, offset, count)).await
    }
    pub async fn write(&self, id: Uuid, offset: u64, bytes: Vec<u8>, append: bool) -> Result<u32> {
        let (count, record) = self
            .local(move |j| {
                let offset = if append {
                    j.working_file(id)?.node.size
                } else {
                    offset
                };
                j.write_working(id, offset, &bytes)
            })
            .await?;
        self.publish(record).await?;
        Ok(count)
    }
    pub async fn truncate(&self, id: Uuid, size: u64) -> Result<WorkingFile> {
        let record = self.local(move |j| j.truncate_working(id, size)).await?;
        self.publish(record).await
    }
    pub async fn seal(&self, id: Uuid) -> Result<()> {
        let record = self
            .local(move |j| {
                j.seal_working(id)?;
                j.working_file(id)
            })
            .await?;
        self.publish(record).await?;
        self.wake.notify_waiters();
        Ok(())
    }
    pub async fn seal_all(&self) -> Result<()> {
        // Continue across per-file failures so every dirty working descriptor
        // receives fsync, even if one snapshot cannot fit in the remaining quota.
        let (files, failed) = self
            .local(|j| {
                let files = j.working_files()?;
                let mut failed = false;
                for file in files.iter().filter(|f| f.dirty) {
                    if j.seal_working(file.id).is_err() {
                        failed = true;
                    }
                }
                Ok((j.working_files()?, failed))
            })
            .await?;
        for file in files {
            self.publish(file).await?;
        }
        self.wake.notify_waiters();
        if failed { Err(Errno::EIO) } else { Ok(()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delayed_save_publication_cannot_restore_an_old_name_or_drop_a_remote_alias() {
        let temp = tempfile::tempdir().expect("temporary state");
        let scope = Scope {
            account: "fixture".into(),
            provider: "fixture".into(),
            collection: "drive".into(),
        };
        let mut journal = UploadJournal::open(&temp.path().join("journal"), &scope.account, 1024)
            .expect("journal");
        let node = Node {
            id: String::new(),
            name: "original".into(),
            parent_id: Some("root".into()),
            kind: NodeKind::File,
            size: 0,
            modified_unix: 1,
            etag: None,
            content_version: None,
            target: None,
        };
        let working = journal
            .create_working(scope.clone(), node, true, &b""[..])
            .expect("create");
        let (_, working) = journal
            .write_working(working.id, 0, b"local")
            .expect("write");
        let old = journal
            .namespace_by_local(&scope, &working.node.id)
            .expect("lookup")
            .expect("object");
        journal
            .relocate_working(working.id, "folder".into(), "renamed".into())
            .expect("rename");
        let renamed = journal.namespace_object(old.id).expect("renamed object");
        let renamed_working = journal.working_file(working.id).expect("working");
        let upload = journal.claim_next().expect("claim").expect("first save");
        let remote = Node {
            id: "assigned-cloud-id".into(),
            etag: Some("etag".into()),
            content_version: Some("content".into()),
            ..working.node.clone()
        };
        journal
            .acknowledge(upload.id, upload.attempt.expect("attempt"), remote.clone())
            .expect("receipt");
        let confirmed = journal.namespace_object(old.id).expect("confirmed");
        let mut projection = Projection::default();
        projection
            .merge(confirmed, Some(renamed_working.clone()))
            .expect("receipt publication");
        projection
            .merge(renamed, Some(renamed_working))
            .expect("late rename publication");
        projection
            .merge(old, Some(working.clone()))
            .expect("late create publication");
        assert!(projection.local_object(&scope, &remote.id).is_none());
        let local = projection
            .local_object(&scope, &working.node.id)
            .expect("local identity retained");
        assert_eq!(local.remote.as_ref().expect("remote binding").id, remote.id);
        assert_eq!(local.node.name, "renamed");
        assert_eq!(local.node.parent_id.as_deref(), Some("folder"));
        assert!(
            project_namespace(projection.objects.values(), &scope, "root", vec![remote])
                .expect("old directory")
                .nodes
                .is_empty()
        );
        let listed = project_namespace(projection.objects.values(), &scope, "folder", vec![])
            .expect("new directory");
        assert_eq!(listed.nodes.len(), 1);
        assert_eq!(listed.nodes[0].name, "renamed");
    }
    #[test]
    fn provider_binding_cannot_redirect_an_existing_local_stream() {
        let temp = tempfile::tempdir().expect("temporary state");
        let scope = Scope {
            account: "fixture".into(),
            provider: "fixture".into(),
            collection: "drive".into(),
        };
        let mut journal = UploadJournal::open(&temp.path().join("journal"), &scope.account, 4096)
            .expect("journal");
        let node = Node {
            id: String::new(),
            name: "old".into(),
            parent_id: Some("root".into()),
            kind: NodeKind::File,
            size: 0,
            modified_unix: 1,
            etag: None,
            content_version: None,
            target: None,
        };
        let first = journal
            .create_working(scope.clone(), node.clone(), true, b"".as_slice())
            .expect("first");
        let second = journal
            .create_working(
                scope.clone(),
                Node {
                    name: "new".into(),
                    ..node
                },
                true,
                b"".as_slice(),
            )
            .expect("second");
        let first_object = journal
            .namespace_by_local(&scope, &first.node.id)
            .expect("lookup")
            .expect("object");
        let mut second_object = journal
            .namespace_by_local(&scope, &second.node.id)
            .expect("lookup")
            .expect("object");
        let mut p = Projection::default();
        p.merge(first_object.clone(), Some(first.clone()))
            .expect("publish first");
        p.merge(second_object.clone(), Some(second.clone()))
            .expect("publish second");
        let old_snapshot = second_object.clone();
        // Model the identity boundary needed by replacement. This does not
        // claim the journal already permits or commits a binding transfer.
        second_object.remote = Some(Node {
            id: first.node.id.clone(),
            etag: Some("ack".into()),
            ..second.node.clone()
        });
        second_object.revision += 1;
        p.merge(second_object.clone(), Some(second.clone()))
            .expect("provider binding has a separate domain");
        assert_eq!(
            p.local_object(&scope, &first.node.id)
                .expect("first stream")
                .working_file,
            Some(first.id)
        );
        assert_eq!(
            p.local_object(&scope, &second.node.id)
                .expect("second stream")
                .working_file,
            Some(second.id)
        );
        assert_eq!(
            p.remote_bindings.get(&key(&scope, &first.node.id)),
            Some(&second_object.id)
        );
        p.merge(old_snapshot, Some(second.clone()))
            .expect("delayed callback");
        assert_eq!(
            p.remote_bindings.get(&key(&scope, &first.node.id)),
            Some(&second_object.id)
        );
        // A provider identity may still have only one current owner.
        let mut duplicate = first_object;
        duplicate.revision += 1;
        duplicate.remote = second_object.remote.clone();
        assert_eq!(p.merge(duplicate, Some(first.clone())), Err(Errno::EIO));
        assert_eq!(
            p.local_object(&scope, &first.node.id)
                .expect("retained stream")
                .working_file,
            Some(first.id)
        );
        // The same opaque provider string in another collection is independent.
        let mut other = second_object;
        other.id = Uuid::new_v4();
        other.scope.collection = "other-drive".into();
        other.remote = Some(Node {
            id: first.node.id.clone(),
            ..second.node.clone()
        });
        other.working_file = None;
        p.merge(other, None).expect("separate scope");
    }
}
