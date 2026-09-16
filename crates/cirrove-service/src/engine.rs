//! Per-account metadata service. Change feeds and foreground directory requests
//! share a provider client but never hold SQLite locks across network awaits.
mod changes;

/// An item's mount-relative path, by walking parents up to the drive root.
///
/// `None` rather than a partial answer: an item whose chain of parents is not
/// fully indexed, or which belongs to a collection this account does not root,
/// has no honest mount-relative path, and half a path points at a real file
/// that is not the one in question. The depth bound is a guard against a cycle
/// in the parent chain, which no correct index has and which would otherwise
/// hang the status call that every client polls.
fn relative_path(
    store: &Store,
    scope: &cirrove_core::Scope,
    root: &str,
    item: &str,
) -> Option<String> {
    const DEEPER_THAN_ANY_REAL_DRIVE: usize = 128;
    let mut parts: Vec<String> = Vec::new();
    let mut id = item.to_string();
    for _ in 0..DEEPER_THAN_ANY_REAL_DRIVE {
        if id == root {
            parts.reverse();
            return Some(parts.join("/"));
        }
        let node = store.node(scope, &id).ok().flatten()?;
        // A node with no parent is a drive root, and `root` names only one of
        // them. An account can subscribe to more than one drive -- this one has
        // two -- and every item in the others walked up to a parentless node
        // that did not match, and resolved to no path at all. What the owner
        // saw on 2026-09-16 was `cirrove pins` naming a file
        // `01YQR2QYPXJNXJZ7LXENA2KXH77S2VBEFB` instead of the PDF they had just
        // kept offline; refused changes and failed saves in that drive were
        // just as nameless. The root's own name is not part of the path.
        let Some(parent) = node.parent_id else {
            parts.reverse();
            return Some(parts.join("/"));
        };
        parts.push(node.name);
        id = parent;
    }
    None
}

#[cfg(test)]
mod deadlines;
#[cfg(test)]
mod discovery;
#[cfg(test)]
mod paths;
#[cfg(test)]
mod persistence;
#[cfg(test)]
mod pinning;
use crate::{accounts::Account, content::ContentCache, private_dir, refresh};
use anyhow::Result;
pub use changes::ChangeNotifications;
use cirrove_core::notifications::{ChangeHint, ChangeHintSender, NotificationState, WatchEnd};
use cirrove_core::{CancellationToken, Node, ProviderError, ReadProvider, Scope};
use cirrove_store::Store;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::{
        Arc, Mutex as StdMutex, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Mutex, Notify, RwLock, watch};
use tokio_util::task::TaskTracker;

/// Discovery shares the store with every feed, so a transient store failure must
/// recover on its own schedule rather than on the next successful poll.
/// A single-item fetch on the filesystem path must not wait indefinitely for an
/// adapter. The directory path already bounds itself at sixty seconds.
const SINGLE_ITEM_TIMEOUT: Duration = Duration::from_secs(60);
const DISCOVERY_RETRY: Duration = Duration::from_secs(1);
const DISCOVERY_RETRY_LIMIT: Duration = Duration::from_secs(60);
/// What one pin reserved and what it has actually kept.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PinStatus {
    pub item: String,
    /// Where the item is in the drive, mount-relative, so a person can be shown
    /// what is kept instead of a provider id. `None` when the chain of parents
    /// is not fully indexed or does not reach this account's root -- a path that
    /// cannot be completed would be a wrong path, not a shorter one, and the
    /// caller should fall back to the id rather than print half of one.
    #[serde(default)]
    pub path: Option<String>,
    pub recursive: bool,
    /// Claimed from the cache budget when the pin was made.
    pub reserved: u64,
    /// Bytes of this pin's blocks present in the cache right now.
    pub resident: u64,
    /// How many blocks the pin owns, so a caller can tell "nothing fetched yet"
    /// from "nothing to fetch".
    pub blocks: u64,
}

/// How much of the cache pinning has claimed, and how close that is to the
/// point where the next pin is refused.
///
/// Reported because "clear free-space behaviour" is a claim about what a user
/// can find out before they are refused, not only about the refusal. A caller
/// who can see the budget filling can act; one who learns about it from an
/// error has already been stopped.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PinBudget {
    /// The account's whole cache allowance.
    pub cache_bytes: u64,
    /// The most pinning may claim of it. Reservations may not take the whole
    /// budget, because a cache with no unreserved room would evict each block an
    /// unpinned read had just fetched.
    pub pinnable_bytes: u64,
    /// Claimed by pins right now.
    pub reserved_bytes: u64,
    /// What a new pin could still take.
    pub free_bytes: u64,
}
impl PinBudget {
    /// Tenths of a percent, so a caller can compare without floating point.
    #[must_use]
    pub fn used_per_mille(&self) -> u64 {
        if self.pinnable_bytes == 0 {
            return 1000;
        }
        (self.reserved_bytes.saturating_mul(1000) / self.pinnable_bytes).min(1000)
    }
    /// A sentence for a person, not a status code.
    #[must_use]
    pub fn explain(&self) -> String {
        let mib = |b: u64| b as f64 / (1024.0 * 1024.0);
        if self.free_bytes == 0 {
            return format!(
                "Pinning has claimed all {:.0} MiB it may use of a {:.0} MiB cache. \
                 Unpin something, or raise cache_bytes for this account, before pinning more.",
                mib(self.pinnable_bytes),
                mib(self.cache_bytes)
            );
        }
        format!(
            "Pinning holds {:.0} of {:.0} MiB it may use ({}.{}%), leaving {:.0} MiB. \
             The rest of the {:.0} MiB cache stays available for ordinary reads.",
            mib(self.reserved_bytes),
            mib(self.pinnable_bytes),
            self.used_per_mille() / 10,
            self.used_per_mille() % 10,
            mib(self.free_bytes),
            mib(self.cache_bytes)
        )
    }
}
/// Whether a feed changing state is worth a line in the log, and which line.
///
/// The daemon used to write nothing at all when a provider refused its
/// authorization. The state reached a user through the tray and `cirrove
/// status`, which is what the acceptance row asks for -- and an operator reading
/// the journal saw a healthy daemon for the seventeen minutes a real grant was
/// withdrawn, with nothing saying what had been refused or why. The one place a
/// person looks when something is wrong was the one place that stayed silent.
///
/// On transitions only. A refused feed retries every sixty seconds, and a line
/// per retry would bury the one that matters under the ones that do not.
/// `indexing` is the state a feed starts in, so reaching it first is not a
/// failure to announce.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FeedNotice {
    /// It stopped working, and this is the first tick that says so.
    Failed,
    /// It works again after having stopped.
    Recovered,
    Nothing,
}

