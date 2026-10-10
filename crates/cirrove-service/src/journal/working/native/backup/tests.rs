#![allow(clippy::unwrap_used)]
use super::super::successors::tests::{ack, seal};
use super::super::tests::{binding, bytes, count, edit, journal, publish, temp};
use super::*;
fn backup(j: &mut UploadJournal, id: Uuid) -> WorkingFile {
    let revision = j.namespace_object(id).unwrap().revision;
    j.backup_native_canonical(id, revision, "Editor.pages~".into())
        .unwrap()
}
fn temporary(j: &mut UploadJournal, id: Uuid, data: &[u8]) -> Uuid {
    let file = j.create_native_temporary(id, ".save".into()).unwrap();
    edit(j, file.id, &bytes(data));
    file.id
}
fn capture(j: &mut UploadJournal, temp: Uuid, original: Uuid) -> atomic::CapturedNativeTemporary {
    j.capture_native_temporary(temp, original)
        .unwrap()
        .capture(&CancellationToken::new())
        .unwrap()
}
#[test]
fn native_backup_gap_restart_raw_recovery_and_rollback_preserve_bytes() {
    let root = temp();
    let data = bytes(b"original");
    let mut j = journal(&root);
    let f = publish(&mut j, binding(&root, &data), &data);
    edit(&mut j, f.id, &bytes(b"dirty retained"));
    let saved = backup(&mut j, f.id);
    assert_eq!(count(&j), 0);
    assert!(j.claim_mutation().unwrap().is_none());
    assert!(j.capture_native_working(f.id).is_err());
    assert!(j.native_retirement_candidate(f.id).is_err());
    assert_eq!(j.namespace_object(f.id).unwrap().node.name, "Editor.pages~");
    drop(j);
    let db = root.path().join("journal/uploads.db");
    let before = std::fs::read(&db).unwrap();
    let ro = RecoveryJournal::open(&root.path().join("journal"), "native-working").unwrap();
    let dest = root.path().join("gap-export.pages");
    let copy = ro
        .working_export_source(f.id, saved.generation)
        .unwrap()
        .prepare_copy(&dest, &CancellationToken::new(), |_| {})
        .unwrap();
    let receipt = ro
        .verify_working_export(copy)
        .unwrap()
        .publish(&CancellationToken::new())
        .unwrap();
    assert_eq!(receipt.source.generation, saved.generation);
    assert_eq!(std::fs::read(dest).unwrap(), bytes(b"dirty retained"));
    drop(ro);
    assert_eq!(std::fs::read(db).unwrap(), before);
    let mut j = journal(&root);
    let revision = j.namespace_object(f.id).unwrap().revision;
    let restored = j.rollback_native_backup(f.id, revision).unwrap();
    assert_eq!(restored.node.name, f.node.name);
    assert!(restored.generation > saved.generation);
    assert_eq!(
        j.read_working(f.id, 0, 4096).unwrap(),
        bytes(b"dirty retained")
    );
    assert_eq!(count(&j), 0);
}
#[test]
fn native_backup_gap_promote_retains_visible_old_inode_and_exact_cloud_name() {
    let root = temp();
    let data = bytes(b"old");
    let mut j = journal(&root);
    let f = publish(&mut j, binding(&root, &data), &data);
    let held = j.working_descriptor(f.id, false).unwrap();
    let next = temporary(&mut j, f.id, b"new");
    backup(&mut j, f.id);
    let ready = capture(&mut j, next, f.id);
    let row = j
        .replace_native_temporary(ready, &CancellationToken::new())
        .unwrap();
    assert_eq!(j.namespace_object(next).unwrap().node.name, f.node.name);
    let old = j.namespace_object(f.id).unwrap();
    assert_eq!(old.node.name, "Editor.pages~");
    assert!(!old.unlinked);
    assert!(old.native_archive.is_none());
    assert!(validate_local_backup(&j.db, &old).unwrap());
    assert_eq!(
        atomic::active(
            &j.db,
            j.namespace_object(next)
                .unwrap()
                .native_archive
                .unwrap()
                .source_owner
        )
        .unwrap()
        .0,
        next
    );
    let mut retained = vec![0; data.len()];
    held.read_exact_at(&mut retained, 0).unwrap();
    assert_eq!(retained, data);
    edit(&mut j, f.id, &bytes(b"late old handle"));
    assert!(j.capture_native_working(f.id).is_err());
    assert_eq!(j.read_working(next, 0, 4096).unwrap(), bytes(b"new"));
    let UploadRepresentation::PackageReplacementArchive {
        original,
        expected_root,
        ..
    } = &row.representation
    else {
        panic!("native")
    };
    assert_eq!(original.name, f.node.name);
    assert_eq!(expected_root, &f.node.name);
    assert_eq!(count(&j), 1);
    drop(held);
    drop(j);
    let mut j = journal(&root);
    let claimed = j.claim_next().unwrap().unwrap();
    assert_eq!(claimed.id, row.id);
    ack(&mut j, &claimed);
    assert!(validate_local_backup(&j.db, &j.namespace_object(f.id).unwrap()).unwrap());
    assert_eq!(
        j.read_working(f.id, 0, 4096).unwrap(),
        bytes(b"late old handle")
    );
}
#[test]
fn native_backup_gap_ack_invalidates_staged_promote_and_rollback_selection() {
    let root = temp();
    let data = bytes(b"old");
    let mut j = journal(&root);
    let f = publish(&mut j, binding(&root, &data), &data);
    let a = seal(&mut j, f.id, b"accepted A");
    let next = temporary(&mut j, f.id, b"B");
    backup(&mut j, f.id);
    let revision = j.namespace_object(f.id).unwrap().revision;
    let ready = capture(&mut j, next, f.id);
    let claimed = j.claim_next().unwrap().unwrap();
    assert_eq!(claimed.id, a.id);
    let receipt = ack(&mut j, &claimed);
    assert!(j.rollback_native_backup(f.id, revision).is_err());
    assert!(
        j.replace_native_temporary(ready, &CancellationToken::new())
            .is_err()
    );
    assert_eq!(count(&j), 1);
    let ready = capture(&mut j, next, f.id);
    let b = j
        .replace_native_temporary(ready, &CancellationToken::new())
        .unwrap();
    assert_eq!(b.base.as_ref().unwrap().predecessor, a.id);
    let UploadRepresentation::PackageReplacementArchive { original, .. } = &b.representation else {
        panic!("native")
    };
    assert_eq!(original.as_ref(), &receipt.current.remote);
}
#[test]
fn native_backup_gap_collisions_and_transaction_failure_never_hide_canonical() {
    let root = temp();
    let data = bytes(b"old");
    let mut j = journal(&root);
    let f = publish(&mut j, binding(&root, &data), &data);
    let collision = j
        .create_native_temporary(f.id, "Editor.pages~".into())
        .unwrap();
    let before = serde_json::to_value(j.namespace_object(f.id).unwrap()).unwrap();
    let revision = j.namespace_object(f.id).unwrap().revision;
    assert!(
        j.backup_native_canonical(f.id, revision, "Editor.pages~".into())
            .is_err()
    );
    assert_eq!(
        serde_json::to_value(j.namespace_object(f.id).unwrap()).unwrap(),
        before
    );
    assert!(gap(&j.db, f.id).unwrap().is_none());
    assert_eq!(
        j.working_file(collision.id).unwrap().node.name,
        "Editor.pages~"
    );
    j.db.execute_batch("CREATE TEMP TRIGGER fail_backup BEFORE UPDATE ON namespace_objects BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(
        j.backup_native_canonical(f.id, revision, "other-backup".into())
            .is_err()
    );
    assert!(gap(&j.db, f.id).unwrap().is_none());
    assert_eq!(j.working_file(f.id).unwrap().node.name, f.node.name);
    assert_eq!(count(&j), 0);
}

