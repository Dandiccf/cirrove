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
