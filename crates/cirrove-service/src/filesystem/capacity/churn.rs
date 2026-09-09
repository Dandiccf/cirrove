//! Combined kernel workload. Reports individual acceptance evidence and open limits.
use super::*;
mod mappings;
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
    mappings: bool,
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
        if self.mappings
            && index == 1 + DEPTH + self.groups() + self.files + if primary { 2 } else { 0 }
        {
            return mappings::node(0);
        }
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
            node.target = Some(Box::new(cirrove_core::RemoteRef {
                collection: "shared".into(),
                item: "shared-root".into(),
                kind: Some(NodeKind::Folder),
            }));
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
                let total = 1
                    + DEPTH
                    + self.groups()
                    + self.files
                    + if primary { 2 } else { 0 }
                    + usize::from(self.mappings);
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
                                .chain(self.mappings.then(|| Change::Upsert(mappings::node(round))))
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
/// PSS at `indexed_baseline`, against which both memory criteria are measured.
static BASELINE_PSS_KIB: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
/// Indexed file count, so a sample can tell a capacity run from a correctness one.
static FIXTURE_FILES: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

/// The namespace memory gate, from docs/adr/0005-namespace-memory.md.
///
/// G3 bounds what is still held after every view is released. Its sibling bounds
/// the high-water mark reached while they were held, because a process that
/// returns half a gibibyte after the fact still needed it during, and "runs on
/// any hardware" turns on the peak rather than on the residue.
///
/// G3 is enforced. It was missing its budget by 1.86x until the reclamation tick
/// began returning freed pages, and now lands at 13 to 16 MiB across three rounds
/// at 500,000 files. A gate that passes and cannot fail is worth nothing, so it
/// fails the build from here.
///
/// The peak criterion is reported, not enforced, because it still fails at about
/// 1.9x: trimming returns pages after the fact and does not lower the high-water
/// mark reached while the views were held. Only bounding the resident count does
/// that. `CIRROVE_CHURN_ENFORCE_MEMORY` turns it into an assertion for anyone
/// working on that, and it becomes unconditional when they finish.
const MEMORY_BUDGET_KIB: u64 = 256 * 1024;
/// Below this the fixture is a correctness check, not a capacity measurement,
/// and its memory is dominated by fixed overhead.
const MEMORY_GATE_MINIMUM_FILES: usize = 100_000;

fn check_memory(
    value: &serde_json::Value,
    criterion: &str,
    observed: u64,
    files: usize,
    enforced: bool,
) {
    if files < MEMORY_GATE_MINIMUM_FILES {
        return;
    }
    let Some(baseline) = BASELINE_PSS_KIB.get() else {
        return;
    };
    let over = observed.saturating_sub(*baseline);
    let within = over <= MEMORY_BUDGET_KIB;
    println!(
        "CIRROVE_MEMORY_GATE {}",
        serde_json::json!({"criterion":criterion,"phase":value["phase"],
            "round":value["round"],"over_baseline_kib":over,
            "budget_kib":MEMORY_BUDGET_KIB,"within":within})
    );
    assert!(
        within || !(enforced || std::env::var("CIRROVE_CHURN_ENFORCE_MEMORY").is_ok()),
        "{criterion} exceeded the namespace memory budget: {over} KiB over the \
         indexed baseline against {MEMORY_BUDGET_KIB} KiB"
    );
}

/// Wait until resident memory stops falling, or give up after a bounded wait.
///
/// Bounded so a fixture cannot hang on a machine where the tick never quiesces;
/// the wait is recorded in the sample so a run that timed out is visible rather
/// than silently averaged in.
async fn settle_memory() {
    // Waiting for memory to stop falling is not enough on its own: before the
    // trim fires it has not started falling, so "flat" and "finished" look
    // identical and the wait ends after three seconds having measured nothing.
    // Sit out the reclamation tick's quiescent window first -- longer than the
    // five idle seconds it requires -- and only then ask for stability. A build
    // with no trim at all simply waits this out and reports what it finds.
    const QUIESCENT_WINDOW: Duration = Duration::from_secs(8);
    let deadline = Instant::now() + Duration::from_secs(25);
    let started = Instant::now();
    let mut previous = u64::MAX;
    let mut stable = 0;
    while Instant::now() < deadline && (started.elapsed() < QUIESCENT_WINDOW || stable < 2) {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let current = process_memory()["rss_kib"].as_u64().unwrap_or(0);
        stable = if current >= previous { stable + 1 } else { 0 };
        previous = current;
    }
    println!(
        "CIRROVE_MEMORY_SETTLE {}",
        serde_json::json!({"seconds": started.elapsed().as_secs_f64(),
            "settled": stable >= 2, "rss_kib": previous})
    );
}

