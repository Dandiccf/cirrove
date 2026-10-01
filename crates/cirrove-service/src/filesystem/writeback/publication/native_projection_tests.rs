#![allow(clippy::unwrap_used)]
use super::*;
use crate::journal::{NamespaceNames, NamespaceSnapshot, NativeArchiveRole};
use cirrove_core::upload::UploadIntent;
fn pair() -> (NamespaceObject, NamespaceObject, WorkingFile) {
    let scope = Scope {
        account: "fixture".into(),
        provider: "icloud".into(),
        collection: "com.apple.CloudDocs".into(),
    };
    let owner_id = Uuid::new_v4();
    let file_id = Uuid::new_v4();
    let remote = Node {
        id: "FILE::com.apple.CloudDocs::original".into(),
        parent_id: Some("root".into()),
        name: "Owned.pages".into(),
        kind: NodeKind::Folder,
        size: 17,
        modified_unix: 0,
        etag: Some("v1".into()),
        content_version: None,
        target: None,
        package: true,
    };
    let owner = NamespaceObject {
        native_archive: None,
        id: owner_id,
        scope: scope.clone(),
        names: NamespaceNames::Sensitive,
        node: Node {
            id: format!("local-native-{owner_id}"),
            ..remote.clone()
        },
        remote: Some(remote.clone()),
        remote_owned: true,
        remote_sequence: 0,
        working_file: None,
        latest: None,
        revision: 1,
        follows_remote: false,
        unlinked: false,
    };
    let node = Node {
        id: format!("local-native-archive-{file_id}"),
        parent_id: Some(owner.node.id.clone()),
        name: remote.name.clone(),
        kind: NodeKind::File,
        size: 101,
        modified_unix: 0,
        etag: None,
        content_version: Some(format!("working-{file_id}")),
        target: None,
        package: false,
    };
    let working = WorkingFile {
        id: file_id,
        scope: scope.clone(),
        node: node.clone(),
        intent: UploadIntent::Replace {
            item: remote.id.clone(),
            expected_etag: "v1".into(),
        },
        latest: None,
        dirty: true,
        generation: 1,
        initial_remote: None,
        unlinked: false,
        native: true,
    };
    let child = NamespaceObject {
        native_archive: Some(NativeArchiveRole {
            retired: false,
            source_owner: owner.id,
            working: file_id,
            artifact: format!("icloud-artifact:{}", remote.id),
        }),
        id: file_id,
        scope,
        names: owner.names,
        node,
        remote: None,
        remote_owned: false,
        remote_sequence: 0,
        working_file: Some(file_id),
        latest: None,
        revision: 1,
        follows_remote: false,
        unlinked: false,
    };
    (owner, child, working)
}
fn batch(
    owner: NamespaceObject,
    child: NamespaceObject,
    working: WorkingFile,
    after: u64,
    through: u64,
) -> NamespacePublication {
    // Deliberately child first: consistency must not depend on UUID iteration.
    NamespacePublication {
        after,
        through,
        objects: vec![
            NamespaceSnapshot {
                native_local: None,
                object: child,
                working: Some(working),
            },
            NamespaceSnapshot {
                native_local: None,
                object: owner,
                working: None,
            },
        ],
    }
}
#[test]
fn native_projection_batch_is_atomic_and_stale_callback_cannot_roll_back_archive() {
    let (mut owner, mut child, working) = pair();
    let mut projection = Projection::default();
    projection
        .publish_batch(batch(owner.clone(), child.clone(), working.clone(), 0, 2))
        .unwrap();
    let old = batch(owner.clone(), child.clone(), working.clone(), 0, 2);
    owner.remote.as_mut().unwrap().id = "FILE::com.apple.CloudDocs::replacement".into();
    owner.remote.as_mut().unwrap().etag = Some("v2".into());
    owner.remote_sequence = 7;
    owner.revision = 2;
    child.native_archive.as_mut().unwrap().artifact =
        format!("icloud-artifact:{}", owner.remote.as_ref().unwrap().id);
    child.revision = 2;
    projection
        .publish_batch(batch(owner.clone(), child.clone(), working.clone(), 2, 4))
        .unwrap();
    assert!(!projection.publish_batch(old).unwrap());
    assert_eq!(
        projection.objects[&child.id].native_archive,
        child.native_archive
    );
    assert_eq!(projection.files[&working.id].generation, 1);
    assert!(projection.files[&working.id].dirty);
    assert_eq!(projection.files[&working.id].node.size, 101);
    assert!(projection.objects[&child.id].latest.is_none());
    let mut next_owner = owner.clone();
    next_owner.remote.as_mut().unwrap().id = "FILE::com.apple.CloudDocs::third".into();
    next_owner.revision = 3;
    let incomplete = NamespacePublication {
        after: 4,
        through: 5,
        objects: vec![NamespaceSnapshot {
            native_local: None,
            object: next_owner,
            working: None,
        }],
    };
    assert!(
        projection.publish_batch(incomplete).is_err(),
        "source moved without its derived child"
    );
    assert_eq!(projection.frontier, 4);
    assert_eq!(projection.objects[&owner.id].remote, owner.remote);
}
#[test]
fn native_projection_refuses_wrong_owner_scope_spoof_and_ordinary_role_downgrade() {
    for arm in 0..5 {
        let (owner, mut child, mut working) = pair();
        match arm {
            0 => child.native_archive.as_mut().unwrap().source_owner = Uuid::new_v4(),
            1 => {
                child.scope.account = "foreign".into();
                working.scope = child.scope.clone();
            }
            2 => child.native_archive.as_mut().unwrap().artifact = "preview-spoof".into(),
            3 => child.latest = Some(Uuid::new_v4()),
            _ => {
                child.native_archive = None;
                child.remote_owned = true;
            }
        }
        let mut projection = Projection::default();
        assert!(
            projection
                .publish_batch(batch(owner, child, working, 0, 2))
                .is_err(),
            "arm {arm}"
        );
        assert!(projection.objects.is_empty());
        assert_eq!(projection.frontier, 0);
    }
}

