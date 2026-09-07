//! Durable local paths without content hydration; no network or cloud accounts.
#![allow(clippy::unwrap_used)]
use cirrove_core::{Node, NodeKind, Scope, mutation::*};
use cirrove_service::journal::{
    JournalError, NamespaceNames, UploadIntent, UploadJournal, UploadState,
};
use std::{
    io::Read,
    path::Path,
    time::{Duration, Instant},
};

fn scope() -> Scope {
    Scope {
        account: "namespace-fixture".into(),
        provider: "fixture".into(),
        collection: "drive".into(),
    }
}
fn node(id: &str, name: &str, size: u64) -> Node {
    Node {
        id: id.into(),
        parent_id: Some("root".into()),
        name: name.into(),
        kind: NodeKind::File,
        size,
        modified_unix: 1,
        etag: Some(format!("etag-{id}")),
        content_version: Some(format!("content-{id}")),
        target: None,
    }
}
fn open(path: &Path, quota: u64) -> UploadJournal {
    let until = Instant::now() + Duration::from_secs(2);
    loop {
        match UploadJournal::open(path, &scope().account, quota) {
            Err(JournalError::Busy) if Instant::now() < until => {
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
fn huge_online_only_file_moves_without_working_bytes_and_survives_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("journal");
    let mut j = open(&path, 1024);
    let remote = node("cloud-id", "Huge.bin", 500 * 1024 * 1024 * 1024);
    let original = j.observe_namespace_file(scope(), remote.clone()).unwrap();
    let renamed = j
        .relocate_namespace_file(
            original.id,
            original.revision,
            "other".into(),
            "Große Datei.bin".into(),
        )
        .unwrap();
    assert!(j.working_files().unwrap().is_empty());
    assert_eq!(j.retained_bytes().unwrap(), 0);
    assert_eq!(std::fs::read_dir(path.join("working")).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(path.join("objects")).unwrap().count(), 0);
    let before = j
        .namespace_overlay(&scope(), "root", vec![remote.clone()])
        .unwrap();
    assert!(before.nodes.is_empty());
    let after = j.namespace_overlay(&scope(), "other", vec![]).unwrap();
    assert_eq!(after.nodes.len(), 1);
    assert_eq!(after.nodes[0].id, remote.id);
    drop(j);
    let mut j = open(&path, 1024);
    let recovered = j.namespace_object(original.id).unwrap();
    assert_eq!(recovered.latest, Some(renamed.id));
    assert!(recovered.working_file.is_none());
    let claimed = j.claim_mutation().unwrap().unwrap();
    assert_eq!(claimed.request.intent.before(), Some(&remote));
    let mut receipt = remote.clone();
    receipt.name = "Große Datei.bin".into();
    receipt.parent_id = Some("other".into());
    receipt.etag = Some("renamed-etag".into());
    j.acknowledge_mutation(
        claimed.id,
        claimed.attempt.unwrap(),
        MutationReceipt::Upsert(receipt.clone()),
    )
    .unwrap();
    let object = j.namespace_object(original.id).unwrap();
    assert_eq!(object.remote, Some(receipt));
    assert_eq!(object.node.id, remote.id);
    assert_eq!(j.retained_bytes().unwrap(), 0);
}

#[test]
fn hydration_after_pending_rename_keeps_identity_name_and_receipt_dependency() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("journal");
    let mut j = open(&path, 1024);
    let remote = node("cloud-id", "Before.txt", 3);
    let object = j.observe_namespace_file(scope(), remote.clone()).unwrap();
    let rename = j
        .relocate_namespace_file(
            object.id,
            object.revision,
            "root".into(),
            "After.txt".into(),
        )
        .unwrap();
    let working = j
        .create_working(scope(), remote.clone(), false, b"old".as_slice())
        .unwrap();
    assert_eq!(working.latest, Some(rename.id));
    assert_eq!(working.node.name, "After.txt");
    let after = j
        .namespace_by_remote(&scope(), &remote.id)
        .unwrap()
        .unwrap();
    assert_eq!(after.id, object.id);
    assert_eq!(after.working_file, Some(working.id));
    j.write_working(working.id, 0, b"new").unwrap();
    let save = j.seal_working(working.id).unwrap().unwrap();
    assert_eq!(save.base.as_ref().unwrap().predecessor, rename.id);
    assert!(j.claim_next().unwrap().is_none());
    let active = j.claim_mutation().unwrap().unwrap();
    let mut receipt = remote;
    receipt.name = "After.txt".into();
    receipt.etag = Some("rename-result".into());
    j.acknowledge_mutation(
        active.id,
        active.attempt.unwrap(),
        MutationReceipt::Upsert(receipt),
    )
    .unwrap();
    let upload = j.claim_next().unwrap().unwrap();
    assert_eq!(
        upload.intent,
        UploadIntent::Replace {
            item: "cloud-id".into(),
            expected_etag: "rename-result".into()
        }
    );
    assert_eq!(payload(&j, upload.id), b"new");
    drop(j);
    let j = open(&path, 1024);
    assert_eq!(
        j.namespace_object(object.id).unwrap().working_file,
        Some(working.id)
    );
}

#[test]
fn hydration_uses_current_local_path_when_the_old_name_has_been_reused() {
    let tmp = tempfile::tempdir().unwrap();
    let mut j = open(&tmp.path().join("journal"), 1024);
    let remote = node("cloud-id", "Before.txt", 3);
    let object = j.observe_namespace_file(scope(), remote.clone()).unwrap();
    j.relocate_namespace_file(
        object.id,
        object.revision,
        "root".into(),
        "After.txt".into(),
    )
    .unwrap();
    let replacement = j
        .create_working(
            scope(),
            node("unused", "Before.txt", 0),
            true,
            b"".as_slice(),
        )
        .unwrap();
    j.write_working(replacement.id, 0, b"keep me").unwrap();
    // The read began against old cloud metadata. Its file content is still valid,
    // but the local namespace now has two different objects at the two names.
    let hydrated = j
        .create_working(scope(), remote, false, b"old".as_slice())
        .unwrap();
    assert_eq!(hydrated.node.name, "After.txt");
    assert_eq!(j.read_working(replacement.id, 0, 100).unwrap(), b"keep me");
    assert_eq!(
        j.namespace_overlay(&scope(), "root", vec![])
            .unwrap()
            .nodes
            .len(),
        2
    );
}

#[test]
fn stale_content_cannot_attach_to_a_newer_remote_namespace_binding() {
    let tmp = tempfile::tempdir().unwrap();
    let mut j = open(&tmp.path().join("journal"), 1024);
    let original = node("cloud-id", "Before.txt", 3);
    let mut fresh = original.clone();
    fresh.content_version = Some("new-content".into());
    fresh.etag = Some("new-etag".into());
    j.observe_namespace_file(scope(), fresh.clone()).unwrap();
    assert!(matches!(
        j.create_working(scope(), original, false, b"old".as_slice()),
        Err(JournalError::Stale)
    ));
    assert!(j.working_files().unwrap().is_empty());
    let object = j.namespace_by_remote(&scope(), &fresh.id).unwrap().unwrap();
    assert_eq!(object.remote, Some(fresh));
    assert!(object.working_file.is_none());
}

#[test]
fn truncated_online_only_object_keeps_its_original_remote_version_without_hydration() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("journal");
    let mut j = open(&path, 1024);
    let remote = node("cloud-id", "Huge.bin", 500 * 1024 * 1024 * 1024);
    let object = j.observe_namespace_file(scope(), remote.clone()).unwrap();
    let rename = j
        .relocate_namespace_file(object.id, object.revision, "other".into(), "New.bin".into())
        .unwrap();
    let working = j.create_truncated_working(scope(), remote.clone()).unwrap();
    assert_eq!(working.initial_remote, Some(remote));
    assert!(working.dirty);
    assert_eq!(working.node.size, 0);
    assert_eq!(working.latest, Some(rename.id));
    assert_eq!(working.node.name, "New.bin");
    assert_eq!(j.retained_bytes().unwrap(), 0);
    j.write_working(working.id, 0, b"small").unwrap();
    let upload = j.seal_working(working.id).unwrap().unwrap();
    assert_eq!(payload(&j, upload.id), b"small");
    assert_eq!(upload.base.unwrap().predecessor, rename.id);
}

#[test]
fn failed_metadata_transaction_does_not_change_path_or_consume_predecessor() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("journal");
    let mut j = open(&path, 1024);
    let remote = node("cloud-id", "Before.txt", 5);
    let original = j.observe_namespace_file(scope(), remote.clone()).unwrap();
    let db = rusqlite::Connection::open(path.join("uploads.db")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_namespace BEFORE INSERT ON namespace_entries BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(
        j.relocate_namespace_file(
            original.id,
            original.revision,
            "root".into(),
            "After.txt".into()
        )
        .is_err()
    );
    assert_eq!(j.namespace_object(original.id).unwrap().node, remote);
    assert!(j.list_mutations(0, 10).unwrap().is_empty());
    db.execute_batch("DROP TRIGGER fail_namespace;").unwrap();
    let first = j
        .relocate_namespace_file(
            original.id,
            original.revision,
            "root".into(),
            "After.txt".into(),
        )
        .unwrap();
    let next = j.namespace_object(original.id).unwrap();
    db.execute_batch("CREATE TRIGGER fail_namespace BEFORE INSERT ON namespace_entries BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(
        j.relocate_namespace_file(next.id, next.revision, "other".into(), "Last.txt".into())
            .is_err()
    );
    assert_eq!(j.namespace_object(next.id).unwrap().latest, Some(first.id));
    db.execute_batch("DROP TRIGGER fail_namespace;").unwrap();
    assert!(
        j.relocate_namespace_file(next.id, next.revision, "other".into(), "Last.txt".into())
            .is_ok()
    );
    assert!(matches!(
        j.relocate_namespace_file(
            next.id,
            next.revision,
            "elsewhere".into(),
            "Stale.txt".into()
        ),
        Err(JournalError::Stale)
    ));
}

#[test]
fn local_destination_collisions_preserve_objects_and_remote_collisions_are_explicit() {
    let tmp = tempfile::tempdir().unwrap();
    let mut j = open(&tmp.path().join("journal"), 1024);
    let remote = node("one", "First.txt", 2);
    let other = node("two", "Second.txt", 3);
    let first = j.observe_namespace_file(scope(), remote.clone()).unwrap();
    let second = j.observe_namespace_file(scope(), other.clone()).unwrap();
    assert!(matches!(
        j.relocate_namespace_file(first.id, first.revision, "root".into(), "SECOND.txt".into()),
        Err(JournalError::Stale)
    ));
    assert_eq!(j.namespace_object(second.id).unwrap().node, other);
    let change = j
        .relocate_namespace_file(
            first.id,
            first.revision,
            "other".into(),
            "Target.txt".into(),
        )
        .unwrap();
    let mut occupant = node("foreign", "target.txt", 9);
    occupant.parent_id = Some("other".into());
    let listing = j
        .namespace_overlay(&scope(), "other", vec![occupant.clone()])
        .unwrap();
    assert_eq!(listing.nodes.len(), 1);
    assert_eq!(listing.nodes[0].id, remote.id);
    assert_eq!(listing.conflicts.len(), 1);
    assert_eq!(listing.conflicts[0].local, first.id);
    assert_eq!(listing.conflicts[0].remote, occupant);
    assert_eq!(
        j.namespace_object(first.id).unwrap().latest,
        Some(change.id)
    );
}

#[test]
fn namespace_name_policy_is_explicit_and_persists_for_later_working_files() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("journal");
    let mut j = open(&path, 1024);
    j.configure_namespace(&scope(), NamespaceNames::Sensitive)
        .unwrap();
    let first = node("one", "File.txt", 3);
    let second = node("two", "file.txt", 3);
    j.observe_namespace_file(scope(), first.clone()).unwrap();
    j.observe_namespace_file(scope(), second.clone()).unwrap();
    j.create_working(scope(), first, false, b"one".as_slice())
        .unwrap();
    j.create_working(scope(), second, false, b"two".as_slice())
        .unwrap();
    assert!(matches!(
        j.configure_namespace(&scope(), NamespaceNames::Insensitive),
        Err(JournalError::Stale)
    ));
    drop(j);
    let j = open(&path, 1024);
    let listing = j.namespace_overlay(&scope(), "root", vec![]).unwrap();
    assert_eq!(listing.nodes.len(), 2);
    assert!(listing.conflicts.is_empty());
}

