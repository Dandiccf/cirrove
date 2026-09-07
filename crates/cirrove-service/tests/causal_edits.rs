//! Save/rename/save/delete lineage across restart; generated local data only.
#![allow(clippy::unwrap_used)]
use cirrove_core::{Node, NodeKind, Scope, mutation::*};
use cirrove_service::journal::{
    JournalError, MutationState, UploadIntent, UploadJournal, UploadState,
};
use std::{
    io::Read,
    path::Path,
    time::{Duration, Instant},
};

fn scope() -> Scope {
    Scope {
        account: "causal-fixture".into(),
        provider: "fixture".into(),
        collection: "drive".into(),
    }
}
fn open(path: &Path) -> UploadJournal {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match UploadJournal::open(path, &scope().account, 1024 * 1024) {
            Err(JournalError::Busy) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(2))
            }
            result => return result.unwrap(),
        }
    }
}
fn node() -> Node {
    Node {
        id: "local-file".into(),
        parent_id: Some("root".into()),
        name: "draft.txt".into(),
        kind: NodeKind::File,
        size: 5,
        modified_unix: 0,
        etag: None,
        content_version: Some("content-first".into()),
        target: None,
    }
}
fn rename(before: Node, name: &str) -> MutationRequest {
    MutationRequest {
        scope: scope(),
        intent: MutationIntent::Relocate {
            before,
            parent: "root".into(),
            name: name.into(),
        },
    }
}
fn create(j: &mut UploadJournal, name: &str) -> cirrove_service::journal::UploadRecord {
    j.enqueue(
        scope(),
        UploadIntent::Create {
            parent: "root".into(),
            name: name.into(),
        },
        b"first".as_slice(),
    )
    .unwrap()
}
fn payload(j: &UploadJournal, id: uuid::Uuid) -> Vec<u8> {
    let mut bytes = vec![];
    j.payload(id).unwrap().read_to_end(&mut bytes).unwrap();
    bytes
}
fn acknowledge_create(j: &mut UploadJournal, id: uuid::Uuid) -> Node {
    let active = j.claim_next().unwrap().unwrap();
    assert_eq!(active.id, id);
    let mut remote = node();
    remote.id = "assigned-id".into();
    remote.etag = Some("after-create".into());
    j.acknowledge(id, active.attempt.unwrap(), remote.clone())
        .unwrap();
    remote
}

#[test]
fn create_rename_save_delete_follow_actual_receipts_including_lost_rename_success() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("journal");
    let mut j = open(&root);
    let first = create(&mut j, "draft.txt");
    let moved = j
        .enqueue_mutation_after(first.id, rename(node(), "Grüße.txt"))
        .unwrap();
    let second = j.enqueue_after(moved.id, b"second".as_slice()).unwrap();
    let removed = j
        .enqueue_mutation_after(
            second.id,
            MutationRequest {
                scope: scope(),
                intent: MutationIntent::RemoveFile { before: node() },
            },
        )
        .unwrap();
    assert!(j.claim_mutation().unwrap().is_none());
    assert!(j.request_mutation_retry(moved.id).is_err());
    let created = acknowledge_create(&mut j, first.id);
    assert!(j.claim_next().unwrap().is_none());
    let active = j.claim_mutation().unwrap().unwrap();
    assert_eq!(active.id, moved.id);
    assert_eq!(active.request.intent.before(), Some(&created));
    let old_attempt = active.attempt.unwrap();
    drop(j); // Server rename completed, but its reply/journal commit was lost.
    let mut j = open(&root);
    assert!(j.claim_next().unwrap().is_none());
    let recovered = j.claim_mutation().unwrap().unwrap();
    assert_eq!(recovered.state, MutationState::Verifying);
    assert_ne!(recovered.attempt, Some(old_attempt));
    let mut renamed = created;
    renamed.name = "Grüße.txt".into();
    renamed.etag = Some("after-rename".into());
    assert!(matches!(
        j.acknowledge_mutation(
            moved.id,
            old_attempt,
            MutationReceipt::Upsert(renamed.clone())
        ),
        Err(JournalError::Stale)
    ));
    j.acknowledge_mutation(
        moved.id,
        recovered.attempt.unwrap(),
        MutationReceipt::Upsert(renamed.clone()),
    )
    .unwrap();
    let active = j.claim_next().unwrap().unwrap();
    assert_eq!(active.id, second.id);
    assert_eq!(
        active.intent,
        UploadIntent::Replace {
            item: renamed.id.clone(),
            expected_etag: "after-rename".into()
        }
    );
    assert_eq!(payload(&j, second.id), b"second");
    assert!(j.claim_mutation().unwrap().is_none());
    let mut saved = renamed;
    saved.size = 6;
    saved.etag = Some("after-second-save".into());
    saved.content_version = Some("content-second".into());
    j.acknowledge(second.id, active.attempt.unwrap(), saved.clone())
        .unwrap();
    let active = j.claim_mutation().unwrap().unwrap();
    assert_eq!(active.id, removed.id);
    assert_eq!(active.request.intent.before(), Some(&saved));
    j.acknowledge_mutation(
        removed.id,
        active.attempt.unwrap(),
        MutationReceipt::Removed { item: saved.id },
    )
    .unwrap();
    assert!(
        j.enqueue_after(removed.id, b"resurrect".as_slice())
            .is_err()
    );
    assert_eq!(payload(&j, first.id), b"first");
    assert_eq!(payload(&j, second.id), b"second");
}

