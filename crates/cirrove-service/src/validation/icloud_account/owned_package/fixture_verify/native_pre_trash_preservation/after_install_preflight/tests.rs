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