#[test]
fn native_backup_gap_cancel_and_failed_promote_preserve_both_streams() {
    let root = temp();
    let data = bytes(b"old");
    let mut j = journal(&root);
    let f = publish(&mut j, binding(&root, &data), &data);
    let next = temporary(&mut j, f.id, b"new");
    backup(&mut j, f.id);
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let ready = capture(&mut j, next, f.id);
    assert!(j.replace_native_temporary(ready, &cancelled).is_err());
    assert!(gap(&j.db, f.id).unwrap().is_some());
    assert_eq!(count(&j), 0);
    let ready = capture(&mut j, next, f.id);
    j.db.execute_batch("CREATE TEMP TRIGGER fail_promotion BEFORE INSERT ON native_working_transfers BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(
        j.replace_native_temporary(ready, &CancellationToken::new())
            .is_err()
    );
    assert!(gap(&j.db, f.id).unwrap().is_some());
    assert_eq!(
        j.db.query_row("SELECT count(*) FROM native_backup_streams", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(
        j.namespace_object(f.id)
            .unwrap()
            .native_archive
            .unwrap()
            .backed_up
    );
    assert_eq!(j.read_working(f.id, 0, 4096).unwrap(), data);
    assert_eq!(j.read_working(next, 0, 4096).unwrap(), bytes(b"new"));
    assert_eq!(count(&j), 0);
}

#[test]
fn native_backup_gap_occupied_canonical_rollback_preserves_exact_gap_revision_and_bytes() {
    let root = temp();
    let data = bytes(b"accepted old");
    let mut j = journal(&root);
    let f = publish(&mut j, binding(&root, &data), &data);
    let collision = j.create_native_temporary(f.id, ".occupant".into()).unwrap();
    backup(&mut j, f.id);
    let child = j.namespace_object(f.id).unwrap();
    let marker = serde_json::to_value(gap(&j.db, f.id).unwrap().unwrap()).unwrap();
    let working = serde_json::to_value(j.working_file(f.id).unwrap()).unwrap();
    let slot = namespace::entry_slot(
        &j.db,
        &child.scope,
        child.node.parent_id.as_deref().unwrap(),
        &f.node.name,
    )
    .unwrap();
    // Simulate an independently committed directory occupant. Rollback must use
    // the actual unique name index, not assume the old canonical slot is free.
    j.db.execute(
        "UPDATE namespace_entries SET slot=?1 WHERE object=?2",
        params![slot, collision.id.to_string()],
    )
    .unwrap();
    assert!(j.rollback_native_backup(f.id, child.revision).is_err());
    assert_eq!(
        serde_json::to_value(gap(&j.db, f.id).unwrap().unwrap()).unwrap(),
        marker
    );
    assert_eq!(j.namespace_object(f.id).unwrap().revision, child.revision);
    assert_eq!(
        serde_json::to_value(j.working_file(f.id).unwrap()).unwrap(),
        working
    );
    assert_eq!(j.read_working(f.id, 0, 4096).unwrap(), data);
    assert_eq!(count(&j), 0);
}

#[test]
fn native_backup_schema18_dirty_recovery_migrates_atomically_to_current_schema() {
    let root = temp();
    let data = bytes(b"original");
    let mut j = journal(&root);
    let f = publish(&mut j, binding(&root, &data), &data);
    let dirty = bytes(b"schema18 retained dirty bytes");
    edit(&mut j, f.id, &dirty);
    let generation = j.working_file(f.id).unwrap().generation;
    drop(j);
    let path = root.path().join("journal/uploads.db");
    let db = Connection::open(&path).unwrap();
    db.execute_batch("DROP TABLE native_backup_gaps; DROP TABLE native_backup_streams; UPDATE namespace_objects SET body=json_remove(body,'$.native_archive.backed_up'); PRAGMA user_version=18;").unwrap();
    drop(db);
    for version in [18, JOURNAL_SCHEMA] {
        let before = std::fs::read(&path).unwrap();
        let ro = RecoveryJournal::open(&root.path().join("journal"), "native-working").unwrap();
        let dest = root.path().join(format!("schema{version}-export"));
        let copy = ro
            .working_export_source(f.id, generation)
            .unwrap()
            .prepare_copy(&dest, &CancellationToken::new(), |_| {})
            .unwrap();
        let receipt = ro
            .verify_working_export(copy)
            .unwrap()
            .publish(&CancellationToken::new())
            .unwrap();
        assert_eq!(receipt.source.generation, generation);
        assert_eq!(std::fs::read(dest).unwrap(), dirty);
        drop(ro);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "read-only recovery migrated or changed DB"
        );
        let db = Connection::open(&path).unwrap();
        assert_eq!(
            db.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
                .unwrap(),
            version
        );
        drop(db);
        if version == 18 {
            // A failure inside the new native table transaction must not raise
            // the durable header: representation::migrate is deliberately inert
            // for an existing18 journal.
            let mut db = Connection::open(&path).unwrap();
            crate::journal::representation::migrate(&mut db, 18).unwrap();
            db.execute_batch("CREATE VIEW native_backup_streams AS SELECT 'bad' AS unsupported;")
                .unwrap();
            assert!(super::super::migrate(&mut db).is_err());
            assert_eq!(
                db.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
                    .unwrap(),
                18
            );
            db.execute_batch("DROP VIEW native_backup_streams;")
                .unwrap();
            drop(db);
            let j = journal(&root);
            assert_eq!(
                j.db.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
                    .unwrap(),
                JOURNAL_SCHEMA
            );
            assert_eq!(j.working_file(f.id).unwrap().generation, generation);
            assert_eq!(j.read_working(f.id, 0, 4096).unwrap(), dirty);
            assert_eq!(count(&j), 0);
            assert!(gap(&j.db, f.id).unwrap().is_none());
            drop(j);
        }
    }
}

#[test]
fn native_backup_gap_retained_listing_hides_exact_canonical_and_keeps_backup_visible() {
    let root = temp();
    let data = bytes(b"old");
    let mut j = journal(&root);
    let bound = binding(&root, &data);
    let f = publish(&mut j, bound.clone(), &data);
    backup(&mut j, f.id);
    let child = j.namespace_object(f.id).unwrap();
    let parent = child.node.parent_id.as_deref().unwrap();
    let listing = j
        .namespace_overlay(&bound.scope, parent, vec![bound.archive.clone()])
        .unwrap();
    assert!(listing.conflicts.is_empty());
    assert_eq!(listing.nodes.len(), 1);
    assert_eq!(listing.nodes[0], child.node);
    assert!(listing.nodes.iter().all(|n| n.name != bound.archive.name));
    for bad in ["name", "parent", "alias"] {
        let mut remote = bound.archive.clone();
        match bad {
            "name" => remote.name = child.node.name.clone(),
            "parent" => remote.parent_id = Some("foreign-parent".into()),
            _ => remote.kind = NodeKind::Shortcut,
        }
        assert!(
            j.namespace_overlay(&bound.scope, parent, vec![remote])
                .is_err(),
            "{bad}"
        );
    }
    let mut unrelated = bound.archive.clone();
    unrelated.id = "different-artifact".into();
    let listing = j
        .namespace_overlay(&bound.scope, parent, vec![unrelated.clone()])
        .unwrap();
    assert!(
        listing.nodes.iter().any(|n| n.id == unrelated.id),
        "same name must not hide unrelated identity"
    );
    assert!(listing.nodes.iter().any(|n| n.id == child.node.id));
}
