#![allow(clippy::unwrap_used)]
use super::*;

fn formats() -> [FuseFormat; 2] {
    [FuseFormat::Pages, FuseFormat::Keynote]
}

#[test]
fn owned_iwork_fuse_explicit_formats_accept_only_matching_source_contract() {
    for format in formats() {
        let (temp, master) = sources_for(format);
        let p = temp.path().join("source-registration.json");
        let digest = write_registration(&p, &master);
        assert!(
            verify_sources(&p, &digest, format).is_ok(),
            "explicit matching FUSE source refused"
        );
        assert!(
            icloud_owned_fuse_source_verify(&p, &digest).is_err(),
            "Numbers route accepted other format"
        );
        let other = if format == FuseFormat::Pages {
            FuseFormat::Keynote
        } else {
            FuseFormat::Pages
        };
        assert!(
            verify_sources(&p, &digest, other).is_err(),
            "cross-format route accepted"
        );
        let _retained = temp.keep();
    }
}
#[test]
fn owned_iwork_fuse_registration_rejects_foreign_label_with_other_fields_valid() {
    for format in formats() {
        let (temp, mut plan, j) = fixture_format(false, false, false, false, format);
        plan.validate().unwrap();
        plan.label = "iCloudNumbersFuseValidation".into();
        assert!(plan.validate().is_err(), "foreign label accepted");
        drop(j);
        let _retained = temp.keep();
    }
}
#[test]
fn owned_iwork_fuse_source_rewrite_rejects_wrong_canonical_root() {
    for format in formats() {
        let (temp, mut master) = sources_for(format);
        // Physical bytes, hash, path and semantic are valid; only canonical B root is wrong.
        master.source_b = source(&master.source_b.path, &format.source_root(), b"B edited");
        assert_eq!(master.source_b.semantic, master.source_b_original.semantic);
        assert!(
            master.verify().is_err(),
            "noncanonical rewrite root accepted"
        );
        let _retained = temp.keep();
    }
}
#[test]
fn owned_iwork_fuse_sources_reject_valid_zip_with_changed_registered_digest() {
    for format in formats() {
        let (temp, mut master) = sources_for(format);
        master.source_b.sha256 = "f".repeat(64);
        assert!(master.verify().is_err(), "wrong raw source digest accepted");
        let _retained = temp.keep();
    }
}
#[test]
fn owned_iwork_fuse_capture_checks_held_a_and_current_b_semantics() {
    for format in formats() {
        let (temp, master) = sources_for(format);
        let master_path = temp.path().join("source-registration.json");
        let master_digest = write_registration(&master_path, &master);
        for purpose in [CapturePurpose::HeldA, CapturePurpose::CurrentB] {
            let value = if matches!(purpose, CapturePurpose::HeldA) {
                b"A original".as_slice()
            } else {
                b"B edited".as_slice()
            };
            let mut plan = CaptureRegistration {
                version: 1,
                run: master.run,
                session_directory: temp.path().to_owned(),
                source_registration_sha256: master_digest.clone(),
                purpose,
                source: source(
                    &temp
                        .path()
                        .join(format!("{}.{}", purpose.stem(), format.extension())),
                    &master.name(),
                    value,
                ),
            };
            let path = temp
                .path()
                .join(format!("{}-registration.json", purpose.stem()));
            let digest = write_registration(&path, &plan);
            verify_capture(&path, &digest, format).unwrap();
            plan.source = source(&plan.source.path, &master.name(), b"other content");
            let digest = write_registration(&path, &plan);
            assert!(
                verify_capture(&path, &digest, format).is_err(),
                "wrong captured content accepted"
            );
        }
        let _retained = temp.keep();
    }
}
#[test]
fn owned_iwork_fuse_actual_atomic_journal_accepts_exact_detached_original() {
    for format in formats() {
        let (temp, plan, j) = fixture_format(true, true, false, true, format);
        let journal = ro(&temp, &plan, j);
        let proof = fuse_journal_binding(&plan, &journal).unwrap();
        assert_eq!(proof.5, (2, 1));
        assert_eq!(proof.2.name, plan.sources().name());
        assert!(proof.2.package && proof.2.kind == NodeKind::Folder);
        assert_eq!(
            proof.3.unwrap().id,
            proof
                .0
                .iter()
                .find(|r| r.id == plan.import)
                .unwrap()
                .remote
                .as_ref()
                .unwrap()
                .id
        );
        drop(journal);
        let _retained = temp.keep();
    }
}
#[test]
fn owned_iwork_fuse_refuses_cli_receipt_without_atomic_working_authority() {
    for format in formats() {
        let (temp, plan, j) = fixture_format(true, false, false, false, format);
        let journal = ro(&temp, &plan, j);
        journal_binding(&plan.receipt_plan(), &journal).unwrap();
        assert!(
            fuse_journal_binding(&plan, &journal).is_err(),
            "CLI receipt substituted for native working save"
        );
        drop(journal);
        let _retained = temp.keep();
    }
}
#[test]
fn owned_iwork_fuse_refuses_foreign_working_identity_with_valid_receipts() {
    for format in formats() {
        let (temp, mut plan, j) = fixture_format(true, true, false, true, format);
        let journal = ro(&temp, &plan, j);
        fuse_journal_binding(&plan, &journal).unwrap();
        plan.working = Some(Uuid::new_v4());
        assert!(
            fuse_journal_binding(&plan, &journal).is_err(),
            "foreign working UUID accepted"
        );
        drop(journal);
        let _retained = temp.keep();
    }
}
#[test]
fn owned_iwork_fuse_refuses_changed_original_revision_with_valid_association() {
    for format in formats() {
        let (temp, plan, j) = fixture_format(true, true, false, true, format);
        let journal = ro(&temp, &plan, j);
        let (rows, _, _, _, actual, _) = fuse_journal_binding(&plan, &journal).unwrap();
        let row = rows.iter().find(|r| Some(r.id) == plan.save).unwrap();
        let mut original = rows
            .iter()
            .find(|r| r.id == plan.import)
            .unwrap()
            .remote
            .clone()
            .unwrap();
        original.etag = Some("foreign-r2".into());
        assert!(
            association_binding(&plan, row, &original, actual.as_ref().unwrap()).is_err(),
            "changed original revision accepted"
        );
        drop(journal);
        let _retained = temp.keep();
    }
}
#[test]
fn owned_iwork_fuse_original_window_is_required_and_not_renewed() {
    for format in formats() {
        let (temp, mut plan, j) = fixture_format(false, false, false, false, format);
        plan.validate().unwrap();
        let w = plan.original_window.clone().unwrap();
        assert_eq!(w.remaining_at(plan.run, 1).unwrap().as_millis(), 1_080_000);
        assert!(w.remaining_at(plan.run, 1_080_001).is_err());
        assert!(w.remaining_at(Uuid::new_v4(), 1).is_err());
        plan.original_window = None;
        assert!(plan.validate().is_err(), "missing original window accepted");
        drop(j);
        let _retained = temp.keep();
    }
}
#[test]
fn owned_iwork_fuse_dispatch_refuses_expiry_after_synchronous_local_work() {
    let end = tokio::time::Instant::now() + std::time::Duration::from_millis(1);
    std::thread::sleep(std::time::Duration::from_millis(10));
    assert!(
        fuse_active(Some(end)).is_err(),
        "expired pre-dispatch original clock accepted"
    );
    assert!(fuse_active(None).is_ok(), "legacy Numbers boundary changed");
}
#[test]
fn owned_iwork_fuse_publication_overrun_does_not_return_success() {
    let end = tokio::time::Instant::now() + std::time::Duration::from_millis(50);
    let mut published = false;
    let result = fuse_publish(Some(end), || {
        published = true;
        std::thread::sleep(std::time::Duration::from_millis(60));
        Ok(())
    });
    assert!(published, "fixture did not reach synchronous publication");
    assert!(result.is_err(), "late synchronous publication accepted");
}
