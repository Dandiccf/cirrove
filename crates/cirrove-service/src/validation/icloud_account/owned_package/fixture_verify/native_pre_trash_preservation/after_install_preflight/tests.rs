#![allow(clippy::unwrap_used)]
use super::*;
fn setup() -> (
    InstallRegistration,
    InstallMarker,
    UploadRecord,
    UploadRecord,
) {
    let (r, m, row, mut other) = super::super::tests::fixture();
    let name = original(&row).unwrap().name.clone();
    other.intent = UploadIntent::Create {
        parent: r.parent.id.clone(),
        name: name.clone(),
    };
    other.remote.as_mut().unwrap().name = name;
    let m = InstallMarker {
        version: 1,
        run: m.run,
        operation: m.operation,
        attempt: m.attempt,
        checkpoint_sha256: m.checkpoint_sha256,
        phase: "after-final-install-preflight".into(),
        rename_called: false,
        original_id: m.original_id,
        staged_id: m.staged_id,
        parent_id: m.parent_id,
        scope: scope(&r),
    };
    (
        InstallRegistration {
            authority: r,
            checkpoint_cipher_sha256: "f".repeat(64),
            baseline: None,
        },
        m,
        row,
        other,
    )
}
fn identities(x: &InstallRegistration, m: &InstallMarker) -> InstallIdentities {
    let r = &x.authority;
    let entry = |id: &str, name: &str| -> DriveEntry {
        serde_json::from_value(serde_json::json!({"drivewsid":id,"docwsid":id.strip_prefix("FILE::com.apple.CloudDocs::").unwrap(),"zone":"com.apple.CloudDocs","parentId":r.parent.id,"type":"FILE","name":name,"extension":"numbers","etag":"v1","size":124})).unwrap()
    };
    InstallIdentities{parent:serde_json::from_value(serde_json::json!({"drivewsid":r.parent.id,"zone":"com.apple.CloudDocs","type":"FOLDER","name":r.parent.name})).unwrap(),
        original_id:m.original_id.clone(),trash_etag:"trash-v2".into(),
        stage:entry(&m.staged_id,"stage"),occupant:entry("FILE::com.apple.CloudDocs::occupant","target")}
}
#[test]
fn native_install_observer_rejects_foreign_operation_checkpoint_and_scope() {
    for arm in 0..10 {
        let (mut x, mut m, mut row, mut c) = setup();
        admission(&x, &m, &row, &c).unwrap();
        match arm {
            0 => m.scope.account = Uuid::new_v4().to_string(),
            1 => m.operation = Uuid::new_v4(),
            2 => m.attempt = Uuid::new_v4(),
            3 => m.staged_id = m.original_id.clone(),
            4 => m.parent_id = "foreign".into(),
            5 => m.rename_called = true,
            6 => m.phase = "handoff-move-old-armed".into(),
            7 => row.remote = c.remote.clone(),
            8 => c.scope.account = Uuid::new_v4().to_string(),
            9 => x.checkpoint_cipher_sha256 = "invalid".into(),
            _ => unreachable!(),
        }
        assert!(admission(&x, &m, &row, &c).is_err());
    }
}
#[test]
fn native_install_observer_accepts_retained_conflict_or_uncertainty_not_uploaded() {
    for state in [UploadState::Conflict, UploadState::VerifyRequired] {
        let (mut x, m, mut row, c) = setup();
        x.authority.phase = Phase::Preservation;
        x.baseline = Some(identities(&x, &m));
        row.state = state;
        row.attempt = None;
        row.failed_attempts = 1;
        admission(&x, &m, &row, &c).unwrap();
        row.state = UploadState::Uploaded;
        assert!(admission(&x, &m, &row, &c).is_err());
    }
    assert_eq!(category(Ok(Reconciliation::Conflict)).unwrap(), "conflict");
    assert_eq!(category(Err(UploadError::Uncertain)).unwrap(), "uncertain");
    assert!(category(Ok(Reconciliation::Uncommitted)).is_err());
    assert!(category(Err(UploadError::CheckpointInvalid)).is_err());
}
#[test]
fn native_install_observer_identity_selection_and_declared_b_rename_only() {
    let (x, m, _, _) = setup();
    let before = identities(&x, &m);
    let duplicate = before.occupant.clone();
    let mut b = before.stage.clone();
    b.name = duplicate.name.clone();
    // A duplicate canonical name remains identifiable by distinct exact IDs.
    assert_eq!(
        by_id(
            &[duplicate.clone(), b.clone()],
            &b.drivewsid,
            &b.display_name(),
            &b.parent_id,
            Some(&b.etag)
        )
        .unwrap()
        .drivewsid,
        b.drivewsid
    );
    assert!(
        by_id(
            &[b.clone(), b.clone()],
            &b.drivewsid,
            &b.display_name(),
            &b.parent_id,
            None
        )
        .is_err()
    );
    let mut after = identities(&x, &m);
    identity_parity(&before, &after, "target.numbers").unwrap();
    after.stage.name = "target".into();
    after.stage.etag = "installed-v2".into();
    identity_parity(&before, &after, "target.numbers").unwrap();
    after.stage.parent_id = "foreign".into();
    assert!(identity_parity(&before, &after, "target.numbers").is_err());
    after = identities(&x, &m);
    after.occupant.etag = "changed".into();
    assert!(identity_parity(&before, &after, "target.numbers").is_err());
    after = identities(&x, &m);
    after.trash_etag = "changed".into();
    assert!(identity_parity(&before, &after, "target.numbers").is_err());
}

