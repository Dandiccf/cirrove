//! Mutable bytes, immutable saves and recovery without cloud credentials.
#![allow(clippy::unwrap_used)]
use cirrove_core::{Node, NodeKind, Scope};
use cirrove_service::journal::{
    JournalError, UploadIntent, UploadJournal, UploadState, WorkingFile,
};
use std::{
    io::Read,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn scope() -> Scope {
    Scope {
        account: "working-fixture".into(),
        provider: "fixture".into(),
        collection: "drive".into(),
    }
}
fn node(size: u64) -> Node {
    Node {
        id: "remote-file".into(),
        parent_id: Some("root".into()),
        name: "Kärnten & Grüße.txt".into(),
        kind: NodeKind::File,
        size,
        modified_unix: 1,
        etag: Some("original".into()),
        content_version: Some("content-original".into()),
        target: None,
    }
}
fn open(root: &Path, quota: u64) -> UploadJournal {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match UploadJournal::open(root, &scope().account, quota) {
            Err(JournalError::Busy) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(2))
            }
            result => return result.unwrap(),
        }
    }
}
fn payload(j: &UploadJournal, id: uuid::Uuid) -> Vec<u8> {
    let mut bytes = vec![];
    j.payload(id).unwrap().read_to_end(&mut bytes).unwrap();
    bytes
}

#[test]
fn repeated_application_saves_keep_immutable_uploads_and_follow_receipts() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let mut j = open(&root, 1024);
    let working = j
        .create_working(scope(), node(8), false, b"original".as_slice())
        .unwrap();
    assert!(!working.dirty);
    assert!(j.seal_working(working.id).unwrap().is_none());
    j.write_working(working.id, 0, b"edit-one").unwrap();
    let first = j.seal_working(working.id).unwrap().unwrap();
    let attempt = j.claim_next().unwrap().unwrap();
    j.write_working(working.id, 0, b"edit-two").unwrap();
    let second = j.seal_working(working.id).unwrap().unwrap();
    assert_eq!(payload(&j, first.id), b"edit-one");
    assert_eq!(payload(&j, second.id), b"edit-two");
    assert_eq!(second.working_file, Some(working.id));
    assert!(j.claim_next().unwrap().is_none());
    let mut receipt = node(8);
    receipt.etag = Some("confirmed-first".into());
    j.acknowledge(first.id, attempt.attempt.unwrap(), receipt)
        .unwrap();
    j.prune_uploaded_payload(first.id).unwrap();
    drop(j);
    let mut j = open(&root, 1024);
    let recovered = j
        .working_by_identity(&scope(), "remote-file")
        .unwrap()
        .unwrap();
    assert_eq!(recovered.id, working.id);
    assert_eq!(recovered.latest, Some(second.id));
    assert!(!recovered.dirty);
    assert_eq!(j.read_working(working.id, 0, 100).unwrap(), b"edit-two");
    let next = j.claim_next().unwrap().unwrap();
    assert_eq!(
        next.intent,
        UploadIntent::Replace {
            item: "remote-file".into(),
            expected_etag: "confirmed-first".into()
        }
    );
    assert_eq!(payload(&j, next.id), b"edit-two");
}

#[test]
fn truncate_sparse_writes_and_quota_failures_preserve_the_working_copy() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let mut j = open(&root, 20);
    let working = j
        .create_working(scope(), node(0), true, b"".as_slice())
        .unwrap();
    assert_eq!(j.write_working(working.id, u64::MAX, b"").unwrap().0, 0);
    j.write_working(working.id, 5, b"abc").unwrap();
    assert_eq!(
        j.read_working(working.id, 0, 100).unwrap(),
        b"\0\0\0\0\0abc"
    );
    j.truncate_working(working.id, 6).unwrap();
    let saved = j.seal_working(working.id).unwrap().unwrap();
    assert_eq!(payload(&j, saved.id), b"\0\0\0\0\0a");
    j.truncate_working(working.id, 12).unwrap();
    assert!(matches!(
        j.seal_working(working.id),
        Err(JournalError::Quota)
    ));
    assert!(j.working_file(working.id).unwrap().dirty);
    assert!(matches!(
        j.write_working(working.id, 19, b"overflow"),
        Err(JournalError::Quota)
    ));
    assert_eq!(
        j.read_working(working.id, 0, 100).unwrap(),
        b"\0\0\0\0\0a\0\0\0\0\0\0"
    );
    assert_eq!(j.list(0, 100).unwrap().len(), 1);
    assert_eq!(payload(&j, saved.id), b"\0\0\0\0\0a");
}

