//! Deferred replacement never uploads placeholder bytes or deletes its source early.
#![allow(clippy::unwrap_used)]
use cirrove_core::{Node, NodeKind, Scope, mutation::MutationReceipt};
use cirrove_service::journal::{JournalError, UploadJournal, UploadState, WorkingSource};
use std::{
    io::Read,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn scope() -> Scope {
    Scope {
        account: "preparation".into(),
        provider: "fixture".into(),
        collection: "drive".into(),
    }
}
fn node(id: &str, name: &str, tag: &str) -> Node {
    Node {
        id: id.into(),
        name: name.into(),
        parent_id: Some("root".into()),
        kind: NodeKind::File,
        size: 3,
        etag: Some(tag.into()),
        content_version: Some(tag.into()),
        modified_unix: 1,
        target: None,
    }
}
fn open(root: &Path, quota: u64) -> UploadJournal {
    let until = Instant::now() + Duration::from_secs(2);
    loop {
        match UploadJournal::open(root, &scope().account, quota) {
            Err(JournalError::Busy) if Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(2))
            }
            r => return r.unwrap(),
        }
    }
}
fn pair(j: &mut UploadJournal, preserve: bool) -> cirrove_service::journal::ReplacementRecord {
    let src = j
        .observe_namespace_file(scope(), node("source", "temporary", "source-original"))
        .unwrap();
    let dst = j
        .observe_namespace_file(scope(), node("target", "document", "target-original"))
        .unwrap();
    j.replace_namespace_file(src.id, src.revision, dst.id, dst.revision, preserve)
        .unwrap()
}
fn bytes(j: &mut UploadJournal, data: &[u8]) -> WorkingSource {
    let mut source = j.reserve_working(data.len() as u64).unwrap();
    source.write_chunk(data).unwrap();
    source
}
fn payload(j: &UploadJournal, id: uuid::Uuid) -> Vec<u8> {
    let mut out = vec![];
    j.payload(id).unwrap().read_to_end(&mut out).unwrap();
    out
}

#[test]
fn deferred_swap_has_no_uploadable_bytes_until_source_capture_and_keeps_reader_gate() {
    let temp = tempfile::tempdir().unwrap();
    let mut j = open(&temp.path().join("journal"), 4096);
    let r = pair(&mut j, true);
    assert_eq!(j.get(r.id).unwrap().state, UploadState::Preparing);
    assert_eq!(j.retained_bytes().unwrap(), 0);
    assert!(matches!(j.payload(r.id), Err(JournalError::Stale)));
    assert!(j.claim_next().unwrap().is_none());
    assert!(j.claim_mutation().unwrap().is_none());
    assert_eq!(j.namespace_object(r.source).unwrap().node.name, "document");
    assert!(j.namespace_object(r.victim).unwrap().unlinked);
    let (preparing, source) = j.claim_preparation().unwrap().unwrap();
    assert_eq!(source.remote.unwrap().id, "source");
    let data = bytes(&mut j, b"new");
    j.complete_preparation(r.id, preparing.attempt.unwrap(), data)
        .unwrap();
    assert_eq!(payload(&j, r.id), b"new");
    assert!(j.claim_next().unwrap().is_none());
    j.release_replacement_readers(r.id).unwrap();
    let upload = j.claim_next().unwrap().unwrap();
    assert_eq!(upload.id, r.id);
    j.acknowledge(
        upload.id,
        upload.attempt.unwrap(),
        node("target", "document", "published"),
    )
    .unwrap();
    let cleanup = j.claim_mutation().unwrap().unwrap();
    assert_eq!(cleanup.request.intent.before().unwrap().id, "source");
    assert_eq!(
        cleanup.request.intent.before().unwrap().etag.as_deref(),
        Some("source-original")
    );
    assert_eq!(
        j.namespace_by_remote(&scope(), "target")
            .unwrap()
            .unwrap()
            .id,
        r.source
    );
}

