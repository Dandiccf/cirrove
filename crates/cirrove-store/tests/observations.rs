//! Network observations can finish after newer metadata has already committed.
#![allow(clippy::unwrap_used)]
use cirrove_core::{Change, ChangePage, Checkpoint, Cursor, Node, NodeKind, Scope};
use cirrove_store::{ObservationResult, Store, StoreError};
fn scope(collection: &str) -> Scope {
    Scope {
        account: "fixture".into(),
        provider: "fixture".into(),
        collection: collection.into(),
    }
}

#[test]
fn ordered_not_found_hides_cached_entries_without_deleting_the_feed_baseline() {
    let mut db = Store::open(":memory:").unwrap();
    let s = scope("drive");
    let item = node("gone", "root", "old");
    let other = node("keep", "root", "one");
    delta(
        &mut db,
        &s,
        vec![Change::Upsert(item.clone()), Change::Upsert(other.clone())],
        false,
    );
    db.observe_directory(&s, "root", &[item.clone(), other.clone()])
        .unwrap();
    let old = db.node_observation(&s, &item.id).unwrap();
    let missing = db.node_observation(&s, &item.id).unwrap();
    assert!(matches!(
        db.publish_absence(&missing).unwrap(),
        cirrove_store::AbsenceResult::Published { changed: true }
    ));
    assert!(db.node(&s, &item.id).unwrap().is_none());
    assert_eq!(db.nodes(&s).unwrap().len(), 2);
    assert_eq!(db.children(&s, "root").unwrap().unwrap(), vec![other]);
    assert!(matches!(
        db.publish_node(&old, &item).unwrap(),
        ObservationResult::Superseded(None)
    ));
    let missing = db.node_observation(&s, &item.id).unwrap();
    let newer = node("gone", "root", "new");
    delta(&mut db, &s, vec![Change::Upsert(newer.clone())], false);
    assert!(
        matches!(db.publish_absence(&missing).unwrap(),cirrove_store::AbsenceResult::Superseded(Some(n)) if n==newer)
    );
}
#[test]
fn even_an_unchanged_listing_supersedes_an_earlier_not_found_reply() {
    let mut db = Store::open(":memory:").unwrap();
    let s = scope("drive");
    let item = node("present", "root", "one");
    db.observe_directory(&s, "root", std::slice::from_ref(&item))
        .unwrap();
    let missing = db.node_observation(&s, &item.id).unwrap();
    assert!(
        !db.observe_directory(&s, "root", std::slice::from_ref(&item))
            .unwrap()
    );
    assert!(
        matches!(db.publish_absence(&missing).unwrap(),cirrove_store::AbsenceResult::Superseded(Some(n)) if n==item)
    );
}
#[test]
fn failed_not_found_publication_rolls_back_absence_and_supersession_together() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("metadata.db");
    let mut db = Store::open(&path).unwrap();
    let s = scope("drive");
    let item = node("a", "root", "one");
    db.observe_directory(&s, "root", std::slice::from_ref(&item))
        .unwrap();
    let previous = db.node_observation(&s, &item.id).unwrap();
    let missing = db.node_observation(&s, &item.id).unwrap();
    let injected = rusqlite::Connection::open(&path).unwrap();
    injected.execute_batch("CREATE TRIGGER deny_absence BEFORE INSERT ON observed_absent BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(db.publish_absence(&missing).is_err());
    assert_eq!(db.node(&s, &item.id).unwrap(), Some(item.clone()));
    assert!(matches!(
        db.publish_node(&previous, &item).unwrap(),
        ObservationResult::Published { .. }
    ));
}
fn node(id: &str, parent: &str, version: &str) -> Node {
    Node {
        id: id.into(),
        parent_id: Some(parent.into()),
        name: id.into(),
        kind: NodeKind::File,
        size: 5,
        modified_unix: 1,
        etag: Some(version.into()),
        content_version: Some(version.into()),
        target: None,
    }
}
fn page(changes: Vec<Change>, complete: bool, cursor: &str) -> ChangePage {
    ChangePage {
        changes,
        checkpoint: if complete {
            Checkpoint::Complete(Cursor(cursor.into()))
        } else {
            Checkpoint::Continue(Cursor(cursor.into()))
        },
    }
}
fn delta(store: &mut Store, scope: &Scope, changes: Vec<Change>, reset: bool) {
    let cursor = store.begin(scope, reset).unwrap();
    store
        .stage(scope, cursor.as_ref(), &page(changes, true, "done"))
        .unwrap();
}
#[test]
fn newer_directory_wins_without_invalidating_unrelated_work() {
    let mut db = Store::open(":memory:").unwrap();
    let scope = scope("drive");
    let old = node("a", "first", "one");
    let separate = node("b", "second", "one");
    db.observe_directory(&scope, "first", std::slice::from_ref(&old))
        .unwrap();
    db.observe_directory(&scope, "second", std::slice::from_ref(&separate))
        .unwrap();
    let first = db.directory_observation(&scope, "first").unwrap();
    let second = db.directory_observation(&scope, "second").unwrap();
    let item = db.node_observation(&scope, "a").unwrap();
    let new = node("a", "first", "two");
    db.observe_directory(&scope, "first", std::slice::from_ref(&new))
        .unwrap();
    assert!(
        matches!(db.publish_directory(&first,std::slice::from_ref(&old)).unwrap(),ObservationResult::Superseded(Some(n)) if n==vec![new.clone()])
    );
    assert!(
        matches!(db.publish_node(&item,&old).unwrap(),ObservationResult::Superseded(Some(n)) if n==new)
    );
    assert!(matches!(
        db.publish_directory(&second, &[separate]).unwrap(),
        ObservationResult::Published { changed: false, .. }
    ));
}
#[test]
fn move_protects_old_and_new_parents_and_the_item_version() {
    let mut db = Store::open(":memory:").unwrap();
    let a = scope("a");
    let b = scope("b");
    let old = node("same-id", "old", "one");
    delta(&mut db, &a, vec![Change::Upsert(old.clone())], false);
    let old_parent = db.directory_observation(&a, "old").unwrap();
    let new_parent = db.directory_observation(&a, "new").unwrap();
    let item = db.node_observation(&a, &old.id).unwrap();
    let other_drive = db.directory_observation(&b, "old").unwrap();
    let moved = node("same-id", "new", "two");
    delta(&mut db, &a, vec![Change::Upsert(moved.clone())], false);
    assert!(
        matches!(db.publish_directory(&old_parent,std::slice::from_ref(&old)).unwrap(),ObservationResult::Superseded(Some(n)) if n.is_empty())
    );
    assert!(
        matches!(db.publish_directory(&new_parent,&[]).unwrap(),ObservationResult::Superseded(Some(n)) if n==vec![moved.clone()])
    );
    assert!(
        matches!(db.publish_node(&item,&old).unwrap(),ObservationResult::Superseded(Some(n)) if n==moved)
    );
    assert!(matches!(
        db.publish_directory(&other_drive, &[old]).unwrap(),
        ObservationResult::Published { .. }
    ));
}
#[test]
fn a_cold_deleted_item_is_not_resurrected_even_without_a_known_parent() {
    let mut db = Store::open(":memory:").unwrap();
    let scope = scope("drive");
    delta(&mut db, &scope, vec![], false);
    let request = db.directory_observation(&scope, "unknown-parent").unwrap();
    let independent = db.directory_observation(&scope, "other").unwrap();
    delta(
        &mut db,
        &scope,
        vec![Change::Delete {
            id: "never-indexed".into(),
        }],
        false,
    );
    assert!(
        matches!(db.publish_directory(&request,&[node("never-indexed","unknown-parent","old")]).unwrap(),ObservationResult::Superseded(Some(n)) if n.is_empty())
    );
    assert!(matches!(
        db.publish_directory(&independent, &[node("other-file", "other", "one")])
            .unwrap(),
        ObservationResult::Published { .. }
    ));
}
#[test]
fn empty_delta_preserves_tickets_while_a_reset_supersedes_the_whole_scope() {
    let mut db = Store::open(":memory:").unwrap();
    let s = scope("drive");
    let item = node("a", "root", "one");
    delta(&mut db, &s, vec![Change::Upsert(item.clone())], false);
    let request = db.directory_observation(&s, "root").unwrap();
    delta(&mut db, &s, vec![], false);
    assert!(matches!(
        db.publish_directory(&request, std::slice::from_ref(&item))
            .unwrap(),
        ObservationResult::Published { changed: false, .. }
    ));
    let request = db.directory_observation(&s, "root").unwrap();
    delta(&mut db, &s, vec![], true);
    assert!(
        matches!(db.publish_directory(&request,&[item]).unwrap(),ObservationResult::Superseded(Some(n)) if n.is_empty())
    );
}
#[test]
fn request_start_not_completion_orders_an_observation_against_a_pending_delta() {
    for starts_first in [false, true] {
        let mut db = Store::open(":memory:").unwrap();
        let s = scope("drive");
        let old = node("a", "root", "old");
        let new = node("a", "root", "new");
        let request = if starts_first {
            Some(db.directory_observation(&s, "root").unwrap())
        } else {
            None
        };
        db.begin(&s, false).unwrap();
        db.stage(
            &s,
            None,
            &page(vec![Change::Upsert(new.clone())], false, "next"),
        )
        .unwrap();
        let request = request.unwrap_or_else(|| db.directory_observation(&s, "root").unwrap());
        assert!(matches!(
            db.publish_directory(&request, std::slice::from_ref(&old))
                .unwrap(),
            ObservationResult::Published { .. }
        ));
        db.stage(
            &s,
            Some(&Cursor("next".into())),
            &page(vec![], true, "done"),
        )
        .unwrap();
        // The older request must yield to a later-started complete feed, even
        // though its reply arrived while that feed was in progress.
        let expected = if starts_first { new } else { old };
        assert_eq!(
            db.children(&s, "root").unwrap().unwrap(),
            vec![expected.clone()]
        );
        assert_eq!(db.node(&s, "a").unwrap(), Some(expected));
    }
}
#[test]
fn publication_failure_rolls_back_metadata_and_supersession_markers() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("metadata.db");
    let mut db = Store::open(&path).unwrap();
    let s = scope("drive");
    let old = node("a", "root", "one");
    db.observe_directory(&s, "root", std::slice::from_ref(&old))
        .unwrap();
    let request = db.directory_observation(&s, "root").unwrap();
    let injected = rusqlite::Connection::open(&path).unwrap();
    injected.execute_batch("CREATE TRIGGER fail_observation BEFORE INSERT ON observed BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(
        db.observe_directory(&s, "root", &[node("a", "root", "two")])
            .is_err()
    );
    injected
        .execute_batch("DROP TRIGGER fail_observation;")
        .unwrap();
    assert!(matches!(
        db.publish_directory(&request, &[old]).unwrap(),
        ObservationResult::Published { changed: false, .. }
    ));
}
#[test]
fn migration_preserves_legacy_ordering_and_tickets_survive_connection_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("metadata.db");
    let s = scope("drive");
    let old = node("a", "root", "old");
    let observed = node("a", "root", "observed");
    {
        let mut db = Store::open(&path).unwrap();
        db.begin(&s, false).unwrap();
        db.observe_directory(&s, "root", std::slice::from_ref(&observed))
            .unwrap();
    }
    {
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch(
            "ALTER TABLE directories ADD COLUMN body TEXT NOT NULL DEFAULT '[]';
            UPDATE directories SET body=(SELECT json_group_array(json(e.body)) FROM directory_entries e WHERE e.scope=directories.scope AND e.parent=directories.parent);
            DROP TABLE directory_entries; DROP INDEX node_parent_name; DROP INDEX observed_parent_name;
            DROP TABLE metadata_versions; DROP TABLE metadata_clock; DROP TABLE observed_absent; DROP INDEX observed_parent;
            ALTER TABLE observed DROP COLUMN source_revision;
            ALTER TABLE directories DROP COLUMN source_revision;
            UPDATE observed SET seen=20000; UPDATE directories SET seen=20000;
            UPDATE rounds SET started=10000; PRAGMA user_version=3;",
        )
        .unwrap();
    }
    let ticket = {
        let mut db = Store::open(&path).unwrap();
        db.stage(&s, None, &page(vec![Change::Upsert(old)], true, "done"))
            .unwrap();
        assert_eq!(db.node(&s, "a").unwrap(), Some(observed.clone()));
        db.directory_observation(&s, "root").unwrap()
    };
    let mut db = Store::open(&path).unwrap();
    assert!(matches!(
        db.publish_directory(&ticket, &[observed]).unwrap(),
        ObservationResult::Published { changed: false, .. }
    ));
    delta(&mut db, &s, vec![Change::Delete { id: "a".into() }], false);
    assert!(db.node(&s, "a").unwrap().is_none());
    assert!(db.children(&s, "root").unwrap().unwrap().is_empty());
    let mut unrelated = Store::open(":memory:").unwrap();
    assert!(matches!(
        unrelated.publish_directory(&ticket, &[]),
        Err(StoreError::OutOfOrder)
    ));
    rusqlite::Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", 7)
        .unwrap();
    assert!(matches!(Store::open(&path), Err(StoreError::SchemaVersion)));
}
#[test]
fn unchanged_sibling_directories_do_not_cancel_their_own_content_requests() {
    let mut db = Store::open(":memory:").unwrap();
    let s = scope("drive");
    let folder = Node {
        kind: NodeKind::Folder,
        ..node("folder", "root", "one")
    };
    let sibling = node("sibling", "root", "one");
    db.observe_directory(&s, "root", &[folder.clone(), sibling])
        .unwrap();
    let request = db.directory_observation(&s, "folder").unwrap();
    db.observe_directory(&s, "root", &[folder, node("sibling", "root", "two")])
        .unwrap();
    assert!(matches!(
        db.publish_directory(&request, &[node("child", "folder", "one")])
            .unwrap(),
        ObservationResult::Published { .. }
    ));
}

