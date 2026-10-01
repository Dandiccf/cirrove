#![allow(clippy::unwrap_used)]
use super::super::successors::tests::{ack, seal};
use super::super::tests::{binding, bytes, edit, journal, publish, temp};
use super::*;
use crate::journal::package_publication::PackagePublicationStatus;
fn complete(j: &mut UploadJournal, id: Uuid, body: &[u8]) -> (UploadRecord, Node) {
    let sealed = seal(j, id, body);
    let claimed = j.claim_next().unwrap().unwrap();
    assert_eq!(claimed.id, sealed.id);
    let receipt = ack(j, &claimed);
    let row = j.get(sealed.id).unwrap();
    j.finish_package_publication(
        &row,
        PackagePublicationStatus::Present(receipt.current.remote.clone()),
        0,
    )
    .unwrap();
    (row, receipt.current.remote)
}
fn rebound(t: &tempfile::TempDir, data: &[u8], current: Node) -> NativeArchiveBinding {
    let mut b = binding(t, data);
    b.source = current;
    b.archive.id = format!("icloud-artifact:{}", b.source.id);
    b.archive.parent_id = Some(b.source.id.clone());
    b.archive.content_version = Some(format!(
        "icloud-artifact-v2:{}",
        serde_json::json!({"source_etag":b.source.etag,"source_size":b.source.size,"source_parent":b.source.parent_id,"sha256":hex::encode(Sha256::digest(data))})
    ));
    b
}
fn count(j: &UploadJournal, table: &str) -> i64 {
    j.db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}