#[test]
fn a_conflicted_rename_blocks_descendants_but_not_another_file() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("journal");
    let mut j = open(&root);
    let first = create(&mut j, "draft.txt");
    let moved = j
        .enqueue_mutation_after(first.id, rename(node(), "renamed.txt"))
        .unwrap();
    let second = j
        .enqueue_after(moved.id, b"unsent edit".as_slice())
        .unwrap();
    acknowledge_create(&mut j, first.id);
    let active = j.claim_mutation().unwrap().unwrap();
    j.defer_mutation(
        moved.id,
        active.attempt.unwrap(),
        MutationState::Conflict,
        Duration::ZERO,
    )
    .unwrap();
    let independent = create(&mut j, "independent.txt");
    assert_eq!(j.claim_next().unwrap().unwrap().id, independent.id);
    assert!(j.claim_next().unwrap().is_none());
    assert_eq!(j.get(second.id).unwrap().state, UploadState::Pending);
    assert_eq!(payload(&j, second.id), b"unsent edit");
    assert!(j.request_retry(second.id).is_err());
    drop(j);
    let j = open(&root);
    assert_eq!(j.mutation(moved.id).unwrap().state, MutationState::Conflict);
    assert_eq!(payload(&j, second.id), b"unsent edit");
}

#[test]
fn receipt_lineage_is_linear_across_operation_kinds_and_account_scopes() {
    let tmp = tempfile::tempdir().unwrap();
    let mut j = open(&tmp.path().join("journal"));
    let first = create(&mut j, "draft.txt");
    let mut foreign = rename(node(), "foreign.txt");
    foreign.scope.collection = "another-drive".into();
    assert!(matches!(
        j.enqueue_mutation_after(first.id, foreign),
        Err(JournalError::Account)
    ));
    let mut wrong_kind = rename(node(), "folder");
    if let MutationIntent::Relocate { before, .. } = &mut wrong_kind.intent {
        before.kind = NodeKind::Folder;
    }
    assert!(matches!(
        j.enqueue_mutation_after(first.id, wrong_kind),
        Err(JournalError::Intent)
    ));
    let moved = j
        .enqueue_mutation_after(first.id, rename(node(), "renamed.txt"))
        .unwrap();
    assert!(matches!(
        j.enqueue_after(first.id, b"fork".as_slice()),
        Err(JournalError::Stale)
    ));
    assert!(matches!(
        j.enqueue_mutation_after(first.id, rename(node(), "fork.txt")),
        Err(JournalError::Stale)
    ));
    let second = j.enqueue_after(moved.id, b"second".as_slice()).unwrap();
    assert!(matches!(
        j.enqueue_mutation_after(moved.id, rename(node(), "fork.txt")),
        Err(JournalError::Stale)
    ));
    assert_eq!(j.list(0, 100).unwrap().len(), 2);
    assert_eq!(j.list_mutations(0, 100).unwrap().len(), 1);
    assert_eq!(payload(&j, second.id), b"second");
}

#[test]
fn version_five_migration_keeps_existing_save_lineage_and_refuses_newer_schema() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("journal");
    let mut j = open(&root);
    let first = create(&mut j, "draft.txt");
    let second = j.enqueue_after(first.id, b"saved".as_slice()).unwrap();
    drop(j);
    let db = rusqlite::Connection::open(root.join("uploads.db")).unwrap();
    db.execute_batch("DROP TABLE write_successors; DROP TABLE namespace_operations; DROP TABLE namespace_remote; DROP TABLE namespace_entries; DROP TABLE namespace_objects; DROP TABLE namespace_scopes; PRAGMA user_version=5;")
        .unwrap();
    drop(db);
    let mut j = open(&root);
    assert!(matches!(
        j.enqueue_mutation_after(first.id, rename(node(), "fork.txt")),
        Err(JournalError::Stale)
    ));
    assert_eq!(
        j.get(second.id).unwrap().base.unwrap().predecessor,
        first.id
    );
    assert_eq!(payload(&j, first.id), b"first");
    assert_eq!(payload(&j, second.id), b"saved");
    drop(j);
    let db = rusqlite::Connection::open(root.join("uploads.db")).unwrap();
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        11
    );
    db.execute_batch("PRAGMA user_version=12;").unwrap();
    drop(db);
    assert!(matches!(
        UploadJournal::open(&root, &scope().account, 1024),
        Err(JournalError::Schema)
    ));
    let db = rusqlite::Connection::open(root.join("uploads.db")).unwrap();
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        12
    );
}

