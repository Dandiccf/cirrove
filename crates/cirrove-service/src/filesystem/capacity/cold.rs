//! Actual mounted cold pagination, interrupted fetching and offline revisit.
use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize};
struct ColdLibrary {
    generated: GeneratedLibrary,
    offline: AtomicBool,
    pause: AtomicBool,
    entered: tokio::sync::Notify,
    pages: AtomicUsize,
}
#[async_trait]
impl MetadataProvider for ColdLibrary {
    fn provider_id(&self) -> &'static str {
        "namespace-capacity-fixture"
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
impl ReadProvider for ColdLibrary {
    async fn node(
        &self,
        _: &Scope,
        id: &str,
        _: &CancellationToken,
    ) -> Result<Node, ProviderError> {
        if id == "root" {
            Ok(self.generated.node_at(0))
        } else {
            Err(ProviderError::NotFound)
        }
    }
    async fn children(
        &self,
        _: &Scope,
        parent: &str,
        cursor: Option<&Cursor>,
        cancel: &CancellationToken,
    ) -> Result<DirectoryPage, ProviderError> {
        self.pages.fetch_add(1, Ordering::SeqCst);
        if self.offline.load(Ordering::SeqCst) {
            return Err(ProviderError::Unavailable);
        }
        assert_eq!(parent, "directory-000000");
        if cursor.is_some() && self.pause.load(Ordering::SeqCst) {
            self.entered.notify_one();
            cancel.cancelled().await;
            return Err(ProviderError::Cancelled);
        }
        let start = cursor.map_or(0, |c| c.0.parse::<usize>().unwrap());
        let end = (start + PAGE).min(self.generated.files);
        Ok(DirectoryPage {
            nodes: (start..end)
                .map(|i| self.generated.node_at(i + 2))
                .collect(),
            next: (end < self.generated.files).then(|| Cursor(end.to_string())),
        })
    }
    async fn read_range(
        &self,
        _: &Scope,
        _: &Node,
        _: u64,
        _: u32,
        _: &CancellationToken,
    ) -> Result<Vec<u8>, ProviderError> {
        self.generated.content_reads.fetch_add(1, Ordering::SeqCst);
        Err(ProviderError::Unavailable)
    }
}
fn provider(files: usize, pause: bool) -> Arc<ColdLibrary> {
    Arc::new(ColdLibrary {
        generated: GeneratedLibrary {
            files,
            per_directory: files,
            revision: AtomicU32::new(1),
            content_reads: AtomicU64::new(0),
            foreground_requests: AtomicU64::new(0),
        },
        offline: AtomicBool::new(false),
        pause: AtomicBool::new(pause),
        entered: tokio::sync::Notify::new(),
        pages: AtomicUsize::new(0),
    })
}
async fn setup(temp: &tempfile::TempDir, provider: Arc<ColdLibrary>) -> (Arc<Engine>, PathBuf) {
    let mount = temp.path().join("mount");
    std::fs::create_dir_all(&mount).unwrap();
    let engine = Engine::new(
        account(mount.clone()),
        provider.clone(),
        temp.path().join("state"),
    )
    .await
    .unwrap();
    let mut store = Store::open(&engine.db).unwrap();
    let scope = engine.scope("capacity-drive");
    store
        .observe_node(&scope, &provider.generated.node_at(0))
        .unwrap();
    store
        .observe_directory(&scope, "root", &[provider.generated.node_at(1)])
        .unwrap();
    (engine, mount)
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn abandoned_cold_fetch_discards_pages_and_restarts_from_first_page() {
    let temp = tempfile::tempdir().unwrap();
    let provider = provider(3000, true);
    let (engine, _) = setup(&temp, provider.clone()).await;
    let scope = engine.scope("capacity-drive");
    let e = engine.clone();
    let s = scope.clone();
    let fetching = tokio::spawn(async move {
        e.with_children(&s, "directory-000000", |rows| rows.count())
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), provider.entered.notified())
        .await
        .unwrap();
    assert!(
        Store::open(&engine.db)
            .unwrap()
            .children(&scope, "directory-000000")
            .unwrap()
            .is_none()
    );
    fetching.abort();
    assert!(fetching.await.unwrap_err().is_cancelled());
    provider.pause.store(false, Ordering::SeqCst);
    assert_eq!(
        engine
            .with_children(&scope, "directory-000000", |rows| rows.fold(
                0,
                |count, row| {
                    row.unwrap();
                    count + 1
                }
            ))
            .await
            .unwrap(),
        3000
    );
    assert_eq!(provider.pages.load(Ordering::SeqCst), 5);
    provider.offline.store(true, Ordering::SeqCst);
    assert_eq!(
        engine
            .child(
                &scope,
                "directory-000000",
                "Projektunterlagen – Übersicht 00002999.txt"
            )
            .await
            .unwrap()
            .size,
        0
    );
    engine.stop().await;
}
pub(super) async fn mounted() {
    let files = std::env::var("CIRROVE_COLD_DIRECTORY_FILES")
        .map_or(10_000, |s| s.parse::<usize>().unwrap());
    assert!((1000..=500_000).contains(&files));
    let temp = tempfile::tempdir().unwrap();
    let provider = provider(files, false);
    let (engine, mount) = setup(&temp, provider.clone()).await;
    let fs = CloudFs::new(engine.clone()).unwrap();
    let inner = fs.inner.clone();
    let session = fs.mount(&mount).unwrap();
    let before = process_memory();
    let started = Instant::now();
    let path = mount.join("directory-000000");
    let sample = inner.clone();
    let held = tokio::task::spawn_blocking(move || {
        let mut entries = std::fs::read_dir(path).unwrap();
        entries.next().unwrap().unwrap();
        report(namespace_sample(
            &sample,
            "cold_first_entry",
            started.elapsed().as_secs_f64(),
        ));
        assert_eq!(
            entries.by_ref().fold(0, |count, entry| {
                entry.unwrap();
                count + 1
            }) + 1,
            files
        );
        entries
    })
    .await
    .unwrap();
    let after = process_memory();
    let elapsed = started.elapsed();
    drop(held);
    let released = Instant::now() + Duration::from_secs(5);
    while inner.directory_budget.usage() != (0, 0) {
        assert!(
            Instant::now() < released,
            "closed snapshot was not released"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(provider.pages.load(Ordering::SeqCst), files.div_ceil(PAGE));
    assert_eq!(provider.generated.content_reads.load(Ordering::SeqCst), 0);
    let count = provider.pages.load(Ordering::SeqCst);
    provider.offline.store(true, Ordering::SeqCst);
    let path = mount.join("directory-000000");
    tokio::task::spawn_blocking(move || {
        assert_eq!(
            std::fs::read_dir(path).unwrap().fold(0, |count, entry| {
                entry.unwrap();
                count + 1
            }),
            files
        )
    })
    .await
    .unwrap();
    assert_eq!(provider.pages.load(Ordering::SeqCst), count);
    assert!(
        Store::open(&engine.db)
            .unwrap()
            .cursor(&engine.scope("capacity-drive"))
            .unwrap()
            .is_none()
    );
    println!(
        "CIRROVE_COLD_PUBLICATION {}",
        serde_json::json!({"fixture":"actual kernel FUSE; generated foreground pages, no completed delta index or cloud account", "files":files,"page_entries":PAGE,"provider_pages":count,"build":if cfg!(debug_assertions){"debug"}else{"release"},"cold_enumeration_seconds":elapsed.as_secs_f64(),"memory_before":before,"memory_after":after})
    );
    assert!(
        after["peak_rss_kib"]
            .as_u64()
            .unwrap()
            .saturating_sub(before["rss_kib"].as_u64().unwrap())
            < 256 * 1024,
        "cold publication exceeded the synthetic RSS budget"
    );
    engine.stop().await;
    tokio::task::spawn_blocking(move || session.umount_and_join())
        .await
        .unwrap()
        .unwrap();
    drop(inner);
    drop(engine);
    let (restarted, mount) = setup(&temp, provider.clone()).await;
    let session = CloudFs::new(restarted.clone())
        .unwrap()
        .mount(&mount)
        .unwrap();
    tokio::task::spawn_blocking(move || {
        assert_eq!(
            std::fs::read_dir(mount.join("directory-000000"))
                .unwrap()
                .fold(0, |count, entry| {
                    entry.unwrap();
                    count + 1
                }),
            files
        )
    })
    .await
    .unwrap();
    assert_eq!(provider.pages.load(Ordering::SeqCst), count);
    restarted.stop().await;
    tokio::task::spawn_blocking(move || session.umount_and_join())
        .await
        .unwrap()
        .unwrap();
}