#[test]
fn receipts_preserve_local_identity_and_do_not_duplicate_uploaded_creates() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("journal");
    let mut j = open(&path, 1024);
    let working = j
        .create_working(
            scope(),
            node("unused", "Created.txt", 0),
            true,
            b"".as_slice(),
        )
        .unwrap();
    let local = j
        .namespace_by_local(&scope(), &working.node.id)
        .unwrap()
        .unwrap();
    j.write_working(working.id, 0, b"new").unwrap();
    let upload = j.seal_working(working.id).unwrap().unwrap();
    let active = j.claim_next().unwrap().unwrap();
    let remote = node("cloud-assigned", "Created.txt", 3);
    j.acknowledge(upload.id, active.attempt.unwrap(), remote.clone())
        .unwrap();
    let assigned = j
        .namespace_by_remote(&scope(), &remote.id)
        .unwrap()
        .unwrap();
    assert_eq!(assigned.id, local.id);
    assert_eq!(assigned.node.id, working.node.id);
    assert_eq!(assigned.remote, Some(remote.clone()));
    assert!(
        j.namespace_by_local(&scope(), &remote.id)
            .unwrap()
            .is_none()
    );
    assert!(
        j.namespace_by_remote(&scope(), &working.node.id)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        j.namespace_by_local(&scope(), &working.node.id)
            .unwrap()
            .unwrap()
            .id,
        local.id
    );
    let listing = j
        .namespace_overlay(&scope(), "root", vec![remote.clone()])
        .unwrap();
    assert_eq!(listing.nodes.len(), 1);
    assert_eq!(listing.nodes[0].id, working.node.id);
    drop(j);
    let j = open(&path, 1024);
    assert!(
        j.namespace_by_local(&scope(), &remote.id)
            .unwrap()
            .is_none()
    );
    assert!(
        j.namespace_by_remote(&scope(), &working.node.id)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        j.namespace_by_remote(&scope(), &remote.id)
            .unwrap()
            .unwrap()
            .id,
        local.id
    );
}

