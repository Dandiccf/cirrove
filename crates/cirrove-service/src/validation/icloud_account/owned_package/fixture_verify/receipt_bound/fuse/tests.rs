#![allow(clippy::unwrap_used)]
use super::*;
use crate::journal::UploadJournal;
use cirrove_core::{
    reads::NativeArchiveBinding,
    upload::{PackageHandoffReceipt, PackageUploadReceipt, RecoveryLocation},
};

fn source(path: &Path, root: &str, value: &[u8]) -> Source {
    let bytes =
        crate::native_import::synthetic_package_archive(&format!("{root}/Metadata/data"), value);
    std::fs::write(path, &bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let receipt = PackageDownload {
        size: bytes.len() as u64,
        sha256: hex::encode(Sha256::digest(&bytes)),
    };
    let semantic = cirrove_icloud::package_archive_semantic_identity_versioned(
        &File::open(path).unwrap(),
        &receipt,
        root,
        2,
        &CancellationToken::new(),
    )
    .unwrap();
    Source {
        path: path.to_owned(),
        size: receipt.size,
        sha256: receipt.sha256,
        root: root.into(),
        semantic,
    }
}
fn sources() -> (tempfile::TempDir, Sources) {
    sources_for(FuseFormat::Numbers)
}
fn sources_for(format: FuseFormat) -> (tempfile::TempDir, Sources) {
    let run = Uuid::new_v4();
    let temp = tempfile::Builder::new()
        .prefix(&format!("cirrove-{}-fuse-{run}", format.format()))
        .rand_bytes(0)
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/var/tmp")
        .unwrap();
    let mut plan = Sources {
        format,
        version: 1,
        run,
        session_directory: temp.path().to_owned(),
        source_a: source(
            &temp.path().join(format!("source-a.{}", format.extension())),
            &format.source_root(),
            b"A original",
        ),
        source_b_original: source(
            &temp
                .path()
                .join(format!("source-b-original.{}", format.extension())),
            &format.source_root(),
            b"B edited",
        ),
        source_b: source(
            &temp
                .path()
                .join(format!("source-b-fuse.{}", format.extension())),
            &format!(
                "Cirrove-{}-Parent-{run}.{}",
                format.application(),
                format.extension()
            ),
            b"B edited",
        ),
    };
    // V2 semantics intentionally exclude the common archive root.
    assert_eq!(plan.source_b_original.semantic, plan.source_b.semantic);
    plan.session_directory = temp.path().to_owned();
    plan.verify().unwrap();
    (temp, plan)
}
fn write_registration(path: &Path, value: &impl serde::Serialize) -> String {
    let bytes = serde_json::to_vec(value).unwrap();
    std::fs::write(path, &bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    hex::encode(Sha256::digest(bytes))
}
fn archive(source: &Source, directory: &Path) -> crate::native_import::ValidatedPackageArchive {
    crate::native_import::ValidatedPackageArchive::capture(
        &source.path,
        directory,
        &source.root,
        &CancellationToken::new(),
    )
    .unwrap()
}
fn fixture(
    save: bool,
    native: bool,
    retired: bool,
) -> (tempfile::TempDir, FuseRegistration, UploadJournal) {
    fixture_mode(save, native, retired, false)
}
fn fixture_mode(
    save: bool,
    native: bool,
    retired: bool,
    atomic: bool,
) -> (tempfile::TempDir, FuseRegistration, UploadJournal) {
    fixture_format(save, native, retired, atomic, FuseFormat::Numbers)
}
fn fixture_format(
    save: bool,
    native: bool,
    retired: bool,
    atomic: bool,
    format: FuseFormat,
) -> (tempfile::TempDir, FuseRegistration, UploadJournal) {
    let (temp, sources) = sources_for(format);
    let (old, mut parent, _, _, old_rows) = super::super::tests::fixture();
    let mut plan = FuseRegistration {
        format,
        original_window: (format != FuseFormat::Numbers).then(|| FuseWindow {
            run: sources.run,
            started_unix_ms: 1,
            deadline_unix_ms: 1_200_001,
            cleanup_reserve_seconds: 120,
        }),
        version: 1,
        run: sources.run,
        account: old.account,
        label: format.label().into(),
        session_directory: sources.session_directory,
        settings_sha256: old.settings_sha256,
        parent_creation: Uuid::nil(),
        import: Uuid::nil(),
        save: None,
        working: None,
        owner: None,
        source_a: sources.source_a,
        source_b_original: sources.source_b_original,
        source_b: sources.source_b,
    };
    parent.name = plan.receipt_plan().parent_name();
    let mut j = UploadJournal::open(
        &temp.path().join("journal"),
        &plan.account.to_string(),
        16 * 1024 * 1024,
    )
    .unwrap();
    let local = j
        .create_namespace_directory(
            plan.receipt_plan().scope(),
            cirrove_icloud::ROOT_ID.into(),
            parent.name.clone(),
        )
        .unwrap();
    let mutation = j.claim_mutation().unwrap().unwrap();
    plan.parent_creation = mutation.id;
    j.acknowledge_mutation(
        mutation.id,
        mutation.attempt.unwrap(),
        MutationReceipt::Upsert(parent.clone()),
    )
    .unwrap();
    let local = j.namespace_object(local.id).unwrap();
    j.handoff_namespace(local.id, local.revision, parent.clone())
        .unwrap();
    let imported = j
        .enqueue_validated_package_archive(
            plan.receipt_plan().scope(),
            UploadIntent::Create {
                parent: parent.id.clone(),
                name: plan.sources().name(),
            },
            archive(&plan.source_a, temp.path()),
            &CancellationToken::new(),
        )
        .unwrap();
    plan.import = imported.id;
    let mut original = old_rows[0].remote.clone().unwrap();
    original.name = plan.sources().name();
    original.size = plan.source_a.semantic.expanded_bytes;
    let claimed = j.claim_next().unwrap().unwrap();
    assert_eq!(claimed.id, imported.id);
    j.acknowledge_package(
        claimed.id,
        claimed.attempt.unwrap(),
        PackageUploadReceipt {
            remote: original.clone(),
            semantic: plan.source_a.semantic.clone(),
        },
    )
    .unwrap();
    let imported = j.get(imported.id).unwrap();
    j.finish_package_publication(
        &imported,
        PackagePublicationStatus::Present(original.clone()),
        0,
    )
    .unwrap();
    assert!(
        j.namespace_for_operation(imported.id).unwrap().is_none(),
        "explicit import has no invented namespace operation"
    );
    if save {
        let queued = if native {
            let canonical = source(
                &temp.path().join("canonical-fixture.numbers"),
                &original.name,
                b"A original",
            );
            assert_eq!(canonical.semantic, plan.source_a.semantic);
            let artifact = Node {
                id: format!("icloud-artifact:{}", original.id),
                parent_id: Some(original.id.clone()),
                name: original.name.clone(),
                kind: NodeKind::File,
                size: canonical.size,
                modified_unix: 0,
                etag: None,
                content_version: Some(format!(
                    "icloud-artifact-v2:{}",
                    serde_json::json!({"source_etag":original.etag,"source_size":original.size,"source_parent":original.parent_id,"sha256":canonical.sha256})
                )),
                target: None,
                package: false,
            };
            let mut hydration = j
                .reserve_native_working(NativeArchiveBinding {
                    scope: plan.receipt_plan().scope(),
                    source: original.clone(),
                    archive: artifact,
                    semantic: canonical.semantic,
                })
                .unwrap();
            hydration
                .write_chunk(&std::fs::read(canonical.path).unwrap())
                .unwrap();
            let file = j
                .publish_native_working(hydration.validate(&CancellationToken::new()).unwrap())
                .unwrap();
            plan.working = Some(file.id);
            if atomic {
                let temporary = j
                    .create_native_temporary(file.id, ".atomic-save".into())
                    .unwrap();
                j.write_working(
                    temporary.id,
                    0,
                    &std::fs::read(&plan.source_b.path).unwrap(),
                )
                .unwrap();
                j.sync_native_temporary(temporary.id).unwrap();
                let captured = j
                    .capture_native_temporary(temporary.id, file.id)
                    .unwrap()
                    .capture(&CancellationToken::new())
                    .unwrap();
                let queued = j
                    .replace_native_temporary(captured, &CancellationToken::new())
                    .unwrap();
                plan.working = Some(temporary.id);
                queued
            } else {
                j.truncate_working(file.id, 0).unwrap();
                j.write_working(file.id, 0, &std::fs::read(&plan.source_b.path).unwrap())
                    .unwrap();
                let captured = j
                    .capture_native_working(file.id)
                    .unwrap()
                    .capture(&CancellationToken::new())
                    .unwrap();
                j.seal_captured_native_working(captured, &CancellationToken::new())
                    .unwrap()
            }
        } else {
            j.enqueue_validated_package_replacement(
                plan.receipt_plan().scope(),
                original.clone(),
                plan.source_a.semantic.clone(),
                archive(&plan.source_b, temp.path()),
                &CancellationToken::new(),
            )
            .unwrap()
        };
        plan.save = Some(queued.id);
        plan.owner = Some(j.namespace_for_operation(queued.id).unwrap().unwrap().id);
        // A fully valid CLI row has identical typed receipts but no native association.
        if !native {
            plan.working = Some(Uuid::new_v4());
        }
        assert!(queued.base.is_none() && queued.working_file.is_none());
        let claimed = j.claim_next().unwrap().unwrap();
        assert_eq!(claimed.id, queued.id);
        let mut current = original.clone();
        current.id = "FILE::com.apple.CloudDocs::current".into();
        current.etag = Some("current-r1".into());
        current.size = plan.source_b.semantic.expanded_bytes;
        let mut backup = original.clone();
        backup.parent_id = Some("FOLDER::com.apple.CloudDocs::TRASH_ROOT".into());
        backup.etag = Some("trash-r1".into());
        j.reserve_identity_handoff(
            claimed.id,
            claimed.attempt.unwrap(),
            RecoveryLocation::Trash {
                local_name: format!("recovery-{}.{}", claimed.id, format.extension()),
                parent: backup.parent_id.clone().unwrap(),
            },
        )
        .unwrap();
        j.acknowledge_package_handoff(
            claimed.id,
            claimed.attempt.unwrap(),
            PackageHandoffReceipt {
                original: original.clone(),
                current: PackageUploadReceipt {
                    remote: current.clone(),
                    semantic: plan.source_b.semantic.clone(),
                },
                backup: PackageUploadReceipt {
                    remote: backup,
                    semantic: plan.source_a.semantic.clone(),
                },
            },
        )
        .unwrap();
        let row = j.get(queued.id).unwrap();
        j.finish_package_publication(&row, PackagePublicationStatus::Present(current.clone()), 0)
            .unwrap();
        if retired {
            let candidate = j
                .native_retirement_candidate(plan.working.unwrap())
                .unwrap();
            j.retire_native_working(&candidate, &current).unwrap();
        }
    }
    plan.validate().unwrap();
    (temp, plan, j)
}
fn ro(temp: &tempfile::TempDir, plan: &FuseRegistration, j: UploadJournal) -> RecoveryJournal {
    drop(j);
    RecoveryJournal::open(&temp.path().join("journal"), &plan.account.to_string()).unwrap()
}
#[test]
fn owned_fuse_registration_is_distinct_and_digest_bound() {
    let (_temp, plan, _j) = fixture(false, false, false);
    plan.validate().unwrap();
    assert!(
        plan.receipt_plan().validate().is_err(),
        "legacy CLI source scope stays strict"
    );
    let bytes = serde_json::to_vec(&plan).unwrap();
    let digest = hex::encode(Sha256::digest(&bytes));
    registered_fuse(&bytes, &digest).unwrap();
    let mut changed = bytes.clone();
    changed.push(b' ');
    assert!(registered_fuse(&changed, &digest).is_err());
    let mut value = serde_json::to_value(&plan).unwrap();
    value["replacement"] = serde_json::json!(Uuid::new_v4());
    let changed = serde_json::to_vec(&value).unwrap();
    assert!(registered_fuse(&changed, &hex::encode(Sha256::digest(&changed))).is_err());
}
#[test]
fn owned_fuse_source_scan_binds_rewrite_root_hash_and_semantics() {
    let (temp, plan) = sources();
    for arm in 0..5 {
        let mut altered = plan.clone();
        match arm {
            0 => altered.source_b.root = "Source.numbers".into(),
            1 => altered.source_b.sha256 = "f".repeat(64),
            2 => {
                altered.source_b.semantic.sha256 = "f".repeat(64);
                altered.source_b_original.semantic = altered.source_b.semantic.clone();
            }
            3 => altered.source_b.path = altered.source_b_original.path.clone(),
            _ => altered.source_a = altered.source_b_original.clone(),
        }
        assert!(altered.verify().is_err(), "source arm {arm} accepted");
    }
    let path = temp.path().join("source-registration.json");
    let digest = write_registration(&path, &plan);
    assert_eq!(
        icloud_owned_fuse_source_verify(&path, &digest).unwrap()["offline_only"],
        true
    );
}
#[test]
fn owned_fuse_capture_scan_binds_purpose_master_and_actual_content() {
    let (temp, master) = sources();
    let master_path = temp.path().join("source-registration.json");
    let master_digest = write_registration(&master_path, &master);
    for purpose in [
        CapturePurpose::CanonicalA,
        CapturePurpose::HeldA,
        CapturePurpose::CurrentB,
        CapturePurpose::RemountedB,
    ] {
        let value = match purpose {
            CapturePurpose::CanonicalA | CapturePurpose::HeldA => b"A original".as_slice(),
            _ => b"B edited".as_slice(),
        };
        let mut plan = CaptureRegistration {
            version: 1,
            run: master.run,
            session_directory: temp.path().to_owned(),
            source_registration_sha256: master_digest.clone(),
            purpose,
            source: source(
                &temp.path().join(format!("{}.numbers", purpose.stem())),
                &master.name(),
                value,
            ),
        };
        let path = temp
            .path()
            .join(format!("{}-registration.json", purpose.stem()));
        for arm in 0..6 {
            let mut altered = plan.clone();
            match arm {
                0 => altered.run = Uuid::new_v4(),
                1 => altered.source.root = "Source.numbers".into(),
                2 => altered.source.sha256 = "f".repeat(64),
                3 => {
                    altered.source.semantic = match purpose {
                        CapturePurpose::CanonicalA | CapturePurpose::HeldA => {
                            master.source_b.semantic.clone()
                        }
                        _ => master.source_a.semantic.clone(),
                    }
                }
                4 => altered.source_registration_sha256 = "f".repeat(64),
                _ => altered.source.path = master.source_b.path.clone(),
            }
            let digest = write_registration(&path, &altered);
            assert!(
                icloud_owned_fuse_capture_verify(&path, &digest).is_err(),
                "capture arm {arm} accepted"
            );
        }
        let digest = write_registration(&path, &plan);
        assert_eq!(
            icloud_owned_fuse_capture_verify(&path, &digest).unwrap()["offline_only"],
            true
        );
        plan.source.size += 1;
        assert!(capture_binding(&plan, &master, &path).is_err());
    }
}
#[test]
fn owned_fuse_actual_journal_proves_preflight_active_and_retired_save() {
    for (save, retired) in [(false, false), (true, false), (true, true)] {
        let (temp, plan, j) = fixture(save, true, retired);
        let journal = ro(&temp, &plan, j);
        let (_, _, current, backup, association, inventory) =
            fuse_journal_binding(&plan, &journal).unwrap();
        assert_eq!(backup.is_some(), save);
        assert_eq!(association.is_some(), save);
        assert_eq!(inventory, (i64::from(save && !retired), 0));
        if let Some(actual) = association {
            assert_eq!(actual.3.native_archive.unwrap().retired, retired);
            assert_eq!(actual.2.remote.unwrap(), current);
        }
    }
}
#[test]
fn owned_fuse_actual_cli_receipt_without_native_association_is_refused() {
    let (temp, plan, j) = fixture(true, false, false);
    let journal = ro(&temp, &plan, j);
    journal_binding(&plan.receipt_plan(), &journal).unwrap();
    assert!(
        journal
            .native_validation_fuse_association(plan.save.unwrap())
            .unwrap()
            .is_none()
    );
    assert!(fuse_journal_binding(&plan, &journal).is_err());
}
#[test]
fn owned_fuse_actual_association_refuses_foreign_registered_working_uuid() {
    let (temp, mut plan, j) = fixture(true, true, false);
    let journal = ro(&temp, &plan, j);
    fuse_journal_binding(&plan, &journal).unwrap();
    plan.working = Some(Uuid::new_v4());
    assert!(
        fuse_journal_binding(&plan, &journal).is_err(),
        "foreign registered working UUID accepted"
    );
}
#[test]
fn owned_fuse_actual_working_inventory_refuses_dirty_and_extra_streams() {
    for dirty in [false, true] {
        let (temp, plan, mut j) = fixture(true, true, false);
        if dirty {
            j.write_working(plan.working.unwrap(), 0, b"unsaved")
                .unwrap();
        } else {
            let mut node = j.working_file(plan.working.unwrap()).unwrap().node;
            node.id = "ordinary-extra".into();
            node.name = "extra.txt".into();
            node.parent_id = j.get(plan.import).unwrap().remote.unwrap().parent_id;
            node.size = 5;
            node.content_version = None;
            node.etag = Some("extra-r1".into());
            let extra = j
                .create_working(plan.receipt_plan().scope(), node, false, &b"extra"[..])
                .unwrap();
            assert!(!extra.dirty && !extra.unlinked);
        }
        let journal = ro(&temp, &plan, j);
        assert!(
            fuse_journal_binding(&plan, &journal).is_err(),
            "dirty or extra working stream accepted"
        );
    }
}
#[test]
fn owned_fuse_association_refuses_detached_owner_and_changed_original() {
    let (temp, plan, j) = fixture(true, true, false);
    let journal = ro(&temp, &plan, j);
    let (rows, _, _, _, actual, _) = fuse_journal_binding(&plan, &journal).unwrap();
    let row = rows.iter().find(|r| Some(r.id) == plan.save).unwrap();
    let original = rows
        .iter()
        .find(|r| r.id == plan.import)
        .unwrap()
        .remote
        .as_ref()
        .unwrap();
    for arm in 0..5 {
        let mut altered = actual.clone().unwrap();
        let mut before = original.clone();
        match arm {
            0 => altered.1 = Uuid::new_v4(),
            1 => altered.3.native_archive.as_mut().unwrap().source_owner = Uuid::new_v4(),
            2 => altered.2.scope.collection = "foreign".into(),
            3 => before.etag = Some("changed-r2".into()),
            _ => altered.4.as_mut().unwrap().dirty = true,
        }
        assert!(
            association_binding(&plan, row, &before, &altered).is_err(),
            "association arm {arm} accepted"
        );
    }
    assert!(working_inventory_binding(None, (1, 0)).is_err());
    assert!(working_inventory_binding(actual.as_ref(), (2, 0)).is_err());
    assert!(working_inventory_binding(actual.as_ref(), (1, 1)).is_err());
}

#[test]
fn owned_fuse_atomic_detached_original_is_verified_without_replay() {
    let (temp, plan, j) = fixture_mode(true, true, false, true);
    let journal = ro(&temp, &plan, j);
    assert_eq!(
        journal.native_validation_working_inventory().unwrap(),
        (2, 1)
    );
    let association = journal
        .native_validation_fuse_association(plan.save.unwrap())
        .unwrap();
    assert!(working_inventory_binding(association.as_ref(), (2, 1)).is_err());
    let result = fuse_journal_binding(&plan, &journal);
    assert!(result.is_ok(), "exact retained atomic original refused");
    assert_eq!(
        result.unwrap().5,
        (2, 1),
        "raw inventory must remain observable"
    );
}
#[test]
fn owned_fuse_atomic_detached_original_refuses_changed_provenance_and_bytes() {
    for arm in 0..9 {
        let (temp, plan, j) = fixture_mode(true, true, false, true);
        drop(j);
        let db = rusqlite::Connection::open(temp.path().join("journal/uploads.db")).unwrap();
        let old: String = db
            .query_row(
                "SELECT previous_working FROM native_working_transfers",
                [],
                |r| r.get(0),
            )
            .unwrap();
        match arm {
            0 => {
                db.execute("UPDATE working_files SET body=json_set(body,'$.dirty',json('true')) WHERE id=?1", [&old]).unwrap();
            }
            1 => {
                db.execute(
                    "UPDATE native_detached_streams SET owner=?1",
                    [Uuid::new_v4().to_string()],
                )
                .unwrap();
            }
            2 => {
                db.execute(
                    "UPDATE native_working_transfers SET successor=?1",
                    [Uuid::new_v4().to_string()],
                )
                .unwrap();
            }
            3 => {
                db.execute(
                    "UPDATE native_working_transfers SET predecessor=?1",
                    [Uuid::new_v4().to_string()],
                )
                .unwrap();
            }
            4 => {
                db.execute("UPDATE namespace_objects SET body=json_set(body,'$.follows_remote',json('true')) WHERE id=?1", [&old]).unwrap();
            }
            5 => {
                db.execute("UPDATE native_detached_streams SET binding=json_set(binding,'$.source.etag','changed')", []).unwrap();
            }
            6 => {
                let path = temp.path().join("journal/working").join(&old);
                let mut bytes = std::fs::read(&path).unwrap();
                bytes[0] ^= 1;
                std::fs::write(path, bytes).unwrap();
            }
            7 => {
                db.execute("DELETE FROM native_detached_streams", [])
                    .unwrap();
            }
            _ => {
                db.execute("UPDATE namespace_objects SET body=json_set(body,'$.node.parent_id','foreign') WHERE id=?1", [&old]).unwrap();
            }
        }
        drop(db);
        let journal =
            RecoveryJournal::open(&temp.path().join("journal"), &plan.account.to_string()).unwrap();
        assert!(
            fuse_journal_binding(&plan, &journal).is_err(),
            "detached arm {arm} accepted"
        );
    }
}

#[path = "format_tests.rs"]
mod format_tests;
