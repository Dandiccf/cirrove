//! Real journal transactions for local replacement and paired cloud bindings.
//! Generated local bytes only; actual FUSE replacement is validated separately.
#![allow(clippy::unwrap_used)]
use cirrove_core::{Node, NodeKind, Scope, mutation::MutationReceipt};
use cirrove_service::journal::{
    JournalError, NamespaceObject, UploadIntent, UploadJournal, UploadState, WorkingFile,
};
use std::{
    path::Path,
    time::{Duration, Instant},
};

fn scope() -> Scope {
    Scope {
        account: "replace-fixture".into(),
        provider: "fixture".into(),
        collection: "drive".into(),
    }
}
fn open(path: &Path) -> UploadJournal {
    let until = Instant::now() + Duration::from_secs(2);
    loop {
        match UploadJournal::open(path, &scope().account, 1024 * 1024) {
            Err(JournalError::Busy) if Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(2))
            }
            r => return r.unwrap(),
        }
    }
}
fn node(id: &str, name: &str, tag: &str, size: u64) -> Node {
    Node {
        id: id.into(),
        name: name.into(),
        parent_id: Some("root".into()),
        kind: NodeKind::File,
        size,
        etag: Some(tag.into()),
        content_version: Some(format!("content-{tag}")),
        modified_unix: 1,
        target: None,
    }
}
fn object(j: &UploadJournal, file: &WorkingFile) -> NamespaceObject {
    j.namespace_by_local(&scope(), &file.node.id)
        .unwrap()
        .unwrap()
}
fn source(j: &mut UploadJournal, name: &str) -> WorkingFile {
    let file = j
        .create_working(scope(), node("", name, "unused", 0), true, b"".as_slice())
        .unwrap();
    j.write_working(file.id, 0, b"new").unwrap();
    j.seal_working(file.id).unwrap();
    j.working_file(file.id).unwrap()
}
fn victim(j: &mut UploadJournal) -> WorkingFile {
    j.create_working(
        scope(),
        node("target-id", "document", "target-etag", 3),
        false,
        b"old".as_slice(),
    )
    .unwrap()
}
fn ack_source(j: &mut UploadJournal, file: &WorkingFile, id: &str) {
    let active = j.claim_next().unwrap().unwrap();
    assert_eq!(Some(active.id), file.latest);
    j.acknowledge(
        active.id,
        active.attempt.unwrap(),
        node(id, &file.node.name, "source-etag", 3),
    )
    .unwrap();
}
fn names(j: &UploadJournal, remote: Vec<Node>) -> Vec<String> {
    j.namespace_overlay(&scope(), "root", remote)
        .unwrap()
        .nodes
        .into_iter()
        .map(|n| n.name)
        .collect()
}

