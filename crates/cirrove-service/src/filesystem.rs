//! Read-only FUSE projection. Callbacks dispatch asynchronous work; no callback
//! holds the namespace map while awaiting a provider or a database operation.
use crate::engine::Engine;
use cirrove_core::{CancellationToken, Node, NodeKind, ProviderError, Scope};
use cirrove_store::Store;
use fuser::{
    Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo, LockOwner,
    OpenFlags, ReplyAttr, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen, Request,
};
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
#[derive(Clone)]
struct View {
    inode: u64,
    parent: u64,
    scope: Scope,
    node: Node,
    name: String,
    alias: Vec<(String, String)>,
    ancestry: Vec<(String, String)>,
}
#[derive(Clone)]
struct OpenFile {
    view: View,
    node: Node,
}
pub struct CloudFs {
    inner: Arc<Inner>,
}
struct Inner {
    engine: Arc<Engine>,
    runtime: Handle,
    views: Mutex<HashMap<u64, View>>,
    files: Mutex<HashMap<u64, OpenFile>>,
    directories: Mutex<HashMap<u64, Arc<Vec<View>>>>,
    next_handle: AtomicU64,
    pending: Arc<Semaphore>,
    reads: Arc<Semaphore>,
    uid: u32,
    gid: u32,
    cancel: CancellationToken,
}
impl CloudFs {
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
        };
        Ok(Self {
            inner: Arc::new(Inner {
                cancel: engine.cancel.child_token(),
                engine,
                runtime: Handle::current(),
                views: Mutex::new(HashMap::from([(1, root)])),
                files: Mutex::new(HashMap::new()),
                directories: Mutex::new(HashMap::new()),
                next_handle: AtomicU64::new(1),
                pending: Arc::new(Semaphore::new(128)),
                reads: Arc::new(Semaphore::new(32)),
                uid: owner.uid(),
                gid: owner.gid(),
            }),
        })
    }
    pub fn start_invalidations(&self, notifier: fuser::Notifier) {
        let inner = self.inner.clone();
        self.inner.runtime.spawn(async move {
            loop {tokio::select!{biased;_=inner.cancel.cancelled()=>break,_=inner.engine.changed.notified()=>{}}
                let entries=inner.views.lock().map(|v|v.values().map(|n|(n.inode,n.parent,n.name.clone())).collect::<Vec<_>>()).unwrap_or_default();
                let notifier=notifier.clone();let _=tokio::task::spawn_blocking(move||{for (inode,parent,name) in entries {let _=notifier.inval_inode(INodeNo(inode),0,0);if inode!=1{let _=notifier.inval_entry(INodeNo(parent),OsStr::new(&name));}}}).await;
            }
        });
    }
    pub fn mount(self, path: &std::path::Path) -> std::io::Result<fuser::BackgroundSession> {
        let mut config = fuser::Config::default();
        config.mount_options = vec![
            fuser::MountOption::RO,
            fuser::MountOption::NoDev,
            fuser::MountOption::NoSuid,
            fuser::MountOption::DefaultPermissions,
            fuser::MountOption::FSName(format!("cirrove:{}", self.inner.engine.account.label)),
            fuser::MountOption::Subtype("cirrove".into()),
        ];
        let inner = self.inner.clone();
        let session = fuser::spawn_mount(self, path, &config)?;
        CloudFs { inner }.start_invalidations(session.notifier());
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
    fn project(parent: &View, child: Node) -> Result<(String, View), ProviderError> {
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
            node.target = None;
        }
        if node.kind == NodeKind::Folder {
            ancestry.push((scope.collection.clone(), node.id.clone()));
        }
        let key = serde_json::to_string(&(&scope.account, &alias, &scope.collection, &node.id))
            .map_err(|_| ProviderError::Unavailable)?;
        let view = View {
            inode: 0,
            parent: parent.inode,
            scope,
            node,
            name,
            alias,
            ancestry,
        };
        Ok((key, view))
    }
    async fn insert(&self, parent: &View, child: Node) -> Result<View, ProviderError> {
        let (key, mut view) = Self::project(parent, child)?;
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
        let nodes = self.engine.children(&parent.scope, &parent.node.id).await?;
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
        let mut keys = vec![];
        let mut projected = vec![];
        for node in nodes {
            match Self::project(&parent, node) {
                Ok((key, view)) => {
                    keys.push(key);
                    projected.push(view);
                }
                Err(ProviderError::Protocol(_)) => {
                    tracing::warn!("cloud entry could not be projected (cycle or invalid name)")
                }
                Err(error) => return Err(error),
            }
        }
        let db = self.engine.db.clone();
        let inodes = tokio::task::spawn_blocking(move || Store::open(db)?.inodes(&keys))
            .await
            .map_err(|_| ProviderError::Unavailable)?
            .map_err(|_| ProviderError::Unavailable)?;
        let mut views = self.views.lock().map_err(|_| ProviderError::Unavailable)?;
        for (mut view, inode) in projected.into_iter().zip(inodes) {
            view.inode = inode;
            views.insert(inode, view.clone());
            result.push(view);
        }
        Ok(result)
    }
    async fn node(&self, view: &View) -> Result<Node, ProviderError> {
        if view.inode == 1 {
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
            perm: if directory { 0o500 } else { 0o400 },
            nlink: if directory { 2 } else { 1 },
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
                let nodes = inner
                    .engine
                    .children(&parent.scope, &parent.node.id)
                    .await?;
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
    fn open(&self, _req: &Request, inode: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        if flags.0 & libc::O_ACCMODE != libc::O_RDONLY {
            reply.error(Errno::EROFS);
            return;
        }
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
                if node.kind != NodeKind::File {
                    return Err(ProviderError::Protocol("not a regular file"));
                }
                let handle = inner.handle();
                inner
                    .files
                    .lock()
                    .map_err(|_| ProviderError::Unavailable)?
                    .insert(handle, OpenFile { view, node });
                Ok::<_, ProviderError>(handle)
            }
            .await;
            match result {
                Ok(handle) => reply.opened(FileHandle(handle), FopenFlags::FOPEN_DIRECT_IO),
                Err(e) => reply.error(errno(&e)),
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
        let Ok(permit) = self.inner.reads.clone().try_acquire_owned() else {
            reply.error(Errno::EAGAIN);
            return;
        };
        let inner = self.inner.clone();
        self.inner.runtime.spawn(async move {
            let _permit = permit;
            let result = async {
                let file = inner
                    .files
                    .lock()
                    .map_err(|_| ProviderError::Unavailable)?
                    .get(&handle.0)
                    .cloned()
                    .ok_or(ProviderError::NotFound)?;
                inner
                    .engine
                    .cache
                    .read(
                        inner.engine.provider.as_ref(),
                        &file.view.scope,
                        &file.node,
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
