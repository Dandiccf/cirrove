//! Synthetic page, ordering and fault fixtures for bounded foreground publication.
#![allow(clippy::unwrap_used)]
use cirrove_core::{
    CancellationToken, Change, ChangePage, Checkpoint, Cursor, DirectoryPage, Node, NodeKind, Scope,
};
use cirrove_store::{
    DirectoryPublication, DirectoryPublicationResult as Published, ObservationResult, Store,
};
use std::{
    path::Path,
    time::{Duration, Instant},
};
fn scope() -> Scope {
    Scope {
        account: "account".into(),
        provider: "fixture".into(),
        collection: "drive".into(),
    }
}
fn node(id: &str, parent: &str, version: u64) -> Node {
    Node {
        id: id.into(),
        parent_id: Some(parent.into()),
        name: format!("name-{id}"),
        kind: NodeKind::File,
        size: version,
        modified_unix: 0,
        etag: Some(version.to_string()),
        content_version: None,
        target: None,
    }
}
fn begin(path: &Path, parent: &str) -> DirectoryPublication {
    Store::open(path)
        .unwrap()
        .directory_publication(
            &scope(),
            parent,
            CancellationToken::new(),
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap()
}
fn page(nodes: Vec<Node>, next: Option<&str>) -> DirectoryPage {
    DirectoryPage {
        nodes,
        next: next.map(|s| Cursor(s.into())),
    }
}
fn delta(db: &mut Store, changes: Vec<Change>, reset: bool) {
    let cursor = db.begin(&scope(), reset).unwrap();
    db.stage(
        &scope(),
        cursor.as_ref(),
        &ChangePage {
            changes,
            checkpoint: Checkpoint::Complete(Cursor("complete".into())),
        },
    )
    .unwrap();
}
#[test]
fn pages_are_invisible_release_writer_and_publish_together() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("metadata.db");
    let mut db = Store::open(&path).unwrap();
    delta(&mut db, vec![Change::Upsert(node("old", "root", 1))], false);
    let stage = begin(&path, "root")
        .page(page(vec![node("b", "root", 1)], Some("next")))
        .unwrap();
    assert_eq!(
        db.children(&scope(), "root").unwrap().unwrap(),
        vec![node("old", "root", 1)]
    );
    // A different writer can commit between pages; no transaction crosses I/O.
    db.observe_node(&scope(), &node("unrelated", "other", 2))
        .unwrap();
    let stage = stage.page(page(vec![node("a", "root", 1)], None)).unwrap();
    assert_eq!(
        stage.publish().unwrap(),
        Published::Published { changed: true }
    );
    assert_eq!(
        db.children(&scope(), "root").unwrap().unwrap(),
        vec![node("a", "root", 1), node("b", "root", 1)]
    );
    assert!(db.node(&scope(), "old").unwrap().is_none());
    assert_eq!(db.nodes(&scope()).unwrap(), vec![node("old", "root", 1)]);
    assert_eq!(
        db.cursor(&scope()).unwrap(),
        Some(Cursor("complete".into()))
    );
}
#[test]
fn duplicates_repeated_cursors_wrong_parents_and_incomplete_listings_fail_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("metadata.db");
    let db = Store::open(&path).unwrap();
    assert!(
        begin(&path, "root")
            .page(page(vec![node("a", "root", 1)], Some("next")))
            .unwrap()
            .publish()
            .is_err()
    );
    let stage = begin(&path, "root")
        .page(page(vec![node("a", "root", 1)], Some("next")))
        .unwrap();
    assert!(stage.page(page(vec![node("a", "root", 2)], None)).is_err());
    let stage = begin(&path, "root")
        .page(page(vec![], Some("repeated")))
        .unwrap();
    assert!(stage.page(page(vec![], Some("repeated"))).is_err());
    assert!(
        begin(&path, "root")
            .page(page(vec![node("a", "other", 1)], None))
            .is_err()
    );
    let mut oversized = node("a", "root", 1);
    oversized.name = "x".repeat(1024 * 1024 + 1);
    assert!(
        begin(&path, "root")
            .page(page(vec![oversized], None))
            .is_err()
    );
    assert!(db.children(&scope(), "root").unwrap().is_none());
}
#[test]
fn superseded_responses_never_restore_deleted_or_moved_items() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("metadata.db");
    let mut db = Store::open(&path).unwrap();
    let stage = begin(&path, "root")
        .page(page(vec![node("a", "root", 1)], None))
        .unwrap();
    delta(&mut db, vec![Change::Delete { id: "a".into() }], false);
    assert_eq!(
        stage.publish().unwrap(),
        Published::Superseded { known: true }
    );
    assert!(db.children(&scope(), "root").unwrap().unwrap().is_empty());
    let stage = begin(&path, "root")
        .page(page(vec![node("a", "root", 1)], None))
        .unwrap();
    db.observe_node(&scope(), &node("a", "other", 2)).unwrap();
    assert_eq!(
        stage.publish().unwrap(),
        Published::Superseded { known: true }
    );
    assert_eq!(db.node(&scope(), "a").unwrap(), Some(node("a", "other", 2)));
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("metadata.db");
    let stage = begin(&path, "root")
        .page(page(vec![node("a", "root", 1)], None))
        .unwrap();
    Store::open(&path)
        .unwrap()
        .observe_node(&scope(), &node("a", "other", 2))
        .unwrap();
    assert_eq!(
        stage.publish().unwrap(),
        Published::Superseded { known: false }
    );
}
#[test]
fn newer_listing_survives_a_pending_baseline_and_preserves_a_move_elsewhere() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("metadata.db");
    let mut db = Store::open(&path).unwrap();
    delta(
        &mut db,
        vec![Change::Upsert(node("moved", "root", 1))],
        false,
    );
    db.observe_node(&scope(), &node("moved", "elsewhere", 2))
        .unwrap();
    let cursor = db.begin(&scope(), true).unwrap();
    let stage = begin(&path, "root")
        .page(page(vec![node("new", "root", 3)], None))
        .unwrap();
    db.stage(
        &scope(),
        cursor.as_ref(),
        &ChangePage {
            changes: vec![],
            checkpoint: Checkpoint::Complete(Cursor("replacement".into())),
        },
    )
    .unwrap();
    assert_eq!(
        stage.publish().unwrap(),
        Published::Superseded { known: true }
    );
    // Inverted completion: publication started after the baseline request and
    // completed first remains an observation when that older baseline commits.
    let cursor = db.begin(&scope(), true).unwrap();
    let stage = begin(&path, "root")
        .page(page(vec![node("new", "root", 3)], None))
        .unwrap();
    assert_eq!(
        stage.publish().unwrap(),
        Published::Published { changed: true }
    );
    db.stage(
        &scope(),
        cursor.as_ref(),
        &ChangePage {
            changes: vec![],
            checkpoint: Checkpoint::Complete(Cursor("replacement-2".into())),
        },
    )
    .unwrap();
    assert_eq!(
        db.children(&scope(), "root").unwrap().unwrap(),
        vec![node("new", "root", 3)]
    );
    delta(
        &mut db,
        vec![Change::Upsert(node("moved", "root", 1))],
        false,
    );
    db.observe_node(&scope(), &node("moved", "elsewhere", 2))
        .unwrap();
    begin(&path, "root")
        .page(page(vec![], None))
        .unwrap()
        .publish()
        .unwrap();
    assert_eq!(
        db.node(&scope(), "moved").unwrap(),
        Some(node("moved", "elsewhere", 2))
    );
}
#[test]
fn cancellation_and_failed_final_insert_rollback_every_visible_change() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("metadata.db");
    let mut db = Store::open(&path).unwrap();
    db.observe_directory(&scope(), "root", &[node("old", "root", 1)])
        .unwrap();
    let token = CancellationToken::new();
    let stage = Store::open(&path)
        .unwrap()
        .directory_publication(
            &scope(),
            "root",
            token.clone(),
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap()
        .page(page(vec![node("new", "root", 2)], None))
        .unwrap();
    token.cancel();
    assert!(stage.publish().is_err());
    let ticket = db.node_observation(&scope(), "old").unwrap();
    let stage = begin(&path, "root")
        .page(page(vec![node("new", "root", 2)], None))
        .unwrap();
    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute_batch("CREATE TRIGGER fail_final BEFORE INSERT ON directory_entries BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(stage.publish().is_err());
    assert_eq!(
        db.children(&scope(), "root").unwrap().unwrap(),
        vec![node("old", "root", 1)]
    );
    assert!(db.node(&scope(), "new").unwrap().is_none());
    assert!(matches!(
        db.publish_node(&ticket, &node("old", "root", 1)).unwrap(),
        ObservationResult::Published { .. }
    ));
}
#[test]
fn unchanged_semantic_legacy_rows_supersede_old_absence_without_a_change_signal() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("metadata.db");
    let mut db = Store::open(&path).unwrap();
    let item = node("a", "root", 1);
    db.observe_directory(&scope(), "root", std::slice::from_ref(&item))
        .unwrap();
    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute_batch("UPDATE directory_entries SET body=json_remove(body,'$.modified_unix','$.content_version'); UPDATE observed SET body=json_remove(body,'$.modified_unix','$.content_version');").unwrap();
    let ticket = db.node_observation(&scope(), "a").unwrap();
    assert_eq!(
        begin(&path, "root")
            .page(page(vec![item.clone()], None))
            .unwrap()
            .publish()
            .unwrap(),
        Published::Published { changed: false }
    );
    assert!(
        matches!(db.publish_absence(&ticket).unwrap(),cirrove_store::AbsenceResult::Superseded(Some(n)) if n==item)
    );
}
#[test]
fn staged_and_legacy_publication_agree_through_repeated_moves_and_absence() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("staged.db");
    let mut staged = Store::open(&path).unwrap();
    let mut legacy = Store::open(":memory:").unwrap();
    for round in 0..48 {
        let parent = if round % 2 == 0 { "root" } else { "other" };
        let remote = node(&format!("{}", round % 7), parent, round);
        for db in [&mut staged, &mut legacy] {
            db.observe_node(&scope(), &remote).unwrap();
        }
        if round % 3 == 0 {
            for db in [&mut staged, &mut legacy] {
                delta(db, vec![Change::Upsert(remote.clone())], false);
            }
        }
        let nodes: Vec<_> = (0..7)
            .filter(|i| (i + round) % 3 != 0)
            .map(|i| node(&i.to_string(), parent, round))
            .collect();
        let ticket = legacy.directory_observation(&scope(), parent).unwrap();
        let expected = legacy.publish_directory(&ticket, &nodes).unwrap();
        let result = begin(&path, parent)
            .page(page(nodes, None))
            .unwrap()
            .publish()
            .unwrap();
        assert!(
            matches!((result,expected),(Published::Published{changed:a},ObservationResult::Published{changed:b,..}) if a==b)
        );
        for parent in ["root", "other"] {
            assert_eq!(
                staged.children(&scope(), parent).unwrap(),
                legacy.children(&scope(), parent).unwrap()
            );
        }
        for i in 0..7 {
            assert_eq!(
                staged.node(&scope(), &i.to_string()).unwrap(),
                legacy.node(&scope(), &i.to_string()).unwrap()
            );
        }
        assert_eq!(
            staged.cursor(&scope()).unwrap(),
            legacy.cursor(&scope()).unwrap()
        );
    }
}

#[test]
fn equal_names_keep_identity_order_and_other_accounts_remain_independent() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("metadata.db");
    let mut db = Store::open(&path).unwrap();
    let mut other = scope();
    other.account = "other-account".into();
    db.observe_directory(&other, "root", &[node("private", "root", 1)])
        .unwrap();
    let mut a = node("a", "root", 1);
    let mut b = node("b", "root", 1);
    a.name = "Übersicht.txt".into();
    b.name = a.name.clone();
    begin(&path, "root")
        .page(page(vec![b.clone()], Some("next")))
        .unwrap()
        .page(page(vec![a.clone()], None))
        .unwrap()
        .publish()
        .unwrap();
    assert_eq!(
        db.children(&scope(), "root").unwrap().unwrap(),
        vec![a.clone(), b]
    );
    assert_eq!(db.child(&scope(), "root", &a.name).unwrap(), Some(Some(a)));
    assert_eq!(
        db.children(&other, "root").unwrap().unwrap(),
        vec![node("private", "root", 1)]
    );
}