#[test]
fn local_swap_retains_old_stream_and_receipt_transfers_only_active_cloud_ownership() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("journal");
    let mut j = open(&path);
    let old = victim(&mut j);
    let new = source(&mut j, "temporary");
    let src = object(&j, &new);
    let dst = object(&j, &old);
    let replacement = j
        .replace_namespace_file(src.id, src.revision, dst.id, dst.revision, true)
        .unwrap();
    assert_eq!(
        names(&j, vec![node("target-id", "document", "target-etag", 3)]),
        vec!["document"]
    );
    assert_eq!(j.namespace_object(src.id).unwrap().node.name, "document");
    assert!(j.namespace_object(dst.id).unwrap().unlinked);
    assert!(j.working_file(old.id).unwrap().unlinked);
    assert_eq!(j.read_working(old.id, 0, 100).unwrap(), b"old");
    assert_eq!(j.read_working(new.id, 0, 100).unwrap(), b"new");
    j.write_working(old.id, 0, b"retained old descriptor")
        .unwrap();
    assert!(j.seal_working(old.id).unwrap().is_none());
    ack_source(&mut j, &new, "source-id");
    assert!(j.claim_next().unwrap().is_none());
    assert!(j.claim_mutation().unwrap().is_none());
    assert_eq!(j.replacement_readers(0, 16).unwrap().len(), 1);
    j.release_replacement_readers(replacement.id).unwrap();
    let upload = j.claim_next().unwrap().unwrap();
    assert_eq!(upload.id, replacement.id);
    assert_eq!(
        upload.intent,
        UploadIntent::Replace {
            item: "target-id".into(),
            expected_etag: "target-etag".into()
        }
    );
    let receipt = node("target-id", "document", "replacement-etag", 3);
    j.acknowledge(upload.id, upload.attempt.unwrap(), receipt.clone())
        .unwrap();
    assert!(j.replacement(replacement.id).unwrap().remote_applied);
    assert_eq!(
        j.namespace_by_remote(&scope(), "target-id")
            .unwrap()
            .unwrap()
            .id,
        src.id
    );
    assert_eq!(
        j.namespace_by_local(&scope(), "target-id")
            .unwrap()
            .unwrap()
            .id,
        dst.id
    );
    let old_object = j.namespace_object(dst.id).unwrap();
    assert!(!old_object.remote_owned);
    assert_eq!(old_object.remote.unwrap().id, "target-id");
    assert_eq!(
        j.namespace_by_remote(&scope(), "source-id")
            .unwrap()
            .unwrap()
            .id,
        replacement.cleanup_object
    );
    assert_eq!(
        names(
            &j,
            vec![receipt, node("source-id", "temporary", "source-etag", 3)]
        ),
        vec!["document"]
    );
    let cleanup = j.claim_mutation().unwrap().unwrap();
    assert_eq!(cleanup.id, replacement.cleanup);
    assert_eq!(cleanup.request.intent.before().unwrap().id, "source-id");
    j.acknowledge_mutation(
        cleanup.id,
        cleanup.attempt.unwrap(),
        MutationReceipt::Removed {
            item: "source-id".into(),
        },
    )
    .unwrap();
    j.write_working(new.id, 0, b"next").unwrap();
    let saved = j.seal_working(new.id).unwrap().unwrap();
    let claimed = j.claim_next().unwrap().unwrap();
    assert_eq!(claimed.id, saved.id);
    assert_eq!(
        claimed.intent,
        UploadIntent::Replace {
            item: "target-id".into(),
            expected_etag: "replacement-etag".into()
        }
    );
    drop(j);
    let j = open(&path);
    assert_eq!(
        j.namespace_by_remote(&scope(), "target-id")
            .unwrap()
            .unwrap()
            .id,
        src.id
    );
    assert_eq!(
        j.read_working(old.id, 0, 100).unwrap(),
        b"retained old descriptor"
    );
    assert_eq!(j.read_working(new.id, 0, 100).unwrap(), b"next");
}

#[test]
fn both_pending_creates_supply_their_own_receipts_and_conflict_keeps_every_stream() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("journal");
    let mut j = open(&path);
    let old = source(&mut j, "document");
    let new = source(&mut j, "temporary");
    let src = object(&j, &new);
    let dst = object(&j, &old);
    let replace = j
        .replace_namespace_file(src.id, src.revision, dst.id, dst.revision, false)
        .unwrap();
    ack_source(&mut j, &old, "target-id");
    ack_source(&mut j, &new, "source-id");
    let upload = j.claim_next().unwrap().unwrap();
    assert_eq!(upload.id, replace.id);
    assert_eq!(
        upload.intent,
        UploadIntent::Replace {
            item: "target-id".into(),
            expected_etag: "source-etag".into()
        }
    );
    j.stop_attempt(upload.id, upload.attempt.unwrap(), UploadState::Conflict)
        .unwrap();
    assert!(j.claim_mutation().unwrap().is_none());
    let extra = source(&mut j, "independent");
    ack_source(&mut j, &extra, "other-id");
    drop(j);
    let j = open(&path);
    assert_eq!(j.get(replace.id).unwrap().state, UploadState::Conflict);
    assert_eq!(j.read_working(old.id, 0, 100).unwrap(), b"new");
    assert_eq!(j.read_working(new.id, 0, 100).unwrap(), b"new");
    assert_eq!(
        j.namespace_by_remote(&scope(), "target-id")
            .unwrap()
            .unwrap()
            .id,
        dst.id
    );
    assert_eq!(
        j.namespace_by_remote(&scope(), "source-id")
            .unwrap()
            .unwrap()
            .id,
        src.id
    );
    assert!(!j.replacement(replace.id).unwrap().remote_applied);
}