#[test]
fn preparation_captures_original_source_even_after_a_new_local_save_follows_it() {
    let temp = tempfile::tempdir().unwrap();
    let mut j = open(&temp.path().join("journal"), 4096);
    let r = pair(&mut j, false);
    let (preparing, p) = j.claim_preparation().unwrap().unwrap();
    let original = bytes(&mut j, b"old");
    let working = j
        .publish_working_for(r.source, scope(), p.remote.unwrap(), original)
        .unwrap();
    j.write_working(working.id, 0, b"new").unwrap();
    let newer = j.seal_working(working.id).unwrap().unwrap();
    assert_eq!(newer.base.unwrap().predecessor, r.id);
    let original = bytes(&mut j, b"old");
    j.complete_preparation(r.id, preparing.attempt.unwrap(), original)
        .unwrap();
    assert_eq!(payload(&j, r.id), b"old");
    assert_eq!(payload(&j, newer.id), b"new");
    assert_eq!(j.read_working(working.id, 0, 10).unwrap(), b"new");
    let first = j.claim_next().unwrap().unwrap();
    assert_eq!(first.id, r.id);
    j.acknowledge(
        first.id,
        first.attempt.unwrap(),
        node("target", "document", "first"),
    )
    .unwrap();
    let second = j.claim_next().unwrap().unwrap();
    assert_eq!(second.id, newer.id);
    assert!(
        matches!(second.intent,cirrove_service::journal::UploadIntent::Replace{item,expected_etag} if item=="target"&&expected_etag=="first")
    );
}

#[test]
fn a_source_move_receipt_is_distinct_from_the_target_content_base() {
    let temp = tempfile::tempdir().unwrap();
    let mut j = open(&temp.path().join("journal"), 4096);
    let src = j
        .observe_namespace_file(scope(), node("source", "temporary", "before-move"))
        .unwrap();
    let dst = j
        .observe_namespace_file(scope(), node("target", "document", "target-original"))
        .unwrap();
    let moved = j
        .relocate_namespace_file(src.id, src.revision, "root".into(), "renamed-temp".into())
        .unwrap();
    let src = j.namespace_object(src.id).unwrap();
    let r = j
        .replace_namespace_file(src.id, src.revision, dst.id, dst.revision, false)
        .unwrap();
    assert!(j.claim_preparation().unwrap().is_none());
    let action = j.claim_mutation().unwrap().unwrap();
    assert_eq!(action.id, moved.id);
    j.acknowledge_mutation(
        action.id,
        action.attempt.unwrap(),
        MutationReceipt::Upsert(node("source", "renamed-temp", "after-move")),
    )
    .unwrap();
    let (preparing, p) = j.claim_preparation().unwrap().unwrap();
    assert_eq!(p.remote.as_ref().unwrap().id, "source");
    assert_eq!(
        p.remote.as_ref().unwrap().etag.as_deref(),
        Some("after-move")
    );
    let data = bytes(&mut j, b"new");
    j.complete_preparation(r.id, preparing.attempt.unwrap(), data)
        .unwrap();
    let upload = j.claim_next().unwrap().unwrap();
    assert!(
        matches!(upload.intent,cirrove_service::journal::UploadIntent::Replace{item,expected_etag} if item=="target"&&expected_etag=="target-original")
    );
}

#[test]
fn interrupted_final_sql_adopts_the_complete_snapshot_without_redownload_or_extra_quota() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let mut j = open(&root, 6);
    let r = pair(&mut j, false);
    let (preparing, _) = j.claim_preparation().unwrap().unwrap();
    let attempt = preparing.attempt.unwrap();
    let db = rusqlite::Connection::open(root.join("uploads.db")).unwrap();
    db.execute_batch("CREATE TRIGGER deny_ready BEFORE UPDATE ON uploads WHEN OLD.state='preparing' AND NEW.state='pending' BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    let data = bytes(&mut j, b"old");
    assert!(j.complete_preparation(r.id, attempt, data).is_err());
    assert_eq!(j.get(r.id).unwrap().state, UploadState::Preparing);
    assert_eq!(j.retained_bytes().unwrap(), 6);
    assert!(j.claim_next().unwrap().is_none());
    db.execute_batch("DROP TRIGGER deny_ready").unwrap();
    drop(db);
    drop(j);
    let mut j = open(&root, 6);
    let (preparing, _) = j.claim_preparation().unwrap().unwrap();
    assert_ne!(preparing.attempt, Some(attempt));
    assert!(matches!(
        j.resume_prepared(r.id, attempt),
        Err(JournalError::Stale)
    ));
    assert!(j.resume_prepared(r.id, preparing.attempt.unwrap()).unwrap());
    assert_eq!(j.retained_bytes().unwrap(), 6);
    assert_eq!(payload(&j, r.id), b"old");
    assert_eq!(j.claim_next().unwrap().unwrap().id, r.id);
}

