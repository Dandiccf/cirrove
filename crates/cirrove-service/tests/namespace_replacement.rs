//! Real journal transactions for local replacement and paired cloud bindings.
//! Generated local bytes only; actual FUSE replacement is validated separately.
#![allow(clippy::unwrap_used)]
use cirrove_core::{Node, NodeKind, Scope, mutation::MutationReceipt};
use cirrove_service::journal::{
    JournalError, NamespaceObject, UploadIntent, UploadJournal, UploadState, WorkingFile,
};
use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
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
        package: false,
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
    source_bytes(j, name, b"new")
}
fn source_bytes(j: &mut UploadJournal, name: &str, bytes: &[u8]) -> WorkingFile {
    let file = j
        .create_working(scope(), node("", name, "unused", 0), true, b"".as_slice())
        .unwrap();
    j.write_working(file.id, 0, bytes).unwrap();
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
    let receipt = node("target-id", "provider-raw-name", "replacement-etag", 3);
    j.acknowledge(upload.id, upload.attempt.unwrap(), receipt.clone())
        .unwrap();
    assert!(j.replacement(replacement.id).unwrap().remote_applied);
    assert_eq!(
        j.namespace_by_remote(&scope(), "target-id")
            .unwrap()
            .unwrap()
            .remote
            .unwrap()
            .name,
        "document"
    );
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
        21
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

// Publish the signal only after its complete UUID has been written and closed.
// The callback exposes the post-create/pre-write boundary deterministically.
fn publish_replacement_ready(
    ready: &Path,
    victim: uuid::Uuid,
    after_create: impl FnOnce(),
) -> std::io::Result<()> {
    let staging = ready.with_extension("tmp");
    let mut file = std::fs::File::create(&staging)?;
    after_create();
    file.write_all(victim.to_string().as_bytes())?;
    drop(file);
    std::fs::rename(staging, ready)
}

#[test]
fn replacement_readiness_is_visible_only_after_complete_uuid_publication() {
    let temp = tempfile::tempdir().unwrap();
    let ready = temp.path().join("ready");
    let victim = uuid::Uuid::new_v4();
    let mut observed_before_write = None;
    publish_replacement_ready(&ready, victim, || {
        observed_before_write = std::fs::read(&ready).ok();
    })
    .unwrap();
    let published: uuid::Uuid = std::fs::read_to_string(&ready).unwrap().parse().unwrap();
    assert_eq!(
        published, victim,
        "completed publisher must preserve the exact UUID"
    );
    assert_eq!(
        observed_before_write, None,
        "final readiness must stay absent at the post-create/pre-write boundary"
    );
}

/// Child for the crash test: performs a local replacement, stops at the requested
/// durable transition, then waits to be killed.
#[test]
#[ignore = "subprocess fixture; activated only by its parent test"]
fn replacement_crash_child() {
    let root = std::env::var("CIRROVE_REPLACEMENT_FIXTURE_ROOT").unwrap();
    let root = Path::new(&root);
    let mut j = open(&root.join("journal"));
    let old = victim(&mut j);
    let new = source(&mut j, "temporary");
    let src = object(&j, &new);
    let dst = object(&j, &old);
    let replacement = j
        .replace_namespace_file(src.id, src.revision, dst.id, dst.revision, true)
        .unwrap();
    if std::env::var("CIRROVE_REPLACEMENT_FIXTURE_PHASE").unwrap() == "released" {
        j.release_replacement_readers(replacement.id).unwrap();
    }
    std::fs::write(
        root.join("reached"),
        cirrove_service::journal::durable::reached().join("\n"),
    )
    .unwrap();
    publish_replacement_ready(&root.join("ready"), old.id, || {}).unwrap();
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// A killed process must not resurrect a replaced name or drop a reader still
/// holding the old bytes.
///
/// Replacement swaps a name between two objects and keeps the victim's stream
/// alive for descriptors that were already open. Both halves are durable state,
/// and the existing tests reach them by injected failure and clean restart, which
/// unwind or resume state the program still owns. This leaves whatever the kernel
/// had actually written.
///
/// The reader gate is the sharp edge: released too eagerly after a crash, a
/// descriptor that was reading the old document loses its bytes; held forever, the
/// bytes never retire. The kill has to leave the gate exactly where the last
/// durable write put it.
#[test]
fn actual_process_death_preserves_the_swapped_name_and_the_reader_gate() {
    for phase in ["replaced", "released"] {
        let temp = tempfile::tempdir().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "replacement_crash_child", "--ignored"])
            .env("CIRROVE_REPLACEMENT_FIXTURE_ROOT", temp.path())
            .env("CIRROVE_REPLACEMENT_FIXTURE_PHASE", phase)
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !temp.path().join("ready").exists() {
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("replacement fixture did not become ready");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        child.kill().unwrap();
        child.wait().unwrap();

        let victim_id: uuid::Uuid = std::fs::read_to_string(temp.path().join("ready"))
            .unwrap()
            .parse()
            .unwrap();
        let j = open(&temp.path().join("journal"));
        // The swap committed before the kill: the name belongs to the source and
        // the victim is unlinked. A crash must not restore the old name.
        assert_eq!(
            names(&j, vec![node("target-id", "document", "target-etag", 3)]),
            vec!["document"]
        );
        let victim = j.working_file(victim_id).unwrap();
        assert!(victim.unlinked, "the replaced file must stay unlinked");
        // And the old bytes are still readable, because a descriptor may hold them.
        assert_eq!(j.read_working(victim_id, 0, 100).unwrap(), b"old");
    }
}

#[test]
fn consecutive_atomic_replacements_support_two_id_handoffs_and_cleanup_after_restart() {
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
    let first_recovery = j
        .reserve_identity_handoff(
            upload.id,
            upload.attempt.unwrap(),
            cirrove_core::upload::RecoveryLocation::Trash {
                local_name: "recovery-one".into(),
                parent: "trash".into(),
            },
        )
        .unwrap();
    let stale_attempt = upload.attempt.unwrap();
    drop(j);
    j = open(&path);
    let upload = j.claim_verification(one.id).unwrap();
    assert_eq!(
        j.reserve_identity_handoff(
            upload.id,
            upload.attempt.unwrap(),
            cirrove_core::upload::RecoveryLocation::Trash {
                local_name: "recovery-one".into(),
                parent: "trash".into()
            }
        )
        .unwrap(),
        first_recovery
    );
    let mut backup = node("target-id", "document", "trash-old", 3);
    backup.parent_id = Some("trash".into());
    assert!(
        j.acknowledge_identity_handoff(
            upload.id,
            stale_attempt,
            node("new-one", "document", "one-etag", 3),
            backup.clone()
        )
        .is_err()
    );
    // A replacement may not seize the still-owned temporary identity. Failure
    // must roll back the victim, cleanup, recovery and upload receipt together.
    assert!(
        j.acknowledge_identity_handoff(
            upload.id,
            upload.attempt.unwrap(),
            node("first-id", "document", "one-etag", 3),
            backup.clone()
        )
        .is_err()
    );
    assert_eq!(
        j.namespace_by_remote(&scope(), "target-id")
            .unwrap()
            .unwrap()
            .id,
        old_object.id
    );
    assert_eq!(
        j.namespace_by_remote(&scope(), "first-id")
            .unwrap()
            .unwrap()
            .id,
        first_object.id
    );
    assert!(!j.namespace_object(first_recovery).unwrap().remote_owned);
    assert_eq!(j.get(upload.id).unwrap().state, UploadState::Verifying);

    j.acknowledge_identity_handoff(
        upload.id,
        upload.attempt.unwrap(),
        node("new-one", "document", "one-etag", 3),
        backup,
    )
    .unwrap();
    assert_eq!(
        j.namespace_by_remote(&scope(), "new-one")
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
            item: "new-one".into(),
            expected_etag: "one-etag".into()
        }
    );
    let second_recovery = j
        .reserve_identity_handoff(
            upload.id,
            upload.attempt.unwrap(),
            cirrove_core::upload::RecoveryLocation::Trash {
                local_name: "recovery-two".into(),
                parent: "trash".into(),
            },
        )
        .unwrap();
    let mut backup = node("new-one", "document", "trash-one", 3);
    backup.parent_id = Some("trash".into());
    j.acknowledge_identity_handoff(
        upload.id,
        upload.attempt.unwrap(),
        node("new-two", "document", "two-etag", 3),
        backup,
    )
    .unwrap();
    assert_eq!(
        j.namespace_by_remote(&scope(), "new-two")
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
        names(&j, vec![node("new-two", "document", "two-etag", 3)]),
        vec!["document"]
    );
    drop(j);
    let j = open(&path);
    for (id, recovery) in [("target-id", first_recovery), ("new-one", second_recovery)] {
        let saved = j.namespace_by_remote(&scope(), id).unwrap().unwrap();
        assert_eq!(saved.id, recovery);
        assert!(saved.unlinked && saved.remote_owned);
        assert_eq!(saved.remote.unwrap().parent_id.as_deref(), Some("trash"));
    }
    assert_eq!(j.read_working(old.id, 0, 100).unwrap(), b"old");
    assert_eq!(
        j.read_working(first.id, 0, 100).unwrap(),
        b"retained intermediate"
    );
    assert_eq!(j.read_working(second.id, 0, 100).unwrap(), b"new");
}