#[test]
fn native_install_preservation_registration_suffix_is_scoped() {
    let (mut x, _, _, _) = setup();
    let root = x.authority.session_directory.clone();
    assert_eq!(
        install_result_name(
            &x.authority,
            &root.join("install-preflight-capture-registration.json")
        )
        .unwrap(),
        "install-preflight-identities-captured.json"
    );
    x.authority.phase = Phase::Preservation;
    assert_eq!(
        install_result_name(
            &x.authority,
            &root.join("install-preflight-preservation-registration.json")
        )
        .unwrap(),
        "install-preflight-identities-preserved.json"
    );
    let id = Uuid::parse_str("a1b2c3d4-1234-4234-8234-123456789abc").unwrap();
    let name = format!("install-preflight-preservation-registration-{id}.json");
    assert_eq!(
        install_result_name(&x.authority, &root.join(&name)).unwrap(),
        format!("install-preflight-identities-preserved-{id}.json")
    );
    assert!(install_result_name(&x.authority, &root.join("foreign").join(&name)).is_err());
    for suffix in [
        "not-a-uuid",
        "00000000-0000-0000-0000-000000000000",
        "A1B2C3D4-1234-4234-8234-123456789ABC",
    ] {
        assert!(
            install_result_name(
                &x.authority,
                &root.join(format!(
                    "install-preflight-preservation-registration-{suffix}.json"
                ))
            )
            .is_err()
        );
    }
    assert!(install_result_name(&x.authority, &root.join(format!("{name}.extra"))).is_err());
    x.authority.phase = Phase::Capture;
    assert!(install_result_name(&x.authority, &root.join(&name)).is_err());
    assert!(
        install_result_name(
            &x.authority,
            &root.join(format!("install-preflight-capture-registration-{id}.json"))
        )
        .is_err()
    );
}

#[test]
fn native_install_failure_projection_contains_only_static_phase_and_booleans() {
    let (x, m, _, _) = setup();
    let mut entry = identities(&x, &m).stage;
    entry.name = "secret-provider-name".into();
    entry.etag = "https://private.invalid/?token=secret-revision".into();
    let diagnostic = InstallObservationDiagnostic {
        phase: InstallObservationPhase::StageSelection,
        stage: Some(entry_projection(
            &[entry],
            &m.staged_id,
            "expected.numbers",
            "canonical.numbers",
            &x.authority.parent.id,
            Some("expected-revision"),
        )),
        ..Default::default()
    };
    let value = serde_json::to_value(&diagnostic).unwrap();
    assert_eq!(value["phase"], "stage_selection");
    let text = serde_json::to_string(&value).unwrap();
    for forbidden in [
        "secret-provider-name",
        "secret-revision",
        "https://",
        m.staged_id.as_str(),
        x.authority.parent.id.as_str(),
    ] {
        assert!(!text.contains(forbidden));
    }
    for (key, field) in value.as_object().unwrap() {
        if key == "phase" {
            continue;
        }
        if let Some(projection) = field.as_object() {
            assert!(projection.values().all(serde_json::Value::is_boolean));
        } else {
            assert!(field.is_null() || field.is_boolean());
        }
    }
    assert_eq!(value["stage"]["selected_id_unique"], true);
    assert_eq!(value["stage"]["exact_name"], false);
    assert_eq!(value["stage"]["expected_revision"], false);
}