#[test]
fn queue_commit_and_working_generation_advance_are_one_transaction() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let mut j = open(&root, 1024);
    let working = j
        .create_working(scope(), node(0), true, b"".as_slice())
        .unwrap();
    j.write_working(working.id, 0, b"preserve").unwrap();
    let fault = rusqlite::Connection::open(root.join("uploads.db")).unwrap();
    fault.execute_batch("CREATE TRIGGER fail_seal BEFORE UPDATE ON working_files WHEN json_extract(NEW.body,'$.dirty')=0 BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(matches!(
        j.seal_working(working.id),
        Err(JournalError::Storage)
    ));
    assert!(j.list(0, 100).unwrap().is_empty());
    let record = j.working_file(working.id).unwrap();
    assert!(record.dirty);
    assert_eq!(record.latest, None);
    assert_eq!(j.retained_bytes().unwrap(), 16); // Mutable bytes and retained orphan.
    fault.execute_batch("DROP TRIGGER fail_seal;").unwrap();
    drop(fault);
    drop(j);
    let mut j = open(&root, 1024);
    let saved = j.seal_working(working.id).unwrap().unwrap();
    assert_eq!(payload(&j, saved.id), b"preserve");
    assert_eq!(j.working_file(working.id).unwrap().latest, Some(saved.id));
    assert_eq!(j.list(0, 100).unwrap().len(), 1);
}

#[test]
fn partial_write_metadata_failure_recovers_actual_bytes_without_claiming_a_save() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let mut j = open(&root, 1024);
    let working = j
        .create_working(scope(), node(4), false, b"base".as_slice())
        .unwrap();
    let fault = rusqlite::Connection::open(root.join("uploads.db")).unwrap();
    fault.execute_batch("CREATE TRIGGER fail_size BEFORE UPDATE ON working_files WHEN json_extract(NEW.body,'$.node.size')!=json_extract(OLD.body,'$.node.size') BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(matches!(
        j.write_working(working.id, 4, b"-extended"),
        Err(JournalError::Storage)
    ));
    assert!(j.working_file(working.id).unwrap().dirty);
    fault.execute_batch("DROP TRIGGER fail_size;").unwrap();
    drop(fault);
    drop(j);
    let mut j = open(&root, 1024);
    let recovered = j.working_file(working.id).unwrap();
    assert!(recovered.dirty);
    assert_eq!(recovered.node.size, 13);
    assert!(j.claim_next().unwrap().is_none());
    assert_eq!(
        j.read_working(working.id, 0, 100).unwrap(),
        b"base-extended"
    );
    let sealed = j.seal_working(working.id).unwrap().unwrap();
    assert_eq!(payload(&j, sealed.id), b"base-extended");
}

#[test]
fn killed_process_preserves_fsynced_save_and_newer_unsealed_changes() {
    let temp = tempfile::tempdir().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "working_crash_fixture", "--ignored"])
        .env("CIRROVE_WORKING_CRASH", temp.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let marker = temp.path().join("ready.json");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !marker.exists() {
        if Instant::now() >= deadline || child.try_wait().unwrap().is_some() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("working fixture did not become ready");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let working: WorkingFile = serde_json::from_slice(&std::fs::read(marker).unwrap()).unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    let mut j = open(&temp.path().join("journal"), 1024);
    assert_eq!(
        j.get(working.latest.unwrap()).unwrap().state,
        UploadState::Pending
    );
    assert_eq!(payload(&j, working.latest.unwrap()), b"saved!!!");
    assert!(j.working_file(working.id).unwrap().dirty);
    assert_eq!(j.read_working(working.id, 0, 100).unwrap(), b"unsaved!");
    let saved = j.seal_working(working.id).unwrap().unwrap();
    assert_eq!(payload(&j, saved.id), b"unsaved!");
}

#[test]
#[ignore = "child process controlled by the synthetic parent test"]
fn working_crash_fixture() {
    let Some(root) = std::env::var_os("CIRROVE_WORKING_CRASH") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let mut j = open(&root.join("journal"), 1024);
    let w = j
        .create_working(scope(), node(0), true, b"".as_slice())
        .unwrap();
    j.write_working(w.id, 0, b"saved!!!").unwrap();
    j.seal_working(w.id).unwrap();
    j.write_working(w.id, 0, b"unsaved!").unwrap();
    std::fs::write(
        root.join("marker.tmp"),
        serde_json::to_vec(&j.working_file(w.id).unwrap()).unwrap(),
    )
    .unwrap();
    std::fs::rename(root.join("marker.tmp"), root.join("ready.json")).unwrap();
    loop {
        std::thread::park();
    }
}

#[test]
fn incomplete_hydration_stays_invisible_and_reserved_bytes_bound_concurrent_work() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let mut j = open(&root, 10);
    let mut source = j.reserve_working(8).unwrap();
    assert_eq!(j.retained_bytes().unwrap(), 8);
    assert!(matches!(j.reserve_working(3), Err(JournalError::Quota)));
    source.write_chunk(b"short").unwrap();
    assert!(matches!(
        j.publish_working(scope(), node(8), false, source),
        Err(JournalError::Corrupt)
    ));
    assert!(j.working_files().unwrap().is_empty());
    assert_eq!(j.retained_bytes().unwrap(), 0);
    let mut source = j.reserve_working(8).unwrap();
    source.write_chunk(b"complete").unwrap();
    let w = j.publish_working(scope(), node(8), false, source).unwrap();
    assert_eq!(j.read_working(w.id, 0, 100).unwrap(), b"complete");
    assert_eq!(j.retained_bytes().unwrap(), 8);
}