#[test]
fn keep_both_of_atomic_conflict_restores_victim_and_defers_source_cleanup() {
    for pending_source in [false, true] {
        rescue_atomic_conflict(pending_source);
    }
}

fn rescue_atomic_conflict(pending_source: bool) {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("journal");
    let mut j = open(&path);
    let old = victim(&mut j);
    let new = source(&mut j, "temporary");
    if !pending_source {
        ack_source(&mut j, &new, "source-id");
    }
    let src = object(&j, &new);
    let dst = object(&j, &old);
    let replacement = j
        .replace_namespace_file(src.id, src.revision, dst.id, dst.revision, false)
        .unwrap();
    if pending_source {
        ack_source(&mut j, &new, "source-id");
    }
    let active = j.claim_next().unwrap().unwrap();
    assert_eq!(active.id, replacement.id);
    j.stop_attempt(active.id, active.attempt.unwrap(), UploadState::Conflict)
        .unwrap();
    let copy = j
        .keep_both(active.id, "root".into(), "rescued".into())
        .unwrap();
    assert_eq!(j.read_working(old.id, 0, 100).unwrap(), b"old");
    assert_eq!(j.read_working(new.id, 0, 100).unwrap(), b"new");
    let cloud = node("target-id", "document", "competing", 6);
    let mut visible = names(
        &j,
        vec![
            cloud.clone(),
            node("source-id", "temporary", "source-etag", 3),
        ],
    );
    visible.sort();
    assert_eq!(visible, vec!["document", "rescued"]);
    let original = j
        .namespace_by_remote(&scope(), "target-id")
        .unwrap()
        .unwrap();
    assert_ne!(original.id, src.id);
    assert_ne!(original.id, dst.id);
    assert!(original.follows_remote);
    assert!(!j.namespace_object(dst.id).unwrap().remote_owned);
    let updated = j.replacement(replacement.id).unwrap();
    assert_ne!(updated.cleanup, replacement.cleanup);
    assert!(!updated.remote_applied);
    assert_eq!(
        serde_json::to_value(j.mutation(replacement.cleanup).unwrap().state).unwrap(),
        "resolved"
    );
    assert!(j.mutation(replacement.cleanup).unwrap().receipt.is_none());
    assert_eq!(
        j.operation_prerequisites(updated.cleanup).unwrap(),
        vec![copy.id]
    );
    assert!(j.claim_mutation().unwrap().is_none());
    drop(j);
    let mut j = open(&path);
    assert!(j.claim_mutation().unwrap().is_none());
    let upload = j.claim_next().unwrap().unwrap();
    assert_eq!(upload.id, copy.id);
    j.acknowledge(
        upload.id,
        upload.attempt.unwrap(),
        node("rescue-id", "rescued", "rescued-etag", 3),
    )
    .unwrap();
    let cleanup = j.claim_mutation().unwrap().unwrap();
    assert_eq!(cleanup.id, updated.cleanup);
    assert_eq!(cleanup.request.intent.before().unwrap().id, "source-id");
    assert_eq!(
        cleanup.request.intent.before().unwrap().etag.as_deref(),
        Some("source-etag")
    );
    j.acknowledge_mutation(
        cleanup.id,
        cleanup.attempt.unwrap(),
        MutationReceipt::Removed {
            item: "source-id".into(),
        },
    )
    .unwrap();
    assert!(j.claim_next().unwrap().is_none());
    assert!(j.claim_mutation().unwrap().is_none());
    assert_eq!(j.get(replacement.id).unwrap().state, UploadState::Resolved);
    drop(j);
    let mut j = open(&path);
    assert!(j.claim_mutation().unwrap().is_none());
    assert_eq!(j.read_working(old.id, 0, 100).unwrap(), b"old");
    assert_eq!(j.read_working(new.id, 0, 100).unwrap(), b"new");
    assert_eq!(
        j.namespace_by_remote(&scope(), "target-id")
            .unwrap()
            .unwrap()
            .id,
        original.id
    );
}