#[test]
fn quota_or_changed_source_keeps_both_cloud_actions_pending_and_other_files_progress() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let mut j = open(&root, 3);
    let r = pair(&mut j, false);
    let (preparing, _) = j.claim_preparation().unwrap().unwrap();
    let data = bytes(&mut j, b"old");
    assert!(matches!(
        j.complete_preparation(r.id, preparing.attempt.unwrap(), data),
        Err(JournalError::Quota)
    ));
    assert_eq!(j.retained_bytes().unwrap(), 0);
    j.defer_preparation(r.id, preparing.attempt.unwrap(), false, Duration::ZERO)
        .unwrap();
    let (retry, p) = j.claim_preparation().unwrap().unwrap();
    assert_eq!(p.remote.unwrap().etag.as_deref(), Some("source-original"));
    j.defer_preparation(r.id, retry.attempt.unwrap(), true, Duration::ZERO)
        .unwrap();
    assert_eq!(j.get(r.id).unwrap().state, UploadState::Conflict);
    assert!(j.claim_next().unwrap().is_none());
    assert!(j.claim_mutation().unwrap().is_none());
    let independent = j
        .enqueue(
            scope(),
            cirrove_service::journal::UploadIntent::Create {
                parent: "root".into(),
                name: "independent".into(),
            },
            b"ok".as_slice(),
        )
        .unwrap();
    assert_eq!(j.claim_next().unwrap().unwrap().id, independent.id);
    drop(j);
    let mut j = open(&root, 3);
    assert_eq!(j.get(r.id).unwrap().state, UploadState::Conflict);
    assert!(j.claim_preparation().unwrap().is_none());
    assert!(j.claim_mutation().unwrap().is_none());
}

#[test]
fn old_schema_migration_retains_saves_and_missing_preparation_table_fails_closed() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let mut j = open(&root, 4096);
    let upload = j
        .enqueue(
            scope(),
            cirrove_service::journal::UploadIntent::Create {
                parent: "root".into(),
                name: "retained".into(),
            },
            b"old".as_slice(),
        )
        .unwrap();
    drop(j);
    let db = rusqlite::Connection::open(root.join("uploads.db")).unwrap();
    db.execute_batch("DROP TABLE upload_preparations; PRAGMA user_version=12")
        .unwrap();
    drop(db);
    let mut j = open(&root, 4096);
    assert_eq!(payload(&j, upload.id), b"old");
    let r = pair(&mut j, true);
    drop(j);
    let db = rusqlite::Connection::open(root.join("uploads.db")).unwrap();
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, i32>(0))
            .unwrap(),
        14
    );
    db.execute_batch("DROP TABLE upload_preparations").unwrap();
    assert!(UploadJournal::open(&root, &scope().account, 4096).is_err());
    assert_eq!(
        db.query_row(
            "SELECT state FROM uploads WHERE id=?1",
            [r.id.to_string()],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "preparing"
    );
    db.execute_batch("PRAGMA user_version=15").unwrap();
    assert!(matches!(
        UploadJournal::open(&root, &scope().account, 4096),
        Err(JournalError::Schema)
    ));
}