#[test]
fn local_swap_and_acknowledged_binding_transfer_each_roll_back_as_a_unit() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("journal");
    let mut j = open(&path);
    let old = victim(&mut j);
    let new = source(&mut j, "temporary");
    let src = object(&j, &new);
    let dst = object(&j, &old);
    let db = rusqlite::Connection::open(path.join("uploads.db")).unwrap();
    db.execute_batch("CREATE TRIGGER deny_replacement BEFORE INSERT ON file_replacements BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(
        j.replace_namespace_file(src.id, src.revision, dst.id, dst.revision, false)
            .is_err()
    );
    assert_eq!(j.namespace_object(src.id).unwrap().node.name, "temporary");
    assert!(!j.working_file(old.id).unwrap().unlinked);
    assert!(j.list_mutations(0, 100).unwrap().is_empty());
    assert_eq!(j.list(0, 100).unwrap().len(), 1);
    assert_eq!(
        db.query_row("SELECT count(*) FROM write_successors", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    db.execute_batch("DROP TRIGGER deny_replacement").unwrap();
    let replace = j
        .replace_namespace_file(src.id, src.revision, dst.id, dst.revision, false)
        .unwrap();
    ack_source(&mut j, &new, "source-id");
    let upload = j.claim_next().unwrap().unwrap();
    db.execute_batch("CREATE TRIGGER deny_transfer BEFORE UPDATE ON file_replacements WHEN json_extract(NEW.body,'$.remote_applied')=1 BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    let receipt = node("target-id", "document", "replacement-etag", 3);
    assert!(
        j.acknowledge(upload.id, upload.attempt.unwrap(), receipt.clone())
            .is_err()
    );
    assert_eq!(j.get(upload.id).unwrap().state, UploadState::Uploading);
    assert_eq!(
        j.namespace_by_remote(&scope(), "target-id")
            .unwrap()
            .unwrap()
            .id,
        dst.id
    );
    assert_eq!(
        j.namespace_by_remote(&scope(), "source-id")
            .unwrap()
            .unwrap()
            .id,
        src.id
    );
    assert!(
        !j.namespace_object(replace.cleanup_object)
            .unwrap()
            .remote_owned
    );
    assert!(j.claim_mutation().unwrap().is_none());
    db.execute_batch("DROP TRIGGER deny_transfer").unwrap();
    drop(db);
    drop(j);
    let mut j = open(&path);
    let verify = j.claim_next_verification().unwrap().unwrap();
    j.acknowledge(verify.id, verify.attempt.unwrap(), receipt)
        .unwrap();
    assert_eq!(
        j.namespace_by_remote(&scope(), "target-id")
            .unwrap()
            .unwrap()
            .id,
        src.id
    );
    assert_eq!(j.claim_mutation().unwrap().unwrap().id, replace.cleanup);
}

#[test]
fn a_later_local_rename_is_not_rolled_back_by_the_replacement_receipt() {
    let tmp = tempfile::tempdir().unwrap();
    let mut j = open(&tmp.path().join("journal"));
    let old = victim(&mut j);
    let new = source(&mut j, "temporary");
    let src = object(&j, &new);
    let dst = object(&j, &old);
    let replace = j
        .replace_namespace_file(src.id, src.revision, dst.id, dst.revision, false)
        .unwrap();
    let rename = j
        .relocate_working(new.id, "other-parent".into(), "moved".into())
        .unwrap();
    ack_source(&mut j, &new, "source-id");
    let upload = j.claim_next().unwrap().unwrap();
    j.acknowledge(
        upload.id,
        upload.attempt.unwrap(),
        node("target-id", "document", "replacement-etag", 3),
    )
    .unwrap();
    assert_eq!(j.namespace_object(src.id).unwrap().node.name, "moved");
    assert_eq!(j.namespace_object(src.id).unwrap().latest, Some(rename.id));
    let cleanup = j.claim_mutation().unwrap().unwrap();
    assert_eq!(cleanup.id, replace.cleanup);
    j.acknowledge_mutation(
        cleanup.id,
        cleanup.attempt.unwrap(),
        MutationReceipt::Removed {
            item: "source-id".into(),
        },
    )
    .unwrap();
    let next = j.claim_mutation().unwrap().unwrap();
    assert_eq!(next.id, rename.id);
    assert_eq!(next.request.intent.before().unwrap().id, "target-id");
    assert_eq!(
        next.request.intent.before().unwrap().etag.as_deref(),
        Some("replacement-etag")
    );
}

#[test]
fn old_reader_barrier_survives_intents_and_only_the_new_journal_owner_releases_it() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("journal");
    let mut j = open(&path);
    let dst = j
        .observe_namespace_file(
            scope(),
            node(
                "target-id",
                "document",
                "target-etag",
                500 * 1024 * 1024 * 1024,
            ),
        )
        .unwrap();
    let new = source(&mut j, "temporary");
    let src = object(&j, &new);
    let r = j
        .replace_namespace_file(src.id, src.revision, dst.id, dst.revision, true)
        .unwrap();
    assert!(j.namespace_object(dst.id).unwrap().working_file.is_none());
    assert_eq!(j.retained_bytes().unwrap(), 9);
    ack_source(&mut j, &new, "source-id");
    assert!(j.claim_next().unwrap().is_none());
    assert!(matches!(
        UploadJournal::open(&path, &scope().account, 1024 * 1024),
        Err(JournalError::Busy)
    ));
    assert!(!j.replacement(r.id).unwrap().local_ready);
    drop(j);
    let mut j = open(&path);
    assert!(j.replacement(r.id).unwrap().local_ready);
    assert!(!j.replacement(r.id).unwrap().remote_applied);
    assert_eq!(j.claim_next().unwrap().unwrap().id, r.id);
    assert!(j.claim_mutation().unwrap().is_none());
}

#[test]
fn stale_or_dirty_pair_is_refused_and_schema_ten_migration_retains_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("journal");
    let mut j = open(&path);
    let old = victim(&mut j);
    let new = source(&mut j, "temporary");
    let src = object(&j, &new);
    let dst = object(&j, &old);
    assert!(matches!(
        j.replace_namespace_file(src.id, src.revision + 1, dst.id, dst.revision, false),
        Err(JournalError::Stale)
    ));
    j.write_working(old.id, 0, b"dirty").unwrap();
    let fresh = object(&j, &old);
    assert!(matches!(
        j.replace_namespace_file(src.id, src.revision, fresh.id, fresh.revision, false),
        Err(JournalError::Stale)
    ));
    assert_eq!(j.list(0, 100).unwrap().len(), 1);
    drop(j);
    let db = rusqlite::Connection::open(path.join("uploads.db")).unwrap();
    db.execute_batch("DROP TABLE file_replacements; DROP INDEX namespace_remote_objects; UPDATE namespace_objects SET body=json_remove(body,'$.remote_owned'); PRAGMA user_version=10").unwrap();
    drop(db);
    let j = open(&path);
    assert_eq!(j.read_working(old.id, 0, 100).unwrap(), b"dirty");
    assert!(j.namespace_object(dst.id).unwrap().remote_owned);
    assert_eq!(
        j.namespace_by_remote(&scope(), "target-id")
            .unwrap()
            .unwrap()
            .id,
        dst.id
    );
    let db = rusqlite::Connection::open(path.join("uploads.db")).unwrap();
    let plan: String = db
        .query_row(
            "EXPLAIN QUERY PLAN DELETE FROM namespace_remote WHERE object=?1",
            [dst.id.to_string()],
            |r| r.get(3),
        )
        .unwrap();
    assert!(plan.contains("namespace_remote_objects"));
    // One object cannot accumulate a second active provider binding.
    assert!(
        db.execute(
            "INSERT INTO namespace_remote VALUES(?1,?2)",
            rusqlite::params![
                serde_json::to_string(&(&scope(), "foreign-id")).unwrap(),
                dst.id.to_string()
            ]
        )
        .is_err()
    );
    drop(j);

    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        13
    );
}

