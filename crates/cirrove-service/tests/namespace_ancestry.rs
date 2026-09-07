//! Local edit paths survive provider removal without recreating remote folders.
#![allow(clippy::unwrap_used)]
use cirrove_core::{
    Node, NodeKind, Scope,
    mutation::{MutationIntent, MutationReceipt},
};
use cirrove_service::journal::UploadJournal;
fn scope() -> Scope {
    Scope {
        account: "ancestry".into(),
        provider: "fixture".into(),
        collection: "drive".into(),
    }
}
fn folder(id: &str, parent: &str, name: &str) -> Node {
    Node {
        id: id.into(),
        parent_id: Some(parent.into()),
        name: name.into(),
        kind: NodeKind::Folder,
        size: 0,
        modified_unix: 1,
        etag: Some("folder-etag".into()),
        content_version: None,
        target: None,
    }
}
#[test]
fn acknowledged_parent_remains_visible_while_its_child_has_unpublished_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let mut j = UploadJournal::open(&temp.path().join("journal"), &scope().account, 4096).unwrap();
    let parent = j
        .create_namespace_directory(scope(), "root".into(), "saved".into())
        .unwrap();
    let mutation = j.claim_mutation().unwrap().unwrap();
    let MutationIntent::CreateFolder {
        parent: provider_parent,
        name,
    } = &mutation.request.intent
    else {
        panic!("expected folder")
    };
    let remote = folder("remote-folder", provider_parent, name);
    j.acknowledge_mutation(
        mutation.id,
        mutation.attempt.unwrap(),
        MutationReceipt::Upsert(remote.clone()),
    )
    .unwrap();
    let parent = j.namespace_object(parent.id).unwrap();
    j.handoff_namespace(parent.id, parent.revision, remote)
        .unwrap();
    let mut node = folder("", &parent.node.id, "keep.txt");
    node.kind = NodeKind::File;
    node.etag = None;
    let file = j.create_working(scope(), node, true, &b""[..]).unwrap();
    j.write_working(file.id, 0, b"keep my local edit").unwrap();
    j.seal_working(file.id).unwrap();
    // The fresh provider root no longer contains the folder.
    let listing = j.namespace_overlay(&scope(), "root", vec![]).unwrap();
    assert!(
        listing.nodes.iter().any(|n| n.id == parent.node.id),
        "local edits lost their reachable ancestor"
    );
}

