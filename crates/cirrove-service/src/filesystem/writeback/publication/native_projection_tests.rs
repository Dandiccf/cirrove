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
                object: child,
                working: Some(working),
            },
            NamespaceSnapshot {
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