#[test]
fn a_local_name_and_cloud_intent_commit_together_and_new_saves_follow_the_rename() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("journal");
    let mut j = open(&root);
    let mut local = node();
    local.size = 0;
    let working = j
        .create_working(scope(), local, true, b"".as_slice())
        .unwrap();
    j.write_working(working.id, 0, b"first").unwrap();
    let moved = j
        .relocate_working(working.id, "root".into(), "Grüße.txt".into())
        .unwrap();
    let first = j.list(0, 100).unwrap().remove(0); // Relocation seals the unsaved source first.
    let renamed = j.working_file(working.id).unwrap();
    assert_eq!(renamed.node.id, working.node.id);
    assert_eq!(renamed.node.name, "Grüße.txt");
    assert_eq!(renamed.latest, Some(moved.id));
    assert!(!renamed.dirty);
    assert_eq!(moved.working_file, Some(working.id));
    assert_eq!(moved.base.as_ref().unwrap().predecessor, first.id);
    j.write_working(working.id, 0, b"second").unwrap();
    let second = j.seal_working(working.id).unwrap().unwrap();
    assert_eq!(second.base.as_ref().unwrap().predecessor, moved.id);
    drop(j);
    let mut j = open(&root);
    assert_eq!(j.working_file(working.id).unwrap().node.name, "Grüße.txt");
    assert_eq!(j.read_working(working.id, 0, 100).unwrap(), b"second");
    let created = acknowledge_create(&mut j, first.id);
    let active = j.claim_mutation().unwrap().unwrap();
    assert_eq!(active.request.intent.before(), Some(&created));
    let mut remote = created;
    remote.name = "Grüße.txt".into();
    remote.etag = Some("renamed".into());
    j.acknowledge_mutation(
        moved.id,
        active.attempt.unwrap(),
        MutationReceipt::Upsert(remote),
    )
    .unwrap();
    let active = j.claim_next().unwrap().unwrap();
    assert_eq!(active.id, second.id);
    assert_eq!(
        active.intent,
        UploadIntent::Replace {
            item: "assigned-id".into(),
            expected_etag: "renamed".into()
        }
    );
    assert_eq!(payload(&j, first.id), b"first");
    assert_eq!(payload(&j, second.id), b"second");
}

