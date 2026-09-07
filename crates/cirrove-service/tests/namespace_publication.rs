//! Committed local namespace batches, with synthetic bytes only.
#![allow(clippy::unwrap_used)]
use cirrove_core::{Node, NodeKind, Scope};
use cirrove_service::journal::{JournalError, UploadJournal};
use std::{
    path::Path,
    time::{Duration, Instant},
};

fn scope() -> Scope {
    Scope {
        account: "publication".into(),
        provider: "fixture".into(),
        collection: "drive".into(),
    }
}
fn node(id: &str) -> Node {
    Node {
        id: id.into(),
        name: format!("{id}.txt"),
        parent_id: Some("root".into()),
        kind: NodeKind::File,
        size: 3,
        etag: Some("original".into()),
        content_version: None,
        modified_unix: 1,
        target: None,
    }
}
fn open(root: &Path) -> UploadJournal {
    let until = Instant::now() + Duration::from_secs(2);
    loop {
        match UploadJournal::open(root, &scope().account, 4096) {
            Err(JournalError::Busy) if Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(2))
            }
            result => return result.unwrap(),
        }
    }
}

#[test]
fn repeated_writes_coalesce_and_indexed_publication_reads_only_changed_objects() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("journal");
    let mut j = open(&root);
    for n in 0..128 {
        j.observe_namespace_file(scope(), node(&format!("unchanged-{n}")))
            .unwrap();
    }
    let working = j
        .create_working(scope(), node("edited"), false, b"old".as_slice())
        .unwrap();
    let initial = j.namespace_publication(0).unwrap();
    assert_eq!(initial.objects.len(), 129);
    for n in 0..32 {
        j.write_working(working.id, 0, &[n, n, n]).unwrap();
    }
    let changed = j.namespace_publication(initial.through).unwrap();
    assert_eq!(changed.objects.len(), 1);
    assert_eq!(changed.objects[0].working.as_ref().unwrap().id, working.id);
    assert!(changed.objects[0].working.as_ref().unwrap().dirty);
    assert!(changed.through > initial.through + 32);
    assert!(
        j.namespace_publication(changed.through)
            .unwrap()
            .objects
            .is_empty()
    );
    let db = rusqlite::Connection::open(root.join("uploads.db")).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM namespace_changes", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        129
    );
    let mut query = db.prepare("EXPLAIN QUERY PLAN SELECT object FROM namespace_changes WHERE sequence>?1 ORDER BY sequence").unwrap();
    let plan = query
        .query_map([initial.through as i64], |r| r.get::<_, String>(3))
        .unwrap()
        .map(|r| r.unwrap())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        plan.contains("SEARCH namespace_changes USING INDEX namespace_changes_sequence"),
        "{plan}"
    );
    assert!(matches!(
        j.namespace_publication(changed.through + 1),
        Err(JournalError::Stale)
    ));
    assert!(matches!(
        j.namespace_publication(u64::MAX),
        Err(JournalError::Stale)
    ));
}

#[test]
fn schema_eleven_migration_seeds_complete_namespace_and_preserves_pending_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("journal");
    let mut j = open(&root);
    let working = j
        .create_working(scope(), node("edited"), false, b"old".as_slice())
        .unwrap();
    j.write_working(working.id, 0, b"new").unwrap();
    let pending = j.seal_working(working.id).unwrap().unwrap();
    drop(j);
    let db = rusqlite::Connection::open(root.join("uploads.db")).unwrap();
    db.execute_batch(
        "DROP TRIGGER namespace_publish_insert; DROP TRIGGER namespace_publish_update;
        DROP TABLE namespace_changes; DROP TABLE namespace_clock; PRAGMA user_version=11;",
    )
    .unwrap();
    drop(db);
    let mut j = open(&root);
    let initial = j.namespace_publication(0).unwrap();
    assert_eq!(initial.objects.len(), 1);
    assert_eq!(initial.objects[0].object.latest, Some(pending.id));
    assert_eq!(j.read_working(working.id, 0, 10).unwrap(), b"new");
    assert!(
        j.namespace_publication(initial.through)
            .unwrap()
            .objects
            .is_empty()
    );
    j.write_working(working.id, 0, b"now").unwrap();
    assert_eq!(
        j.namespace_publication(initial.through)
            .unwrap()
            .objects
            .len(),
        1
    );
    drop(j);
    let j = open(&root);
    assert_eq!(j.namespace_publication(0).unwrap().objects.len(), 1);
    assert_eq!(j.read_working(working.id, 0, 10).unwrap(), b"now");
    let db = rusqlite::Connection::open(root.join("uploads.db")).unwrap();
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, i32>(0))
            .unwrap(),
        13
    );
}

#[test]
fn missing_publication_schema_or_trigger_is_refused_without_touching_pending_edits() {
    for broken in [
        "DROP TABLE namespace_changes",
        "DROP TRIGGER namespace_publish_update",
        "DROP INDEX namespace_changes_sequence",
        "DELETE FROM namespace_clock",
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("journal");
        let mut j = open(&root);
        let working = j
            .create_working(scope(), node("edited"), false, b"old".as_slice())
            .unwrap();
        j.write_working(working.id, 0, b"new").unwrap();
        j.seal_working(working.id).unwrap();
        drop(j);
        let db = rusqlite::Connection::open(root.join("uploads.db")).unwrap();
        db.execute_batch(broken).unwrap();
        assert!(
            UploadJournal::open(&root, &scope().account, 4096).is_err(),
            "{broken}"
        );
        assert_eq!(
            std::fs::read(root.join("working").join(working.id.to_string())).unwrap(),
            b"new"
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM uploads", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
}

#[test]
fn rollback_and_clock_exhaustion_never_expose_an_unpublished_namespace_change() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("journal");
    let mut j = open(&root);
    let original = j.observe_namespace_file(scope(), node("original")).unwrap();
    let initial = j.namespace_publication(0).unwrap();
    let db = rusqlite::Connection::open(root.join("uploads.db")).unwrap();
    db.execute_batch("CREATE TRIGGER deny_entry BEFORE INSERT ON namespace_entries BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(j.observe_namespace_file(scope(), node("rejected")).is_err());
    let unchanged = j.namespace_publication(initial.through).unwrap();
    assert_eq!(unchanged.through, initial.through);
    assert!(unchanged.objects.is_empty());
    assert!(
        j.namespace_by_remote(&scope(), "rejected")
            .unwrap()
            .is_none()
    );
    db.execute_batch(
        "DROP TRIGGER deny_entry; UPDATE namespace_clock SET value=9223372036854775807;",
    )
    .unwrap();
    assert!(
        j.relocate_namespace_file(
            original.id,
            original.revision,
            "root".into(),
            "renamed".into()
        )
        .is_err()
    );
    assert_eq!(
        j.namespace_object(original.id).unwrap().node.name,
        "original.txt"
    );
    assert!(
        j.namespace_publication(initial.through)
            .unwrap()
            .objects
            .is_empty()
    );
    assert!(j.claim_mutation().unwrap().is_none());
}
