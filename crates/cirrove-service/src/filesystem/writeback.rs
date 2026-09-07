//! Experimental local edit projection. Network hydration never holds the journal
//! or namespace mutex. Ordinary daemon mounts do not construct this layer yet.
use super::*;
use crate::journal::{JournalError, UploadJournal, WorkingFile};
use std::sync::Weak;
use uuid::Uuid;

type Result<T> = std::result::Result<T, Errno>;
type EditKey = (String, String, String, String);

pub(super) struct Writeback {
    journal: Arc<Mutex<UploadJournal>>,
    visible: Mutex<HashMap<EditKey, WorkingFile>>,
    hydrating: Mutex<HashMap<EditKey, Weak<tokio::sync::Mutex<()>>>>,
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
        let files = tokio::task::spawn_blocking(move || {
            let j = load.lock().map_err(|_| JournalError::Storage)?;
            if !j.owns_account(&owner) {
                return Err(JournalError::Account);
            }
            j.working_files()
        })
        .await
        .map_err(std::io::Error::other)?
        .map_err(std::io::Error::other)?;
        Ok(Arc::new(Self {
            journal,
            visible: Mutex::new(
                files
                    .into_iter()
                    .map(|f| (key(&f.scope, &f.node.id), f))
                    .collect(),
            ),
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
    fn publish(&self, record: WorkingFile) -> Result<WorkingFile> {
        let mut visible = self.visible.lock().map_err(|_| Errno::EIO)?;
        let identity = key(&record.scope, &record.node.id);
        if let Some(current) = visible.get(&identity)
            && (current.generation, !current.dirty) > (record.generation, !record.dirty)
        {
            return Ok(current.clone());
        }
        visible.insert(identity, record.clone());
        Ok(record)
    }
    pub fn working(&self, scope: &Scope, item: &str) -> Result<Option<WorkingFile>> {
        Ok(self
            .visible
            .lock()
            .map_err(|_| Errno::EIO)?
            .get(&key(scope, item))
            .cloned())
    }
    pub fn overlay(&self, scope: &Scope, parent: &str, mut nodes: Vec<Node>) -> Result<Vec<Node>> {
        let visible = self.visible.lock().map_err(|_| Errno::EIO)?;
        for record in visible
            .values()
            .filter(|r| r.scope == *scope && r.node.parent_id.as_deref() == Some(parent))
        {
            nodes.retain(|n| {
                n.id != record.node.id && n.name.to_lowercase() != record.node.name.to_lowercase()
            });
            nodes.push(record.node.clone());
        }
        Ok(nodes)
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
                let mut node = view.node.clone();
                if truncate {
                    node.size = 0;
                }
                let size = node.size;
                let mut source = self.local(move |j| j.reserve_working(size)).await?;
                let mut offset = 0;
                while offset < size {
                    let bytes = engine
                        .cache
                        .read(
                            engine.provider.as_ref(),
                            &view.scope,
                            &view.node,
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
                self.publish(record)?
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
        self.publish(record)
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
        self.publish(record)?;
        Ok(count)
    }
    pub async fn truncate(&self, id: Uuid, size: u64) -> Result<WorkingFile> {
        let record = self.local(move |j| j.truncate_working(id, size)).await?;
        self.publish(record)
    }
    pub async fn seal(&self, id: Uuid) -> Result<()> {
        let record = self
            .local(move |j| {
                j.seal_working(id)?;
                j.working_file(id)
            })
            .await?;
        self.publish(record)?;
        Ok(())
    }
}