#[test]
fn failed_relocation_transaction_keeps_the_old_name_and_does_not_consume_lineage() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("journal");
    let mut j = open(&root);
    let mut local = node();
    local.size = 0;
    let working = j
        .create_working(scope(), local, true, b"".as_slice())
        .unwrap();
    j.write_working(working.id, 0, b"first").unwrap();
    let first = j.seal_working(working.id).unwrap().unwrap();
    let db = rusqlite::Connection::open(root.join("uploads.db")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_move BEFORE UPDATE ON working_files WHEN json_extract(NEW.body,'$.node.name')!=json_extract(OLD.body,'$.node.name') BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(matches!(
        j.relocate_working(working.id, "root".into(), "new.txt".into()),
        Err(JournalError::Storage)
    ));
    assert!(j.list_mutations(0, 100).unwrap().is_empty());
    assert_eq!(j.working_file(working.id).unwrap().node.name, "draft.txt");
    assert_eq!(j.working_file(working.id).unwrap().latest, Some(first.id));
    assert_eq!(
        db.query_row("SELECT count(*) FROM write_successors", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    db.execute_batch("DROP TRIGGER fail_move;").unwrap();
    drop(db);
    drop(j);
    let mut j = open(&root);
    let moved = j
        .relocate_working(working.id, "root".into(), "new.txt".into())
        .unwrap();
    assert_eq!(j.working_file(working.id).unwrap().latest, Some(moved.id));
    assert_eq!(j.read_working(working.id, 0, 100).unwrap(), b"first");
    let mut other = node();
    other.name = "occupied.txt".into();
    other.size = 0;
    j.create_working(scope(), other, true, b"".as_slice())
        .unwrap();
    assert!(matches!(
        j.relocate_working(working.id, "root".into(), "occupied.txt".into()),
        Err(JournalError::Stale)
    ));
    assert_eq!(j.working_file(working.id).unwrap().node.name, "new.txt");
    assert_eq!(j.list_mutations(0, 100).unwrap().len(), 1);
}

#[test]
fn relocating_a_clean_working_copy_does_not_create_an_upload_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let mut j = open(&tmp.path().join("journal"));
    let mut remote = node();
    remote.id = "existing".into();
    remote.etag = Some("existing-version".into());
    let working = j
        .create_working(scope(), remote.clone(), false, b"first".as_slice())
        .unwrap();
    let retained = j.retained_bytes().unwrap();
    let moved = j
        .relocate_working(working.id, "another-folder".into(), "moved.txt".into())
        .unwrap();
    assert_eq!(j.retained_bytes().unwrap(), retained);
    assert!(j.list(0, 100).unwrap().is_empty());
    assert!(moved.base.is_none());
    let active = j.claim_mutation().unwrap().unwrap();
    assert_eq!(active.request.intent.before(), Some(&remote));
    assert_eq!(
        j.working_file(working.id)
            .unwrap()
            .node
            .parent_id
            .as_deref(),
        Some("another-folder")
    );
}

#[test]
fn changed_or_unverifiable_content_after_an_uncertain_rename_never_rebases_a_save() {
    for unknown in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("journal");
        let mut j = open(&root);
        let first = create(&mut j, "draft.txt");
        let moved = j
            .enqueue_mutation_after(first.id, rename(node(), "renamed.txt"))
            .unwrap();
        let second = j
            .enqueue_after(moved.id, b"my retained edit".as_slice())
            .unwrap();
        let created = acknowledge_create(&mut j, first.id);
        let active = j.claim_mutation().unwrap().unwrap();
        j.defer_mutation(
            moved.id,
            active.attempt.unwrap(),
            MutationState::VerifyRequired,
            Duration::ZERO,
        )
        .unwrap();
        let active = j.claim_mutation().unwrap().unwrap();
        let mut observed = created;
        observed.name = "renamed.txt".into();
        observed.etag = Some("external-version".into());
        observed.content_version = if unknown {
            None
        } else {
            Some("someone-elses-content".into())
        };
        let state = j
            .acknowledge_mutation(
                moved.id,
                active.attempt.unwrap(),
                MutationReceipt::Upsert(observed),
            )
            .unwrap();
        assert_eq!(
            state,
            if unknown {
                MutationState::NeedsReview
            } else {
                MutationState::Conflict
            }
        );
        assert!(j.claim_next().unwrap().is_none());
        assert!(!j.get(second.id).unwrap().base.unwrap().resolved);
        assert_eq!(payload(&j, second.id), b"my retained edit");
        drop(j);
        let mut j = open(&root);
        assert_eq!(j.mutation(moved.id).unwrap().state, state);
        assert!(j.claim_next().unwrap().is_none());
    }
}

#[test]
fn all_ready_identity_bindings_are_reserved_before_either_worker_selects_new_work() {
    let tmp = tempfile::tempdir().unwrap();
    let mut j = open(&tmp.path().join("journal"));
    let mut creates = vec![];
    let mut moves = vec![];
    for i in 0..257 {
        let name = format!("draft-{i}");
        let first = create(&mut j, &name);
        let mut local = node();
        local.name = name;
        local.id = format!("local-{i}");
        moves.push(
            j.enqueue_mutation_after(first.id, rename(local, &format!("renamed-{i}")))
                .unwrap()
                .id,
        );
        creates.push(first.id);
    }
    let mut active = vec![];
    for id in &creates {
        let r = j.claim_next().unwrap().unwrap();
        assert_eq!(&r.id, id);
        active.push(r);
    }
    for (i, r) in active.into_iter().enumerate() {
        let mut remote = node();
        remote.name = format!("draft-{i}");
        remote.id = format!("remote-{i}");
        remote.etag = Some("created".into());
        j.acknowledge(r.id, r.attempt.unwrap(), remote).unwrap();
    }
    let mut external = node();
    external.id = "remote-256".into();
    external.name = "external-name".into();
    external.etag = Some("external".into());
    let later = j.enqueue_mutation(rename(external, "later")).unwrap();
    assert!(
        j.claim_mutation().unwrap().is_none(),
        "remaining unbound identities must stop selection"
    );
    assert!(!j.mutation(moves[256]).unwrap().base.unwrap().resolved);
    assert_eq!(j.claim_mutation().unwrap().unwrap().id, moves[0]);
    assert!(j.mutation(moves[256]).unwrap().base.unwrap().resolved);
    assert_eq!(j.mutation(later.id).unwrap().state, MutationState::Pending);
}
