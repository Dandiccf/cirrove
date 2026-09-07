//! Removed names, retained streams and conditional deletion without cloud access.
#![allow(clippy::unwrap_used)]
use cirrove_core::{
    Node, NodeKind, Scope,
    mutation::{MutationIntent, MutationReceipt},
};
use cirrove_service::journal::{JournalError, MutationState, UploadJournal};
use std::{
    path::Path,
    time::{Duration, Instant},
};
fn scope() -> Scope {
    Scope {
        account: "unlinked".into(),
        provider: "fixture".into(),
        collection: "drive".into(),
    }
}
fn node(id: &str, name: &str, size: u64) -> Node {
    Node {
        id: id.into(),
        name: name.into(),
        parent_id: Some("root".into()),
        kind: NodeKind::File,
        size,
        modified_unix: 1,
        etag: Some("original".into()),
        content_version: Some("content".into()),
        target: None,
    }
}
fn journal(path: &Path) -> UploadJournal {
    let until = Instant::now() + Duration::from_secs(2);
    loop {
        match UploadJournal::open(path, &scope().account, 1024 * 1024) {
            Err(JournalError::Busy) if Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(1))
            }
            result => return result.unwrap(),
        }
    }
}
#[test]
fn an_online_only_unlink_releases_its_name_without_allocating_content() {
    let tmp = tempfile::tempdir().unwrap();
    let mut j = journal(&tmp.path().join("journal"));
    let remote = node("remote", "huge.bin", 500 * 1024 * 1024 * 1024);
    let object = j.observe_namespace_file(scope(), remote.clone()).unwrap();
    let removed = j
        .unlink_namespace_file(object.id, object.revision, false)
        .unwrap();
    assert!(removed.object.unlinked);
    assert!(removed.working.is_none());
    assert_eq!(j.retained_bytes().unwrap(), 0);
    assert!(
        j.namespace_overlay(&scope(), "root", vec![remote.clone()])
            .unwrap()
            .nodes
            .is_empty()
    );
    let other = node("someone-else", "huge.bin", 8);
    assert_eq!(
        j.namespace_overlay(&scope(), "root", vec![remote, other.clone()])
            .unwrap()
            .nodes,
        vec![other]
    );
    let claim = j.claim_mutation().unwrap().unwrap();
    assert_eq!(claim.id, removed.mutation.id);
    assert!(
        matches!(claim.request.intent,MutationIntent::RemoveFile{before} if before.id=="remote" && before.etag.as_deref()==Some("original"))
    );
    assert!(!j.namespace_is_clean(&removed.object).unwrap());
}
#[test]
fn unlinked_descriptor_writes_stay_local_and_a_reused_name_has_an_independent_stream() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("journal");
    let mut j = journal(&path);
    let old = j
        .create_working(scope(), node("", "same.txt", 0), true, b"".as_slice())
        .unwrap();
    j.write_working(old.id, 0, b"before").unwrap();
    let object = j
        .namespace_by_local(&scope(), &old.node.id)
        .unwrap()
        .unwrap();
    assert!(matches!(
        j.unlink_namespace_file(object.id, object.revision, false),
        Err(JournalError::Stale)
    ));
    let saved = j.seal_working(old.id).unwrap().unwrap();
    let object = j
        .namespace_by_local(&scope(), &old.node.id)
        .unwrap()
        .unwrap();
    let removed = j
        .unlink_namespace_file(object.id, object.revision, false)
        .unwrap();
    assert_eq!(
        removed.mutation.base.as_ref().unwrap().predecessor,
        saved.id
    );
    assert!(j.claim_mutation().unwrap().is_none());
    j.truncate_working(old.id, 0).unwrap();
    j.write_working(old.id, 0, b"orphaned descriptor").unwrap();
    assert!(j.seal_working(old.id).unwrap().is_none());
    assert!(j.working_file(old.id).unwrap().dirty);
    let new = j
        .create_working(scope(), node("", "same.txt", 0), true, b"".as_slice())
        .unwrap();
    j.write_working(new.id, 0, b"new path").unwrap();
    j.seal_working(new.id).unwrap();
    assert_ne!(old.node.id, new.node.id);
    let claim = j.claim_next().unwrap().unwrap();
    assert_eq!(claim.id, saved.id);
    let remote = node("assigned", "same.txt", 6);
    j.acknowledge(claim.id, claim.attempt.unwrap(), remote.clone())
        .unwrap();
    let deletion = j.claim_mutation().unwrap().unwrap();
    assert!(
        matches!(&deletion.request.intent,MutationIntent::RemoveFile{before} if before==&remote)
    );
    assert!(j.claim_next().unwrap().is_none());
    j.acknowledge_mutation(
        deletion.id,
        deletion.attempt.unwrap(),
        MutationReceipt::Removed {
            item: remote.id.clone(),
        },
    )
    .unwrap();
    assert_eq!(
        j.mutation(deletion.id).unwrap().state,
        MutationState::Applied
    );
    assert_eq!(
        j.namespace_overlay(&scope(), "root", vec![remote])
            .unwrap()
            .nodes
            .len(),
        1
    );
    assert_eq!(
        j.read_working(old.id, 0, 100).unwrap(),
        b"orphaned descriptor"
    );
    assert_eq!(j.read_working(new.id, 0, 100).unwrap(), b"new path");
    assert!(j.claim_next().unwrap().is_some());
    drop(j);
    let j = journal(&path);
    assert!(j.working_file(old.id).unwrap().unlinked);
    assert_eq!(
        j.read_working(old.id, 0, 100).unwrap(),
        b"orphaned descriptor"
    );
    assert_eq!(j.read_working(new.id, 0, 100).unwrap(), b"new path");
}
#[test]
fn unlink_rollback_keeps_the_entry_stream_and_operation_frontier_together() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("journal");
    let mut j = journal(&path);
    let file = j
        .create_working(
            scope(),
            node("remote", "same.txt", 3),
            false,
            b"old".as_slice(),
        )
        .unwrap();
    let object = j
        .namespace_by_local(&scope(), &file.node.id)
        .unwrap()
        .unwrap();
    let db = rusqlite::Connection::open(path.join("uploads.db")).unwrap();
    db.execute_batch("CREATE TRIGGER deny_unlink BEFORE UPDATE ON namespace_objects BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(
        j.unlink_namespace_file(object.id, object.revision, false)
            .is_err()
    );
    assert!(!j.working_file(file.id).unwrap().unlinked);
    assert_eq!(
        j.namespace_overlay(&scope(), "root", vec![])
            .unwrap()
            .nodes
            .len(),
        1
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM mutations", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        0
    );
    assert_eq!(j.read_working(file.id, 0, 20).unwrap(), b"old");
    db.execute_batch("DROP TRIGGER deny_unlink;").unwrap();
    let removed = j
        .unlink_namespace_file(object.id, object.revision, false)
        .unwrap();
    assert!(removed.object.unlinked);
    assert!(matches!(
        j.relocate_working(file.id, "root".into(), "restored".into()),
        Err(JournalError::Intent)
    ));
}