#[test]
fn namespace_and_upload_receipt_acknowledge_in_one_transaction() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("journal");
    let mut j = open(&path, 1024);
    let working = j
        .create_working(
            scope(),
            node("unused", "Created.txt", 0),
            true,
            b"".as_slice(),
        )
        .unwrap();
    let upload = j.seal_working(working.id).unwrap().unwrap();
    let active = j.claim_next().unwrap().unwrap();
    let db = rusqlite::Connection::open(path.join("uploads.db")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_binding BEFORE INSERT ON namespace_remote BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(
        j.acknowledge(
            upload.id,
            active.attempt.unwrap(),
            node("assigned", "Created.txt", 0)
        )
        .is_err()
    );
    assert_eq!(j.get(upload.id).unwrap().state, UploadState::Uploading);
    assert!(
        j.namespace_by_remote(&scope(), "assigned")
            .unwrap()
            .is_none()
    );
    db.execute_batch("DROP TRIGGER fail_binding;").unwrap();
    j.acknowledge(
        upload.id,
        active.attempt.unwrap(),
        node("assigned", "Created.txt", 0),
    )
    .unwrap();
    assert_eq!(j.get(upload.id).unwrap().state, UploadState::Uploaded);
    assert!(
        j.namespace_by_remote(&scope(), "assigned")
            .unwrap()
            .is_some()
    );
}

