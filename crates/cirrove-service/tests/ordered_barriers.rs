//! Two-object ordering for replacement publication and subsequent source cleanup.
//! Synthetic journal operations; no mounted-path replacement or cloud mutations.
#![allow(clippy::unwrap_used)]
use cirrove_core::{
    Node, NodeKind, Scope,
    mutation::{MutationIntent, MutationReceipt, MutationRequest},
};
use cirrove_service::journal::{
    JournalError, MutationState, UploadIntent, UploadJournal, UploadState,
};
use std::{
    io::{self, Read},
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};
use uuid::Uuid;

fn scope() -> Scope {
    Scope {
        account: "barrier-fixture".into(),
        provider: "fixture".into(),
        collection: "drive".into(),
    }
}
fn open(path: &Path) -> UploadJournal {
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let until = Instant::now() + Duration::from_secs(2);
    loop {
        match UploadJournal::open(path, &scope().account, 1024 * 1024) {
            Err(JournalError::Busy) if Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(2))
            }
            value => return value.unwrap(),
        }
    }
}
fn create(j: &mut UploadJournal, name: &str) -> Uuid {
    j.enqueue(
        scope(),
        UploadIntent::Create {
            parent: "root".into(),
            name: name.into(),
        },
        b"old".as_slice(),
    )
    .unwrap()
    .id
}
fn node(id: &str, name: &str, etag: &str, size: u64) -> Node {
    Node {
        id: id.into(),
        name: name.into(),
        parent_id: Some("root".into()),
        kind: NodeKind::File,
        size,
        etag: Some(etag.into()),
        content_version: Some(format!("content-{etag}")),
        modified_unix: 1,
        target: None,
    }
}
fn removed(before: Node) -> MutationRequest {
    MutationRequest {
        scope: scope(),
        intent: MutationIntent::RemoveFile { before },
    }
}
fn payload(j: &UploadJournal, id: Uuid) -> Vec<u8> {
    let mut bytes = vec![];
    j.payload(id).unwrap().read_to_end(&mut bytes).unwrap();
    bytes
}
fn ack_create(j: &mut UploadJournal, expected: Uuid, remote: Node) {
    let claimed = j.claim_next().unwrap().unwrap();
    assert_eq!(claimed.id, expected);
    j.acknowledge(claimed.id, claimed.attempt.unwrap(), remote)
        .unwrap();
}

#[test]
fn replacement_uses_the_target_base_and_cleanup_waits_for_publication_across_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("journal");
    let mut j = open(&path);
    let target = create(&mut j, "document.txt");
    let source = create(&mut j, "temporary.txt");
    let replacement = j
        .enqueue_after_all(target, &[source], b"replacement".as_slice())
        .unwrap();
    let cleanup = j
        .enqueue_mutation_after_all(
            source,
            &[replacement.id],
            removed(node("local-source", "temporary.txt", "placeholder", 3)),
        )
        .unwrap();
    let later = j
        .enqueue_after(replacement.id, b"later".as_slice())
        .unwrap();
    assert_eq!(
        j.operation_prerequisites(replacement.id).unwrap(),
        vec![source]
    );
    assert_eq!(
        j.operation_prerequisites(cleanup.id).unwrap(),
        vec![replacement.id]
    );
    ack_create(
        &mut j,
        target,
        node("target-id", "document.txt", "target-etag", 3),
    );
    assert!(j.claim_mutation().unwrap().is_none());
    let active = j.claim_next().unwrap().unwrap();
    assert_eq!(active.id, source);
    assert!(j.claim_next().unwrap().is_none());
    j.acknowledge(
        source,
        active.attempt.unwrap(),
        node("source-id", "temporary.txt", "source-etag", 3),
    )
    .unwrap();
    let write = j.claim_next().unwrap().unwrap();
    assert_eq!(write.id, replacement.id);
    assert_eq!(
        write.intent,
        UploadIntent::Replace {
            item: "target-id".into(),
            expected_etag: "target-etag".into()
        }
    );
    assert_eq!(payload(&j, write.id), b"replacement");
    assert!(j.claim_mutation().unwrap().is_none());
    // An uncertain remote commit never releases source cleanup. Restart must
    // reconcile the target operation first, retaining both snapshots meanwhile.
    drop(j);
    let mut j = open(&path);
    assert_eq!(j.get(write.id).unwrap().state, UploadState::VerifyRequired);
    assert!(j.claim_mutation().unwrap().is_none());
    assert!(j.claim_next().unwrap().is_none());
    let verification = j.claim_next_verification().unwrap().unwrap();
    assert_eq!(verification.id, write.id);
    assert!(j.claim_mutation().unwrap().is_none());
    j.acknowledge(
        verification.id,
        verification.attempt.unwrap(),
        node("target-id", "document.txt", "published-etag", 11),
    )
    .unwrap();
    let delete = j.claim_mutation().unwrap().unwrap();
    assert_eq!(delete.id, cleanup.id);
    assert_eq!(delete.request.intent.before().unwrap().id, "source-id");
    assert_eq!(
        delete.request.intent.before().unwrap().etag.as_deref(),
        Some("source-etag")
    );
    assert_ne!(delete.request.intent.before().unwrap().id, "target-id");
    j.acknowledge_mutation(
        delete.id,
        delete.attempt.unwrap(),
        MutationReceipt::Removed {
            item: "source-id".into(),
        },
    )
    .unwrap();
    let next = j.claim_next().unwrap().unwrap();
    assert_eq!(next.id, later.id);
    assert_eq!(
        next.intent,
        UploadIntent::Replace {
            item: "target-id".into(),
            expected_etag: "published-etag".into()
        }
    );
    assert_eq!(payload(&j, source), b"old");
    assert_eq!(payload(&j, replacement.id), b"replacement");
}