#[test]
fn atomic_rescue_rolls_back_binding_and_cleanup_changes_on_transaction_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("journal");
    let mut j = open(&path);
    let old = victim(&mut j);
    let new = source(&mut j, "temporary");
    ack_source(&mut j, &new, "source-id");
    let src = object(&j, &new);
    let dst = object(&j, &old);
    let replacement = j
        .replace_namespace_file(src.id, src.revision, dst.id, dst.revision, false)
        .unwrap();
    let active = j.claim_next().unwrap().unwrap();
    j.stop_attempt(active.id, active.attempt.unwrap(), UploadState::Conflict)
        .unwrap();
    let db = rusqlite::Connection::open(path.join("uploads.db")).unwrap();
    // Fail after the cleanup was superseded and a replacement queued, so all
    // preceding ownership changes must roll back with the rescue itself.
    db.execute_batch("CREATE TRIGGER refuse_rescue BEFORE UPDATE ON file_replacements WHEN json_extract(NEW.body,'$.rescued_as') IS NOT NULL BEGIN SELECT RAISE(ABORT,'synthetic rescue failure'); END;").unwrap();
    assert!(matches!(
        j.keep_both(active.id, "root".into(), "rescued".into()),
        Err(JournalError::Storage)
    ));
    assert_eq!(j.list(0, 100).unwrap().len(), 2);
    assert_eq!(j.get(active.id).unwrap().state, UploadState::Conflict);
    assert_eq!(
        j.mutation(replacement.cleanup).unwrap().state,
        cirrove_service::journal::MutationState::Pending
    );
    assert_eq!(
        j.replacement(active.id).unwrap().cleanup,
        replacement.cleanup
    );
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
    assert_eq!(j.working_file(new.id).unwrap().node.name, "document");
    assert_eq!(j.read_working(old.id, 0, 100).unwrap(), b"old");
    assert!(j.claim_mutation().unwrap().is_none());
    db.execute_batch("DROP TRIGGER refuse_rescue").unwrap();
    j.keep_both(active.id, "root".into(), "rescued".into())
        .unwrap();
}

