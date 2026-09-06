//! Synthetic durability checks: no credentials, provider requests or cloud writes.
#![allow(clippy::unwrap_used)]
use cirrove_core::{Node, NodeKind, Scope};
use cirrove_service::journal::{
    JournalError, UploadIntent, UploadJournal, UploadRecord, UploadState,
};
use std::{
    io::Read,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const BYTES: &[u8] = b"keep this local edit";
fn scope() -> Scope {
    Scope {
        account: "synthetic-account".into(),
        provider: "fixture".into(),
        collection: "drive".into(),
    }
}
fn create(name: &str) -> UploadIntent {
    UploadIntent::Create {
        parent: "root".into(),
        name: name.into(),
    }
}
fn open(root: &Path) -> UploadJournal {
    open_released(root, &scope().account, 1024 * 1024).unwrap()
}
fn open_released(root: &Path, account: &str, quota: u64) -> Result<UploadJournal, JournalError> {
    // Parallel crash tests fork. An inherited flock description can briefly
    // outlive the parent's owner until the child execs, despite CLOEXEC.
    // This helper is only for first opens or after dropping the previous owner;
    // the held-owner exclusion assertion below still calls open directly.
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match UploadJournal::open(root, account, quota) {
            Err(JournalError::Busy) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(2));
            }
            result => return result,
        }
    }
}
fn remote(record: &UploadRecord) -> Node {
    let (id, name) = match &record.intent {
        UploadIntent::Create { name, .. } => ("created-id".to_owned(), name.clone()),
        UploadIntent::Replace { item, .. } => (item.clone(), "Existing.txt".into()),
    };
    Node {
        id,
        name,
        parent_id: Some("root".into()),
        kind: NodeKind::File,
        size: record.size,
        modified_unix: 0,
        etag: Some("new-metadata-tag".into()),
        content_version: Some("new-content-tag".into()),
        target: None,
    }
}
fn payload(journal: &UploadJournal, id: uuid::Uuid) -> Vec<u8> {
    let mut bytes = vec![];
    journal
        .payload(id)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    bytes
}

#[test]
fn version_one_queue_migrates_without_losing_local_payloads_or_ordering() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let mut journal = open(&root);
    let first = journal.enqueue(scope(), create("one.txt"), BYTES).unwrap();
    let second = journal.enqueue(scope(), create("two.txt"), BYTES).unwrap();
    let attempt = journal.claim_next().unwrap().unwrap();
    assert_eq!(attempt.id, first.id);
    drop(journal);
    let db = rusqlite::Connection::open(root.join("uploads.db")).unwrap();
    db.execute_batch("UPDATE uploads SET body=json_remove(body,'$.session_key','$.transferred_bytes','$.retry_at','$.failed_attempts'); PRAGMA user_version=1;").unwrap();
    drop(db);
    let mut journal = open(&root);
    let recovered = journal.claim_next_verification().unwrap().unwrap();
    assert_eq!(recovered.id, first.id);
    assert_eq!(recovered.transferred_bytes, 0);
    assert_eq!(recovered.session_key, None);
    assert_eq!(payload(&journal, first.id), BYTES);
    assert_eq!(journal.claim_next().unwrap().unwrap().id, second.id);
    assert_eq!(payload(&journal, second.id), BYTES);
}

#[test]
fn snapshots_are_private_immutable_and_survive_restart_with_unicode_names() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let mut journal = open(&root);
    let record = journal
        .enqueue(scope(), create("Kärnten & Grüße.txt"), BYTES)
        .unwrap();
    assert_eq!(record.state, UploadState::Pending);
    assert_eq!(payload(&journal, record.id), BYTES);
    for path in [
        root.join("uploads.db"),
        root.join("uploads.db-wal"),
        root.join("objects").join(record.id.to_string()),
    ] {
        assert_eq!(path.metadata().unwrap().permissions().mode() & 0o077, 0);
    }
    assert_eq!(
        root.join("objects")
            .join(record.id.to_string())
            .metadata()
            .unwrap()
            .permissions()
            .mode()
            & 0o222,
        0
    );
    drop(journal);
    let journal = open(&root);
    assert_eq!(journal.get(record.id).unwrap().intent, record.intent);
    assert_eq!(payload(&journal, record.id), BYTES);
    assert_eq!(journal.list(0, 100).unwrap().len(), 1);
}