#[test]
fn namespace_scopes_and_conflicting_remote_aliases_cannot_merge_local_objects() {
    let tmp = tempfile::tempdir().unwrap();
    let mut j = open(&tmp.path().join("journal"), 1024);
    let first = j
        .observe_namespace_file(scope(), node("same", "First.txt", 3))
        .unwrap();
    let mut other_scope = scope();
    other_scope.collection = "second-drive".into();
    let second = j
        .observe_namespace_file(other_scope.clone(), node("same", "Second.txt", 3))
        .unwrap();
    assert_ne!(first.id, second.id);
    assert_eq!(
        j.namespace_by_remote(&other_scope, "same")
            .unwrap()
            .unwrap()
            .id,
        second.id
    );
    let mut alien = scope();
    alien.account = "other-account".into();
    assert!(matches!(
        j.observe_namespace_file(alien, node("same", "Alien.txt", 3)),
        Err(JournalError::Account)
    ));
    let local = j
        .create_working(scope(), node("unused", "New.txt", 0), true, b"".as_slice())
        .unwrap();
    let upload = j.seal_working(local.id).unwrap().unwrap();
    let active = j.claim_next().unwrap().unwrap();
    // A malformed provider receipt may not bind this new object to another
    // already tracked file, even if its name/size pass the upload shape check.
    assert!(
        j.acknowledge(
            upload.id,
            active.attempt.unwrap(),
            node("same", "New.txt", 0)
        )
        .is_err()
    );
    assert_eq!(j.get(upload.id).unwrap().state, UploadState::Uploading);
    assert_eq!(
        j.namespace_by_remote(&scope(), "same").unwrap().unwrap().id,
        first.id
    );
    assert_eq!(j.namespace_object(first.id).unwrap().node.name, "First.txt");
}