#[test]
fn completed_atomic_replacement_does_not_prevent_rescue_of_a_later_ordinary_save() {
    let tmp = tempfile::tempdir().unwrap();
    let mut j = open(&tmp.path().join("journal"));
    let old = victim(&mut j);
    let new = source(&mut j, "temporary");
    ack_source(&mut j, &new, "source-id");
    let src = object(&j, &new);
    let dst = object(&j, &old);
    j.replace_namespace_file(src.id, src.revision, dst.id, dst.revision, false)
        .unwrap();
    let active = j.claim_next().unwrap().unwrap();
    j.acknowledge(
        active.id,
        active.attempt.unwrap(),
        node("target-id", "document", "replaced", 3),
    )
    .unwrap();
    let cleanup = j.claim_mutation().unwrap().unwrap();
    j.acknowledge_mutation(
        cleanup.id,
        cleanup.attempt.unwrap(),
        MutationReceipt::Removed {
            item: "source-id".into(),
        },
    )
    .unwrap();
    j.write_working(new.id, 0, b"latest").unwrap();
    j.seal_working(new.id).unwrap();
    let active = j.claim_next().unwrap().unwrap();
    j.stop_attempt(active.id, active.attempt.unwrap(), UploadState::Conflict)
        .unwrap();
    let rescue = j
        .keep_both(active.id, "root".into(), "rescued".into())
        .unwrap();
    assert_eq!(rescue.size, 6);
    assert_eq!(j.read_working(new.id, 0, 100).unwrap(), b"latest");
    assert_eq!(j.read_working(old.id, 0, 100).unwrap(), b"old");
}

