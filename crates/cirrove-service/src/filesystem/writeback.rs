//! Experimental local edit projection. Network hydration never holds the journal
//! or namespace mutex. Ordinary daemon mounts do not construct this layer yet.
use super::*;
use crate::journal::{
    JournalError, NamespaceCollision, NamespaceObject, UploadJournal, WorkingFile,
    project_namespace,
};
use std::sync::Weak;
use uuid::Uuid;

type Result<T> = std::result::Result<T, Errno>;
type EditKey = (String, String, String, String);

pub(super) struct Writeback {
    journal: Arc<Mutex<UploadJournal>>,
    pub wake: Arc<tokio::sync::Notify>,
    projection: Mutex<Projection>,
    hydrating: Mutex<HashMap<EditKey, Weak<tokio::sync::Mutex<()>>>>,
}
#[derive(Default)]
struct Projection {
    objects: HashMap<Uuid, NamespaceObject>,
    identities: HashMap<EditKey, Uuid>,
    files: HashMap<Uuid, WorkingFile>,
    conflicts: HashMap<EditKey, Vec<NamespaceCollision>>,
}
impl Projection {
    // A journal snapshot is published atomically, in revision order. A delayed
    // save callback cannot undo a newer rename or its confirmed remote alias.
    fn merge(&mut self, object: NamespaceObject, working: Option<WorkingFile>) -> Result<()> {
        let identity = key(&object.scope, &object.node.id);
        let remote = object.remote.as_ref().map(|r| key(&object.scope, &r.id));
        if let Some(old) = self.objects.get(&object.id) {
            if old.scope != object.scope
                || old.node.id != object.node.id
                || old.names != object.names
            {
                return Err(Errno::EIO);
            }
            if old.revision >= object.revision {
                return Ok(());
            }
        }
        if std::iter::once(&identity)
            .chain(remote.iter())
            .any(|k| self.identities.get(k).is_some_and(|id| *id != object.id))
        {
            return Err(Errno::EIO);
        }
        match &working {
            Some(file)
                if object.working_file == Some(file.id)
                    && file.scope == object.scope
                    && file.node.id == object.node.id
                    && file.node == object.node => {}
            None if object.working_file.is_none() => {}
            _ => return Err(Errno::EIO),
        }
        self.identities.insert(identity, object.id);
        if let Some(remote) = remote {
            self.identities.insert(remote, object.id);
        }
        if let Some(file) = working {
            self.files.insert(file.id, file);
        }
        self.objects.insert(object.id, object);
        Ok(())
    }
    fn object(&self, scope: &Scope, item: &str) -> Option<&NamespaceObject> {
        self.identities
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
            for object in j.namespace_objects()? {
                let working = object
                    .working_file
                    .map(|id| j.working_file(id))
                    .transpose()?;
                projection
                    .merge(object, working)
                    .map_err(|_| JournalError::Corrupt)?;
            }
            Ok(projection)
        })
        .await
        .map_err(std::io::Error::other)?
        .map_err(std::io::Error::other)?;
        Ok(Arc::new(Self {
            journal,
            wake: Arc::new(tokio::sync::Notify::new()),
            projection: Mutex::new(projection),
            hydrating: Mutex::new(HashMap::new()),
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
        let scope = record.scope.clone();
        let item = record.node.id.clone();
        let (object, working) = self
            .local(move |j| {
                let object = j
                    .namespace_by_identity(&scope, &item)?
                    .ok_or(JournalError::Corrupt)?;
                let working = j.working_file(object.working_file.ok_or(JournalError::Corrupt)?)?;
                Ok((object, working))
            })
            .await?;
        let mut projection = self.projection.lock().map_err(|_| Errno::EIO)?;
        projection.merge(object, Some(working))?;
        projection
            .object(&record.scope, &record.node.id)
            .and_then(|o| o.working_file)
            .and_then(|id| projection.files.get(&id))
            .cloned()
            .ok_or(Errno::EIO)
    }
    pub async fn refresh_operation(&self, id: Uuid) -> Result<()> {
        let snapshot = self
            .local(move |j| {
                j.namespace_for_operation(id)?
                    .map(|object| {
                        let working = object
                            .working_file
                            .map(|id| j.working_file(id))
                            .transpose()?;
                        Ok((object, working))
                    })
                    .transpose()
            })
            .await?;
        if let Some((object, working)) = snapshot {
            self.projection
                .lock()
                .map_err(|_| Errno::EIO)?
                .merge(object, working)?;
        }
        Ok(())
    }
    pub fn node(&self, scope: &Scope, item: &str) -> Result<Option<Node>> {
        Ok(self
            .projection
            .lock()
            .map_err(|_| Errno::EIO)?
            .object(scope, item)
            .map(|o| o.node.clone()))
    }
    pub fn working(&self, scope: &Scope, item: &str) -> Result<Option<WorkingFile>> {
        let projection = self.projection.lock().map_err(|_| Errno::EIO)?;
        Ok(projection
            .object(scope, item)
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
    pub async fn relocate(
        &self,
        scope: Scope,
        node: Node,
        source_parent: String,
        source_name: String,
        parent: String,
        name: String,
    ) -> Result<Node> {
        let (object, working) = self
            .local(move |j| {
                let object = match j.namespace_by_identity(&scope, &node.id)? {
                    Some(object) => object,
                    None => j.observe_namespace_file(scope, node)?,
                };
                // Recheck the source at the journal's serialization point. Another
                // rename may have completed after the cached directory was read.
                if object.node.parent_id.as_ref() != Some(&source_parent)
                    || object.node.name != source_name
                {
                    return Err(JournalError::Stale);
                }
                j.relocate_namespace_file(object.id, object.revision, parent, name)?;
                let object = j.namespace_object(object.id)?;
                let working = object
                    .working_file
                    .map(|id| j.working_file(id))
                    .transpose()?;
                Ok((object, working))
            })
            .await?;
        let scope = object.scope.clone();
        let item = object.node.id.clone();
        let mut projection = self.projection.lock().map_err(|_| Errno::EIO)?;
        projection.merge(object, working)?;
        let node = projection
            .object(&scope, &item)
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
                    .object(&view.scope, &view.node.id)
                    .and_then(|o| o.remote.clone())
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
            .namespace_by_identity(&scope, &working.node.id)
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
        let by_remote = projection
            .object(&scope, &remote.id)
            .expect("remote alias retained");
        assert_eq!(by_remote.node.id, working.node.id);
        assert_eq!(by_remote.node.name, "renamed");
        assert_eq!(by_remote.node.parent_id.as_deref(), Some("folder"));
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
}