pub(crate) fn feed_notice(before: &str, after: &str) -> FeedNotice {
    if before == after {
        return FeedNotice::Nothing;
    }
    match (before, after) {
        (_, "ready") if before != "indexing" => FeedNotice::Recovered,
        (_, "ready") => FeedNotice::Nothing,
        // Indexing is work in progress rather than a fault, and a feed passes
        // through it on every reset. Announcing it would make the log noisy in
        // exactly the situation where it needs to be readable.
        (_, "indexing") => FeedNotice::Nothing,
        _ => FeedNotice::Failed,
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FeedHealth {
    pub collection: String,
    pub state: String,
    pub last_success: Option<u64>,
    pub retry_at: Option<u64>,
    pub message: Option<String>,
    #[serde(default)]
    pub notifications: NotificationState,
}
struct Feed {
    cancel: CancellationToken,
    hints: watch::Receiver<ChangeHint>,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
/// A folder pin that has been walked and reserved, with the fetching left.
struct PlannedPin {
    files: Vec<Node>,
    bytes: u64,
    complete: bool,
}
/// What a fetch managed to keep, and whether it was stopped rather than done.
struct KeptOffline {
    blocks: usize,
    stopped: bool,
}
pub struct Engine {
    pub account: Account,
    pub db: PathBuf,
    pub provider: Arc<dyn ReadProvider>,
    pub cache: ContentCache,
    pub cancel: CancellationToken,
    pub changed: ChangeNotifications,
    feeds: RwLock<HashMap<String, Feed>>,
    health: RwLock<HashMap<String, FeedHealth>>,
    directories: StdMutex<HashMap<String, Weak<Mutex<()>>>>,
    tasks: TaskTracker,
    directory_publications: Arc<tokio::sync::Semaphore>,
    discovery: Notify,
    discovery_started: AtomicBool,
    discovery_failures: AtomicU64,
    activity: crate::activity::DirectoryActivity,
    /// Survives a remount, unlike the write workers that record into it: a user
    /// asking why their saves failed should not have the answer erased by the
    /// remount that failing saves can themselves provoke.
    pub save_refusals: Arc<crate::journal::SaveRefusals>,
    /// What the delta feed delivered lately, for a window and a tray.
    pub recent: crate::recent::RecentChanges,
    /// Long work somebody asked for and can watch or stop. See [`crate::jobs`].
    pub jobs: Arc<crate::jobs::Jobs>,
    /// Bumped whenever what this account keeps offline changes: a pin made or
    /// released, or a file of one arriving.
    ///
    /// A level, not a log, and it exists for the file manager. Files asks the
    /// daemon about a path when it lists a directory and then keeps the answer,
    /// so a pin made in the window left a stale badge sitting in an open window
    /// until something else made Files re-list. A counter on the event channel
    /// is the smallest thing that can say "ask again" without saying what
    /// changed -- which would be a path, on a channel that carries none.
    ///
    /// Bumped per file rather than per job on purpose: the status vector is
    /// rebuilt every five seconds, so a fetch of three hundred files produces
    /// one event per sample rather than three hundred. That coalescing is the
    /// channel's whole design (ADR 0007).
    kept_generation: AtomicU64,
    /// One connection stays open for the account's lifetime. Without it every
    /// `Store::open` is both the first and the last connection to a WAL
    /// database, so SQLite creates `metadata.db-wal` and `-shm` on open and, on
    /// close, checkpoints and unlinks them. That is durable filesystem work
    /// inside every cached read, on the filesystem the content cache is
    /// saturating. It is never used for queries; it only keeps the WAL index
    /// alive, and it holds no transaction, so checkpointing stays normal.
    _keeper: StdMutex<Store>,
    _owner: std::fs::File,
}
impl Engine {
    pub async fn new(
        account: Account,
        provider: Arc<dyn ReadProvider>,
        state: PathBuf,
    ) -> Result<Arc<Self>> {
        let directory = state.join("accounts").join(&account.id);
        private_dir(&directory)?;
        let owner = crate::accounts::account_lock(&directory)?;
        let db = directory.join("metadata.db");
        let path = db.clone();
        let keeper = tokio::task::spawn_blocking(move || Store::open(path)).await??;
        let blocks = directory.join("blocks.db");
        let quota = account.cache_bytes;
        // Read pins before the cache exists. Its reconcile pass evicts to fit the
        // quota at construction, long before start() could publish anything, so a
        // cache built without them would delete pinned blocks at every mount
        // while the registry went on saying they were kept.
        let pins = db.clone();
        let reservations = tokio::task::spawn_blocking(
            move || -> cirrove_store::Result<crate::content::Reservations> {
                let store = Store::open(pins)?;
                Ok(crate::content::Reservations {
                    protected: store.protected_blocks()?,
                    reserved: store.reserved_bytes()?,
                })
            },
        )
        .await??;
        let cache = tokio::task::spawn_blocking(move || {
            ContentCache::new_reserving(directory.join("cache"), blocks, quota, reservations)
        })
        .await??;
        Ok(Arc::new(Self {
            account,
            db,
            provider,
            cache,
            cancel: CancellationToken::new(),
            changed: ChangeNotifications::default(),
            feeds: RwLock::new(HashMap::new()),
            health: RwLock::new(HashMap::new()),
            directories: StdMutex::new(HashMap::new()),
            tasks: TaskTracker::new(),
            directory_publications: Arc::new(tokio::sync::Semaphore::new(2)),
            discovery: Notify::new(),
            discovery_started: AtomicBool::new(false),
            discovery_failures: AtomicU64::new(0),
            activity: crate::activity::DirectoryActivity::default(),
            save_refusals: Arc::new(crate::journal::SaveRefusals::default()),
            recent: crate::recent::RecentChanges::default(),
            jobs: Arc::new(crate::jobs::Jobs::default()),
            kept_generation: AtomicU64::new(0),
            _keeper: StdMutex::new(keeper),
            _owner: owner,
        }))
    }
    /// What is kept offline, as a number that changes when it does.
    pub fn kept_generation(&self) -> u64 {
        self.kept_generation.load(Ordering::Relaxed)
    }
    fn kept_changed(&self) {
        self.kept_generation.fetch_add(1, Ordering::Relaxed);
    }
    pub fn scope(&self, collection: &str) -> Scope {
        Scope {
            account: self.account.id.clone(),
            provider: self.provider.provider_id().into(),
            collection: collection.into(),
        }
    }
    /// Publish what pinning currently claims to the cache.
    ///
    /// The cache holds no database handle, so the two are kept in step here
    /// rather than by eviction reading the store on every pass. Called at start
    /// and after every pin change; a mount that skipped it would evict pinned
    /// content while the registry still said it was kept.
    pub async fn refresh_reservations(&self) -> Result<()> {
        let db = self.db.clone();
        let (reserved, protected) = tokio::task::spawn_blocking(
            move || -> cirrove_store::Result<(u64, std::collections::HashSet<String>)> {
                let store = Store::open(db)?;
                Ok((store.reserved_bytes()?, store.protected_blocks()?))
            },
        )
        .await??;
        if let Ok(mut view) = self.cache.reservations().lock() {
            view.reserved = reserved;
            view.protected = protected;
        }
        Ok(())
    }
    /// How much of the cache budget pins may claim.
    ///
    /// Not all of it. A reservation covering the whole quota leaves nothing for
    /// ordinary reading: every block an unpinned file needs would be the block
    /// eviction has to take next, so the mount would fetch and discard the same
    /// bytes forever while appearing to work. The headroom is a tenth of the
    /// budget, and never fewer than eight blocks, so a small cache keeps enough
    /// to stream through rather than a tenth of very little.
    pub fn pinnable_budget(&self) -> u64 {
        let headroom = (self.account.cache_bytes / 10)
            .max(8 * crate::content::BLOCK_SIZE as u64)
            .min(self.account.cache_bytes);
        self.account.cache_bytes.saturating_sub(headroom)
    }
    /// Record a pin and put it into effect. Refusals are returned, not raised:
    /// a budget that cannot hold the request is an answer for the caller, not a
    /// fault of the engine.
    pub async fn pin(
        &self,
        scope: Scope,
        item: String,
        recursive: bool,
        reserved: u64,
    ) -> Result<std::result::Result<cirrove_store::pins::Pin, cirrove_store::pins::PinRefusal>>
    {
        let db = self.db.clone();
        let budget = self.pinnable_budget();
        let key = serde_json::to_string(&scope).unwrap_or_default();
        let outcome = tokio::task::spawn_blocking(move || {
            Store::open(db)?.pin(&key, &item, recursive, reserved, budget)
        })
        .await??;
        if outcome.is_ok() {
            self.refresh_reservations().await?;
            self.kept_changed();
        }
        Ok(outcome)
    }
    /// Fetch a pinned file's content and protect the blocks it occupies.
    ///
    /// Pinning without this is an accounting entry: the space is reserved and
    /// nothing is kept, so the first offline read still fails. The blocks are
    /// named with the cache's own key derivation, because a protected set built
    /// any other way would cover keys nothing ever writes and would be
    /// indistinguishable from no protection at all.
    ///
    /// Content revisions change block keys, so this is also what a pin needs
    /// after the file changes remotely.
    pub async fn materialise_pin(&self, scope: &Scope, node: &Node) -> Result<usize> {
        // A file with no content version has no block keys to bind to, and a
        // pin bound to nothing looks exactly like one that works.
        anyhow::Context::context(
            crate::content::block_keys(scope, node),
            "pinned file has no content version to bind its blocks to",
        )?;
        Ok(self
            .keep_files_offline(scope, &node.id, std::slice::from_ref(node), None)
            .await?
            .blocks)
    }
    /// Fetch every block of every file listed, and protect what was kept.
    ///
    /// The one place content is fetched because somebody asked for it rather
    /// than because something read it, and therefore the one place with progress
    /// worth reporting: `progress` is the job a person is watching, when there
    /// is one. It ends early when that job is stopped.
    ///
    /// A fetch that fails part way still protects what it managed to keep. Three
    /// hundred files of three hundred and forty are three hundred files a person
    /// can open on a train, and blocks nothing protects are blocks eviction
    /// takes at the next download.
    async fn keep_files_offline(
        &self,
        scope: &Scope,
        item: &str,
        files: &[Node],
        progress: Option<&crate::jobs::JobHandle>,
    ) -> Result<KeptOffline> {
        let cancel = progress.map_or(&self.cancel, |job| &job.cancel);
        let mut keys: Vec<String> = Vec::new();
        let (mut files_done, mut bytes_done) = (0u64, 0u64);
        let mut failure = None;
        'files: for file in files {
            let mut start = 0;
            while start < file.size {
                if cancel.is_cancelled() {
                    break 'files;
                }
                let length = (file.size - start).min(crate::content::BLOCK_SIZE as u64) as u32;
                if let Err(error) = self
                    .cache
                    .read(self.provider.as_ref(), scope, file, start, length, cancel)
                    .await
                {
                    failure = Some(error);
                    break 'files;
                }
                start += crate::content::BLOCK_SIZE as u64;
                bytes_done += u64::from(length);
                if let Some(job) = progress {
                    job.advance(files_done, bytes_done);
                }
            }
            files_done += 1;
            if let Some(file_keys) = crate::content::block_keys(scope, file) {
                keys.extend(file_keys);
            }
            if let Some(job) = progress {
                job.advance(files_done, bytes_done);
            }
            // A file that has arrived is a badge that has changed. Coalesced by
            // the status loop, so a folder of three hundred costs one event per
            // sample rather than three hundred.
            self.kept_changed();
        }
        // Stopping means the pin goes with the job, so there is nothing to
        // protect and nothing to publish: the caller releases it.
        let stopped = cancel.is_cancelled();
        if stopped {
            return Ok(KeptOffline {
                blocks: keys.len(),
                stopped,
            });
        }
        let db = self.db.clone();
        let key = serde_json::to_string(scope).unwrap_or_default();
        let owned = keys.clone();
        let item = item.to_owned();
        tokio::task::spawn_blocking(move || Store::open(db)?.protect_blocks(&key, &item, &owned))
            .await??;
        // Published only after the blocks exist. Protecting keys before their
        // content is fetched would shrink what eviction may take while the cache
        // still has to make room for the fetch itself.
        self.refresh_reservations().await?;
        match failure {
            Some(error) => Err(error.into()),
            None => Ok(KeptOffline {
                blocks: keys.len(),
                stopped,
            }),
        }
    }
    /// Start keeping files offline as a job, and hand back its id.
    ///
    /// The fetching half of a pin. It is spawned rather than awaited because a
    /// control request answers in one exchange and the client half gives up
    /// after three seconds: a folder that took longer than that used to report
    /// `Cirrove pin timed out` to the person who asked for it while the daemon
    /// went on keeping every file, which is a wrong answer about work that
    /// succeeded.
    async fn keep_offline_job(
        self: &Arc<Self>,
        scope: &Scope,
        root: &Node,
        files: Vec<Node>,
        bytes: u64,
    ) -> String {
        // Named by where it sits in the drive. The item id is what the daemon
        // acts on and it is not something a person can recognise.
        let name = self
            .relative_path_of(scope, &root.id)
            .await
            .unwrap_or_else(|| root.name.clone());
        let handle = self.jobs.start(
            crate::jobs::JobKind::KeepOffline,
            name,
            files.len() as u64,
            bytes,
            &self.cancel,
        );
        let id = handle.id().to_owned();
        let engine = self.clone();
        let scope = scope.clone();
        let item = root.id.clone();
        self.tasks.spawn(async move {
            let outcome = engine
                .keep_files_offline(&scope, &item, &files, Some(&handle))
                .await;
            match outcome {
                Ok(kept) if kept.stopped => {
                    // Only a stop somebody asked for releases the pin. The same
                    // token is cancelled when the account stops, and unpinning
                    // there would quietly throw away what a user chose to keep
                    // every time their machine shut down.
                    if !handle.asked_to_stop() {
                        return;
                    }
                    // A person who stopped a fetch did not ask to keep half a
                    // folder, and a pin reserving the whole of it while holding
                    // part of it misreports both.
                    if let Err(error) = engine.unpin(scope, item).await {
                        tracing::warn!("stopped keeping offline, but the pin remains: {error}");
                    }
                    let _ = engine.cache.reclaim().await;
                    handle.failed(crate::jobs::JobState::Stopped, None);
                }
                Ok(_) => handle.finished(),
                Err(error) => {
                    handle.failed(crate::jobs::JobState::Failed, Some(error.to_string()));
                }
            }
        });
        id
    }
    /// Every file at or below `root`, with the total bytes they occupy.
    ///
    /// Reads the cached directory views rather than the provider: a recursive pin
    /// is a decision about what is already known to be there, and walking the
    /// provider would make pinning a large folder an expensive remote traversal
    /// before it has kept a single byte. A subtree that is not indexed yet is
    /// reported as what is known, so the caller can see the difference rather
    /// than being handed a total that quietly excluded it.
    pub async fn subtree_files(&self, scope: &Scope, root: &str) -> Result<(Vec<Node>, u64, bool)> {
        let db = self.db.clone();
        let scope = scope.clone();
        let root = root.to_string();
        Ok(
            tokio::task::spawn_blocking(
                move || -> cirrove_store::Result<(Vec<Node>, u64, bool)> {
                    let store = Store::open(db)?;
                    let mut files = Vec::new();
                    let mut bytes = 0u64;
                    let mut complete = true;
                    let mut pending = vec![root];
                    // Depth-first with an explicit stack. A folder tree deep enough to
                    // overflow a recursive walk is a folder tree a user can make.
                    while let Some(parent) = pending.pop() {
                        match store.children(&scope, &parent)? {
                            Some(children) => {
                                for child in children {
                                    match child.kind {
                                        cirrove_core::NodeKind::Folder => pending.push(child.id),
                                        _ => {
                                            bytes = bytes.saturating_add(child.size);
                                            files.push(child);
                                        }
                                    }
                                }
                            }
                            None => complete = false,
                        }
                    }
                    Ok((files, bytes, complete))
                },
            )
            .await??,
        )
    }
    /// Pin a folder and everything under it.
    ///
    /// Returns the refusal untouched when the subtree does not fit, before any
    /// content is fetched: a partial download that is then refused would have
    /// spent the bandwidth and kept nothing.
    pub async fn pin_folder(
        &self,
        scope: &Scope,
        root: &Node,
    ) -> Result<std::result::Result<(usize, bool), cirrove_store::pins::PinRefusal>> {
        let planned = match self.plan_folder_pin(scope, root).await? {
            Ok(planned) => planned,
            Err(refusal) => return Ok(Err(refusal)),
        };
        self.keep_files_offline(scope, &root.id, &planned.files, None)
            .await?;
        Ok(Ok((planned.files.len(), planned.complete)))
    }
    /// Walk the subtree and reserve what it needs, without fetching anything.
    ///
    /// The half of a folder pin that belongs inside a request: it reads the
    /// local index and writes one row, so it answers in milliseconds and it is
    /// where every refusal a caller can act on comes from. The fetching half is
    /// minutes of network and belongs to a job.
    async fn plan_folder_pin(
        &self,
        scope: &Scope,
        root: &Node,
    ) -> Result<std::result::Result<PlannedPin, cirrove_store::pins::PinRefusal>> {
        let (files, logical, complete) = self.subtree_files(scope, &root.id).await?;
        // Same correction as a single file, per file in the walk: the walk sums
        // logical sizes and the cache stores a digest with every block.
        let bytes: u64 = files
            .iter()
            .map(|f| crate::content::stored_bytes(f.size))
            .sum::<u64>()
            .max(logical);
        if let Err(refusal) = self
            .pin(scope.clone(), root.id.clone(), true, bytes)
            .await?
        {
            return Ok(Err(refusal));
        }
        Ok(Ok(PlannedPin {
            files,
            bytes,
            complete,
        }))
    }
    /// What pinning has claimed of the cache and what is left.
    pub async fn pin_budget(&self) -> Result<PinBudget> {
        let db = self.db.clone();
        let reserved =
            tokio::task::spawn_blocking(move || Store::open(db)?.reserved_bytes()).await??;
        let pinnable = self.pinnable_budget();
        Ok(PinBudget {
            cache_bytes: self.account.cache_bytes,
            pinnable_bytes: pinnable,
            reserved_bytes: reserved,
            free_bytes: pinnable.saturating_sub(reserved),
        })
    }
    /// What each pin has actually kept, as against what it reserved.
    ///
    /// Reserved and resident are reported separately on purpose. A pin whose
    /// content was never fetched reserves space and keeps nothing, and a status
    /// that showed only the reservation would report content as available that
    /// no offline read could produce.
    /// Where an item sits in the drive, as a mount-relative path.
    ///
    /// The same walk the pin listing does, exposed because the stuck-change
    /// listing needs it too: the writeback layer knows an item id and nothing
    /// else, and this is the only place with an index to turn one into a path
    /// a person recognises.
    pub async fn relative_path_of(&self, scope: &Scope, item: &str) -> Option<String> {
        let db = self.db.clone();
        let root = self.account.root_id.clone();
        let scope = scope.clone();
        let item = item.to_owned();
        tokio::task::spawn_blocking(move || {
            let store = Store::open(db).ok()?;
            relative_path(&store, &scope, &root, &item)
        })
        .await
        .ok()
        .flatten()
    }
    pub async fn pin_status(&self) -> Result<Vec<PinStatus>> {
        let db = self.db.clone();
        let blocks = self.blocks_path();
        let cache = self.cache_path();
        let root = self.account.root_id.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<PinStatus>> {
            let store = Store::open(db)?;
            let index = cirrove_store::BlockIndex::open(&blocks)?;
            let sizes: std::collections::HashMap<String, u64> =
                index.oldest()?.into_iter().collect();
            let mut out = Vec::new();
            for pin in store.pins()? {
                let keys = store.blocks_of(&pin.scope, &pin.item)?;
                // Present on disk, not merely indexed: the index is rebuilt from
                // the directory, but between a block being forgotten and the
                // rebuild it would otherwise be counted as available.
                let resident = keys
                    .iter()
                    .filter(|key| cache.join(key).exists())
                    .filter_map(|key| sizes.get(key))
                    .sum();
                let path = serde_json::from_str::<cirrove_core::Scope>(&pin.scope)
                    .ok()
                    .and_then(|scope| relative_path(&store, &scope, &root, &pin.item));
                out.push(PinStatus {
                    item: pin.item,
                    path,
                    recursive: pin.recursive,
                    reserved: pin.reserved,
                    resident,
                    blocks: keys.len() as u64,
                });
            }
            Ok(out)
        })
        .await?
    }
    pub(crate) fn blocks_path(&self) -> PathBuf {
        self.db.with_file_name("blocks.db")
    }

    /// Where published blocks live. Exposed so a caller reasoning about cache
    /// files derives the path from here rather than rebuilding it and drifting.
    ///
    /// Public because an acceptance test has to be able to weigh the directory:
    /// "unpinning frees the bytes" is a claim about a filesystem, and rebuilding
    /// the path in the test would let the two drift apart silently.
    pub fn cache_path(&self) -> PathBuf {
        self.db.with_file_name("cache")
    }
    /// The state of mount-relative paths, for a file manager drawing badges:
    /// what each is, whether a pin covers it -- its own, or a recursive one on
    /// a folder above -- and how much of a file's content is on disk. One
    /// exchange for a whole listing; the pins are read once for all of them.
    /// A path that does not resolve gets a refusal of its own rather than
    /// failing the rest: a listing with one broken entry is still a listing.
    /// Remove these paths without passing through the provider's recycle bin.
    ///
    /// Only ever reached because a person asked for it a second time, in words:
    /// POSIX has one `unlink` and no flag in which "and skip the recycle bin"
    /// could live (ADR 0008).
    ///
    /// **A folder is refused, and that is a decision rather than an omission.**
    /// Graph's delete on a folder is recursive, and a folder's eTag does not
    /// move when a child is added -- measured, `folder_etag_and_mtime_ignore_
    /// their_children` -- so nothing available over Graph can tell whether a
    /// child arrived between the check and the delete. The recycle bin is the
    /// only recovery from that, and a permanent delete is precisely the thing
    /// that removes it. One file at a time can be looked at; a subtree cannot.
    /// The name of a wastebasket sitting in the drive root, if one is there.
    ///
    /// The mount refuses to create one and refuses renames into one, so nothing
    /// can put anything in it any more. What it cannot do is remove one that
    /// arrived before the guard existed -- a file manager made a real
    /// `.Trash-1000/` in this owner's live drive on 2026-09-11 -- and removing
    /// somebody's folder is not a mount's decision to take. Telling them it is
    /// there was the part that was missing (ADR 0008, "what is not closed").
    pub async fn wastebasket(self: &Arc<Self>) -> Option<String> {
        let scope = self.scope(&self.account.drive.id);
        let root = self.account.root_id.clone();
        let children = self.children(&scope, &root).await.ok()?;
        children
            .into_iter()
            .map(|node| node.name)
            .find(|name| crate::filesystem::is_trash_directory(name))
    }

    ///
    /// The provider is passed in rather than read from the engine: the engine
    /// holds a read provider, and the index that turns a path into an item.
    /// Writing belongs to the write side. This is the one place they meet.
    pub async fn delete_permanently(
        self: &Arc<Self>,
        paths: &[String],
        provider: &dyn cirrove_core::mutation::MutationProvider,
    ) -> Vec<crate::PermanentDeletion> {
        let support = provider.deletion();
        let mut done = Vec::with_capacity(paths.len());
        for path in paths {
            let refuse = |why: &str| crate::PermanentDeletion {
                path: path.clone(),
                removed: false,
                refusal: Some(why.to_owned()),
            };
            if !support.permanent {
                done.push(refuse(
                    "this provider has no permanent deletion, and an ordinary delete \
                     must not be substituted for one that was asked for by name",
                ));
                continue;
            }
            let request = crate::PinRequest {
                path: Some(path.clone()),
                ..Default::default()
            };
            let (scope, node) = match self.resolve_request(&request).await {
                Ok(resolved) => resolved,
                Err(error) => {
                    done.push(refuse(&error.to_string()));
                    continue;
                }
            };
            if node.kind == cirrove_core::NodeKind::Folder {
                done.push(refuse(
                    "a folder cannot be deleted permanently: the provider's delete is \
                     recursive and nothing can tell whether a child arrived a moment \
                     ago, so the recycle bin is the only recovery -- and this is the \
                     one operation that removes it",
                ));
                continue;
            }
            let outcome = provider
                .delete_permanently(&scope, &node.id, node.etag.as_deref(), &self.cancel)
                .await;
            match outcome {
                Ok(()) => {
                    // The delta feed reports the removal, and the folder it was
                    // in is marked active so the feed is asked sooner rather
                    // than at its own pace: a person who has just destroyed
                    // something should not watch it linger in the file manager.
                    if let Some(parent) = node.parent_id.as_deref() {
                        self.activity.touch(&scope, parent);
                    }
                    done.push(crate::PermanentDeletion {
                        path: path.clone(),
                        removed: true,
                        refusal: None,
                    });
                }
                Err(error) => done.push(refuse(&error.to_string())),
            }
        }
        done
    }
    pub async fn path_states(self: &Arc<Self>, paths: &[String]) -> Result<Vec<crate::PathState>> {
        let db = self.db.clone();
        let pins = tokio::task::spawn_blocking(move || Store::open(db)?.pins()).await??;
        let cache = self.cache_path();
        let mut states = Vec::with_capacity(paths.len());
        for path in paths {
            let request = crate::PinRequest {
                path: Some(path.clone()),
                ..Default::default()
            };
            let (scope, node) = match self.resolve_request(&request).await {
                Ok(resolved) => resolved,
                Err(error) => {
                    states.push(crate::PathState {
                        path: path.clone(),
                        refusal: Some(error.to_string()),
                        ..Default::default()
                    });
                    continue;
                }
            };
            let folder = node.kind == cirrove_core::NodeKind::Folder;
            let pinned = self.pin_covering(&scope, &node, &pins).await;
            let resident = if folder {
                0
            } else {
                resident_bytes(&cache, &scope, &node)
            };
            states.push(crate::PathState {
                path: path.clone(),
                item: node.id.clone(),
                kind: if folder { "folder" } else { "file" }.into(),
                pinned,
                size: node.size,
                resident,
                refusal: None,
            });
        }
        Ok(states)
    }
    /// "direct" for the item's own pin, "inherited" for a recursive pin on a
    /// folder above it, nothing otherwise. Walks up through the index only when
    /// a recursive pin exists to be found.
    async fn pin_covering(
        &self,
        scope: &Scope,
        node: &Node,
        pins: &[cirrove_store::pins::Pin],
    ) -> Option<String> {
        let key = serde_json::to_string(scope).unwrap_or_default();
        if pins.iter().any(|p| p.scope == key && p.item == node.id) {
            return Some("direct".into());
        }
        let recursive: Vec<&str> = pins
            .iter()
            .filter(|p| p.scope == key && p.recursive)
            .map(|p| p.item.as_str())
            .collect();
        if recursive.is_empty() {
            return None;
        }
        let mut parent = node.parent_id.clone();
        // Bounded: a cycle in the index must not hang a badge.
        for _ in 0..256 {
            let id = parent?;
            if recursive.contains(&id.as_str()) {
                return Some("inherited".into());
            }
            parent = self.node(scope, &id).await.ok()?.parent_id;
        }
        None
    }
    /// Release a pin and the space it held. Reports whether one existed.
    pub async fn unpin(&self, scope: Scope, item: String) -> Result<bool> {
        let db = self.db.clone();
        let key = serde_json::to_string(&scope).unwrap_or_default();
        let removed =
            tokio::task::spawn_blocking(move || Store::open(db)?.unpin(&key, &item)).await??;
        self.refresh_reservations().await?;
        if removed {
            self.kept_changed();
        }
        Ok(removed)
    }
    /// Resolve, reserve and materialise a pin asked for over the control socket.
    ///
    /// This is the path `materialise_pin` and `pin_folder` never had. The command
    /// line used to write the reservation into the index itself, which is why a
    /// pin a user made kept nothing: only the daemon holds an engine, and only an
    /// engine can fetch.
    pub async fn apply_pin_request(
        self: &Arc<Self>,
        request: &crate::PinRequest,
    ) -> Result<crate::PinReply> {
        let (scope, node) = match self.resolve_request(request).await {
            Ok(resolved) => resolved,
            Err(error) => {
                return Ok(crate::PinReply {
                    refusal: Some(error.to_string()),
                    ..Default::default()
                });
            }
        };
        if request.recursive {
            return Ok(match self.plan_folder_pin(&scope, &node).await? {
                Ok(planned) => {
                    let files = planned.files.len() as u64;
                    let complete = planned.complete;
                    let job = self
                        .keep_offline_job(&scope, &node, planned.files, planned.bytes)
                        .await;
                    crate::PinReply {
                        accepted: true,
                        reserved: self.reserved_for(&node.id).await.unwrap_or(0),
                        item: node.id,
                        files,
                        complete,
                        job: Some(job),
                        refusal: None,
                    }
                }
                Err(refusal) => crate::PinReply {
                    item: node.id,
                    refusal: Some(refusal.to_string()),
                    ..Default::default()
                },
            });
        }
        // A folder has no content of its own to keep; what a pin on it can mean
        // is everything beneath it, and that is a choice the caller makes with
        // `recursive`, not one to make for them by charging their budget for a
        // subtree they did not ask about. Refused in words: the first version
        // fell through to materialising a folder, which has no blocks, and the
        // error dropped the connection with no reply at all.
        if node.kind == cirrove_core::NodeKind::Folder {
            return Ok(crate::PinReply {
                item: node.id,
                refusal: Some(
                    "that is a folder; pin it with --recursive to keep every file beneath it"
                        .into(),
                ),
                ..Default::default()
            });
        }
        // A single file reserves what it will actually occupy: the node's size
        // plus one digest per block. An explicit --bytes is taken as given.
        let reserved = request
            .bytes
            .unwrap_or_else(|| crate::content::stored_bytes(node.size));
        match self
            .pin(scope.clone(), node.id.clone(), false, reserved)
            .await?
        {
            Err(refusal) => Ok(crate::PinReply {
                item: node.id,
                refusal: Some(refusal.to_string()),
                ..Default::default()
            }),
            Ok(_) => {
                // A single file is a job too. Most are small and the job is over
                // before anyone looks, but "most" is not a size limit: one file
                // can be a four-gigabyte recording, and the request that keeps it
                // must answer in the same breath as the one that keeps a folder.
                let size = node.size;
                let job = self
                    .keep_offline_job(&scope, &node, vec![node.clone()], size)
                    .await;
                Ok(crate::PinReply {
                    accepted: true,
                    item: node.id,
                    reserved,
                    files: 1,
                    complete: true,
                    job: Some(job),
                    refusal: None,
                })
            }
        }
    }
    /// Release a pin named by item id, from the local registry alone.
    ///
    /// A pin is a local record and releasing one must not depend on the
    /// provider being able to resolve its id. Measured on a real account on
    /// 2026-09-15: a folder inside a linked SharePoint library is pinned under
    /// that library's scope, and unpinning by id looked the id up in the
    /// account's own drive and answered "remote item not found" -- so the
    /// window's Stop keeping button, which acts by id because that is the
    /// handle that survives a rename, could not release such a pin at all. The
    /// same lookup would have failed with the network down, which is exactly
    /// when someone wants their disk space back.
    ///
    /// Returns how many records were released, which is zero when the id names
    /// nothing pinned -- that case still goes through resolution, so the caller
    /// can be told whether the item exists at all.
    async fn release_recorded_pin(&self, item: &str) -> Result<usize> {
        let db = self.db.clone();
        let item = item.to_owned();
        let released = tokio::task::spawn_blocking(move || -> cirrove_store::Result<usize> {
            let mut store = Store::open(db)?;
            let scopes: Vec<String> = store
                .pins()?
                .into_iter()
                .filter(|pin| pin.item == item)
                .map(|pin| pin.scope)
                .collect();
            let mut released = 0;
            for scope in scopes {
                if store.unpin(&scope, &item)? {
                    released += 1;
                }
            }
            Ok(released)
        })
        .await??;
        if released > 0 {
            self.refresh_reservations().await?;
            self.kept_changed();
        }
        Ok(released)
    }
    /// Release a pin and give the space back, rather than only unreserving it.
    pub async fn apply_unpin_request(
        self: &Arc<Self>,
        request: &crate::PinRequest,
    ) -> Result<crate::PinReply> {
        if let Some(item) = &request.item
            && self.release_recorded_pin(item).await? > 0
        {
            // Unreserving is not freeing; the same reclaim the resolved path does.
            self.cache.reclaim().await?;
            return Ok(crate::PinReply {
                accepted: true,
                item: item.clone(),
                complete: true,
                ..Default::default()
            });
        }
        let (scope, node) = match self.resolve_request(request).await {
            Ok(resolved) => resolved,
            Err(error) => {
                return Ok(crate::PinReply {
                    refusal: Some(error.to_string()),
                    ..Default::default()
                });
            }
        };
        let removed = self.unpin(scope, node.id.clone()).await?;
        if !removed {
            return Ok(crate::PinReply {
                item: node.id,
                refusal: Some("that item is not pinned".into()),
                ..Default::default()
            });
        }
        // Unreserving is not freeing. Without this the blocks stay on disk until
        // some unrelated download happens to trigger an eviction pass, which is
        // not what "unpin frees space" means to anyone who typed it.
        self.cache.reclaim().await?;
        Ok(crate::PinReply {
            accepted: true,
            item: node.id,
            complete: true,
            ..Default::default()
        })
    }
    /// What a pin currently reserves, for reporting back what was accepted.
    async fn reserved_for(&self, item: &str) -> Option<u64> {
        self.pin_status()
            .await
            .ok()?
            .into_iter()
            .find(|p| p.item == item)
            .map(|p| p.reserved)
    }
    /// Turn `--path` or `--item` into a scope and a node.
    ///
    /// Path resolution belongs here because only the daemon can list a directory
    /// the index has not reached yet, and because most of this account's content
    /// can live in a linked collection whose scope is not the account's own drive
    /// -- a caller outside the daemon would record the pin under the wrong key.
    async fn resolve_request(
        self: &Arc<Self>,
        request: &crate::PinRequest,
    ) -> Result<(Scope, Node)> {
        let scope = self.scope(&self.account.drive.id);
        if let Some(item) = &request.item {
            let node = self.node(&scope, item).await?;
            return Ok((scope, node));
        }
        let Some(path) = &request.path else {
            return Err(anyhow::anyhow!("name an item with --path or --item"));
        };
        let mut scope = scope;
        let mut node = self.node(&scope, &self.account.root_id).await?;
        for name in path.split('/').filter(|s| !s.is_empty()) {
            // A shortcut leaves this drive: follow it before descending, or the
            // rest of the path is looked up in a collection that does not hold it.
            if let Some(target) = node.target.clone() {
                scope = self.scope(&target.collection);
                node = self.node(&scope, &target.item).await?;
            }
            node = self.child(&scope, &node.id, name).await?;
        }
        if let Some(target) = node.target.clone() {
            scope = self.scope(&target.collection);
            node = self.node(&scope, &target.item).await?;
        }
        Ok((scope, node))
    }
    pub async fn start(self: &Arc<Self>) -> Result<()> {
        // Before anything can evict, so a restart never spends the window
        // between mounting and the first pin change treating pinned blocks as
        // ordinary ones.
        self.refresh_reservations().await?;
        if !self.discovery_started.swap(true, Ordering::SeqCst) {
            let engine = self.clone();
            self.tasks.spawn(async move {
                engine.refresh_active_directories().await;
            });
            let engine = self.clone();
            self.tasks.spawn(async move {
                // A failed discovery must not wait for the next successful feed
                // poll. That interval can be an hour, and a feed that stops
                // succeeding never notifies again, so a linked drive would stay
                // unsubscribed for the whole time.
                let mut retry = Duration::ZERO;
                loop {
                    tokio::select! {biased;
                        _=engine.cancel.cancelled()=>return,
                        _=engine.discovery.notified()=>(),
                        _=tokio::time::sleep(retry), if !retry.is_zero()=>(),
                    }
                    match engine.discover().await {
                        Ok(()) => retry = Duration::ZERO,
                        Err(error) => {
                            engine.discovery_failures.fetch_add(1, Ordering::SeqCst);
                            retry = (retry * 2).clamp(DISCOVERY_RETRY, DISCOVERY_RETRY_LIMIT);
                            tracing::warn!(%error, "linked-drive discovery failed; retrying");
                        }
                    }
                }
            });
        }
        self.ensure_feed(self.account.drive.id.clone(), self.account.root_id.clone())
            .await?;
        let db = self.db.clone();
        let account = self.account.id.clone();
        let subscriptions =
            tokio::task::spawn_blocking(move || Store::open(db)?.subscriptions(&account)).await??;
        for (drive, root) in subscriptions {
            self.ensure_feed(drive, root).await?;
        }
        Ok(())
    }
    pub async fn stop(&self) {
        self.cancel.cancel();
        for feed in self.feeds.read().await.values() {
            feed.cancel.cancel();
        }
        self.tasks.close();
        self.tasks.wait().await;
    }
    pub async fn health(&self) -> Vec<FeedHealth> {
        let mut health: Vec<_> = self.health.read().await.values().cloned().collect();
        let feeds = self.feeds.read().await;
        for entry in &mut health {
            if let Some(feed) = feeds.get(&entry.collection) {
                entry.notifications = feed.hints.borrow().state;
            }
        }
        health
    }
    pub fn directory_freshness(&self) -> crate::DirectoryFreshness {
        self.activity.status()
    }
    async fn refresh_active_directories(self: Arc<Self>) {
        while let Some(job) = self.activity.next(&self.cancel).await {
            let key = match serde_json::to_string(&(&job.scope, &job.parent)) {
                Ok(key) => key,
                Err(_) => break,
            };
            let gate = match self.directory_gate(&key) {
                Ok(gate) => gate,
                Err(_) => break,
            };
            if let Ok(_guard) = gate.try_lock_owned() {
                let result = self
                    .fetch_directory(&job.scope, &job.parent)
                    .await
                    .map(|_| ());
                self.activity.finish(&job, Some(&result));
            } else {
                self.activity.finish(&job, None);
            }
        }
    }
    pub fn ensure_feed(
        self: &Arc<Self>,
        collection: String,
        root: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            if self.cancel.is_cancelled() {
                return Ok(());
            }
            let scope = self.scope(&collection);
            let db = self.db.clone();
            tokio::task::spawn_blocking(move || Store::open(db)?.subscribe(&scope, &root))
                .await??;
            let mut feeds = self.feeds.write().await;
            if feeds.contains_key(&collection) || self.cancel.is_cancelled() {
                return Ok(());
            }
            let cancel = self.cancel.child_token();
            let (sender, hints) = ChangeHintSender::channel();
            feeds.insert(
                collection.clone(),
                Feed {
                    cancel: cancel.clone(),
                    hints: hints.clone(),
                },
            );
            let provider = self.provider.clone();
            let scope = self.scope(&collection);
            let watch_cancel = cancel.clone();
            self.tasks
                .spawn(watch_changes(provider, scope, sender, watch_cancel));
            let engine = self.clone();
            self.tasks.spawn(async move {
                engine.poll(collection, hints, cancel).await;
            });
            Ok(())
        })
    }
    async fn poll(
        self: Arc<Self>,
        collection: String,
        mut hints: watch::Receiver<ChangeHint>,
        cancel: CancellationToken,
    ) {
        let scope = self.scope(&collection);
        let mut delay = Duration::ZERO;
        let mut failures = 0u32;
        let mut reset = false;
        let mut generation = 0;
        let mut channel_open = true;
        let mut last_start = tokio::time::Instant::now() - Duration::from_secs(1);
        let mut health = FeedHealth {
            collection: collection.clone(),
            state: "indexing".into(),
            last_success: None,
            retry_at: None,
            message: None,
            notifications: NotificationState::Polling,
        };
        let db = self.db.clone();
        let s = scope.clone();
        if let Ok(Ok(Some(body))) =
            tokio::task::spawn_blocking(move || Store::open(db)?.health(&s)).await
            && let Ok(old) = serde_json::from_str::<FeedHealth>(&body)
        {
            health.last_success = old.last_success;
        }
        loop {
            // Failed refreshes retain their backoff even if more hints arrive.
            // After success, a pending generation wakes the feed before polling.
            let deadline = tokio::time::Instant::now() + delay;
            loop {
                if failures == 0 && hints.borrow().generation != generation {
                    break;
                }
                tokio::select! {biased;
                    _=cancel.cancelled()=>return,
                    _=tokio::time::sleep_until(deadline)=>break,
                    result=hints.changed(), if failures == 0 && channel_open => {
                        if result.is_err() { channel_open = false; }
                    }
                }
            }
            // Coalesce bursts without perpetually postponing an active stream.
            tokio::select! {biased;
                _=cancel.cancelled()=>return,
                _=tokio::time::sleep_until(last_start + Duration::from_millis(250))=>(),
            }
            last_start = tokio::time::Instant::now();
            generation = hints.borrow_and_update().generation;
            health.state = "indexing".into();
            health.retry_at = None;
            self.set_health(&scope, &health).await;
            let result = refresh(
                self.provider.as_ref(),
                &scope,
                &self.db,
                reset,
                &cancel,
                Some(&self.recent),
            )
            .await;
            let was = health.state.clone();
            match result {
                Ok(_) => {
                    reset = false;
                    failures = 0;
                    health.state = "ready".into();
                    health.last_success = Some(now());
                    health.message = None;
                    delay = Duration::from_secs(self.account.poll_seconds);
                    self.changed.metadata();
                    // One coalescing worker, not a task per notification burst.
                    self.discovery.notify_one();
                }
                Err(error) => {
                    if cancel.is_cancelled() {
                        break;
                    }
                    failures = failures.saturating_add(1);
                    let provider = error.downcast_ref::<ProviderError>();
                    (health.state, delay) = match provider {
                        Some(ProviderError::CursorExpired) => {
                            reset = true;
                            ("rebuilding".into(), Duration::from_secs(1))
                        }
                        Some(ProviderError::Throttled(delay)) => {
                            ("throttled".into(), *delay + Duration::from_secs(1))
                        }
                        Some(ProviderError::Authentication) => {
                            ("sign_in_required".into(), Duration::from_secs(60))
                        }
                        Some(ProviderError::Permission) | Some(ProviderError::NotFound) => {
                            ("unavailable".into(), Duration::from_secs(300))
                        }
                        _ => (
                            "offline".into(),
                            Duration::from_secs(2u64.saturating_pow(failures.min(7)).min(120)),
                        ),
                    };
                    // Safe typed errors only; database and network details may contain paths.
                    health.message = Some(
                        provider
                            .map(ToString::to_string)
                            .unwrap_or_else(|| "local metadata operation failed".into()),
                    );
                    let jitter = self.account.id.bytes().fold(0u64, |a, b| a + b as u64) % 4;
                    delay += Duration::from_secs(jitter);
                }
            }
            health.retry_at = Some(now() + delay.as_secs());
            // Say it once, where an operator looks. `health.message` is built
            // from typed provider errors only -- the branch above is explicit
            // that database and network detail may carry paths -- so it is safe
            // to write down.
            match feed_notice(&was, &health.state) {
                FeedNotice::Failed => tracing::warn!(
                    collection = %scope.collection,
                    state = %health.state,
                    reason = health.message.as_deref().unwrap_or("unknown"),
                    "a collection stopped updating"
                ),
                FeedNotice::Recovered => tracing::info!(
                    collection = %scope.collection,
                    was = %was,
                    "a collection is updating again"
                ),
                FeedNotice::Nothing => {}
            }
            self.set_health(&scope, &health).await;
        }
    }
    async fn set_health(&self, scope: &Scope, health: &FeedHealth) {
        self.health
            .write()
            .await
            .insert(scope.collection.clone(), health.clone());
        if let Ok(body) = serde_json::to_string(health) {
            let db = self.db.clone();
            let scope = scope.clone();
            let _ = tokio::task::spawn_blocking(move || Store::open(db)?.set_health(&scope, &body))
                .await;
        }
    }
    async fn discover(self: &Arc<Self>) -> Result<()> {
        let mut queue =
            VecDeque::from([(self.account.drive.id.clone(), self.account.root_id.clone())]);
        let mut seen = HashSet::new();
        while let Some((drive, root)) = queue.pop_front() {
            if !seen.insert((drive.clone(), root.clone())) {
                continue;
            }
            if self.cancel.is_cancelled() {
                return Ok(());
            }
            if seen.len() > 256 {
                tracing::warn!(
                    "linked-drive discovery exceeded 256 roots; keeping existing subscriptions"
                );
                return Ok(());
            }
            self.ensure_feed(drive.clone(), root.clone()).await?;
            let scope = self.scope(&drive);
            let db = self.db.clone();
            let nodes = tokio::task::spawn_blocking(move || {
                Store::open(db)?.shortcuts_under(&scope, &root)
            })
            .await??;
            for node in nodes {
                if let Some(target) = node.target {
                    queue.push_back((target.collection, target.item));
                }
            }
        }
        // Remove subscriptions only after every reachable scope has a complete
        // index. A temporarily unavailable library is not evidence of deletion.
        let db = self.db.clone();
        let scopes: Vec<_> = seen.iter().map(|(drive, _)| self.scope(drive)).collect();
        let complete = tokio::task::spawn_blocking(move || -> cirrove_store::Result<bool> {
            let store = Store::open(db)?;
            for scope in scopes {
                if store.cursor(&scope)?.is_none() {
                    return Ok(false);
                }
            }
            Ok(true)
        })
        .await??;
        if complete {
            let keep: HashSet<_> = seen.iter().map(|(drive, _)| drive.clone()).collect();
            let db = self.db.clone();
            let account = self.account.id.clone();
            tokio::task::spawn_blocking(move || {
                Store::open(db)?.replace_subscriptions(&account, &seen)
            })
            .await??;
            let mut feeds = self.feeds.write().await;
            feeds.retain(|drive, feed| {
                if keep.contains(drive) {
                    true
                } else {
                    feed.cancel.cancel();
                    false
                }
            });
            self.health
                .write()
                .await
                .retain(|drive, _| keep.contains(drive));
        }
        Ok(())
    }
    pub async fn node(&self, scope: &Scope, id: &str) -> Result<Node, ProviderError> {
        self.obtain_node(scope, id, false).await
    }
    pub(crate) async fn refresh_node(
        &self,
        scope: &Scope,
        id: &str,
    ) -> Result<Node, ProviderError> {
        self.obtain_node(scope, id, true).await
    }
    async fn obtain_node(
        &self,
        scope: &Scope,
        id: &str,
        refresh: bool,
    ) -> Result<Node, ProviderError> {
        let db = self.db.clone();
        let s = scope.clone();
        let item = id.to_string();
        let (cached, ticket) = tokio::task::spawn_blocking(move || -> cirrove_store::Result<_> {
            let mut store = Store::open(db)?;
            let cached = if refresh {
                None
            } else {
                store.node(&s, &item)?
            };
            let ticket = if cached.is_none() {
                Some(store.node_observation(&s, &item)?)
            } else {
                None
            };
            Ok((cached, ticket))
        })
        .await
        .map_err(|_| ProviderError::Unavailable)?
        .map_err(|_| ProviderError::Unavailable)?;
        if let Some(node) = cached {
            return Ok(node);
        }
        let ticket = ticket.ok_or(ProviderError::Unavailable)?;
        // A single-item fetch reaches this point only when the index has no row
        // for the item, which a cached navigation should never hit; several
        // actual-kernel fixtures assert zero foreground requests across deep
        // traversals. When it does happen it is on a latency-bounded FUSE path,
        // so enforce the deadline here rather than trusting an adapter to honour
        // the token, exactly as the directory path already does.
        let fetch = tokio::time::timeout(
            SINGLE_ITEM_TIMEOUT,
            self.provider.node(scope, id, &self.cancel),
        );
        let node = match fetch.await.unwrap_or(Err(ProviderError::Unavailable)) {
            Ok(node) => node,
            Err(ProviderError::NotFound) => {
                let db = self.db.clone();
                let result =
                    tokio::task::spawn_blocking(move || Store::open(db)?.publish_absence(&ticket))
                        .await
                        .map_err(|_| ProviderError::Unavailable)?
                        .map_err(|_| ProviderError::Unavailable)?;
                return match result {
                    cirrove_store::AbsenceResult::Published { changed } => {
                        if changed {
                            self.changed.metadata();
                        }
                        Err(ProviderError::NotFound)
                    }
                    cirrove_store::AbsenceResult::Superseded(Some(node)) => Ok(node),
                    cirrove_store::AbsenceResult::Superseded(None) => {
                        Err(ProviderError::VersionChanged)
                    }
                };
            }
            Err(error) => return Err(error),
        };
        let db = self.db.clone();
        let result =
            tokio::task::spawn_blocking(move || Store::open(db)?.publish_node(&ticket, &node))
                .await
                .map_err(|_| ProviderError::Unavailable)?
                .map_err(|_| ProviderError::Unavailable)?;
        match result {
            cirrove_store::ObservationResult::Published { value, changed } => {
                if changed {
                    self.changed.metadata();
                }
                Ok(value)
            }
            cirrove_store::ObservationResult::Superseded(Some(node)) => Ok(node),
            cirrove_store::ObservationResult::Superseded(None) => {
                Err(ProviderError::VersionChanged)
            }
        }
    }
    fn directory_gate(&self, key: &str) -> Result<Arc<Mutex<()>>, ProviderError> {
        let mut gates = self
            .directories
            .lock()
            .map_err(|_| ProviderError::Unavailable)?;
        if let Some(gate) = gates.get(key).and_then(Weak::upgrade) {
            return Ok(gate);
        }
        gates.retain(|_, g| g.strong_count() > 0);
        let gate = Arc::new(Mutex::new(()));
        gates.insert(key.into(), Arc::downgrade(&gate));
        Ok(gate)
    }
    /// A warm name lookup decodes only its indexed match. Unknown directories
    /// still use the coalesced foreground listing path before selecting a name.
    pub async fn child(
        self: &Arc<Self>,
        scope: &Scope,
        parent: &str,
        name: &str,
    ) -> Result<Node, ProviderError> {
        if scope.account != self.account.id || scope.provider != self.provider.provider_id() {
            return Err(ProviderError::Protocol("provider/account mismatch"));
        }
        if self.cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let db = self.db.clone();
        let s = scope.clone();
        let p = parent.to_owned();
        let n = name.to_owned();
        let cached = tokio::task::spawn_blocking(move || Store::open(db)?.child(&s, &p, &n))
            .await
            .map_err(|_| ProviderError::Unavailable)?
            .map_err(|_| ProviderError::Unavailable)?;
        if let Some(node) = cached {
            self.activity.touch(scope, parent);
            return node.ok_or(ProviderError::NotFound);
        }
        let name = name.to_owned();
        self.with_children(scope, parent, move |nodes| {
            for node in nodes {
                let node = node.map_err(|_| ProviderError::Unavailable)?;
                if node.name == name {
                    return Ok(node);
                }
            }
            Err(ProviderError::NotFound)
        })
        .await?
    }
    pub async fn children(
        self: &Arc<Self>,
        scope: &Scope,
        parent: &str,
    ) -> Result<Vec<Node>, ProviderError> {
        self.with_children(scope, parent, |rows| {
            rows.collect::<cirrove_store::Result<Vec<_>>>()
        })
        .await?
        .map_err(|_| ProviderError::Unavailable)
    }
    /// The consumer executes once on a blocking worker within a consistent
    /// metadata read, never during a provider request. Unknown directories are
    /// fetched first under the existing coalescing gate and total fetch deadline.
    pub async fn with_children<T, F>(
        self: &Arc<Self>,
        scope: &Scope,
        parent: &str,
        consume: F,
    ) -> Result<T, ProviderError>
    where
        F: FnMut(&mut dyn Iterator<Item = cirrove_store::Result<Node>>) -> T + Send + 'static,
        T: Send + 'static,
    {
        if scope.account != self.account.id || scope.provider != self.provider.provider_id() {
            return Err(ProviderError::Protocol("provider/account mismatch"));
        }
        let (cached, consume) = self.consume_cached(scope, parent, consume).await?;
        if let Some(value) = cached {
            self.activity.touch(scope, parent);
            return Ok(value);
        }
        let key =
            serde_json::to_string(&(scope, parent)).map_err(|_| ProviderError::Unavailable)?;
        let gate = self.directory_gate(&key)?;
        let _guard = tokio::select! {biased;_=self.cancel.cancelled()=>return Err(ProviderError::Cancelled),g=gate.lock()=>g};
        let (cached, consume) = self.consume_cached(scope, parent, consume).await?;
        if let Some(value) = cached {
            self.activity.touch(scope, parent);
            return Ok(value);
        }
        let result = self.fetch_directory(scope, parent).await.map(|_| ());
        self.activity.touch(scope, parent);
        self.activity.observed(
            &crate::activity::DirectoryJob {
                scope: scope.clone(),
                parent: parent.into(),
            },
            &result,
        );
        result?;
        let (cached, _) = self.consume_cached(scope, parent, consume).await?;
        cached.ok_or(ProviderError::VersionChanged)
    }
    async fn consume_cached<T, F>(
        &self,
        scope: &Scope,
        parent: &str,
        mut consume: F,
    ) -> Result<(Option<T>, F), ProviderError>
    where
        F: FnMut(&mut dyn Iterator<Item = cirrove_store::Result<Node>>) -> T + Send + 'static,
        T: Send + 'static,
    {
        if self.cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let db = self.db.clone();
        let scope = scope.clone();
        let parent = parent.to_owned();
        tokio::task::spawn_blocking(move || {
            let store = Store::open(db).map_err(|_| ProviderError::Unavailable)?;
            let result = store
                .with_children(&scope, &parent, &mut consume)
                .map_err(|_| ProviderError::Unavailable)?;
            Ok((result, consume))
        })
        .await
        .map_err(|_| ProviderError::Unavailable)?
    }
    async fn fetch_directory(
        self: &Arc<Self>,
        scope: &Scope,
        parent: &str,
    ) -> Result<(), ProviderError> {
        let cancel = self.cancel.child_token();
        // Dropping a timeout or an abandoned caller also cancels blocking SQL.
        let _cancel_on_drop = cancel.clone().drop_guard();
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        tokio::select! {biased; _=cancel.cancelled()=>Err(ProviderError::Cancelled),
            result=tokio::time::timeout_at(deadline.into(),self.fetch_directory_inner(scope,parent,cancel.clone(),deadline))=>result.map_err(|_|ProviderError::Unavailable)?,
        }
    }
    async fn fetch_directory_inner(
        self: &Arc<Self>,
        scope: &Scope,
        parent: &str,
        cancel: CancellationToken,
        deadline: std::time::Instant,
    ) -> Result<(), ProviderError> {
        use cirrove_store::DirectoryPublicationResult;
        for _ in 0..3 {
            let permit = tokio::select! {biased;
                _=cancel.cancelled()=>return Err(ProviderError::Cancelled),
                p=self.directory_publications.clone().acquire_owned()=>p.map_err(|_|ProviderError::Unavailable)?,
            };
            let db = self.db.clone();
            let s = scope.clone();
            let p = parent.to_owned();
            let token = cancel.clone();
            // The permit travels with blocking work: timing out its async waiter
            // cannot admit another builder before the old one has actually ended.
            let mut staged = self
                .tasks
                .spawn_blocking(move || {
                    Store::open(db)?
                        .directory_publication(&s, &p, token, deadline)
                        .map(|stage| (stage, permit))
                })
                .await
                .map_err(|_| ProviderError::Unavailable)?
                .map_err(|_| ProviderError::Unavailable)?;
            let mut cursor = None;
            loop {
                let page = self
                    .provider
                    .children(scope, parent, cursor.as_ref(), &cancel)
                    .await?;
                let next = page.next.clone();
                staged = self
                    .tasks
                    .spawn_blocking(move || {
                        let (stage, permit) = staged;
                        stage.page(page).map(|stage| (stage, permit))
                    })
                    .await
                    .map_err(|_| ProviderError::Unavailable)?
                    .map_err(|_| ProviderError::Unavailable)?;
                cursor = next;
                if cursor.is_none() {
                    break;
                }
            }
            let db = self.db.clone();
            let s = scope.clone();
            let p = parent.to_owned();
            let engine = self.clone();
            let token = cancel.clone();
            let (result, targets) = self
                .tasks
                .spawn_blocking(move || -> cirrove_store::Result<_> {
                    let (stage, _permit) = staged;
                    let result = stage.publish()?;
                    if matches!(
                        result,
                        DirectoryPublicationResult::Published { changed: true }
                    ) {
                        engine.changed.metadata();
                    }
                    let mut targets = Vec::new();
                    if result != (DirectoryPublicationResult::Superseded { known: false }) {
                        Store::open(db)?
                            .with_children(&s, &p, |rows| {
                                for node in rows {
                                    if token.is_cancelled() || std::time::Instant::now() >= deadline
                                    {
                                        return Err(cirrove_store::StoreError::Cancelled);
                                    }
                                    if let Some(target) = node?.target {
                                        if targets.iter().any(
                                            |previous: &cirrove_core::RemoteRef| {
                                                previous.collection == target.collection
                                                    && previous.item == target.item
                                            },
                                        ) {
                                            continue;
                                        }
                                        targets.push(*target);
                                        if targets.len() == 257 {
                                            break;
                                        }
                                    }
                                }
                                Ok::<_, cirrove_store::StoreError>(())
                            })?
                            .transpose()?;
                    }
                    Ok((result, targets))
                })
                .await
                .map_err(|_| ProviderError::Unavailable)?
                .map_err(|_| ProviderError::Unavailable)?;
            if matches!(
                result,
                DirectoryPublicationResult::Superseded { known: false }
            ) {
                continue;
            }
            if targets.len() > 256 {
                tracing::warn!("directory linked-drive discovery exceeded 256 targets");
            }
            for target in targets.into_iter().take(256) {
                let _ = self.ensure_feed(target.collection, target.item).await;
            }
            return Ok(());
        }
        Err(ProviderError::VersionChanged)
    }
}

