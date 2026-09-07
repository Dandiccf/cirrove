//! FUSE projection: ordinary mounts are read-only; isolated test mounts allow edits.
//! Callbacks dispatch asynchronous work; no callback
//! holds the namespace map while awaiting a provider or a database operation.
mod lifecycle;
mod session;
pub(crate) use lifecycle::WriteControl;
mod writeback;
use crate::engine::Engine;
use cirrove_core::{CancellationToken, Node, NodeKind, ProviderError, Scope};
use cirrove_store::Store;
use fuser::{
    Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo, LockOwner,
    OpenFlags, RenameFlags, ReplyAttr, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry,
    ReplyOpen, ReplyXattr, Request,
};
pub use session::CloudSession;
use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    os::unix::fs::MetadataExt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, UNIX_EPOCH},
};
use tokio::{runtime::Handle, sync::Semaphore};

const TTL: Duration = Duration::from_secs(1);
const READ_QUEUE_TIMEOUT: Duration = Duration::from_secs(30);
#[derive(Clone)]
struct View {
    inode: u64,
    parent: u64,
    scope: Scope,
    node: Node,
    name: String,
    alias: Vec<(String, String)>,
    reference: bool,
    ancestry: Vec<(String, String)>,
}
#[derive(Clone)]
struct OpenFile {
    view: View,
    node: Node,
    flags: i32,
    _lease: Option<writeback::FileLease>,
    remote_reads: tokio_util::task::TaskTracker,
}
pub struct CloudFs {
    inner: Arc<Inner>,
}
struct Inner {
    engine: Arc<Engine>,
    writeback: Option<Arc<writeback::Writeback>>,
    edits: lifecycle::EditAdmission,
    runtime: Handle,
    views: Mutex<HashMap<u64, View>>,
    files: Mutex<HashMap<u64, Arc<OpenFile>>>,
    directories: Mutex<HashMap<u64, Arc<Vec<View>>>>,
    next_handle: AtomicU64,
    pending: Arc<Semaphore>,
    admitted_reads: Arc<Semaphore>,
    reads: Arc<Semaphore>,
    writes: Arc<Semaphore>,
    uid: u32,
    gid: u32,
    cancel: CancellationToken,
}
impl CloudFs {
    fn finish_handle(&self, handle: FileHandle, reply: ReplyEmpty) {
        let file = self
            .inner
            .files
            .lock()
            .ok()
            .and_then(|f| f.get(&handle.0).cloned());
        let Some(file) = file else {
            reply.error(Errno::EBADF);
            return;
        };
        // Read-only previews must not seal someone else's unfinished generation.
        if file.flags & libc::O_ACCMODE == libc::O_RDONLY {
            reply.ok();
            return;
        }
        let Ok(admission) = self.inner.edits.admit() else {
            reply.error(Errno::ENODEV);
            return;
        };
        let inner = self.inner.clone();
        self.inner.runtime.spawn(async move {
            let _admission = admission;
            if let Some(writer) = &inner.writeback {
                match writer.working(&file.view.scope, &file.view.node.id) {
                    Ok(Some(record)) => match writer.seal(record.id).await {
                        Ok(()) => reply.ok(),
                        Err(e) => reply.error(e),
                    },
                    Ok(None) => reply.ok(),
                    Err(e) => reply.error(e),
                }
            } else {
                reply.ok();
            }
        });
    }
    /// Developer-only writable sessions. The manager still uses `new` (read-only).
    /// The supplied account must be disabled and explicitly granted write access.
    pub async fn new_experimental_writable(
        engine: Arc<Engine>,
        journal: Arc<Mutex<crate::journal::UploadJournal>>,
    ) -> std::io::Result<Self> {
        let writer = writeback::Writeback::new(&engine, journal).await?;
        let mut fs = Self::new(engine)?;
        Arc::get_mut(&mut fs.inner)
            .ok_or_else(|| std::io::Error::other("filesystem already shared"))?
            .writeback = Some(writer);
        Ok(fs)
    }
    pub fn new(engine: Arc<Engine>) -> std::io::Result<Self> {
        let owner = std::fs::metadata("/proc/self")?;
        let root = Node {
            id: engine.account.root_id.clone(),
            parent_id: None,
            name: engine.account.label.clone(),
            kind: NodeKind::Folder,
            size: 0,
            modified_unix: 0,
            etag: None,
            content_version: None,
            target: None,
        };
        let scope = engine.scope(&engine.account.drive.id);
        let root = View {
            inode: 1,
            parent: 1,
            ancestry: vec![(scope.collection.clone(), root.id.clone())],
            scope,
            node: root,
            name: engine.account.label.clone(),
            alias: vec![],
            reference: false,
        };
        Ok(Self {
            inner: Arc::new(Inner {
                cancel: engine.cancel.child_token(),
                engine,
                writeback: None,
                edits: lifecycle::EditAdmission::new(),
                runtime: Handle::current(),
                views: Mutex::new(HashMap::from([(1, root)])),
                files: Mutex::new(HashMap::new()),
                directories: Mutex::new(HashMap::new()),
                next_handle: AtomicU64::new(1),
                pending: Arc::new(Semaphore::new(128)),
                admitted_reads: Arc::new(Semaphore::new(1024)),
                reads: Arc::new(Semaphore::new(32)),
                writes: Arc::new(Semaphore::new(32)),
                uid: owner.uid(),
                gid: owner.gid(),
            }),
        })
    }
    pub fn start_invalidations(&self, notifier: fuser::Notifier) {
        let inner = self.inner.clone();
        self.inner.runtime.spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = inner.cancel.cancelled() => break,
                    _ = inner.engine.changed.notified() => {}
                }
                let entries = inner
                    .views
                    .lock()
                    .map(|views| {
                        views
                            .values()
                            .map(|view| {
                                (
                                    view.inode,
                                    view.parent,
                                    view.name.clone(),
                                    view.node.kind == NodeKind::Folder,
                                )
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let notifier = notifier.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    for (inode, parent, name, directory) in entries {
                        // Root attributes are synthetic and fixed for the mount.
                        // Invalidating them while its first GETATTR is in flight
                        // can discard that reply and leave initial kernel owner/
                        // permissions in place. Child dentries are invalidated
                        // individually below; directory handles do not cache data.
                        if inode == 1 {
                            continue;
                        }
                        // File revisions have separate inodes. Preserve pages of
                        // an old open mapping; refresh attributes and path lookup.
                        let offset = if directory { 0 } else { -1 };
                        let _ = notifier.inval_inode(INodeNo(inode), offset, 0);
                        let _ = notifier.inval_entry(INodeNo(parent), OsStr::new(&name));
                    }
                })
                .await;
            }
        });
    }
    pub fn mount(self, path: &std::path::Path) -> std::io::Result<CloudSession> {
        let mut config = fuser::Config::default();
        config.mount_options = vec![
            if self.inner.writeback.is_some() {
                fuser::MountOption::RW
            } else {
                fuser::MountOption::RO
            },
            fuser::MountOption::NoDev,
            fuser::MountOption::NoSuid,
            fuser::MountOption::DefaultPermissions,
            fuser::MountOption::FSName(format!("cirrove:{}", self.inner.engine.account.id)),
            fuser::MountOption::Subtype("cirrove".into()),
        ];
        let inner = self.inner.clone();
        let session = fuser::Session::new(self, path, &config)?;
        let notifier = session.notifier();
        let session = CloudSession::start(
            session,
            path,
            &format!("cirrove:{}", inner.engine.account.id),
            inner.cancel.clone(),
        )?;
        CloudFs { inner }.start_invalidations(notifier);
        Ok(session)
    }
}
impl Inner {
    fn view(&self, inode: u64) -> Result<View, ProviderError> {
        self.views
            .lock()
            .map_err(|_| ProviderError::Unavailable)?
            .get(&inode)
            .cloned()
            .ok_or(ProviderError::NotFound)
    }
    fn project(parent: &View, child: Node) -> Result<View, ProviderError> {
        if child.name.is_empty()
            || child.name == "."
            || child.name == ".."
            || child.name.contains('/')
            || child.name.contains('\0')
        {
            return Err(ProviderError::Protocol("invalid cloud filename"));
        }
        let name = child.name.clone();
        let mut scope = parent.scope.clone();
        let mut alias = parent.alias.clone();
        let mut ancestry = parent.ancestry.clone();
        let mut node = child.clone();
        if let Some(target) = &child.target {
            scope.collection = target.collection.clone();
            if ancestry
                .iter()
                .any(|v| v == &(target.collection.clone(), target.item.clone()))
                || ancestry.len() > 128
            {
                return Err(ProviderError::Protocol("shortcut cycle or excessive depth"));
            }
            alias.push((parent.scope.collection.clone(), child.id.clone()));
            node.id = target.item.clone();
            node.kind = target.kind.clone().unwrap_or(NodeKind::Folder);
            node.etag = None;
            node.content_version = None;
            node.target = None;
        }
        if node.kind == NodeKind::Folder {
            ancestry.push((scope.collection.clone(), node.id.clone()));
        }
        let view = View {
            inode: 0,
            parent: parent.inode,
            scope,
            node,
            name,
            alias,
            reference: child.target.is_some(),
            ancestry,
        };
        Ok(view)
    }
    fn inode_key(view: &View, writable: bool) -> Result<String, ProviderError> {
        let identity = (
            &view.scope.account,
            &view.alias,
            &view.scope.collection,
            &view.node.id,
        );
        // Regular-file revisions have independent kernel page caches. Stable
        // provider identity remains account/drive/item; names never enter the key.
        if view.node.kind == NodeKind::File && !writable {
            serde_json::to_string(&(
                "content-inode-v1",
                identity,
                view.node.content_revision(),
                view.node.size,
            ))
        } else {
            serde_json::to_string(&identity)
        }
        .map_err(|_| ProviderError::Unavailable)
    }
    async fn insert(&self, parent: &View, child: Node) -> Result<View, ProviderError> {
        let mut view = Self::project(parent, child)?;
        if view.reference {
            view.node = self.engine.node(&view.scope, &view.node.id).await?;
        }
        let key = Self::inode_key(&view, self.writeback.is_some())?;
        let db = self.engine.db.clone();
        view.inode = tokio::task::spawn_blocking(move || Store::open(db)?.inode(&key))
            .await
            .map_err(|_| ProviderError::Unavailable)?
            .map_err(|_| ProviderError::Unavailable)?;
        self.views
            .lock()
            .map_err(|_| ProviderError::Unavailable)?
            .insert(view.inode, view.clone());
        Ok(view)
    }
    async fn listing(&self, inode: u64) -> Result<Vec<View>, ProviderError> {
        let parent = self.view(inode)?;
        let nodes = self.children(&parent).await?;
        let mut result = vec![
            View {
                name: ".".into(),
                ..parent.clone()
            },
            View {
                name: "..".into(),
                ..self.view(parent.parent)?
            },
        ];
        let mut projected = vec![];
        for node in nodes {
            match Self::project(&parent, node) {
                Ok(view) => projected.push(view),
                Err(ProviderError::Protocol(_)) => {
                    tracing::warn!("cloud entry could not be projected (cycle or invalid name)")
                }
                Err(error) => return Err(error),
            }
        }
        let db = self.engine.db.clone();
        let writable = self.writeback.is_some();
        let projected = tokio::task::spawn_blocking(move || -> Result<Vec<View>, ProviderError> {
            let mut store = Store::open(db).map_err(|_| ProviderError::Unavailable)?;
            for view in &mut projected {
                // A cold link may have a provisional directory-entry inode until
                // lookup resolves its target. Listing never waits for that network
                // request; only cached target metadata participates here.
                if view.reference
                    && let Some(node) = store
                        .node(&view.scope, &view.node.id)
                        .map_err(|_| ProviderError::Unavailable)?
                {
                    view.node = node;
                }
            }
            let keys = projected
                .iter()
                .map(|view| Self::inode_key(view, writable))
                .collect::<Result<Vec<_>, _>>()?;
            let inodes = store
                .inodes(&keys)
                .map_err(|_| ProviderError::Unavailable)?;
            for (view, inode) in projected.iter_mut().zip(inodes) {
                view.inode = inode;
            }
            Ok(projected)
        })
        .await
        .map_err(|_| ProviderError::Unavailable)??;
        let mut views = self.views.lock().map_err(|_| ProviderError::Unavailable)?;
        for view in projected {
            views.insert(view.inode, view.clone());
            result.push(view);
        }
        Ok(result)
    }
    async fn children(&self, parent: &View) -> Result<Vec<Node>, ProviderError> {
        let nodes = self.engine.children(&parent.scope, &parent.node.id).await?;
        match &self.writeback {
            Some(writer) => writer
                .overlay(&parent.scope, &parent.node.id, nodes)
                .map_err(|_| ProviderError::Unavailable),
            None => Ok(nodes),
        }
    }
    async fn node(&self, view: &View) -> Result<Node, ProviderError> {
        if let Some(writer) = &self.writeback
            && let Some(node) = writer
                .node(&view.scope, &view.node.id)
                .map_err(|_| ProviderError::Unavailable)?
        {
            return Ok(node);
        }
        if let Some(writer) = &self.writeback
            && writer
                .follows_remote(&view.scope, &view.node.id)
                .map_err(|_| ProviderError::Unavailable)?
        {
            let item = writer
                .remote_identity(&view.scope, &view.node.id)
                .map_err(|_| ProviderError::Unavailable)?
                .ok_or(ProviderError::Unavailable)?;
            let mut node = self.engine.node(&view.scope, &item).await?;
            node.id = view.node.id.clone();
            return Ok(node);
        }
        if view.inode == 1 || view.node.kind == NodeKind::File {
            return Ok(view.node.clone());
        }
        self.engine.node(&view.scope, &view.node.id).await
    }
    fn attr(&self, view: &View, node: &Node) -> FileAttr {
        let directory = node.kind == NodeKind::Folder;
        let time = UNIX_EPOCH + Duration::from_secs(node.modified_unix);
        FileAttr {
            ino: INodeNo(view.inode),
            size: node.size,
            blocks: node.size.div_ceil(512),
            atime: time,
            mtime: time,
            ctime: time,
            crtime: time,
            kind: if directory {
                FileType::Directory
            } else {
                FileType::RegularFile
            },
            perm: if self.writeback.is_some() {
                if directory { 0o700 } else { 0o600 }
            } else if directory {
                0o500
            } else {
                0o400
            },
            nlink: if directory {
                2
            } else if self
                .writeback
                .as_ref()
                .is_some_and(|w| w.is_unlinked(&view.scope, &view.node.id).unwrap_or(false))
            {
                0
            } else {
                1
            },
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }
    fn handle(&self) -> u64 {
        self.next_handle.fetch_add(1, Ordering::Relaxed)
    }
}
fn errno(error: &ProviderError) -> Errno {
    match error {
        ProviderError::NotFound => Errno::ENOENT,
        ProviderError::Permission | ProviderError::Authentication => Errno::EACCES,
        // These tokens denote mount/account shutdown, not an interrupted syscall.
        // EINTR makes libc/Rust readers retry forever against a cancelled mount.
        ProviderError::Cancelled => Errno::ENODEV,
        ProviderError::VersionChanged => Errno::ESTALE,
        ProviderError::Throttled(_) => Errno::EAGAIN,
        _ => Errno::EIO,
    }
}
impl Filesystem for CloudFs {
    fn init(&mut self, _req: &Request, config: &mut fuser::KernelConfig) -> std::io::Result<()> {
        if self.inner.writeback.is_some() {
            // Otherwise Linux strips O_TRUNC from OPEN and sends SETATTR only
            // afterward: opening would hydrate the old file before discarding it.
            return config
                .add_capabilities(fuser::InitFlags::FUSE_ATOMIC_O_TRUNC)
                .map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::Unsupported,
                        "kernel lacks atomic FUSE open/truncate support",
                    )
                });
        }
        config
            .add_capabilities(fuser::InitFlags::FUSE_DIRECT_IO_ALLOW_MMAP)
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "kernel lacks FUSE direct-I/O mmap support",
                )
            })?;
        Ok(())
    }

    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let Ok(permit) = self.inner.pending.clone().try_acquire_owned() else {
            reply.error(Errno::EAGAIN);
            return;
        };
        let inner = self.inner.clone();
        let name = name.to_os_string();
        self.inner.runtime.spawn(async move {
            let _permit = permit;
            let result = async {
                let parent = inner.view(parent.0)?;
                let nodes = inner.children(&parent).await?;
                let node = nodes
                    .into_iter()
                    .find(|n| OsStr::new(&n.name) == name)
                    .ok_or(ProviderError::NotFound)?;
                let view = inner.insert(&parent, node).await?;
                let node = inner.node(&view).await?;
                Ok::<_, ProviderError>((view, node))
            }
            .await;
            match result {
                Ok((view, node)) => reply.entry(&TTL, &inner.attr(&view, &node), Generation(0)),
                Err(e) => reply.error(errno(&e)),
            }
        });
    }
    fn getattr(&self, _req: &Request, inode: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        let Ok(permit) = self.inner.pending.clone().try_acquire_owned() else {
            reply.error(Errno::EAGAIN);
            return;
        };
        let inner = self.inner.clone();
        self.inner.runtime.spawn(async move {
            let _permit = permit;
            let result = async {
                let view = inner.view(inode.0)?;
                let node = inner.node(&view).await?;
                Ok::<_, ProviderError>((view, node))
            }
            .await;
            match result {
                Ok((view, node)) => reply.attr(&TTL, &inner.attr(&view, &node)),
                Err(e) => reply.error(errno(&e)),
            }
        });
    }
    fn create(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        mode: u32,
        _umask: u32,
        flags: i32,
        reply: fuser::ReplyCreate,
    ) {
        let Some(writer) = self.inner.writeback.clone() else {
            reply.error(Errno::EROFS);
            return;
        };
        if mode & libc::S_IFMT != libc::S_IFREG {
            reply.error(Errno::EOPNOTSUPP);
            return;
        }
        let Some(name) = name.to_str().map(str::to_owned) else {
            reply.error(Errno::EINVAL);
            return;
        };
        let Ok(permit) = self.inner.writes.clone().try_acquire_owned() else {
            reply.error(Errno::EAGAIN);
            return;
        };
        let Ok(admission) = self.inner.edits.admit() else {
            reply.error(Errno::ENODEV);
            return;
        };
        let inner = self.inner.clone();
        self.inner.runtime.spawn(async move {
            let _permit = permit;
            let _admission = admission;
            let result = async {
                let parent = inner.view(parent.0).map_err(|e| errno(&e))?;
                let nodes = inner.children(&parent).await.map_err(|e| errno(&e))?;
                if nodes
                    .iter()
                    .any(|n| n.name.to_lowercase() == name.to_lowercase())
                {
                    return Err(Errno::EEXIST);
                }
                let node = Node {
                    id: String::new(),
                    parent_id: Some(parent.node.id.clone()),
                    name,
                    kind: NodeKind::File,
                    size: 0,
                    modified_unix: 0,
                    etag: None,
                    content_version: None,
                    target: None,
                };
                let record = writer
                    .create(parent.scope.clone(), node)
                    .await
                    .map_err(|e| if e == Errno::ESTALE { Errno::EEXIST } else { e })?;
                let lease = writer
                    .lease(&parent.scope, &record.node.id, &inner.cancel)
                    .await?;
                let view = inner
                    .insert(&parent, record.node.clone())
                    .await
                    .map_err(|e| errno(&e))?;
                let attr = inner.attr(&view, &record.node);
                let handle = inner.handle();
                writer.publish_open(
                    &inner,
                    handle,
                    OpenFile {
                        view,
                        node: record.node,
                        flags,
                        _lease: Some(lease),
                        remote_reads: tokio_util::task::TaskTracker::new(),
                    },
                )?;
                Ok::<_, Errno>((attr, handle))
            }
            .await;
            match result {
                Ok((attr, handle)) => reply.created(
                    &TTL,
                    &attr,
                    Generation(0),
                    FileHandle(handle),
                    FopenFlags::FOPEN_DIRECT_IO,
                ),
                Err(e) => reply.error(e),
            }
        });
    }
    fn rename(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        newparent: INodeNo,
        newname: &OsStr,
        flags: RenameFlags,
        reply: ReplyEmpty,
    ) {
        let Some(writer) = self.inner.writeback.clone() else {
            reply.error(Errno::EROFS);
            return;
        };
        if !(flags & !RenameFlags::RENAME_NOREPLACE).is_empty() {
            reply.error(Errno::EOPNOTSUPP);
            return;
        }
        let (Some(name), Some(newname)) = (name.to_str(), newname.to_str()) else {
            reply.error(Errno::EINVAL);
            return;
        };
        let (name, newname) = (name.to_owned(), newname.to_owned());
        let Ok(permit) = self.inner.writes.clone().try_acquire_owned() else {
            reply.error(Errno::EAGAIN);
            return;
        };
        let Ok(admission) = self.inner.edits.admit() else {
            reply.error(Errno::ENODEV);
            return;
        };
        let inner = self.inner.clone();
        self.inner.runtime.spawn(async move {
            let _permit = permit;
            let _admission = admission;
            let result = async {
                let parent = inner.view(parent.0).map_err(|e| errno(&e))?;
                let destination = inner.view(newparent.0).map_err(|e| errno(&e))?;
                if parent.scope != destination.scope || parent.alias != destination.alias {
                    return Err(Errno::EXDEV);
                }
                if parent.node.kind != NodeKind::Folder || destination.node.kind != NodeKind::Folder
                {
                    return Err(Errno::ENOTDIR);
                }
                let nodes = inner.children(&parent).await.map_err(|e| errno(&e))?;
                let source = nodes
                    .iter()
                    .find(|n| n.name == name)
                    .cloned()
                    .ok_or(Errno::ENOENT)?;
                if source.kind != NodeKind::File || source.target.is_some() {
                    return Err(Errno::EOPNOTSUPP);
                }
                let _lease = writer
                    .lease(&parent.scope, &source.id, &inner.cancel)
                    .await?;
                if parent.node.id == destination.node.id && name == newname {
                    return if flags.contains(RenameFlags::RENAME_NOREPLACE) {
                        Err(Errno::EEXIST)
                    } else {
                        Ok(())
                    };
                }
                let occupants = if parent.inode == destination.inode {
                    nodes
                } else {
                    inner.children(&destination).await.map_err(|e| errno(&e))?
                };
                if let Some(victim) = occupants
                    .iter()
                    .find(|n| n.id != source.id && n.name.to_lowercase() == newname.to_lowercase())
                {
                    if flags.contains(RenameFlags::RENAME_NOREPLACE) || victim.name != newname {
                        return Err(Errno::EEXIST);
                    }
                    if victim.kind == NodeKind::Folder {
                        return Err(Errno::EISDIR);
                    }
                    if victim.kind != NodeKind::File || victim.target.is_some() {
                        return Err(Errno::EOPNOTSUPP);
                    }
                    let moved = writer
                        .replace(&inner, parent.scope.clone(), source, victim.clone())
                        .await?;
                    inner
                        .insert(&destination, moved)
                        .await
                        .map_err(|e| errno(&e))?;
                    return Ok(());
                }
                let moved = writer
                    .relocate(
                        parent.scope.clone(),
                        source,
                        parent.node.id.clone(),
                        name,
                        destination.node.id.clone(),
                        newname,
                    )
                    .await?;
                inner
                    .insert(&destination, moved)
                    .await
                    .map_err(|e| errno(&e))?;
                inner.engine.changed.notify_waiters();
                Ok::<_, Errno>(())
            }
            .await;
            match result {
                Ok(()) => reply.ok(),
                Err(error) => reply.error(error),
            }
        });
    }
    fn unlink(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let Some(writer) = self.inner.writeback.clone() else {
            reply.error(Errno::EROFS);
            return;
        };
        let Some(name) = name.to_str().map(str::to_owned) else {
            reply.error(Errno::EINVAL);
            return;
        };
        let Ok(permit) = self.inner.writes.clone().try_acquire_owned() else {
            reply.error(Errno::EAGAIN);
            return;
        };
        let Ok(admission) = self.inner.edits.admit() else {
            reply.error(Errno::ENODEV);
            return;
        };
        let inner = self.inner.clone();
        self.inner.runtime.spawn(async move {
            let _permit = permit;
            let _admission = admission;
            let result = async {
                let parent = inner.view(parent.0).map_err(|e| errno(&e))?;
                if parent.node.kind != NodeKind::Folder {
                    return Err(Errno::ENOTDIR);
                }
                let source = inner
                    .children(&parent)
                    .await
                    .map_err(|e| errno(&e))?
                    .into_iter()
                    .find(|n| n.name == name)
                    .ok_or(Errno::ENOENT)?;
                if source.kind != NodeKind::File {
                    return Err(Errno::EISDIR);
                }
                if source.target.is_some() {
                    return Err(Errno::EOPNOTSUPP);
                }
                let view = inner.insert(&parent, source).await.map_err(|e| errno(&e))?;
                writer.unlink(&inner, view).await
            }
            .await;
            match result {
                Ok(()) => reply.ok(),
                Err(e) => reply.error(e),
            }
        });
    }
    fn write(
        &self,
        _req: &Request,
        _ino: INodeNo,
        handle: FileHandle,
        offset: u64,
        data: &[u8],
        _write_flags: fuser::WriteFlags,
        _flags: OpenFlags,
        _owner: Option<LockOwner>,
        reply: fuser::ReplyWrite,
    ) {
        let Some(writer) = self.inner.writeback.clone() else {
            reply.error(Errno::EROFS);
            return;
        };
        if data.len() > 8 * 1024 * 1024 {
            reply.error(Errno::EINVAL);
            return;
        }
        let Ok(permit) = self.inner.writes.clone().try_acquire_owned() else {
            reply.error(Errno::EAGAIN);
            return;
        };
        let Ok(admission) = self.inner.edits.admit() else {
            reply.error(Errno::ENODEV);
            return;
        };
        let inner = self.inner.clone();
        let bytes = data.to_vec();
        self.inner.runtime.spawn(async move {
            let _permit = permit;
            let _admission = admission;
            let result = async {
                let file = inner
                    .files
                    .lock()
                    .map_err(|_| Errno::EIO)?
                    .get(&handle.0)
                    .cloned()
                    .ok_or(Errno::EBADF)?;
                if file.flags & libc::O_ACCMODE == libc::O_RDONLY {
                    return Err(Errno::EBADF);
                }
                let working = writer
                    .working(&file.view.scope, &file.view.node.id)?
                    .ok_or(Errno::EIO)?;
                let count = writer
                    .write(working.id, offset, bytes, file.flags & libc::O_APPEND != 0)
                    .await?;
                if file.flags & (libc::O_SYNC | libc::O_DSYNC) != 0 {
                    writer.seal(working.id).await?;
                }
                Ok::<_, Errno>(count)
            }
            .await;
            match result {
                Ok(count) => reply.written(count),
                Err(e) => reply.error(e),
            }
        });
    }
    fn setattr(
        &self,
        _req: &Request,
        ino: INodeNo,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<fuser::TimeOrNow>,
        mtime: Option<fuser::TimeOrNow>,
        ctime: Option<std::time::SystemTime>,
        fh: Option<FileHandle>,
        crtime: Option<std::time::SystemTime>,
        chgtime: Option<std::time::SystemTime>,
        bkuptime: Option<std::time::SystemTime>,
        flags: Option<fuser::BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        let Some(writer) = self.inner.writeback.clone() else {
            reply.error(Errno::EROFS);
            return;
        };
        if mode.is_some()
            || uid.is_some()
            || gid.is_some()
            || atime.is_some()
            || mtime.is_some()
            || ctime.is_some()
            || crtime.is_some()
            || chgtime.is_some()
            || bkuptime.is_some()
            || flags.is_some()
        {
            reply.error(Errno::EOPNOTSUPP);
            return;
        }
        let Some(size) = size else {
            reply.error(Errno::EINVAL);
            return;
        };
        let Ok(permit) = self.inner.writes.clone().try_acquire_owned() else {
            reply.error(Errno::EAGAIN);
            return;
        };
        let Ok(admission) = self.inner.edits.admit() else {
            reply.error(Errno::ENODEV);
            return;
        };
        let inner = self.inner.clone();
        self.inner.runtime.spawn(async move {
            let _permit = permit;
            let _admission = admission;
            let result = async {
                if let Some(fh) = fh {
                    let file = inner
                        .files
                        .lock()
                        .map_err(|_| Errno::EIO)?
                        .get(&fh.0)
                        .cloned()
                        .ok_or(Errno::EBADF)?;
                    if file.view.inode != ino.0 || file.flags & libc::O_ACCMODE == libc::O_RDONLY {
                        return Err(Errno::EBADF);
                    }
                    let working = writer
                        .working(&file.view.scope, &file.view.node.id)?
                        .ok_or(Errno::EIO)?;
                    let record = writer.truncate(working.id, size).await?;
                    return Ok(inner.attr(&file.view, &record.node));
                }
                let view = inner.view(ino.0).map_err(|e| errno(&e))?;
                if writer.is_unlinked(&view.scope, &view.node.id)? {
                    return Err(Errno::ENOENT);
                }
                let _lease = writer
                    .lease(&view.scope, &view.node.id, &inner.cancel)
                    .await?;
                let mut view = view;
                view.node = inner.node(&view).await.map_err(|e| errno(&e))?;
                let working = writer
                    .prepare(&inner.engine, &view, size == 0, &inner.cancel)
                    .await?;
                let record = writer.truncate(working.id, size).await?;
                Ok::<_, Errno>(inner.attr(&view, &record.node))
            }
            .await;
            match result {
                Ok(attr) => reply.attr(&TTL, &attr),
                Err(e) => reply.error(e),
            }
        });
    }
    fn getxattr(
        &self,
        _req: &Request,
        inode: INodeNo,
        _name: &OsStr,
        _size: u32,
        reply: ReplyXattr,
    ) {
        // Ordinary desktop probes are not filesystem failures. Per-file Cirrove
        // status attributes will be provided by the later desktop integration.
        match self.inner.view(inode.0) {
            Ok(_) => reply.error(Errno::ENODATA),
            Err(error) => reply.error(errno(&error)),
        }
    }
    fn listxattr(&self, _req: &Request, inode: INodeNo, size: u32, reply: ReplyXattr) {
        match self.inner.view(inode.0) {
            Ok(_) if size == 0 => reply.size(0),
            Ok(_) => reply.data(&[]),
            Err(error) => reply.error(errno(&error)),
        }
    }
    fn flush(
        &self,
        _req: &Request,
        _inode: INodeNo,
        handle: FileHandle,
        _owner: LockOwner,
        reply: ReplyEmpty,
    ) {
        self.finish_handle(handle, reply);
    }
    fn fsync(
        &self,
        _req: &Request,
        _inode: INodeNo,
        handle: FileHandle,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        self.finish_handle(handle, reply);
    }
    fn open(&self, _req: &Request, inode: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        if self.inner.writeback.is_some()
            && flags.0 & libc::O_TRUNC != 0
            && flags.0 & libc::O_ACCMODE == libc::O_RDONLY
        {
            // POSIX leaves this flag combination undefined; do not accept a
            // destructive open that cannot participate in writable-handle sealing.
            reply.error(Errno::EINVAL);
            return;
        }
        if self.inner.writeback.is_none()
            && (flags.0 & libc::O_ACCMODE != libc::O_RDONLY || flags.0 & libc::O_TRUNC != 0)
        {
            reply.error(Errno::EROFS);
            return;
        }
        let Ok(permit) = self.inner.pending.clone().try_acquire_owned() else {
            reply.error(Errno::EAGAIN);
            return;
        };
        let admission = if flags.0 & libc::O_ACCMODE != libc::O_RDONLY {
            match self.inner.edits.admit() {
                Ok(token) => Some(token),
                Err(e) => {
                    reply.error(e);
                    return;
                }
            }
        } else {
            None
        };
        let inner = self.inner.clone();
        self.inner.runtime.spawn(async move {
            let _permit = permit;
            let _admission = admission;
            let result = async {
                let mut view = inner.view(inode.0).map_err(|e| errno(&e))?;
                let lease = match &inner.writeback {
                    Some(writer) => Some(
                        writer
                            .lease(&view.scope, &view.node.id, &inner.cancel)
                            .await?,
                    ),
                    None => None,
                };
                let node = inner.node(&view).await.map_err(|e| errno(&e))?;
                if node.kind != NodeKind::File {
                    return Err(Errno::EISDIR);
                }
                let file = OpenFile {
                    view: view.clone(),
                    node: node.clone(),
                    flags: flags.0,
                    _lease: lease,
                    remote_reads: tokio_util::task::TaskTracker::new(),
                };
                let file = if let Some(writer) = &inner.writeback {
                    writer.register_open(file)?
                } else {
                    Arc::new(file)
                };
                if flags.0 & libc::O_ACCMODE != libc::O_RDONLY {
                    let writer = inner.writeback.as_ref().ok_or(Errno::EROFS)?;
                    view.node = node.clone();
                    writer
                        .prepare(
                            &inner.engine,
                            &view,
                            flags.0 & libc::O_TRUNC != 0,
                            &inner.cancel,
                        )
                        .await?;
                }
                let handle = inner.handle();
                inner
                    .files
                    .lock()
                    .map_err(|_| Errno::EIO)?
                    .insert(handle, file);
                Ok::<_, Errno>(handle)
            }
            .await;
            match result {
                Ok(handle) => reply.opened(FileHandle(handle), FopenFlags::FOPEN_DIRECT_IO),
                Err(e) => reply.error(e),
            }
        });
    }
    fn read(
        &self,
        _req: &Request,
        _inode: INodeNo,
        handle: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        // Bound task admission separately from active read buffers. A routine
        // thumbnail burst waits asynchronously instead of failing at 32 readers.
        let Ok(admission) = self.inner.admitted_reads.clone().try_acquire_owned() else {
            reply.error(Errno::EAGAIN);
            return;
        };
        let inner = self.inner.clone();
        self.inner.runtime.spawn(async move {
            let _admission = admission;
            let _permit = tokio::select! {
                biased;
                _ = inner.cancel.cancelled() => {
                    reply.error(Errno::ENODEV);
                    return;
                },
                permit = tokio::time::timeout(READ_QUEUE_TIMEOUT, inner.reads.acquire()) => {
                    match permit {
                        Ok(Ok(permit)) => permit,
                        Ok(Err(_)) => { reply.error(Errno::EIO); return; },
                        Err(_) => { reply.error(Errno::ETIMEDOUT); return; },
                    }
                }
            };
            let result = async {
                let file = inner
                    .files
                    .lock()
                    .map_err(|_| ProviderError::Unavailable)?
                    .get(&handle.0)
                    .cloned()
                    .ok_or(ProviderError::NotFound)?;
                if file.flags & libc::O_ACCMODE == libc::O_WRONLY {
                    return Err(ProviderError::Permission);
                }
                let (source, _flight) = if let Some(writer) = &inner.writeback {
                    match writer
                        .read_source(&file)
                        .map_err(|_| ProviderError::Unavailable)?
                    {
                        writeback::ReadSource::Working(id) => {
                            return writer
                                .read(id, offset, size)
                                .await
                                .map_err(|_| ProviderError::Unavailable);
                        }
                        writeback::ReadSource::Remote(node, token) => (node, Some(token)),
                    }
                } else {
                    (file.node.clone(), None)
                };
                inner
                    .engine
                    .cache
                    .read(
                        inner.engine.provider.as_ref(),
                        &file.view.scope,
                        &source,
                        offset,
                        size,
                        &inner.cancel,
                    )
                    .await
            }
            .await;
            match result {
                Ok(data) => reply.data(&data),
                Err(e) => reply.error(errno(&e)),
            }
        });
    }
    fn release(
        &self,
        _req: &Request,
        _ino: INodeNo,
        handle: FileHandle,
        _flags: OpenFlags,
        _owner: Option<LockOwner>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        if let Ok(mut files) = self.inner.files.lock() {
            files.remove(&handle.0);
        }
        reply.ok();
    }
    fn opendir(&self, _req: &Request, inode: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        let Ok(permit) = self.inner.pending.clone().try_acquire_owned() else {
            reply.error(Errno::EAGAIN);
            return;
        };
        let inner = self.inner.clone();
        self.inner.runtime.spawn(async move {
            let _permit = permit;
            match inner.listing(inode.0).await {
                Ok(entries) => {
                    let handle = inner.handle();
                    match inner.directories.lock() {
                        Ok(mut dirs) => {
                            dirs.insert(handle, Arc::new(entries));
                            reply.opened(FileHandle(handle), FopenFlags::empty());
                        }
                        Err(_) => reply.error(Errno::EIO),
                    }
                }
                Err(e) => reply.error(errno(&e)),
            }
        });
    }
    fn readdir(
        &self,
        _req: &Request,
        _inode: INodeNo,
        handle: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let entries = self
            .inner
            .directories
            .lock()
            .ok()
            .and_then(|d| d.get(&handle.0).cloned());
        let Some(entries) = entries else {
            reply.error(Errno::EBADF);
            return;
        };
        for (index, entry) in entries.iter().enumerate().skip(offset as usize) {
            if reply.add(
                INodeNo(entry.inode),
                (index + 1) as u64,
                if entry.node.kind == NodeKind::Folder {
                    FileType::Directory
                } else {
                    FileType::RegularFile
                },
                OsString::from(&entry.name),
            ) {
                break;
            }
        }
        reply.ok();
    }
    fn releasedir(
        &self,
        _req: &Request,
        _ino: INodeNo,
        handle: FileHandle,
        _flags: OpenFlags,
        reply: ReplyEmpty,
    ) {
        if let Ok(mut dirs) = self.inner.directories.lock() {
            dirs.remove(&handle.0);
        }
        reply.ok();
    }
    fn destroy(&mut self) {
        self.inner.cancel.cancel();
    }
}