#[test]
fn newer_item_observations_update_cached_directory_names_and_both_move_locations() {
    for cached in [false, true] {
        let mut db = Store::open(":memory:").unwrap();
        let s = scope("drive");
        let old = node("a", "source", "one");
        delta(&mut db, &s, vec![Change::Upsert(old.clone())], false);
        if cached {
            db.observe_directory(&s, "source", std::slice::from_ref(&old))
                .unwrap();
            db.observe_directory(&s, "destination", &[]).unwrap();
        }
        let mut renamed = old.clone();
        renamed.name = "renamed.txt".into();
        renamed.size = 9;
        renamed.etag = Some("renamed".into());
        db.observe_node(&s, &renamed).unwrap();
        assert_eq!(
            db.children(&s, "source").unwrap().unwrap(),
            vec![renamed.clone()]
        );
        renamed.parent_id = Some("destination".into());
        db.observe_node(&s, &renamed).unwrap();
        assert!(db.children(&s, "source").unwrap().unwrap().is_empty());
        assert_eq!(
            db.children(&s, "destination").unwrap().unwrap(),
            vec![renamed.clone()]
        );
        // Refreshing the old source does not erase the known new location.
        db.observe_directory(&s, "source", &[]).unwrap();
        assert_eq!(db.node(&s, "a").unwrap(), Some(renamed));
    }
}
#[test]
fn complete_directory_removal_invalidates_item_cache_without_touching_other_parents() {
    let mut db = Store::open(":memory:").unwrap();
    let s = scope("drive");
    let gone = node("gone", "root", "one");
    let keep = node("keep", "other", "one");
    delta(
        &mut db,
        &s,
        vec![Change::Upsert(gone.clone()), Change::Upsert(keep.clone())],
        false,
    );
    db.observe_node(&s, &gone).unwrap();
    let old_lookup = db.node_observation(&s, "gone").unwrap();
    db.observe_directory(&s, "root", &[]).unwrap();
    assert!(db.node(&s, "gone").unwrap().is_none());
    assert_eq!(
        db.nodes(&s).unwrap().len(),
        2,
        "foreground absence must not rewrite the committed delta baseline"
    );
    assert!(matches!(
        db.publish_node(&old_lookup, &gone).unwrap(),
        ObservationResult::Superseded(None)
    ));
    assert_eq!(db.node(&s, "keep").unwrap(), Some(keep));
}

