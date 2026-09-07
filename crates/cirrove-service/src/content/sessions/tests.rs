#![allow(clippy::unwrap_used)]
use super::*;
use async_trait::async_trait;
use cirrove_core::{ChangePage, Cursor, DirectoryPage, MetadataProvider, NodeKind};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Notify;

#[derive(Default)]
struct Counts {
    opens: AtomicUsize,
    reads: AtomicUsize,
    legacy: AtomicUsize,
}
#[derive(Default)]
struct Provider {
    counts: Arc<Counts>,
    wrong: bool,
    unsupported: bool,
    held_item: Option<String>,
    entered: Notify,
    release: Notify,
}
struct Session {
    identity: ReadIdentity,
    counts: Arc<Counts>,
}
#[async_trait]
impl ReadSession for Session {
    fn identity(&self) -> &ReadIdentity {
        &self.identity
    }
    async fn read_range(
        &self,
        offset: u64,
        length: u32,
        _: &CancellationToken,
    ) -> Result<Vec<u8>, ProviderError> {
        self.counts.reads.fetch_add(1, Ordering::SeqCst);
        Ok(vec![
            b'A';
            self.identity.size.saturating_sub(offset).min(length as u64)
                as usize
        ])
    }
}
#[async_trait]
impl MetadataProvider for Provider {
    fn provider_id(&self) -> &'static str {
        "fixture"
    }
    async fn changes(
        &self,
        _: &Scope,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> Result<ChangePage, ProviderError> {
        Err(ProviderError::Unavailable)
    }
}
#[async_trait]
impl ReadProvider for Provider {
    async fn open_read_session(
        &self,
        scope: &Scope,
        node: &Node,
        cancel: &CancellationToken,
    ) -> Result<Option<Arc<dyn ReadSession>>, ProviderError> {
        self.counts.opens.fetch_add(1, Ordering::SeqCst);
        if self.held_item.as_deref() == Some(&node.id) {
            self.entered.notify_one();
            tokio::select! { biased;
                _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
                _ = self.release.notified() => (),
            }
        }
        tokio::task::yield_now().await;
        if self.unsupported {
            return Ok(None);
        }
        let mut identity = ReadIdentity::new(scope, node)?;
        if self.wrong {
            identity.scope.account = "other-account".into();
        }
        Ok(Some(Arc::new(Session {
            identity,
            counts: self.counts.clone(),
        })))
    }
    async fn node(&self, _: &Scope, _: &str, _: &CancellationToken) -> Result<Node, ProviderError> {
        Err(ProviderError::Unavailable)
    }
    async fn children(
        &self,
        _: &Scope,
        _: &str,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> Result<DirectoryPage, ProviderError> {
        Err(ProviderError::Unavailable)
    }
    async fn read_range(
        &self,
        _: &Scope,
        node: &Node,
        offset: u64,
        length: u32,
        _: &CancellationToken,
    ) -> Result<Vec<u8>, ProviderError> {
        self.counts.legacy.fetch_add(1, Ordering::SeqCst);
        Ok(vec![
            b'A';
            node.size.saturating_sub(offset).min(length as u64)
                as usize
        ])
    }
}
fn scope() -> Scope {
    Scope {
        account: "account".into(),
        provider: "fixture".into(),
        collection: "drive".into(),
    }
}
fn node() -> Node {
    Node {
        id: "file".into(),
        parent_id: None,
        name: "name".into(),
        kind: NodeKind::File,
        size: 3 * super::super::BLOCK_SIZE as u64,
        modified_unix: 0,
        etag: Some("meta".into()),
        content_version: Some("v1".into()),
        target: None,
    }
}
async fn read(
    sessions: &Sessions,
    provider: &Provider,
    scope: &Scope,
    node: &Node,
) -> Result<Vec<u8>, ProviderError> {
    sessions
        .read(provider, scope, node, 0, 32, &CancellationToken::new())
        .await
}

#[tokio::test]
async fn creation_is_coalesced_across_concurrent_ranges_and_reopens() {
    let sessions = Arc::new(Sessions::default());
    let provider = Arc::new(Provider::default());
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..32 {
        let (s, p) = (sessions.clone(), provider.clone());
        tasks.spawn(async move {
            assert_eq!(
                read(&s, &p, &scope(), &node()).await.unwrap(),
                vec![b'A'; 32]
            );
        });
    }
    while let Some(result) = tasks.join_next().await {
        result.unwrap();
    }
    read(&sessions, &provider, &scope(), &node()).await.unwrap();
    assert_eq!(provider.counts.opens.load(Ordering::SeqCst), 1);
    assert_eq!(provider.counts.reads.load(Ordering::SeqCst), 33);
    assert_eq!(provider.counts.legacy.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn identity_separates_accounts_drives_items_revision_namespaces_and_sizes() {
    let sessions = Sessions::default();
    let p = Provider::default();
    read(&sessions, &p, &scope(), &node()).await.unwrap();
    let mut renamed = node();
    renamed.name = "renamed".into();
    renamed.etag = Some("new-meta".into());
    read(&sessions, &p, &scope(), &renamed).await.unwrap();
    assert_eq!(p.counts.opens.load(Ordering::SeqCst), 1);
    for field in [
        "account",
        "collection",
        "provider",
        "item",
        "revision",
        "namespace",
        "size",
    ] {
        let (mut s, mut n) = (scope(), node());
        match field {
            "account" => s.account = "other".into(),
            "collection" => s.collection = "other".into(),
            "provider" => s.provider = "other".into(),
            "item" => n.id = "other".into(),
            "revision" => n.content_version = Some("other".into()),
            "namespace" => {
                n.etag = n.content_version.take();
            }
            "size" => n.size += 1,
            _ => unreachable!(),
        }
        read(&sessions, &p, &s, &n).await.unwrap();
    }
    assert_eq!(p.counts.opens.load(Ordering::SeqCst), 8);
}

#[tokio::test]
async fn wrong_identity_never_reads_and_unsupported_sessions_use_original_contract() {
    let sessions = Sessions::default();
    let p = Provider {
        wrong: true,
        ..Default::default()
    };
    assert!(read(&sessions, &p, &scope(), &node()).await.is_err());
    assert_eq!(p.counts.reads.load(Ordering::SeqCst), 0);
    let sessions = Sessions::default();
    let p = Provider {
        unsupported: true,
        ..Default::default()
    };
    for _ in 0..3 {
        read(&sessions, &p, &scope(), &node()).await.unwrap();
    }
    assert_eq!(p.counts.opens.load(Ordering::SeqCst), 1);
    assert_eq!(p.counts.legacy.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn active_entries_are_never_evicted_and_overflow_uses_bounded_fallback() {
    let sessions = Sessions::default();
    let p = Provider::default();
    let mut held = vec![];
    for index in 0..SESSION_LIMIT {
        let mut n = node();
        n.id = format!("held-{index}");
        held.push(
            sessions
                .entry(&ReadIdentity::new(&scope(), &n).unwrap())
                .unwrap()
                .unwrap(),
        );
    }
    read(&sessions, &p, &scope(), &node()).await.unwrap();
    assert_eq!(p.counts.opens.load(Ordering::SeqCst), 0);
    assert_eq!(p.counts.legacy.load(Ordering::SeqCst), 1);
    assert_eq!(sessions.entries.lock().unwrap().len(), SESSION_LIMIT);
    drop(held);
    read(&sessions, &p, &scope(), &node()).await.unwrap();
    assert_eq!(p.counts.opens.load(Ordering::SeqCst), 1);
    assert_eq!(sessions.entries.lock().unwrap().len(), SESSION_LIMIT);
    for (_, used) in sessions.entries.lock().unwrap().values_mut() {
        *used = Instant::now() - IDLE_LIFETIME;
    }
    read(&sessions, &p, &scope(), &node()).await.unwrap();
    assert_eq!(sessions.entries.lock().unwrap().len(), 1);
    assert_eq!(p.counts.opens.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn cancelled_creator_releases_initialization_without_blocking_other_files() {
    let sessions = Sessions::default();
    let p = Provider {
        held_item: Some("file".into()),
        ..Default::default()
    };
    let cancel = CancellationToken::new();
    let (s, n) = (scope(), node());
    let pending = sessions.read(&p, &s, &n, 0, 32, &cancel);
    tokio::pin!(pending);
    tokio::select! {
        result = &mut pending => panic!("unexpected {result:?}"),
        _ = p.entered.notified() => (),
    }
    let mut other = node();
    other.id = "other".into();
    tokio::time::timeout(
        Duration::from_secs(1),
        read(&sessions, &p, &scope(), &other),
    )
    .await
    .unwrap()
    .unwrap();
    cancel.cancel();
    assert!(matches!(pending.await, Err(ProviderError::Cancelled)));
    p.release.notify_one();
    tokio::time::timeout(Duration::from_secs(1), read(&sessions, &p, &s, &n))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(p.counts.opens.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn disk_blocks_survive_restart_without_transport_and_later_blocks_reuse_session() {
    use super::super::{BLOCK_SIZE, ContentCache};
    let temp = tempfile::tempdir().unwrap();
    let cache_path = temp.path().join("cache");
    let db = temp.path().join("db");
    let p = Provider::default();
    let (s, n, cancel) = (scope(), node(), CancellationToken::new());
    let cache = ContentCache::new(cache_path.clone(), db.clone(), 32 * 1024 * 1024).unwrap();
    for offset in [0, BLOCK_SIZE as u64, 2 * BLOCK_SIZE as u64] {
        assert_eq!(
            cache.read(&p, &s, &n, offset, 32, &cancel).await.unwrap(),
            vec![b'A'; 32]
        );
    }
    assert_eq!(p.counts.opens.load(Ordering::SeqCst), 1);
    assert_eq!(p.counts.reads.load(Ordering::SeqCst), 3);
    drop(cache);
    let cache = ContentCache::new(cache_path, db, 32 * 1024 * 1024).unwrap();
    for offset in [0, BLOCK_SIZE as u64, 2 * BLOCK_SIZE as u64] {
        assert_eq!(
            cache.read(&p, &s, &n, offset, 32, &cancel).await.unwrap(),
            vec![b'A'; 32]
        );
    }
    assert_eq!(p.counts.reads.load(Ordering::SeqCst), 3);
    assert_eq!(p.counts.opens.load(Ordering::SeqCst), 1);
}