#[test]
fn atomic_rescue_refuses_uncertain_cleanup_before_sealing_newer_bytes() {
    for state in ["verify_required", "pending"] {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("journal");
        let mut j = open(&path);
        let old = victim(&mut j);
        let new = source(&mut j, "temporary");
        ack_source(&mut j, &new, "source-id");
        let src = object(&j, &new);
        let dst = object(&j, &old);
        let r = j
            .replace_namespace_file(src.id, src.revision, dst.id, dst.revision, false)
            .unwrap();
        let active = j.claim_next().unwrap().unwrap();
        j.stop_attempt(active.id, active.attempt.unwrap(), UploadState::Conflict)
            .unwrap();
        j.write_working(new.id, 0, b"newest").unwrap();
        // A formerly attempted cleanup must never be relabelled as unsent,
        // including a retry that has returned to Pending.
        let db = rusqlite::Connection::open(path.join("uploads.db")).unwrap();
        db.execute("UPDATE mutations SET state=?2,body=json_set(body,'$.state',?2,'$.failed_attempts',1) WHERE id=?1", rusqlite::params![r.cleanup.to_string(), state]).unwrap();
        assert!(matches!(
            j.keep_both(active.id, "root".into(), "rescued".into()),
            Err(JournalError::Stale)
        ));
        assert_eq!(j.list(0, 100).unwrap().len(), 2);
        assert!(j.working_file(new.id).unwrap().dirty);
        assert_eq!(j.read_working(new.id, 0, 100).unwrap(), b"newest");
        assert_eq!(j.get(active.id).unwrap().state, UploadState::Conflict);
        assert_eq!(
            j.namespace_by_remote(&scope(), "target-id")
                .unwrap()
                .unwrap()
                .id,
            dst.id
        );
    }
}

#[test]
fn atomic_rescue_does_not_release_an_external_dependent_of_its_cleanup() {
    let tmp = tempfile::tempdir().unwrap();
    let mut j = open(&tmp.path().join("journal"));
    let old = victim(&mut j);
    let new = source(&mut j, "temporary");
    ack_source(&mut j, &new, "source-id");
    let src = object(&j, &new);
    let dst = object(&j, &old);
    let r = j
        .replace_namespace_file(src.id, src.revision, dst.id, dst.revision, false)
        .unwrap();
    let active = j.claim_next().unwrap().unwrap();
    j.stop_attempt(active.id, active.attempt.unwrap(), UploadState::Conflict)
        .unwrap();
    let other = j
        .enqueue(
            scope(),
            UploadIntent::Create {
                parent: "root".into(),
                name: "other".into(),
            },
            b"other".as_slice(),
        )
        .unwrap();
    let dependent = j
        .enqueue_after_all(other.id, &[r.cleanup], b"dependent".as_slice())
        .unwrap();
    j.write_working(new.id, 0, b"newest").unwrap();
    assert!(matches!(
        j.keep_both(active.id, "root".into(), "rescued".into()),
        Err(JournalError::Stale)
    ));
    assert_eq!(j.list(0, 100).unwrap().len(), 4);
    assert_eq!(j.get(dependent.id).unwrap().state, UploadState::Pending);
    assert_eq!(
        j.mutation(r.cleanup).unwrap().state,
        cirrove_service::journal::MutationState::Pending
    );
    assert!(j.working_file(new.id).unwrap().dirty);
    assert_eq!(j.get(active.id).unwrap().state, UploadState::Conflict);
}