#[test]
fn consecutive_pending_replacements_transfer_the_same_destination_without_retargeting_old_streams()
{
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("journal");
    let mut j = open(&path);
    let old = victim(&mut j);
    let first = source(&mut j, "first-temp");
    let second = source(&mut j, "second-temp");
    let old_object = object(&j, &old);
    let first_object = object(&j, &first);
    let second_object = object(&j, &second);
    let one = j
        .replace_namespace_file(
            first_object.id,
            first_object.revision,
            old_object.id,
            old_object.revision,
            false,
        )
        .unwrap();
    let first_now = j.namespace_object(first_object.id).unwrap();
    let two = j
        .replace_namespace_file(
            second_object.id,
            second_object.revision,
            first_now.id,
            first_now.revision,
            false,
        )
        .unwrap();
    j.write_working(first.id, 0, b"retained intermediate")
        .unwrap();
    assert!(j.seal_working(first.id).unwrap().is_none());
    ack_source(&mut j, &first, "first-id");
    ack_source(&mut j, &second, "second-id");
    let upload = j.claim_next().unwrap().unwrap();
    assert_eq!(upload.id, one.id);
    j.acknowledge(
        upload.id,
        upload.attempt.unwrap(),
        node("target-id", "document", "one-etag", 3),
    )
    .unwrap();
    assert_eq!(
        j.namespace_by_remote(&scope(), "target-id")
            .unwrap()
            .unwrap()
            .id,
        first_object.id
    );
    let upload = j.claim_next().unwrap().unwrap();
    assert_eq!(upload.id, two.id);
    assert_eq!(
        upload.intent,
        UploadIntent::Replace {
            item: "target-id".into(),
            expected_etag: "one-etag".into()
        }
    );
    j.acknowledge(
        upload.id,
        upload.attempt.unwrap(),
        node("target-id", "document", "two-etag", 3),
    )
    .unwrap();
    assert_eq!(
        j.namespace_by_remote(&scope(), "target-id")
            .unwrap()
            .unwrap()
            .id,
        second_object.id
    );
    assert!(!j.namespace_object(first_object.id).unwrap().remote_owned);
    assert!(!j.namespace_object(old_object.id).unwrap().remote_owned);
    for (expected, id) in [(one.cleanup, "first-id"), (two.cleanup, "second-id")] {
        let cleanup = j.claim_mutation().unwrap().unwrap();
        assert_eq!(cleanup.id, expected);
        assert_eq!(cleanup.request.intent.before().unwrap().id, id);
        j.acknowledge_mutation(
            cleanup.id,
            cleanup.attempt.unwrap(),
            MutationReceipt::Removed { item: id.into() },
        )
        .unwrap();
    }
    assert_eq!(
        names(&j, vec![node("target-id", "document", "two-etag", 3)]),
        vec!["document"]
    );
    drop(j);
    let j = open(&path);
    assert_eq!(j.read_working(old.id, 0, 100).unwrap(), b"old");
    assert_eq!(
        j.read_working(first.id, 0, 100).unwrap(),
        b"retained intermediate"
    );
    assert_eq!(j.read_working(second.id, 0, 100).unwrap(), b"new");
}

