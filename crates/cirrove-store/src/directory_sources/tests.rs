#![allow(clippy::unwrap_used)]
use super::*;
use cirrove_core::{CancellationToken, Checkpoint, DirectoryPage, NodeKind};
use std::time::{Duration, Instant};
fn source(version: u64) -> Node {
    Node {
        id: "package".into(),
        parent_id: Some("root".into()),
        name: "Own.pages".into(),
        kind: NodeKind::Folder,
        size: version,
        etag: Some(format!("v{version}")),
        content_version: None,
        modified_unix: 0,
        target: None,
        package: true,
    }
}
fn child(version: u64) -> Node {
    Node {
        id: format!("artifact-{version}"),
        parent_id: Some("package".into()),
        kind: NodeKind::File,
        package: false,
        ..source(version)
    }
}
fn fixture() -> (tempfile::TempDir, Scope, Store) {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open(root.path().join("metadata.db")).unwrap();
    let scope = Scope {
        account: "account".into(),
        provider: "fixture".into(),
        collection: "drive".into(),
    };
    let cursor = store.begin(&scope, false).unwrap();
    store
        .stage(
            &scope,
            cursor.as_ref(),
            &ChangePage {
                changes: vec![Change::Upsert(source(1))],
                checkpoint: Checkpoint::Complete(Cursor("initial".into())),
            },
        )
        .unwrap();
    (root, scope, store)
}
fn stage(root: &Path, scope: &Scope, version: u64) -> DirectoryPublication {
    Store::open(root.join("metadata.db"))
        .unwrap()
        .directory_publication(
            scope,
            "package",
            CancellationToken::new(),
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap()
        .bind_source(source(version))
        .unwrap()
        .page(DirectoryPage {
            nodes: vec![child(version)],
            next: None,
        })
        .unwrap()
}
#[test]
fn package_source_direct_parent_and_delta_observations_keep_old_snapshot_until_replaced() {
    for route in 0..4 {
        let (root, scope, mut store) = fixture();
        store.observe_node(&scope, &source(1)).unwrap();
        stage(root.path(), &scope, 1).publish().unwrap();
        match route {
            0 => {
                let ticket = store.node_observation(&scope, "package").unwrap();
                store.publish_node(&ticket, &source(2)).unwrap();
            }
            1 => {
                let ticket = store.directory_observation(&scope, "root").unwrap();
                store.publish_directory(&ticket, &[source(2)]).unwrap();
            }
            _ => {
                let cursor = store.begin(&scope, route == 3).unwrap();
                store
                    .stage(
                        &scope,
                        cursor.as_ref(),
                        &ChangePage {
                            changes: vec![Change::Upsert(source(2))],
                            checkpoint: Checkpoint::Complete(Cursor("done".into())),
                        },
                    )
                    .unwrap();
            }
        }
        let state = store
            .directory_source_state(&scope, "package")
            .unwrap()
            .unwrap();
        assert!(state.changed, "route {route}");
        assert_eq!(state.bound, Some(source(1)));
        assert_eq!(state.current, Some(source(2)));
        assert_eq!(
            store.children(&scope, "package").unwrap().unwrap(),
            vec![child(1)]
        );
        stage(root.path(), &scope, 2).publish().unwrap();
        assert!(
            !store
                .directory_source_state(&scope, "package")
                .unwrap()
                .unwrap()
                .changed
        );
        assert_eq!(
            store.children(&scope, "package").unwrap().unwrap(),
            vec![child(2)]
        );
    }
}
#[test]
fn package_source_publication_cas_rejects_inflight_old_generation_atomically() {
    for change_before_ticket in [true, false] {
        let (root, scope, mut store) = fixture();
        store.observe_node(&scope, &source(1)).unwrap();
        stage(root.path(), &scope, 1).publish().unwrap();
        // The source was captured before the builder's observation ticket.
        // Ticket ordering alone cannot reject this first arm: its ticket is
        // newer than the changed source, but its converter still used v1.
        if change_before_ticket {
            store.observe_node(&scope, &source(2)).unwrap();
        }
        let pending = stage(root.path(), &scope, 1);
        if !change_before_ticket {
            store.observe_node(&scope, &source(2)).unwrap();
        }
        assert_eq!(
            pending.publish().unwrap(),
            DirectoryPublicationResult::SourceChanged
        );
        assert_eq!(
            store.children(&scope, "package").unwrap().unwrap(),
            vec![child(1)]
        );
        let state = store
            .directory_source_state(&scope, "package")
            .unwrap()
            .unwrap();
        assert_eq!(state.bound, Some(source(1)));
        assert!(state.changed);
    }
}
#[test]
fn package_source_missing_invalid_and_content_tag_changes_are_distinct() {
    let (root, scope, mut store) = fixture();
    let mut original = source(1);
    original.content_version = Some("content-1".into());
    store.observe_node(&scope, &original).unwrap();
    Store::open(root.path().join("metadata.db"))
        .unwrap()
        .directory_publication(
            &scope,
            "package",
            CancellationToken::new(),
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap()
        .bind_source(original.clone())
        .unwrap()
        .page(DirectoryPage {
            nodes: vec![child(1)],
            next: None,
        })
        .unwrap()
        .publish()
        .unwrap();
    let mut metadata = original.clone();
    metadata.etag = Some("metadata-new".into());
    metadata.modified_unix = 9;
    store.observe_node(&scope, &metadata).unwrap();
    assert!(
        !store
            .directory_source_state(&scope, "package")
            .unwrap()
            .unwrap()
            .changed
    );
    metadata.kind = NodeKind::File;
    store.observe_node(&scope, &metadata).unwrap();
    assert!(
        store
            .directory_source_state(&scope, "package")
            .unwrap()
            .unwrap()
            .changed
    );
    let ticket = store.node_observation(&scope, "package").unwrap();
    store.publish_absence(&ticket).unwrap();
    let state = store
        .directory_source_state(&scope, "package")
        .unwrap()
        .unwrap();
    assert!(state.changed);
    assert!(state.current.is_none());
    assert_eq!(
        store.children(&scope, "package").unwrap().unwrap(),
        vec![child(1)]
    );
}
#[test]
fn package_source_schema7_migration_retains_legacy_children_without_inventing_binding() {
    let (root, scope, mut store) = fixture();
    store.observe_node(&scope, &source(1)).unwrap();
    store
        .observe_directory(&scope, "package", &[child(1)])
        .unwrap();
    store
        .db
        .execute_batch("DROP TABLE directory_sources; PRAGMA user_version=7;")
        .unwrap();
    drop(store);
    let mut store = Store::open(root.path().join("metadata.db")).unwrap();
    assert_eq!(schema_version(root.path().join("metadata.db")).unwrap(), 8);
    let state = store
        .directory_source_state(&scope, "package")
        .unwrap()
        .unwrap();
    assert!(!state.changed);
    assert!(state.bound.is_none());
    store.observe_node(&scope, &source(2)).unwrap();
    assert!(
        store
            .directory_source_state(&scope, "package")
            .unwrap()
            .unwrap()
            .changed
    );
    assert_eq!(
        store.children(&scope, "package").unwrap().unwrap(),
        vec![child(1)]
    );
    store
        .db
        .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
        .unwrap();
    drop(store);
    assert!(matches!(
        Store::open(root.path().join("metadata.db")),
        Err(StoreError::SchemaVersion)
    ));
}

#[test]
fn package_source_legacy_classification_survives_deletion_and_reclassification_without_revision_authority()
 {
    for reclassify in [false, true] {
        let (root, scope, mut store) = fixture();
        store
            .observe_directory(&scope, "package", &[child(1)])
            .unwrap();
        store
            .db
            .execute_batch("DROP TABLE directory_sources; PRAGMA user_version=7;")
            .unwrap();
        drop(store);
        let mut store = Store::open(root.path().join("metadata.db")).unwrap();
        let initial = store
            .directory_source_state(&scope, "package")
            .unwrap()
            .unwrap();
        assert!(initial.bound.is_none());
        assert_eq!(initial.legacy_classification, Some(source(1)));
        assert!(!initial.changed);
        if reclassify {
            let mut ordinary = source(2);
            ordinary.package = false;
            ordinary.kind = NodeKind::File;
            store.observe_node(&scope, &ordinary).unwrap();
        } else {
            let ticket = store.node_observation(&scope, "package").unwrap();
            store.publish_absence(&ticket).unwrap();
        }
        let changed = store
            .directory_source_state(&scope, "package")
            .unwrap()
            .unwrap();
        assert!(changed.changed);
        assert!(changed.bound.is_none());
        assert_eq!(changed.legacy_classification, Some(source(1)));
        assert_eq!(
            store.children(&scope, "package").unwrap().unwrap(),
            vec![child(1)]
        );
        store.observe_node(&scope, &source(2)).unwrap();
        stage(root.path(), &scope, 2).publish().unwrap();
        let rebound = store
            .directory_source_state(&scope, "package")
            .unwrap()
            .unwrap();
        assert_eq!(rebound.bound, Some(source(2)));
        assert!(rebound.legacy_classification.is_none());
        assert!(!rebound.changed);
    }
}
#[test]
fn package_source_migration_absence_uses_only_retained_classification_not_guessed_children() {
    for retained_identity in [true, false] {
        let (root, scope, mut store) = fixture();
        store
            .observe_directory(&scope, "package", &[child(1)])
            .unwrap();
        let ticket = store.node_observation(&scope, "package").unwrap();
        store.publish_absence(&ticket).unwrap();
        if !retained_identity {
            store
                .db
                .execute("DELETE FROM nodes WHERE id='package'", [])
                .unwrap();
        }
        store
            .db
            .execute_batch("DROP TABLE directory_sources; PRAGMA user_version=7;")
            .unwrap();
        drop(store);
        let store = Store::open(root.path().join("metadata.db")).unwrap();
        let state = store
            .directory_source_state(&scope, "package")
            .unwrap()
            .unwrap();
        assert!(state.bound.is_none());
        assert!(state.current.is_none());
        assert!(state.changed);
        assert_eq!(state.legacy_classification.is_some(), retained_identity);
        // An archive-looking child is not evidence of a generated parent. If
        // all prior source metadata is gone, migration leaves it unclassified.
        assert_eq!(
            store.children(&scope, "package").unwrap().unwrap(),
            vec![child(1)]
        );
    }
    let (root, scope, mut store) = fixture();
    let mut ordinary = source(1);
    ordinary.package = false;
    store.observe_node(&scope, &ordinary).unwrap();
    store
        .observe_directory(&scope, "package", &[child(1)])
        .unwrap();
    store
        .db
        .execute_batch("DROP TABLE directory_sources; PRAGMA user_version=7;")
        .unwrap();
    drop(store);
    let store = Store::open(root.path().join("metadata.db")).unwrap();
    let state = store
        .directory_source_state(&scope, "package")
        .unwrap()
        .unwrap();
    assert!(state.bound.is_none());
    assert!(state.legacy_classification.is_none());
    assert!(!state.changed);
}
