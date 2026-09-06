//! Per-account metadata service. Change feeds and foreground directory requests
//! share a provider client but never hold SQLite locks across network awaits.
use crate::{accounts::Account, content::ContentCache, private_dir, refresh};
use anyhow::Result;
use cirrove_core::{CancellationToken, Node, ProviderError, ReadProvider, Scope};
use cirrove_store::Store;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::{Arc, Mutex as StdMutex, Weak},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Mutex, Notify, RwLock};
use tokio_util::task::TaskTracker;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FeedHealth {
    pub collection: String,
    pub state: String,
    pub last_success: Option<u64>,
    pub retry_at: Option<u64>,
    pub message: Option<String>,
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
    feeds: RwLock<HashMap<String, CancellationToken>>,
    health: RwLock<HashMap<String, FeedHealth>>,
    directories: StdMutex<HashMap<String, Weak<Mutex<()>>>>,
    tasks: TaskTracker,
    discovery: Mutex<()>,
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
            discovery: Mutex::new(()),
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
        for cancel in self.feeds.read().await.values() {
            cancel.cancel();
        }
        self.tasks.close();
        self.tasks.wait().await;
    }
    pub async fn health(&self) -> Vec<FeedHealth> {
        self.health.read().await.values().cloned().collect()
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
            feeds.insert(collection.clone(), cancel.clone());
            let engine = self.clone();
            self.tasks.spawn(async move {
                engine.poll(collection, cancel).await;
            });
            Ok(())
        })
    }
    async fn poll(self: Arc<Self>, collection: String, cancel: CancellationToken) {
        let scope = self.scope(&collection);
        let mut delay = Duration::ZERO;
        let mut failures = 0u32;
        let mut reset = false;
        let mut health = FeedHealth {
            collection: collection.clone(),
            state: "indexing".into(),
            last_success: None,
            retry_at: None,
            message: None,
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
            tokio::select! {biased;_=cancel.cancelled()=>break,_=tokio::time::sleep(delay)=>()}
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
                    // Discovery is independent of the feed task, avoiding recursive async types.
                    let engine = self.clone();
                    self.tasks.spawn(async move {
                        let _ = engine.discover().await;
                    });
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
        let _discovery = tokio::select! {
            biased; _ = self.cancel.cancelled() => return Ok(()),
            guard = self.discovery.lock() => guard,
        };
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
            feeds.retain(|drive, cancel| {
                if keep.contains(drive) {
                    true
                } else {
                    cancel.cancel();
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
        let db = self.db.clone();
        let s = scope.clone();
        let item = id.to_string();
        let cached = tokio::task::spawn_blocking(move || Store::open(db)?.node(&s, &item))
            .await
            .map_err(|_| ProviderError::Unavailable)?
            .map_err(|_| ProviderError::Unavailable)?;
        if let Some(node) = cached {
            return Ok(node);
        }
        let node = self.provider.node(scope, id, &self.cancel).await?;
        let db = self.db.clone();
        let s = scope.clone();
        let n = node.clone();
        tokio::task::spawn_blocking(move || Store::open(db)?.observe_node(&s, &n))
            .await
            .map_err(|_| ProviderError::Unavailable)?
            .map_err(|_| ProviderError::Unavailable)?;
        Ok(node)
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
        let db = self.db.clone();
        let s = scope.clone();
        let p = parent.to_owned();
        let (cached, age) = tokio::task::spawn_blocking(move || -> cirrove_store::Result<_> {
            let store = Store::open(db)?;
            Ok((store.children(&s, &p)?, store.directory_age(&s, &p)?))
        })
        .await
        .map_err(|_| ProviderError::Unavailable)?
        .map_err(|_| ProviderError::Unavailable)?;
        if let Some(nodes) = cached {
            if age.is_some_and(|age| age >= self.account.poll_seconds)
                && !self.cancel.is_cancelled()
            {
                let key = serde_json::to_string(&(scope, parent))
                    .map_err(|_| ProviderError::Unavailable)?;
                if let Ok(guard) = self.directory_gate(&key)?.try_lock_owned() {
                    let engine = self.clone();
                    let scope = scope.clone();
                    let parent = parent.to_owned();
                    self.tasks.spawn(async move {
                        let _guard = guard;
                        let _ = engine.fetch_directory(&scope, &parent).await;
                    });
                }
            }
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
            return Ok(nodes);
        }
        self.fetch_directory(scope, parent).await
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
        let s = scope.clone();
        let p = parent.to_owned();
        let n = nodes.clone();
        tokio::task::spawn_blocking(move || Store::open(db)?.observe_directory(&s, &p, &n))
            .await
            .map_err(|_| ProviderError::Unavailable)?
            .map_err(|_| ProviderError::Unavailable)?;
        for node in &nodes {
            if let Some(target) = &node.target {
                let _ = self
                    .ensure_feed(target.collection.clone(), target.item.clone())
                    .await;
            }
        }
        self.changed.notify_waiters();
        Ok(nodes)
    }
}
