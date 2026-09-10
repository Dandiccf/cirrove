//! Real byte and namespace checks against a deterministic provider, with no cloud credentials.
#![allow(clippy::unwrap_used)]
use async_trait::async_trait;
use cirrove_auth::{AppRegistration, Identity};
use cirrove_core::*;
use cirrove_onedrive::DriveInfo;
use cirrove_service::{
    accounts::Account,
    content::{BLOCK_SIZE, ContentCache},
    engine::Engine,
    filesystem::CloudFs,
};
use cirrove_store::{BlockIndex, Store};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::RwLock;

struct Fixture {
    nodes: RwLock<HashMap<(String, String), Node>>,
    reads: AtomicU64,
    offline: AtomicBool,
    stall: AtomicBool,
    delay_ms: AtomicU64,
    push: AtomicBool,
    hints: RwLock<HashMap<String, notifications::ChangeHintSender>>,
    change_calls: AtomicU64,
    hold_change: AtomicBool,
    change_entered: tokio::sync::Notify,
    release_change: tokio::sync::Notify,
    throttle_change: AtomicBool,
    disconnect: tokio::sync::Notify,
    subscriptions: AtomicU64,
    directory_calls: AtomicU64,
    stall_root_directory: AtomicBool,
    throttle_directory: AtomicBool,
    hold_observation: AtomicBool,
    observation_entered: tokio::sync::Notify,
    release_observation: tokio::sync::Notify,
}
fn file(id: &str, parent: Option<&str>, kind: NodeKind, size: u64) -> Node {
    Node {
        id: id.into(),
        name: id.into(),
        parent_id: parent.map(str::to_owned),
        kind,
        size,
        modified_unix: 1_700_000_000,
        etag: Some("version-1".into()),
        content_version: None,
        target: None,
    }
}
fn account(path: std::path::PathBuf) -> Account {
    Account {
        id: "00000000-0000-4000-8000-000000000001".into(),
        label: "fixture".into(),
        registration: AppRegistration {
            client_id: "00000000-0000-4000-8000-000000000002".into(),
            authority: "common".into(),
        },
        identity: Identity {
            tenant_id: "00000000-0000-4000-8000-000000000003".into(),
            subject: "synthetic".into(),
            username: "fixture@example.invalid".into(),
            graph_user_id: "user".into(),
            display_name: "Synthetic fixture".into(),
        },
        credential_id: "00000000-0000-4000-8000-000000000004".into(),
        access: cirrove_auth::AccessMode::ReadOnly,
        drive: DriveInfo {
            id: "home".into(),
            name: "Fixture".into(),
            drive_type: "business".into(),
            web_url: "https://example.invalid".into(),
        },
        root_id: "root".into(),
        mount_path: path,
        enabled: true,
        poll_seconds: 30,
        cache_bytes: 16 * 1024 * 1024,
    }
}
impl Fixture {
    fn new() -> Arc<Self> {
        let mut nodes = HashMap::new();
        for node in [
            file("root", None, NodeKind::Folder, 0),
            file("small.txt", Some("root"), NodeKind::File, 3_100_000),
            file(
                "large.bin",
                Some("root"),
                NodeKind::File,
                3 * 1024 * 1024 * 1024,
            ),
            file("folder", Some("root"), NodeKind::Folder, 0),
            file("deep.txt", Some("folder"), NodeKind::File, 17),
        ] {
            nodes.insert(("home".into(), node.id.clone()), node);
        }
        for id in ["Documents", "Documents-again"] {
            let mut link = file(id, Some("root"), NodeKind::Shortcut, 0);
            link.target = Some(Box::new(RemoteRef {
                collection: "library".into(),
                item: "shared".into(),
                kind: Some(NodeKind::Folder),
            }));
            nodes.insert(("home".into(), link.id.clone()), link);
        }
        for node in [
            file("shared", None, NodeKind::Folder, 0),
            file("shared.txt", Some("shared"), NodeKind::File, 29),
        ] {
            nodes.insert(("library".into(), node.id.clone()), node);
        }
        Arc::new(Self {
            nodes: RwLock::new(nodes),
            reads: AtomicU64::new(0),
            offline: AtomicBool::new(false),
            stall: AtomicBool::new(false),
            delay_ms: AtomicU64::new(0),
            push: AtomicBool::new(false),
            hints: RwLock::new(HashMap::new()),
            change_calls: AtomicU64::new(0),
            hold_change: AtomicBool::new(false),
            change_entered: tokio::sync::Notify::new(),
            release_change: tokio::sync::Notify::new(),
            throttle_change: AtomicBool::new(false),
            disconnect: tokio::sync::Notify::new(),
            subscriptions: AtomicU64::new(0),
            directory_calls: AtomicU64::new(0),
            stall_root_directory: AtomicBool::new(false),
            throttle_directory: AtomicBool::new(false),
            hold_observation: AtomicBool::new(false),
            observation_entered: tokio::sync::Notify::new(),
            release_observation: tokio::sync::Notify::new(),
        })
    }
    fn online(&self) -> Result<(), ProviderError> {
        if self.offline.load(Ordering::SeqCst) {
            Err(ProviderError::Unavailable)
        } else {
            Ok(())
        }
    }
}
#[async_trait]
impl MetadataProvider for Fixture {
    fn provider_id(&self) -> &'static str {
        "fixture"
    }
    async fn watch_changes(
        &self,
        scope: &Scope,
        hints: notifications::ChangeHintSender,
        cancel: &CancellationToken,
    ) -> Result<notifications::WatchEnd, ProviderError> {
        if !self.push.load(Ordering::SeqCst) {
            return Ok(notifications::WatchEnd::Unsupported);
        }
        self.subscriptions.fetch_add(1, Ordering::SeqCst);
        self.hints
            .write()
            .await
            .insert(scope.collection.clone(), hints.clone());
        hints.connected();
        tokio::select! {biased;
            _=cancel.cancelled()=>Err(ProviderError::Cancelled),
            _=self.disconnect.notified()=>Err(ProviderError::Unavailable),
        }
    }
    async fn changes(
        &self,
        scope: &Scope,
        _: Option<&Cursor>,
        cancel: &CancellationToken,
    ) -> Result<ChangePage, ProviderError> {
        self.change_calls.fetch_add(1, Ordering::SeqCst);
        if self.throttle_change.swap(false, Ordering::SeqCst) {
            return Err(ProviderError::Throttled(Duration::from_secs(1)));
        }
        self.online()?;
        let page = ChangePage {
            changes: self
                .nodes
                .read()
                .await
                .iter()
                .filter(|((drive, _), _)| drive == &scope.collection)
                .map(|(_, node)| Change::Upsert(node.clone()))
                .collect(),
            checkpoint: Checkpoint::Complete(Cursor("checkpoint".into())),
        };
        if self.hold_change.swap(false, Ordering::SeqCst) {
            self.change_entered.notify_one();
            tokio::select! {biased;
                _=cancel.cancelled()=>return Err(ProviderError::Cancelled),
                _=self.release_change.notified()=>(),
            }
        }
        Ok(page)
    }
}
#[async_trait]
impl ReadProvider for Fixture {
    async fn node(
        &self,
        scope: &Scope,
        id: &str,
        cancel: &CancellationToken,
    ) -> Result<Node, ProviderError> {
        self.online()?;
        let node = self
            .nodes
            .read()
            .await
            .get(&(scope.collection.clone(), id.into()))
            .cloned()
            .ok_or(ProviderError::NotFound);
        self.hold_observation(cancel).await?;
        node
    }
    async fn children(
        &self,
        scope: &Scope,
        parent: &str,
        _: Option<&Cursor>,
        cancel: &CancellationToken,
    ) -> Result<DirectoryPage, ProviderError> {
        self.directory_calls.fetch_add(1, Ordering::SeqCst);
        if self.throttle_directory.swap(false, Ordering::SeqCst) {
            return Err(ProviderError::Throttled(Duration::from_secs(20)));
        }
        if parent == "root" && self.stall_root_directory.load(Ordering::SeqCst) {
            cancel.cancelled().await;
            return Err(ProviderError::Cancelled);
        }
        self.online()?;
        let page = DirectoryPage {
            nodes: self
                .nodes
                .read()
                .await
                .iter()
                .filter(|((drive, _), node)| {
                    drive == &scope.collection && node.parent_id.as_deref() == Some(parent)
                })
                .map(|(_, node)| node.clone())
                .collect(),
            next: None,
        };
        self.hold_observation(cancel).await?;
        Ok(page)
    }
    async fn read_range(
        &self,
        scope: &Scope,
        node: &Node,
        offset: u64,
        length: u32,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>, ProviderError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.stall.load(Ordering::SeqCst) {
            cancel.cancelled().await;
            return Err(ProviderError::Cancelled);
        }
        tokio::select! {_=cancel.cancelled()=>return Err(ProviderError::Cancelled),_=tokio::time::sleep(Duration::from_millis(self.delay_ms.load(Ordering::SeqCst)))=>()}
        self.online()?;
        let current = self.node(scope, &node.id, cancel).await?;
        if node.content_revision() != current.content_revision() || node.size != current.size {
            return Err(ProviderError::VersionChanged);
        }
        let count = node.size.saturating_sub(offset).min(length as u64);
        let revision_offset = if node.content_version.as_deref() == Some("content-2") {
            17
        } else {
            0
        };
        Ok((offset..offset + count)
            .map(|position| ((position + revision_offset) % 251) as u8)
            .collect())
    }
}
fn assert_bytes(bytes: &[u8], offset: u64) {
    assert!(
        bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| *byte == ((offset + index as u64) % 251) as u8)
    );
}
async fn ready(engine: &Arc<Engine>) {
    let mut observed = Vec::new();
    let waited = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let feeds = engine.health().await;
            if feeds.len() == 2 && feeds.iter().all(|feed| feed.state == "ready") {
                return;
            }
            observed = feeds;
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    // The bound stays at three seconds; a timeout must say which feeds were
    // missing and in which state, instead of an anonymous Elapsed.
    assert!(
        waited.is_ok(),
        "feeds were not ready within three seconds; last observed: {observed:?}"
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn coalesced_large_range_reads_survive_restart_and_offline() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let provider = Fixture::new();
    provider.delay_ms.store(50, Ordering::SeqCst);
    let engine = Engine::new(
        account(temp.path().join("mount")),
        provider.clone(),
        state.clone(),
    )
    .await
    .unwrap();
    let scope = engine.scope("home");
    let node = provider
        .node(&scope, "large.bin", &engine.cancel)
        .await
        .unwrap();
    let offset = 2 * 1024 * 1024 * 1024 + 197;
    let mut jobs = tokio::task::JoinSet::new();
    for _ in 0..32 {
        let engine = engine.clone();
        let scope = scope.clone();
        let node = node.clone();
        jobs.spawn(async move {
            engine
                .cache
                .read(
                    engine.provider.as_ref(),
                    &scope,
                    &node,
                    offset,
                    8192,
                    &engine.cancel,
                )
                .await
                .unwrap()
        });
    }
    while let Some(result) = jobs.join_next().await {
        let bytes = result.unwrap();
        assert_eq!(bytes.len(), 8192);
        assert_bytes(&bytes, offset);
    }
    assert_eq!(
        provider.reads.load(Ordering::SeqCst),
        1,
        "all readers should share one 4 MiB request"
    );
    engine.stop().await;
    drop(engine);
    provider.offline.store(true, Ordering::SeqCst);
    let restarted = Engine::new(account(temp.path().join("mount")), provider.clone(), state)
        .await
        .unwrap();
    let bytes = restarted
        .cache
        .read(
            provider.as_ref(),
            &scope,
            &node,
            offset,
            8192,
            &restarted.cancel,
        )
        .await
        .unwrap();
    assert_bytes(&bytes, offset);
    assert_eq!(
        provider.reads.load(Ordering::SeqCst),
        1,
        "restart should reuse the validated disk block"
    );
    restarted.stop().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cached_navigation_survives_stalled_reads_and_metadata_restart() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let provider = Fixture::new();
    let config = account(temp.path().join("mount"));
    let engine = Engine::new(config.clone(), provider.clone(), state.clone())
        .await
        .unwrap();
    engine.start().await.unwrap();
    ready(&engine).await;
    let scope = engine.scope("home");
    let node = engine.node(&scope, "small.txt").await.unwrap();
    provider.stall.store(true, Ordering::SeqCst);
    let download = {
        let e = engine.clone();
        let s = scope.clone();
        tokio::spawn(async move {
            e.cache
                .read(e.provider.as_ref(), &s, &node, 0, 4096, &e.cancel)
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(1), async {
        while provider.reads.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    provider.offline.store(true, Ordering::SeqCst);
    let nodes = tokio::time::timeout(
        Duration::from_millis(300),
        engine.children(&scope, "folder"),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(nodes[0].id, "deep.txt");
    tokio::time::timeout(Duration::from_secs(2), engine.stop())
        .await
        .unwrap();
    assert!(matches!(
        download.await.unwrap(),
        Err(ProviderError::Cancelled)
    ));
    drop(engine);
    let restarted = Engine::new(config, provider, state).await.unwrap();
    assert_eq!(
        restarted.children(&scope, "folder").await.unwrap()[0].id,
        "deep.txt"
    );
    restarted.stop().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_read_is_coalesced_without_a_retry_storm() {
    let temp = tempfile::tempdir().unwrap();
    let provider = Fixture::new();
    let engine = Engine::new(
        account(temp.path().join("mount")),
        provider.clone(),
        temp.path().join("state"),
    )
    .await
    .unwrap();
    let scope = engine.scope("home");
    let node = engine.node(&scope, "small.txt").await.unwrap();
    provider.offline.store(true, Ordering::SeqCst);
    let mut jobs = tokio::task::JoinSet::new();
    for _ in 0..32 {
        let e = engine.clone();
        let s = scope.clone();
        let n = node.clone();
        jobs.spawn(async move {
            e.cache
                .read(e.provider.as_ref(), &s, &n, 0, 4096, &e.cancel)
                .await
        });
    }
    while let Some(result) = jobs.join_next().await {
        assert!(matches!(result.unwrap(), Err(ProviderError::Unavailable)));
    }
    assert_eq!(provider.reads.load(Ordering::SeqCst), 1);
    engine.stop().await;
}

#[tokio::test]
async fn named_lookup_distinguishes_cold_unknown_from_cached_absence_and_survives_restart() {
    let temp = tempfile::tempdir().unwrap();
    let provider = Fixture::new();
    let config = account(temp.path().join("mount"));
    let state = temp.path().join("state");
    let engine = Engine::new(config.clone(), provider.clone(), state.clone())
        .await
        .unwrap();
    let scope = engine.scope("home");
    assert_eq!(
        engine.child(&scope, "root", "small.txt").await.unwrap().id,
        "small.txt"
    );
    let fetched = provider.directory_calls.load(Ordering::SeqCst);
    assert!(
        fetched > 0,
        "cold lookup did not fetch the unknown directory"
    );
    provider.offline.store(true, Ordering::SeqCst);
    for name in ["absent", "Small.txt"] {
        assert!(matches!(
            engine.child(&scope, "root", name).await,
            Err(ProviderError::NotFound)
        ));
    }
    assert_eq!(
        engine.child(&scope, "root", "small.txt").await.unwrap().id,
        "small.txt"
    );
    let mut other = scope.clone();
    other.account = "other-account".into();
    assert!(matches!(
        engine.child(&other, "root", "small.txt").await,
        Err(ProviderError::Protocol(_))
    ));
    assert_eq!(provider.directory_calls.load(Ordering::SeqCst), fetched);
    engine.stop().await;
    assert!(matches!(
        engine.child(&scope, "root", "small.txt").await,
        Err(ProviderError::Cancelled)
    ));
    drop(engine);
    let restarted = Engine::new(config, provider.clone(), state).await.unwrap();
    assert_eq!(
        restarted
            .child(&scope, "root", "small.txt")
            .await
            .unwrap()
            .id,
        "small.txt"
    );
    assert_eq!(provider.directory_calls.load(Ordering::SeqCst), fetched);
    restarted.stop().await;
}
#[tokio::test]
async fn corrupt_blocks_redownload_and_interrupted_publications_obey_quota() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("cache");
    let db = temp.path().join("db");
    let provider = Fixture::new();
    let scope = Scope {
        account: "a".into(),
        provider: "fixture".into(),
        collection: "home".into(),
    };
    let cancel = CancellationToken::new();
    let node = provider.node(&scope, "small.txt", &cancel).await.unwrap();
    let cache = ContentCache::new(path.clone(), db.clone(), BLOCK_SIZE as u64 + 32).unwrap();
    cache
        .read(provider.as_ref(), &scope, &node, 0, 512, &cancel)
        .await
        .unwrap();
    drop(cache);
    let block = std::fs::read_dir(&path)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    // Fixed vectors guard the existing disk key and binary checksum header.
    assert_eq!(
        block.file_name().unwrap(),
        "6213745a8b09a73cc9c94467e9e2eae2165add597056b63d69bacb125274ae46"
    );
    assert_eq!(
        hex::encode(&std::fs::read(&block).unwrap()[..32]),
        "cefde0f05239043b331c09c52247915eee1a4da545d16e5815d2e79b9e8e5572"
    );
    std::fs::write(&block, b"corruption").unwrap();
    let cache = ContentCache::new(path.clone(), db.clone(), BLOCK_SIZE as u64 + 32).unwrap();
    let bytes = cache
        .read(provider.as_ref(), &scope, &node, 0, 512, &cancel)
        .await
        .unwrap();
    assert_bytes(&bytes, 0);
    assert_eq!(provider.reads.load(Ordering::SeqCst), 2);
    drop(cache);
    let temporary = path.join(format!("{}.{}.tmp", "a".repeat(64), uuid::Uuid::new_v4()));
    std::fs::write(&temporary, "interrupted write").unwrap();
    let orphan = path.join("b".repeat(64));
    std::fs::write(&orphan, vec![0; BLOCK_SIZE as usize + 32]).unwrap();
    let _cache = ContentCache::new(path.clone(), db.clone(), BLOCK_SIZE as u64 + 32).unwrap();
    assert!(!temporary.exists());
    let total: u64 = std::fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().len())
        .sum();
    assert!(total <= BLOCK_SIZE as u64 + 32);
    assert_eq!(BlockIndex::open(db).unwrap().oldest().unwrap().len(), 1);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires kernel FUSE; retains application handles during service shutdown"]
async fn real_shutdown_with_open_handles_does_not_wait_for_applications() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount with spaces");
    let other_mount = temp.path().join("other mount");
    std::fs::create_dir(&mount).unwrap();
    std::fs::create_dir(&other_mount).unwrap();
    let provider = Fixture::new();
    let engine = Engine::new(
        account(mount.clone()),
        provider.clone(),
        temp.path().join("state"),
    )
    .await
    .unwrap();
    let other = Engine::new(
        account(other_mount.clone()),
        provider,
        temp.path().join("other state"),
    )
    .await
    .unwrap();
    engine.start().await.unwrap();
    other.start().await.unwrap();
    ready(&engine).await;
    ready(&other).await;
    let session = CloudFs::new(engine.clone()).unwrap().mount(&mount).unwrap();
    let other_session = CloudFs::new(other.clone())
        .unwrap()
        .mount(&other_mount)
        .unwrap();
    // File managers, previews and shells can retain both types of descriptor.
    // The second mount deliberately has the same account identity: cancellation
    // must belong to this particular kernel connection, not an account/path guess.
    let held_file = std::fs::File::open(mount.join("folder/deep.txt")).unwrap();
    let held_directory = std::fs::File::open(&mount).unwrap();
    engine.stop().await;
    let mut shutdown = tokio::task::spawn_blocking(move || session.umount_and_join());
    let completed = tokio::time::timeout(Duration::from_secs(2), &mut shutdown).await;
    let stopped_with_open_handles = completed.is_ok();
    if let Ok(result) = completed {
        result.unwrap().unwrap();
    }
    // Release only our own fixture handles even when the regression fails, so
    // the old blocking join can finish and the test reports instead of hanging.
    drop(held_file);
    drop(held_directory);
    if !stopped_with_open_handles {
        tokio::time::timeout(Duration::from_secs(3), shutdown)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    assert!(std::fs::read_dir(&mount).unwrap().next().is_none());
    let bytes = std::fs::read(other_mount.join("folder/deep.txt")).unwrap();
    assert_eq!(bytes.len(), 17);
    assert_bytes(&bytes, 0);
    other.stop().await;
    tokio::task::spawn_blocking(move || other_session.umount_and_join())
        .await
        .unwrap()
        .unwrap();
    assert!(
        stopped_with_open_handles,
        "shutdown waited for application-owned file/directory handles"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a working /dev/fuse and fusermount3; run explicitly"]
async fn real_fuse_reads_shortcuts_seek_readonly_restart_and_ejection() {
    use std::{
        io::{Read, Seek, SeekFrom},
        os::unix::fs::MetadataExt,
    };
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let state = temp.path().join("state");
    let config = account(mount.clone());
    let provider = Fixture::new();
    let engine = Engine::new(config.clone(), provider.clone(), state.clone())
        .await
        .unwrap();
    engine.start().await.unwrap();
    ready(&engine).await;
    let session = CloudFs::new(engine.clone()).unwrap().mount(&mount).unwrap();
    let path = mount.clone();
    let inode = tokio::time::timeout(
        Duration::from_secs(8),
        tokio::task::spawn_blocking(move || {
            let names: Vec<_> = std::fs::read_dir(&path)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            assert!(names.contains(&"Documents".into()));
            let bytes = std::fs::read(path.join("Documents/shared.txt")).unwrap();
            assert_eq!(bytes.len(), 29);
            assert_bytes(&bytes, 0);
            assert_ne!(
                std::fs::metadata(path.join("Documents/shared.txt"))
                    .unwrap()
                    .ino(),
                std::fs::metadata(path.join("Documents-again/shared.txt"))
                    .unwrap()
                    .ino()
            );
            assert_eq!(
                std::fs::read(path.join("folder/deep.txt")).unwrap().len(),
                17
            );
            let mut file = std::fs::File::open(path.join("large.bin")).unwrap();
            let offset = 2 * 1024 * 1024 * 1024 + 197;
            file.seek(SeekFrom::Start(offset)).unwrap();
            let mut bytes = [0u8; 8192];
            file.read_exact(&mut bytes).unwrap();
            assert_bytes(&bytes, offset);
            file.sync_all().unwrap();
            assert_eq!(
                std::fs::write(path.join("new.txt"), "blocked")
                    .unwrap_err()
                    .raw_os_error(),
                Some(libc::EROFS)
            );
            std::fs::metadata(path.join("large.bin")).unwrap().ino()
        }),
    )
    .await
    .unwrap()
    .unwrap();
    let attributes = tokio::process::Command::new("python3")
        .args(["-c", "import errno, os, sys\np = sys.argv[1]\nassert os.listxattr(p) == []\ntry:\n os.getxattr(p, 'user.cirrove.unavailable')\nexcept OSError as e:\n assert e.errno == errno.ENODATA, e\nelse:\n raise AssertionError('missing attribute was reported as present')"])
        .arg(mount.join("large.bin")).status().await.unwrap();
    assert!(attributes.success());
    // Active and queued application reads block, but folders retain capacity and
    // shutdown must release all readers without libc retrying EINTR forever.
    provider.stall.store(true, Ordering::SeqCst);
    let before = provider.reads.load(Ordering::SeqCst);
    let slow_path = mount.join("small.txt");
    let stalled = tokio::task::spawn_blocking(move || {
        let files = (0..96)
            .map(|_| std::fs::File::open(&slow_path).unwrap())
            .collect::<Vec<_>>();
        let barrier = Arc::new(std::sync::Barrier::new(files.len()));
        let readers = files
            .into_iter()
            .map(|mut file| {
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    let mut bytes = [0; 8192];
                    file.read_exact(&mut bytes)
                })
            })
            .collect::<Vec<_>>();
        readers
            .into_iter()
            .map(|reader| reader.join().unwrap())
            .collect::<Vec<_>>()
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while provider.reads.load(Ordering::SeqCst) == before {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let path = mount.join("folder");
    tokio::time::timeout(
        Duration::from_millis(500),
        tokio::task::spawn_blocking(move || std::fs::read_dir(path).unwrap().count()),
    )
    .await
    .unwrap()
    .unwrap();
    engine.stop().await;
    assert!(
        stalled
            .await
            .unwrap()
            .iter()
            .all(|result| result.as_ref().unwrap_err().raw_os_error() == Some(libc::ENODEV))
    );
    provider.stall.store(false, Ordering::SeqCst);
    tokio::task::spawn_blocking(move || session.umount_and_join())
        .await
        .unwrap()
        .unwrap();
    drop(engine);
    provider.offline.store(true, Ordering::SeqCst);
    let engine = Engine::new(config, provider.clone(), state).await.unwrap();
    let session = CloudFs::new(engine.clone()).unwrap().mount(&mount).unwrap();
    let path = mount.clone();
    tokio::time::timeout(
        Duration::from_secs(5),
        tokio::task::spawn_blocking(move || {
            assert_eq!(
                std::fs::metadata(path.join("large.bin")).unwrap().ino(),
                inode
            );
            let mut file = std::fs::File::open(path.join("large.bin")).unwrap();
            let offset = 2 * 1024 * 1024 * 1024 + 197;
            file.seek(SeekFrom::Start(offset)).unwrap();
            let mut bytes = [0; 8192];
            file.read_exact(&mut bytes).unwrap();
            assert_bytes(&bytes, offset);
        }),
    )
    .await
    .unwrap()
    .unwrap();
    let result = tokio::process::Command::new("fusermount3")
        .arg("-u")
        .arg(&mount)
        .status()
        .await
        .unwrap();
    assert!(result.success());
    // The external unmount ends the old kernel session; a new projection is independent.
    tokio::task::spawn_blocking(move || session.join())
        .await
        .unwrap()
        .unwrap();
    let session = CloudFs::new(engine.clone()).unwrap().mount(&mount).unwrap();
    let path = mount.clone();
    assert_eq!(
        tokio::task::spawn_blocking(move || std::fs::read(path.join("folder/deep.txt")))
            .await
            .unwrap()
            .unwrap()
            .len(),
        17
    );
    engine.stop().await;
    tokio::task::spawn_blocking(move || session.umount_and_join())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a working /dev/fuse and fusermount3; run explicitly"]
async fn real_manager_does_not_claim_another_accounts_mount() {
    use cirrove_service::{accounts::Settings, manager::Manager, private_dir};
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let provider = Fixture::new();
    let original = Engine::new(
        account(mount.clone()),
        provider.clone(),
        temp.path().join("original"),
    )
    .await
    .unwrap();
    original.start().await.unwrap();
    ready(&original).await;
    let session = CloudFs::new(original.clone())
        .unwrap()
        .mount(&mount)
        .unwrap();
    let state = temp.path().join("second");
    private_dir(&state).unwrap();
    let mut config = account(mount.clone());
    config.id = "00000000-0000-4000-8000-000000000005".into();
    std::fs::write(
        state.join("accounts.json"),
        serde_json::to_vec(&Settings {
            version: 1,
            accounts: vec![config],
        })
        .unwrap(),
    )
    .unwrap();
    let cancel = CancellationToken::new();
    let factory: cirrove_service::manager::ProviderFactory =
        Arc::new(move |_| Ok(provider.clone()));
    let (manager, worker) = Manager::start_with_provider(state, cancel.clone(), factory);
    tokio::time::timeout(Duration::from_secs(10), async {
        while manager.status.read().await.is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    {
        let status = manager.status.read().await;
        assert!(!status[0].mounted, "a foreign Cirrove session is not ours");
        assert_ne!(status[0].state, "ready");
    }
    assert_bytes(&std::fs::read(mount.join("small.txt")).unwrap(), 0);
    tokio::task::spawn_blocking(move || session.umount_and_join())
        .await
        .unwrap()
        .unwrap();
    original.stop().await;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if manager
                .status
                .read()
                .await
                .first()
                .is_some_and(|s| s.mounted && s.state == "ready")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_bytes(&std::fs::read(mount.join("small.txt")).unwrap(), 0);
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), worker)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a working /dev/fuse and fusermount3; run explicitly"]
async fn real_manager_remounts_enabled_drives_and_keeps_disabled_drives_unmounted() {
    use cirrove_service::{
        accounts::{Settings, set_enabled},
        manager::Manager,
        private_dir,
    };
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    private_dir(&state).unwrap();
    let mount = temp.path().join("mount");
    let config = account(mount.clone());
    let settings = Settings {
        version: 1,
        accounts: vec![config],
    };
    std::fs::write(
        state.join("accounts.json"),
        serde_json::to_vec(&settings).unwrap(),
    )
    .unwrap();
    let provider = Fixture::new();
    let factory: cirrove_service::manager::ProviderFactory =
        Arc::new(move |_| Ok(provider.clone()));
    let cancel = CancellationToken::new();
    let (manager, worker) =
        Manager::start_with_provider(state.clone(), cancel.clone(), factory.clone());
    async fn wait_state(manager: &Manager, expected: &str, mounted: bool) {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let statuses = manager.status.read().await;
                if statuses.first().is_some_and(|s| {
                    s.mounted == mounted && (expected.is_empty() || s.state == expected)
                }) {
                    break;
                }
                drop(statuses);
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .unwrap();
    }
    wait_state(&manager, "", true).await;
    // A newly mounted filesystem can be transiently busy. Require an ordinary
    // unmount within two seconds; never force/detach it or retry other errors.
    let eject_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let output = tokio::time::timeout_at(
            eject_deadline,
            tokio::process::Command::new("fusermount3")
                .env("LC_ALL", "C")
                .arg("-u")
                .arg(&mount)
                .kill_on_drop(true)
                .output(),
        )
        .await
        .expect("test mount stayed busy past the ejection deadline")
        .unwrap();
        if output.status.success() {
            break;
        }
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("Device or resource busy"), "{error}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Wait on the actual kernel state, not a stale status snapshot.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let mounts = tokio::fs::read_to_string("/proc/self/mountinfo")
                .await
                .unwrap();
            if mounts
                .lines()
                .any(|line| line.split_whitespace().nth(4) == mount.to_str())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    set_enabled(&state, "fixture", false).unwrap();
    wait_state(&manager, "disabled", false).await;
    assert!(std::fs::read_dir(&mount).unwrap().next().is_none());
    std::fs::write(mount.join("local.txt"), "do not obscure").unwrap();
    set_enabled(&state, "fixture", true).unwrap();
    wait_state(
        &manager,
        "mount unavailable; directory must be empty and unmounted",
        false,
    )
    .await;
    assert_eq!(
        std::fs::read_to_string(mount.join("local.txt")).unwrap(),
        "do not obscure"
    );
    std::fs::remove_file(mount.join("local.txt")).unwrap();
    wait_state(&manager, "", true).await;
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), worker)
        .await
        .unwrap()
        .unwrap();
    assert!(std::fs::read_dir(&mount).unwrap().next().is_none());
    let cancel = CancellationToken::new();
    let (manager, worker) = Manager::start_with_provider(state, cancel.clone(), factory);
    wait_state(&manager, "", true).await;
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), worker)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "subprocess fixture for real_manager_recovers_disconnected_mount_after_process_death"]
async fn abrupt_exit_mount_fixture() {
    let Some(root) = std::env::var_os("CIRROVE_CRASH_FIXTURE_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let fixture = Fixture::new();
    let cancel = CancellationToken::new();
    let mut terminate =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
    let (manager, worker) = cirrove_service::manager::Manager::start_with_provider(
        root.join("state"),
        cancel.clone(),
        Arc::new(move |_| Ok(fixture.clone())),
    );
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if manager
                .status
                .read()
                .await
                .first()
                .is_some_and(|status| status.mounted)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    std::fs::write(root.join("ready"), b"mounted").unwrap();
    terminate.recv().await;
    cancel.cancel();
    worker.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires kernel FUSE; SIGTERM only its own synthetic child manager"]
async fn real_manager_exits_with_client_handles_open() {
    use cirrove_service::{accounts::Settings, private_dir};
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let mount = temp.path().join("mount");
    private_dir(&state).unwrap();
    let config = account(mount.clone());
    std::fs::write(
        state.join("accounts.json"),
        serde_json::to_vec(&Settings {
            version: 1,
            accounts: vec![config.clone()],
        })
        .unwrap(),
    )
    .unwrap();
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "abrupt_exit_mount_fixture",
            "--ignored",
            "--nocapture",
        ])
        .env("CIRROVE_CRASH_FIXTURE_ROOT", temp.path())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        while !temp.path().join("ready").exists() {
            assert!(child.try_wait().unwrap().is_none());
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let held_file = std::fs::File::open(mount.join("folder/deep.txt")).unwrap();
    let held_directory = std::fs::File::open(&mount).unwrap();
    assert!(
        tokio::process::Command::new("kill")
            .args(["-TERM", &child.id().unwrap().to_string()])
            .status()
            .await
            .unwrap()
            .success()
    );
    let stopped = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
    drop(held_file);
    drop(held_directory);
    let exited_without_clients_closing = stopped.is_ok();
    match stopped {
        Ok(exit) => assert!(exit.unwrap().success()),
        Err(_) => {
            child.kill().await.unwrap();
            child.wait().await.unwrap();
            let _ = tokio::process::Command::new("fusermount3")
                .args(["-u", "-z", "--"])
                .arg(&mount)
                .status()
                .await;
        }
    }
    assert!(
        exited_without_clients_closing,
        "manager process did not exit with open client handles"
    );
    assert!(std::fs::read_dir(&mount).unwrap().next().is_none());
    // A new process/account owner can use the same state after graceful exit.
    let recovered = Engine::new(config, Fixture::new(), state).await.unwrap();
    recovered.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires kernel FUSE; kills only its own synthetic subprocess"]
async fn real_manager_recovers_disconnected_mount_after_process_death() {
    use cirrove_service::{accounts::Settings, manager::Manager, private_dir};
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    private_dir(&state).unwrap();
    let mount = temp.path().join("mount");
    // Also detach this test's mount on assertion failure before TempDir cleanup.
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::process::Command::new("fusermount3")
                .args(["-u", "-z", "--"])
                .arg(&self.0)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
    let _cleanup = Cleanup(mount.clone());
    let settings = Settings {
        version: 1,
        accounts: vec![account(mount.clone())],
    };
    std::fs::write(
        state.join("accounts.json"),
        serde_json::to_vec(&settings).unwrap(),
    )
    .unwrap();
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "abrupt_exit_mount_fixture",
            "--ignored",
            "--nocapture",
        ])
        .env("CIRROVE_CRASH_FIXTURE_ROOT", temp.path())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        while !temp.path().join("ready").exists() {
            assert!(
                child.try_wait().unwrap().is_none(),
                "mount fixture exited before readiness"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    let mounts = std::fs::read_to_string("/proc/self/mountinfo").unwrap();
    assert!(
        mounts
            .lines()
            .any(|line| line.split_whitespace().nth(4) == mount.to_str()),
        "fixture must leave a disconnected FUSE mount"
    );
    let cancel = CancellationToken::new();
    let fixture = Fixture::new();
    let (manager, worker) = Manager::start_with_provider(
        state,
        cancel.clone(),
        Arc::new(move |_| Ok(fixture.clone())),
    );
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if manager
                .status
                .read()
                .await
                .first()
                .is_some_and(|status| status.mounted && status.state == "ready")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let path = mount.join("folder/deep.txt");
    let bytes = tokio::task::spawn_blocking(move || std::fs::read(path))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bytes.len(), 17);
    assert_bytes(&bytes, 0);
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(10), worker)
        .await
        .unwrap()
        .unwrap();
    assert!(std::fs::read_dir(mount).unwrap().next().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "local FUSE performance fixture; run explicitly with --nocapture"]
async fn synthetic_latency_report() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let provider = Fixture::new();
    {
        let mut nodes = provider.nodes.write().await;
        for i in 0..500 {
            let name = format!("entry-{i:04}");
            nodes.insert(
                ("home".into(), name.clone()),
                file(&name, Some("root"), NodeKind::File, 10),
            );
        }
        for i in 0..20 {
            let name = format!("image-{i:02}.bin");
            nodes.insert(
                ("home".into(), name.clone()),
                file(&name, Some("root"), NodeKind::File, 3_100_000),
            );
        }
    }
    provider.delay_ms.store(50, Ordering::SeqCst);
    let engine = Engine::new(
        account(mount.clone()),
        provider.clone(),
        temp.path().join("state"),
    )
    .await
    .unwrap();
    engine.start().await.unwrap();
    ready(&engine).await;
    let session = CloudFs::new(engine.clone()).unwrap().mount(&mount).unwrap();
    let report=tokio::task::spawn_blocking(move||{
        let mut cold=vec![];let mut warm=vec![];let mut listing=vec![];
        for i in 0..20 {
            let start=std::time::Instant::now();let names=std::fs::read_dir(&mount).unwrap().count();assert_eq!(names,525);listing.push(start.elapsed().as_secs_f64()*1000.0);
            let path=mount.join(format!("image-{i:02}.bin"));
            let start=std::time::Instant::now();let bytes=std::fs::read(&path).unwrap();cold.push(start.elapsed().as_secs_f64()*1000.0);assert_eq!(bytes.len(),3_100_000);assert_bytes(&bytes,0);
            let start=std::time::Instant::now();let bytes=std::fs::read(&path).unwrap();warm.push(start.elapsed().as_secs_f64()*1000.0);assert_eq!(bytes.len(),3_100_000);assert_bytes(&bytes,0);
        }
        let stats=|mut samples:Vec<f64>|{samples.sort_by(f64::total_cmp);serde_json::json!({"samples":samples.len(),"p50_ms":samples[samples.len().div_ceil(2)-1],"p95_ms":samples[(samples.len()*95).div_ceil(100)-1]})};
        let process=std::fs::read_to_string("/proc/self/status").unwrap();let peak=process.lines().find(|line|line.starts_with("VmHWM:")).unwrap().split_whitespace().nth(1).unwrap().parse::<u64>().unwrap();
        serde_json::json!({"fixture":"synthetic FUSE; 50 ms provider delay; no internet or OAuth", "build":"debug", "bytes_per_file":3_100_000,"directory_entries":525,"cold_read":stats(cold),"warm_read":stats(warm),"directory_listing":stats(listing),"process_peak_rss_kib":peak})
    }).await.unwrap();
    assert_eq!(provider.reads.load(Ordering::SeqCst), 20);
    println!(
        "CIRROVE_BENCHMARK {}",
        serde_json::json!({"measurements":report,"provider_range_calls":20,"warm_provider_range_calls":0})
    );
    engine.stop().await;
    tokio::task::spawn_blocking(move || session.umount_and_join())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a working /dev/fuse and fusermount3; run explicitly"]
async fn real_memory_mapped_reads_are_supported() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let provider = Fixture::new();
    let engine = Engine::new(account(mount.clone()), provider, temp.path().join("state"))
        .await
        .unwrap();
    engine.start().await.unwrap();
    ready(&engine).await;
    let session = CloudFs::new(engine.clone()).unwrap().mount(&mount).unwrap();
    let output = tokio::process::Command::new("python3")
        .args([
            "-c",
            r#"
import mmap,sys
from pathlib import Path
with open(sys.argv[1], 'rb') as f:
    for access in (mmap.ACCESS_READ,mmap.ACCESS_COPY):
        with mmap.mmap(f.fileno(),0,access=access) as view:
            assert view[1024:2048] == bytes(i % 251 for i in range(1024,2048))
            if access == mmap.ACCESS_COPY:
                view[0:4] = b'test'
    with mmap.mmap(f.fileno(),0,access=mmap.ACCESS_READ) as view:
        assert view[0:4] == bytes(range(4))
with open(Path(sys.argv[1]).with_name('large.bin'),'rb') as f:
    with mmap.mmap(f.fileno(),0,access=mmap.ACCESS_READ) as view:
        offset=2*1024*1024*1024+197
        assert view[offset:offset+16384] == bytes(i % 251 for i in range(offset,offset+16384))
# Linux rusage can retain the parent's pre-exec high-water mark. VmHWM
# belongs to this program's address space, which is the memory we are testing.
peak_kib=int(next(line.split()[1] for line in Path('/proc/self/status').read_text().splitlines() if line.startswith('VmHWM:')))
assert peak_kib < 128*1024, f'mapped application peak RSS: {peak_kib} KiB'
"#,
        ])
        .arg(mount.join("small.txt"))
        .kill_on_drop(true)
        .output()
        .await
        .unwrap();
    engine.stop().await;
    tokio::task::spawn_blocking(move || session.umount_and_join())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a working /dev/fuse and fusermount3; run explicitly"]
async fn real_thumbnail_burst_queues_reads_without_blocking_cached_navigation() {
    use anyhow::{Context, ensure};
    use std::io::Read;
    const READERS: usize = 96;
    // This exercises admission and responsive navigation, not a disk-throughput
    // SLA for CI hosts: 96 tiny reads populate almost 300 MB of durable blocks.
    // Allow the service's bounded queue and provider phases to finish while
    // retaining the independent 500 ms deadline on every directory request.
    const BURST_TIMEOUT: Duration = Duration::from_secs(60);
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let provider = Fixture::new();
    {
        let mut nodes = provider.nodes.write().await;
        for i in 0..READERS {
            let name = format!("thumbnail-{i:03}.bin");
            nodes.insert(
                ("home".into(), name.clone()),
                file(&name, Some("root"), NodeKind::File, 3_100_000),
            );
        }
    }
    provider.delay_ms.store(250, Ordering::SeqCst);
    let engine = Engine::new(
        account(mount.clone()),
        provider.clone(),
        temp.path().join("state"),
    )
    .await
    .unwrap();
    engine.start().await.unwrap();
    ready(&engine).await;
    let session = CloudFs::new(engine.clone()).unwrap().mount(&mount).unwrap();
    let path = mount.clone();
    let (opened, opening) = tokio::sync::oneshot::channel();
    let mut readers = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        // Open first so this checks content admission independently of metadata.
        let files = (0..READERS)
            .map(|i| std::fs::File::open(path.join(format!("thumbnail-{i:03}.bin"))))
            .collect::<Result<Vec<_>, _>>()?;
        let _ = opened.send(());
        let barrier = Arc::new(std::sync::Barrier::new(READERS));
        let threads = files
            .into_iter()
            .map(|mut file| {
                let barrier = barrier.clone();
                std::thread::spawn(move || -> std::io::Result<Vec<u8>> {
                    barrier.wait();
                    let mut bytes = vec![0; 64 * 1024];
                    file.read_exact(&mut bytes)?;
                    Ok(bytes)
                })
            })
            .collect::<Vec<_>>();
        let mut errors = vec![];
        for thread in threads {
            match thread
                .join()
                .map_err(|_| anyhow::anyhow!("reader panicked"))?
            {
                Ok(bytes) => assert_bytes(&bytes, 0),
                Err(error) => errors.push(error),
            }
        }
        ensure!(
            errors.is_empty(),
            "{} of {READERS} simultaneous reads failed: {errors:?}",
            errors.len()
        );
        Ok(())
    });
    let setup = tokio::time::timeout(Duration::from_secs(20), opening)
        .await
        .context("opening thumbnail files timed out")
        .and_then(|r| r.context("opening thumbnail files failed"));
    let burst_start = std::time::Instant::now();
    let mut listings_ms = vec![];
    let result = if setup.is_ok() {
        tokio::time::timeout(BURST_TIMEOUT, async {
            while provider.reads.load(Ordering::SeqCst) < 4 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            // Keep probing beyond the first downloads, including later queued
            // waves and cache eviction. An early fast sample cannot hide a
            // stall during the remainder of the burst.
            while !readers.is_finished() || listings_ms.len() < 20 {
                let path = mount.join("folder");
                let start = std::time::Instant::now();
                let entries = tokio::time::timeout(
                    Duration::from_millis(500),
                    tokio::task::spawn_blocking(move || {
                        std::fs::read_dir(path)?.collect::<Result<Vec<_>, std::io::Error>>()
                    }),
                )
                .await
                .context("cached navigation stalled behind downloads")???;
                ensure!(entries.len() == 1, "cached directory changed during burst");
                listings_ms.push(start.elapsed().as_secs_f64() * 1000.0);
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            (&mut readers).await??;
            ensure!(
                provider.reads.load(Ordering::SeqCst) == READERS as u64,
                "burst retried provider reads"
            );
            listings_ms.sort_by(f64::total_cmp);
            println!(
                "CIRROVE_THUMBNAIL_BURST {}",
                serde_json::json!({
                    "fixture": "synthetic kernel FUSE, 250 ms content delay; no cloud traffic",
                    "simultaneous_readers": READERS, "file_bytes": 3_100_000, "read_bytes": 64 * 1024,
                    "provider_range_calls": READERS, "directory_samples": listings_ms.len(),
                    "directory_p50_ms": listings_ms[(listings_ms.len() - 1) / 2],
                    "directory_p95_ms": listings_ms[(listings_ms.len() - 1) * 95 / 100],
                    "directory_max_ms": listings_ms.last(),
                    "burst_elapsed_ms": burst_start.elapsed().as_secs_f64() * 1000.0,
                    "burst_deadline_seconds": BURST_TIMEOUT.as_secs(),
                })
            );
            Ok::<_, anyhow::Error>(())
        })
        .await
        .with_context(|| format!(
            "thumbnail burst timed out (provider reads started: {}, reader task finished: {}, directory samples: {}, elapsed: {:?})",
            provider.reads.load(Ordering::SeqCst), readers.is_finished(), listings_ms.len(), burst_start.elapsed()
        ))
        .and_then(|result| result)
    } else {
        setup
    };
    engine.stop().await;
    // Shutdown cancels outstanding reads before joining the kernel session.
    if !readers.is_finished() {
        let _ = readers.await;
    }
    tokio::task::spawn_blocking(move || session.umount_and_join())
        .await
        .unwrap()
        .unwrap();
    assert!(result.is_ok(), "{result:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a working /dev/fuse and fusermount3; run explicitly"]
async fn real_old_and_new_mappings_keep_separate_versions_and_rename_reuses_content() {
    use anyhow::{Context, ensure};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let provider = Fixture::new();
    {
        let mut nodes = provider.nodes.write().await;
        nodes
            .get_mut(&("home".into(), "small.txt".into()))
            .unwrap()
            .content_version = Some("content-1".into());
        let mut link = file("Linked.txt", Some("root"), NodeKind::Shortcut, 0);
        link.target = Some(Box::new(RemoteRef {
            collection: "home".into(),
            item: "small.txt".into(),
            kind: Some(NodeKind::File),
        }));
        nodes.insert(("home".into(), link.id.clone()), link);
    }
    let engine = Engine::new(
        account(mount.clone()),
        provider.clone(),
        temp.path().join("state"),
    )
    .await
    .unwrap();
    engine.start().await.unwrap();
    ready(&engine).await;
    let session = CloudFs::new(engine.clone()).unwrap().mount(&mount).unwrap();
    let mut child=tokio::process::Command::new("python3").args(["-u","-c",r#"
import mmap, os, pathlib, sys, time
root=pathlib.Path(sys.argv[1])
old=open(root/'small.txt','rb'); oldmap=mmap.mmap(old.fileno(),0,access=mmap.ACCESS_READ)
old_alias=open(root/'Linked.txt','rb'); aliasmap=mmap.mmap(old_alias.fileno(),0,access=mmap.ACCESS_READ)
old_inode=os.fstat(old.fileno()).st_ino; old_alias_inode=os.fstat(old_alias.fileno()).st_ino
assert old_inode != old_alias_inode
assert oldmap[:4096] == aliasmap[:4096] == bytes(i % 251 for i in range(4096))
print('mapped',flush=True)
assert sys.stdin.readline().strip()=='content'
def wait_new(path,previous):
    deadline=time.monotonic()+4
    while time.monotonic()<deadline:
        try:
            inode=os.stat(path).st_ino
            if inode!=previous:return inode
        except FileNotFoundError:pass
        time.sleep(.02)
    raise AssertionError('new namespace version did not become visible')
new_inode=wait_new(root/'small.txt',old_inode); new_alias_inode=wait_new(root/'Linked.txt',old_alias_inode)
new=open(root/'small.txt','rb'); newmap=mmap.mmap(new.fileno(),0,access=mmap.ACCESS_READ)
new_alias=open(root/'Linked.txt','rb'); newaliasmap=mmap.mmap(new_alias.fileno(),0,access=mmap.ACCESS_READ)
assert newmap[:4096] == newaliasmap[:4096] == bytes((i+17) % 251 for i in range(4096))
assert oldmap[:4096] == aliasmap[:4096] == bytes(i % 251 for i in range(4096))
assert os.fstat(new.fileno()).st_ino==new_inode
print('updated',flush=True)
assert sys.stdin.readline().strip()=='rename'
assert wait_new(root/'renamed.txt',old_inode)==new_inode
with open(root/'renamed.txt','rb') as renamed:
    assert renamed.read(4096)==bytes((i+17) % 251 for i in range(4096))
assert oldmap[:4096]==bytes(i % 251 for i in range(4096))
assert newmap[:4096]==bytes((i+17) % 251 for i in range(4096))
for view in (oldmap,aliasmap,newmap,newaliasmap):view.close()
for f in (old,old_alias,new,new_alias):f.close()
"#]).arg(&mount).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true).spawn().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut stdin = child.stdin.take().unwrap();
    let result = tokio::time::timeout(Duration::from_secs(12), async {
        let mut line = String::new();
        stdout.read_line(&mut line).await?;
        ensure!(line.trim() == "mapped", "initial mmap failed");
        {
            let mut nodes = provider.nodes.write().await;
            let node = nodes.get_mut(&("home".into(), "small.txt".into())).unwrap();
            node.content_version = Some("content-2".into());
            node.etag = Some("metadata-2".into());
        }
        cirrove_service::refresh(
            provider.as_ref(),
            &engine.scope("home"),
            &engine.db,
            false,
            &engine.cancel,
        )
        .await?;
        engine.changed.notify_waiters();
        stdin.write_all(b"content\n").await?;
        line.clear();
        stdout.read_line(&mut line).await?;
        ensure!(
            line.trim() == "updated",
            "new/old mmap versions did not stay isolated"
        );
        let reads = provider.reads.load(Ordering::SeqCst);
        {
            let mut nodes = provider.nodes.write().await;
            let node = nodes.get_mut(&("home".into(), "small.txt".into())).unwrap();
            node.name = "renamed.txt".into();
            node.etag = Some("metadata-3".into());
        }
        cirrove_service::refresh(
            provider.as_ref(),
            &engine.scope("home"),
            &engine.db,
            false,
            &engine.cancel,
        )
        .await?;
        engine.changed.notify_waiters();
        stdin.write_all(b"rename\n").await?;
        let status = child.wait().await?;
        ensure!(
            status.success(),
            "mapping process failed after content replacement/rename"
        );
        ensure!(
            provider.reads.load(Ordering::SeqCst) == reads,
            "metadata-only rename downloaded content again"
        );
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("mapping regression timed out")
    .and_then(|result| result);
    if child.try_wait().unwrap().is_none() {
        let _ = child.kill().await;
    }
    let output = child.wait_with_output().await.unwrap();
    engine.stop().await;
    tokio::task::spawn_blocking(move || session.umount_and_join())
        .await
        .unwrap()
        .unwrap();
    assert!(
        result.is_ok(),
        "{result:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn push_engine(temp: &tempfile::TempDir) -> (Arc<Fixture>, Arc<Engine>) {
    let provider = Fixture::new();
    provider
        .nodes
        .write()
        .await
        .retain(|(drive, _), node| drive == "home" && node.target.is_none());
    provider.push.store(true, Ordering::SeqCst);
    let mut config = account(temp.path().join("mount"));
    config.poll_seconds = 3600; // A timer cannot accidentally satisfy these tests.
    let engine = Engine::new(config, provider.clone(), temp.path().join("state"))
        .await
        .unwrap();
    engine.start().await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if engine.health().await.iter().any(|h| {
                h.state == "ready" && h.notifications == notifications::NotificationState::Connected
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    (provider, engine)
}
async fn fixture_remote_change(provider: &Fixture, revision: &str) {
    let mut node = file("remote.txt", Some("root"), NodeKind::File, 17);
    node.etag = Some(revision.into());
    provider
        .nodes
        .write()
        .await
        .insert(("home".into(), node.id.clone()), node);
}
async fn wait_revision(engine: &Engine, revision: &str) {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let store = Store::open(&engine.db).unwrap();
            if store
                .node(&engine.scope("home"), "remote.txt")
                .unwrap()
                .is_some_and(|n| n.etag.as_deref() == Some(revision))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_refreshes_immediately_and_retains_bursts_during_an_active_delta() {
    let temp = tempfile::tempdir().unwrap();
    let (provider, engine) = push_engine(&temp).await;
    let hints = provider.hints.read().await["home"].clone();
    fixture_remote_change(&provider, "first").await;
    hints.changed();
    wait_revision(&engine, "first").await;
    let before = provider.change_calls.load(Ordering::SeqCst);
    provider.hold_change.store(true, Ordering::SeqCst);
    fixture_remote_change(&provider, "staged").await;
    hints.changed();
    tokio::time::timeout(Duration::from_secs(2), provider.change_entered.notified())
        .await
        .unwrap();
    fixture_remote_change(&provider, "latest").await;
    for _ in 0..1000 {
        hints.changed();
    }
    provider.release_change.notify_one();
    wait_revision(&engine, "latest").await;
    assert!(provider.change_calls.load(Ordering::SeqCst) - before <= 3);
    tokio::time::timeout(Duration::from_secs(1), engine.stop())
        .await
        .unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_preserves_retry_after_and_reconnect_catches_unreported_changes() {
    let temp = tempfile::tempdir().unwrap();
    let (provider, engine) = push_engine(&temp).await;
    let hints = provider.hints.read().await["home"].clone();
    provider.throttle_change.store(true, Ordering::SeqCst);
    fixture_remote_change(&provider, "throttled").await;
    hints.changed();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !engine.health().await.iter().any(|h| h.state == "throttled") {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let calls = provider.change_calls.load(Ordering::SeqCst);
    for _ in 0..1000 {
        hints.changed();
    }
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(provider.change_calls.load(Ordering::SeqCst), calls);
    wait_revision(&engine, "throttled").await;
    fixture_remote_change(&provider, "during-disconnect").await;
    let subscriptions = provider.subscriptions.load(Ordering::SeqCst);
    provider.disconnect.notify_one();
    wait_revision(&engine, "during-disconnect").await;
    assert!(provider.subscriptions.load(Ordering::SeqCst) > subscriptions);
    engine.stop().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unsupported_notifications_do_not_create_a_busy_poll_loop() {
    let temp = tempfile::tempdir().unwrap();
    let provider = Fixture::new();
    let engine = Engine::new(
        account(temp.path().join("mount")),
        provider.clone(),
        temp.path().join("state"),
    )
    .await
    .unwrap();
    engine.start().await.unwrap();
    ready(&engine).await;
    let calls = provider.change_calls.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(provider.change_calls.load(Ordering::SeqCst), calls);
    assert!(
        engine
            .health()
            .await
            .iter()
            .all(|h| h.notifications == notifications::NotificationState::Polling)
    );
    engine.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a working /dev/fuse and fusermount3; run explicitly"]
async fn real_push_updates_the_mounted_namespace_without_waiting_for_polling() {
    let temp = tempfile::tempdir().unwrap();
    let (provider, engine) = push_engine(&temp).await;
    let mount = engine.account.mount_path.clone();
    std::fs::create_dir(&mount).unwrap();
    let session = CloudFs::new(engine.clone()).unwrap().mount(&mount).unwrap();
    let hints = provider.hints.read().await["home"].clone();
    // Keep activity revalidation stalled so only the push-driven delta can
    // expose this fixture's changes through the mount.
    provider.stall_root_directory.store(true, Ordering::SeqCst);
    // Prime a negative entry and directory listing in the kernel.
    let path = mount.join("remote.txt");
    let absent = path.clone();
    assert!(
        tokio::task::spawn_blocking(move || std::fs::metadata(absent))
            .await
            .unwrap()
            .is_err()
    );
    fixture_remote_change(&provider, "new").await;
    hints.changed();
    wait_revision(&engine, "new").await;
    let bytes = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let path = path.clone();
            if let Ok(bytes) = tokio::task::spawn_blocking(move || std::fs::read(path))
                .await
                .unwrap()
            {
                break bytes;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(bytes.len(), 17);
    assert_bytes(&bytes, 0);
    {
        let mut nodes = provider.nodes.write().await;
        let node = nodes
            .get_mut(&("home".into(), "remote.txt".into()))
            .unwrap();
        node.name = "renamed.txt".into();
        node.etag = Some("renamed".into());
    }
    hints.changed();
    wait_revision(&engine, "renamed").await;
    let current = mount.clone();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let current = current.clone();
            let done = tokio::task::spawn_blocking(move || {
                !current.join("remote.txt").exists()
                    && std::fs::read(current.join("renamed.txt")).is_ok()
            })
            .await
            .unwrap();
            if done {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    engine.stop().await;
    tokio::task::spawn_blocking(move || session.umount_and_join())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn active_directory_observes_create_and_delete_without_push_or_delta_timer() {
    let temp = tempfile::tempdir().unwrap();
    let provider = Fixture::new();
    let mut config = account(temp.path().join("mount"));
    config.poll_seconds = 3600;
    let engine = Engine::new(config, provider.clone(), temp.path().join("state"))
        .await
        .unwrap();
    engine.start().await.unwrap();
    ready(&engine).await;
    let delta_calls = provider.change_calls.load(Ordering::SeqCst);
    fixture_remote_change(&provider, "active").await;
    let cached = engine
        .children(&engine.scope("home"), "root")
        .await
        .unwrap();
    assert!(!cached.iter().any(|n| n.id == "remote.txt"));
    wait_revision(&engine, "active").await;
    provider
        .nodes
        .write()
        .await
        .remove(&("home".into(), "remote.txt".into()));
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if !Store::open(&engine.db)
                .unwrap()
                .children(&engine.scope("home"), "root")
                .unwrap()
                .unwrap()
                .iter()
                .any(|n| n.id == "remote.txt")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(provider.change_calls.load(Ordering::SeqCst), delta_calls);
    assert!(provider.directory_calls.load(Ordering::SeqCst) <= 3);
    engine.stop().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stalled_active_refresh_keeps_cached_and_cold_foreground_requests_responsive() {
    let temp = tempfile::tempdir().unwrap();
    let provider = Fixture::new();
    let engine = Engine::new(
        account(temp.path().join("mount")),
        provider.clone(),
        temp.path().join("state"),
    )
    .await
    .unwrap();
    engine.start().await.unwrap();
    ready(&engine).await;
    provider.stall_root_directory.store(true, Ordering::SeqCst);
    engine
        .children(&engine.scope("home"), "root")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while provider.directory_calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        for _ in 0..20 {
            engine
                .children(&engine.scope("home"), "root")
                .await
                .unwrap();
        }
        engine
            .children(&engine.scope("cold"), "different-directory")
            .await
            .unwrap();
    })
    .await
    .unwrap();
    assert_eq!(provider.directory_calls.load(Ordering::SeqCst), 2);
    tokio::time::timeout(Duration::from_secs(1), engine.stop())
        .await
        .unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn repeated_activity_cannot_override_background_directory_throttling() {
    let temp = tempfile::tempdir().unwrap();
    let provider = Fixture::new();
    let engine = Engine::new(
        account(temp.path().join("mount")),
        provider.clone(),
        temp.path().join("state"),
    )
    .await
    .unwrap();
    engine.start().await.unwrap();
    ready(&engine).await;
    provider.throttle_directory.store(true, Ordering::SeqCst);
    engine
        .children(&engine.scope("home"), "root")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while engine.directory_freshness().delayed == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    for _ in 0..50 {
        engine
            .children(&engine.scope("home"), "root")
            .await
            .unwrap();
    }
    assert_eq!(provider.directory_calls.load(Ordering::SeqCst), 1);
    engine.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a working /dev/fuse and fusermount3; run explicitly"]
async fn real_active_directory_refreshes_during_a_stalled_read_without_push_or_delta() {
    let temp = tempfile::tempdir().unwrap();
    let provider = Fixture::new();
    let mut config = account(temp.path().join("mount"));
    config.poll_seconds = 3600;
    let engine = Engine::new(config, provider.clone(), temp.path().join("state"))
        .await
        .unwrap();
    engine.start().await.unwrap();
    ready(&engine).await;
    let delta_calls = provider.change_calls.load(Ordering::SeqCst);
    let mount = engine.account.mount_path.clone();
    std::fs::create_dir(&mount).unwrap();
    let session = CloudFs::new(engine.clone()).unwrap().mount(&mount).unwrap();
    provider.stall.store(true, Ordering::SeqCst);
    let path = mount.join("small.txt");
    let reader = tokio::task::spawn_blocking(move || std::fs::read(path));
    let result = tokio::time::timeout(Duration::from_secs(40), async {
        // The blocked file lookup activates its parent. Wait for that initial
        // listing to finish, so a later refresh must discover each mutation.
        loop {
            if provider.reads.load(Ordering::SeqCst) > 0
                && provider.directory_calls.load(Ordering::SeqCst) > 0
                && engine.directory_freshness().refreshing == 0
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        for (name, previous) in [
            (Some("remote.txt"), None),
            (Some("geändert.txt"), Some("remote.txt")),
            (None, Some("geändert.txt")),
        ] {
            {
                let mut nodes = provider.nodes.write().await;
                let key = ("home".into(), "remote.txt".into());
                if let Some(name) = name {
                    let mut node = file("remote.txt", Some("root"), NodeKind::File, 17);
                    node.name = name.into();
                    nodes.insert(key, node);
                } else {
                    nodes.remove(&key);
                }
            }
            loop {
                let current = mount.clone();
                let names = tokio::task::spawn_blocking(move || {
                    std::fs::read_dir(current)?
                        .map(|entry| entry.map(|e| e.file_name()))
                        .collect::<std::io::Result<Vec<_>>>()
                })
                .await??;
                if name.is_none_or(|name| names.iter().any(|n| n == name))
                    && previous.is_none_or(|name| names.iter().all(|n| n != name))
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        }
        anyhow::ensure!(!reader.is_finished(), "the content request was not stalled");
        anyhow::ensure!(provider.change_calls.load(Ordering::SeqCst) == delta_calls);
        anyhow::ensure!(provider.directory_calls.load(Ordering::SeqCst) <= 5);
        anyhow::ensure!(provider.reads.load(Ordering::SeqCst) == 1);
        Ok::<_, anyhow::Error>(())
    })
    .await;
    // Always release the blocked read and mount before reporting a failure.
    engine.stop().await;
    tokio::task::spawn_blocking(move || session.umount_and_join())
        .await
        .unwrap()
        .unwrap();
    assert!(reader.await.unwrap().is_err());
    result.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires kernel FUSE; experimental isolated local writes and journal recovery"]
async fn real_local_saves_remain_readable_during_upload_and_after_offline_restart() {
    use cirrove_service::journal::{UploadIntent, UploadJournal, UploadState};
    use std::{
        io::{Read, Seek, SeekFrom, Write},
        sync::Mutex,
    };
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    let state = temp.path().join("state");
    let journal_root = temp.path().join("journal");
    std::fs::create_dir(&mount).unwrap();
    let provider = Fixture::new();
    let mut account = account(mount.clone());
    account.enabled = false;
    account.access = cirrove_auth::AccessMode::ReadWrite;
    let engine = Engine::new(account.clone(), provider.clone(), state.clone())
        .await
        .unwrap();
    engine.start().await.unwrap();
    ready(&engine).await;
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&journal_root, &account.id, 64 * 1024 * 1024).unwrap(),
    ));
    let session = CloudFs::new_experimental_writable(engine.clone(), journal.clone())
        .await
        .unwrap()
        .mount(&mount)
        .unwrap();
    let path = mount.join("Grüße & Kärnten.txt");
    let j = journal.clone();
    let m = mount.clone();
    let last = tokio::task::spawn_blocking(move || {
        let mut f = std::fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        f.write_all(b"first").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
        assert!(
            j.lock().unwrap().list(0, 100).unwrap().is_empty(),
            "closing a read-only preview must not seal another application's unfinished edit"
        );
        f.sync_all().unwrap();
        let first = j.lock().unwrap().claim_next().unwrap().unwrap();
        assert_eq!(first.state, UploadState::Uploading);
        f.seek(SeekFrom::Start(0)).unwrap();
        f.write_all(b"second save").unwrap();
        f.set_len(11).unwrap();
        f.sync_all().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second save");
        assert!(j.lock().unwrap().claim_next().unwrap().is_none());
        {
            let mut j = j.lock().unwrap();
            let mut bytes = vec![];
            j.payload(first.id)
                .unwrap()
                .read_to_end(&mut bytes)
                .unwrap();
            assert_eq!(bytes, b"first");
            let receipt = Node {
                id: "created-remote-id".into(),
                parent_id: Some("root".into()),
                name: "Grüße & Kärnten.txt".into(),
                kind: NodeKind::File,
                size: 5,
                modified_unix: 1,
                etag: Some("receipt-one".into()),
                content_version: Some("uploaded-one".into()),
                target: None,
            };
            j.acknowledge(first.id, first.attempt.unwrap(), receipt)
                .unwrap();
            let second = j.claim_next().unwrap().unwrap();
            assert_eq!(
                second.intent,
                UploadIntent::Replace {
                    item: "created-remote-id".into(),
                    expected_etag: "receipt-one".into()
                }
            );
            let mut bytes = vec![];
            j.payload(second.id)
                .unwrap()
                .read_to_end(&mut bytes)
                .unwrap();
            assert_eq!(bytes, b"second save");
        }
        f.set_len(6).unwrap();
        f.sync_all().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        drop(f);
        // Editing one byte keeps the rest of an existing remote file intact.
        let existing = m.join("folder/deep.txt");
        let original = std::fs::read(&existing).unwrap();
        let mut edit = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&existing)
            .unwrap();
        edit.seek(SeekFrom::Start(4)).unwrap();
        edit.write_all(b"X").unwrap();
        edit.sync_all().unwrap();
        let mut expected = original;
        expected[4] = b'X';
        assert_eq!(std::fs::read(existing).unwrap(), expected);
        let records = j.lock().unwrap().list(0, 100).unwrap();
        records.into_iter().find(|r| r.size == 6).unwrap().id
    })
    .await
    .unwrap();
    provider.offline.store(true, Ordering::SeqCst);
    engine.stop().await;
    tokio::task::spawn_blocking(move || session.umount_and_join())
        .await
        .unwrap()
        .unwrap();
    drop(engine);
    drop(journal);
    let engine = Engine::new(account.clone(), provider, state).await.unwrap();
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&journal_root, &account.id, 64 * 1024 * 1024).unwrap(),
    ));
    assert_eq!(
        journal.lock().unwrap().get(last).unwrap().state,
        UploadState::Pending
    );
    let session = CloudFs::new_experimental_writable(engine.clone(), journal)
        .await
        .unwrap()
        .mount(&mount)
        .unwrap();
    tokio::task::spawn_blocking(move || {
        assert_eq!(
            std::fs::read(mount.join("Grüße & Kärnten.txt")).unwrap(),
            b"second"
        );
        let edited = std::fs::read(mount.join("folder/deep.txt")).unwrap();
        assert_eq!(edited[4], b'X');
        assert_eq!(edited.len(), 17);
    })
    .await
    .unwrap();
    engine.stop().await;
    tokio::task::spawn_blocking(move || session.umount_and_join())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires kernel FUSE; delayed writable hydration must not block navigation or other saves"]
async fn real_stalled_write_open_preserves_navigation_and_independent_local_saves() {
    use cirrove_service::journal::UploadJournal;
    use std::{io::Write, sync::Mutex};
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let provider = Fixture::new();
    let mut account = account(mount.clone());
    account.enabled = false;
    account.access = cirrove_auth::AccessMode::ReadWrite;
    let engine = Engine::new(account.clone(), provider.clone(), temp.path().join("state"))
        .await
        .unwrap();
    engine.start().await.unwrap();
    ready(&engine).await;
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&temp.path().join("journal"), &account.id, 64 * 1024 * 1024).unwrap(),
    ));
    let session = CloudFs::new_experimental_writable(engine.clone(), journal.clone())
        .await
        .unwrap()
        .mount(&mount)
        .unwrap();
    provider.stall.store(true, Ordering::SeqCst);
    let p = mount.join("small.txt");
    let blocked =
        tokio::task::spawn_blocking(move || std::fs::OpenOptions::new().write(true).open(p));
    tokio::time::timeout(Duration::from_secs(2), async {
        while provider.reads.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        journal.try_lock().is_ok(),
        "network hydration retained the upload journal lock"
    );
    tokio::time::timeout(
        Duration::from_secs(2),
        tokio::task::spawn_blocking(move || {
            assert!(std::fs::read_dir(&mount).unwrap().count() >= 4);
            let mut f = std::fs::File::create(mount.join("independent.txt")).unwrap();
            f.write_all(b"saved during stalled download").unwrap();
            f.sync_all().unwrap();
        }),
    )
    .await
    .unwrap()
    .unwrap();
    engine.stop().await;
    assert!(blocked.await.unwrap().is_err());
    tokio::task::spawn_blocking(move || session.umount_and_join())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "synthetic writable FUSE child, launched only by its crash-recovery parent"]
async fn writable_crash_mount_fixture() {
    use cirrove_service::journal::UploadJournal;
    use std::sync::Mutex;
    let Some(root) = std::env::var_os("CIRROVE_WRITABLE_CRASH_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let mount = root.join("mount");
    std::fs::create_dir(&mount).unwrap();
    let mut account = account(mount.clone());
    account.enabled = false;
    account.access = cirrove_auth::AccessMode::ReadWrite;
    let engine = Engine::new(account.clone(), Fixture::new(), root.join("state"))
        .await
        .unwrap();
    engine.start().await.unwrap();
    ready(&engine).await;
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&root.join("journal"), &account.id, 1024 * 1024).unwrap(),
    ));
    let _session = CloudFs::new_experimental_writable(engine.clone(), journal)
        .await
        .unwrap()
        .mount(&mount)
        .unwrap();
    std::fs::write(root.join("ready"), b"mounted").unwrap();
    std::future::pending::<()>().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires kernel FUSE; kills only an isolated synthetic writable mount process"]
async fn real_writable_mount_crash_keeps_the_saved_generation_and_newer_local_bytes() {
    use cirrove_service::journal::{UploadJournal, UploadState};
    use std::{
        io::{Read, Seek, SeekFrom, Write},
        sync::Mutex,
    };
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::process::Command::new("fusermount3")
                .args(["-u", "-z", "--"])
                .arg(&self.0)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
    let _cleanup = Cleanup(mount.clone());
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "writable_crash_mount_fixture",
            "--ignored",
            "--nocapture",
        ])
        .env("CIRROVE_WRITABLE_CRASH_ROOT", temp.path())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        while !temp.path().join("ready").exists() {
            assert!(child.try_wait().unwrap().is_none());
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    // The application is a different process from the daemon. Holding a write
    // handle to its own FUSE mount can deadlock a dying test daemon in close().
    let file_path = mount.join("saved.txt");
    let file = tokio::task::spawn_blocking(move || {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(file_path)
            .unwrap();
        file.write_all(b"durable generation").unwrap();
        file.sync_all().unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(b"newer").unwrap();
        file
    })
    .await
    .unwrap();
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    drop(file);
    assert_eq!(
        std::fs::read(mount.join("saved.txt"))
            .unwrap_err()
            .raw_os_error(),
        Some(libc::ENOTCONN)
    );
    assert!(
        std::process::Command::new("fusermount3")
            .args(["-u", "-z", "--"])
            .arg(&mount)
            .status()
            .unwrap()
            .success()
    );
    let mut account = account(mount.clone());
    account.enabled = false;
    account.access = cirrove_auth::AccessMode::ReadWrite;
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&temp.path().join("journal"), &account.id, 1024 * 1024).unwrap(),
    ));
    {
        let j = journal.lock().unwrap();
        let record = j.list(0, 100).unwrap().remove(0);
        assert_eq!(record.state, UploadState::Pending);
        let mut bytes = vec![];
        j.payload(record.id)
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(bytes, b"durable generation");
        assert!(j.working_files().unwrap()[0].dirty);
    }
    let provider = Fixture::new();
    provider.offline.store(true, Ordering::SeqCst);
    let engine = Engine::new(account, provider, temp.path().join("state"))
        .await
        .unwrap();
    let session = CloudFs::new_experimental_writable(engine.clone(), journal)
        .await
        .unwrap()
        .mount(&mount)
        .unwrap();
    tokio::task::spawn_blocking(move || {
        assert_eq!(
            std::fs::read(mount.join("saved.txt")).unwrap(),
            b"newerle generation"
        )
    })
    .await
    .unwrap();
    engine.stop().await;
    tokio::task::spawn_blocking(move || session.umount_and_join())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn experimental_writes_require_opt_in_and_matching_journal_owner() {
    use cirrove_service::journal::UploadJournal;
    use std::sync::Mutex;
    // An enabled account used to be refused as well. That made a writable mount
    // unreachable rather than deliberate: the daemon does not mount a disabled
    // account, so nothing a user could configure satisfied both halves. The write
    // grant is the opt-in, and the journal owner still has to match.
    for (enabled, access, matching, accepted) in [
        (false, cirrove_auth::AccessMode::ReadOnly, true, false),
        (true, cirrove_auth::AccessMode::ReadOnly, true, false),
        (true, cirrove_auth::AccessMode::ReadWrite, true, true),
        (false, cirrove_auth::AccessMode::ReadWrite, false, false),
        (true, cirrove_auth::AccessMode::ReadWrite, false, false),
        (false, cirrove_auth::AccessMode::ReadWrite, true, true),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let mut account = account(temp.path().join("mount"));
        account.enabled = enabled;
        account.access = access;
        let journal = Arc::new(Mutex::new(
            UploadJournal::open(
                &temp.path().join("journal"),
                if matching {
                    &account.id
                } else {
                    "another-account"
                },
                1024,
            )
            .unwrap(),
        ));
        let engine = Engine::new(account, Fixture::new(), temp.path().join("state"))
            .await
            .unwrap();
        assert_eq!(
            CloudFs::new_experimental_writable(engine, journal)
                .await
                .is_ok(),
            accepted
        );
    }
}

impl Fixture {
    async fn hold_observation(&self, cancel: &CancellationToken) -> Result<(), ProviderError> {
        if self.hold_observation.swap(false, Ordering::SeqCst) {
            self.observation_entered.notify_one();
            tokio::select! {biased;
                _=cancel.cancelled()=>return Err(ProviderError::Cancelled),
                _=self.release_observation.notified()=>{},
            }
        }
        Ok(())
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn late_directory_response_cannot_overwrite_a_newer_visible_listing() {
    let temp = tempfile::tempdir().unwrap();
    let provider = Fixture::new();
    let engine = Engine::new(
        account(temp.path().join("mount")),
        provider.clone(),
        temp.path().join("state"),
    )
    .await
    .unwrap();
    let scope = engine.scope("cold");
    let original = file("entry", Some("root"), NodeKind::File, 5);
    provider
        .nodes
        .write()
        .await
        .insert(("cold".into(), original.id.clone()), original.clone());
    provider.hold_observation.store(true, Ordering::SeqCst);
    let request_engine = engine.clone();
    let request_scope = scope.clone();
    let request =
        tokio::spawn(async move { request_engine.children(&request_scope, "root").await });
    tokio::time::timeout(
        Duration::from_secs(2),
        provider.observation_entered.notified(),
    )
    .await
    .unwrap();
    let mut newer = original;
    newer.name = "new name".into();
    newer.etag = Some("newer".into());
    Store::open(&engine.db)
        .unwrap()
        .observe_directory(&scope, "root", &[newer.clone()])
        .unwrap();
    provider.offline.store(true, Ordering::SeqCst);
    provider.release_observation.notify_one();
    let listing = tokio::time::timeout(Duration::from_secs(2), request)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(listing, vec![newer.clone()]);
    assert_eq!(
        Store::open(&engine.db)
            .unwrap()
            .children(&scope, "root")
            .unwrap()
            .unwrap(),
        vec![newer]
    );
    assert_eq!(provider.directory_calls.load(Ordering::SeqCst), 1);
    engine.stop().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn late_node_response_cannot_restore_an_older_content_version() {
    let temp = tempfile::tempdir().unwrap();
    let provider = Fixture::new();
    let engine = Engine::new(
        account(temp.path().join("mount")),
        provider.clone(),
        temp.path().join("state"),
    )
    .await
    .unwrap();
    let scope = engine.scope("cold");
    let original = file("entry", Some("root"), NodeKind::File, 5);
    provider
        .nodes
        .write()
        .await
        .insert(("cold".into(), original.id.clone()), original.clone());
    provider.hold_observation.store(true, Ordering::SeqCst);
    let request_engine = engine.clone();
    let request_scope = scope.clone();
    let request = tokio::spawn(async move { request_engine.node(&request_scope, "entry").await });
    tokio::time::timeout(
        Duration::from_secs(2),
        provider.observation_entered.notified(),
    )
    .await
    .unwrap();
    let mut newer = original;
    newer.size = 8;
    newer.etag = Some("newer".into());
    Store::open(&engine.db)
        .unwrap()
        .observe_node(&scope, &newer)
        .unwrap();
    provider.offline.store(true, Ordering::SeqCst);
    provider.release_observation.notify_one();
    let node = tokio::time::timeout(Duration::from_secs(2), request)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(node, newer);
    assert_eq!(
        Store::open(&engine.db)
            .unwrap()
            .node(&scope, "entry")
            .unwrap()
            .unwrap(),
        newer
    );
    engine.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn superseded_cold_listing_retries_when_no_complete_newer_listing_exists() {
    let temp = tempfile::tempdir().unwrap();
    let provider = Fixture::new();
    let engine = Engine::new(
        account(temp.path().join("mount")),
        provider.clone(),
        temp.path().join("state"),
    )
    .await
    .unwrap();
    let scope = engine.scope("cold");
    let original = file("entry", Some("root"), NodeKind::File, 5);
    provider
        .nodes
        .write()
        .await
        .insert(("cold".into(), original.id.clone()), original.clone());
    provider.hold_observation.store(true, Ordering::SeqCst);
    let e = engine.clone();
    let s = scope.clone();
    let request = tokio::spawn(async move { e.children(&s, "root").await });
    tokio::time::timeout(
        Duration::from_secs(2),
        provider.observation_entered.notified(),
    )
    .await
    .unwrap();
    let mut newer = original;
    newer.name = "new name".into();
    newer.etag = Some("newer".into());
    provider
        .nodes
        .write()
        .await
        .insert(("cold".into(), newer.id.clone()), newer.clone());
    Store::open(&engine.db)
        .unwrap()
        .observe_node(&scope, &newer)
        .unwrap();
    assert!(
        Store::open(&engine.db)
            .unwrap()
            .children(&scope, "root")
            .unwrap()
            .is_none()
    );
    provider.release_observation.notify_one();
    let listing = tokio::time::timeout(Duration::from_secs(2), request)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(listing, vec![newer]);
    assert_eq!(provider.directory_calls.load(Ordering::SeqCst), 2);
    engine.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; stale directory replies must not reach applications"]
async fn real_delayed_directory_reply_cannot_restore_a_name_replaced_by_delta() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let provider = Fixture::new();
    let mut original = file("stable", Some("root"), NodeKind::File, 5);
    original.name = "old.txt".into();
    provider.nodes.write().await.clear();
    provider
        .nodes
        .write()
        .await
        .insert(("home".into(), original.id.clone()), original.clone());
    provider.hold_observation.store(true, Ordering::SeqCst);
    let engine = Engine::new(
        account(mount.clone()),
        provider.clone(),
        temp.path().join("state"),
    )
    .await
    .unwrap();
    let fs = CloudFs::new(engine.clone()).unwrap();
    let path = mount.clone();
    let session = tokio::task::spawn_blocking(move || fs.mount(&path))
        .await
        .unwrap()
        .unwrap();
    let mut app = tokio::process::Command::new("python3")
        .arg("-c")
        .arg("import json,os,sys; print(json.dumps(sorted(os.listdir(sys.argv[1]))))")
        .arg(&mount)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(3),
        provider.observation_entered.notified(),
    )
    .await
    .unwrap();
    let mut newer = original;
    newer.name = "new.txt".into();
    newer.etag = Some("newer".into());
    provider
        .nodes
        .write()
        .await
        .insert(("home".into(), newer.id.clone()), newer.clone());
    let scope = engine.scope("home");
    {
        let mut store = Store::open(&engine.db).unwrap();
        store.begin(&scope, false).unwrap();
        store
            .stage(
                &scope,
                None,
                &ChangePage {
                    changes: vec![Change::Upsert(newer)],
                    checkpoint: Checkpoint::Complete(Cursor("published".into())),
                },
            )
            .unwrap();
    }
    engine.changed.notify_waiters();
    provider.release_observation.notify_one();
    tokio::time::timeout(Duration::from_secs(3), app.wait())
        .await
        .unwrap()
        .unwrap();
    let output = app.wait_with_output().await.unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Vec<String>>(&output.stdout).unwrap(),
        vec!["new.txt"]
    );
    assert_eq!(provider.reads.load(Ordering::SeqCst), 0);
    engine.stop().await;
    tokio::task::spawn_blocking(move || session.umount_and_join())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn late_not_found_reply_cannot_hide_a_newer_visible_item() {
    let temp = tempfile::tempdir().unwrap();
    let provider = Fixture::new();
    let engine = Engine::new(
        account(temp.path().join("mount")),
        provider.clone(),
        temp.path().join("state"),
    )
    .await
    .unwrap();
    let scope = engine.scope("cold");
    provider.hold_observation.store(true, Ordering::SeqCst);
    let fetch = engine.clone();
    let target = scope.clone();
    let request = tokio::spawn(async move { fetch.node(&target, "new-item").await });
    tokio::time::timeout(
        Duration::from_secs(2),
        provider.observation_entered.notified(),
    )
    .await
    .unwrap();
    let newer = file("new-item", Some("root"), NodeKind::File, 8);
    Store::open(&engine.db)
        .unwrap()
        .observe_directory(&scope, "root", std::slice::from_ref(&newer))
        .unwrap();
    provider.release_observation.notify_one();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), request)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        newer
    );
    assert_eq!(
        Store::open(&engine.db)
            .unwrap()
            .node(&scope, "new-item")
            .unwrap(),
        Some(newer)
    );
}

/// A pin made the way a user makes one must actually keep the bytes.
///
/// Until now `cirrove pin` wrote the reservation straight into the index and
/// stopped: `Engine::materialise_pin` and `Engine::pin_folder` had no caller
/// outside their own tests, because only the daemon holds an engine and the
/// command line is not the daemon. So a pin reserved space and kept nothing, and
/// the first offline read still failed -- which is the whole of what pinning is
/// for.
///
/// This drives the real path: a manager holding a live engine, a control socket
/// beside it, and the same client function the command line calls. It fails
/// against a daemon whose socket answers only `status`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pin_from_the_command_line_reaches_the_daemon_and_keeps_the_bytes() {
    use cirrove_service::{accounts::Settings, manager::Manager, private_dir};
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let state = temp.path().join("state");
    private_dir(&state).unwrap();
    let mut config = account(mount.clone());
    // The shared fixture account carries a 16 MiB cache, and reservations may not
    // claim the whole budget: the floor is eight blocks, which is 32 MiB, so a
    // 16 MiB cache can pin nothing at all. That refusal is correct and is not
    // what this test is about.
    config.cache_bytes = 128 * 1024 * 1024;
    std::fs::write(
        state.join("accounts.json"),
        serde_json::to_vec(&Settings {
            version: 1,
            accounts: vec![config],
        })
        .unwrap(),
    )
    .unwrap();

    let provider = Fixture::new();
    let reads = provider.clone();
    let cancel = CancellationToken::new();
    let factory: cirrove_service::manager::ProviderFactory = {
        let provider = provider.clone();
        Arc::new(move |_| Ok(provider.clone()))
    };
    let (manager, worker) = Manager::start_with_provider(state.clone(), cancel.clone(), factory);
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if manager
                .status
                .read()
                .await
                .first()
                .is_some_and(|s| s.state == "ready")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("account did not become ready");

    let socket = state.join("control.sock");
    let server = tokio::spawn(cirrove_service::serve_managed(
        state.join("metadata.db"),
        socket.clone(),
        cancel.clone(),
        Some(manager.clone()),
    ));
    tokio::time::timeout(Duration::from_secs(10), async {
        while cirrove_service::status(&socket).await.is_err() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("control socket never answered");

    // Nothing fetched yet, so a later read counter has something to be measured
    // against.
    let before = reads.reads.load(Ordering::SeqCst);
    let reply = cirrove_service::pin(
        &socket,
        &cirrove_service::PinRequest {
            path: Some("small.txt".into()),
            ..Default::default()
        },
    )
    .await
    .expect("pin request failed");
    assert!(
        reply.accepted && reply.refusal.is_none(),
        "the daemon refused an ordinary pin: {reply:?}"
    );
    assert!(
        reads.reads.load(Ordering::SeqCst) > before,
        "a pin that fetches nothing is an accounting entry; the provider was never read"
    );

    let pinned = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let status = cirrove_service::status(&socket).await.unwrap();
            if let Some(pin) = status.accounts[0].pins.first()
                && pin.resident > 0
            {
                break pin.clone();
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("status never reported resident bytes for the pin");
    assert!(
        pinned.blocks > 0 && pinned.resident > 0,
        "status must report what the pin kept, not only what it reserved: {pinned:?}"
    );
    assert!(
        pinned.reserved >= pinned.resident,
        "a pin cannot hold more than it reserved: {pinned:?}"
    );

    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(10), worker).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), server).await;
}

/// Pinned content must read through the kernel with the provider unreachable,
/// and unpinned content must not.
///
/// Every existing pin test runs against `Engine`/`Store` directly. That covers
/// the durability layer and says nothing about the thing the milestone actually
/// promises: a file you pinned is there when the network is not. The unpinned
/// control is what stops a broken offline switch from passing this for the wrong
/// reason -- without it, a fixture that quietly kept serving would look like a
/// working pin.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a working /dev/fuse and fusermount3; run explicitly"]
async fn real_a_pinned_file_reads_offline_through_the_mount_and_an_unpinned_one_does_not() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let provider = Fixture::new();
    let mut config = account(mount.clone());
    // Reservations may not claim the whole budget and the floor is eight blocks,
    // so the fixture's own 16 MiB cache can pin nothing.
    config.cache_bytes = 128 * 1024 * 1024;
    let engine = Engine::new(config.clone(), provider.clone(), temp.path().join("state"))
        .await
        .unwrap();
    engine.start().await.unwrap();
    ready(&engine).await;

    let scope = engine.scope("home");
    let pinned = engine.node(&scope, "small.txt").await.unwrap();
    let reserved = cirrove_service::content::stored_bytes(pinned.size);
    engine
        .pin(scope.clone(), pinned.id.clone(), false, reserved)
        .await
        .unwrap()
        .expect("the budget holds one small file");
    let blocks = engine.materialise_pin(&scope, &pinned).await.unwrap();
    assert!(blocks > 0, "materialising must fetch something");

    // A fresh engine on the same directory: the in-memory block cache is empty,
    // so anything that reads afterwards can only be coming off disk. Shadowing
    // does not drop the old binding, and it holds the state directory's lock.
    engine.stop().await;
    drop(engine);
    let engine = Engine::new(config, provider.clone(), temp.path().join("state"))
        .await
        .unwrap();
    engine.start().await.unwrap();
    ready(&engine).await;
    let session = CloudFs::new(engine.clone()).unwrap().mount(&mount).unwrap();

    provider.offline.store(true, Ordering::SeqCst);
    let before = provider.reads.load(Ordering::SeqCst);

    let path = mount.join("small.txt");
    let bytes = tokio::task::spawn_blocking(move || std::fs::read(path))
        .await
        .unwrap()
        .expect("a pinned file must read with the provider unreachable");
    assert_eq!(bytes.len() as u64, pinned.size);
    assert_bytes(&bytes, 0);
    assert_eq!(
        provider.reads.load(Ordering::SeqCst),
        before,
        "the bytes came from the provider, so this proves nothing about the pin"
    );

    // The control. Same mount, same offline provider, a file nobody pinned.
    let other = mount.join("folder/deep.txt");
    let refused = tokio::task::spawn_blocking(move || std::fs::read(other))
        .await
        .unwrap();
    assert!(
        refused.is_err(),
        "an unpinned file read offline must fail, or the offline switch is not doing anything"
    );

    session.umount_and_join().unwrap();
    engine.stop().await;
}

/// Shared setup for the pin mount tests: an engine with a budget that can hold
/// something, started and ready, with the state directory returned so a restart
/// can reuse it.
async fn pin_fixture(
    temp: &tempfile::TempDir,
    mount: &std::path::Path,
) -> (Arc<Fixture>, Arc<Engine>, Account) {
    std::fs::create_dir_all(mount).unwrap();
    let provider = Fixture::new();
    let mut config = account(mount.to_path_buf());
    // Reservations may not claim the whole budget and the floor is eight blocks,
    // so the fixture's own 16 MiB cache can pin nothing at all.
    config.cache_bytes = 128 * 1024 * 1024;
    let engine = Engine::new(config.clone(), provider.clone(), temp.path().join("state"))
        .await
        .unwrap();
    engine.start().await.unwrap();
    ready(&engine).await;
    (provider, engine, config)
}

/// A recursive pin has to keep the files, not the flag.
///
/// `--recursive` was recorded in the index and nothing walked the subtree, so a
/// folder pin kept exactly nothing. This pins a folder through the daemon's own
/// request path and reads a file inside it offline through the mount.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a working /dev/fuse and fusermount3; run explicitly"]
async fn real_a_recursive_pin_keeps_the_files_under_a_folder_readable_offline() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    let (provider, engine, config) = pin_fixture(&temp, &mount).await;

    let reply = engine
        .apply_pin_request(&cirrove_service::PinRequest {
            path: Some("folder".into()),
            recursive: true,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        reply.accepted && reply.refusal.is_none(),
        "the daemon refused an ordinary folder pin: {reply:?}"
    );
    assert_eq!(reply.files, 1, "the walk must find the file inside");
    assert!(reply.complete, "every folder in this fixture is indexed");

    engine.stop().await;
    drop(engine);
    let engine = Engine::new(config, provider.clone(), temp.path().join("state"))
        .await
        .unwrap();
    engine.start().await.unwrap();
    ready(&engine).await;
    let session = CloudFs::new(engine.clone()).unwrap().mount(&mount).unwrap();

    provider.offline.store(true, Ordering::SeqCst);
    let before = provider.reads.load(Ordering::SeqCst);
    let path = mount.join("folder/deep.txt");
    let bytes = tokio::task::spawn_blocking(move || std::fs::read(path))
        .await
        .unwrap()
        .expect("a file under a recursively pinned folder must read offline");
    assert_eq!(bytes.len(), 17);
    assert_eq!(
        provider.reads.load(Ordering::SeqCst),
        before,
        "the bytes came from the provider, so this proves nothing about the pin"
    );

    session.umount_and_join().unwrap();
    engine.stop().await;
}

/// After an unpin the bytes lose their protection and become ordinary cache.
///
/// This started as a test that unpinning deletes bytes, which is wrong and the
/// artifact records why: releasing a reservation *raises* the ordinary budget,
/// so eviction is less likely afterwards, not more. What unpinning actually
/// changes is protection -- the blocks stop being the ones eviction may not
/// take. That is the property worth pinning down, because losing it would mean
/// unpinned content occupying the cache forever.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a working /dev/fuse and fusermount3; run explicitly"]
async fn real_unpinned_blocks_stop_being_protected_from_eviction() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir_all(&mount).unwrap();
    let provider = Fixture::new();
    let mut config = account(mount.clone());
    // Enough to pin the small file and little else, so one further read has to
    // evict something to make room.
    config.cache_bytes = 40 * 1024 * 1024;
    let engine = Engine::new(config, provider.clone(), temp.path().join("state"))
        .await
        .unwrap();
    engine.start().await.unwrap();
    ready(&engine).await;

    let request = cirrove_service::PinRequest {
        path: Some("small.txt".into()),
        ..Default::default()
    };
    assert!(
        engine.apply_pin_request(&request).await.unwrap().accepted,
        "the budget holds one small file"
    );
    let scope = engine.scope("home");
    let pinned = engine.node(&scope, "small.txt").await.unwrap();
    let keys = cirrove_service::content::block_keys(&scope, &pinned).unwrap();
    let cache = engine.cache_path();
    let present = |keys: &[String]| keys.iter().filter(|k| cache.join(k).exists()).count();
    assert_eq!(
        present(&keys),
        keys.len(),
        "the pin must be on disk to start"
    );

    // While pinned, filling the cache must not take it.
    let big = engine.node(&scope, "large.bin").await.unwrap();
    for start in 0..12u64 {
        let _ = engine
            .cache
            .read(
                engine.provider.as_ref(),
                &scope,
                &big,
                start * cirrove_service::content::BLOCK_SIZE as u64,
                cirrove_service::content::BLOCK_SIZE,
                &engine.cancel,
            )
            .await;
    }
    assert_eq!(
        present(&keys),
        keys.len(),
        "a pinned block was evicted while the pin was in force"
    );

    let reply = engine.apply_unpin_request(&request).await.unwrap();
    assert!(reply.accepted, "unpin was refused: {reply:?}");

    // Same pressure again. Now nothing protects those blocks.
    for start in 12..24u64 {
        let _ = engine
            .cache
            .read(
                engine.provider.as_ref(),
                &scope,
                &big,
                start * cirrove_service::content::BLOCK_SIZE as u64,
                cirrove_service::content::BLOCK_SIZE,
                &engine.cancel,
            )
            .await;
    }
    assert!(
        present(&keys) < keys.len(),
        "after unpinning, the blocks must be evictable like any other"
    );
    engine.stop().await;
}

/// A refused pin must say what to do and leave what is already kept alone.
///
/// The refusal carries the numbers and the remedy; the accepted-first-pin
/// assertion is what stops a request path that refuses everything from passing
/// this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a working /dev/fuse and fusermount3; run explicitly"]
async fn real_a_refused_pin_names_the_budget_and_leaves_the_kept_one_alone() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    let (provider, engine, _) = pin_fixture(&temp, &mount).await;
    let small = cirrove_service::PinRequest {
        path: Some("small.txt".into()),
        ..Default::default()
    };
    assert!(
        engine.apply_pin_request(&small).await.unwrap().accepted,
        "a request path that refuses everything would satisfy the rest for the wrong reason"
    );
    let reserved_before = engine.pin_status().await.unwrap()[0].reserved;

    // large.bin is three gibibytes against a 128 MiB budget.
    let refused = engine
        .apply_pin_request(&cirrove_service::PinRequest {
            path: Some("large.bin".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        !refused.accepted,
        "three gibibytes must not fit: {refused:?}"
    );
    let message = refused.refusal.expect("a refusal must carry a reason");
    assert!(
        message.contains("unpin") || message.contains("budget"),
        "a refusal has to name the action that would make room: {message}"
    );

    let pins = engine.pin_status().await.unwrap();
    assert_eq!(pins.len(), 1, "the refused pin must not have been recorded");
    assert_eq!(
        pins[0].reserved, reserved_before,
        "a refusal must not disturb what is already reserved"
    );

    let session = CloudFs::new(engine.clone()).unwrap().mount(&mount).unwrap();
    provider.offline.store(true, Ordering::SeqCst);
    let path = mount.join("small.txt");
    assert!(
        tokio::task::spawn_blocking(move || std::fs::read(path))
            .await
            .unwrap()
            .is_ok(),
        "the pin that was accepted must still be readable offline"
    );
    session.umount_and_join().unwrap();
    engine.stop().await;
}

/// Pin protection survives a restart and holds against pressure arriving at once.
///
/// Two mechanisms establish it and either one suffices: `ContentCache` is built
/// already knowing what pinning claims, and `Engine::start` republishes the
/// reservations before anything else runs. Disabling one at a time leaves this
/// test green; disabling both makes it fail with a pinned block evicted. So it
/// asserts the property, not which path provides it -- the redundancy is
/// deliberate and the earlier name for this test claimed more than it measures.
///
/// What it adds over `a_restart_republishes_pins_before_anything_can_evict` is
/// the route a user takes: `Engine::new`, a real mount, and cache pressure with
/// no pause in between.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a working /dev/fuse and fusermount3; run explicitly"]
async fn real_pin_protection_survives_a_restart_and_immediate_cache_pressure() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir_all(&mount).unwrap();
    let provider = Fixture::new();
    let mut config = account(mount.clone());
    config.cache_bytes = 40 * 1024 * 1024;
    let engine = Engine::new(config.clone(), provider.clone(), temp.path().join("state"))
        .await
        .unwrap();
    engine.start().await.unwrap();
    ready(&engine).await;
    assert!(
        engine
            .apply_pin_request(&cirrove_service::PinRequest {
                path: Some("small.txt".into()),
                ..Default::default()
            })
            .await
            .unwrap()
            .accepted
    );
    let scope = engine.scope("home");
    let pinned = engine.node(&scope, "small.txt").await.unwrap();
    let keys = cirrove_service::content::block_keys(&scope, &pinned).unwrap();
    let cache = engine.cache_path();
    engine.stop().await;
    drop(engine);

    // Rebuilt and put under pressure with no pause in between.
    let engine = Engine::new(config, provider.clone(), temp.path().join("state"))
        .await
        .unwrap();
    engine.start().await.unwrap();
    ready(&engine).await;
    let session = CloudFs::new(engine.clone()).unwrap().mount(&mount).unwrap();
    let big = engine.node(&scope, "large.bin").await.unwrap();
    for start in 0..16u64 {
        let _ = engine
            .cache
            .read(
                engine.provider.as_ref(),
                &scope,
                &big,
                start * cirrove_service::content::BLOCK_SIZE as u64,
                cirrove_service::content::BLOCK_SIZE,
                &engine.cancel,
            )
            .await;
    }
    assert_eq!(
        keys.iter().filter(|k| cache.join(k).exists()).count(),
        keys.len(),
        "a pinned block was evicted in the window between remounting and the first pin refresh"
    );

    provider.offline.store(true, Ordering::SeqCst);
    let path = mount.join("small.txt");
    let bytes = tokio::task::spawn_blocking(move || std::fs::read(path))
        .await
        .unwrap()
        .expect("the pinned file must still read offline after all that pressure");
    assert_eq!(bytes.len() as u64, pinned.size);
    session.umount_and_join().unwrap();
    engine.stop().await;
}

/// The reclamation tick must reach a mount that only reads.
///
/// Its trigger was a namespace view-count signal: it fires when a mount has held
/// more than a thousand views and since shed half of them, which is a traversal.
/// Reading content changes no view count, so on a mount that only reads the
/// condition is never true and the trim never runs -- which is what a live daemon
/// measurement showed as peak and residue being the same number, pass after pass.
///
/// This reads enough to leave real free memory in the arenas, goes quiet, and
/// waits. It fails against the previous trigger, which cannot fire here at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a working /dev/fuse and fusermount3; waits out the quiescent ticks"]
async fn real_reclamation_reaches_a_mount_that_only_reads() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let provider = Fixture::new();
    let mut config = account(mount.clone());
    config.cache_bytes = 256 * 1024 * 1024;
    let engine = Engine::new(config, provider.clone(), temp.path().join("state"))
        .await
        .unwrap();
    engine.start().await.unwrap();
    ready(&engine).await;
    let session = CloudFs::new(engine.clone()).unwrap().mount(&mount).unwrap();
    let before = cirrove_service::filesystem::allocator_trims();

    // Read enough that the allocator is holding something worth returning. The
    // view count barely moves: this is one file read repeatedly, which is the
    // shape the old trigger cannot see.
    let scope = engine.scope("home");
    let big = engine.node(&scope, "large.bin").await.unwrap();
    for round in 0..4u64 {
        for start in 0..48u64 {
            let _ = engine
                .cache
                .read(
                    engine.provider.as_ref(),
                    &scope,
                    &big,
                    start * cirrove_service::content::BLOCK_SIZE as u64,
                    cirrove_service::content::BLOCK_SIZE,
                    &engine.cancel,
                )
                .await;
        }
        println!(
            "TRIM_TRIGGER round={round} free_arena_mib={:.1}",
            cirrove_allocator::free_arena_bytes() as f64 / (1024.0 * 1024.0)
        );
    }

    // Quiet, and long enough for the quiescent gate plus a tick or two.
    let trimmed = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if cirrove_service::filesystem::allocator_trims() > before {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await;
    assert!(
        trimmed.is_ok(),
        "a mount that only reads never reclaimed; the allocator held {:.1} MiB free",
        cirrove_allocator::free_arena_bytes() as f64 / (1024.0 * 1024.0)
    );

    session.umount_and_join().unwrap();
    engine.stop().await;
}
