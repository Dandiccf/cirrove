//! Per-account metadata service. Change feeds and foreground directory requests
//! share a provider client but never hold SQLite locks across network awaits.
use crate::{accounts::Account, content::ContentCache, private_dir, refresh};
use anyhow::Result;
use cirrove_core::notifications::{ChangeHint, ChangeHintSender, NotificationState, WatchEnd};
use cirrove_core::{CancellationToken, Node, ProviderError, ReadProvider, Scope};
use cirrove_store::Store;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::{
        Arc, Mutex as StdMutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Mutex, Notify, RwLock, watch};
use tokio_util::task::TaskTracker;

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
pub struct Engine {
    pub account: Account,
    pub db: PathBuf,
    pub provider: Arc<dyn ReadProvider>,
    pub cache: ContentCache,
    pub cancel: CancellationToken,
    pub changed: Notify,
    feeds: RwLock<HashMap<String, Feed>>,
    health: RwLock<HashMap<String, FeedHealth>>,
    directories: StdMutex<HashMap<String, Weak<Mutex<()>>>>,
    tasks: TaskTracker,
    discovery: Notify,
    discovery_started: AtomicBool,
    activity: crate::activity::DirectoryActivity,
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
        tokio::task::spawn_blocking(move || Store::open(path)).await??;
        let cache_db = db.clone();
        let quota = account.cache_bytes;
        let cache = tokio::task::spawn_blocking(move || {
            ContentCache::new(directory.join("cache"), cache_db, quota)
        })
        .await??;
        Ok(Arc::new(Self {
            account,
            db,
            provider,
            cache,
            cancel: CancellationToken::new(),
            changed: Notify::new(),
            feeds: RwLock::new(HashMap::new()),
            health: RwLock::new(HashMap::new()),
            directories: StdMutex::new(HashMap::new()),
            tasks: TaskTracker::new(),
            discovery: Notify::new(),
            discovery_started: AtomicBool::new(false),
            activity: crate::activity::DirectoryActivity::default(),
            _owner: owner,
        }))
    }
    pub fn scope(&self, collection: &str) -> Scope {
        Scope {
            account: self.account.id.clone(),
            provider: self.provider.provider_id().into(),
            collection: collection.into(),
        }
    }
    pub async fn start(self: &Arc<Self>) -> Result<()> {
        if !self.discovery_started.swap(true, Ordering::SeqCst) {
            let engine = self.clone();
            self.tasks.spawn(async move {
                engine.refresh_active_directories().await;
            });
            let engine = self.clone();
            self.tasks.spawn(async move {
                loop {
                    tokio::select! {biased;
                        _=engine.cancel.cancelled()=>return,
                        _=engine.discovery.notified()=>(),
                    }
                    let _ = engine.discover().await;
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
            let result = refresh(self.provider.as_ref(), &scope, &self.db, reset, &cancel).await;
            match result {
                Ok(_) => {
                    reset = false;
                    failures = 0;
                    health.state = "ready".into();
                    health.last_success = Some(now());
                    health.message = None;
                    delay = Duration::from_secs(self.account.poll_seconds);
                    self.changed.notify_waiters();
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
        let node = match self.provider.node(scope, id, &self.cancel).await {
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
                            self.changed.notify_waiters();
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
                    self.changed.notify_waiters();
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
    pub async fn children(
        self: &Arc<Self>,
        scope: &Scope,
        parent: &str,
    ) -> Result<Vec<Node>, ProviderError> {
        if scope.account != self.account.id || scope.provider != self.provider.provider_id() {
            return Err(ProviderError::Protocol("provider/account mismatch"));
        }
        let db = self.db.clone();
        let s = scope.clone();
        let p = parent.to_owned();
        let cached = tokio::task::spawn_blocking(move || -> cirrove_store::Result<_> {
            let store = Store::open(db)?;
            store.children(&s, &p)
        })
        .await
        .map_err(|_| ProviderError::Unavailable)?
        .map_err(|_| ProviderError::Unavailable)?;
        if let Some(nodes) = cached {
            self.activity.touch(scope, parent);
            return Ok(nodes);
        }
        let key =
            serde_json::to_string(&(scope, parent)).map_err(|_| ProviderError::Unavailable)?;
        let gate = self.directory_gate(&key)?;
        let _guard = tokio::select! {biased;_=self.cancel.cancelled()=>return Err(ProviderError::Cancelled),g=gate.lock()=>g};
        let db = self.db.clone();
        let s = scope.clone();
        let p = parent.to_owned();
        if let Some(nodes) = tokio::task::spawn_blocking(move || Store::open(db)?.children(&s, &p))
            .await
            .map_err(|_| ProviderError::Unavailable)?
            .map_err(|_| ProviderError::Unavailable)?
        {
            self.activity.touch(scope, parent);
            return Ok(nodes);
        }
        let result = self.fetch_directory(scope, parent).await;
        self.activity.touch(scope, parent);
        self.activity.observed(
            &crate::activity::DirectoryJob {
                scope: scope.clone(),
                parent: parent.into(),
            },
            &result.as_ref().map(|_| ()).map_err(Clone::clone),
        );
        result
    }
    async fn fetch_directory(
        self: &Arc<Self>,
        scope: &Scope,
        parent: &str,
    ) -> Result<Vec<Node>, ProviderError> {
        tokio::select! {biased; _=self.cancel.cancelled()=>Err(ProviderError::Cancelled),
            result=tokio::time::timeout(Duration::from_secs(60),self.fetch_directory_inner(scope,parent))=>result.map_err(|_|ProviderError::Unavailable)?,
        }
    }
    async fn fetch_directory_inner(
        self: &Arc<Self>,
        scope: &Scope,
        parent: &str,
    ) -> Result<Vec<Node>, ProviderError> {
        // A concurrent publication can invalidate a cold snapshot. Cached newer
        // data wins immediately; otherwise retry within the outer total deadline.
        for _ in 0..3 {
            let db = self.db.clone();
            let s = scope.clone();
            let p = parent.to_owned();
            let ticket =
                tokio::task::spawn_blocking(move || Store::open(db)?.directory_observation(&s, &p))
                    .await
                    .map_err(|_| ProviderError::Unavailable)?
                    .map_err(|_| ProviderError::Unavailable)?;
            let mut nodes = vec![];
            let mut cursor = None;
            let mut seen = HashSet::new();
            loop {
                let page = self
                    .provider
                    .children(scope, parent, cursor.as_ref(), &self.cancel)
                    .await?;
                nodes.extend(page.nodes);
                if nodes.len() > 100_000 {
                    return Err(ProviderError::Protocol("directory exceeds entry limit"));
                }
                cursor = page.next;
                let Some(next) = &cursor else { break };
                if !seen.insert(next.0.clone()) {
                    return Err(ProviderError::Protocol("repeated directory cursor"));
                }
            }
            nodes.sort_by(|a, b| a.name.cmp(&b.name));
            let db = self.db.clone();
            let result = tokio::task::spawn_blocking(move || {
                Store::open(db)?.publish_directory(&ticket, &nodes)
            })
            .await
            .map_err(|_| ProviderError::Unavailable)?
            .map_err(|_| ProviderError::Unavailable)?;
            let (nodes, changed) = match result {
                cirrove_store::ObservationResult::Published { value, changed } => (value, changed),
                cirrove_store::ObservationResult::Superseded(Some(nodes)) => (nodes, false),
                cirrove_store::ObservationResult::Superseded(None) => continue,
            };
            for node in &nodes {
                if let Some(target) = &node.target {
                    let _ = self
                        .ensure_feed(target.collection.clone(), target.item.clone())
                        .await;
                }
            }
            if changed {
                self.changed.notify_waiters();
            }
            return Ok(nodes);
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