#[test]
fn unchanged_observations_still_supersede_older_contradictory_responses() {
    let mut db = Store::open(":memory:").unwrap();
    let s = scope("drive");
    let item = node("a", "root", "one");
    delta(&mut db, &s, vec![Change::Upsert(item.clone())], false);
    let directory = db.directory_observation(&s, "root").unwrap();
    db.observe_node(&s, &item).unwrap();
    assert!(
        matches!(db.publish_directory(&directory,&[]).unwrap(),ObservationResult::Superseded(Some(n)) if n==vec![item.clone()])
    );
    let lookup = db.node_observation(&s, "a").unwrap();
    assert!(
        !db.observe_directory(&s, "root", std::slice::from_ref(&item))
            .unwrap()
    );
    assert!(
        matches!(db.publish_node(&lookup,&node("a","root","obsolete")).unwrap(),ObservationResult::Superseded(Some(n)) if n==item)
    );
    db.observe_directory(&s, "root", &[]).unwrap();
    let directory = db.directory_observation(&s, "root").unwrap();
    assert!(!db.observe_directory(&s, "root", &[]).unwrap());
    assert!(
        matches!(db.publish_directory(&directory,&[item]).unwrap(),ObservationResult::Superseded(Some(n)) if n.is_empty())
    );
}
#[test]
fn removing_an_individually_cached_item_also_supersedes_its_old_unknown_location() {
    let mut db = Store::open(":memory:").unwrap();
    let s = scope("drive");
    let old = node("a", "old-location", "one");
    let current = node("a", "current-location", "two");
    let request = db.directory_observation(&s, "old-location").unwrap();
    db.observe_node(&s, &current).unwrap();
    assert!(db.children(&s, "current-location").unwrap().is_none());
    db.observe_directory(&s, "current-location", &[]).unwrap();
    assert!(db.node(&s, "a").unwrap().is_none());
    assert!(matches!(
        db.publish_directory(&request, &[old]).unwrap(),
        ObservationResult::Superseded(None)
    ));
}