#[test]
fn failed_or_uncertain_deletion_never_discards_the_detached_stream() {
    for state in [
        MutationState::VerifyRequired,
        MutationState::Conflict,
        MutationState::Failed,
        MutationState::NeedsReview,
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("journal");
        let mut j = journal(&path);
        let file = j
            .create_working(
                scope(),
                node("remote", "file.txt", 3),
                false,
                b"old".as_slice(),
            )
            .unwrap();
        let object = j
            .namespace_by_local(&scope(), &file.node.id)
            .unwrap()
            .unwrap();
        let removed = j
            .unlink_namespace_file(object.id, object.revision, false)
            .unwrap();
        let claim = j.claim_mutation().unwrap().unwrap();
        j.defer_mutation(
            claim.id,
            claim.attempt.unwrap(),
            state,
            Duration::from_secs(300),
        )
        .unwrap();
        j.write_working(file.id, 0, b"retained").unwrap();
        j.seal_working(file.id).unwrap();
        assert_eq!(j.collect_retired_working(100).unwrap(), 0);
        assert!(
            !j.namespace_is_clean(&j.namespace_object(object.id).unwrap())
                .unwrap()
        );
        drop(j);
        let j = journal(&path);
        assert_eq!(j.mutation(removed.mutation.id).unwrap().state, state);
        assert_eq!(j.read_working(file.id, 0, 100).unwrap(), b"retained");
        assert!(
            j.namespace_overlay(&scope(), "root", vec![])
                .unwrap()
                .nodes
                .is_empty()
        );
    }
}
#[test]
fn schema_eight_migration_keeps_pending_streams_and_newer_schema_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("journal");
    let mut j = journal(&path);
    let file = j
        .create_working(
            scope(),
            node("remote", "file.txt", 3),
            false,
            b"old".as_slice(),
        )
        .unwrap();
    j.write_working(file.id, 0, b"new").unwrap();
    let saved = j.seal_working(file.id).unwrap().unwrap();
    drop(j);
    let db = rusqlite::Connection::open(path.join("uploads.db")).unwrap();
    db.execute_batch("CREATE TABLE old_working(id TEXT PRIMARY KEY, identity TEXT NOT NULL UNIQUE,slot TEXT NOT NULL UNIQUE,body TEXT NOT NULL);
        INSERT INTO old_working SELECT id,identity,slot,json_remove(body,'$.unlinked') FROM working_files;
        DROP TABLE working_files; ALTER TABLE old_working RENAME TO working_files;
        UPDATE namespace_objects SET body=json_remove(body,'$.unlinked'); PRAGMA user_version=8;").unwrap();
    drop(db);
    let mut j = journal(&path);
    assert!(!j.working_file(file.id).unwrap().unlinked);
    assert_eq!(j.read_working(file.id, 0, 100).unwrap(), b"new");
    let object = j
        .namespace_by_local(&scope(), &file.node.id)
        .unwrap()
        .unwrap();
    let removed = j
        .unlink_namespace_file(object.id, object.revision, false)
        .unwrap();
    assert_eq!(removed.mutation.base.unwrap().predecessor, saved.id);
    drop(j);
    let db = rusqlite::Connection::open(path.join("uploads.db")).unwrap();
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        12
    );
    db.execute_batch("PRAGMA user_version=13;").unwrap();
    drop(db);
    assert!(matches!(
        UploadJournal::open(&path, &scope().account, 1024 * 1024),
        Err(JournalError::Schema)
    ));
}