#[test]
fn failed_or_conflicted_prerequisites_block_replacement_but_not_independent_files() {
    for failure in [
        UploadState::Failed,
        UploadState::Conflict,
        UploadState::VerifyRequired,
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let mut j = open(tmp.path());
        let target = create(&mut j, "target");
        let source = create(&mut j, "source");
        let replacement = j
            .enqueue_after_all(target, &[source], b"new".as_slice())
            .unwrap();
        let cleanup = j
            .enqueue_mutation_after_all(
                source,
                &[replacement.id],
                removed(node("local-source", "source", "placeholder", 3)),
            )
            .unwrap();
        ack_create(
            &mut j,
            target,
            node("target-id", "target", "target-etag", 3),
        );
        let claimed = j.claim_next().unwrap().unwrap();
        assert_eq!(claimed.id, source);
        j.defer_attempt(source, claimed.attempt.unwrap(), failure, Duration::ZERO)
            .unwrap();
        assert!(j.get(replacement.id).unwrap().base.unwrap().resolved);
        assert!(j.claim_next().unwrap().is_none());
        assert!(j.claim_mutation().unwrap().is_none());
        let independent = create(&mut j, "independent");
        ack_create(
            &mut j,
            independent,
            node("other-id", "independent", "other-etag", 3),
        );
        assert_eq!(j.get(replacement.id).unwrap().state, UploadState::Pending);
        assert_eq!(
            j.mutation(cleanup.id).unwrap().state,
            MutationState::Pending
        );
        assert_eq!(payload(&j, replacement.id), b"new");
        if failure == UploadState::VerifyRequired {
            let verify = j.claim_next_verification().unwrap().unwrap();
            assert_eq!(verify.id, source);
            j.acknowledge(
                source,
                verify.attempt.unwrap(),
                node("source-id", "source", "verified-source", 3),
            )
            .unwrap();
            assert_eq!(j.claim_next().unwrap().unwrap().id, replacement.id);
        }
    }
}

#[test]
fn namespace_prerequisites_do_not_supply_the_content_base_or_bypass_uncertainty() {
    let tmp = tempfile::tempdir().unwrap();
    let mut j = open(tmp.path());
    let target = create(&mut j, "target");
    let other = node("other-id", "other", "other-etag", 3);
    let change = j.enqueue_mutation(removed(other)).unwrap();
    let dependent = j
        .enqueue_after_all(target, &[change.id], b"new".as_slice())
        .unwrap();
    ack_create(
        &mut j,
        target,
        node("target-id", "target", "target-etag", 3),
    );
    let active = j.claim_mutation().unwrap().unwrap();
    j.defer_mutation(
        active.id,
        active.attempt.unwrap(),
        MutationState::NeedsReview,
        Duration::ZERO,
    )
    .unwrap();
    assert!(j.claim_next().unwrap().is_none());
    assert!(j.claim_mutation().unwrap().is_none());
    j.request_mutation_retry(change.id).unwrap();
    let verify = j.claim_mutation().unwrap().unwrap();
    assert_eq!(verify.state, MutationState::Verifying);
    assert!(j.claim_next().unwrap().is_none());
    j.acknowledge_mutation(
        verify.id,
        verify.attempt.unwrap(),
        MutationReceipt::Removed {
            item: "other-id".into(),
        },
    )
    .unwrap();
    let next = j.claim_next().unwrap().unwrap();
    assert_eq!(next.id, dependent.id);
    assert_eq!(
        next.intent,
        UploadIntent::Replace {
            item: "target-id".into(),
            expected_etag: "target-etag".into()
        }
    );
}