#[test]
fn native_install_preserves_provider_renamed_b_by_identity_and_content_authority() {
    let (x, m, _, _) = setup();
    let before = identities(&x, &m);
    let mut after = identities(&x, &m);
    after.stage.name = "target 2".into();
    after.stage.etag = "provider-renamed-v2".into();
    identity_parity(&before, &after, "target.numbers").unwrap();
    let selected = stage_by_id(
        &[after.stage.clone()],
        &m.staged_id,
        &before.stage.display_name(),
        &before.stage.parent_id,
        Some(&before.stage.etag),
        Phase::Preservation,
    )
    .unwrap();
    assert_eq!(selected.drivewsid, before.stage.drivewsid);
    assert!(
        stage_by_id(
            &[after.stage.clone()],
            &m.staged_id,
            &before.stage.display_name(),
            &before.stage.parent_id,
            Some(&before.stage.etag),
            Phase::Capture
        )
        .is_err()
    );
    for arm in 0..10 {
        let mut changed = identities(&x, &m);
        changed.stage = after.stage.clone();
        match arm {
            0 => changed.stage.drivewsid = "foreign".into(),
            1 => changed.stage.parent_id = "foreign".into(),
            2 => changed.stage.docwsid = "foreign".into(),
            3 => changed.stage.size += 1,
            4 => changed.stage.extension = "pages".into(),
            5 => changed.stage.name = "../foreign".into(),
            6 => changed.occupant.etag = "changed".into(),
            7 => changed.trash_etag = "changed".into(),
            8 => changed.parent.etag = "changed".into(),
            9 => changed.stage.name = "bad\nname".into(),
            _ => unreachable!(),
        }
        assert!(identity_parity(&before, &changed, "target.numbers").is_err());
    }
    // The archive content proof still runs after this metadata admission;
    // neither a renamed node nor an unchanged size proves the source contents.
}

#[test]
fn native_install_archive_wrapper_does_not_replace_identity_or_content_authority() {
    use base64::Engine as _;
    fn archive(encoded: &str) -> (File, PackageDownload) {
        let raw = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        let mut file = tempfile::tempfile().unwrap();
        std::io::Write::write_all(&mut file, &raw).unwrap();
        (
            file,
            PackageDownload {
                size: raw.len() as u64,
                sha256: hex::encode(Sha256::digest(&raw)),
            },
        )
    }
    let (x, m, _, _) = setup();
    let nodes = identities(&x, &m);
    let mut f = fixture(
        &x.authority,
        &x.authority.source_b,
        &nodes.parent,
        &nodes.stage,
    );
    let (file, receipt) = archive(
        "UEsDBBQAAAAAAAAAIQB5X0WyGAAAABgAAAAhAAAAdGFyZ2V0Lm51bWJlcnMvSW5kZXgvRG9jdW1lbnQuaXdhb3duZWQgc3ludGhldGljIGNvbnRlbnRzUEsBAhQDFAAAAAAAAAAhAHlfRbIYAAAAGAAAACEAAAAAAAAAAAAAAIABAAAAAHRhcmdldC5udW1iZXJzL0luZGV4L0RvY3VtZW50Lml3YVBLBQYAAAAAAQABAE8AAABXAAAAAAA=",
    );
    let canonical = "target.numbers";
    f.semantic = Some(
        cirrove_icloud::package_archive_semantic_identity_versioned(
            &file,
            &receipt,
            canonical,
            2,
            &CancellationToken::new(),
        )
        .unwrap(),
    );
    f.expected_root = "target 2.numbers".into();
    proof_stage_archive(&f, &file, &receipt, canonical, Phase::Preservation).unwrap();
    assert!(proof_stage_archive(&f, &file, &receipt, canonical, Phase::Capture).is_err());
    for encoded in [
        "UEsDBBQAAAAAAAAAIQADVNt2GgAAABoAAAAhAAAAdGFyZ2V0Lm51bWJlcnMvSW5kZXgvRG9jdW1lbnQuaXdhY2hhbmdlZCBzeW50aGV0aWMgY29udGVudHNQSwECFAMUAAAAAAAAACEAA1TbdhoAAAAaAAAAIQAAAAAAAAAAAAAAgAEAAAAAdGFyZ2V0Lm51bWJlcnMvSW5kZXgvRG9jdW1lbnQuaXdhUEsFBgAAAAABAAEATwAAAFkAAAAAAA==",
        "UEsDBBQAAAAAAAAAIQB5X0WyGAAAABgAAAAiAAAAZm9yZWlnbi5udW1iZXJzL0luZGV4L0RvY3VtZW50Lml3YW93bmVkIHN5bnRoZXRpYyBjb250ZW50c1BLAQIUAxQAAAAAAAAAIQB5X0WyGAAAABgAAAAiAAAAAAAAAAAAAACAAQAAAABmb3JlaWduLm51bWJlcnMvSW5kZXgvRG9jdW1lbnQuaXdhUEsFBgAAAAABAAEAUAAAAFgAAAAAAA==",
    ] {
        let (bad, bad_receipt) = archive(encoded);
        assert!(
            proof_stage_archive(&f, &bad, &bad_receipt, canonical, Phase::Preservation).is_err()
        );
    }
    f.expected_root = canonical.into();
    proof_stage_archive(&f, &file, &receipt, canonical, Phase::Capture).unwrap();
}
