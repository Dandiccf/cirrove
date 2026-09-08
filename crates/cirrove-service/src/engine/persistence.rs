//! A cached read must not perform durable filesystem work. Without a connection
//! held open for the account's lifetime, every `Store::open` is both the first
//! and the last connection to a WAL database, so SQLite creates the write-ahead
//! log and its shared index on open and checkpoints and unlinks them on close.
#![allow(clippy::unwrap_used)]
use super::discovery::{LinkedLibrary, fixture_account};
use super::*;

fn sidecars(db: &std::path::Path) -> (bool, bool) {
    let name = db.file_name().unwrap().to_string_lossy().into_owned();
    let directory = db.parent().unwrap();
    (
        directory.join(format!("{name}-wal")).exists(),
        directory.join(format!("{name}-shm")).exists(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cached_reads_never_create_or_unlink_the_write_ahead_log() {
    let temp = tempfile::tempdir().unwrap();
    let provider = Arc::new(LinkedLibrary {
        linked: AtomicBool::new(false),
        primary_changes: AtomicU64::new(0),
    });
    let engine = Engine::new(
        fixture_account(temp.path().join("mount")),
        provider.clone(),
        temp.path().join("state"),
    )
    .await
    .unwrap();
    let scope = engine.scope("primary");
    crate::refresh(provider.as_ref(), &scope, &engine.db, false, &engine.cancel)
        .await
        .unwrap();
    assert_eq!(
        sidecars(&engine.db),
        (true, true),
        "the account must hold the write-ahead log open"
    );

    // Every one of these opens and drops its own connection. None of them may be
    // the last one, because the last one pays two file creations, a checkpoint
    // with two fsyncs and two unlinks on the filesystem the content cache is
    // also writing to.
    for round in 0..8u64 {
        engine.children(&scope, "root").await.unwrap();
        engine.node(&scope, "root").await.unwrap();
        let db = engine.db.clone();
        let key = format!("probe-{round}");
        tokio::task::spawn_blocking(move || Store::open(db).unwrap().inode(&key).unwrap())
            .await
            .unwrap();
        assert_eq!(
            sidecars(&engine.db),
            (true, true),
            "a cached read closed the last connection in round {round}"
        );
    }

    // Shutdown still leaves no sidecar behind, so a crash-free stop is clean.
    engine.stop().await;
    drop(engine);
    assert_eq!(
        sidecars(
            &temp
                .path()
                .join("state/accounts/00000000-0000-4000-8000-000000000015/metadata.db")
        ),
        (false, false),
        "closing the account must retire its write-ahead log"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_held_write_ahead_log_stays_bounded_under_sustained_writes() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("metadata.db");
    let keeper = Store::open(&db).unwrap();
    let wal = temp.path().join("metadata.db-wal");
    let mut peak = 0u64;
    // Each of these commits its own transaction, exactly as the inode allocator
    // does during a large listing. Nothing truncates the log on close any more,
    // so automatic checkpointing and the size limit must do it instead.
    for round in 0..4000u64 {
        let path = db.clone();
        tokio::task::spawn_blocking(move || {
            Store::open(path)
                .unwrap()
                .inode(&format!("sustained-{round}"))
                .unwrap()
        })
        .await
        .unwrap();
        if let Ok(meta) = std::fs::metadata(&wal) {
            peak = peak.max(meta.len());
        }
    }
    assert!(
        peak > 0,
        "the log must exist while a connection is held; peak {peak}"
    );
    // Automatic checkpointing settles this at roughly a thousand pages; the
    // observed peak here is 4.13 MB, so twice that is a meaningful ceiling
    // rather than a formality.
    assert!(
        peak <= 8 * 1024 * 1024,
        "the held log grew to {peak} bytes; automatic checkpointing is not bounding it"
    );
    drop(keeper);
    assert!(
        !wal.exists(),
        "closing the last connection must retire the log"
    );
}