#[test]
fn native_retirement_reuses_one_slot_with_monotonic_generation_and_retained_history() {
    let t = temp();
    let initial = bytes(b"initial");
    let mut j = journal(&t);
    let mut f = publish(&mut j, binding(&t, &initial), &initial);
    let id = f.id;
    let mut revision = j.namespace_object(id).unwrap().revision;
    for cycle in 0..4 {
        let text = format!("cycle {cycle}");
        let data = bytes(text.as_bytes());
        let (row, current) = complete(&mut j, id, text.as_bytes());
        let generation = j.working_file(id).unwrap().generation;
        let candidate = j.native_retirement_candidate(id).unwrap();
        let frontier = j.namespace_publication(0).unwrap().through;
        j.retire_native_working(&candidate, &current).unwrap();
        assert!(j.working_file(id).is_err());
        assert!(j.working.join(id.to_string()).exists());
        let batch = j.namespace_publication(frontier).unwrap();
        assert_eq!(batch.objects.len(), 2);
        let child = j.namespace_object(id).unwrap();
        assert!(child.valid_retired_native_archive());
        assert!(child.revision > revision);
        revision = child.revision;
        assert_eq!(count(&j, "native_retired_slots"), 1);
        assert!(
            j.get(row.id)
                .unwrap()
                .native_replacement_receipt()
                .is_some()
        );
        assert!(
            j.reserve_native_working(rebound(&t, &data, current.clone()))
                .is_err(),
            "must collect old path before UUID reuse"
        );
        drop(j);
        j = journal(&t);
        // Writable reopen already collects durable retirement intents.
        assert!(!j.working.join(id.to_string()).exists());
        assert_eq!(count(&j, "retired_working"), 0);
        assert_eq!(j.collect_retired_working(1).unwrap(), 0);
        f = publish(&mut j, rebound(&t, &data, current), &data);
        assert_eq!(f.id, id);
        assert!(f.generation > generation);
        assert!(f.latest.is_none());
        assert!(!f.dirty);
        assert_eq!(j.read_working(id, 0, 1_000_000).unwrap(), data);
        let child = j.namespace_object(id).unwrap();
        assert!(child.revision > revision);
        revision = child.revision;
        assert_eq!(count(&j, "native_retired_slots"), 0);
        assert_eq!(j.db.query_row("SELECT count(*) FROM namespace_objects WHERE json_type(body,'$.native_archive')='object'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
        assert_eq!(
            j.db.query_row(
                "SELECT count(*) FROM namespace_changes WHERE object=?1",
                [id.to_string()],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(count(&j, "native_working_operations"), cycle + 1);
        projection::validate_child(&j.db, &child).unwrap();
    }
}
#[test]
fn native_retirement_dirty_successor_stale_revision_and_generation_preserve_bytes() {
    let t = temp();
    let data = bytes(b"initial");
    let mut j = journal(&t);
    let f = publish(&mut j, binding(&t, &data), &data);
    assert!(
        j.native_retirement_candidate(f.id).is_err(),
        "hydration is not an uploaded receipt"
    );
    let (_, current) = complete(&mut j, f.id, b"uploaded");
    let candidate = j.native_retirement_candidate(f.id).unwrap();
    let mut changed = current.clone();
    changed.etag = Some("newer".into());
    assert!(j.retire_native_working(&candidate, &changed).is_err());
    let dirty = bytes(b"accepted later bytes");
    edit(&mut j, f.id, &dirty);
    assert!(j.retire_native_working(&candidate, &current).is_err());
    assert!(j.native_retirement_candidate(f.id).is_err());
    assert_eq!(j.read_working(f.id, 0, 1_000_000).unwrap(), dirty);
    let sealed = seal(&mut j, f.id, b"pending successor");
    assert!(j.native_retirement_candidate(f.id).is_err());
    assert_eq!(j.working_file(f.id).unwrap().latest, Some(sealed.id));
    assert_eq!(count(&j, "retired_working"), 0);
    assert_eq!(count(&j, "native_retired_slots"), 0);
}
#[test]
fn native_retirement_transaction_failure_restores_pair_head_binding_and_frontier() {
    let t = temp();
    let data = bytes(b"initial");
    let mut j = journal(&t);
    let f = publish(&mut j, binding(&t, &data), &data);
    let (_, current) = complete(&mut j, f.id, b"uploaded");
    let selected = j.native_retirement_candidate(f.id).unwrap();
    let frontier = j.namespace_publication(0).unwrap().through;
    j.db.execute_batch("CREATE TEMP TRIGGER fail_retired_child BEFORE UPDATE ON namespace_objects WHEN json_extract(NEW.body,'$.native_archive.retired')=1 BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(j.retire_native_working(&selected, &current).is_err());
    assert_eq!(
        j.native_retirement_candidate(f.id).unwrap().snapshot,
        selected.snapshot
    );
    assert_eq!(j.namespace_publication(0).unwrap().through, frontier);
    assert_eq!(count(&j, "retired_working"), 0);
    assert_eq!(count(&j, "native_retired_slots"), 0);
    assert_eq!(
        j.read_working(f.id, 0, 1_000_000).unwrap(),
        bytes(b"uploaded")
    );
}
#[test]
fn native_retirement_unlink_before_intent_cleanup_is_restart_idempotent() {
    let t = temp();
    let data = bytes(b"initial");
    let mut j = journal(&t);
    let f = publish(&mut j, binding(&t, &data), &data);
    let (_, current) = complete(&mut j, f.id, b"uploaded");
    let selected = j.native_retirement_candidate(f.id).unwrap();
    j.retire_native_working(&selected, &current).unwrap();
    std::fs::remove_file(j.working.join(f.id.to_string())).unwrap();
    File::open(&j.working).unwrap().sync_all().unwrap();
    drop(j);
    let mut j = journal(&t);
    // Reopen completes the interrupted intent even when bytes were unlinked.
    assert!(!j.working.join(f.id.to_string()).exists());
    assert_eq!(count(&j, "retired_working"), 0);
    assert_eq!(j.collect_retired_working(1).unwrap(), 0);
    let child = j.namespace_object(f.id).unwrap();
    projection::validate_child(&j.db, &child).unwrap();
    assert_eq!(count(&j, "native_retired_slots"), 1);
}
#[test]
fn native_retirement_late_capture_and_duplicate_hydration_cannot_reenter_reused_uuid() {
    let t = temp();
    let data = bytes(b"initial");
    let mut j = journal(&t);
    let f = publish(&mut j, binding(&t, &data), &data);
    edit(&mut j, f.id, &bytes(b"uploaded"));
    let late = j
        .capture_native_working(f.id)
        .unwrap()
        .capture(&CancellationToken::new())
        .unwrap();
    let capture = j
        .capture_native_working(f.id)
        .unwrap()
        .capture(&CancellationToken::new())
        .unwrap();
    let row = j
        .seal_captured_native_working(capture, &CancellationToken::new())
        .unwrap();
    let claimed = j.claim_next().unwrap().unwrap();
    let receipt = ack(&mut j, &claimed);
    j.finish_package_publication(
        &j.get(row.id).unwrap(),
        PackagePublicationStatus::Present(receipt.current.remote.clone()),
        0,
    )
    .unwrap();
    let selected = j.native_retirement_candidate(f.id).unwrap();
    j.retire_native_working(&selected, &receipt.current.remote)
        .unwrap();
    j.collect_retired_working(1).unwrap();
    let data = bytes(b"uploaded");
    let bound = rebound(&t, &data, receipt.current.remote);
    let mut delayed = j.reserve_native_working(bound.clone()).unwrap();
    delayed.write_chunk(&data).unwrap();
    let delayed = delayed.validate(&CancellationToken::new()).unwrap();
    let new = publish(&mut j, bound, &data);
    edit(&mut j, new.id, &bytes(b"new dirty incarnation"));
    assert!(j.publish_native_working(delayed).is_err());
    assert!(
        j.seal_captured_native_working(late, &CancellationToken::new())
            .is_err()
    );
    assert_eq!(
        j.read_working(new.id, 0, 1_000_000).unwrap(),
        bytes(b"new dirty incarnation")
    );
}

fn dormant(j: &mut UploadJournal, t: &tempfile::TempDir) -> (Uuid, UploadRecord, Node, u64) {
    let data = bytes(b"initial");
    let f = publish(j, binding(t, &data), &data);
    let (row, current) = complete(j, f.id, b"old acknowledged content");
    let generation = j.working_file(f.id).unwrap().generation;
    let candidate = j.native_retirement_candidate(f.id).unwrap();
    j.retire_native_working(&candidate, &current).unwrap();
    j.collect_retired_working(1).unwrap();
    (f.id, row, current, generation)
}
#[test]
fn native_reactivation_changed_same_id_uses_new_validated_proof_and_no_old_predecessor() {
    let t = temp();
    let mut j = journal(&t);
    let (id, row, mut current, generation) = dormant(&mut j, &t);
    current.etag = Some("fresh external revision".into());
    current.size += 91;
    current.parent_id = Some("FOLDER::com.apple.CloudDocs::new-owned-parent".into());
    let data = bytes(b"different content changed remotely while dormant");
    let bound = rebound(&t, &data, current.clone());
    let expected = bound.semantic.clone();
    assert_ne!(row.package_completion.as_ref(), Some(&expected));
    let f = publish(&mut j, bound, &data);
    assert_eq!(f.id, id);
    assert!(f.generation > generation);
    assert!(f.latest.is_none());
    let head = successors::head(&j.db, id).unwrap();
    assert_eq!(head.current, current);
    assert_eq!(head.semantic, expected);
    assert_eq!(head.sequence, row.sequence);
    let next = seal(&mut j, id, b"new local edit");
    assert!(next.base.is_none());
    let UploadRepresentation::PackageReplacementArchive {
        original,
        original_semantic,
        ..
    } = next.representation
    else {
        panic!("native")
    };
    assert_eq!(*original, current);
    assert_eq!(original_semantic, expected);
    assert_eq!(
        j.get(row.id).unwrap().package_completion,
        row.package_completion,
        "old receipt remains old evidence"
    );
}
#[test]
fn native_reactivation_changed_revision_rejects_old_semantics_and_exact_owner_races() {
    for arm in 0..4 {
        let t = temp();
        let mut j = journal(&t);
        let (id, row, mut current, _) = dormant(&mut j, &t);
        current.etag = Some("changed".into());
        let data = bytes(b"fresh different bytes");
        let mut bound = rebound(&t, &data, current);
        if arm == 0 {
            bound.semantic = row.package_completion.clone().unwrap();
        }
        let mut hydration = j.reserve_native_working(bound).unwrap();
        hydration.write_chunk(&data).unwrap();
        if arm == 0 {
            assert!(hydration.validate(&CancellationToken::new()).is_err());
            continue;
        }
        let validated = hydration.validate(&CancellationToken::new()).unwrap();
        let child = j.namespace_object(id).unwrap();
        let owner_id = child.native_archive.unwrap().source_owner;
        if arm == 1 {
            let mut owner = j.namespace_object(owner_id).unwrap();
            owner.revision += 1;
            let tx = j.db.transaction().unwrap();
            namespace::save(&tx, &owner).unwrap();
            tx.commit().unwrap();
        } else if arm == 2 {
            let mut child = j.namespace_object(id).unwrap();
            child.revision += 1;
            let tx = j.db.transaction().unwrap();
            namespace::save(&tx, &child).unwrap();
            tx.commit().unwrap();
        } else {
            let op = Uuid::new_v4();
            j.db.execute(
                "INSERT INTO write_queue(id,complete) VALUES(?1,0)",
                [op.to_string()],
            )
            .unwrap();
            j.db.execute(
                "INSERT INTO namespace_operations VALUES(?1,?2)",
                params![op.to_string(), owner_id.to_string()],
            )
            .unwrap();
        }
        assert!(j.publish_native_working(validated).is_err(), "arm {arm}");
        assert!(!j.working.join(id.to_string()).exists());
        assert_eq!(count(&j, "native_retired_slots"), 1);
        assert_eq!(count(&j, "working_files"), 0);
    }
}
#[test]
fn native_reactivation_identity_is_not_a_name_and_failed_refresh_rolls_back_history() {
    let t = temp();
    let mut j = journal(&t);
    let (id, _, mut current, _) = dormant(&mut j, &t);
    let data = bytes(b"new remote");
    let mut different = current.clone();
    different.id = "FILE::com.apple.CloudDocs::different-identity".into();
    let binding = Binding::checked(rebound(&t, &data, different)).unwrap();
    assert!(
        select_slot(&j, &binding).unwrap().is_none(),
        "same basename must not acquire old UUID"
    );
    let child = j.namespace_object(id).unwrap();
    let owner_id = child.native_archive.as_ref().unwrap().source_owner;
    let old_owner = serde_json::to_string(&j.namespace_object(owner_id).unwrap()).unwrap();
    let old_child = serde_json::to_string(&child).unwrap();
    let frontier = j.namespace_publication(0).unwrap().through;
    current.etag = Some("new external".into());
    let mut hydration = j
        .reserve_native_working(rebound(&t, &data, current))
        .unwrap();
    hydration.write_chunk(&data).unwrap();
    let validated = hydration.validate(&CancellationToken::new()).unwrap();
    j.db.execute_batch("CREATE TEMP TRIGGER fail_reactivation BEFORE UPDATE ON namespace_objects WHEN json_extract(NEW.body,'$.native_archive.retired')=0 BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(j.publish_native_working(validated).is_err());
    assert_eq!(
        serde_json::to_string(&j.namespace_object(owner_id).unwrap()).unwrap(),
        old_owner
    );
    assert_eq!(
        serde_json::to_string(&j.namespace_object(id).unwrap()).unwrap(),
        old_child
    );
    assert_eq!(j.namespace_publication(0).unwrap().through, frontier);
    assert_eq!(count(&j, "working_files"), 0);
    assert_eq!(count(&j, "native_working_heads"), 0);
    assert_eq!(count(&j, "native_retired_slots"), 1);
    assert!(
        j.working.join(id.to_string()).exists(),
        "uncommitted verified inode stays retained as orphan"
    );
}

#[test]
fn native_dormant_path_admission_requires_fresh_same_identity_proof() {
    let t = temp();
    let mut j = journal(&t);
    let (id, row, mut current, generation) = dormant(&mut j, &t);
    let child = j.namespace_object(id).unwrap();
    let owner = j
        .namespace_object(child.native_archive.as_ref().unwrap().source_owner)
        .unwrap();
    let data = bytes(b"new remote bytes after retirement");
    current.etag = Some("new dormant revision".into());
    current.size += 17;
    let bound = rebound(&t, &data, current.clone());
    // Match Inner::node: following source is presented under its retained local
    // owner ID. The resolver must still receive the true provider identity.
    let mut presented = current.clone();
    presented.id = owner.node.id.clone();
    presented.parent_id = owner.node.parent_id.clone();
    let mut archive = bound.archive.clone();
    archive.parent_id = Some(owner.node.id.clone());
    let selected = j
        .select_native_edit(&bound.scope, &presented, &archive, Some(&archive))
        .unwrap();
    assert!(selected.working.is_none());
    assert_eq!(selected.source, current);
    assert_eq!(selected.archive, bound.archive);
    selected.recheck(&j).unwrap();
    assert_eq!(
        serde_json::to_value(j.namespace_object(owner.id).unwrap()).unwrap(),
        serde_json::to_value(&owner).unwrap(),
        "selection must not refresh history"
    );
    assert!(
        j.select_native_edit(&bound.scope, &presented, &child.node, Some(&child.node))
            .is_err(),
        "dormant local archive cannot bypass resolver"
    );
    let mut foreign = bound.scope.clone();
    foreign.account = "foreign".into();
    assert!(
        j.select_native_edit(&foreign, &presented, &archive, None)
            .is_err()
    );
    let saved_slot: String =
        j.db.query_row(
            "SELECT body FROM native_retired_slots WHERE working=?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    j.db.execute("UPDATE native_retired_slots SET body=json_set(body,'$.current.id','different-provider-identity') WHERE working=?1", [id.to_string()]).unwrap();
    assert!(
        j.select_native_edit(&bound.scope, &presented, &archive, None)
            .is_err(),
        "different identity cannot take over dormant slot"
    );
    j.db.execute(
        "UPDATE native_retired_slots SET body=?2 WHERE working=?1",
        params![id.to_string(), saved_slot],
    )
    .unwrap();
    // An independently checked archive with old semantic evidence cannot hydrate
    // the new revision merely because pathname admission succeeded.
    let mut wrong = bound.clone();
    wrong.semantic = row.package_completion.clone().unwrap();
    let mut capture = j.reserve_native_working(wrong).unwrap();
    capture.write_chunk(&data).unwrap();
    assert!(capture.validate(&CancellationToken::new()).is_err());
    assert!(j.working_file(id).is_err());
    selected.recheck(&j).unwrap();
    let fresh = publish(&mut j, bound, &data);
    assert_eq!(fresh.id, id);
    assert!(fresh.generation > generation);
    assert!(fresh.latest.is_none());
    assert_eq!(j.native_binding(id).unwrap().source, current);
    assert!(
        selected.recheck(&j).is_err(),
        "old selection cannot cross reactivation"
    );
    assert_eq!(
        serde_json::to_value(j.get(row.id).unwrap()).unwrap(),
        serde_json::to_value(&row).unwrap(),
        "completed old proof remains immutable"
    );
}

#[test]
fn native_retirement_temporary_owner_blocks_selection_and_late_commit() {
    let t = temp();
    let data = bytes(b"initial");
    let mut j = journal(&t);
    let canonical = publish(&mut j, binding(&t, &data), &data);
    let (_, current) = complete(&mut j, canonical.id, b"uploaded");
    let selected = j.native_retirement_candidate(canonical.id).unwrap();
    let temporary = j
        .create_native_temporary(canonical.id, ".editor-save".into())
        .unwrap();
    let pending = bytes(b"unsaved next revision");
    edit(&mut j, temporary.id, &pending);
    j.sync_native_temporary(temporary.id).unwrap();
    assert!(
        j.native_retirement_candidate(canonical.id).is_err(),
        "temporary owner must block selection"
    );
    assert!(
        j.retire_native_working(&selected, &current).is_err(),
        "temporary created after selection must block commit"
    );
    assert_eq!(count(&j, "retired_working"), 0);
    drop(j);
    let j = journal(&t);
    assert_eq!(
        j.read_working(canonical.id, 0, 1_000_000).unwrap(),
        bytes(b"uploaded")
    );
    assert_eq!(j.read_working(temporary.id, 0, 1_000_000).unwrap(), pending);
    assert!(j.native_retirement_candidate(canonical.id).is_err());
}

#[test]
fn native_retirement_unlinked_temp_does_not_permanently_pin_source_owner() {
    let root = temp();
    let initial = bytes(b"original");
    let mut j = journal(&root);
    let f = publish(&mut j, binding(&root, &initial), &initial);
    let (_, remote) = complete(&mut j, f.id, b"committed");
    let temporary = j
        .create_native_temporary(f.id, ".abandoned".into())
        .unwrap();
    edit(&mut j, temporary.id, b"recoverable partial bytes");
    assert!(j.native_retirement_candidate(f.id).is_err());
    let selected = j.working_file(temporary.id).unwrap();
    j.unlink_native_local_temporary(selected.id, &selected.scope, &selected.node)
        .unwrap();
    let candidate = j.native_retirement_candidate(f.id).unwrap();
    j.retire_native_working(&candidate, &remote).unwrap();
    assert_eq!(
        j.read_working(temporary.id, 0, 4096).unwrap(),
        b"recoverable partial bytes"
    );
    edit(&mut j, temporary.id, b"late local descriptor");
    assert!(j.sync_native_local_stream(temporary.id).unwrap());
    assert!(j.namespace_object(temporary.id).unwrap().unlinked);
    assert_eq!(count(&j, "uploads"), 1);
    let generation = j.working_file(temporary.id).unwrap().generation;
    drop(j);
    let database = root.path().join("journal/uploads.db");
    let before = std::fs::read(&database).unwrap();
    let recovery =
        crate::journal::RecoveryJournal::open(&root.path().join("journal"), "native-working")
            .unwrap();
    let destination = root.path().join("retired-owner-temp-export");
    let staged = recovery
        .working_export_source(temporary.id, generation)
        .unwrap()
        .prepare_copy(&destination, &CancellationToken::new(), |_| {})
        .unwrap();
    let receipt = recovery
        .verify_working_export(staged)
        .unwrap()
        .publish(&CancellationToken::new())
        .unwrap();
    assert_eq!(receipt.source.file, temporary.id);
    assert_eq!(receipt.source.generation, generation);
    assert_eq!(
        std::fs::read(destination).unwrap(),
        b"late local descriptor"
    );
    drop(recovery);
    assert_eq!(std::fs::read(database).unwrap(), before);
}
