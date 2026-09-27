//! A staged provider replacement must publish both identities or neither.
#![allow(clippy::unwrap_used)]
use cirrove_core::{Node, NodeKind, Scope};
use cirrove_service::journal::{UploadJournal, UploadState};
use std::os::unix::fs::PermissionsExt;

fn scope() -> Scope {
    Scope {
        account: "handoff-two-ids".into(),
        provider: "fixture".into(),
        collection: "drive".into(),
    }
}

fn file(id: &str, name: &str, revision: &str, size: u64) -> Node {
    Node {
        id: id.into(),
        parent_id: Some("root".into()),
        name: name.into(),
        kind: NodeKind::File,
        size,
        modified_unix: 1,
        etag: Some(revision.into()),
        content_version: Some(revision.into()),
        target: None,
        package: false,
    }
}

fn open(root: &std::path::Path) -> UploadJournal {
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700)).unwrap();
    UploadJournal::open(root, &scope().account, 4096).unwrap()
}

#[test]
fn staged_replacement_keeps_old_identity_and_stable_local_file_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let mut journal = open(root.path());
    let original = file("old-item", "report.txt", "old-version", 3);
    let working = journal
        .create_working(scope(), original.clone(), false, b"old".as_slice())
        .unwrap();
    journal.write_working(working.id, 0, b"new!").unwrap();
    let upload = journal.seal_working(working.id).unwrap().unwrap();
    let claimed = journal.claim_next().unwrap().unwrap();
    assert_eq!(claimed.id, upload.id);
    let attempt = claimed.attempt.unwrap();
    let recovery = journal
        .reserve_identity_handoff(upload.id, attempt, "recovery-cirrove.txt".into())
        .unwrap();
    assert!(!journal.namespace_object(recovery).unwrap().remote_owned);
    let backup = file("old-item", "recovery-cirrove.txt", "renamed-old", 3);
    let current = file("new-item", "report.txt", "new-version", 4);
    assert!(
        journal
            .acknowledge(upload.id, attempt, current.clone())
            .is_err()
    );
    journal
        .acknowledge_identity_handoff(upload.id, attempt, current.clone(), backup.clone())
        .unwrap();
    assert_eq!(journal.get(upload.id).unwrap().state, UploadState::Uploaded);
    let original_local = journal
        .namespace_by_local(&scope(), "old-item")
        .unwrap()
        .unwrap();
    assert_eq!(original_local.remote.as_ref().unwrap().id, "new-item");
    assert_eq!(
        journal
            .namespace_by_remote(&scope(), "new-item")
            .unwrap()
            .unwrap()
            .id,
        original_local.id
    );
    let recovered = journal
        .namespace_by_remote(&scope(), "old-item")
        .unwrap()
        .unwrap();
    assert_eq!(recovered.id, recovery);
    assert!(recovered.unlinked);
    assert!(recovered.remote_owned);
    assert_eq!(recovered.remote.unwrap(), backup);
    let visible = journal
        .namespace_overlay(&scope(), "root", vec![backup.clone(), current.clone()])
        .unwrap();
    assert_eq!(visible.nodes.len(), 1);
    assert_eq!(visible.nodes[0].id, "old-item");
    assert!(visible.conflicts.is_empty());
    drop(journal);

    let mut journal = open(root.path());
    assert_eq!(
        journal
            .namespace_by_remote(&scope(), "old-item")
            .unwrap()
            .unwrap()
            .id,
        recovery
    );
    assert_eq!(journal.read_working(working.id, 0, 4).unwrap(), b"new!");
    let successor = journal
        .enqueue_after(upload.id, b"later".as_slice())
        .unwrap();
    let claimed = journal.claim_next().unwrap().unwrap();
    assert_eq!(claimed.id, successor.id);
    assert!(
        matches!(claimed.intent, cirrove_service::journal::UploadIntent::Replace { item, .. } if item == "new-item")
    );
}

#[test]
fn incorrect_backup_receipt_does_not_partially_transfer_either_identity() {
    let root = tempfile::tempdir().unwrap();
    let mut journal = open(root.path());
    let working = journal
        .create_working(
            scope(),
            file("old-item", "report.txt", "old-version", 3),
            false,
            b"old".as_slice(),
        )
        .unwrap();
    journal.write_working(working.id, 0, b"new!").unwrap();
    let upload = journal.seal_working(working.id).unwrap().unwrap();
    let attempt = journal.claim_next().unwrap().unwrap().attempt.unwrap();
    let recovery = journal
        .reserve_identity_handoff(upload.id, attempt, "recovery-cirrove.txt".into())
        .unwrap();
    let current = file("new-item", "report.txt", "new-version", 4);
    let wrong = file("old-item", "other-name.txt", "renamed-old", 3);
    assert!(
        journal
            .acknowledge_identity_handoff(upload.id, attempt, current.clone(), wrong)
            .is_err()
    );
    assert_eq!(
        journal.get(upload.id).unwrap().state,
        UploadState::Uploading
    );
    assert_eq!(
        journal
            .namespace_by_remote(&scope(), "old-item")
            .unwrap()
            .unwrap()
            .remote
            .unwrap()
            .name,
        "report.txt"
    );
    assert!(
        journal
            .namespace_by_remote(&scope(), "new-item")
            .unwrap()
            .is_none()
    );
    assert!(!journal.namespace_object(recovery).unwrap().remote_owned);
    assert_eq!(journal.read_working(working.id, 0, 4).unwrap(), b"new!");
}

#[test]
fn reserved_recovery_survives_process_death_before_receipt() {
    let root = tempfile::tempdir().unwrap();
    let mut journal = open(root.path());
    let working = journal
        .create_working(
            scope(),
            file("old-item", "report.txt", "old-version", 3),
            false,
            b"old".as_slice(),
        )
        .unwrap();
    journal.write_working(working.id, 0, b"new!").unwrap();
    let upload = journal.seal_working(working.id).unwrap().unwrap();
    let first = journal.claim_next().unwrap().unwrap();
    let recovery = journal
        .reserve_identity_handoff(
            upload.id,
            first.attempt.unwrap(),
            "recovery-cirrove.txt".into(),
        )
        .unwrap();
    drop(journal);

    let mut journal = open(root.path());
    assert_eq!(
        journal.get(upload.id).unwrap().state,
        UploadState::VerifyRequired
    );
    let verification = journal.claim_verification(upload.id).unwrap();
    let attempt = verification.attempt.unwrap();
    assert_eq!(
        journal
            .reserve_identity_handoff(upload.id, attempt, "recovery-cirrove.txt".into())
            .unwrap(),
        recovery
    );
    assert!(
        journal
            .reserve_identity_handoff(upload.id, attempt, "different-name.txt".into())
            .is_err()
    );
    journal
        .acknowledge_identity_handoff(
            upload.id,
            attempt,
            file("new-item", "report.txt", "new-version", 4),
            file("old-item", "recovery-cirrove.txt", "renamed-old", 3),
        )
        .unwrap();
    assert_eq!(journal.get(upload.id).unwrap().state, UploadState::Uploaded);
    assert_eq!(
        journal
            .namespace_by_remote(&scope(), "old-item")
            .unwrap()
            .unwrap()
            .id,
        recovery
    );
}