/// Reconnection never owns a metadata request slot or a filesystem lock. Providers
/// without notifications keep ordinary polling; transient failures never disable
/// notifications permanently. Reconnection triggers catch-up through the sender.
async fn watch_changes(
    provider: Arc<dyn ReadProvider>,
    scope: Scope,
    hints: ChangeHintSender,
    cancel: CancellationToken,
) {
    let mut failures = 0u32;
    loop {
        hints.state(NotificationState::Connecting);
        let started = tokio::time::Instant::now();
        let result = tokio::select! {biased;
            _=cancel.cancelled()=>return,
            result=provider.watch_changes(&scope, hints.clone(), &cancel)=>result,
        };
        if started.elapsed() >= Duration::from_secs(60) {
            failures = 0;
        }
        let delay = match result {
            Ok(WatchEnd::Unsupported) => {
                hints.state(NotificationState::Polling);
                return;
            }
            Ok(WatchEnd::Renew) => continue,
            Err(ProviderError::Cancelled) => return,
            Err(ProviderError::Authentication) => {
                hints.state(NotificationState::SignInRequired);
                Duration::from_secs(60)
            }
            Err(error) => {
                hints.state(NotificationState::Retrying);
                failures = failures.saturating_add(1);
                let jitter = scope.collection.bytes().fold(0u64, |a, b| a + b as u64) % 4;
                match error {
                    ProviderError::Throttled(delay) => delay + Duration::from_secs(1),
                    ProviderError::Permission | ProviderError::NotFound => Duration::from_secs(300),
                    _ => Duration::from_secs(2u64.pow(failures.min(7)).min(120) + jitter),
                }
            }
        };
        tokio::select! {biased; _=cancel.cancelled()=>return, _=tokio::time::sleep(delay)=>()}
    }
}

