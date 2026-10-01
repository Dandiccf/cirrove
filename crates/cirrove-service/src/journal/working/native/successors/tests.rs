#![allow(clippy::unwrap_used)]
use super::super::tests::{binding, bytes, edit, journal, publish, temp};
use super::*;
use cirrove_core::upload::{PackageHandoffReceipt, PackageUploadReceipt, RecoveryLocation};
pub(in crate::journal::working::native) fn seal(
    j: &mut UploadJournal,
    id: Uuid,
    body: &[u8],
) -> UploadRecord {
    edit(j, id, &bytes(body));
    let copy = j
        .capture_native_working(id)
        .unwrap()
        .capture(&CancellationToken::new())
        .unwrap();
    j.seal_captured_native_working(copy, &CancellationToken::new())
        .unwrap()
}
pub(in crate::journal::working::native) fn receipt(row: &UploadRecord) -> PackageHandoffReceipt {
    let UploadRepresentation::PackageReplacementArchive {
        original,
        semantic,
        original_semantic,
        ..
    } = &row.representation
    else {
        panic!("native")
    };
    let mut current = original.as_ref().clone();
    current.id = format!("FILE::com.apple.CloudDocs::{}", row.id);
    current.etag = Some(format!("revision-{}", row.sequence));
    current.size = 1234 + row.sequence;
    let mut backup = original.as_ref().clone();
    backup.parent_id = Some("FOLDER::com.apple.CloudDocs::TRASH_ROOT".into());
    backup.etag = Some(format!("trash-{}", row.sequence));
    PackageHandoffReceipt {
        original: original.as_ref().clone(),
        current: PackageUploadReceipt {
            remote: current,
            semantic: semantic.clone(),
        },
        backup: PackageUploadReceipt {
            remote: backup,
            semantic: original_semantic.clone(),
        },
    }
}
pub(in crate::journal::working::native) fn reserve(j: &mut UploadJournal, row: &UploadRecord) {
    j.reserve_identity_handoff(
        row.id,
        row.attempt.unwrap(),
        RecoveryLocation::Trash {
            local_name: format!("recovery-{}.pages", row.id),
            parent: "FOLDER::com.apple.CloudDocs::TRASH_ROOT".into(),
        },
    )
    .unwrap();
}
pub(in crate::journal::working::native) fn ack(
    j: &mut UploadJournal,
    row: &UploadRecord,
) -> PackageHandoffReceipt {
    reserve(j, row);
    j.acknowledge_package_handoff(row.id, row.attempt.unwrap(), receipt(row))
        .unwrap();
    receipt(row)
}
fn assert_base(row: &UploadRecord, previous: &PackageHandoffReceipt) {
    let UploadRepresentation::PackageReplacementArchive {
        original,
        original_semantic,
        ..
    } = &row.representation
    else {
        panic!("native")
    };
    assert_eq!(original.as_ref(), &previous.current.remote);
    assert_eq!(original_semantic, &previous.current.semantic);
    assert!(
        matches!(&row.intent,UploadIntent::Replace{item,expected_etag} if item==&previous.current.remote.id && Some(expected_etag)==previous.current.remote.etag.as_ref())
    );
    assert!(row.base.as_ref().unwrap().resolved);
}
#[test]
fn linear_native_saves_rebase_every_proof_and_preserve_newer_dirty_bytes() {
    let t = temp();
    let data = bytes(b"original contents");
    let mut j = journal(&t);
    let f = publish(&mut j, binding(&t, &data), &data);
    let a = seal(&mut j, f.id, b"edit A");
    let b = seal(&mut j, f.id, b"edit B");
    edit(&mut j, f.id, &bytes(b"dirty C"));
    let dirty = j.working_file(f.id).unwrap();
    let claimed = j.claim_next().unwrap().unwrap();
    assert_eq!(claimed.id, a.id);
    assert!(j.claim_next().unwrap().is_none());
    let a_receipt = ack(&mut j, &claimed);
    let after = j.working_file(f.id).unwrap();
    assert!(after.dirty);
    assert_eq!(after.generation, dirty.generation);
    assert_eq!(after.latest, Some(b.id));
    assert_eq!(after.node, dirty.node);
    assert_eq!(j.read_working(f.id, 0, 4096).unwrap(), bytes(b"dirty C"));
    let capture = j
        .capture_native_working(f.id)
        .unwrap()
        .capture(&CancellationToken::new())
        .unwrap();
    let c = j
        .seal_captured_native_working(capture, &CancellationToken::new())
        .unwrap();
    let next = j.claim_next().unwrap().unwrap();
    assert_eq!(next.id, b.id);
    assert_base(&next, &a_receipt);
    assert_eq!(
        j.confirmed_upload_base(next.id).unwrap(),
        Some(a_receipt.current.remote.clone())
    );
    let resource_count: i64 =
        j.db.query_row(
            "SELECT count(*) FROM write_resources WHERE id=?1 AND resource=?2",
            params![
                next.id.to_string(),
                mutations::upload_resources(&next.scope, &next.intent).unwrap()[0]
            ],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(resource_count, 1);
    let b_receipt = ack(&mut j, &next);
    let next = j.claim_next().unwrap().unwrap();
    assert_eq!(next.id, c.id);
    assert_base(&next, &b_receipt);
    let _ = ack(&mut j, &next);
    assert!(j.claim_next().unwrap().is_none());
    let objects = [a.id, b.id, c.id].map(|id| j.namespace_for_operation(id).unwrap().unwrap().id);
    assert_eq!(objects[0], objects[1]);
    assert_eq!(objects[1], objects[2]);
    assert_eq!(j.working_file(f.id).unwrap().node.kind, NodeKind::File);
}
#[test]
fn restart_between_ack_and_resolution_uses_retained_receipt_only() {
    let t = temp();
    let data = bytes(b"original contents");
    let mut j = journal(&t);
    let f = publish(&mut j, binding(&t, &data), &data);
    let a = seal(&mut j, f.id, b"A");
    let b = seal(&mut j, f.id, b"B");
    let claimed = j.claim_next().unwrap().unwrap();
    assert_eq!(claimed.id, a.id);
    let verified = ack(&mut j, &claimed);
    assert!(!j.get(b.id).unwrap().base.unwrap().resolved);
    drop(j);
    let mut j = journal(&t);
    let claim = j.claim_next().unwrap().unwrap();
    assert_eq!(claim.id, b.id);
    assert_base(&claim, &verified);
    drop(j);
    let mut j = journal(&t);
    assert!(j.claim_next().unwrap().is_none());
    let resumed = j.claim_next_verification().unwrap().unwrap();
    assert_eq!(resumed.id, b.id);
    assert_base(&resumed, &verified);
    let _ = ack(&mut j, &resumed);
}
#[test]
fn uncertain_or_conflicting_predecessor_blocks_descendants_without_byte_loss() {
    for state in [UploadState::VerifyRequired, UploadState::Conflict] {
        let t = temp();
        let data = bytes(b"original contents");
        let mut j = journal(&t);
        let f = publish(&mut j, binding(&t, &data), &data);
        let a = seal(&mut j, f.id, b"A");
        let b = seal(&mut j, f.id, b"B");
        edit(&mut j, f.id, b"incomplete C");
        let claimed = j.claim_next().unwrap().unwrap();
        j.stop_attempt(a.id, claimed.attempt.unwrap(), state)
            .unwrap();
        assert!(j.claim_next().unwrap().is_none());
        assert!(!j.get(b.id).unwrap().base.unwrap().resolved);
        drop(j);
        let ro = RecoveryJournal::open(&t.path().join("journal"), "native-working").unwrap();
        for (id, expected) in [(a.id, bytes(b"A")), (b.id, bytes(b"B"))] {
            let dst = t.path().join(id.to_string());
            ro.local_export_source(id)
                .unwrap()
                .copy_to(&dst, &CancellationToken::new(), |_| {})
                .unwrap();
            assert_eq!(std::fs::read(dst).unwrap(), expected);
        }
        assert_eq!(ro.working_recovery_list(None, 10).unwrap().0.len(), 1);
    }
}
#[test]
fn acknowledgement_binding_failure_rolls_back_queue_namespace_and_receipt() {
    let t = temp();
    let data = bytes(b"original contents");
    let mut j = journal(&t);
    let f = publish(&mut j, binding(&t, &data), &data);
    let a = seal(&mut j, f.id, b"A");
    let b = seal(&mut j, f.id, b"B");
    let row = j.claim_next().unwrap().unwrap();
    let recovery_id = j
        .reserve_identity_handoff(
            row.id,
            row.attempt.unwrap(),
            RecoveryLocation::Trash {
                local_name: format!("recovery-{}.pages", row.id),
                parent: "FOLDER::com.apple.CloudDocs::TRASH_ROOT".into(),
            },
        )
        .unwrap();
    let source_key: String =
        j.db.query_row(
            "SELECT source FROM native_working_bindings WHERE working=?1",
            [f.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    let recovery_before = serde_json::to_value(j.namespace_object(recovery_id).unwrap()).unwrap();
    let retained_before = j.get(a.id).unwrap();
    assert!(retained_before.native_replacement_receipt().is_none());
    assert!(retained_before.remote.is_none());
    assert!(retained_before.package_completion.is_none());
    let retained_before = serde_json::to_value(retained_before).unwrap();
    let owner = j.namespace_for_operation(a.id).unwrap().unwrap();
    let old = head(&j.db, f.id).unwrap();
    let frontier: i64 =
        j.db.query_row("SELECT value FROM namespace_clock", [], |r| r.get(0))
            .unwrap();
    j.db.execute_batch("CREATE TEMP TRIGGER fail_native_ack BEFORE UPDATE ON native_working_heads BEGIN SELECT RAISE(ABORT,'synthetic acknowledgement boundary'); END;").unwrap();
    assert!(
        j.acknowledge_package_handoff(a.id, row.attempt.unwrap(), receipt(&row))
            .is_err()
    );
    let retained_after = j.get(a.id).unwrap();
    assert_eq!(retained_after.state, UploadState::Uploading);
    assert!(retained_after.native_replacement_receipt().is_none());
    assert!(retained_after.remote.is_none());
    assert!(retained_after.package_completion.is_none());
    assert_eq!(
        serde_json::to_value(retained_after).unwrap(),
        retained_before
    );
    assert_eq!(
        serde_json::to_value(j.namespace_object(recovery_id).unwrap()).unwrap(),
        recovery_before
    );
    assert_eq!(
        j.db.query_row::<String, _, _>(
            "SELECT source FROM native_working_bindings WHERE working=?1",
            [f.id.to_string()],
            |r| r.get(0),
        )
        .unwrap(),
        source_key
    );
    assert_eq!(
        j.namespace_for_operation(a.id).unwrap().unwrap().remote,
        owner.remote
    );
    assert_eq!(head(&j.db, f.id).unwrap().current, old.current);
    assert!(!j.get(b.id).unwrap().base.unwrap().resolved);
    assert_eq!(
        j.db.query_row::<i64, _, _>("SELECT value FROM namespace_clock", [], |r| r.get(0))
            .unwrap(),
        frontier
    );
    j.db.execute_batch("DROP TRIGGER fail_native_ack;").unwrap();
    j.acknowledge_package_handoff(a.id, row.attempt.unwrap(), receipt(&row))
        .unwrap();
    assert_eq!(j.claim_next().unwrap().unwrap().id, b.id);
}
#[test]
fn foreign_predecessor_association_and_forged_semantic_never_resolve() {
    for corrupt_semantic in [false, true] {
        let t = temp();
        let data = bytes(b"original contents");
        let mut j = journal(&t);
        let f = publish(&mut j, binding(&t, &data), &data);
        let a = seal(&mut j, f.id, b"A");
        let b = seal(&mut j, f.id, b"B");
        let row = j.claim_next().unwrap().unwrap();
        let _ = ack(&mut j, &row);
        if corrupt_semantic {
            j.db.execute("UPDATE uploads SET body=json_set(body,'$.package_completion.sha256',?2) WHERE id=?1",params![a.id.to_string(),"0".repeat(64)]).unwrap();
        } else {
            j.db.execute(
                "UPDATE native_working_operations SET owner=?2 WHERE operation=?1",
                params![b.id.to_string(), Uuid::new_v4().to_string()],
            )
            .unwrap();
        }
        assert!(j.claim_next().is_err());
        let row = j.get(b.id).unwrap();
        assert!(!row.base.unwrap().resolved);
        assert_eq!(row.state, UploadState::Pending);
    }
}

#[test]
fn competing_seal_cannot_branch_and_acknowledged_identity_collision_rolls_back() {
    let t = temp();
    let data = bytes(b"original contents");
    let mut j = journal(&t);
    let f = publish(&mut j, binding(&t, &data), &data);
    edit(&mut j, f.id, &bytes(b"A"));
    let first = j
        .capture_native_working(f.id)
        .unwrap()
        .capture(&CancellationToken::new())
        .unwrap();
    let stale = j
        .capture_native_working(f.id)
        .unwrap()
        .capture(&CancellationToken::new())
        .unwrap();
    let a = j
        .seal_captured_native_working(first, &CancellationToken::new())
        .unwrap();
    assert!(
        j.seal_captured_native_working(stale, &CancellationToken::new())
            .is_err()
    );
    let row = j.claim_next().unwrap().unwrap();
    reserve(&mut j, &row);
    let r = receipt(&row);
    let mut collision = binding(&t, &bytes(b"A"));
    collision.source = r.current.remote.clone();
    // Keep this independent owner in another directory: hydration now correctly
    // reserves its name. The returned identity still collides at acknowledgement.
    collision.source.parent_id = Some("FOLDER::com.apple.CloudDocs::other-owned".into());
    collision.semantic = r.current.semantic.clone();
    collision.archive.id = format!("icloud-artifact:{}", collision.source.id);
    collision.archive.parent_id = Some(collision.source.id.clone());
    let raw = collision
        .archive
        .content_version
        .as_ref()
        .unwrap()
        .strip_prefix("icloud-artifact-v2:")
        .unwrap();
    let mut revision: serde_json::Value = serde_json::from_str(raw).unwrap();
    revision["source_etag"] = serde_json::json!(collision.source.etag);
    revision["source_size"] = serde_json::json!(collision.source.size);
    revision["source_parent"] = serde_json::json!(collision.source.parent_id);
    collision.archive.content_version = Some(format!("icloud-artifact-v2:{revision}"));
    let _other = publish(&mut j, collision, &bytes(b"A"));
    let before = j.namespace_for_operation(a.id).unwrap().unwrap().remote;
    assert!(
        j.acknowledge_package_handoff(a.id, row.attempt.unwrap(), r)
            .is_err()
    );
    assert_eq!(j.get(a.id).unwrap().state, UploadState::Uploading);
    assert_eq!(
        j.namespace_for_operation(a.id).unwrap().unwrap().remote,
        before
    );
    assert!(head(&j.db, f.id).unwrap().sequence < a.sequence);
}