#[test]
fn restart_rearms_source_gate_and_reclaims_only_identified_download_temporaries() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let mut j = open(&root, 4096);
    let r = pair(&mut j, false);
    let (preparing, _) = j.claim_preparation().unwrap().unwrap();
    let reserved = j
        .reserve_preparation(r.id, preparing.attempt.unwrap())
        .unwrap();
    drop(j);
    assert!(matches!(
        UploadJournal::open(&root, &scope().account, 4096),
        Err(JournalError::Busy)
    ));
    drop(reserved);
    let partial = root
        .join("working")
        .join(format!(".cirrove-preparing-{}-interrupted", r.id));
    std::fs::write(&partial, b"partial source").unwrap();
    std::fs::set_permissions(&partial, std::fs::Permissions::from_mode(0o600)).unwrap();
    let unknown = root.join("working").join("unknown-retained");
    std::fs::write(&unknown, b"unsent").unwrap();
    std::fs::set_permissions(&unknown, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut j = open(&root, 4096);
    assert!(!partial.exists());
    assert!(unknown.exists());
    assert!(!j.replacement(r.id).unwrap().local_ready);
    let (preparing, _) = j.claim_preparation().unwrap().unwrap();
    let data = bytes(&mut j, b"old");
    j.complete_preparation(r.id, preparing.attempt.unwrap(), data)
        .unwrap();
    assert!(j.claim_next().unwrap().is_none());
    j.release_replacement_readers(r.id).unwrap();
    assert_eq!(j.claim_next().unwrap().unwrap().id, r.id);
}