/// Bytes of a file's content present on disk: the block files that exist,
/// less the digest each carries ahead of its data. Metadata, not the index --
/// between a block being forgotten and the index rebuilt, the index would
/// still count it.
fn resident_bytes(cache: &std::path::Path, scope: &Scope, node: &Node) -> u64 {
    let Some(keys) = crate::content::block_keys(scope, node) else {
        return 0;
    };
    let present: u64 = keys
        .iter()
        .filter_map(|key| std::fs::metadata(cache.join(key)).ok())
        .map(|meta| meta.len().saturating_sub(crate::content::BLOCK_DIGEST))
        .sum();
    present.min(node.size)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{FeedNotice, feed_notice};

    /// A refused collection is announced once, not once a minute.
    ///
    /// The daemon wrote nothing at all when a real grant was withdrawn, and an
    /// operator reading the journal saw a healthy daemon for seventeen minutes.
    /// The cure has to avoid the opposite failure: a feed in this state retries
    /// every sixty seconds, and a line per retry would bury the one line that
    /// matters under the ones that do not.
    #[test]
    fn a_collection_that_stopped_is_announced_once_and_its_return_once() {
        assert_eq!(feed_notice("ready", "sign_in_required"), FeedNotice::Failed);
        for _ in 0..5 {
            assert_eq!(
                feed_notice("sign_in_required", "sign_in_required"),
                FeedNotice::Nothing,
                "a retry is not news"
            );
        }
        assert_eq!(
            feed_notice("sign_in_required", "ready"),
            FeedNotice::Recovered
        );
        assert_eq!(feed_notice("ready", "ready"), FeedNotice::Nothing);
    }

    /// One failure replacing another is still worth a line, because the remedy
    /// changes with it: waiting out a throttle and signing in again are not the
    /// same instruction to a person.
    #[test]
    fn a_different_failure_is_not_the_same_failure() {
        assert_eq!(
            feed_notice("offline", "sign_in_required"),
            FeedNotice::Failed
        );
        assert_eq!(feed_notice("throttled", "offline"), FeedNotice::Failed);
    }

    /// Starting up is not a fault. A feed begins in `indexing` and passes
    /// through it again on every reset, so announcing it would make the log
    /// noisy in exactly the situation where it needs to be readable.
    #[test]
    fn indexing_is_work_rather_than_a_fault() {
        assert_eq!(feed_notice("indexing", "ready"), FeedNotice::Nothing);
        assert_eq!(feed_notice("ready", "indexing"), FeedNotice::Nothing);
        assert_eq!(
            feed_notice("rebuilding", "indexing"),
            FeedNotice::Nothing,
            "a reset passes through indexing and is not a new fault"
        );
        // But a real failure after indexing still speaks.
        assert_eq!(feed_notice("indexing", "offline"), FeedNotice::Failed);
    }
}
