//! FUSE projection: ordinary mounts are read-only; isolated test mounts allow edits.
//! Callbacks dispatch asynchronous work; no callback
//! holds the namespace map while awaiting a provider or a database operation.
#[cfg(test)]
mod capacity;
mod directories;
mod lifecycle;
mod residency;
use residency::{LookupRefs, NamespaceViews};
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
    residency: Arc<LookupRefs>,
    // A view (including a directory snapshot or old open file) protects its
    // parent entry. The retained parent view protects the rest of the chain.
    _parent_residency: Option<Arc<LookupRefs>>,
    inode: u64,
    parent: u64,
    scope: Scope,
    node: Node,
    name: String,
    alias: Vec<(String, String)>,
    reference: bool,
    entry: Option<Node>,
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
struct OpenDirectory {
    snapshot: directories::Snapshot,
    _route: [View; 2],
}
struct Inner {
    engine: Arc<Engine>,
    writeback: Option<Arc<writeback::Writeback>>,
    edits: lifecycle::EditAdmission,
    runtime: Handle,
    views: Mutex<NamespaceViews>,
    files: Mutex<HashMap<u64, Arc<OpenFile>>>,
    directories: Mutex<HashMap<u64, Arc<OpenDirectory>>>,
    directory_budget: directories::Budget,
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
            residency: Arc::default(),
            _parent_residency: None,
            inode: 1,
            parent: 1,
            ancestry: vec![(scope.collection.clone(), root.id.clone())],
            scope,
            node: root,
            name: engine.account.label.clone(),
            alias: vec![],
            reference: false,
            entry: None,
        };
        Ok(Self {
            inner: Arc::new(Inner {
                cancel: engine.cancel.child_token(),
                engine,
                writeback: None,
                edits: lifecycle::EditAdmission::new(),
                runtime: Handle::current(),
                views: Mutex::new(NamespaceViews::new(root)),
                files: Mutex::new(HashMap::new()),
                directories: Mutex::new(HashMap::new()),
                directory_budget: directories::Budget::default(),
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
    fn start_view_reclamation(&self) {
        let inner = self.inner.clone();
        self.inner.runtime.spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            loop {
                tokio::select! { biased;
                    _ = inner.cancel.cancelled() => break,
                    _ = tick.tick() => {
                        if let Ok(mut views) = inner.views.lock() { views.collect(4096); }
                    }
                }
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
        let fs = CloudFs { inner };
        fs.start_invalidations(notifier);
        fs.start_view_reclamation();
        Ok(session)
    }
}
impl Inner {
    fn acquire_lookup(&self, inode: u64) -> Result<(), ProviderError> {
        self.views
            .lock()
            .map_err(|_| ProviderError::Unavailable)?
            .acquire_lookup(inode)
    }
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
            residency: Arc::default(),
            _parent_residency: Some(parent.residency.clone()),
            inode: 0,
            parent: parent.inode,
            scope,
            node,
            name,
            alias,
            reference: child.target.is_some(),
            entry: child.target.as_ref().map(|_| child.clone()),
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
            let (local, retained) = match &self.writeback {
                Some(writer) => writer
                    .reference_view(&view.scope, &view.node.id)
                    .map_err(|e| {
                        if e == Errno::ENOENT {
                            ProviderError::NotFound
                        } else {
                            ProviderError::Unavailable
                        }
                    })?,
                None => (None, false),
            };
            if let Some(local) = local {
                view.node.id = local;
                view.node = self.node(&view).await?;
            } else if view.node.kind == NodeKind::Folder && retained {
                // A retained shortcut is the local route to an absent target
                // root. Its directory view remains traversable without metadata.
            } else {
                view.node = self.engine.node(&view.scope, &view.node.id).await?;
            }
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
            .insert(view)
    }
    async fn listing(&self, parent: &View) -> Result<OpenDirectory, Errno> {
        let route = [
            parent.clone(),
            self.view(parent.parent).map_err(|e| errno(&e))?,
        ];
        let db = self.engine.db.clone();
        let budget = self.directory_budget.clone();
        let writable = self.writeback.is_some();
        let cancel = self.cancel.clone();
        let build = move |nodes: &mut dyn Iterator<Item = cirrove_store::Result<Node>>| {
            Self::build_listing(nodes, &route, &db, &budget, writable, &cancel)
        };
        if self.writeback.is_none() {
            // The metadata reader and inode writer are independent WAL
            // connections. No read-to-write upgrade or full remote Vec is needed.
            self.engine
                .with_children(&parent.scope, &parent.node.id, build)
                .await
                .map_err(|e| errno(&e))?
        } else {
            // Preserve the existing atomic local overlay until that projection
            // also has a streaming contract; never omit pending local entries.
            let nodes = self.children(parent).await.map_err(|e| errno(&e))?;
            tokio::task::spawn_blocking(move || build(&mut nodes.into_iter().map(Ok)))
                .await
                .map_err(|_| Errno::EIO)?
        }
    }
    fn build_listing(
        nodes: &mut dyn Iterator<Item = cirrove_store::Result<Node>>,
        route: &[View; 2],
        db: &std::path::Path,
        budget: &directories::Budget,
        writable: bool,
        cancel: &CancellationToken,
    ) -> Result<OpenDirectory, Errno> {
        let mut store = Store::open(db).map_err(|_| Errno::EIO)?;
        let mut snapshot = budget.start(db.parent().ok_or(Errno::EIO)?)?;
        snapshot.push(route[0].inode, true, ".")?;
        snapshot.push(route[1].inode, true, "..")?;
        let mut projected = Vec::with_capacity(128);
        for node in nodes {
            if cancel.is_cancelled() {
                return Err(Errno::ENODEV);
            }
            match Self::project(&route[0], node.map_err(|_| Errno::EIO)?) {
                Ok(view) => projected.push(view),
                Err(ProviderError::Protocol(_)) => {
                    tracing::warn!("cloud entry could not be projected (cycle or invalid name)");
                }
                Err(error) => return Err(errno(&error)),
            }
            if projected.len() == 128 {
                Self::snapshot_batch(&mut store, &mut projected, writable, &mut snapshot)?;
            }
        }
        Self::snapshot_batch(&mut store, &mut projected, writable, &mut snapshot)?;
        if cancel.is_cancelled() {
            return Err(Errno::ENODEV);
        }
        Ok(OpenDirectory {
            snapshot: snapshot.finish()?,
            _route: route.clone(),
        })
    }
    fn snapshot_batch(
        store: &mut Store,
        projected: &mut Vec<View>,
        writable: bool,
        snapshot: &mut directories::Builder,
    ) -> Result<(), Errno> {
        for view in projected.iter_mut() {
            // A cold link stays provisional until LOOKUP resolves its target;
            // listing only uses cached target metadata, never provider I/O.
            if view.reference
                && let Some(node) = store
                    .node(&view.scope, &view.node.id)
                    .map_err(|_| Errno::EIO)?
            {
                view.node = node;
            }
        }
        let keys = projected
            .iter()
            .map(|view| Self::inode_key(view, writable).map_err(|e| errno(&e)))
            .collect::<Result<Vec<_>, _>>()?;
        let inodes = store.inodes(&keys).map_err(|_| Errno::EIO)?;
        for (view, inode) in projected.drain(..).zip(inodes) {
            // READDIR does not create kernel lookup references. Only the parent
            // route is retained by the snapshot, not every projected child.
            snapshot.push(inode, view.node.kind == NodeKind::Folder, &view.name)?;
        }
        Ok(())
    }
    async fn children(&self, parent: &View) -> Result<Vec<Node>, ProviderError> {
        let identity = match &self.writeback {
            Some(writer) => writer
                .directory_identity(&parent.scope, &parent.node.id)
                .map_err(|_| ProviderError::Unavailable)?,
            None => Some(parent.node.id.clone()),
        };
        let nodes = match identity {
            Some(item) => match self.engine.children(&parent.scope, &item).await {
                Ok(nodes) => nodes,
                Err(ProviderError::NotFound) => {
                    let retained = match &self.writeback {
                        Some(w) => w
                            .retains_directory(&parent.scope, &parent.node.id)
                            .map_err(|_| ProviderError::Unavailable)?,
                        None => false,
                    };
                    if !retained {
                        return Err(ProviderError::NotFound);
                    }
                    Vec::new()
                }
                Err(error) => return Err(error),
            },
            None => Vec::new(),
        };
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
            && view.node.kind == NodeKind::Folder
            && writer
                .retains_directory(&view.scope, &view.node.id)
                .map_err(|_| ProviderError::Unavailable)?
        {
            return Ok(view.node.clone());
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
            writer
                .localize_parent(&view.scope, &mut node)
                .map_err(|_| ProviderError::Unavailable)?;
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
    fn forget(&self, _req: &Request, inode: INodeNo, nlookup: u64) {
        if let Ok(mut views) = self.inner.views.lock()
            && !views.forget(inode.0, nlookup)
        {
            tracing::warn!(
                "inconsistent FUSE lookup references; namespace reclamation is conservative"
            );
        }
    }
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
        // Capture residency before dispatch, including its ancestor leases.
        let parent = match inner.view(parent.0) {
            Ok(view) => view,
            Err(error) => {
                reply.error(errno(&error));
                return;
            }
        };
        let name = name.to_os_string();
        self.inner.runtime.spawn(async move {
            let _permit = permit;
            let result = async {
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
                Ok((view, node)) => match inner.acquire_lookup(view.inode) {
                    Ok(()) => reply.entry(&TTL, &inner.attr(&view, &node), Generation(0)),
                    Err(error) => reply.error(errno(&error)),
                },
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
        let view = match inner.view(inode.0) {
            Ok(view) => view,
            Err(error) => {
                reply.error(errno(&error));
                return;
            }
        };
        self.inner.runtime.spawn(async move {
            let _permit = permit;
            let result = async {
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
    fn mkdir(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
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
        // Capture residency before dispatch, including its ancestor leases.
        let parent = match inner.view(parent.0) {
            Ok(view) => view,
            Err(error) => {
                reply.error(errno(&error));
                return;
            }
        };
        self.inner.runtime.spawn(async move {
            let _permit = permit;
            let _admission = admission;
            let result = async {
                if parent.node.kind != NodeKind::Folder {
                    return Err(Errno::ENOTDIR);
                }
                if inner
                    .children(&parent)
                    .await
                    .map_err(|e| errno(&e))?
                    .iter()
                    .any(|node| node.name.to_lowercase() == name.to_lowercase())
                {
                    return Err(Errno::EEXIST);
                }
                inner.capture_ancestors(&parent).await?;
                let node = writer
                    .create_directory(parent.scope.clone(), parent.node.id.clone(), name)
                    .await
                    .map_err(|e| if e == Errno::ESTALE { Errno::EEXIST } else { e })?;
                let view = inner
                    .insert(&parent, node.clone())
                    .await
                    .map_err(|e| errno(&e))?;
                inner.engine.changed.notify_waiters();
                let attr = inner.attr(&view, &node);
                Ok::<_, Errno>((view, attr))
            }
            .await;
            match result {
                Ok((view, attr)) => match inner.acquire_lookup(view.inode) {
                    Ok(()) => reply.entry(&TTL, &attr, Generation(0)),
                    Err(error) => reply.error(errno(&error)),
                },
                Err(error) => reply.error(error),
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
        // Capture residency before dispatch, including its ancestor leases.
        let parent = match inner.view(parent.0) {
            Ok(view) => view,
            Err(error) => {
                reply.error(errno(&error));
                return;
            }
        };
        self.inner.runtime.spawn(async move {
            let _permit = permit;
            let _admission = admission;
            let result = async {
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
                inner.capture_ancestors(&parent).await?;
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
                Ok((attr, handle)) => match inner.acquire_lookup(attr.ino.0) {
                    Ok(()) => reply.created(
                        &TTL,
                        &attr,
                        Generation(0),
                        FileHandle(handle),
                        FopenFlags::FOPEN_DIRECT_IO,
                    ),
                    Err(error) => {
                        if let Ok(mut files) = inner.files.lock() {
                            files.remove(&handle);
                        }
                        reply.error(errno(&error));
                    }
                },
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
        // Capture residency before dispatch, including its ancestor leases.
        let parent = match inner.view(parent.0) {
            Ok(view) => view,
            Err(error) => {
                reply.error(errno(&error));
                return;
            }
        };
        let destination = match inner.view(newparent.0) {
            Ok(view) => view,
            Err(error) => {
                reply.error(errno(&error));
                return;
            }
        };
        self.inner.runtime.spawn(async move {
            let _permit = permit;
            let _admission = admission;
            let result = async {
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
                inner.capture_ancestors(&parent).await?;
                inner.capture_ancestors(&destination).await?;
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
        // Capture residency before dispatch, including its ancestor leases.
        let parent = match inner.view(parent.0) {
            Ok(view) => view,
            Err(error) => {
                reply.error(errno(&error));
                return;
            }
        };
        self.inner.runtime.spawn(async move {
            let _permit = permit;
            let _admission = admission;
            let result = async {
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
        let (opened, path_view) = if let Some(fh) = fh {
            let file = inner
                .files
                .lock()
                .ok()
                .and_then(|files| files.get(&fh.0).cloned());
            let Some(file) = file else {
                reply.error(Errno::EBADF);
                return;
            };
            (Some(file), None)
        } else {
            let view = match inner.view(ino.0) {
                Ok(view) => view,
                Err(error) => {
                    reply.error(errno(&error));
                    return;
                }
            };
            (None, Some(view))
        };
        self.inner.runtime.spawn(async move {
            let _permit = permit;
            let _admission = admission;
            let result = async {
                if let Some(file) = opened {
                    if file.view.inode != ino.0 || file.flags & libc::O_ACCMODE == libc::O_RDONLY {
                        return Err(Errno::EBADF);
                    }
                    let working = writer
                        .working(&file.view.scope, &file.view.node.id)?
                        .ok_or(Errno::EIO)?;
                    let record = writer.truncate(working.id, size).await?;
                    return Ok(inner.attr(&file.view, &record.node));
                }
                let view = path_view.ok_or(Errno::EIO)?;
                if writer.is_unlinked(&view.scope, &view.node.id)? {
                    return Err(Errno::ENOENT);
                }
                let _lease = writer
                    .lease(&view.scope, &view.node.id, &inner.cancel)
                    .await?;
                let mut view = view;
                view.node = inner.node(&view).await.map_err(|e| errno(&e))?;
                inner.capture_ancestors(&view).await?;
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
        let mut view = match inner.view(inode.0) {
            Ok(view) => view,
            Err(error) => {
                reply.error(errno(&error));
                return;
            }
        };
        self.inner.runtime.spawn(async move {
            let _permit = permit;
            let _admission = admission;
            let result = async {
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
                if let Some(writer) = &inner.writeback
                    && writer.is_unlinked(&view.scope, &view.node.id)?
                {
                    return Err(Errno::ENOENT);
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
                    inner.capture_ancestors(&view).await?;
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
        let parent = match inner.view(inode.0) {
            Ok(view) => view,
            Err(error) => {
                reply.error(errno(&error));
                return;
            }
        };
        self.inner.runtime.spawn(async move {
            let _permit = permit;
            match inner.listing(&parent).await {
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
                Err(e) => reply.error(e),
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
        let Ok(permit) = self.inner.pending.clone().try_acquire_owned() else {
            reply.error(Errno::EAGAIN);
            return;
        };
        let cancel = self.inner.cancel.clone();
        self.inner.runtime.spawn_blocking(move || {
            let _permit = permit;
            if cancel.is_cancelled() {
                reply.error(Errno::ENODEV);
                return;
            }
            let page = match entries.snapshot.page(offset) {
                Ok(page) => page,
                Err(error) => {
                    reply.error(error.into());
                    return;
                }
            };
            if cancel.is_cancelled() {
                reply.error(Errno::ENODEV);
                return;
            }
            for (index, entry) in page.into_iter().enumerate() {
                if reply.add(
                    INodeNo(entry.inode),
                    offset + index as u64 + 1,
                    if entry.directory {
                        FileType::Directory
                    } else {
                        FileType::RegularFile
                    },
                    OsString::from(entry.name),
                ) {
                    break;
                }
            }
            reply.ok();
        });
    }
    fn releasedir(
        &self,
        _req: &Request,
        _ino: INodeNo,
        handle: FileHandle,
        _flags: OpenFlags,
        reply: ReplyEmpty,
    ) {
        let directory = self
            .inner
            .directories
            .lock()
            .ok()
            .and_then(|mut dirs| dirs.remove(&handle.0));
        // Closing anonymous snapshot files may reclaim disk blocks. Do it away
        // from the map lock and FUSE callback; outstanding reads keep their Arc.
        self.inner.runtime.spawn_blocking(move || drop(directory));
        reply.ok();
    }
    fn destroy(&mut self) {
        self.inner.cancel.cancel();
    }
}
