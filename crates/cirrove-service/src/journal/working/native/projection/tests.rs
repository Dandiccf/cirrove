#![allow(clippy::unwrap_used)]
use super::super::successors::tests::{ack, seal};
use super::super::tests::{binding, bytes, edit, journal, publish, temp};
use super::*;

#[test]
fn native_projection_hydration_and_edits_publish_source_and_file_without_cloud_ownership() {
    let t = temp();
    let data = bytes(b"initial");
    let bound = binding(&t, &data);
    let mut j = journal(&t);
    let working = publish(&mut j, bound.clone(), &data);
    let batch = j.namespace_publication(0).unwrap();
    assert_eq!(batch.objects.len(), 2);
    let child = j.namespace_object(working.id).unwrap();
    let role = child.native_archive.as_ref().unwrap();
    let owner = j.namespace_object(role.source_owner).unwrap();
    assert_eq!(owner.node.kind, NodeKind::Folder);
    assert!(owner.node.package);
    assert_eq!(child.node.kind, NodeKind::File);
    assert!(!child.node.package);
    assert_eq!(child.node.parent_id, Some(owner.node.id.clone()));
    assert!(child.remote.is_none());
    assert!(!child.remote_owned);
    assert!(child.latest.is_none());
    assert_eq!(
        j.db.query_row("SELECT count(*) FROM namespace_operations", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(!j.namespace_is_clean(&child).unwrap());
    assert!(!j.namespace_is_clean(&owner).unwrap());
    let edited = bytes(b"read your local edits before sealing");
    edit(&mut j, working.id, &edited);
    let after = j.namespace_publication(batch.through).unwrap();
    assert_eq!(after.objects.len(), 1);
    assert_eq!(after.objects[0].object.id, working.id);
    assert!(after.objects[0].working.as_ref().unwrap().dirty);
    assert_eq!(j.read_working(working.id, 0, 1_000_000).unwrap(), edited);
    let root = j
        .namespace_overlay(
            &bound.scope,
            bound.source.parent_id.as_ref().unwrap(),
            vec![bound.source.clone()],
        )
        .unwrap();
    assert_eq!(root.nodes, vec![owner.node.clone()]);
    assert!(root.conflicts.is_empty());
    let preview = Node {
        id: "preview".into(),
        name: "Preview.pdf".into(),
        ..bound.archive.clone()
    };
    let view = j
        .namespace_overlay(
            &bound.scope,
            &owner.node.id,
            vec![bound.archive.clone(), preview.clone()],
        )
        .unwrap();
    assert_eq!(view.nodes.len(), 2);
    assert!(view.conflicts.is_empty());
    assert_eq!(
        view.nodes
            .iter()
            .find(|n| n.id == working.node.id)
            .unwrap()
            .size,
        edited.len() as u64
    );
    assert!(view.nodes.iter().any(|n| n.id == preview.id));
    let mut spoof = bound.archive;
    spoof.kind = NodeKind::Folder;
    assert!(
        j.namespace_overlay(&bound.scope, &owner.node.id, vec![spoof])
            .is_err()
    );
    assert!(matches!(
        j.relocate_namespace_item(
            child.id,
            child.revision,
            "root".into(),
            "Other.pages".into()
        ),
        Err(JournalError::Intent)
    ));
    assert!(matches!(
        j.seal_working(working.id),
        Err(JournalError::Intent)
    ));
}
#[test]
fn native_projection_ack_preserves_dirty_successor_and_publishes_derived_identity_with_parent() {
    let t = temp();
    let data = bytes(b"original");
    let bound = binding(&t, &data);
    let mut j = journal(&t);
    let f = publish(&mut j, bound.clone(), &data);
    let owner = j
        .namespace_object(f.id)
        .unwrap()
        .native_archive
        .unwrap()
        .source_owner;
    let a = seal(&mut j, f.id, b"A");
    let b = seal(&mut j, f.id, b"B");
    let dirty = bytes(b"newer C");
    edit(&mut j, f.id, &dirty);
    let selected = j.working_file(f.id).unwrap();
    let before = j.namespace_publication(0).unwrap().through;
    let claimed = j.claim_next().unwrap().unwrap();
    assert_eq!(claimed.id, a.id);
    let receipt = ack(&mut j, &claimed);
    let batch = j.namespace_publication(before).unwrap();
    assert!(batch.objects.iter().any(|s| s.object.id == owner));
    let child = batch.objects.iter().find(|s| s.object.id == f.id).unwrap();
    assert_eq!(
        child.object.native_archive.as_ref().unwrap().artifact,
        format!("icloud-artifact:{}", receipt.current.remote.id)
    );
    assert!(child.object.latest.is_none());
    assert!(!child.object.remote_owned);
    let current = child.working.as_ref().unwrap();
    assert!(current.dirty);
    assert_eq!(current.generation, selected.generation);
    assert_eq!(current.latest, Some(b.id));
    assert_eq!(current.node.size, dirty.len() as u64);
    assert_ne!(current.node.size, receipt.current.remote.size);
    assert_eq!(j.namespace_for_operation(a.id).unwrap().unwrap().id, owner);
    assert_eq!(j.namespace_for_operation(b.id).unwrap().unwrap().id, owner);
    let child_id = child.object.node.id.clone();
    let parent_id = child.object.node.parent_id.clone();
    drop(j);
    let j = journal(&t);
    assert_eq!(j.read_working(f.id, 0, 1_000_000).unwrap(), dirty);
    let reopened = j.namespace_object(f.id).unwrap();
    assert_eq!(reopened.node.id, child_id);
    assert_eq!(reopened.node.parent_id, parent_id);
    validate_child(&j.db, &reopened).unwrap();
    assert!(
        !j.namespace_is_clean(&j.namespace_object(owner).unwrap())
            .unwrap()
    );
}
#[test]
fn native_projection_hydration_transaction_rolls_back_source_head_child_and_binding() {
    let t = temp();
    let data = bytes(b"rollback");
    let bound = binding(&t, &data);
    let mut j = journal(&t);
    let mut hydration = j.reserve_native_working(bound).unwrap();
    hydration.write_chunk(&data).unwrap();
    let ready = hydration.validate(&CancellationToken::new()).unwrap();
    j.db.execute_batch("CREATE TEMP TRIGGER fail_native_child BEFORE INSERT ON namespace_objects
        WHEN json_type(NEW.body,'$.native_archive')='object' BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(j.publish_native_working(ready).is_err());
    for table in [
        "namespace_objects",
        "native_working_heads",
        "native_working_bindings",
        "working_files",
        "uploads",
    ] {
        assert_eq!(
            j.db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0,
            "{table}"
        );
    }
    assert_eq!(
        std::fs::read_dir(&j.working).unwrap().count(),
        1,
        "published orphan bytes must remain retained"
    );
}
#[test]
fn native_projection_wrong_owner_scope_alias_and_operation_cannot_gain_authority() {
    let t = temp();
    let data = bytes(b"guards");
    let bound = binding(&t, &data);
    let mut j = journal(&t);
    let f = publish(&mut j, bound, &data);
    let original = j.namespace_object(f.id).unwrap();
    let frontier = j.namespace_publication(0).unwrap().through;
    for arm in 0..6 {
        let mut bad = original.clone();
        bad.revision += 1;
        match arm {
            0 => bad.native_archive.as_mut().unwrap().source_owner = Uuid::new_v4(),
            1 => bad.scope.account = "foreign".into(),
            2 => bad.native_archive.as_mut().unwrap().artifact = "spoof".into(),
            3 => bad.latest = Some(Uuid::new_v4()),
            4 => bad.remote_owned = true,
            _ => bad.native_archive.as_mut().unwrap().working = Uuid::new_v4(),
        }
        {
            let tx = j.db.transaction().unwrap();
            assert!(namespace::save(&tx, &bad).is_err(), "arm {arm}");
        }
        assert_eq!(j.namespace_publication(0).unwrap().through, frontier);
    }
    let owner = original.native_archive.as_ref().unwrap().source_owner;
    let source = j.namespace_object(owner).unwrap().remote.unwrap();
    let tx = j.db.transaction().unwrap();
    assert!(
        package_replacement::prepare_owner(&tx, &f.scope, &source, None, None).is_err(),
        "unrelated explicit replacement stole native source"
    );
}
