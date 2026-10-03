#![allow(clippy::unwrap_used)]
use super::super::successors::tests::{ack, seal};
use super::super::tests::{binding, bytes, count, edit, journal, publish, temp};
use super::*;
fn temporary(j: &mut UploadJournal, canonical: Uuid, name: &str, body: &[u8]) -> WorkingFile {
    let t = j.create_native_temporary(canonical, name.into()).unwrap();
    edit(j, t.id, &bytes(body));
    j.sync_native_temporary(t.id).unwrap();
    j.working_file(t.id).unwrap()
}
fn replace(j: &mut UploadJournal, temp: Uuid, canonical: Uuid) -> UploadRecord {
    let ready = j
        .capture_native_temporary(temp, canonical)
        .unwrap()
        .capture(&CancellationToken::new())
        .unwrap();
    j.replace_native_temporary(ready, &CancellationToken::new())
        .unwrap()
}
#[test]
fn native_atomic_temp_is_durable_local_only_and_never_ordinary_upload() {
    let root = temp();
    let data = bytes(b"original");
    let mut j = journal(&root);
    let f = publish(&mut j, binding(&root, &data), &data);
    let t = temporary(&mut j, f.id, ".editor-save", b"edited");
    assert!(j.seal_working(t.id).is_err());
    assert!(j.capture_native_working(t.id).is_err());
    assert_eq!(count(&j), 0);
    assert!(j.claim_mutation().unwrap().is_none());
    drop(j);
    let j = journal(&root);
    assert_eq!(j.read_working(t.id, 0, 4096).unwrap(), bytes(b"edited"));
    j.sync_native_temporary(t.id).unwrap();
}
#[test]
fn native_atomic_pending_a_b_c_keeps_provenance_and_detached_descriptor_bytes() {
    let root = temp();
    let data = bytes(b"original");
    let mut j = journal(&root);
    let f = publish(&mut j, binding(&root, &data), &data);
    let a = seal(&mut j, f.id, b"A");
    let old_descriptor = j.working_descriptor(f.id, false).unwrap();
    let b_file = temporary(&mut j, f.id, ".B", b"B");
    let b = replace(&mut j, b_file.id, f.id);
    let c_file = temporary(&mut j, b_file.id, ".C", b"C");
    let c = replace(&mut j, c_file.id, b_file.id);
    assert!(j.working_file(f.id).unwrap().unlinked);
    assert!(j.namespace_object(f.id).unwrap().unlinked);
    assert!(j.working_file(b_file.id).unwrap().unlinked);
    let mut old = vec![0; bytes(b"A").len()];
    old_descriptor.read_exact_at(&mut old, 0).unwrap();
    assert_eq!(old, bytes(b"A"));
    edit(&mut j, f.id, &bytes(b"detached later write"));
    assert!(j.capture_native_working(f.id).is_err());
    assert_eq!(j.read_working(c_file.id, 0, 4096).unwrap(), bytes(b"C"));
    for (op, working) in [(a.id, f.id), (b.id, b_file.id), (c.id, c_file.id)] {
        assert!(j.get(op).unwrap().working_file.is_none());
        let retained: String =
            j.db.query_row(
                "SELECT working FROM native_working_operations WHERE operation=?1",
                [op.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(retained, working.to_string());
    }
    drop(old_descriptor);
    drop(j);
    let mut j = journal(&root);
    for expected in [a.id, b.id, c.id] {
        let claim = j.claim_next().unwrap().unwrap();
        assert_eq!(claim.id, expected);
        let receipt = ack(&mut j, &claim);
        let child = j.namespace_object(c_file.id).unwrap();
        assert_eq!(
            child.native_archive.as_ref().unwrap().artifact,
            format!("icloud-artifact:{}", receipt.current.remote.id)
        );
        assert_eq!(j.read_working(c_file.id, 0, 4096).unwrap(), bytes(b"C"));
        drop(j);
        j = journal(&root);
    }
    assert!(j.claim_next().unwrap().is_none());
    assert_eq!(count(&j), 3);
    assert_eq!(
        j.read_working(f.id, 0, 4096).unwrap(),
        bytes(b"detached later write")
    );
    assert_eq!(
        j.namespace_object(c_file.id).unwrap().node.name,
        "Owned.pages"
    );
}
#[test]
fn native_atomic_stale_generation_cancel_and_sql_failure_preserve_both_names() {
    for mode in 0..4 {
        let root = temp();
        let data = bytes(b"original");
        let mut j = journal(&root);
        let f = publish(&mut j, binding(&root, &data), &data);
        let t = temporary(&mut j, f.id, ".save", b"edited");
        let ready = j
            .capture_native_temporary(t.id, f.id)
            .unwrap()
            .capture(&CancellationToken::new())
            .unwrap();
        let cancel = CancellationToken::new();
        match mode {
            0=>{edit(&mut j,t.id,&bytes(b"new generation"));},
            1=>{edit(&mut j,f.id,&bytes(b"canonical changed"));},
            2=>cancel.cancel(),
            _=>j.db.execute_batch("CREATE TRIGGER fail_native_transfer BEFORE INSERT ON native_working_transfers BEGIN SELECT RAISE(ABORT,'synthetic'); END;").unwrap(),
        }
        assert!(j.replace_native_temporary(ready, &cancel).is_err());
        assert_eq!(count(&j), 0);
        assert!(!j.working_file(f.id).unwrap().unlinked);
        assert_eq!(j.namespace_object(f.id).unwrap().node.name, "Owned.pages");
        assert_eq!(j.namespace_object(t.id).unwrap().node.name, ".save");
        assert!(j.working_file(t.id).unwrap().dirty);
        drop(j);
        let j = journal(&root);
        assert!(!j.working_file(f.id).unwrap().unlinked);
    }
}
#[test]
fn native_atomic_missing_or_wrong_transfer_edge_blocks_ack_without_replay() {
    for corrupt in [false, true] {
        let root = temp();
        let data = bytes(b"original");
        let mut j = journal(&root);
        let f = publish(&mut j, binding(&root, &data), &data);
        let a = seal(&mut j, f.id, b"A");
        let t = temporary(&mut j, f.id, ".B", b"B");
        let _b = replace(&mut j, t.id, f.id);
        if corrupt {
            j.db.execute(
                "UPDATE native_working_transfers SET successor=?1",
                [Uuid::new_v4().to_string()],
            )
            .unwrap();
        } else {
            j.db.execute("DELETE FROM native_working_transfers", [])
                .unwrap();
        }
        let claim = j.claim_next().unwrap().unwrap();
        assert_eq!(claim.id, a.id);
        assert!(
            j.reserve_identity_handoff(
                claim.id,
                claim.attempt.unwrap(),
                cirrove_core::upload::RecoveryLocation::Trash {
                    local_name: "recovery.pages".into(),
                    parent: "FOLDER::com.apple.CloudDocs::TRASH_ROOT".into()
                }
            )
            .is_err()
        );
        assert!(j.claim_next().unwrap().is_none());
        assert_eq!(j.read_working(t.id, 0, 4096).unwrap(), bytes(b"B"));
    }
}
#[test]
fn native_atomic_wrong_owner_invalid_archive_and_canonical_name_are_refused() {
    let root = temp();
    let data = bytes(b"original");
    let mut j = journal(&root);
    let f = publish(&mut j, binding(&root, &data), &data);
    assert!(
        j.create_native_temporary(f.id, "Owned.pages".into())
            .is_err()
    );
    assert!(j.create_native_temporary(f.id, "../escape".into()).is_err());
    let t = j.create_native_temporary(f.id, ".save".into()).unwrap();
    j.write_working(t.id, 0, b"not an archive").unwrap();
    assert!(
        j.capture_native_temporary(t.id, f.id)
            .unwrap()
            .capture(&CancellationToken::new())
            .is_err()
    );
    edit(&mut j, t.id, &bytes(b"valid"));
    j.db.execute(
        "UPDATE native_temporary_streams SET owner=?1",
        [Uuid::new_v4().to_string()],
    )
    .unwrap();
    assert!(j.capture_native_temporary(t.id, f.id).is_err());
    assert_eq!(count(&j), 0);
}

#[test]
fn native_atomic_cancel_after_commit_returns_retained_operation() {
    let root = temp();
    let data = bytes(b"original");
    let mut j = journal(&root);
    let f = publish(&mut j, binding(&root, &data), &data);
    let t = temporary(&mut j, f.id, ".save", b"edited");
    let cancel = CancellationToken::new();
    let ready = j
        .capture_native_temporary(t.id, f.id)
        .unwrap()
        .capture(&cancel)
        .unwrap();
    let saved = j
        .replace_native_temporary_observed(ready, &cancel, |_| cancel.cancel())
        .unwrap();
    assert!(cancel.is_cancelled());
    assert_eq!(count(&j), 1);
    drop(j);
    let j = journal(&root);
    assert_eq!(j.get(saved.id).unwrap().id, saved.id);
    assert_eq!(j.namespace_object(t.id).unwrap().node.name, "Owned.pages");
}
#[test]
fn native_atomic_uncertain_predecessor_retains_all_streams_without_dispatching_successor() {
    let root = temp();
    let data = bytes(b"original");
    let mut j = journal(&root);
    let f = publish(&mut j, binding(&root, &data), &data);
    let a = seal(&mut j, f.id, b"A");
    let t = temporary(&mut j, f.id, ".B", b"B");
    let b = replace(&mut j, t.id, f.id);
    let claimed = j.claim_next().unwrap().unwrap();
    assert_eq!(claimed.id, a.id);
    j.stop_attempt(a.id, claimed.attempt.unwrap(), UploadState::VerifyRequired)
        .unwrap();
    drop(j);
    let mut j = journal(&root);
    assert!(j.claim_next().unwrap().is_none());
    assert!(!j.get(b.id).unwrap().base.unwrap().resolved);
    assert_eq!(j.read_working(f.id, 0, 4096).unwrap(), bytes(b"A"));
    assert_eq!(j.read_working(t.id, 0, 4096).unwrap(), bytes(b"B"));
    assert_eq!(count(&j), 2);
}

#[test]
fn native_atomic_namespace_capacity_refuses_before_spool_or_namespace_publication() {
    let root = temp();
    let data = bytes(b"original");
    let mut j = journal(&root);
    let f = publish(&mut j, binding(&root, &data), &data);
    // Independent retained namespace history can hit the cap with few spool files.
    j.db.execute_batch("WITH RECURSIVE history(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM history WHERE n<9998) INSERT INTO namespace_objects(id,identity,scope,working,body) SELECT 'history-'||n,'history-identity-'||n,'history-scope',NULL,'{}' FROM history;").unwrap();
    let retained = j.retained_bytes().unwrap();
    let paths = std::fs::read_dir(&j.working).unwrap().count();
    assert!(matches!(
        j.create_native_temporary(f.id, ".save".into()),
        Err(JournalError::Quota)
    ));
    assert_eq!(j.retained_bytes().unwrap(), retained);
    assert_eq!(std::fs::read_dir(&j.working).unwrap().count(), paths);
    let count: i64 =
        j.db.query_row("SELECT count(*) FROM namespace_objects", [], |r| r.get(0))
            .unwrap();
    assert_eq!(count, 10000);
    let temps: i64 =
        j.db.query_row("SELECT count(*) FROM native_temporary_streams", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(temps, 0);
}
#[test]
fn native_atomic_repeated_editor_temporary_name_reuses_name_not_inode() {
    let root = temp();
    let data = bytes(b"original");
    let mut j = journal(&root);
    let f = publish(&mut j, binding(&root, &data), &data);
    let first = temporary(&mut j, f.id, ".save", b"first");
    let _ = replace(&mut j, first.id, f.id);
    let second = temporary(&mut j, first.id, ".save", b"second");
    assert_ne!(first.id, second.id);
    let _ = replace(&mut j, second.id, first.id);
    assert_eq!(j.read_working(f.id, 0, 4096).unwrap(), data);
    assert_eq!(j.read_working(first.id, 0, 4096).unwrap(), bytes(b"first"));
    assert_eq!(
        j.read_working(second.id, 0, 4096).unwrap(),
        bytes(b"second")
    );
    let slot: String =
        j.db.query_row(
            "SELECT slot FROM working_files WHERE id=?1",
            [second.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(slot, format!("native-working-{}", second.id));
}
#[test]
fn native_atomic_readonly_recovery_exports_temp_and_detached_bytes_without_journal_writes() {
    let root = temp();
    let data = bytes(b"original");
    let mut j = journal(&root);
    let f = publish(&mut j, binding(&root, &data), &data);
    let replacement = temporary(&mut j, f.id, ".save", b"canonical");
    let _ = replace(&mut j, replacement.id, f.id);
    edit(&mut j, f.id, &bytes(b"late old descriptor"));
    let pending = temporary(&mut j, replacement.id, ".save", b"retained temp");
    let old_generation = j.working_file(f.id).unwrap().generation;
    let temp_generation = j.working_file(pending.id).unwrap().generation;
    drop(j);
    let database = root.path().join("journal/uploads.db");
    let before = std::fs::read(&database).unwrap();
    let ro = RecoveryJournal::open(&root.path().join("journal"), "native-working").unwrap();
    for (id, generation, expected) in [
        (f.id, old_generation, bytes(b"late old descriptor")),
        (pending.id, temp_generation, bytes(b"retained temp")),
    ] {
        let path = root.path().join(format!("export-{id}"));
        let prepared = ro
            .working_export_source(id, generation)
            .unwrap()
            .prepare_copy(&path, &CancellationToken::new(), |_| {})
            .unwrap();
        let receipt = ro
            .verify_working_export(prepared)
            .unwrap()
            .publish(&CancellationToken::new())
            .unwrap();
        assert_eq!(receipt.source.file, id);
        assert_eq!(receipt.source.generation, generation);
        assert_eq!(std::fs::read(path).unwrap(), expected);
    }
    drop(ro);
    assert_eq!(std::fs::read(database).unwrap(), before);
}

#[test]
fn native_temporary_rename_unlink_preserves_handles_and_readonly_recovery() {
    let root = temp();
    let original = bytes(b"original");
    let mut j = journal(&root);
    let canonical = publish(&mut j, binding(&root, &original), &original);
    let t = temporary(&mut j, canonical.id, ".save", b"temporary");
    let held = j.working_descriptor(t.id, false).unwrap();
    let moved = j
        .rename_native_local_temporary(t.id, &t.scope, &t.node, ".save-two".into())
        .unwrap();
    assert_eq!(moved.id, t.id);
    assert_eq!(moved.generation, t.generation);
    assert!(
        j.unlink_native_local_temporary(t.id, &t.scope, &t.node)
            .is_err()
    );
    let removed = j
        .unlink_native_local_temporary(t.id, &moved.scope, &moved.node)
        .unwrap();
    assert!(removed.unlinked);
    assert!(j.native_local_stream(t.id).unwrap().unwrap().detached);
    let mut read = vec![0; bytes(b"temporary").len()];
    held.read_exact_at(&mut read, 0).unwrap();
    assert_eq!(read, bytes(b"temporary"));
    edit(&mut j, t.id, &bytes(b"late unlinked descriptor"));
    assert!(j.sync_native_local_stream(t.id).unwrap());
    assert!(j.capture_native_temporary(t.id, canonical.id).is_err());
    assert!(j.seal_working(t.id).is_err());
    let reused = temporary(&mut j, canonical.id, ".save-two", b"new stream");
    assert_ne!(reused.id, t.id);
    assert_eq!(j.read_working(canonical.id, 0, 4096).unwrap(), original);
    assert_eq!(count(&j), 0);
    assert!(j.claim_mutation().unwrap().is_none());
    let generation = j.working_file(t.id).unwrap().generation;
    drop(held);
    drop(j);
    let db = root.path().join("journal/uploads.db");
    let before = std::fs::read(&db).unwrap();
    let ro = RecoveryJournal::open(&root.path().join("journal"), "native-working").unwrap();
    let destination = root.path().join("unlinked-export");
    let staged = ro
        .working_export_source(t.id, generation)
        .unwrap()
        .prepare_copy(&destination, &CancellationToken::new(), |_| {})
        .unwrap();
    ro.verify_working_export(staged)
        .unwrap()
        .publish(&CancellationToken::new())
        .unwrap();
    assert_eq!(
        std::fs::read(destination).unwrap(),
        bytes(b"late unlinked descriptor")
    );
    drop(ro);
    assert_eq!(std::fs::read(db).unwrap(), before);
}

#[test]
fn native_temporary_path_changes_reject_authority_collision_and_roll_back() {
    let root = temp();
    let original = bytes(b"original");
    let mut j = journal(&root);
    let canonical = publish(&mut j, binding(&root, &original), &original);
    let t = temporary(&mut j, canonical.id, ".save", b"temporary");
    let occupied = temporary(&mut j, canonical.id, ".occupied", b"occupant");
    let before = serde_json::to_string(&j.namespace_object(t.id).unwrap()).unwrap();
    for name in ["Owned.pages", "OWNED.PAGES", ".occupied", "../escape"] {
        assert!(
            j.rename_native_local_temporary(t.id, &t.scope, &t.node, name.into())
                .is_err()
        );
        assert_eq!(
            serde_json::to_string(&j.namespace_object(t.id).unwrap()).unwrap(),
            before
        );
        assert_eq!(j.working_file(t.id).unwrap().node, t.node);
    }
    let mut foreign = t.scope.clone();
    foreign.account = "another-account".into();
    assert!(
        j.unlink_native_local_temporary(t.id, &foreign, &t.node)
            .is_err()
    );
    assert!(
        j.unlink_native_local_temporary(canonical.id, &canonical.scope, &canonical.node)
            .is_err()
    );
    j.db.execute_batch("CREATE TEMP TRIGGER refuse_local_temp BEFORE UPDATE ON namespace_objects BEGIN SELECT RAISE(ABORT,'test rollback'); END;").unwrap();
    assert!(
        j.unlink_native_local_temporary(t.id, &t.scope, &t.node)
            .is_err()
    );
    assert!(!j.working_file(t.id).unwrap().unlinked);
    assert_eq!(
        serde_json::to_string(&j.namespace_object(t.id).unwrap()).unwrap(),
        before
    );
    j.db.execute_batch("DROP TRIGGER refuse_local_temp;")
        .unwrap();
    assert_eq!(
        j.read_working(occupied.id, 0, 4096).unwrap(),
        bytes(b"occupant")
    );
    assert_eq!(count(&j), 0);
}
