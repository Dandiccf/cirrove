//! FUSE projection: ordinary mounts are read-only; isolated test mounts allow edits.
//! Callbacks dispatch asynchronous work; no callback
//! holds the namespace map while awaiting a provider or a database operation.
#[cfg(test)]
mod capacity;
mod ceiling;
mod directories;
mod invalidation;
mod lifecycle;
mod residency;
#[cfg(test)]
mod trash;
/// How many namespace views have been quarantined since start. Re-exported so a
/// daemon can report it: the flag is set on a reference-count inconsistency,
/// cleared nowhere, and a quarantined view pins its ancestor chain for the life
/// of the mount.
pub use residency::quarantined_views;
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
use tokio::{
    runtime::Handle,
    sync::{OwnedSemaphorePermit, Semaphore},
};

/// How long the kernel may cache an entry or attribute before asking again.
///
/// A constant, and deliberately not configurable. It was briefly overridable so
/// that ADR 0006 could measure whether a shorter TTL lowers the traversal peak.
/// It does not: a hundredfold reduction moved the peak by half a percent, because
/// expiry is revalidation rather than eviction and the kernel's dentry shrinker
/// runs under memory pressure, not on a clock. See
/// docs/benchmarks/namespace-entry-ttl.json before reaching for this again.
const TTL: Duration = Duration::from_secs(1);
const READ_QUEUE_TIMEOUT: Duration = Duration::from_secs(30);
/// The mount point itself. FUSE fixes it at 1, and `CloudFs::new` builds the root
/// view with that inode.
const ROOT_INODE: u64 = 1;

/// The names the freedesktop trash specification puts at the top of a mounted
/// filesystem: `$topdir/.Trash`, and `$topdir/.Trash-$uid` when the first is
/// absent or not sticky.
///
/// A cloud mount must refuse to hold one. GIO creates it on the first Delete in
/// a file manager and then *renames* files into it, so a trash here would be a
/// second wastebasket living inside the user's own drive -- visible on every
/// other device, syncing its contents, and leaving the provider's recycle bin
/// empty while the file manager reports the deletion as undoable. The cloud
/// already has a recycle bin, and `MutationIntent::RemoveFile` already reaches
/// it, which is what makes the local one redundant rather than merely untidy.
///
/// `EOPNOTSUPP` is the refusal because GIO reads it as "this filesystem has no
/// trash" and falls back to asking about permanent deletion. That prompt is
/// still not the truth -- the delete underneath goes to the provider's recycle
/// bin -- and ADR 0008 records why the honest version needs more than a guard.
///
/// Only the mount root is refused. A `.Trash-1000` the user keeps somewhere
/// inside their drive is their folder, and no trash implementation looks there.
/// A name the provider would refuse, as the errno an application can act on:
/// a limit is `ENAMETOOLONG`, everything else `EINVAL` -- the same answers a
/// local filesystem gives for a name it cannot hold.
fn name_errno(problem: cirrove_core::NameProblem) -> Errno {
    match problem {
        cirrove_core::NameProblem::TooLong => Errno::ENAMETOOLONG,
        cirrove_core::NameProblem::Invalid(_) => Errno::EINVAL,
    }
}
pub(crate) fn is_trash_directory(name: &str) -> bool {
    name == ".Trash"
        || name
            .strip_prefix(".Trash-")
            .is_some_and(|uid| !uid.is_empty() && uid.bytes().all(|b| b.is_ascii_digit()))
}
/// Allocator trims performed since start. Three attempts at the trim condition
/// failed because whether it fired could only be inferred from the memory it was
/// supposed to move; this makes it a number a fixture can assert on directly.
pub(crate) static TRIMS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many times this process has returned free pages to the kernel.
///
/// Exposed because the daemon had no way to answer "has the trim ever fired?".
/// It logged at debug against a daemon running at info, so a real session could
/// only be inferred from memory that never came back -- which is how a trigger
/// that could not fire on a read workload went unnoticed.
pub fn allocator_trims() -> u64 {
    TRIMS.load(std::sync::atomic::Ordering::Relaxed)
}