#[test]
fn quota_and_reader_failures_do_not_acknowledge_partial_edits() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let mut journal = UploadJournal::open(&root, &scope().account, 4).unwrap();
    assert!(matches!(
        journal.enqueue(scope(), create("Too large"), b"12345".as_slice()),
        Err(JournalError::Quota)
    ));
    assert!(journal.list(0, 10).unwrap().is_empty());
    assert_eq!(journal.retained_bytes().unwrap(), 0);
    struct BrokenReader(bool);
    impl Read for BrokenReader {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            if self.0 {
                return Err(std::io::Error::other("synthetic interrupted source"));
            }
            self.0 = true;
            bytes[0] = 42;
            Ok(1)
        }
    }
    assert!(matches!(
        journal.enqueue(scope(), create("Interrupted"), BrokenReader(false)),
        Err(JournalError::Storage)
    ));
    assert_eq!(journal.retained_bytes().unwrap(), 0);
    let record = journal
        .enqueue(scope(), create("Exact quota"), b"1234".as_slice())
        .unwrap();
    assert_eq!(payload(&journal, record.id), b"1234");
    assert!(matches!(
        journal.enqueue(scope(), create("No capacity"), b"x".as_slice()),
        Err(JournalError::Quota)
    ));
    assert_eq!(journal.list(0, 10).unwrap().len(), 1);
}

#[test]
fn failed_database_commit_retains_published_bytes_without_enqueuing_them() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let mut journal = open(&root);
    let fault = rusqlite::Connection::open(root.join("uploads.db")).unwrap();
    fault.execute_batch("CREATE TRIGGER reject_enqueue BEFORE INSERT ON uploads BEGIN SELECT RAISE(ABORT,'synthetic storage failure'); END;").unwrap();
    assert!(matches!(
        journal.enqueue(scope(), create("Interrupted"), BYTES),
        Err(JournalError::Storage)
    ));
    assert!(journal.list(0, 10).unwrap().is_empty());
    assert_eq!(journal.retained_bytes().unwrap(), BYTES.len() as u64);
    drop(fault);
    drop(journal);
    let mut journal = open(&root);
    assert!(journal.claim_next().unwrap().is_none());
    assert_eq!(journal.retained_bytes().unwrap(), BYTES.len() as u64);
    let file = std::fs::read_dir(root.join("objects"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(std::fs::read(file).unwrap(), BYTES);
}

#[test]
fn crashes_require_verification_and_old_attempts_cannot_acknowledge_new_ones() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let mut journal = open(&root);
    let record = journal
        .enqueue(scope(), create("Saved.txt"), BYTES)
        .unwrap();
    let old = journal.claim_next().unwrap().unwrap();
    drop(journal);
    let mut journal = open(&root);
    assert_eq!(
        journal.get(record.id).unwrap().state,
        UploadState::VerifyRequired
    );
    assert!(journal.claim_next().unwrap().is_none());
    assert!(matches!(
        journal.acknowledge(record.id, old.attempt.unwrap(), remote(&record)),
        Err(JournalError::Stale)
    ));
    assert!(matches!(
        journal.prune_uploaded_payload(record.id),
        Err(JournalError::Stale)
    ));
    journal.retry_verified_uncommitted(record.id).unwrap();
    let next = journal.claim_next().unwrap().unwrap();
    assert_ne!(next.attempt, old.attempt);
    assert!(matches!(
        journal.stop_attempt(record.id, old.attempt.unwrap(), UploadState::Failed),
        Err(JournalError::Stale)
    ));
    journal
        .acknowledge(record.id, next.attempt.unwrap(), remote(&record))
        .unwrap();
    drop(journal);
    let mut journal = open(&root);
    assert_eq!(journal.get(record.id).unwrap().state, UploadState::Uploaded);
    assert!(journal.claim_next().unwrap().is_none());
    journal.prune_uploaded_payload(record.id).unwrap();
    assert_eq!(journal.retained_bytes().unwrap(), 0);
    assert!(journal.get(record.id).unwrap().remote.is_some());
}

