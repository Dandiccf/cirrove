#![allow(clippy::unwrap_used)]
use super::*;
use async_trait::async_trait;
use cirrove_core::reads::ReadWindowSink;
use cirrove_core::{ChangePage, Cursor, DirectoryPage, MetadataProvider, NodeKind};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::sync::Notify;

#[derive(Default)]
struct Counts {
    opens: AtomicUsize,
    reads: AtomicUsize,
    legacy: AtomicUsize,
    windows: AtomicUsize,
    transferred: AtomicUsize,
    window_limit: AtomicUsize,
    fail_window: AtomicBool,
    hold_window: AtomicBool,
    window_entered: Notify,
    window_release: Notify,
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
    fn window_limit(&self) -> u32 {
        self.counts.window_limit.load(Ordering::SeqCst) as u32
    }
    async fn read_window(
        &self,
        offset: u64,
        length: u32,
        sink: &mut dyn ReadWindowSink,
        cancel: &CancellationToken,
    ) -> Result<(), ProviderError> {
        self.counts.windows.fetch_add(1, Ordering::SeqCst);
        let chunk = [if self.identity.revision == "v2" {
            b'B'
        } else {
            b'A'
        }; 64 * 1024];
        let mut left = self.identity.size.saturating_sub(offset).min(length as u64);
        while left > 0 {
            let n = left.min(chunk.len() as u64) as usize;
            sink.write_chunk(&chunk[..n]).await?;
            self.counts.transferred.fetch_add(n, Ordering::SeqCst);
            left -= n as u64;
            if self.counts.hold_window.load(Ordering::SeqCst) {
                self.counts.window_entered.notify_one();
                tokio::select! { biased;
                    _=cancel.cancelled()=>return Err(ProviderError::Cancelled),
                    _=self.counts.window_release.notified()=>(),
                }
            }
        }
        if self.counts.fail_window.load(Ordering::SeqCst) {
            return Err(ProviderError::VersionChanged);
        }
        Ok(())
    }
    async fn read_range(
        &self,
        offset: u64,
        length: u32,
        _: &CancellationToken,
    ) -> Result<Vec<u8>, ProviderError> {
        self.counts.reads.fetch_add(1, Ordering::SeqCst);
        self.counts.transferred.fetch_add(
            self.identity.size.saturating_sub(offset).min(length as u64) as usize,
            Ordering::SeqCst,
        );
        Ok(vec![
            if self.identity.revision == "v2" {
                b'B'
            } else {
                b'A'
            };
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

fn window_provider() -> Provider {
    let p = Provider::default();
    p.counts
        .window_limit
        .store(64 * 1024 * 1024, Ordering::SeqCst);
    p
}
fn window_cache(temp: &tempfile::TempDir, quota: u64) -> super::super::ContentCache {
    super::super::ContentCache::new(temp.path().join("cache"), temp.path().join("db"), quota)
        .unwrap()
}
#[tokio::test]
async fn first_small_sparse_and_low_quota_reads_do_not_prefetch() {
    for quota in [16 * 1024 * 1024, 512 * 1024 * 1024] {
        let temp = tempfile::tempdir().unwrap();
        let p = window_provider();
        let cache = window_cache(&temp, quota);
        let (s, mut n, cancel) = (scope(), node(), CancellationToken::new());
        n.size = 256 * 1024 * 1024;
        for offset in [0, 128 * 1024 * 1024, 32 * 1024 * 1024, 64 * 1024 * 1024] {
            cache.read(&p, &s, &n, offset, 32, &cancel).await.unwrap();
        }
        assert_eq!(p.counts.windows.load(Ordering::SeqCst), 0);
        assert_eq!(
            p.counts.transferred.load(Ordering::SeqCst),
            4 * BLOCK_SIZE as usize
        );
        n.id = "small".into();
        n.size = 3_100_000;
        cache.read(&p, &s, &n, 0, 32, &cancel).await.unwrap();
        assert_eq!(p.counts.windows.load(Ordering::SeqCst), 0);
        if quota == 16 * 1024 * 1024 {
            assert_eq!(cache.window_stats().staging_budget_bytes, 0);
        }
    }
}

#[tokio::test]
async fn failed_window_validation_publishes_no_blocks_and_releases_staging() {
    let temp = tempfile::tempdir().unwrap();
    let p = window_provider();
    let cache = window_cache(&temp, 512 * 1024 * 1024);
    let (s, n, cancel) = (scope(), node(), CancellationToken::new());
    cache.read(&p, &s, &n, 0, 32, &cancel).await.unwrap();
    p.counts.fail_window.store(true, Ordering::SeqCst);
    assert!(matches!(
        cache.read(&p, &s, &n, BLOCK_SIZE as u64, 32, &cancel).await,
        Err(ProviderError::VersionChanged)
    ));
    assert_eq!(cache.window_stats().staging_reserved_bytes, 0);
    assert_eq!(cache.window_stats().validated_windows, 0);
    assert_eq!(
        std::fs::read_dir(temp.path().join("cache"))
            .unwrap()
            .count(),
        1
    );
    // A different content revision never sees the rejected window.
    p.counts.fail_window.store(false, Ordering::SeqCst);
    let mut n = n;
    n.content_version = Some("v2".into());
    assert_eq!(
        cache
            .read(&p, &s, &n, BLOCK_SIZE as u64, 32, &cancel)
            .await
            .unwrap(),
        vec![b'B'; 32]
    );
}

#[tokio::test]
async fn overlapping_reads_share_one_window_and_unrelated_files_keep_working() {
    let temp = tempfile::tempdir().unwrap();
    let p = Arc::new(window_provider());
    let cache = Arc::new(window_cache(&temp, 512 * 1024 * 1024));
    let (s, n, cancel) = (scope(), node(), CancellationToken::new());
    cache
        .read(p.as_ref(), &s, &n, 0, 32, &cancel)
        .await
        .unwrap();
    p.counts.hold_window.store(true, Ordering::SeqCst);
    let first = {
        let (p, c, s, n) = (p.clone(), cache.clone(), s.clone(), n.clone());
        tokio::spawn(async move {
            c.read(
                p.as_ref(),
                &s,
                &n,
                BLOCK_SIZE as u64,
                32,
                &CancellationToken::new(),
            )
            .await
        })
    };
    tokio::time::timeout(Duration::from_secs(3), p.counts.window_entered.notified())
        .await
        .unwrap();
    let second = {
        let (p, c, s, n) = (p.clone(), cache.clone(), s.clone(), n.clone());
        tokio::spawn(async move {
            c.read(
                p.as_ref(),
                &s,
                &n,
                2 * BLOCK_SIZE as u64,
                32,
                &CancellationToken::new(),
            )
            .await
        })
    };
    tokio::task::yield_now().await;
    assert!(!first.is_finished() && !second.is_finished());
    let mut other = n.clone();
    other.id = "other".into();
    tokio::time::timeout(
        Duration::from_secs(1),
        cache.read(p.as_ref(), &s, &other, 0, 32, &cancel),
    )
    .await
    .unwrap()
    .unwrap();
    p.counts.hold_window.store(false, Ordering::SeqCst);
    p.counts.window_release.notify_one();
    assert_eq!(first.await.unwrap().unwrap(), vec![b'A'; 32]);
    assert_eq!(second.await.unwrap().unwrap(), vec![b'A'; 32]);
    assert_eq!(p.counts.windows.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancellation_discards_partial_window_then_retries_small() {
    let temp = tempfile::tempdir().unwrap();
    let p = Arc::new(window_provider());
    let cache = Arc::new(window_cache(&temp, 512 * 1024 * 1024));
    let (s, n, cancel) = (scope(), node(), CancellationToken::new());
    cache
        .read(p.as_ref(), &s, &n, 0, 32, &cancel)
        .await
        .unwrap();
    p.counts.hold_window.store(true, Ordering::SeqCst);
    let task = {
        let (p, c, s, n, k) = (
            p.clone(),
            cache.clone(),
            s.clone(),
            n.clone(),
            cancel.clone(),
        );
        tokio::spawn(async move { c.read(p.as_ref(), &s, &n, BLOCK_SIZE as u64, 32, &k).await })
    };
    tokio::time::timeout(Duration::from_secs(3), p.counts.window_entered.notified())
        .await
        .unwrap();
    cancel.cancel();
    assert!(matches!(task.await.unwrap(), Err(ProviderError::Cancelled)));
    assert_eq!(cache.window_stats().staging_reserved_bytes, 0);
    assert_eq!(
        std::fs::read_dir(temp.path().join("cache"))
            .unwrap()
            .count(),
        1
    );
    p.counts.hold_window.store(false, Ordering::SeqCst);
    cache
        .read(
            p.as_ref(),
            &s,
            &n,
            BLOCK_SIZE as u64,
            32,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(p.counts.windows.load(Ordering::SeqCst), 1);
    assert_eq!(p.counts.reads.load(Ordering::SeqCst), 2);
}

#[test]
fn slow_transfers_limit_growth_and_abandoned_windows_reset_sequential_prediction() {
    let entry = Entry::new();
    entry.progressed(0, BLOCK_SIZE).unwrap();
    entry
        .transferred(BLOCK_SIZE, Duration::from_secs(10))
        .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let staging = Staging::new(temp.path().to_path_buf(), 512 * 1024 * 1024);
    assert!(
        entry
            .window(
                BLOCK_SIZE as u64,
                BLOCK_SIZE,
                64 * 1024 * 1024,
                64 * 1024 * 1024,
                &staging
            )
            .unwrap()
            .is_none()
    );
    entry
        .transferred(BLOCK_SIZE, Duration::from_millis(10))
        .unwrap();
    let slot = entry
        .window(
            BLOCK_SIZE as u64,
            BLOCK_SIZE,
            64 * 1024 * 1024,
            64 * 1024 * 1024,
            &staging,
        )
        .unwrap()
        .unwrap();
    assert_eq!(slot.length, 2 * BLOCK_SIZE);
    drop(slot);
    assert!(
        entry
            .window(
                BLOCK_SIZE as u64,
                BLOCK_SIZE,
                64 * 1024 * 1024,
                64 * 1024 * 1024,
                &staging
            )
            .unwrap()
            .is_none()
    );
    assert_eq!(staging.stats().staging_reserved_bytes, 0);
}

#[tokio::test]
async fn saturated_staging_falls_back_without_waiting_for_another_files_window() {
    let temp = tempfile::tempdir().unwrap();
    let p = Arc::new(window_provider());
    let cache = Arc::new(window_cache(&temp, 32 * 1024 * 1024));
    let (s, n, cancel) = (scope(), node(), CancellationToken::new());
    let mut other = n.clone();
    other.id = "other".into();
    cache
        .read(p.as_ref(), &s, &n, 0, 32, &cancel)
        .await
        .unwrap();
    cache
        .read(p.as_ref(), &s, &other, 0, 32, &cancel)
        .await
        .unwrap();
    p.counts.hold_window.store(true, Ordering::SeqCst);
    let task = {
        let (p, c, s, n) = (p.clone(), cache.clone(), s.clone(), n.clone());
        tokio::spawn(async move {
            c.read(
                p.as_ref(),
                &s,
                &n,
                BLOCK_SIZE as u64,
                32,
                &CancellationToken::new(),
            )
            .await
        })
    };
    tokio::time::timeout(Duration::from_secs(3), p.counts.window_entered.notified())
        .await
        .unwrap();
    assert_eq!(cache.window_stats().staging_reserved_bytes, 2 * BLOCK_SIZE);
    tokio::time::timeout(
        Duration::from_secs(1),
        cache.read(p.as_ref(), &s, &other, BLOCK_SIZE as u64, 32, &cancel),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(p.counts.windows.load(Ordering::SeqCst), 1);
    p.counts.hold_window.store(false, Ordering::SeqCst);
    p.counts.window_release.notify_one();
    task.await.unwrap().unwrap();
}