#[derive(Clone)]
struct View {
    residency: Arc<LookupRefs>,
    // A view (including a directory snapshot or old open file) protects its
    // parent entry. The retained parent view protects the rest of the chain.
    _parent_residency: Option<Arc<LookupRefs>>,
    inode: u64,
    parent: u64,
    // Immutable metadata is shared by operations/open handles. Sibling files
    // also share their unchanged scope, alias route and ancestry.
    scope: Arc<Scope>,
    // What a live view remembers of its item, instead of a whole `Node`.
    //
    // Counted before this changed: of 74 reads of the node, 41 wanted `id` and
    // 14 wanted `kind`; the nine uses of the whole node were all assignments.
    // A `Node` per live view is five heap strings kept in case somebody asks,
    // and at 750,438 views during a traversal that is the larger half of the
    // 650 bytes each one costs.
    //
    // The id is an `Arc<str>` so the invalidation index can share it rather
    // than keeping the separate `Box<str>` copy measurement found it holding.
    id: Arc<str>,
    kind: NodeKind,
    size: u64,
    modified_unix: u64,
    package: bool,
    // The whole node, and only where a writeback exists.
    //
    // The write path needs all of it: `materialize` puts the node into the
    // journal's namespace, and `unlink` must name the version the caller looked
    // at rather than a fresh one -- fetching would delete a version nobody saw,
    // which is what ADR 0008 exists to prevent. So a writable mount keeps what
    // it kept before.
    //
    // A read-only mount needs none of it, and that is where a traversal of
    // 750,000 files happens. `None` is eight bytes; the `Arc<Node>` it replaces
    // was about 336 with its allocation and its five heap strings.
    node: Option<Arc<Node>>,
    name: Arc<str>,
    alias: Arc<Vec<(String, String)>>,
    reference: bool,
    entry: Option<Arc<Node>>,
    ancestry: Arc<Vec<(String, String)>>,
}
impl View {
    /// Take from a node exactly what a live view keeps. Everything else stays
    /// in the store, which is where it already is.
    fn remember(&mut self, node: &Node, writable: bool) {
        self.id = node.id.as_str().into();
        self.kind = node.kind.clone();
        self.size = node.size;
        self.modified_unix = node.modified_unix;
        self.package = node.package;
        if writable {
            self.node = Some(Arc::new(node.clone()));
        }
    }
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
    invalidation_metrics: invalidation::InvalidationMetrics,
    ceiling_metrics: ceiling::CeilingMetrics,
    over_ceiling: tokio::sync::Notify,
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
                match writer.working(&file.view.scope, &file.view.id) {
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
            package: false,
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
        let mut view = View {
            residency: Arc::default(),
            _parent_residency: None,
            inode: ROOT_INODE,
            parent: ROOT_INODE,
            ancestry: vec![(scope.collection.clone(), root.id.clone())].into(),
            scope: scope.into(),
            id: Arc::from(""),
            kind: NodeKind::Folder,
            size: 0,
            modified_unix: 0,
            package: false,
            node: None,
            name: engine.account.label.as_str().into(),
            alias: vec![].into(),
            reference: false,
            entry: None,
        };
        // The root's node is synthesised, not stored: its name is the account
        // label, which no row carries. So this one view keeps it -- one
        // allocation for the life of the mount -- and `remember` is asked for
        // the writable shape whatever the mount, for that reason alone.
        view.remember(&root, true);
        let root = view;
        Ok(Self {
            inner: Arc::new(Inner {
                cancel: engine.cancel.child_token(),
                engine,
                writeback: None,
                edits: lifecycle::EditAdmission::new(),
                runtime: Handle::current(),
                views: Mutex::new({
                    let mut views = NamespaceViews::new(root);
                    views.set_ceiling(ceiling::configured());
                    views
                }),
                invalidation_metrics: invalidation::InvalidationMetrics::default(),
                ceiling_metrics: ceiling::CeilingMetrics::default(),
                over_ceiling: tokio::sync::Notify::new(),
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
        let wake = self.inner.engine.changed.subscribe();
        self.inner
            .runtime
            .spawn(ceiling::run(self.inner.clone(), notifier.clone()));
        self.inner
            .runtime
            .spawn(invalidation::run(self.inner.clone(), notifier, wake));
    }
    fn start_view_reclamation(&self) {
        let inner = self.inner.clone();
        self.inner.runtime.spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            // A traversal leaves most of its memory freed but not returned. Give
            // it back once the mount has shed what it was holding and gone quiet.
            //
            // The signal has to be the view count, not the collector's own work.
            // `forget` reclaims directly when the kernel drops its last reference,
            // so on a traversal workload the collector reclaims nothing at all and
            // anything keyed to its return value fires once at mount, against an
            // empty namespace, and never again. Two earlier spellings of this
            // failed exactly that way and the measurements looked like a trim that
            // did nothing.
            //
            // So: remember the most views held since the last trim, and trim when
            // the mount is quiet and now holds far fewer. That is precisely when
            // there are freed pages worth returning.
            // The view-count signal above catches a traversal. It cannot catch a
            // mount that only reads content: reading changes no view count, so
            // `high_water` never exceeds `held * 2` and never exceeds SHED_FLOOR,
            // and the trim never runs at all. Measured on a live daemon reading
            // 475 MB out of cache, peak and residue were the same number in
            // nearly every pass -- it returned none of what it took.
            //
            // So: a second, independent signal. Free bytes across the arenas is
            // exactly what trimming would give back, and it is the only figure
            // that separates a process holding half a gibibyte it has already
            // freed from one that is genuinely using it. Either signal may fire;
            // both are gated on the same quiet mount and the same blocking
            // worker.
            //
            // The floor only avoids a pointless walk; it is not what protects
            // throughput. The quiescent gate does that -- the trim cannot fire
            // while the mount is doing anything -- so the floor sits where
            // returning the memory stops being worth the walk. A pass measured
            // 717 microseconds around 50 MiB, roughly 14 microseconds per
            // mebibyte, so eight is about a hundred microseconds.
            //
            // A cadence, not a threshold on a quantity. Three quantities were
            // tried and all three were wrong, which is recorded in
            // docs/benchmarks/trim-cadence.json: free arena bytes and
            // resident-minus-live do not fall when a trim succeeds, so both fire
            // every tick forever, and resident growth since the last trim cannot
            // tell a partial reclaim from nothing to do, so it froze the daemon
            // at 320 MiB for eight and a half hours while an arm that trimmed
            // freely reached 165 on the same workload.
            //
            // Every one of those was a proxy for "is there something to give
            // back", and none of the available quantities answers it. A cadence
            // does not need the answer: it pays a known, bounded cost to ask the
            // allocator, which is the one thing that does know. A pass around
            // 100 MiB is roughly 1.5 ms, so once a minute is about 0.0025
            // percent of a core.
            //
            // The floor keeps a settled small process from paying even that. A
            // daemon that has never grown past it has nothing worth a walk.
            //
            // Resident size above the floor is a gate on whether a walk is worth
            // it, NOT the trigger -- the interval is the trigger. That
            // distinction is the whole of the change: resident-minus-live was
            // tried as a trigger and is one of the three recorded failures,
            // because it includes the SQLite page cache over a 560 MB index and
            // so sits permanently around 90 MiB above any floor.
            //
            // What the cadence bought and what it did not, measured over two
            // hours on the live daemon in docs/benchmarks/trim-cadence.json:
            // trims fired 118 times at 0.99 a minute against 78 frozen over
            // eight and a half hours, and the mean came down to 209.1 MiB from
            // 320.2. But resident size drifted +40.5 MiB between the first hour
            // and the second while the cadence fired steadily, and the peak
            // touched 250.3 MiB against a registered ceiling of 250. The run is
            // recorded as NOT SETTLED: this is better than what it replaces and
            // is not shown to be bounded, and a longer run is what would tell an
            // asymptote from a ramp.
            const QUIESCENT_TICKS: u32 = 5;
            const SHED_FACTOR: usize = 2;
            const SHED_FLOOR: usize = 1024;
            // Both overridable for tests only, in the style of this crate's
            // other fixture variables. A read-only mount fixture retains a flat 16.5
            // MiB however much it reads -- the baseline and nothing more -- so it
            // cannot reach a floor derived from a daemon holding 184,000 nodes
            // and a 560 MB index. That is a fact about the fixture's size, not
            // about the trigger, and lowering the floor lets a test assert the
            // mechanism while the shipped number stays what measurement chose.
            let floor = std::env::var("CIRROVE_RECLAIM_FLOOR_BYTES")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(96 * 1024 * 1024);
            let interval = std::env::var("CIRROVE_RECLAIM_INTERVAL_SECONDS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(60);
            let mut idle = 0u32;
            let mut high_water = 0usize;
            // Far enough in the past that the first pass is not delayed.
            let mut last_trim = tokio::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(interval))
                .unwrap_or_else(tokio::time::Instant::now);
            loop {
                tokio::select! { biased;
                    _ = inner.cancel.cancelled() => break,
                    _ = tick.tick() => {
                        // Drained in short passes that each release the lock,
                        // not one long one. A single fixed budget a second is a
                        // constant against a backlog that grows with the
                        // traversal, and the Fedora VM showed where that ends:
                        // 2.7 million stale candidates, 71 MB of queue, and the
                        // peak criterion failing because of it.
                        let mut reclaimed = 0;
                        let mut held = 0;
                        let mut passes = 0;
                        loop {
                            let again = match inner.views.lock() {
                                Ok(mut views) => {
                                    reclaimed += views.collect(4096);
                                    held = views.len();
                                    passes += 1;
                                    views.wants_another_pass(passes)
                                }
                                Err(_) => break,
                            };
                            if !again {
                                break;
                            }
                            tokio::task::yield_now().await;
                        }
                        if passes == 0 {
                            continue;
                        }
                        // The guard is dropped before trimming: trim takes every
                        // arena lock in turn, and holding the namespace lock
                        // across that would block every filesystem reply.
                        idle = if reclaimed == 0 {
                            idle.saturating_add(1)
                        } else {
                            0
                        };
                        high_water = high_water.max(held);
                        let shed = high_water > held.saturating_mul(SHED_FACTOR).max(SHED_FLOOR);
                        let resident = cirrove_allocator::resident_bytes();
                        let retained = resident >= floor
                            && last_trim.elapsed() >= std::time::Duration::from_secs(interval);
                        if idle >= QUIESCENT_TICKS && (shed || retained) {
                            high_water = held;
                            TRIMS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            last_trim = tokio::time::Instant::now();
                            tokio::task::spawn_blocking(move || {
                                let released = cirrove_allocator::trim();
                                // At info, not debug: this was invisible in a
                                // real session, so "has it ever fired?" could
                                // not be answered from the journal, only
                                // inferred from memory that never came back.
                                tracing::info!(
                                    released,
                                    shed,
                                    retained,
                                    "returned free pages to the kernel"
                                );
                            });
                        }
                    }
                }
            }
        });
    }
    /// Mount options, and deliberately not a thread count.
    ///
    /// Dispatch stays single-threaded. Raising fuser's `n_threads` to four leaves
    /// three namespace views alive after twelve seconds in
    /// `filesystem::capacity::real_combined_namespace_churn_preserves_mapped_content`:
    /// concurrent dispatch reorders requests against the lookup-count bookkeeping
    /// a view's lifetime depends on, because `acquire_lookup` runs inside a
    /// spawned task while `forget` runs on the dispatch thread, and one thread
    /// serialises them by construction where four do not. Head-of-line blocking
    /// behind bulk reads is therefore still possible, and making the reference
    /// accounting order-independent is a prerequisite for addressing it.
    ///
    /// Split out so a test can hold that decision in place. The failure it
    /// prevents is a handful of views surviving a settle window in one ignored
    /// fixture -- quiet, intermittent-looking, and easy to attribute to anything
    /// else.
    fn dispatch_config(&self) -> fuser::Config {
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
        config
    }
    pub fn mount(self, path: &std::path::Path) -> std::io::Result<CloudSession> {
        let config = self.dispatch_config();
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
    /// Take the kernel's reference on a view, and wake the ceiling if this is
    /// the reference that put the mount over it.
    ///
    /// The wake is here rather than on a timer because a mount under its
    /// ceiling must cost nothing at all -- a 50 ms poll is 20 wakeups a second
    /// on a laptop doing nothing.
    fn acquire_lookup(&self, inode: u64) -> Result<(), ProviderError> {
        let mut views = self.views.lock().map_err(|_| ProviderError::Unavailable)?;
        views.acquire_lookup(inode)?;
        let over = views.over_ceiling() > 0;
        drop(views);
        if over {
            self.over_ceiling.notify_one();
        }
        Ok(())
    }
    fn view(&self, inode: u64) -> Result<View, ProviderError> {
        self.views
            .lock()
            .map_err(|_| ProviderError::Unavailable)?
            .get(&inode)
            .cloned()
            .ok_or(ProviderError::NotFound)
    }
    /// Whether this view lies in a trash directory at the mount root.
    ///
    /// Refusing to *create* one is not enough on its own. A file manager also
    /// adopts an existing `$topdir/.Trash-$uid` -- left by an earlier Cirrove, or
    /// by another tool -- and trashing is a rename into it, not a mkdir. Without
    /// this the guard in `mkdir` would hold only for drives that never had one.
    ///
    /// Only renames *into* it are refused. A user whose drive already contains a
    /// trash directory must be able to move their files back out of it, and
    /// refusing that would trap them there.
    fn inside_root_trash(&self, view: &View) -> bool {
        let mut current = view.clone();
        // Bounded rather than `loop`: a damaged parent chain must refuse an
        // answer, not hang the rename that asked.
        for _ in 0..256 {
            if current.inode == ROOT_INODE {
                return false;
            }
            if current.parent == ROOT_INODE {
                return is_trash_directory(&current.name);
            }
            match self.view(current.parent) {
                Ok(parent) => current = parent,
                Err(_) => return false,
            }
        }
        false
    }
    /// Returns the node beside the view rather than inside it: the caller
    /// wants it for the inode key and for attributes, and a view that keeps it
    /// for its whole life is what costs 650 bytes each during a traversal.
    fn project(parent: &View, child: Node, writable: bool) -> Result<(View, Node), ProviderError> {
        if child.name.is_empty()
            || child.name == "."
            || child.name == ".."
            || child.name.contains('/')
            || child.name.contains('\0')
        {
            return Err(ProviderError::Protocol("invalid cloud filename"));
        }
        let name = child.name.as_str().into();
        let mut scope = parent.scope.clone();
        let mut alias = parent.alias.clone();
        let mut ancestry = parent.ancestry.clone();
        let entry = child.target.as_ref().map(|_| Arc::new(child.clone()));
        let mut node = child;
        if let Some(target) = node.target.take() {
            Arc::make_mut(&mut scope).collection = target.collection.clone();
            if ancestry
                .iter()
                .any(|v| v == &(target.collection.clone(), target.item.clone()))
                || ancestry.len() > 128
            {
                return Err(ProviderError::Protocol("shortcut cycle or excessive depth"));
            }
            Arc::make_mut(&mut alias).push((parent.scope.collection.clone(), node.id.clone()));
            node.id = target.item.clone();
            node.kind = target.kind.clone().unwrap_or(NodeKind::Folder);
            node.etag = None;
            node.content_version = None;
            node.target = None;
        }
        if node.kind == NodeKind::Folder {
            Arc::make_mut(&mut ancestry).push((scope.collection.clone(), node.id.clone()));
        }
        let view = View {
            residency: Arc::default(),
            _parent_residency: Some(parent.residency.clone()),
            inode: 0,
            parent: parent.inode,
            scope,
            id: node.id.as_str().into(),
            kind: node.kind.clone(),
            size: node.size,
            modified_unix: node.modified_unix,
            package: node.package,
            node: writable.then(|| Arc::new(node.clone())),
            name,
            alias,
            reference: entry.is_some(),
            entry,
            ancestry,
        };
        // The node goes back to the caller rather than into the view: it is
        // wanted for the inode key and for attributes, both while the caller
        // still holds it. Keeping it for the life of the view is the cost.
        Ok((view, node))
    }
    /// The node is passed rather than read off the view: a view carries its
    /// identity, and the content revision belongs to the metadata, which every
    /// caller of this holds already.
    fn inode_key(view: &View, node: &Node, writable: bool) -> Result<String, ProviderError> {
        let identity = (
            &view.scope.account,
            view.alias.as_ref(),
            &view.scope.collection,
            &*view.id,
        );
        // Regular-file revisions have independent kernel page caches. Stable
        // provider identity remains account/drive/item; names never enter the key.
        if view.kind == NodeKind::File && !writable {
            serde_json::to_string(&(
                "content-inode-v1",
                identity,
                node.content_revision(),
                node.size,
            ))
        } else {
            serde_json::to_string(&identity)
        }
        .map_err(|_| ProviderError::Unavailable)
    }
    async fn insert(&self, parent: &View, child: Node) -> Result<View, ProviderError> {
        let writable = self.writeback.is_some();
        // The node lives as long as this call, not as long as the view.
        let (mut view, mut node) = Self::project(parent, child, writable)?;
        if view.reference {
            let (local, retained) = match &self.writeback {
                Some(writer) => writer.reference_view(&view.scope, &view.id).map_err(|e| {
                    if e == Errno::ENOENT {
                        ProviderError::NotFound
                    } else {
                        ProviderError::Unavailable
                    }
                })?,
                None => (None, false),
            };
            if let Some(local) = local {
                view.id = local.as_str().into();
                node = self.node(&view).await?;
                view.remember(&node, writable);
            } else if view.kind == NodeKind::Folder && retained {
                // A retained shortcut is the local route to an absent target
                // root. Its directory view remains traversable without metadata.
            } else {
                node = self.engine.node(&view.scope, &view.id).await?;
                view.remember(&node, writable);
            }
        }
        let key = Self::inode_key(&view, &node, writable)?;
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
        if self.writeback.is_none() {
            return self.streaming_listing(parent).await;
        }
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
        // Preserve the atomic writable overlay until it has a streaming contract.
        let nodes = self.children(parent).await.map_err(|e| errno(&e))?;
        tokio::task::spawn_blocking(move || build(&mut nodes.into_iter().map(Ok)))
            .await
            .map_err(|_| Errno::EIO)?
    }
    async fn streaming_listing(&self, parent: &View) -> Result<OpenDirectory, Errno> {
        let route = [
            parent.clone(),
            self.view(parent.parent).map_err(|e| errno(&e))?,
        ];
        let (engine, budget, cancel) = (
            self.engine.clone(),
            self.directory_budget.clone(),
            self.cancel.clone(),
        );
        let (scope, item, db) = (parent.scope.clone(), parent.id.clone(), engine.db.clone());
        let (send, receive) = tokio::sync::oneshot::channel();
        let worker = self.runtime.spawn(async move {
            let mut send = Some(send);
            let (builder, mut send, route) = engine
                .with_children(&scope, &item, move |nodes| {
                    let mut send = send.take();
                    let mut batches = 0usize;
                    let builder = Self::build_snapshot(
                        nodes,
                        &route,
                        &db,
                        &budget,
                        false,
                        &cancel,
                        |builder| {
                            batches += 1;
                            if (batches - 1).is_multiple_of(8)
                                && let Some(snapshot) = builder.publish()?
                            {
                                Self::send_listing(&mut send, snapshot, &route)?;
                            }
                            Ok(())
                        },
                    )?;
                    Ok::<_, Errno>((builder, send, route.clone()))
                })
                .await
                .map_err(|error| errno(&error))??;
            // Do not announce EOF before the consistent metadata read finishes.
            // Final buffered writes and descriptor cleanup still belong on a
            // blocking worker, even though the initial scan has already ended.
            tokio::task::spawn_blocking(move || {
                if let Some(snapshot) = builder.complete()? {
                    Self::send_listing(&mut send, snapshot, &route)?;
                }
                Ok::<_, Errno>(())
            })
            .await
            .map_err(|_| Errno::EIO)?
        });
        match receive.await {
            Ok(listing) => Ok(listing),
            // A cold-provider or pre-publication error keeps its original errno.
            Err(_) => match worker.await {
                Ok(Err(error)) => Err(error),
                _ => Err(Errno::EIO),
            },
        }
    }
    fn send_listing(
        send: &mut Option<tokio::sync::oneshot::Sender<OpenDirectory>>,
        snapshot: directories::Snapshot,
        route: &[View; 2],
    ) -> Result<(), Errno> {
        send.take()
            .ok_or(Errno::EIO)?
            .send(OpenDirectory {
                snapshot,
                _route: route.clone(),
            })
            .map_err(|_| Errno::ENODEV)
    }
    fn build_listing(
        nodes: &mut dyn Iterator<Item = cirrove_store::Result<Node>>,
        route: &[View; 2],
        db: &std::path::Path,
        budget: &directories::Budget,
        writable: bool,
        cancel: &CancellationToken,
    ) -> Result<OpenDirectory, Errno> {
        let snapshot =
            Self::build_snapshot(nodes, route, db, budget, writable, cancel, |_| Ok(()))?;
        Ok(OpenDirectory {
            snapshot: snapshot.finish()?,
            _route: route.clone(),
        })
    }
    fn build_snapshot(
        nodes: &mut dyn Iterator<Item = cirrove_store::Result<Node>>,
        route: &[View; 2],
        db: &std::path::Path,
        budget: &directories::Budget,
        writable: bool,
        cancel: &CancellationToken,
        mut publish: impl FnMut(&mut directories::Builder) -> Result<(), Errno>,
    ) -> Result<directories::Builder, Errno> {
        let mut store = Store::open(db).map_err(|_| Errno::EIO)?;
        let mut snapshot = budget.start(db.parent().ok_or(Errno::EIO)?)?;
        let result = (|| {
            snapshot.push(route[0].inode, true, ".")?;
            snapshot.push(route[1].inode, true, "..")?;
            let mut projected = Vec::with_capacity(128);
            for node in nodes {
                if cancel.is_cancelled() || snapshot.cancelled() {
                    return Err(Errno::ENODEV);
                }
                match Self::project(&route[0], node.map_err(|_| Errno::EIO)?, writable) {
                    Ok(view) => projected.push(view),
                    Err(ProviderError::Protocol(_)) => {
                        tracing::warn!(
                            "cloud entry could not be projected (cycle or invalid name)"
                        );
                    }
                    Err(error) => return Err(errno(&error)),
                }
                if projected.len() == 128 {
                    Self::snapshot_batch(&mut store, &mut projected, writable, &mut snapshot)?;
                    publish(&mut snapshot)?;
                }
            }
            Self::snapshot_batch(&mut store, &mut projected, writable, &mut snapshot)?;
            if cancel.is_cancelled() || snapshot.cancelled() {
                return Err(Errno::ENODEV);
            }
            Ok(())
        })();
        result.inspect_err(|error: &Errno| snapshot.fail(error.code()))?;
        Ok(snapshot)
    }
    /// The nodes travel with their views through the batch and are dropped with
    /// it. A batch is 128 entries; a view outlives the listing that made it.
    fn snapshot_batch(
        store: &mut Store,
        projected: &mut Vec<(View, Node)>,
        writable: bool,
        snapshot: &mut directories::Builder,
    ) -> Result<(), Errno> {
        for (view, node) in projected.iter_mut() {
            // A cold link stays provisional until LOOKUP resolves its target;
            // listing only uses cached target metadata, never provider I/O.
            if view.reference
                && let Some(fresh) = store.node(&view.scope, &view.id).map_err(|_| Errno::EIO)?
            {
                view.remember(&fresh, writable);
                *node = fresh;
            }
        }
        let keys = projected
            .iter()
            .map(|(view, node)| Self::inode_key(view, node, writable).map_err(|e| errno(&e)))
            .collect::<Result<Vec<_>, _>>()?;
        let inodes = store.inodes(&keys).map_err(|_| Errno::EIO)?;
        for ((view, _node), inode) in projected.drain(..).zip(inodes) {
            // READDIR does not create kernel lookup references. Only the parent
            // route is retained by the snapshot, not every projected child.
            snapshot.push(inode, view.kind == NodeKind::Folder, &view.name)?;
        }
        Ok(())
    }
    async fn children(&self, parent: &View) -> Result<Vec<Node>, ProviderError> {
        let identity = match &self.writeback {
            Some(writer) => writer
                .directory_identity(&parent.scope, &parent.id)
                .map_err(|_| ProviderError::Unavailable)?,
            None => Some(parent.id.to_string()),
        };
        let nodes = match identity {
            Some(item) => match self.engine.children(&parent.scope, &item).await {
                Ok(nodes) => nodes,
                Err(ProviderError::NotFound) => {
                    let retained = match &self.writeback {
                        Some(w) => w
                            .retains_directory(&parent.scope, &parent.id)
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
                .overlay(&parent.scope, &parent.id, nodes)
                .map_err(|_| ProviderError::Unavailable),
            None => Ok(nodes),
        }
    }
    async fn node(&self, view: &View) -> Result<Node, ProviderError> {
        if let Some(writer) = &self.writeback
            && let Some(node) = writer
                .node(&view.scope, &view.id)
                .map_err(|_| ProviderError::Unavailable)?
        {
            return Ok(node);
        }
        if let Some(writer) = &self.writeback
            && view.kind == NodeKind::Folder
            && writer
                .retains_directory(&view.scope, &view.id)
                .map_err(|_| ProviderError::Unavailable)?
        {
            return Ok(view
                .node
                .as_ref()
                .ok_or(ProviderError::Unavailable)?
                .as_ref()
                .clone());
        }
        if let Some(writer) = &self.writeback
            && writer
                .follows_remote(&view.scope, &view.id)
                .map_err(|_| ProviderError::Unavailable)?
        {
            let item = writer
                .remote_identity(&view.scope, &view.id)
                .map_err(|_| ProviderError::Unavailable)?
                .ok_or(ProviderError::Unavailable)?;
            let mut node = self.engine.node(&view.scope, &item).await?;
            node = writer
                .present_node(&view.scope, &view.id, node)
                .map_err(|_| ProviderError::Unavailable)?;
            node.id = view.id.to_string();
            writer
                .localize_parent(&view.scope, &mut node)
                .map_err(|_| ProviderError::Unavailable)?;
            return Ok(node);
        }
        // A writable mount keeps the node and must answer with the version the
        // caller looked at rather than the current one (ADR 0008). A read-only
        // view keeps none; the callers that still need a whole node there --
        // `open`, and the content path behind it -- pay a store read for it,
        // which an attribute no longer does.
        if (view.inode == ROOT_INODE || view.kind == NodeKind::File)
            && let Some(node) = &view.node
        {
            return Ok(node.as_ref().clone());
        }
        self.engine.node(&view.scope, &view.id).await
    }
    /// The attributes of a view, and nothing else read to find them.
    ///
    /// A live view keeps exactly what a `getattr` answers with -- kind, size
    /// and modified time -- so a read-only mount answers one from the view
    /// alone: no node held for its lifetime, and no store read per call.
    async fn attributes_of(&self, view: &View) -> Result<FileAttr, ProviderError> {
        match &self.writeback {
            None => Ok(self.attr_of(view)),
            // A working copy, an overlay or a retained recovery route can each
            // make the view stale, so a writable mount asks, as it always has.
            Some(_) => {
                let node = self.node(view).await?;
                Ok(self.attr(view, &node))
            }
        }
    }
    fn attr_of(&self, view: &View) -> FileAttr {
        self.attributes(view, view.kind.clone(), view.size, view.modified_unix)
    }
    /// Attributes from a node the caller holds, which may be newer than the
    /// view: a truncate answers with the size it has just written.
    fn attr(&self, view: &View, node: &Node) -> FileAttr {
        self.attributes(view, node.kind.clone(), node.size, node.modified_unix)
    }
    fn attributes(&self, view: &View, kind: NodeKind, size: u64, modified_unix: u64) -> FileAttr {
        let directory = kind == NodeKind::Folder;
        let time = UNIX_EPOCH + Duration::from_secs(modified_unix);
        FileAttr {
            ino: INodeNo(view.inode),
            size,
            blocks: size.div_ceil(512),
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
                .is_some_and(|w| w.is_unlinked(&view.scope, &view.id).unwrap_or(false))
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
/// Admission waits for its turn. It never refuses.
///
/// These semaphores bound how much work runs at once, not how much the kernel
/// is allowed to ask for. Refusing an ordinary operation with `EAGAIN` is a
/// contract a filesystem cannot offer: POSIX permits that answer only on a
/// descriptor opened `O_NONBLOCK`, so a caller that never asked for one reads
/// it as damage rather than as backpressure. On 2026-09-16 a desktop file
/// indexer crawling a mount on the owner's machine collected 13,589 failures,
/// 7,892 of them reported as damaged PDF documents, because a parser met
/// `EAGAIN` in the middle of a file it was entitled to read. Nothing was
/// actually wrong with those files. ADR 0006 named this failure in advance --
/// "`ls: Resource temporarily unavailable` is the failure a user would see" --
/// and it was never closed. ADR 0012 records the change.
///
/// The queue this creates cannot grow without bound: the kernel limits how many
/// FUSE requests are outstanding, so the waiters are capped by the requests the
/// kernel is willing to have in flight, not by the callers behind them.
async fn admit(gate: Arc<Semaphore>, cancel: &CancellationToken) -> Option<OwnedSemaphorePermit> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => None,
        permit = gate.acquire_owned() => permit.ok(),
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
        let gate = self.inner.pending.clone();
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
            let Some(_permit) = admit(gate, &inner.cancel).await else {
                reply.error(Errno::ENODEV);
                return;
            };
            let result = async {
                let node = if inner.writeback.is_none() {
                    let name = name.to_str().ok_or(ProviderError::NotFound)?;
                    inner.engine.child(&parent.scope, &parent.id, name).await?
                } else {
                    // Pending local edits, aliases and retained recovery routes
                    // must participate in the writable namespace lookup.
                    inner
                        .children(&parent)
                        .await?
                        .into_iter()
                        .find(|n| OsStr::new(&n.name) == name)
                        .ok_or(ProviderError::NotFound)?
                };
                let view = inner.insert(&parent, node).await?;
                let attr = inner.attributes_of(&view).await?;
                Ok::<_, ProviderError>((view, attr))
            }
            .await;
            match result {
                Ok((view, attr)) => match inner.acquire_lookup(view.inode) {
                    Ok(()) => reply.entry(&TTL, &attr, Generation(0)),
                    Err(error) => reply.error(errno(&error)),
                },
                Err(e) => reply.error(errno(&e)),
            }
        });
    }
    fn getattr(&self, _req: &Request, inode: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        let gate = self.inner.pending.clone();
        let inner = self.inner.clone();
        let view = match inner.view(inode.0) {
            Ok(view) => view,
            Err(error) => {
                reply.error(errno(&error));
                return;
            }
        };
        self.inner.runtime.spawn(async move {
            let Some(_permit) = admit(gate, &inner.cancel).await else {
                reply.error(Errno::ENODEV);
                return;
            };
            let result = inner.attributes_of(&view).await;
            match result {
                Ok(attr) => reply.attr(&TTL, &attr),
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
        if let Some(problem) = self.inner.engine.provider.name_problem(&name) {
            reply.error(name_errno(problem));
            return;
        }
        if parent.0 == ROOT_INODE && is_trash_directory(&name) {
            reply.error(Errno::EOPNOTSUPP);
            return;
        }
        let gate = self.inner.writes.clone();
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
            let Some(_permit) = admit(gate, &inner.cancel).await else {
                reply.error(Errno::ENODEV);
                return;
            };
            let _admission = admission;
            let result = async {
                if parent.kind != NodeKind::Folder {
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
                inner.refuse_within_package(&parent)?;
                inner.capture_ancestors(&parent).await?;
                let node = writer
                    .create_directory(parent.scope.as_ref().clone(), parent.id.to_string(), name)
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
        if let Some(problem) = self.inner.engine.provider.name_problem(&name) {
            reply.error(name_errno(problem));
            return;
        }
        let gate = self.inner.writes.clone();
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
            let Some(_permit) = admit(gate, &inner.cancel).await else {
                reply.error(Errno::ENODEV);
                return;
            };
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
                    package: false,
                    id: String::new(),
                    parent_id: Some(parent.id.to_string()),
                    name,
                    kind: NodeKind::File,
                    size: 0,
                    modified_unix: 0,
                    etag: None,
                    content_version: None,
                    target: None,
                };
                inner.refuse_within_package(&parent)?;
                inner.capture_ancestors(&parent).await?;
                let record = writer
                    .create(parent.scope.as_ref().clone(), node)
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
        // The old name is whatever it is; the new one is the one being chosen.
        if let Some(problem) = self.inner.engine.provider.name_problem(&newname) {
            reply.error(name_errno(problem));
            return;
        }
        let gate = self.inner.writes.clone();
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
            let Some(_permit) = admit(gate, &inner.cancel).await else {
                reply.error(Errno::ENODEV);
                return;
            };
            let _admission = admission;
            let result = async {
                if parent.scope != destination.scope || parent.alias != destination.alias {
                    return Err(Errno::EXDEV);
                }
                if parent.kind != NodeKind::Folder || destination.kind != NodeKind::Folder {
                    return Err(Errno::ENOTDIR);
                }
                // Trashing is a rename. See `inside_root_trash`.
                if inner.inside_root_trash(&destination) {
                    return Err(Errno::EOPNOTSUPP);
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
                if parent.id == destination.id && name == newname {
                    return if flags.contains(RenameFlags::RENAME_NOREPLACE) {
                        Err(Errno::EEXIST)
                    } else {
                        Ok(())
                    };
                }
                inner.refuse_within_package(&parent)?;
                inner.capture_ancestors(&parent).await?;
                inner.refuse_within_package(&destination)?;
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
                        .replace(
                            &inner,
                            parent.scope.as_ref().clone(),
                            source,
                            victim.clone(),
                        )
                        .await?;
                    inner
                        .insert(&destination, moved)
                        .await
                        .map_err(|e| errno(&e))?;
                    return Ok(());
                }
                let moved = writer
                    .relocate(
                        parent.scope.as_ref().clone(),
                        source,
                        parent.id.to_string(),
                        name,
                        destination.id.to_string(),
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
    /// Remove an empty directory, refusing a populated one the way POSIX does.
    ///
    /// The `ENOTEMPTY` below is the user-visible guarantee, and it is checked from
    /// the directory's own listing. The provider is checked again immediately
    /// before the delete, because Graph's DELETE on a folder is recursive and a
    /// folder's eTag does not move when a child appears, so no precondition can
    /// carry this. That leaves a window of one round trip in which a child created
    /// by someone else is removed along with the directory. It is documented on
    /// `MutationIntent::RemoveFolder` and it cannot be closed over Graph.
    fn rmdir(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let Some(writer) = self.inner.writeback.clone() else {
            reply.error(Errno::EROFS);
            return;
        };
        let Some(name) = name.to_str().map(str::to_owned) else {
            reply.error(Errno::EINVAL);
            return;
        };
        let gate = self.inner.writes.clone();
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
            let Some(_permit) = admit(gate, &inner.cancel).await else {
                reply.error(Errno::ENODEV);
                return;
            };
            let _admission = admission;
            let result = async {
                if parent.kind != NodeKind::Folder {
                    return Err(Errno::ENOTDIR);
                }
                let source = inner
                    .children(&parent)
                    .await
                    .map_err(|e| errno(&e))?
                    .into_iter()
                    .find(|n| n.name == name)
                    .ok_or(Errno::ENOENT)?;
                if source.kind != NodeKind::Folder {
                    return Err(Errno::ENOTDIR);
                }
                // A shortcut names a directory somewhere else. Removing the link
                // is not removing the target, and this path cannot express that.
                if source.target.is_some() {
                    return Err(Errno::EOPNOTSUPP);
                }
                let view = inner.insert(&parent, source).await.map_err(|e| errno(&e))?;
                inner.refuse_within_package(&view)?;
                if !inner
                    .children(&view)
                    .await
                    .map_err(|e| errno(&e))?
                    .is_empty()
                {
                    return Err(Errno::ENOTEMPTY);
                }
                writer.rmdir(&inner, view).await
            }
            .await;
            match result {
                Ok(()) => reply.ok(),
                Err(e) => reply.error(e),
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
        let gate = self.inner.writes.clone();
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
            let Some(_permit) = admit(gate, &inner.cancel).await else {
                reply.error(Errno::ENODEV);
                return;
            };
            let _admission = admission;
            let result = async {
                if parent.kind != NodeKind::Folder {
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
                // unlink and rmdir never walk their ancestors, so neither was
                // covered by the guard that sits beside capture_ancestors.
                inner.refuse_within_package(&view)?;
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
        let gate = self.inner.writes.clone();
        let Ok(admission) = self.inner.edits.admit() else {
            reply.error(Errno::ENODEV);
            return;
        };
        let inner = self.inner.clone();
        let bytes = data.to_vec();
        self.inner.runtime.spawn(async move {
            let Some(_permit) = admit(gate, &inner.cancel).await else {
                reply.error(Errno::ENODEV);
                return;
            };
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
                    .working(&file.view.scope, &file.view.id)?
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
        let gate = self.inner.writes.clone();
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
            let Some(_permit) = admit(gate, &inner.cancel).await else {
                reply.error(Errno::ENODEV);
                return;
            };
            let _admission = admission;
            let result = async {
                if let Some(file) = opened {
                    if file.view.inode != ino.0 || file.flags & libc::O_ACCMODE == libc::O_RDONLY {
                        return Err(Errno::EBADF);
                    }
                    let working = writer
                        .working(&file.view.scope, &file.view.id)?
                        .ok_or(Errno::EIO)?;
                    let record = writer.truncate(working.id, size).await?;
                    return Ok(inner.attr(&file.view, &record.node));
                }
                let view = path_view.ok_or(Errno::EIO)?;
                if writer.is_unlinked(&view.scope, &view.id)? {
                    return Err(Errno::ENOENT);
                }
                let _lease = writer.lease(&view.scope, &view.id, &inner.cancel).await?;
                let mut view = view;
                view.node = Some(Arc::new(inner.node(&view).await.map_err(|e| errno(&e))?));
                inner.refuse_within_package(&view)?;
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
        let gate = self.inner.pending.clone();
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
            let Some(_permit) = admit(gate, &inner.cancel).await else {
                reply.error(Errno::ENODEV);
                return;
            };
            let _admission = admission;
            let result = async {
                let lease = match &inner.writeback {
                    Some(writer) => Some(writer.lease(&view.scope, &view.id, &inner.cancel).await?),
                    None => None,
                };
                let node = inner.node(&view).await.map_err(|e| errno(&e))?;
                if node.kind != NodeKind::File {
                    return Err(Errno::EISDIR);
                }
                if let Some(writer) = &inner.writeback
                    && writer.is_unlinked(&view.scope, &view.id)?
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
                    view.node = Some(Arc::new(node.clone()));
                    inner.refuse_within_package(&view)?;
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
        let gate = self.inner.admitted_reads.clone();
        let inner = self.inner.clone();
        self.inner.runtime.spawn(async move {
            let Some(_admission) = admit(gate, &inner.cancel).await else {
                reply.error(Errno::ENODEV);
                return;
            };
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
        let gate = self.inner.pending.clone();
        let inner = self.inner.clone();
        let parent = match inner.view(inode.0) {
            Ok(view) => view,
            Err(error) => {
                reply.error(errno(&error));
                return;
            }
        };
        self.inner.runtime.spawn(async move {
            let Some(_permit) = admit(gate, &inner.cancel).await else {
                reply.error(Errno::ENODEV);
                return;
            };
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
        let gate = self.inner.pending.clone();
        let cancel = self.inner.cancel.clone();
        self.inner.runtime.spawn(async move {
            let ready = tokio::select! {
                biased;
                _ = cancel.cancelled() => Err(Errno::ENODEV),
                result = entries.snapshot.ready(offset) => result.map_err(Errno::from),
            };
            if let Err(error) = ready {
                reply.error(error);
                return;
            }
            // Wait for a place before occupying a blocking thread, not on one.
            let Some(permit) = admit(gate, &cancel).await else {
                reply.error(Errno::ENODEV);
                return;
            };
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
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
            })
            .await
            .ok();
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
