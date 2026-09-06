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
use cirrove_store::Store;
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
            link.target = Some(RemoteRef {
                collection: "library".into(),
                item: "shared".into(),
                kind: Some(NodeKind::Folder),
            });
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
    async fn changes(
        &self,
        scope: &Scope,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> Result<ChangePage, ProviderError> {
        self.online()?;
        Ok(ChangePage {
            changes: self
                .nodes
                .read()
                .await
                .iter()
                .filter(|((drive, _), _)| drive == &scope.collection)
                .map(|(_, node)| Change::Upsert(node.clone()))
                .collect(),
            checkpoint: Checkpoint::Complete(Cursor("checkpoint".into())),
        })
    }
}
#[async_trait]
impl ReadProvider for Fixture {
    async fn node(
        &self,
        scope: &Scope,
        id: &str,
        _: &CancellationToken,
    ) -> Result<Node, ProviderError> {
        self.online()?;
        self.nodes
            .read()
            .await
            .get(&(scope.collection.clone(), id.into()))
            .cloned()
            .ok_or(ProviderError::NotFound)
    }
    async fn children(
        &self,
        scope: &Scope,
        parent: &str,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> Result<DirectoryPage, ProviderError> {
        self.online()?;
        Ok(DirectoryPage {
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
        })
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
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let feeds = engine.health().await;
            if feeds.len() == 2 && feeds.iter().all(|feed| feed.state == "ready") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
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
    assert_eq!(Store::open(db).unwrap().oldest_blocks().unwrap().len(), 1);
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
    assert!(
        tokio::process::Command::new("fusermount3")
            .arg("-u")
            .arg(&mount)
            .status()
            .await
            .unwrap()
            .success()
    );
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
    let (manager, _worker) = cirrove_service::manager::Manager::start_with_provider(
        root.join("state"),
        CancellationToken::new(),
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
    std::future::pending::<()>().await;
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
import mmap,sys,resource
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
assert resource.getrusage(resource.RUSAGE_SELF).ru_maxrss < 128*1024
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
    let mut readers = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        // Open first so this checks content admission independently of metadata.
        let files = (0..READERS)
            .map(|i| std::fs::File::open(path.join(format!("thumbnail-{i:03}.bin"))))
            .collect::<Result<Vec<_>, _>>()?;
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
    let result = tokio::time::timeout(Duration::from_secs(20), async {
        while provider.reads.load(Ordering::SeqCst) < 4 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let mut listings_ms = vec![];
        for _ in 0..20 {
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
            tokio::time::sleep(Duration::from_millis(10)).await;
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
                "directory_p50_ms": listings_ms[9], "directory_p95_ms": listings_ms[18],
            })
        );
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("thumbnail burst timed out")
    .and_then(|result| result);
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
        link.target = Some(RemoteRef {
            collection: "home".into(),
            item: "small.txt".into(),
            kind: Some(NodeKind::File),
        });
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