#[test]
fn chained_atomic_conflict_rescues_latest_and_defers_each_temporary_cleanup() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("journal");
    let mut j = open(&path);
    let old = victim(&mut j);
    let first = source(&mut j, "temporary-1");
    ack_source(&mut j, &first, "source-1");
    let second = source_bytes(&mut j, "temporary-2", b"two");
    ack_source(&mut j, &second, "source-2");
    let a = object(&j, &first);
    let b = object(&j, &old);
    let r1 = j
        .replace_namespace_file(a.id, a.revision, b.id, b.revision, false)
        .unwrap();
    let active = j.claim_next().unwrap().unwrap();
    assert_eq!(active.id, r1.id);
    j.stop_attempt(active.id, active.attempt.unwrap(), UploadState::Conflict)
        .unwrap();
    let a = object(&j, &second);
    let b = object(&j, &first);
    let r2 = j
        .replace_namespace_file(a.id, a.revision, b.id, b.revision, false)
        .unwrap();
    let copy = j.keep_both(r1.id, "root".into(), "rescued".into()).unwrap();
    assert_eq!(j.get(r1.id).unwrap().state, UploadState::Resolved);
    assert_eq!(j.get(r2.id).unwrap().state, UploadState::Resolved);
    assert_eq!(j.read_working(old.id, 0, 100).unwrap(), b"old");
    assert_eq!(j.read_working(first.id, 0, 100).unwrap(), b"new");
    assert_eq!(j.read_working(second.id, 0, 100).unwrap(), b"two");
    let mut rescued = Vec::new();
    std::io::Read::read_to_end(&mut j.payload(copy.id).unwrap(), &mut rescued).unwrap();
    assert_eq!(rescued, b"two");
    for r in [&r1, &r2] {
        assert_eq!(
            j.mutation(r.cleanup).unwrap().state,
            cirrove_service::journal::MutationState::Resolved
        );
        let updated = j.replacement(r.id).unwrap();
        assert_eq!(updated.rescued_as, Some(copy.id));
        assert_eq!(
            j.operation_prerequisites(updated.cleanup).unwrap(),
            vec![copy.id]
        );
    }
    assert!(j.claim_mutation().unwrap().is_none());
    drop(j);
    let mut j = open(&path);
    let active = j.claim_next().unwrap().unwrap();
    assert_eq!(active.id, copy.id);
    j.acknowledge(
        active.id,
        active.attempt.unwrap(),
        node("rescue-id", "rescued", "rescue-etag", 3),
    )
    .unwrap();
    let mut removed = Vec::new();
    for _ in 0..2 {
        let active = j.claim_mutation().unwrap().unwrap();
        let id = active.request.intent.before().unwrap().id.clone();
        removed.push(id.clone());
        j.acknowledge_mutation(
            active.id,
            active.attempt.unwrap(),
            MutationReceipt::Removed { item: id },
        )
        .unwrap();
    }
    removed.sort();
    assert_eq!(removed, vec!["source-1", "source-2"]);
    assert!(j.claim_mutation().unwrap().is_none());
    assert!(j.claim_next().unwrap().is_none());
    let mut visible = names(
        &j,
        vec![
            node("target-id", "document", "competing", 6),
            node("rescue-id", "rescued", "rescue-etag", 3),
        ],
    );
    visible.sort();
    assert_eq!(visible, vec!["document", "rescued"]);
}

fn atomic_chain_fixture(
    path: &Path,
) -> (
    UploadJournal,
    WorkingFile,
    Vec<WorkingFile>,
    Vec<cirrove_service::journal::ReplacementRecord>,
) {
    let mut j = open(path);
    let old = victim(&mut j);
    let mut files = Vec::new();
    for index in 0..3 {
        let file = source_bytes(&mut j, &format!("temp-{index}"), &[b'a' + index; 3]);
        ack_source(&mut j, &file, &format!("temporary-{index}"));
        files.push(file);
    }
    let mut records = Vec::new();
    for index in 0..3 {
        let src = object(&j, &files[index]);
        let dst = object(&j, if index == 0 { &old } else { &files[index - 1] });
        let record = j
            .replace_namespace_file(src.id, src.revision, dst.id, dst.revision, false)
            .unwrap();
        if index == 0 {
            let active = j.claim_next().unwrap().unwrap();
            assert_eq!(active.id, record.id);
            j.stop_attempt(active.id, active.attempt.unwrap(), UploadState::Conflict)
                .unwrap();
        }
        records.push(record);
    }
    (j, old, files, records)
}

#[test]
fn chained_atomic_rescue_seals_newest_dirty_bytes_and_retains_older_streams() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("journal");
    let (mut j, old, files, records) = atomic_chain_fixture(&path);
    j.write_working(files[2].id, 0, b"last").unwrap();
    let copy = j
        .keep_both(records[0].id, "root".into(), "rescued".into())
        .unwrap();
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut j.payload(copy.id).unwrap(), &mut bytes).unwrap();
    assert_eq!(bytes, b"last");
    for record in &records {
        assert_eq!(j.get(record.id).unwrap().state, UploadState::Resolved);
        assert_eq!(j.replacement(record.id).unwrap().rescued_as, Some(copy.id));
    }
    assert!(j.claim_mutation().unwrap().is_none());
    drop(j);
    let j = open(&path);
    assert_eq!(j.read_working(old.id, 0, 100).unwrap(), b"old");
    assert_eq!(j.read_working(files[0].id, 0, 100).unwrap(), b"aaa");
    assert_eq!(j.read_working(files[1].id, 0, 100).unwrap(), b"bbb");
    assert_eq!(j.read_working(files[2].id, 0, 100).unwrap(), b"last");
}

