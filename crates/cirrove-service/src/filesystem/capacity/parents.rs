//! Real kernel lifetime checks with deep paths and duplicate linked projections.
use super::*;
use cirrove_core::RemoteRef;
use std::os::unix::fs::MetadataExt;

const DEPTH: usize = 8;
const LEAVES: usize = 2_000;

fn tree(root: &str) -> Vec<Node> {
    let make = |id: String, parent: Option<String>, name: String, kind| Node {
        id,
        parent_id: parent,
        name,
        kind,
        size: 0,
        modified_unix: 1_700_000_000,
        etag: Some("original".into()),
        content_version: None,
        target: None,
    };
    let mut nodes = vec![make(root.into(), None, root.into(), NodeKind::Folder)];
    let mut parent = root.to_owned();
    for n in 0..DEPTH {
        let id = format!("level-{n:02}");
        nodes.push(make(id.clone(), Some(parent), id.clone(), NodeKind::Folder));
        parent = id;
    }
    for n in 0..LEAVES {
        let id = format!("leaf-{n:04}");
        nodes.push(make(id.clone(), Some(parent.clone()), id, NodeKind::File));
    }
    nodes
}
fn deep_path(root: &std::path::Path) -> PathBuf {
    (0..DEPTH).fold(root.to_path_buf(), |path, n| {
        path.join(format!("level-{n:02}"))
    })
}
pub(super) async fn settle(inner: &Inner, expected: usize) {
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        // Ask the actual kernel to release unused dentries. We never manufacture
        // FORGET counts or drop active kernel references to hit a memory target.
        inner.engine.changed.notify_one();
        let remaining = inner.views.lock().unwrap().len();
        if remaining == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{remaining} namespace views remain; expected {expected}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
async fn seed(engine: &Engine, collection: &str, nodes: Vec<Node>) {
    let db = engine.db.clone();
    let scope = engine.scope(collection);
    tokio::task::spawn_blocking(move || {
        let mut store = Store::open(db).unwrap();
        let cursor = store.begin(&scope, true).unwrap();
        store
            .stage(
                &scope,
                cursor.as_ref(),
                &ChangePage {
                    changes: nodes.into_iter().map(Change::Upsert).collect(),
                    checkpoint: Checkpoint::Complete(Cursor("fixture".into())),
                },
            )
            .unwrap();
    })
    .await
    .unwrap();
}

pub(super) async fn directories_and_aliases() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let provider = Arc::new(GeneratedLibrary {
        files: 0,
        per_directory: PER_DIRECTORY,
        revision: AtomicU32::new(1),
        content_reads: AtomicU64::new(0),
        foreground_requests: AtomicU64::new(0),
    });
    let engine = Engine::new(
        account(mount.clone()),
        provider.clone(),
        temp.path().join("state"),
    )
    .await
    .unwrap();
    let mut primary = tree("root");
    for (id, name) in [("link-a", "Shared A"), ("link-b", "Shared B")] {
        primary.push(Node {
            id: id.into(),
            parent_id: Some("root".into()),
            name: name.into(),
            kind: NodeKind::Shortcut,
            target: Some(Box::new(RemoteRef {
                collection: "shared".into(),
                item: "shared-root".into(),
                kind: Some(NodeKind::Folder),
            })),
            ..primary[0].clone()
        });
    }
    seed(&engine, "capacity-drive", primary).await;
    seed(&engine, "shared", tree("shared-root")).await;
    let fs = CloudFs::new(engine.clone()).unwrap();
    let inner = fs.inner.clone();
    let session = fs.mount(&mount).unwrap();
    let paths = [
        deep_path(&mount),
        deep_path(&mount.join("Shared A")),
        deep_path(&mount.join("Shared B")),
    ];
    let paths_for_open = paths.clone();
    let (held_file, held_directory, original_inodes) = tokio::task::spawn_blocking(move || {
        let mut ids = Vec::new();
        for path in &paths_for_open {
            assert_eq!(std::fs::read_dir(path).unwrap().count(), LEAVES);
            ids.push(std::fs::metadata(path).unwrap().ino());
        }
        assert_ne!(
            ids[1], ids[2],
            "duplicate links must keep distinct projected inodes"
        );
        let held_file = std::fs::File::open(paths_for_open[0].join("leaf-0000")).unwrap();
        let mut held_directory = std::fs::read_dir(&paths_for_open[1]).unwrap();
        assert_eq!(
            held_directory.next().unwrap().unwrap().file_name(),
            "leaf-0000"
        );
        (held_file, held_directory, ids)
    })
    .await
    .unwrap();
    // Root, primary chain + file, and one alias's root + deep chain + snapshot.
    settle(&inner, 1 + DEPTH + 1 + 1 + DEPTH).await;
    assert!(
        inner
            .views
            .lock()
            .unwrap()
            .values()
            .all(|v| !v.alias.iter().any(|(_, id)| id == "link-b"))
    );
    assert_eq!(held_file.metadata().unwrap().len(), 0);

    let mut changed = tree("shared-root");
    changed
        .iter_mut()
        // This entry lies beyond the initial getdents buffer. A buffered name
        // near the beginning would not prove continued server snapshot use.
        .find(|n| n.id == "leaf-1500")
        .unwrap()
        .name = "renamed-leaf".into();
    seed(&engine, "shared", changed).await;
    engine.changed.notify_one();
    tokio::task::spawn_blocking(move || {
        let names = held_directory
            .map(|e| e.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(names.len(), LEAVES - 1);
        assert!(names.iter().any(|n| n == "leaf-1500"));
        assert!(!names.iter().any(|n| n == "renamed-leaf"));
    })
    .await
    .unwrap();
    settle(&inner, 1 + DEPTH + 1).await;
    drop(held_file);
    settle(&inner, 1).await;

    let paths_for_revisit = paths.clone();
    tokio::task::spawn_blocking(move || {
        for (index, path) in paths_for_revisit.iter().enumerate() {
            assert_eq!(
                std::fs::metadata(path).unwrap().ino(),
                original_inodes[index]
            );
            let names = std::fs::read_dir(path)
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect::<Vec<_>>();
            assert_eq!(names.len(), LEAVES);
            if index > 0 {
                assert!(names.iter().any(|n| n == "renamed-leaf"));
                assert!(!names.iter().any(|n| n == "leaf-1500"));
            }
        }
    })
    .await
    .unwrap();
    settle(&inner, 1).await;
    assert_eq!(provider.foreground_requests.load(Ordering::SeqCst), 0);
    assert_eq!(provider.content_reads.load(Ordering::SeqCst), 0);
    println!(
        "CIRROVE_DIRECTORY_PARENTS {}",
        serde_json::json!({"fixture":"synthetic kernel FUSE with deep paths and duplicate linked-drive projections", "depth":DEPTH,"leaves_per_directory":LEAVES,"retained_after_close":inner.views.lock().unwrap().len(),"offline_revisit_provider_requests":0,"held_snapshot_survived_remote_rename":true})
    );
    engine.stop().await;
    tokio::task::spawn_blocking(move || session.umount_and_join())
        .await
        .unwrap()
        .unwrap();
}

pub(super) async fn targeted_aliases() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let provider = Arc::new(GeneratedLibrary {
        files: 0,
        per_directory: 1000,
        revision: AtomicU32::new(1),
        content_reads: AtomicU64::new(0),
        foreground_requests: AtomicU64::new(0),
    });
    let engine = Engine::new(
        account(mount.clone()),
        provider.clone(),
        temp.path().join("state"),
    )
    .await
    .unwrap();
    let root = tree("root").remove(0);
    let mut a = root.clone();
    a.id = "link-a".into();
    a.parent_id = Some("root".into());
    a.name = "Alias A".into();
    a.kind = NodeKind::Shortcut;
    a.target = Some(Box::new(RemoteRef {
        collection: "shared".into(),
        item: "shared-root".into(),
        kind: Some(NodeKind::Folder),
    }));
    let mut b = a.clone();
    b.id = "link-b".into();
    b.name = "Alias B".into();
    seed(&engine, "capacity-drive", vec![root, a.clone(), b]).await;
    let root = tree("shared-root").remove(0);
    let mut leaf = root.clone();
    leaf.id = "leaf".into();
    leaf.parent_id = Some("shared-root".into());
    leaf.name = "leaf".into();
    leaf.kind = NodeKind::File;
    seed(&engine, "shared", vec![root.clone(), leaf.clone()]).await;
    let fs = CloudFs::new(engine.clone()).unwrap();
    let inner = fs.inner.clone();
    let session = fs.mount(&mount).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while inner.invalidation_metrics.batches.load(Ordering::Relaxed) == 0 {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let path = mount.clone();
    let held = tokio::task::spawn_blocking(move || {
        let a = std::fs::File::open(path.join("Alias A/leaf")).unwrap();
        let b = std::fs::File::open(path.join("Alias B/leaf")).unwrap();
        assert_ne!(a.metadata().unwrap().ino(), b.metadata().unwrap().ino());
        (a, b)
    })
    .await
    .unwrap();
    let before = inner.invalidation_metrics.entries.load(Ordering::Relaxed);
    leaf.name = "renamed".into();
    leaf.size = 7;
    leaf.etag = Some("changed".into());
    Store::open(&engine.db)
        .unwrap()
        .observe_node(&engine.scope("shared"), &leaf)
        .unwrap();
    engine.changed.metadata();
    let deadline = Instant::now() + Duration::from_secs(5);
    while inner.invalidation_metrics.entries.load(Ordering::Relaxed) < before + 4 {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let path = mount.clone();
    tokio::task::spawn_blocking(move || {
        for alias in ["Alias A", "Alias B"] {
            assert_eq!(
                std::fs::metadata(path.join(alias).join("leaf"))
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::NotFound
            );
            assert_eq!(
                std::fs::metadata(path.join(alias).join("renamed"))
                    .unwrap()
                    .len(),
                7
            );
        }
        assert_eq!(held.0.metadata().unwrap().len(), 0);
        assert_eq!(held.1.metadata().unwrap().len(), 0);
        drop(held);
    })
    .await
    .unwrap();
    let before = inner.invalidation_metrics.entries.load(Ordering::Relaxed);
    a.name = "Moved alias".into();
    Store::open(&engine.db)
        .unwrap()
        .observe_node(&engine.scope("capacity-drive"), &a)
        .unwrap();
    engine.changed.metadata();
    let deadline = Instant::now() + Duration::from_secs(5);
    while inner.invalidation_metrics.entries.load(Ordering::Relaxed) == before {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let path = mount.clone();
    tokio::task::spawn_blocking(move || {
        assert_eq!(
            std::fs::metadata(path.join("Alias A")).unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
        assert_eq!(
            std::fs::metadata(path.join("Moved alias/renamed"))
                .unwrap()
                .len(),
            7
        );
        assert_eq!(
            std::fs::metadata(path.join("Alias B/renamed"))
                .unwrap()
                .len(),
            7
        );
    })
    .await
    .unwrap();
    // A replacement feed uses a scope mark, including all aliases of that scope.
    let before = inner.invalidation_metrics.entries.load(Ordering::Relaxed);
    leaf.name = "after-reset".into();
    seed(&engine, "shared", vec![root, leaf]).await;
    engine.changed.metadata();
    let deadline = Instant::now() + Duration::from_secs(5);
    while inner.invalidation_metrics.entries.load(Ordering::Relaxed) == before {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let path = mount.clone();
    tokio::task::spawn_blocking(move || {
        assert_eq!(
            std::fs::metadata(path.join("Moved alias/after-reset"))
                .unwrap()
                .len(),
            7
        );
        assert_eq!(
            std::fs::metadata(path.join("Alias B/after-reset"))
                .unwrap()
                .len(),
            7
        );
    })
    .await
    .unwrap();
    settle(&inner, 1).await;
    assert_eq!(provider.foreground_requests.load(Ordering::SeqCst), 0);
    assert_eq!(provider.content_reads.load(Ordering::SeqCst), 0);
    engine.stop().await;
    tokio::task::spawn_blocking(move || session.umount_and_join())
        .await
        .unwrap()
        .unwrap();
}
