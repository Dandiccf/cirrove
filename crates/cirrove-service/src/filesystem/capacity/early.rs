//! A real directory must expose useful entries before its full snapshot exists.
use super::*;

pub(super) async fn run(close_early: bool) {
    let files = 2_000;
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let provider = Arc::new(GeneratedLibrary {
        files,
        per_directory: files,
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
    let scope = engine.scope("capacity-drive");
    crate::refresh(provider.as_ref(), &scope, &engine.db, false, &engine.cancel)
        .await
        .unwrap();
    let fs = CloudFs::new(engine.clone()).unwrap();
    let inner = fs.inner.clone();
    let session = fs.mount(&mount).unwrap();
    let path = mount.join("directory-000000");
    let directory = path.clone();
    let inode = tokio::task::spawn_blocking(move || std::fs::metadata(directory).unwrap().ino())
        .await
        .unwrap();
    let parent = inner.view(inode).unwrap();
    let keys = (0..128)
        .map(|index| {
            let view = Inner::project(&parent, provider.node_at(index + 2)).unwrap();
            Inner::inode_key(&view, false).unwrap()
        })
        .collect::<Vec<_>>();
    drop(parent);
    let db = engine.db.clone();
    tokio::task::spawn_blocking(move || Store::open(db).unwrap().inodes(&keys).unwrap())
        .await
        .unwrap();

    // The first batch can read existing inode keys. Every later batch needs the
    // writer held here. This establishes causality without a slow-machine guess.
    let db = engine.db.clone();
    let changed_id = provider.node_at(files + 1).id;
    let (release, released) = std::sync::mpsc::channel();
    let (started, ready) = tokio::sync::oneshot::channel();
    let writer = tokio::task::spawn_blocking(move || {
        let db = rusqlite::Connection::open(db).unwrap();
        db.execute_batch("BEGIN IMMEDIATE").unwrap();
        started.send(()).unwrap();
        if released.recv_timeout(Duration::from_secs(5)).is_ok() {
            // Commit a remote-style rename after the old reader has established
            // its SQLite snapshot but before later inode batches can proceed.
            assert_eq!(db.execute(
                "UPDATE nodes SET body=json_set(body,'$.name','renamed-after-open.txt') WHERE id=?1",
                [changed_id],
            ).unwrap(), 1);
            db.execute_batch("COMMIT").unwrap();
        } else {
            db.execute_batch("ROLLBACK").unwrap();
        }
    });
    ready.await.unwrap();
    let opened = Instant::now();
    let directory = path.clone();
    let mut reader = tokio::task::spawn_blocking(move || {
        let mut entries = std::fs::read_dir(directory).unwrap();
        let first = entries.next().unwrap().unwrap().file_name();
        (first, entries)
    });
    let (first, entries) = tokio::time::timeout(Duration::from_millis(500), &mut reader)
        .await
        .expect("first directory entry waited for the full snapshot")
        .unwrap();
    let first_ms = opened.elapsed().as_secs_f64() * 1000.0;
    assert!(first_ms < 500.0, "first-entry observation exceeded 500 ms");
    assert_eq!(first, "Projektunterlagen – Übersicht 00000000.txt");
    assert!(!writer.is_finished());
    assert_eq!(inner.directory_budget.usage().1, 1);
    assert_eq!(
        inner
            .directories
            .lock()
            .unwrap()
            .values()
            .next()
            .unwrap()
            .snapshot
            .len(),
        130
    );

    if close_early {
        // RELEASEDIR can arrive while the producer is blocked allocating the
        // next batch. Its reservation must then retire once that batch returns.
        tokio::task::spawn_blocking(move || drop(entries))
            .await
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while !inner.directories.lock().unwrap().is_empty() {
            assert!(
                Instant::now() < deadline,
                "directory close did not reach FUSE"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        release.send(()).unwrap();
        writer.await.unwrap();
    } else {
        release.send(()).unwrap();
        writer.await.unwrap();
        tokio::task::spawn_blocking(move || {
            let names = std::iter::once(first)
                .chain(entries.map(|entry| entry.unwrap().file_name()))
                .collect::<Vec<_>>();
            assert_eq!(names.len(), files);
            for (index, name) in names.iter().enumerate() {
                assert_eq!(
                    name,
                    &std::ffi::OsString::from(format!(
                        "Projektunterlagen – Übersicht {index:08}.txt"
                    ))
                );
            }
            let current = std::fs::read_dir(path)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<Vec<_>>();
            assert_eq!(current.len(), files);
            assert!(current.contains(&std::ffi::OsString::from("renamed-after-open.txt")));
            assert!(!current.contains(names.last().unwrap()));
        })
        .await
        .unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while inner.directory_budget.usage() != (0, 0) {
        assert!(
            Instant::now() < deadline,
            "snapshot reservation survived producer and reader"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    parents::settle(&inner, 1).await;
    assert_eq!(provider.foreground_requests.load(Ordering::SeqCst), 0);
    assert_eq!(provider.content_reads.load(Ordering::SeqCst), 0);
    println!(
        "CIRROVE_EARLY_DIRECTORY {}",
        serde_json::json!({
            "files":files,"first_entry_ms":first_ms,"closed_during_build":close_early,
            "retained_views":inner.views.lock().unwrap().len(),"snapshot_bytes_after_close":0,
            "scope":"synthetic actual kernel; first cached inode batch precedes blocked later inode writes"
        })
    );
    engine.stop().await;
    tokio::task::spawn_blocking(move || session.umount_and_join())
        .await
        .unwrap()
        .unwrap();
}