#[test]
fn sealed_source_checksum_survives_final_sql_failure_and_detects_same_length_damage() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let mut j = open(&root, 4096);
    let r = pair(&mut j, false);
    let (preparing, _) = j.claim_preparation().unwrap().unwrap();
    let db = rusqlite::Connection::open(root.join("uploads.db")).unwrap();
    db.execute_batch("CREATE TRIGGER deny_ready BEFORE UPDATE ON uploads WHEN OLD.state='preparing' AND NEW.state='pending' BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    let data = bytes(&mut j, b"old");
    assert!(
        j.complete_preparation(r.id, preparing.attempt.unwrap(), data)
            .is_err()
    );
    assert!(j.upload_preparation(r.id).unwrap().sha256.is_some());
    let payload = root.join("objects").join(r.id.to_string());
    std::fs::set_permissions(&payload, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(&payload, b"bad").unwrap();
    std::fs::set_permissions(&payload, std::fs::Permissions::from_mode(0o400)).unwrap();
    db.execute_batch("DROP TRIGGER deny_ready").unwrap();
    drop(db);
    drop(j);
    let mut j = open(&root, 4096);
    let (preparing, _) = j.claim_preparation().unwrap().unwrap();
    assert!(matches!(
        j.resume_prepared(r.id, preparing.attempt.unwrap()),
        Err(JournalError::Corrupt)
    ));
    assert!(j.claim_next().unwrap().is_none());
    assert!(j.claim_mutation().unwrap().is_none());
    assert!(payload.exists());
}

#[test]
fn delayed_hydration_cannot_attach_to_the_new_owner_of_a_transferred_provider_id() {
    let temp = tempfile::tempdir().unwrap();
    let mut j = open(&temp.path().join("journal"), 4096);
    let r = pair(&mut j, false);
    let delayed = bytes(&mut j, b"old");
    let (preparing, _) = j.claim_preparation().unwrap().unwrap();
    let data = bytes(&mut j, b"new");
    j.complete_preparation(r.id, preparing.attempt.unwrap(), data)
        .unwrap();
    let upload = j.claim_next().unwrap().unwrap();
    j.acknowledge(
        upload.id,
        upload.attempt.unwrap(),
        node("target", "document", "published"),
    )
    .unwrap();
    assert!(matches!(
        j.publish_working_for(
            r.victim,
            scope(),
            node("target", "document", "target-original"),
            delayed
        ),
        Err(JournalError::Stale)
    ));
    assert!(j.namespace_object(r.victim).unwrap().working_file.is_none());
    let current = j.namespace_object(r.source).unwrap().working_file.unwrap();
    assert_eq!(j.read_working(current, 0, 10).unwrap(), b"new");
}

/// Child for the crash test: prepares a replacement, stops at the requested
/// durable transition, then waits to be killed.
#[test]
#[ignore = "subprocess fixture; activated only by its parent test"]
fn preparation_crash_child() {
    let root = std::env::var("CIRROVE_PREPARATION_FIXTURE_ROOT").unwrap();
    let root = Path::new(&root);
    let mut j = open(&root.join("journal"), 4096);
    let r = pair(&mut j, true);
    let (preparing, _source) = j.claim_preparation().unwrap().unwrap();
    // Reserving leaves a .cirrove-preparing- temporary on disk. Dying here is the
    // state the next process has to recover from, which is a durable transition
    // of its own and one no fixture reaches by finishing cleanly.
    let _reserved = j
        .reserve_preparation(r.id, preparing.attempt.unwrap())
        .unwrap();
    if std::env::var("CIRROVE_PREPARATION_FIXTURE_PHASE").unwrap() == "completed" {
        let data = bytes(&mut j, b"new");
        j.complete_preparation(r.id, preparing.attempt.unwrap(), data)
            .unwrap();
    }
    // Persist which durable writes were actually reached, so the parent asserts
    // on what the program did rather than on a reading of the call graph.
    std::fs::write(
        root.join("reached"),
        cirrove_service::journal::durable::reached().join("\n"),
    )
    .unwrap();
    std::fs::write(root.join("ready"), r.id.to_string()).unwrap();
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// A killed process must leave preparation in the state its last durable write
/// reached, not in the state it was heading for.
///
/// The other preparation tests interrupt by injecting an error, which unwinds a
/// transaction the program still controls. This leaves whatever the kernel had
/// actually written, which is the case the milestone's "crash at every durable
/// transition" is about, and preparation was one of seven journal modules
/// carrying durable transitions with no kill point at all.
#[test]
fn actual_process_death_keeps_a_claimed_preparation_unfinished_and_a_completed_one_whole() {
    for phase in ["claimed", "completed"] {
        let temp = tempfile::tempdir().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "preparation_crash_child", "--ignored"])
            .env("CIRROVE_PREPARATION_FIXTURE_ROOT", temp.path())
            .env("CIRROVE_PREPARATION_FIXTURE_PHASE", phase)
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !temp.path().join("ready").exists() {
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("preparation fixture did not become ready");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        child.kill().unwrap();
        child.wait().unwrap();

        let id: uuid::Uuid = std::fs::read_to_string(temp.path().join("ready"))
            .unwrap()
            .parse()
            .unwrap();
        let reached = std::fs::read_to_string(temp.path().join("reached")).unwrap();
        let reached: Vec<&str> = reached.lines().collect();
        assert!(
            reached.contains(&"preparation::enqueue_preparing_replacement")
                && reached.contains(&"preparation::claim_preparation"),
            "the fixture died without crossing the transitions it exists to cover: {reached:?}"
        );
        assert_eq!(
            reached.contains(&"preparation::complete_preparation::objects_dir"),
            phase == "completed",
            "completion must be reached in exactly one of the two phases"
        );
        // Opening here IS the recovery pass: this process is the one that has to
        // clean up after the killed one, so its own reached set is the evidence.
        let mut j = open(&temp.path().join("journal"), 4096);
        assert!(
            cirrove_service::journal::durable::reached()
                .contains(&"preparation::recover_preparation_files"),
            "opening after a kill mid-preparation did not run recovery"
        );
        if phase == "completed" {
            // The captured source survives the kill verbatim.
            assert_eq!(payload(&j, id), b"new");
        } else {
            // Killed before capture: no bytes were adopted, and nothing may be
            // uploadable. A placeholder here would upload an empty replacement
            // over a real document.
            assert!(matches!(j.payload(id), Err(JournalError::Stale)));
            assert_eq!(j.get(id).unwrap().state, UploadState::Preparing);
        }
        // Either way the reservation is recoverable rather than orphaned: a
        // fresh claim must be possible after restart.
        assert!(j.claim_preparation().unwrap().is_some() || phase == "completed");
    }
}