#[test]
fn schema_six_working_files_keep_confirmed_remote_bindings_and_pending_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("journal");
    let mut j = open(&path, 1024);
    let working = j
        .create_working(
            scope(),
            node("unused", "Created.txt", 0),
            true,
            b"".as_slice(),
        )
        .unwrap();
    j.write_working(working.id, 0, b"old").unwrap();
    let first = j.seal_working(working.id).unwrap().unwrap();
    let active = j.claim_next().unwrap().unwrap();
    let remote = node("assigned", "Created.txt", 3);
    j.acknowledge(first.id, active.attempt.unwrap(), remote.clone())
        .unwrap();
    j.write_working(working.id, 0, b"new").unwrap();
    let next = j.seal_working(working.id).unwrap().unwrap();
    drop(j);
    let db = rusqlite::Connection::open(path.join("uploads.db")).unwrap();
    db.execute_batch("DROP TABLE namespace_operations; DROP TABLE namespace_remote; DROP TABLE namespace_entries;
        DROP TABLE namespace_objects; DROP TABLE namespace_scopes; PRAGMA user_version=6;").unwrap();
    drop(db);
    let j = open(&path, 1024);
    let migrated = j
        .namespace_by_remote(&scope(), &remote.id)
        .unwrap()
        .unwrap();
    assert_eq!(migrated.id, working.id);
    assert_eq!(migrated.latest, Some(next.id));
    assert_eq!(migrated.remote, Some(remote.clone()));
    assert_eq!(payload(&j, next.id), b"new");
    assert_eq!(
        j.namespace_overlay(&scope(), "root", vec![remote])
            .unwrap()
            .nodes
            .len(),
        1
    );
}

#[test]
fn provider_observations_cannot_adopt_an_unrelated_local_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("journal");
    let mut j = open(&path, 1024);
    let working = j
        .create_working(scope(), node("", "Local.txt", 0), true, b"".as_slice())
        .unwrap();
    j.write_working(working.id, 0, b"local unsent").unwrap();
    let before = j
        .namespace_by_local(&scope(), &working.node.id)
        .unwrap()
        .unwrap();
    // IDs are opaque. A provider ID must not be treated as the local object
    // merely because its string equals a locally allocated ID.
    let remote = node(&working.node.id, "Foreign.txt", 12);
    assert!(j.observe_namespace_file(scope(), remote.clone()).is_err());
    assert!(
        j.namespace_by_remote(&scope(), &remote.id)
            .unwrap()
            .is_none()
    );
    assert_eq!(j.namespace_objects().unwrap().len(), 1);
    assert_eq!(
        j.namespace_object(before.id).unwrap().revision,
        before.revision
    );
    assert_eq!(j.namespace_object(before.id).unwrap().node, before.node);
    assert_eq!(j.read_working(working.id, 0, 100).unwrap(), b"local unsent");
    drop(j);
    let j = open(&path, 1024);
    assert!(
        j.namespace_by_remote(&scope(), &remote.id)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        j.namespace_by_local(&scope(), &working.node.id)
            .unwrap()
            .unwrap()
            .id,
        before.id
    );
    assert_eq!(j.read_working(working.id, 0, 100).unwrap(), b"local unsent");
}