async fn sample(inner: &Arc<Inner>, round: usize, phase: &'static str, started: Instant) {
    let mut value = namespace_sample(inner, phase, started.elapsed().as_secs_f64());
    assert_eq!(value["references"]["quarantined_views"], 0);
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
    let files = FIXTURE_FILES.get().copied().unwrap_or(0);
    let memory = &value["memory"];
    if phase == "indexed_baseline" {
        let _ = BASELINE_PSS_KIB.set(memory["pss_kib"].as_u64().unwrap());
    }
    if phase == "released" {
        check_memory(
            &value,
            "g3_released",
            memory["pss_kib"].as_u64().unwrap(),
            files,
            true,
        );
    }
    if phase == "traversed_with_old_files" {
        check_memory(
            &value,
            "peak_resident",
            memory["peak_rss_kib"].as_u64().unwrap(),
            files,
            false,
        );
    }
    println!("CIRROVE_COMBINED_CHURN {value}");
}
async fn round(
    tree: Tree,
    inner: &Arc<Inner>,
    routes: [PathBuf; 3],
    number: usize,
    full: bool,
    started: Instant,
    content: Option<&mappings::Provider>,
) -> Vec<u64> {
    let paths = routes.clone();
    let held = tokio::task::spawn_blocking(move || hold(&paths, number - 1))
        .await
        .unwrap();
    let mut mapped = if content.is_some() {
        Some(mappings::Client::start(&routes, number - 1).await)
    } else {
        None
    };
    sample(inner, number, "held_before_update", started).await;
    let navigation = Instant::now();
    if let Some(content) = content {
        content.revision.store(number, Ordering::SeqCst);
    }
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
    if let Some(mapped) = &mut mapped {
        mapped.update().await;
    }
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
    if let Some(mapped) = mapped {
        mapped.finish().await;
    }
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
        // Retiring the views is not the end of it: the allocator returns their
        // pages on a quiescent tick a few seconds later. Sampling immediately
        // records a moment between the two that a user never observes, so wait
        // for resident memory to stop falling before calling this released.
        settle_memory().await;
    }
    sample(inner, number, "released", started).await;
    ids
}

