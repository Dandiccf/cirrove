//! Retirement has a durable cleanup intent; pending edits never become cache.
#![allow(clippy::unwrap_used)]
use cirrove_core::{Node, NodeKind, Scope};
use cirrove_service::journal::{JournalError, UploadIntent, UploadJournal};
use std::{
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
fn scope() -> Scope {
    Scope {
        account: "handoff".into(),
        provider: "fixture".into(),
        collection: "drive".into(),
    }
}
fn node(id: &str, name: &str, tag: &str, size: u64) -> Node {
    Node {
        id: id.into(),
        name: name.into(),
        parent_id: Some("root".into()),
        kind: NodeKind::File,
        size,
        modified_unix: 1,
        etag: Some(tag.into()),
        content_version: Some(tag.into()),
        target: None,
    }
}
fn open(path: &Path) -> UploadJournal {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match UploadJournal::open(path, &scope().account, 4096) {
            Err(JournalError::Busy) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(1))
            }
            result => return result.unwrap(),
        }
    }
}
#[test]
fn clean_bytes_detach_and_remote_metadata_owns_the_entry() {
    let temp = tempfile::tempdir().unwrap();
    let mut j = open(temp.path());
    let old = node("remote", "before.txt", "one", 3);
    let file = j
        .create_working(scope(), old.clone(), false, b"old".as_slice())
        .unwrap();
    let object = j.namespace_by_remote(&scope(), &old.id).unwrap().unwrap();
    let mut new = node("remote", "after.txt", "two", 5);
    new.parent_id = Some("other".into());
    let followed = j
        .handoff_namespace(object.id, object.revision, new.clone())
        .unwrap();
    assert!(followed.follows_remote);
    assert!(j.working_files().unwrap().is_empty());
    assert!(
        temp.path()
            .join("working")
            .join(file.id.to_string())
            .exists()
    );
    assert_eq!(j.retained_bytes().unwrap(), 3);
    assert!(
        j.namespace_overlay(&scope(), "root", vec![])
            .unwrap()
            .nodes
            .is_empty()
    );
    assert_eq!(
        j.namespace_overlay(&scope(), "other", vec![new.clone()])
            .unwrap()
            .nodes,
        vec![new]
    );
    assert_eq!(j.collect_retired_working(1).unwrap(), 1);
    assert_eq!(j.retained_bytes().unwrap(), 0);
}
#[test]
fn created_identity_survives_handoff_and_reactivation_uses_the_current_cloud_version() {
    let temp = tempfile::tempdir().unwrap();
    let mut j = open(temp.path());
    let file = j
        .create_working(
            scope(),
            node("", "local.txt", "ignored", 0),
            true,
            b"".as_slice(),
        )
        .unwrap();
    j.write_working(file.id, 0, b"mine").unwrap();
    j.seal_working(file.id).unwrap();
    let claim = j.claim_next().unwrap().unwrap();
    let remote = node("assigned", "local.txt", "uploaded", 4);
    j.acknowledge(claim.id, claim.attempt.unwrap(), remote.clone())
        .unwrap();
    j.prune_uploaded_payload(claim.id).unwrap();
    let object = j
        .namespace_by_local(&scope(), &file.node.id)
        .unwrap()
        .unwrap();
    j.handoff_namespace(object.id, object.revision, remote.clone())
        .unwrap();
    j.collect_retired_working(1).unwrap();
    let mut external = node("assigned", "external.txt", "external", 7);
    external.parent_id = Some("new-parent".into());
    let listed = j
        .namespace_overlay(&scope(), "new-parent", vec![external.clone()])
        .unwrap()
        .nodes;
    assert_eq!(listed[0].id, file.node.id);
    assert_eq!(listed[0].etag, external.etag);
    let next = j
        .create_working(scope(), external.clone(), false, b"foreign".as_slice())
        .unwrap();
    assert_eq!(next.node.id, file.node.id);
    assert_eq!(next.node.name, external.name);
    assert_eq!(next.node.parent_id, external.parent_id);
    assert!(next.latest.is_none());
    assert_eq!(
        next.intent,
        UploadIntent::Replace {
            item: external.id.clone(),
            expected_etag: "external".into()
        }
    );
    j.write_working(next.id, 0, b"updated").unwrap();
    let upload = j.seal_working(next.id).unwrap().unwrap();
    assert!(upload.base.is_none());
    assert_eq!(upload.intent, next.intent);
    let object = j.namespace_object(object.id).unwrap();
    assert!(!object.follows_remote);
    assert!(!j.namespace_is_clean(&object).unwrap());
    assert_eq!(j.read_working(next.id, 0, 20).unwrap(), b"updated");
}
#[test]
fn dirty_pending_inflight_and_successor_bytes_cannot_be_retired() {
    for phase in 0..5 {
        let temp = tempfile::tempdir().unwrap();
        let mut j = open(temp.path());
        let remote = node("remote", "file.txt", "original", 3);
        let file = j
            .create_working(scope(), remote.clone(), false, b"old".as_slice())
            .unwrap();
        j.write_working(file.id, 0, b"new").unwrap();
        if phase >= 1 {
            j.seal_working(file.id).unwrap();
        }
        if phase >= 2 {
            let claim = j.claim_next().unwrap().unwrap();
            if phase >= 3 {
                j.acknowledge(
                    claim.id,
                    claim.attempt.unwrap(),
                    node("remote", "file.txt", "uploaded", 3),
                )
                .unwrap();
                j.write_working(file.id, 0, b"next").unwrap();
                if phase == 4 {
                    j.seal_working(file.id).unwrap();
                }
            }
        }
        let object = j
            .namespace_by_remote(&scope(), &remote.id)
            .unwrap()
            .unwrap();
        assert!(!j.namespace_is_clean(&object).unwrap());
        assert!(matches!(
            j.handoff_namespace(object.id, object.revision, remote),
            Err(JournalError::Stale)
        ));
        assert_eq!(j.collect_retired_working(16).unwrap(), 0);
        assert_eq!(
            j.read_working(file.id, 0, 20).unwrap(),
            if phase >= 3 {
                b"next".as_slice()
            } else {
                b"new".as_slice()
            }
        );
    }
}
#[test]
fn a_failed_detach_transaction_keeps_working_rows_bytes_and_cleanup_intents_together() {
    let temp = tempfile::tempdir().unwrap();
    let mut j = open(temp.path());
    let remote = node("remote", "file.txt", "original", 3);
    let file = j
        .create_working(scope(), remote.clone(), false, b"old".as_slice())
        .unwrap();
    let object = j
        .namespace_by_remote(&scope(), &remote.id)
        .unwrap()
        .unwrap();
    let db = rusqlite::Connection::open(temp.path().join("uploads.db")).unwrap();
    db.execute_batch("CREATE TRIGGER deny_detach BEFORE UPDATE ON namespace_objects BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(
        j.handoff_namespace(object.id, object.revision, remote.clone())
            .is_err()
    );
    assert_eq!(j.collect_retired_working(10).unwrap(), 0);
    assert_eq!(j.read_working(file.id, 0, 20).unwrap(), b"old");
    assert!(!j.namespace_object(object.id).unwrap().follows_remote);
    db.execute_batch("DROP TRIGGER deny_detach;").unwrap();
    j.handoff_namespace(object.id, object.revision, remote)
        .unwrap();
    assert_eq!(j.collect_retired_working(10).unwrap(), 1);
}
#[test]
fn restart_finishes_only_explicit_cleanup_and_retains_unknown_spool_bytes() {
    for after_delete in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let mut j = open(temp.path());
        let remote = node("remote", "file.txt", "original", 3);
        let file = j
            .create_working(scope(), remote.clone(), false, b"old".as_slice())
            .unwrap();
        let object = j
            .namespace_by_remote(&scope(), &remote.id)
            .unwrap()
            .unwrap();
        let unknown = temp
            .path()
            .join("working")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::write(&unknown, b"retained unknown bytes").unwrap();
        std::fs::set_permissions(&unknown, std::fs::Permissions::from_mode(0o600)).unwrap();
        j.handoff_namespace(object.id, object.revision, remote)
            .unwrap();
        if after_delete {
            let db = rusqlite::Connection::open(temp.path().join("uploads.db")).unwrap();
            db.execute_batch("CREATE TRIGGER deny_cleanup BEFORE DELETE ON retired_working BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
            assert!(j.collect_retired_working(1).is_err());
            assert!(
                !temp
                    .path()
                    .join("working")
                    .join(file.id.to_string())
                    .exists()
            );
            db.execute_batch("DROP TRIGGER deny_cleanup;").unwrap();
        }
        drop(j);
        let j = open(temp.path());
        assert!(j.working_files().unwrap().is_empty());
        assert!(j.namespace_object(object.id).unwrap().follows_remote);
        assert_eq!(std::fs::read(unknown).unwrap(), b"retained unknown bytes");
        assert_eq!(j.retained_bytes().unwrap(), 22);
    }
}
#[test]
fn stale_revision_foreign_identity_and_a_corrupt_cleanup_target_are_refused() {
    let temp = tempfile::tempdir().unwrap();
    let mut j = open(temp.path());
    let remote = node("remote", "file.txt", "original", 3);
    let file = j
        .create_working(scope(), remote.clone(), false, b"old".as_slice())
        .unwrap();
    let object = j
        .namespace_by_remote(&scope(), &remote.id)
        .unwrap()
        .unwrap();
    assert!(matches!(
        j.handoff_namespace(object.id, object.revision + 1, remote.clone()),
        Err(JournalError::Stale)
    ));
    assert!(matches!(
        j.handoff_namespace(
            object.id,
            object.revision,
            node("other", "file.txt", "other", 3)
        ),
        Err(JournalError::Stale)
    ));
    j.handoff_namespace(object.id, object.revision, remote)
        .unwrap();
    let path = temp.path().join("working").join(file.id.to_string());
    std::fs::remove_file(&path).unwrap();
    let protected = temp.path().join("protected");
    std::fs::write(&protected, b"keep").unwrap();
    std::os::unix::fs::symlink(&protected, &path).unwrap();
    assert!(j.collect_retired_working(1).is_err());
    assert_eq!(std::fs::read(protected).unwrap(), b"keep");
    assert!(
        std::fs::symlink_metadata(path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}
#[test]
fn schema_seven_migration_preserves_pending_work_and_refuses_future_schema() {
    let temp = tempfile::tempdir().unwrap();
    let id;
    {
        let mut j = open(temp.path());
        let file = j
            .create_working(
                scope(),
                node("", "file.txt", "unused", 0),
                true,
                b"".as_slice(),
            )
            .unwrap();
        id = file.id;
        j.write_working(id, 0, b"pending").unwrap();
        j.seal_working(id).unwrap();
    }
    let db = rusqlite::Connection::open(temp.path().join("uploads.db")).unwrap();
    db.execute_batch("DROP TABLE retired_working; DROP INDEX namespace_operation_objects;
        DROP INDEX uploaded_payloads; ALTER TABLE uploads DROP COLUMN payload_present;
        DROP INDEX namespace_handoff_candidates;
        UPDATE namespace_objects SET body=json_remove(body,'$.follows_remote'); PRAGMA user_version=7;").unwrap();
    let j = open(temp.path());
    assert_eq!(j.read_working(id, 0, 20).unwrap(), b"pending");
    let object = j
        .namespace_by_local(&scope(), &j.working_file(id).unwrap().node.id)
        .unwrap()
        .unwrap();
    assert!(!object.follows_remote);
    assert!(!j.namespace_is_clean(&object).unwrap());
    drop(j);
    db.execute_batch("PRAGMA user_version=15;").unwrap();
    assert!(matches!(
        UploadJournal::open(temp.path(), &scope().account, 4096),
        Err(JournalError::Schema)
    ));
}

#[test]
fn acknowledged_snapshot_cleanup_retries_after_metadata_failure_and_keeps_pending_payloads() {
    let temp = tempfile::tempdir().unwrap();
    let mut j = open(temp.path());
    let done = j
        .create_working(
            scope(),
            node("", "done.txt", "unused", 0),
            true,
            b"".as_slice(),
        )
        .unwrap();
    j.write_working(done.id, 0, b"done").unwrap();
    j.seal_working(done.id).unwrap();
    let claim = j.claim_next().unwrap().unwrap();
    j.acknowledge(
        claim.id,
        claim.attempt.unwrap(),
        node("assigned", "done.txt", "ack", 4),
    )
    .unwrap();
    let pending = j
        .create_working(
            scope(),
            node("", "pending.txt", "unused", 0),
            true,
            b"".as_slice(),
        )
        .unwrap();
    j.write_working(pending.id, 0, b"pending").unwrap();
    let upload = j.seal_working(pending.id).unwrap().unwrap();
    let db = rusqlite::Connection::open(temp.path().join("uploads.db")).unwrap();
    db.execute_batch("CREATE TRIGGER deny_collection BEFORE UPDATE ON uploads WHEN NEW.payload_present=0 BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(j.collect_uploaded_payloads(1).is_err());
    assert!(
        !temp
            .path()
            .join("objects")
            .join(claim.id.to_string())
            .exists()
    );
    assert!(j.get(claim.id).unwrap().remote.is_some());
    db.execute_batch("DROP TRIGGER deny_collection;").unwrap();
    assert_eq!(j.collect_uploaded_payloads(1).unwrap(), 1);
    assert_eq!(j.collect_uploaded_payloads(10).unwrap(), 0);
    let mut bytes = vec![];
    std::io::Read::read_to_end(&mut j.payload(upload.id).unwrap(), &mut bytes).unwrap();
    assert_eq!(bytes, b"pending");
    assert_eq!(j.read_working(done.id, 0, 20).unwrap(), b"done");
    assert_eq!(j.read_working(pending.id, 0, 20).unwrap(), b"pending");
}

/// Child for the crash test: hands a namespace entry over to remote ownership,
/// stops at the requested durable transition, then waits to be killed.
#[test]
#[ignore = "subprocess fixture; activated only by its parent test"]
fn handoff_crash_child() {
    let root = std::env::var("CIRROVE_HANDOFF_FIXTURE_ROOT").unwrap();
    let root = Path::new(&root);
    // open() sets permissions on the directory, so it has to exist first.
    std::fs::create_dir_all(root.join("journal")).unwrap();
    let mut j = open(&root.join("journal"));
    let old = node("remote", "before.txt", "one", 3);
    let file = j
        .create_working(scope(), old.clone(), false, b"old".as_slice())
        .unwrap();
    let object = j.namespace_by_remote(&scope(), &old.id).unwrap().unwrap();
    let mut new = node("remote", "after.txt", "two", 5);
    new.parent_id = Some("other".into());
    j.handoff_namespace(object.id, object.revision, new)
        .unwrap();
    if std::env::var("CIRROVE_HANDOFF_FIXTURE_PHASE").unwrap() == "collected" {
        assert_eq!(j.collect_retired_working(1).unwrap(), 1);
    }
    // Report the spool path rather than letting the parent assume a layout.
    let spool = root
        .join("journal")
        .join("working")
        .join(file.id.to_string());
    std::fs::write(
        root.join("reached"),
        cirrove_service::journal::durable::reached().join("\n"),
    )
    .unwrap();
    std::fs::write(root.join("ready"), spool.display().to_string()).unwrap();
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// A killed process must not lose detached bytes before anything claims them.
///
/// The existing tests reach retirement by injecting a failed transaction and by
/// restarting cleanly, which are narrower: they unwind or resume state the program
/// still owns. This leaves whatever the kernel had actually written.
///
/// The bytes are the point. Detaching hands the namespace entry to remote
/// metadata while the local copy becomes recovery data, so between the handoff and
/// an explicit cleanup they are the only copy of an edit whose remote side has
/// already moved on.
///
/// This asserts the part that is settled: the handoff itself is durable, and the
/// detached bytes are still on disk when the process dies. What happens to them on
/// the NEXT open is recorded as an open question rather than pinned here -- see
/// docs/benchmarks/durable-transition-crash-coverage.json. Pinning behaviour whose
/// intent is undecided would turn this into a change detector.
#[test]
fn actual_process_death_keeps_detached_bytes_on_disk() {
    for phase in ["handed-off", "collected"] {
        let temp = tempfile::tempdir().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "handoff_crash_child", "--ignored"])
            .env("CIRROVE_HANDOFF_FIXTURE_ROOT", temp.path())
            .env("CIRROVE_HANDOFF_FIXTURE_PHASE", phase)
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !temp.path().join("ready").exists() {
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("handoff fixture did not become ready");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        child.kill().unwrap();
        child.wait().unwrap();

        let spool =
            std::path::PathBuf::from(std::fs::read_to_string(temp.path().join("ready")).unwrap());
        // Read the disk before reopening: a journal open runs recovery, and this
        // is about what the kill left, not what recovery then decides.
        assert_eq!(
            spool.exists(),
            phase == "handed-off",
            "a kill must leave detached bytes on disk until cleanup claims them, \
             and must not leave them once it has"
        );
        // The handoff itself committed before the kill: the namespace entry now
        // belongs to remote metadata and no working file claims it.
        let j = open(&temp.path().join("journal"));
        assert!(j.working_files().unwrap().is_empty());
    }
}