#[test]
fn reader_barriers_gate_only_their_delete_and_expire_with_the_old_owner() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("journal");
    let mut j = journal(&path);
    let first = j
        .observe_namespace_file(scope(), node("first", "first.txt", 7))
        .unwrap();
    let blocked = j
        .unlink_namespace_file(first.id, first.revision, true)
        .unwrap();
    assert!(!blocked.mutation.local_ready);
    assert!(j.claim_mutation().unwrap().is_none());
    let second = j
        .observe_namespace_file(scope(), node("second", "second.txt", 7))
        .unwrap();
    let independent = j
        .unlink_namespace_file(second.id, second.revision, false)
        .unwrap();
    let claim = j.claim_mutation().unwrap().unwrap();
    assert_eq!(claim.id, independent.mutation.id);
    j.acknowledge_mutation(
        claim.id,
        claim.attempt.unwrap(),
        MutationReceipt::Removed {
            item: "second".into(),
        },
    )
    .unwrap();
    assert!(j.claim_mutation().unwrap().is_none());
    assert_eq!(j.unlinked_readers(0, 16).unwrap().len(), 1);
    drop(j);
    let mut j = journal(&path);
    assert!(j.unlinked_readers(0, 16).unwrap().is_empty());
    let claim = j.claim_mutation().unwrap().unwrap();
    assert_eq!(claim.id, blocked.mutation.id);
    assert!(claim.local_ready);
    assert!(
        j.namespace_overlay(&scope(), "root", vec![])
            .unwrap()
            .nodes
            .is_empty()
    );
}