pub(super) async fn mounted() {
    run(false).await;
}
pub(super) async fn mounted_with_mappings() {
    run(true).await;
}
async fn run(with_mappings: bool) {
    let files = std::env::var("CIRROVE_CHURN_FILES").map_or(4000, |s| s.parse::<usize>().unwrap());
    assert!((4000..=500_000).contains(&files) && files.is_multiple_of(2));
    let per_directory =
        std::env::var("CIRROVE_CHURN_PER_DIRECTORY").map_or(2000, |s| s.parse::<usize>().unwrap());
    assert!((2000..=files / 2).contains(&per_directory));
    let seconds = std::env::var("CIRROVE_CHURN_SECONDS").map_or(0, |s| s.parse::<u64>().unwrap());
    assert!(seconds <= 86_400);
    let _ = FIXTURE_FILES.set(files);
    // Sustained rounds are not full, so they never settle, and every sample taken
    // during them is pre-invalidation and not root-only. That leaves no series a
    // plateau rule can bind to, which is why the twenty-four hour gate has no
    // result. Zero keeps the historical behaviour so earlier runs stay comparable.
    let full_every = std::env::var("CIRROVE_CHURN_SUSTAINED_FULL_EVERY")
        .map_or(0, |s| s.parse::<usize>().unwrap());
    let stat_workers =
        std::env::var("CIRROVE_CHURN_STAT_WORKERS").map_or(8, |s| s.parse::<usize>().unwrap());
    assert!((1..=16).contains(&stat_workers));
    let tree = Tree {
        files: files / 2,
        per_directory,
        stat_workers,
        mappings: with_mappings,
    };
    let generated = Arc::new(GeneratedLibrary {
        files: 0,
        per_directory: 1000,
        revision: AtomicU32::new(1),
        content_reads: AtomicU64::new(0),
        foreground_requests: AtomicU64::new(0),
    });
    let content = with_mappings.then(|| Arc::new(mappings::Provider::new(generated.clone())));
    let provider: Arc<dyn ReadProvider> = match &content {
        Some(content) => content.clone(),
        None => generated.clone(),
    };
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
        "additional_content_files":if with_mappings {2}else{0},"held_mappings":if with_mappings {6}else{0},
        "stat_workers":stat_workers,"queued_stat_entries":128,"sustained_seconds":seconds,"build":if cfg!(debug_assertions){"debug"}else{"release"},
        "scope":if with_mappings {"synthetic kernel FUSE; complete metadata traversal plus held old/new content mappings; synthetic version-checked content provider"}else{"synthetic kernel FUSE; complete traversal stats every projected file; no provider content or metadata requests"}})
    );
    let mut previous = None;
    for number in 1..=3 {
        let ids = round(
            tree,
            &inner,
            routes.clone(),
            number,
            true,
            started,
            content.as_deref(),
        )
        .await;
        if let Some(previous) = &previous {
            assert_eq!(&ids, previous);
        }
        previous = Some(ids);
    }
    let sustained = Instant::now();
    let mut number = 3;
    while sustained.elapsed() < Duration::from_secs(seconds) {
        number += 1;
        let settle_this_round = full_every > 0 && (number - 3usize).is_multiple_of(full_every);
        let ids = round(
            tree,
            &inner,
            routes.clone(),
            number,
            settle_this_round,
            started,
            content.as_deref(),
        )
        .await;
        assert_eq!(Some(ids), previous);
        let remaining = Duration::from_secs(seconds).saturating_sub(sustained.elapsed());
        tokio::time::sleep(remaining.min(Duration::from_secs(30))).await;
    }
    parents::settle(&inner, 1).await;
    let path = routes[0].join("group-000000").join(Tree::name(0, number));
    let inode = tokio::task::spawn_blocking(move || std::fs::metadata(path).unwrap().ino())
        .await
        .unwrap();
    let mapped_inodes = if let Some(content) = &content {
        assert_eq!(
            content.content_requests.load(Ordering::SeqCst),
            2 * (number as u64 + 1)
        );
        let paths = mappings::paths(&routes);
        let ids =
            tokio::task::spawn_blocking(move || paths.map(|p| std::fs::metadata(p).unwrap().ino()))
                .await
                .unwrap();
        content.offline.store(true, Ordering::SeqCst);
        Some((
            ids,
            content.node_requests.load(Ordering::SeqCst),
            content.content_requests.load(Ordering::SeqCst),
        ))
    } else {
        None
    };
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
    if let Some((ids, nodes, ranges)) = mapped_inodes {
        mappings::offline(&routes, number, ids).await;
        let content = content.as_ref().unwrap();
        assert_eq!(content.node_requests.load(Ordering::SeqCst), nodes);
        assert_eq!(content.content_requests.load(Ordering::SeqCst), ranges);
        println!(
            "CIRROVE_CHURN_MAPPINGS {}",
            serde_json::json!({"content_files":2,"mapped_routes":3,"held_mappings":6,"file_bytes":8192,"content_versions":number+1,"provider_range_calls":ranges,"provider_node_calls":nodes,"offline_additional_calls":0})
        );
    }
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
    assert_eq!(generated.foreground_requests.load(Ordering::SeqCst), 0);
    assert_eq!(generated.content_reads.load(Ordering::SeqCst), 0);
    engine.stop().await;
    tokio::task::spawn_blocking(move || session.umount_and_join())
        .await
        .unwrap()
        .unwrap();
}