#[test]
fn verification_can_record_a_lost_success_without_reuploading() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let mut journal = open(&root);
    let record = journal
        .enqueue(scope(), create("Saved.txt"), BYTES)
        .unwrap();
    journal.claim_next().unwrap().unwrap();
    drop(journal);
    let mut journal = open(&root);
    let attempt = journal.claim_verification(record.id).unwrap();
    assert_eq!(attempt.state, UploadState::Verifying);
    drop(journal);
    let mut journal = open(&root);
    assert_eq!(
        journal.get(record.id).unwrap().state,
        UploadState::VerifyRequired
    );
    assert!(matches!(
        journal.acknowledge(record.id, attempt.attempt.unwrap(), remote(&record)),
        Err(JournalError::Stale)
    ));
    let attempt = journal.claim_verification(record.id).unwrap();
    journal
        .acknowledge(record.id, attempt.attempt.unwrap(), remote(&record))
        .unwrap();
    assert_eq!(journal.get(record.id).unwrap().state, UploadState::Uploaded);
    assert_eq!(payload(&journal, record.id), BYTES);
}

#[test]
fn unresolved_edits_block_the_same_identity_but_not_other_files() {
    let temp = tempfile::tempdir().unwrap();
    let mut journal = open(&temp.path().join("journal"));
    let intent = UploadIntent::Replace {
        item: "existing-id".into(),
        expected_etag: "observed-tag".into(),
    };
    let first = journal.enqueue(scope(), intent.clone(), BYTES).unwrap();
    let second = journal.enqueue(scope(), intent, BYTES).unwrap();
    let independent = journal
        .enqueue(scope(), create("Independent.txt"), BYTES)
        .unwrap();
    let first_attempt = journal.claim_next().unwrap().unwrap();
    assert_eq!(first_attempt.id, first.id);
    let other_attempt = journal.claim_next().unwrap().unwrap();
    assert_eq!(other_attempt.id, independent.id);
    journal
        .stop_attempt(
            first.id,
            first_attempt.attempt.unwrap(),
            UploadState::Conflict,
        )
        .unwrap();
    assert!(journal.claim_next().unwrap().is_none());
    assert_eq!(journal.get(second.id).unwrap().state, UploadState::Pending);
    assert!(matches!(
        journal.retry_verified_uncommitted(first.id),
        Err(JournalError::Stale)
    ));
    for id in [first.id, second.id, independent.id] {
        assert!(matches!(
            journal.prune_uploaded_payload(id),
            Err(JournalError::Stale)
        ));
        assert_eq!(payload(&journal, id), BYTES);
    }
}

#[test]
fn corrupted_payload_cannot_upload_or_block_independent_files() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let mut journal = open(&root);
    let record = journal
        .enqueue(scope(), create("Saved.txt"), BYTES)
        .unwrap();
    let next = journal
        .enqueue(scope(), create("Independent.txt"), BYTES)
        .unwrap();
    let path = root.join("objects").join(record.id.to_string());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(&path, vec![0; BYTES.len()]).unwrap();
    assert!(matches!(journal.claim_next(), Err(JournalError::Corrupt)));
    assert_eq!(journal.get(record.id).unwrap().state, UploadState::Failed);
    assert_eq!(journal.claim_next().unwrap().unwrap().id, next.id);
    assert!(matches!(
        journal.retry_verified_uncommitted(record.id),
        Err(JournalError::Corrupt)
    ));
    assert!(matches!(
        journal.prune_uploaded_payload(record.id),
        Err(JournalError::Stale)
    ));
    assert!(path.exists());
}