#[test]
fn direct_verification_cannot_skip_a_persisted_prerequisite() {
    let tmp = tempfile::tempdir().unwrap();
    let mut j = open(tmp.path());
    let target = create(&mut j, "target");
    let source = create(&mut j, "source");
    let dependent = j
        .enqueue_after_all(target, &[source], b"new".as_slice())
        .unwrap();
    ack_create(
        &mut j,
        target,
        node("target-id", "target", "target-etag", 3),
    );
    let source_claim = j.claim_next().unwrap().unwrap();
    assert_eq!(source_claim.id, source);
    j.defer_attempt(
        source,
        source_claim.attempt.unwrap(),
        UploadState::Failed,
        Duration::ZERO,
    )
    .unwrap();
    assert!(j.get(dependent.id).unwrap().base.unwrap().resolved);
    // Fault fixture: a record presented for explicit recovery while its durable
    // prerequisite is still incomplete. The direct API must also fail closed.
    let db = rusqlite::Connection::open(tmp.path().join("uploads.db")).unwrap();
    db.execute("UPDATE uploads SET state='verify_required',body=json_set(body,'$.state','verify_required') WHERE id=?1",[dependent.id.to_string()]).unwrap();
    assert!(matches!(
        j.claim_verification(dependent.id),
        Err(JournalError::Stale)
    ));
    assert!(j.claim_next_verification().unwrap().is_none());
    assert_eq!(
        j.get(dependent.id).unwrap().state,
        UploadState::VerifyRequired
    );
    assert_eq!(payload(&j, dependent.id), b"new");
}

struct NeverRead<'a>(&'a AtomicUsize);
impl Read for NeverRead<'_> {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(io::Error::other("unexpected source read"))
    }
}
#[test]
fn invalid_edges_and_failed_transactions_do_not_consume_the_linear_successor() {
    let tmp = tempfile::tempdir().unwrap();
    let mut j = open(tmp.path());
    let target = create(&mut j, "target");
    let source = create(&mut j, "source");
    let mut foreign = scope();
    foreign.collection = "other-drive".into();
    let alien = j
        .enqueue(
            foreign,
            UploadIntent::Create {
                parent: "root".into(),
                name: "alien".into(),
            },
            b"old".as_slice(),
        )
        .unwrap();
    let reads = AtomicUsize::new(0);
    assert!(matches!(
        j.enqueue_after_all(target, &[alien.id], NeverRead(&reads)),
        Err(JournalError::Account)
    ));
    assert!(matches!(
        j.enqueue_after_all(target, &[source, source], NeverRead(&reads)),
        Err(JournalError::Intent)
    ));
    assert!(matches!(
        j.enqueue_after_all(target, &[Uuid::new_v4()], NeverRead(&reads)),
        Err(JournalError::Missing)
    ));
    assert!(matches!(
        j.enqueue_after_all(target, &[source; 17], NeverRead(&reads)),
        Err(JournalError::Quota)
    ));
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    let db = rusqlite::Connection::open(tmp.path().join("uploads.db")).unwrap();
    db.execute_batch("CREATE TRIGGER deny_barrier BEFORE INSERT ON write_prerequisites BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(
        j.enqueue_after_all(target, &[source], b"retained orphan".as_slice())
            .is_err()
    );
    assert_eq!(j.list(0, 100).unwrap().len(), 3);
    assert_eq!(
        db.query_row("SELECT count(*) FROM write_prerequisites", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM write_successors", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    db.execute_batch("DROP TRIGGER deny_barrier").unwrap();
    let replacement = j
        .enqueue_after_all(target, &[source], b"new".as_slice())
        .unwrap();
    assert!(matches!(
        j.enqueue_after(target, b"fork".as_slice()),
        Err(JournalError::Stale)
    ));
    // Ordering on source did not consume its content successor: guarded cleanup
    // can follow source while independently depending on target publication.
    assert!(
        j.enqueue_mutation_after_all(
            source,
            &[replacement.id],
            removed(node("local", "source", "pending", 3))
        )
        .is_ok()
    );
}

#[test]
fn schema_nine_migration_retains_pending_saves_and_missing_new_schema_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let mut j = open(tmp.path());
    let first = create(&mut j, "file");
    let next = j.enqueue_after(first, b"next".as_slice()).unwrap();
    drop(j);
    let db = rusqlite::Connection::open(tmp.path().join("uploads.db")).unwrap();
    db.execute_batch("DROP TABLE write_prerequisites; PRAGMA user_version=9;")
        .unwrap();
    drop(db);
    let j = open(tmp.path());
    assert!(j.operation_prerequisites(first).unwrap().is_empty());
    assert!(j.operation_prerequisites(next.id).unwrap().is_empty());
    assert_eq!(payload(&j, first), b"old");
    assert_eq!(payload(&j, next.id), b"next");
    assert_eq!(j.get(next.id).unwrap().base.unwrap().predecessor, first);
    drop(j);
    let db = rusqlite::Connection::open(tmp.path().join("uploads.db")).unwrap();
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        13
    );
    db.execute_batch("DROP TABLE write_prerequisites").unwrap();
    assert!(UploadJournal::open(tmp.path(), &scope().account, 1024 * 1024).is_err());
    assert_eq!(
        db.query_row("SELECT count(*) FROM uploads", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
    db.execute_batch("PRAGMA user_version=14").unwrap();
    assert!(matches!(
        UploadJournal::open(tmp.path(), &scope().account, 1024 * 1024),
        Err(JournalError::Schema)
    ));
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        14
    );
}
