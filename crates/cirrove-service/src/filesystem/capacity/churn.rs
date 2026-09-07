//! Combined kernel workload. Reports individual acceptance evidence and open limits.
use super::*;
use std::{
    fs::{File, ReadDir},
    os::unix::fs::MetadataExt,
    path::Path,
};
const DEPTH: usize = 12;
const RENAME_FILE: usize = 1500;
const HELD_PER_ROUTE: usize = 8;
#[derive(Clone, Copy)]
struct Tree {
    files: usize,
    per_directory: usize,
    stat_workers: usize,
}
impl Tree {
    fn groups(self) -> usize {
        self.files.div_ceil(self.per_directory)
    }
    fn name(file: usize, round: usize) -> String {
        if file == RENAME_FILE && round > 0 {
            format!("renamed-leaf-{round:08}")
        } else {
            format!("Projektunterlagen – Übersicht {file:08}.txt")
        }
    }
    fn file(self, file: usize, round: usize) -> Node {
        Node {
            id: format!("file-{file:08}"),
            parent_id: Some(format!("group-{:06}", file / self.per_directory)),
            name: Self::name(file, round),
            kind: NodeKind::File,
            size: 0,
            modified_unix: 1_700_000_000,
            etag: Some(format!(
                "revision-{}",
                if file < HELD_PER_ROUTE || file == RENAME_FILE {
                    round
                } else {
                    0
                }
            )),
            content_version: None,
            target: None,
        }
    }
    fn node(self, index: usize, primary: bool) -> Node {
        let root = if primary { "root" } else { "shared-root" };
        let mut node = self.file(0, 0);
        node.kind = NodeKind::Folder;
        if index == 0 {
            node.id = root.into();
            node.name = root.into();
            node.parent_id = None;
        } else if index <= DEPTH {
            let level = index - 1;
            node.id = format!("level-{level:02}");
            node.name = node.id.clone();
            node.parent_id = Some(if level == 0 {
                root.into()
            } else {
                format!("level-{:02}", level - 1)
            });
        } else if index <= DEPTH + self.groups() {
            node.id = format!("group-{:06}", index - DEPTH - 1);
            node.name = node.id.clone();
            node.parent_id = Some(format!("level-{:02}", DEPTH - 1));
        } else if index <= DEPTH + self.groups() + self.files {
            return self.file(index - DEPTH - self.groups() - 1, 0);
        } else {
            assert!(primary);
            let alias = index - DEPTH - self.groups() - self.files - 1;
            node.id = format!("shortcut-{alias}");
            node.name = format!("Shared {alias}");
            node.parent_id = Some("root".into());
            node.kind = NodeKind::Shortcut;
            node.target = Some(cirrove_core::RemoteRef {
                collection: "shared".into(),
                item: "shared-root".into(),
                kind: Some(NodeKind::Folder),
            });
        }
        node
    }
    fn routes(mount: &Path) -> [PathBuf; 3] {
        [
            mount.to_path_buf(),
            mount.join("Shared 0"),
            mount.join("Shared 1"),
        ]
        .map(|root| (0..DEPTH).fold(root, |p, n| p.join(format!("level-{n:02}"))))
    }
    async fn seed(self, engine: &Engine) {
        let db = engine.db.clone();
        let scopes = [engine.scope("capacity-drive"), engine.scope("shared")];
        tokio::task::spawn_blocking(move || {
            let mut store = Store::open(db).unwrap();
            for (index, scope) in scopes.iter().enumerate() {
                let primary = index == 0;
                let total = 1 + DEPTH + self.groups() + self.files + if primary { 2 } else { 0 };
                let mut cursor = store.begin(scope, true).unwrap();
                for start in (0..total).step_by(PAGE) {
                    let end = (start + PAGE).min(total);
                    let next = Cursor(format!("fixture-{end}"));
                    let page = ChangePage {
                        changes: (start..end)
                            .map(|n| Change::Upsert(self.node(n, primary)))
                            .collect(),
                        checkpoint: if end == total {
                            Checkpoint::Complete(next.clone())
                        } else {
                            Checkpoint::Continue(next.clone())
                        },
                    };
                    store.stage(scope, cursor.as_ref(), &page).unwrap();
                    cursor = Some(next);
                }
            }
        })
        .await
        .unwrap();
    }
    async fn update(self, engine: &Engine, round: usize) {
        let db = engine.db.clone();
        let scopes = [engine.scope("capacity-drive"), engine.scope("shared")];
        tokio::task::spawn_blocking(move || {
            let mut store = Store::open(db).unwrap();
            for scope in scopes {
                let cursor = store.begin(&scope, false).unwrap();
                store
                    .stage(
                        &scope,
                        cursor.as_ref(),
                        &ChangePage {
                            changes: (0..HELD_PER_ROUTE)
                                .chain(std::iter::once(RENAME_FILE))
                                .map(|file| Change::Upsert(self.file(file, round)))
                                .collect(),
                            checkpoint: Checkpoint::Complete(Cursor(format!("round-{round}"))),
                        },
                    )
                    .unwrap();
            }
        })
        .await
        .unwrap();
        engine.changed.metadata();
    }
}
struct Held {
    files: Vec<(File, u64)>,
    directories: Vec<ReadDir>,
    directory_inodes: Vec<u64>,
}
fn hold(routes: &[PathBuf; 3], round: usize) -> Held {
    let mut held = Held {
        files: vec![],
        directories: vec![],
        directory_inodes: vec![],
    };
    for route in routes {
        let path = route.join("group-000000");
        held.directory_inodes
            .push(std::fs::metadata(&path).unwrap().ino());
        for file in 0..HELD_PER_ROUTE {
            let file = File::open(path.join(Tree::name(file, round))).unwrap();
            let inode = file.metadata().unwrap().ino();
            held.files.push((file, inode));
        }
        // Warm an unchanged lookup before measuring navigation during the update.
        assert_eq!(
            std::fs::metadata(path.join(Tree::name(42, round)))
                .unwrap()
                .len(),
            0
        );
        let mut directory = std::fs::read_dir(path).unwrap();
        assert!(
            directory.next().unwrap().unwrap().file_name()
                != Tree::name(RENAME_FILE, round).as_str()
        );
        held.directories.push(directory);
    }
    assert_ne!(held.directory_inodes[1], held.directory_inodes[2]);
    held
}
async fn sample(inner: &Arc<Inner>, round: usize, phase: &'static str, started: Instant) {
    let mut value = namespace_sample(inner, phase, started.elapsed().as_secs_f64());
    let diagnostics = inner.views.lock().unwrap().diagnostics();
    assert_eq!(diagnostics["quarantined_views"], 0);
    value["references"] = diagnostics;
    let payloads = inner.payloads.clone();
    value["projection_payloads"] = tokio::task::spawn_blocking(move || payloads.diagnostics())
        .await
        .unwrap();
    assert!(
        value["projection_payloads"]["cache_accounted_bytes"]
            .as_u64()
            .unwrap()
            <= value["projection_payloads"]["cache_limit_bytes"]
                .as_u64()
                .unwrap()
    );
    assert!(value["open_files"].as_u64().unwrap() <= 32);
    assert!(value["open_directory_handles"].as_u64().unwrap() <= 8);
    value["round"] = round.into();
    let db = inner.engine.db.clone();
    value["store"] = tokio::task::spawn_blocking(move || {
        let conn = rusqlite::Connection::open(&db).unwrap();
        let inodes = conn
            .query_row("SELECT count(*) FROM inodes", [], |r| r.get::<_, i64>(0))
            .unwrap();
        let pages = conn
            .pragma_query_value(None, "page_count", |r| r.get::<_, i64>(0))
            .unwrap();
        let free = conn
            .pragma_query_value(None, "freelist_count", |r| r.get::<_, i64>(0))
            .unwrap();
        let wal = PathBuf::from(format!("{}-wal", db.display()));
        serde_json::json!({"inode_rows":inodes,"database_pages":pages,"freelist_pages":free,
            "database_file_bytes":std::fs::metadata(&db).unwrap().len(),
            "wal_file_bytes":std::fs::metadata(wal).map_or(0,|m|m.len())})
    })
    .await
    .unwrap();
    value["invalidation_marks"] = inner
        .invalidation_metrics
        .marks
        .load(Ordering::Relaxed)
        .into();
    value["invalidation_notifications"] = inner
        .invalidation_metrics
        .entries
        .load(Ordering::Relaxed)
        .into();
    value["max_invalidation_batch"] = inner
        .invalidation_metrics
        .max_batch
        .load(Ordering::Relaxed)
        .into();
    assert!(value["max_invalidation_batch"].as_u64().unwrap() <= 128);
    println!("CIRROVE_COMBINED_CHURN {value}");
}
async fn round(
    tree: Tree,
    inner: &Arc<Inner>,
    routes: [PathBuf; 3],
    number: usize,
    full: bool,
    started: Instant,
) -> Vec<u64> {
    let paths = routes.clone();
    let held = tokio::task::spawn_blocking(move || hold(&paths, number - 1))
        .await
        .unwrap();
    sample(inner, number, "held_before_update", started).await;
    let navigation = Instant::now();
    tree.update(&inner.engine, number).await;
    let paths = routes.clone();
    let old_first = held.files[0].1;
    tokio::task::spawn_blocking(move || {
        for path in &paths {
            assert_eq!(
                std::fs::metadata(path.join("group-000000").join(Tree::name(42, number)))
                    .unwrap()
                    .len(),
                0
            );
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while std::fs::metadata(paths[0].join("group-000000").join(Tree::name(0, number)))
            .unwrap()
            .ino()
            == old_first
        {
            assert!(
                Instant::now() < deadline,
                "new content-version inode did not become visible"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    })
    .await
    .unwrap();
    let navigation_ms = navigation.elapsed().as_secs_f64() * 1000.0;
    assert!(
        navigation_ms < 500.0,
        "cached navigation exceeded 500 ms during committed changes: {navigation_ms}"
    );
    let paths = routes.clone();
    let (files, ids) = tokio::task::spawn_blocking(move || {
        for (directory, path) in held.directories.into_iter().zip(&paths) {
            let mut count = 1;
            let mut old_name = false;
            for entry in directory {
                let name = entry.unwrap().file_name();
                old_name |= name == Tree::name(RENAME_FILE, number - 1).as_str();
                assert_ne!(name, Tree::name(RENAME_FILE, number).as_str());
                count += 1;
            }
            assert!(old_name, "continued snapshot lost the old unbuffered entry");
            assert_eq!(count, tree.per_directory.min(tree.files));
            assert_eq!(
                std::fs::metadata(
                    path.join("group-000000")
                        .join(Tree::name(RENAME_FILE, number))
                )
                .unwrap()
                .len(),
                0
            );
        }
        for (file, inode) in &held.files {
            assert_eq!(file.metadata().unwrap().ino(), *inode);
            assert_eq!(file.metadata().unwrap().len(), 0);
        }
        (held.files, held.directory_inodes)
    })
    .await
    .unwrap();
    // RELEASEDIR is asynchronous. Wait for ordinary close completion before
    // attributing snapshot storage to the traversal rather than an old handle.
    let deadline = Instant::now() + Duration::from_secs(5);
    while inner.directory_budget.usage() != (0, 0) {
        assert!(Instant::now() < deadline, "snapshot close did not finish");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let traversal = Instant::now();
    let paths = routes.clone();
    let visited = tokio::task::spawn_blocking(move || {
        // Every entry is statted. A bounded application queue models a file
        // manager inspecting entries concurrently without collecting the tree.
        std::thread::scope(|scope| {
            let (sender, receiver) = std::sync::mpsc::sync_channel::<std::fs::DirEntry>(128);
            let receiver = Arc::new(Mutex::new(receiver));
            let workers = (0..tree.stat_workers)
                .map(|_| {
                    let receiver = receiver.clone();
                    scope.spawn(move || {
                        let mut count = 0;
                        loop {
                            let entry = receiver.lock().unwrap().recv();
                            let Ok(entry) = entry else {
                                break;
                            };
                            assert_eq!(entry.metadata().unwrap().len(), 0);
                            count += 1;
                        }
                        count
                    })
                })
                .collect::<Vec<_>>();
            for path in paths {
                for group in 0..if full { tree.groups() } else { 1 } {
                    for entry in std::fs::read_dir(path.join(format!("group-{group:06}"))).unwrap()
                    {
                        sender.send(entry.unwrap()).unwrap();
                    }
                }
            }
            drop(sender);
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .sum::<usize>()
        })
    })
    .await
    .unwrap();
    assert_eq!(
        visited,
        3 * if full {
            tree.files
        } else {
            tree.files.min(tree.per_directory)
        }
    );
    sample(inner, number, "traversed_with_old_files", started).await;
    println!(
        "CIRROVE_CHURN_ROUND {}",
        serde_json::json!({"round":number,"full_traversal":full,
        "stat_operations":visited,"navigation_ms":navigation_ms,"traversal_seconds":traversal.elapsed().as_secs_f64()})
    );
    drop(files);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !inner.files.lock().unwrap().is_empty() || inner.directory_budget.usage() != (0, 0) {
        assert!(
            Instant::now() < deadline,
            "file or snapshot close did not finish"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    if full {
        parents::settle(inner, 1).await;
    }
    sample(inner, number, "released", started).await;
    ids
}

pub(super) async fn mounted() {
    let files = std::env::var("CIRROVE_CHURN_FILES").map_or(4000, |s| s.parse::<usize>().unwrap());
    assert!((4000..=500_000).contains(&files) && files.is_multiple_of(2));
    let per_directory =
        std::env::var("CIRROVE_CHURN_PER_DIRECTORY").map_or(2000, |s| s.parse::<usize>().unwrap());
    assert!((2000..=files / 2).contains(&per_directory));
    let seconds = std::env::var("CIRROVE_CHURN_SECONDS").map_or(0, |s| s.parse::<u64>().unwrap());
    assert!(seconds <= 86_400);
    let stat_workers =
        std::env::var("CIRROVE_CHURN_STAT_WORKERS").map_or(8, |s| s.parse::<usize>().unwrap());
    assert!((1..=16).contains(&stat_workers));
    let tree = Tree {
        files: files / 2,
        per_directory,
        stat_workers,
    };
    let provider = Arc::new(GeneratedLibrary {
        files: 0,
        per_directory: 1000,
        revision: AtomicU32::new(1),
        content_reads: AtomicU64::new(0),
        foreground_requests: AtomicU64::new(0),
    });
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let state = temp.path().join("state");
    let engine = Engine::new(account(mount.clone()), provider.clone(), state.clone())
        .await
        .unwrap();
    tree.seed(&engine).await;
    let fs = CloudFs::new(engine.clone()).unwrap();
    let inner = fs.inner.clone();
    let session = fs.mount(&mount).unwrap();
    let started = Instant::now();
    let routes = Tree::routes(&mount);
    sample(&inner, 0, "indexed_baseline", started).await;
    println!(
        "CIRROVE_CHURN_CONFIG {}",
        serde_json::json!({"indexed_files":files,"projected_files":tree.files*3,
        "depth":DEPTH,"routes":3,"files_per_directory":per_directory,"held_files":24,"held_snapshots":3,
        "stat_workers":stat_workers,"queued_stat_entries":128,"sustained_seconds":seconds,"build":if cfg!(debug_assertions){"debug"}else{"release"},
        "scope":"synthetic kernel FUSE; complete traversal stats every projected file; no provider content or metadata requests"})
    );
    let mut previous = None;
    for number in 1..=3 {
        let ids = round(tree, &inner, routes.clone(), number, true, started).await;
        if let Some(previous) = &previous {
            assert_eq!(&ids, previous);
        }
        previous = Some(ids);
    }
    let sustained = Instant::now();
    let mut number = 3;
    while sustained.elapsed() < Duration::from_secs(seconds) {
        number += 1;
        let ids = round(tree, &inner, routes.clone(), number, false, started).await;
        assert_eq!(Some(ids), previous);
        let remaining = Duration::from_secs(seconds).saturating_sub(sustained.elapsed());
        tokio::time::sleep(remaining.min(Duration::from_secs(30))).await;
    }
    parents::settle(&inner, 1).await;
    let path = routes[0].join("group-000000").join(Tree::name(0, number));
    let inode = tokio::task::spawn_blocking(move || std::fs::metadata(path).unwrap().ino())
        .await
        .unwrap();
    engine.stop().await;
    tokio::task::spawn_blocking(move || session.umount_and_join())
        .await
        .unwrap()
        .unwrap();
    drop(inner);
    drop(engine);
    let engine = Engine::new(account(mount.clone()), provider.clone(), state)
        .await
        .unwrap();
    let fs = CloudFs::new(engine.clone()).unwrap();
    let inner = fs.inner.clone();
    let session = fs.mount(&mount).unwrap();
    let routes = Tree::routes(&mount);
    tokio::task::spawn_blocking(move || {
        assert_eq!(
            std::fs::metadata(routes[0].join("group-000000").join(Tree::name(0, number)))
                .unwrap()
                .ino(),
            inode
        );
        for (route, expected) in routes.iter().zip(previous.unwrap()) {
            assert_eq!(
                std::fs::metadata(route.join("group-000000")).unwrap().ino(),
                expected
            );
            assert_eq!(
                std::fs::metadata(
                    route
                        .join("group-000000")
                        .join(Tree::name(RENAME_FILE, number))
                )
                .unwrap()
                .len(),
                0
            );
        }
    })
    .await
    .unwrap();
    parents::settle(&inner, 1).await;
    sample(&inner, number, "offline_remount", started).await;
    assert_eq!(provider.foreground_requests.load(Ordering::SeqCst), 0);
    assert_eq!(provider.content_reads.load(Ordering::SeqCst), 0);
    engine.stop().await;
    tokio::task::spawn_blocking(move || session.umount_and_join())
        .await
        .unwrap()
        .unwrap();
}