#[test]
fn native_projection_retirement_and_reactivation_require_paired_monotonic_publication() {
    let (mut owner, mut child, mut working) = pair();
    let mut projection = Projection::default();
    projection
        .publish_batch(batch(owner.clone(), child.clone(), working.clone(), 0, 2))
        .unwrap();
    let old = batch(owner.clone(), child.clone(), working.clone(), 0, 2);
    owner.follows_remote = true;
    owner.revision += 1;
    child.unlinked = true;
    child.working_file = None;
    child.revision += 1;
    child.native_archive.as_mut().unwrap().retired = true;
    let incomplete = NamespacePublication {
        after: 2,
        through: 4,
        objects: vec![NamespaceSnapshot {
            native_local: None,
            object: child.clone(),
            working: None,
        }],
    };
    assert!(projection.publish_batch(incomplete).is_err());
    assert_eq!(projection.frontier, 2);
    projection
        .publish_batch(NamespacePublication {
            after: 2,
            through: 4,
            objects: vec![
                NamespaceSnapshot {
                    native_local: None,
                    object: child.clone(),
                    working: None,
                },
                NamespaceSnapshot {
                    native_local: None,
                    object: owner.clone(),
                    working: None,
                },
            ],
        })
        .unwrap();
    assert!(!projection.files.contains_key(&working.id));
    assert!(projection.objects[&child.id].unlinked);
    owner.follows_remote = false;
    owner.revision += 1;
    child.unlinked = false;
    child.working_file = Some(working.id);
    child.revision += 1;
    child.native_archive.as_mut().unwrap().retired = false;
    working.generation += 1;
    projection
        .publish_batch(batch(owner, child.clone(), working.clone(), 4, 6))
        .unwrap();
    assert!(!projection.publish_batch(old).unwrap());
    assert_eq!(projection.files[&working.id].generation, working.generation);
    assert_eq!(projection.objects[&child.id].revision, child.revision);
    assert_eq!(projection.native_archives.len(), 1);
}

#[test]
fn native_atomic_projection_requires_complete_paired_roles_and_preserves_old_uuid() {
    let (owner, old, old_file) = pair();
    let mut p = Projection::default();
    p.publish_batch(batch(owner.clone(), old.clone(), old_file.clone(), 0, 1))
        .unwrap();
    let new_id = Uuid::new_v4();
    let mut temp_file = old_file.clone();
    temp_file.id = new_id;
    temp_file.node.id = format!("local-native-archive-{new_id}");
    temp_file.node.name = ".save".into();
    let mut temp = old.clone();
    temp.id = new_id;
    temp.node = temp_file.node.clone();
    temp.working_file = Some(new_id);
    temp.native_archive = None;
    let temp_role = crate::journal::NativeLocalStream {
        source_owner: owner.id,
        detached: false,
    };
    p.publish_batch(NamespacePublication {
        after: 1,
        through: 2,
        objects: vec![NamespaceSnapshot {
            object: temp.clone(),
            working: Some(temp_file.clone()),
            native_local: Some(temp_role),
        }],
    })
    .unwrap();
    let mut detached = old.clone();
    detached.unlinked = true;
    detached.native_archive = None;
    detached.revision += 1;
    let mut detached_file = old_file;
    detached_file.unlinked = true;
    let detached_role = crate::journal::NativeLocalStream {
        source_owner: owner.id,
        detached: true,
    };
    let mut current = temp;
    current.revision += 1;
    current.node.name = owner.node.name.clone();
    current.native_archive = old.native_archive.clone();
    current.native_archive.as_mut().unwrap().working = new_id;
    temp_file.node = current.node.clone();
    let missing = NamespacePublication {
        after: 2,
        through: 3,
        objects: vec![NamespaceSnapshot {
            object: detached.clone(),
            working: Some(detached_file.clone()),
            native_local: Some(detached_role.clone()),
        }],
    };
    assert!(p.publish_batch(missing).is_err());
    assert_eq!(p.frontier, 2);
    assert_eq!(p.native_archives[&owner.id], old.id);
    let full = NamespacePublication {
        after: 2,
        through: 4,
        objects: vec![
            NamespaceSnapshot {
                object: current.clone(),
                working: Some(temp_file),
                native_local: None,
            },
            NamespaceSnapshot {
                object: detached,
                working: Some(detached_file),
                native_local: Some(detached_role),
            },
        ],
    };
    p.publish_batch(full).unwrap();
    assert_eq!(p.native_archives[&owner.id], new_id);
    assert!(p.files[&old.id].unlinked);
    assert!(!p.files[&new_id].unlinked);
    assert_eq!(p.objects[&new_id].node.name, "Owned.pages");
}