#[test]
fn negative_observations_follow_request_order_and_yield_to_a_later_feed() {
    for earlier in [false, true] {
        let mut db = Store::open(":memory:").unwrap();
        let s = scope("drive");
        let item = node("a", "root", "one");
        delta(&mut db, &s, vec![Change::Upsert(item.clone())], false);
        let ticket = if earlier {
            Some(db.directory_observation(&s, "root").unwrap())
        } else {
            None
        };
        let cursor = db.begin(&s, false).unwrap();
        let ticket = ticket.unwrap_or_else(|| db.directory_observation(&s, "root").unwrap());
        assert!(matches!(
            db.publish_directory(&ticket, &[]).unwrap(),
            ObservationResult::Published { .. }
        ));
        assert!(db.node(&s, "a").unwrap().is_none());
        db.stage(
            &s,
            cursor.as_ref(),
            &page(vec![Change::Upsert(item.clone())], true, "next"),
        )
        .unwrap();
        assert_eq!(db.node(&s, "a").unwrap().is_some(), earlier);
        assert_eq!(
            !db.children(&s, "root").unwrap().unwrap().is_empty(),
            earlier
        );
        delta(&mut db, &s, vec![Change::Upsert(item.clone())], false);
        assert_eq!(db.node(&s, "a").unwrap(), Some(item.clone()));
        assert_eq!(db.children(&s, "root").unwrap().unwrap(), vec![item]);
    }
}