#[test]
fn insufficient_snapshot_space_keeps_both_names_and_original_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("journal");
    let mut j = open(&path);
    let old = victim(&mut j);
    let new = source(&mut j, "temporary");
    let src = object(&j, &new);
    let dst = object(&j, &old);
    assert_eq!(j.retained_bytes().unwrap(), 9);
    drop(j);
    let mut j = UploadJournal::open(&path, &scope().account, 10).unwrap();
    assert!(matches!(
        j.replace_namespace_file(src.id, src.revision, dst.id, dst.revision, false),
        Err(JournalError::Quota)
    ));
    assert_eq!(j.namespace_object(src.id).unwrap().node.name, "temporary");
    assert!(!j.namespace_object(dst.id).unwrap().unlinked);
    assert_eq!(j.read_working(old.id, 0, 100).unwrap(), b"old");
    assert_eq!(j.read_working(new.id, 0, 100).unwrap(), b"new");
    assert_eq!(j.retained_bytes().unwrap(), 9);
    assert_eq!(j.namespace_objects().unwrap().len(), 2);
    assert_eq!(j.list(0, 100).unwrap().len(), 1);
}

#[test]
fn already_cached_source_and_target_need_no_synthetic_predecessor_operation() {
    let tmp = tempfile::tempdir().unwrap();
    let mut j = open(&tmp.path().join("journal"));
    let old = victim(&mut j);
    let new = j
        .create_working(
            scope(),
            node("source-id", "source", "source-original", 3),
            false,
            b"new".as_slice(),
        )
        .unwrap();
    let src = object(&j, &new);
    let dst = object(&j, &old);
    let r = j
        .replace_namespace_file(src.id, src.revision, dst.id, dst.revision, false)
        .unwrap();
    assert_eq!(j.list(0, 100).unwrap().len(), 1);
    assert!(j.get(r.id).unwrap().base.is_none());
    let upload = j.claim_next().unwrap().unwrap();
    j.acknowledge(
        upload.id,
        upload.attempt.unwrap(),
        node("target-id", "document", "published", 3),
    )
    .unwrap();
    assert_eq!(
        j.namespace_by_local(&scope(), "source-id")
            .unwrap()
            .unwrap()
            .id,
        src.id
    );
    assert_eq!(
        j.namespace_by_remote(&scope(), "source-id")
            .unwrap()
            .unwrap()
            .id,
        r.cleanup_object
    );
    assert_eq!(
        j.namespace_by_local(&scope(), "target-id")
            .unwrap()
            .unwrap()
            .id,
        dst.id
    );
    assert_eq!(
        j.namespace_by_remote(&scope(), "target-id")
            .unwrap()
            .unwrap()
            .id,
        src.id
    );
    let cleanup = j.claim_mutation().unwrap().unwrap();
    assert!(cleanup.base.is_none());
    assert_eq!(
        cleanup.request.intent.before().unwrap().etag.as_deref(),
        Some("source-original")
    );
    assert_eq!(j.read_working(old.id, 0, 100).unwrap(), b"old");
    assert_eq!(j.read_working(new.id, 0, 100).unwrap(), b"new");
}