#[test]
fn chained_atomic_rescue_rolls_back_every_owner_and_cleanup_together() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("journal");
    let (mut j, old, files, records) = atomic_chain_fixture(&path);
    let count = j.list(0, 100).unwrap().len();
    let db = rusqlite::Connection::open(path.join("uploads.db")).unwrap();
    db.execute_batch(&format!("CREATE TRIGGER refuse_chain BEFORE UPDATE ON file_replacements WHEN NEW.id='{}' AND json_extract(NEW.body,'$.rescued_as') IS NOT NULL BEGIN SELECT RAISE(ABORT,'synthetic chain failure'); END;",records[0].id)).unwrap();
    assert!(matches!(
        j.keep_both(records[0].id, "root".into(), "rescued".into()),
        Err(JournalError::Storage)
    ));
    assert_eq!(j.list(0, 100).unwrap().len(), count);
    for record in &records {
        assert!(j.replacement(record.id).unwrap().rescued_as.is_none());
        assert_eq!(
            j.mutation(record.cleanup).unwrap().state,
            cirrove_service::journal::MutationState::Pending
        );
    }
    for file in files.iter().chain(std::iter::once(&old)) {
        assert!(object(&j, file).remote_owned);
    }
    assert_eq!(j.get(records[0].id).unwrap().state, UploadState::Conflict);
    assert_eq!(names(&j, vec![]), vec!["document"]);
}

#[test]
fn chained_atomic_rescue_refuses_attempted_cleanup_without_sealing_dirty_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("journal");
    let (mut j, _, files, records) = atomic_chain_fixture(&path);
    j.write_working(files[2].id, 0, b"last").unwrap();
    let count = j.list(0, 100).unwrap().len();
    let db = rusqlite::Connection::open(path.join("uploads.db")).unwrap();
    db.execute(
        "UPDATE mutations SET body=json_set(body,'$.failed_attempts',1) WHERE id=?1",
        [records[1].cleanup.to_string()],
    )
    .unwrap();
    assert!(matches!(
        j.keep_both(records[0].id, "root".into(), "rescued".into()),
        Err(JournalError::Stale)
    ));
    assert_eq!(j.list(0, 100).unwrap().len(), count);
    assert!(j.working_file(files[2].id).unwrap().dirty);
    assert_eq!(j.read_working(files[2].id, 0, 100).unwrap(), b"last");
    for record in records {
        assert!(j.replacement(record.id).unwrap().rescued_as.is_none());
    }
}

#[test]
fn chained_atomic_rescue_refuses_external_dependents_of_any_member_or_cleanup() {
    for cleanup in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("journal");
        let (mut j, _, files, records) = atomic_chain_fixture(&path);
        let other = j
            .enqueue(
                scope(),
                UploadIntent::Create {
                    parent: "root".into(),
                    name: "other".into(),
                },
                b"other".as_slice(),
            )
            .unwrap();
        let prerequisite = if cleanup {
            records[1].cleanup
        } else {
            records[1].id
        };
        let dependent = j
            .enqueue_after_all(other.id, &[prerequisite], b"dependent".as_slice())
            .unwrap();
        j.write_working(files[2].id, 0, b"last").unwrap();
        let count = j.list(0, 100).unwrap().len();
        assert!(matches!(
            j.keep_both(records[0].id, "root".into(), "rescued".into()),
            Err(JournalError::Stale)
        ));
        assert_eq!(j.list(0, 100).unwrap().len(), count);
        assert!(j.working_file(files[2].id).unwrap().dirty);
        assert_eq!(j.get(dependent.id).unwrap().state, UploadState::Pending);
        for record in records {
            assert!(j.replacement(record.id).unwrap().rescued_as.is_none());
        }
    }
}