fn local_file(
    j: &mut UploadJournal,
    s: Scope,
    parent: &str,
    name: &str,
) -> cirrove_service::journal::UploadRecord {
    let mut n = folder("", parent, name);
    n.kind = NodeKind::File;
    n.etag = None;
    let f = j.create_working(s, n, true, &b""[..]).unwrap();
    j.write_working(f.id, 0, b"local bytes").unwrap();
    j.seal_working(f.id).unwrap().unwrap()
}
#[test]
fn snapshots_protect_linked_paths_across_restart_and_release_after_confirmation() {
    use cirrove_core::{RemoteRef, upload::UploadIntent};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let mut j = UploadJournal::open(&root, &scope().account, 4096).unwrap();
    let mut shared = scope();
    shared.collection = "shared".into();
    let mut link = folder("link", "root", "Documents");
    link.kind = NodeKind::Shortcut;
    link.target = Some(RemoteRef {
        collection: shared.collection.clone(),
        item: "shared-root".into(),
        kind: Some(NodeKind::Folder),
    });
    j.capture_namespace_ancestors(vec![
        (scope(), link),
        (shared.clone(), folder("nested", "shared-root", "Projects")),
    ])
    .unwrap();
    assert!(
        j.namespace_overlay(&scope(), "root", vec![])
            .unwrap()
            .nodes
            .is_empty()
    );
    let save = local_file(&mut j, shared.clone(), "nested", "keep.txt");
    assert_eq!(
        j.namespace_overlay(&scope(), "root", vec![]).unwrap().nodes[0].name,
        "Documents"
    );
    assert!(j.list_mutations(0, 100).unwrap().is_empty());
    let mut other = shared.clone();
    other.collection = "another-drive".into();
    assert!(
        j.namespace_overlay(&other, "shared-root", vec![])
            .unwrap()
            .nodes
            .is_empty()
    );
    drop(j);
    let mut j = UploadJournal::open(&root, &scope().account, 4096).unwrap();
    assert_eq!(
        j.namespace_overlay(&shared, "shared-root", vec![])
            .unwrap()
            .nodes[0]
            .name,
        "Projects"
    );
    let claimed = j.claim_next().unwrap().unwrap();
    assert_eq!(claimed.id, save.id);
    let UploadIntent::Create { parent, name } = claimed.intent else {
        panic!("create")
    };
    let mut remote = folder("file", &parent, &name);
    remote.kind = NodeKind::File;
    remote.size = claimed.size;
    remote.content_version = Some("content".into());
    j.acknowledge(save.id, claimed.attempt.unwrap(), remote.clone())
        .unwrap();
    let object = j.namespace_for_operation(save.id).unwrap().unwrap();
    j.handoff_namespace(object.id, object.revision, remote)
        .unwrap();
    assert!(
        j.namespace_overlay(&scope(), "root", vec![])
            .unwrap()
            .nodes
            .is_empty()
    );
    assert!(
        j.namespace_overlay(&shared, "shared-root", vec![])
            .unwrap()
            .nodes
            .is_empty()
    );
}
#[test]
fn local_reuse_cannot_hide_a_retained_ancestor_and_capture_failure_is_atomic() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let mut j = UploadJournal::open(&root, &scope().account, 4096).unwrap();
    let parent = folder("parent", "root", "Documents");
    j.capture_namespace_ancestors(vec![(scope(), parent.clone())])
        .unwrap();
    let save = local_file(&mut j, scope(), "parent", "keep.txt");
    assert!(
        j.create_namespace_directory(scope(), "root".into(), "DOCUMENTS".into())
            .is_err()
    );
    assert_eq!(j.namespace_objects().unwrap().len(), 2);
    assert!(j.payload(save.id).is_ok());
    let mut wrong = scope();
    wrong.account = "another-account".into();
    assert!(
        j.capture_namespace_ancestors(vec![
            (scope(), folder("first", "root", "first")),
            (wrong, folder("second", "first", "second"))
        ])
        .is_err()
    );
    assert_eq!(j.namespace_objects().unwrap().len(), 2);
    assert!(j.namespace_by_local(&scope(), "first").unwrap().is_none());
    assert!(
        j.capture_namespace_ancestors(vec![(scope(), parent); 129])
            .is_err()
    );
}

#[test]
fn snapshots_cannot_take_over_an_existing_local_name_and_foreign_occupants_are_reported() {
    let temp = tempfile::tempdir().unwrap();
    let mut j = UploadJournal::open(&temp.path().join("journal"), &scope().account, 4096).unwrap();
    let existing = local_file(&mut j, scope(), "root", "already-used");
    assert!(
        j.capture_namespace_ancestors(vec![(scope(), folder("other", "root", "already-used"))])
            .is_err()
    );
    assert!(j.namespace_by_local(&scope(), "other").unwrap().is_none());
    assert!(j.payload(existing.id).is_ok());
    j.capture_namespace_ancestors(vec![(scope(), folder("ancestor", "root", "Documents"))])
        .unwrap();
    local_file(&mut j, scope(), "ancestor", "pending.txt");
    let foreign = folder("foreign", "root", "Documents");
    let listing = j
        .namespace_overlay(&scope(), "root", vec![foreign.clone()])
        .unwrap();
    assert_eq!(listing.conflicts.len(), 1);
    assert_eq!(listing.conflicts[0].remote, foreign);
    assert!(listing.nodes.iter().any(|n| n.id == "ancestor"));
}