#[test]
fn ownership_account_and_preconditions_are_enforced() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let mut journal = open(&root);
    assert!(matches!(
        UploadJournal::open(&root, &scope().account, 100),
        Err(JournalError::Busy)
    ));
    let mut foreign = scope();
    foreign.account = "different".into();
    assert!(matches!(
        journal.enqueue(foreign, create("Wrong"), BYTES),
        Err(JournalError::Account)
    ));
    for intent in [
        create("../escape"),
        create(".."),
        UploadIntent::Replace {
            item: "existing".into(),
            expected_etag: String::new(),
        },
    ] {
        assert!(matches!(
            journal.enqueue(scope(), intent, BYTES),
            Err(JournalError::Intent)
        ));
    }
    drop(journal);
    assert!(matches!(
        open_released(&root, "different", 100),
        Err(JournalError::Account)
    ));
    let alias = temp.path().join("alias");
    std::os::unix::fs::symlink(&root, &alias).unwrap();
    assert!(matches!(
        UploadJournal::open(&alias, &scope().account, 100),
        Err(JournalError::Storage)
    ));
}

#[test]
fn malformed_remote_receipts_never_release_the_local_copy() {
    let temp = tempfile::tempdir().unwrap();
    let mut journal = open(&temp.path().join("journal"));
    let record = journal
        .enqueue(scope(), create("Empty.txt"), b"".as_slice())
        .unwrap();
    let attempt = journal.claim_next().unwrap().unwrap().attempt.unwrap();
    let mut wrong_parent = remote(&record);
    wrong_parent.parent_id = Some("another-folder".into());
    let mut wrong_size = remote(&record);
    wrong_size.size = 1;
    let mut no_revision = remote(&record);
    no_revision.etag = None;
    no_revision.content_version = None;
    for receipt in [wrong_parent, wrong_size, no_revision] {
        assert!(matches!(
            journal.acknowledge(record.id, attempt, receipt),
            Err(JournalError::Corrupt)
        ));
        assert_eq!(
            journal.get(record.id).unwrap().state,
            UploadState::Uploading
        );
        assert!(matches!(
            journal.prune_uploaded_payload(record.id),
            Err(JournalError::Stale)
        ));
    }
    assert!(payload(&journal, record.id).is_empty());
    journal
        .acknowledge(record.id, attempt, remote(&record))
        .unwrap();
    journal.prune_uploaded_payload(record.id).unwrap();
    assert!(journal.list(u64::MAX, 10).unwrap().is_empty());
}

struct KillOnDrop(Child);
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
#[test]
fn actual_process_death_preserves_pending_uncertain_and_acknowledged_states() {
    for phase in ["pending", "uploading", "uploaded"] {
        let temp = tempfile::tempdir().unwrap();
        let mut child = KillOnDrop(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "journal_crash_fixture", "--ignored"])
                .env("CIRROVE_JOURNAL_CRASH_ROOT", temp.path())
                .env("CIRROVE_JOURNAL_CRASH_PHASE", phase)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        let marker = temp.path().join("ready.json");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !marker.exists() {
            assert!(Instant::now() < deadline, "fixture did not become ready");
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "fixture exited early"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let record: UploadRecord = serde_json::from_slice(&std::fs::read(marker).unwrap()).unwrap();
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        let journal = open(&temp.path().join("journal"));
        let state = match phase {
            "pending" => UploadState::Pending,
            "uploading" => UploadState::VerifyRequired,
            _ => UploadState::Uploaded,
        };
        assert_eq!(journal.get(record.id).unwrap().state, state);
        assert_eq!(payload(&journal, record.id), BYTES);
    }
}

#[test]
#[ignore = "subprocess fixture; activated only by its synthetic parent test"]
fn journal_crash_fixture() {
    let Some(root) = std::env::var_os("CIRROVE_JOURNAL_CRASH_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let phase = std::env::var("CIRROVE_JOURNAL_CRASH_PHASE").unwrap();
    let mut journal = open(&root.join("journal"));
    let record = journal
        .enqueue(scope(), create("Saved.txt"), BYTES)
        .unwrap();
    if phase != "pending" {
        let attempt = journal.claim_next().unwrap().unwrap();
        if phase == "uploaded" {
            journal
                .acknowledge(record.id, attempt.attempt.unwrap(), remote(&record))
                .unwrap();
        }
    }
    // Publish the marker atomically; its appearance follows the durable call.
    std::fs::write(
        root.join("marker.tmp"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    std::fs::rename(root.join("marker.tmp"), root.join("ready.json")).unwrap();
    loop {
        std::thread::park();
    }
}
