use super::*;
use crate::journal::UploadJournal;
use cirrove_core::upload::RecoveryLocation;
type FixtureInputs = (Registration, Vec<u8>, Vec<u8>, Vec<u8>, Account, PathBuf);
fn fixture() -> Result<FixtureInputs> {
    let retained_root = tempfile::tempdir()?.keep();
    std::fs::set_permissions(&retained_root, std::fs::Permissions::from_mode(0o700))?;
    let account_id = Uuid::new_v4();
    let writer = Uuid::new_v4();
    let observer = Uuid::new_v4();
    let root = PathBuf::from(format!("/var/tmp/cirrove-calc-trash-observer-{observer}"));
    let parent = Node {
        id: "FOLDER::com.apple.CloudDocs::parent".into(),
        parent_id: Some(cirrove_icloud::ROOT_ID.into()),
        name: format!("Cirrove-Native-{writer}"),
        kind: NodeKind::Folder,
        size: 0,
        modified_unix: 0,
        etag: Some("parent-etag".into()),
        content_version: None,
        target: None,
        package: false,
    };
    let original = Node {
        id: "FILE::com.apple.CloudDocs::original-A".into(),
        parent_id: Some(parent.id.clone()),
        name: format!("Cirrove-Calc-{writer}.xlsx"),
        kind: NodeKind::File,
        size: 3,
        modified_unix: 1,
        etag: Some("A-etag".into()),
        content_version: Some("A-etag".into()),
        target: None,
        package: false,
    };
    let current = Node {
        id: "FILE::com.apple.CloudDocs::current-B".into(),
        size: 8,
        etag: Some("B-etag".into()),
        content_version: Some("B-etag".into()),
        ..original.clone()
    };
    let backup = Node {
        parent_id: Some("FOLDER::com.apple.CloudDocs::TRASH_ROOT".into()),
        modified_unix: 0,
        etag: Some("trash-etag".into()),
        content_version: Some("trash-etag".into()),
        ..original.clone()
    };
    let mut journal = UploadJournal::open(
        &retained_root.join("journal"),
        &account_id.to_string(),
        LIMIT,
    )?;
    let scope = Scope {
        account: account_id.to_string(),
        provider: "icloud".into(),
        collection: "drive".into(),
    };
    let working = journal.create_working(scope, original.clone(), false, &b"old"[..])?;
    journal.write_working(working.id, 0, b"new-body")?;
    let upload = journal.seal_working(working.id)?.context("sealed target")?;
    let claim = journal.claim_next()?.context("claimed target")?;
    let attempt = claim.attempt.context("target attempt")?;
    journal.reserve_identity_handoff(
        upload.id,
        attempt,
        RecoveryLocation::Trash {
            local_name: "recovery-A".into(),
            parent: "FOLDER::com.apple.CloudDocs::TRASH_ROOT".into(),
        },
    )?;
    journal.acknowledge_identity_handoff(upload.id, attempt, current, backup.clone())?;
    let target = serde_json::to_vec(&journal.get(upload.id)?)?;
    let writer_root = PathBuf::from(format!("/var/tmp/cirrove-calc-editor-{writer}"));
    let source_sha = hex::encode(Sha256::digest(b"old"));
    let parent_entry: DriveEntry = serde_json::from_value(serde_json::json!({
        "drivewsid":parent.id,"docwsid":"parent","type":"FOLDER","zone":"com.apple.CloudDocs",
        "name":parent.name,"parentId":cirrove_icloud::ROOT_ID,"etag":"actual-parent-revision"}))?;
    let document: DriveEntry = serde_json::from_value(serde_json::json!({
        "drivewsid":original.id,"docwsid":"original-A","type":"FILE","zone":"com.apple.CloudDocs",
        "name":format!("Cirrove-Calc-{writer}"),"extension":"xlsx","parentId":parent.id,
        "etag":"A-etag","size":3}))?;
    let settings_sha = "a".repeat(64);
    let f = serde_json::to_vec(
        &serde_json::json!({"version":1,"run":writer,"account":account_id,
        "session_directory":writer_root,"settings_sha256":settings_sha,"parent":parent_entry,
        "document":document,"format":"xlsx","representation":"data","source":writer_root.join("source-a.xlsx"),
        "source_size":3,"source_sha256":source_sha,"source_root":null,"expected_root":original.name,"semantic":null}),
    )?;
    let f_sha = hex::encode(Sha256::digest(&f));
    let proof = serde_json::to_vec(
        &serde_json::json!({"run":writer,"account":account_id,"parent":parent.id,
        "item":original.id,"etag":"A-etag","representation":"data","format":"xlsx","size":3,
        "sha256":source_sha,"semantic":null,"manifest_sha256":f_sha,"content_identity_verified":true,
        "gui_fidelity_verified":false,"cloud_mutated":false}),
    )?;
    let account = Account {
        id: account_id.to_string(),
        label: "iCloudCalcTrashValidation".into(),
        registration: AppRegistration::ICloud,
        identity: cirrove_auth::Identity {
            tenant_id: String::new(),
            subject: "synthetic".into(),
            username: "fixture@example.invalid".into(),
            graph_user_id: String::new(),
            display_name: "Fixture".into(),
        },
        credential_id: Uuid::new_v4().to_string(),
        access: AccessMode::ReadOnly,
        drive: cirrove_core::CollectionInfo {
            id: "drive".into(),
            name: "iCloud Drive".into(),
            drive_type: "icloud_drive".into(),
            web_url: String::new(),
        },
        root_id: cirrove_icloud::ROOT_ID.into(),
        mount_path: root.join("mount"),
        enabled: true,
        poll_seconds: 3600,
        cache_bytes: LIMIT,
    };
    let r = Registration {
        version: 1,
        observer_run: observer,
        writer_run: writer,
        account: account_id,
        collection: "drive".into(),
        session_directory: root.clone(),
        root_dev: 1,
        root_ino: 1,
        settings_sha256: settings_sha,
        parent,
        original_a: original,
        backup_a: backup,
        target_operation: upload.id,
        source_a: Source {
            path: root.join("source-a.xlsx"),
            size: 3,
            sha256: source_sha,
            root: None,
        },
        prior_a_fixture: Pin {
            path: root.join("prior-a-fixture.json"),
            sha256: f_sha,
        },
        prior_a_read_receipt: Pin {
            path: root.join("prior-a-read-receipt.json"),
            sha256: hex::encode(Sha256::digest(&proof)),
        },
        completed_target_upload: Pin {
            path: root.join("target-upload.json"),
            sha256: hex::encode(Sha256::digest(&target)),
        },
        started_unix_seconds: 1000,
        deadline_unix_seconds: 1090,
    };
    Ok((r, f, proof, target, account, retained_root))
}
fn decode_registration(r: &Registration) -> Result<Registration> {
    let raw = serde_json::to_vec(r)?;
    registered(&raw, &hex::encode(Sha256::digest(&raw)), 1001)
}
fn check_values(r: &Registration, f: &[u8], p: &[u8], u: &[u8]) -> Result<()> {
    let checked = decode_registration(r)?;
    historical(&checked, f, p, u)
}
#[test]
fn owned_calc_trash_registration_binds_fresh_a_fixture_and_target_backup() -> Result<()> {
    let (r, f, p, u, _, _temp) = fixture()?;
    check_values(&r, &f, &p, &u)?;
    assert_eq!(active_remaining(&r, 1001)?, 69);
    // Known explicitly false Node package fields are not unknown fields.
    let mut raw = serde_json::to_value(&r)?;
    raw["original_a"]["package"] = false.into();
    let raw = serde_json::to_vec(&raw)?;
    registered(&raw, &hex::encode(Sha256::digest(&raw)), 1001)?;
    let mut ordinary: serde_json::Value = serde_json::from_slice(&u)?;
    ordinary["representation"] =
        serde_json::to_value(cirrove_core::upload::UploadRepresentation::FileBytes)?;
    historical(&r, &f, &p, &serde_json::to_vec(&ordinary)?)?;
    Ok(())
}
#[test]
fn owned_calc_trash_registration_refuses_foreign_original_and_shadow_backup() -> Result<()> {
    let (r, f, p, u, _, _temp) = fixture()?;
    for arm in 0..7 {
        let mut bad = r.clone();
        match arm {
            0 => bad.original_a.id = "FILE::com.apple.CloudDocs::shadow".into(),
            1 => bad.backup_a.id = "FILE::com.apple.CloudDocs::shadow".into(),
            2 => bad.backup_a.parent_id = Some(r.parent.id.clone()),
            3 => bad.backup_a.etag = Some("substituted-revision".into()),
            4 => bad.backup_a.content_version = Some("substituted-content".into()),
            5 => bad.target_operation = Uuid::new_v4(),
            _ => bad.writer_run = Uuid::new_v4(),
        }
        assert!(
            check_values(&bad, &f, &p, &u).is_err(),
            "original/backup arm {arm}"
        );
    }
    Ok(())
}
#[test]
fn owned_calc_trash_registration_refuses_account_collection_package_and_receipt_drift() -> Result<()>
{
    let (r, f, p, u, a, _temp) = fixture()?;
    for arm in 0..9 {
        let mut bad: serde_json::Value = serde_json::from_slice(&u)?;
        match arm {
            0 => bad["scope"]["account"] = Uuid::new_v4().to_string().into(),
            1 => bad["scope"]["collection"] = "com.apple.CloudDocs".into(),
            2 => bad["scope"]["provider"] = "onedrive".into(),
            3 => bad["state"] = "uploading".into(),
            4 => bad["transferred_bytes"] = 0.into(),
            5 => bad["failed_attempts"] = 1.into(),
            6 => bad["identity_handoff"]["backup"]["package"] = true.into(),
            7 => bad["identity_handoff"]["old_item"] = "foreign".into(),
            _ => bad["identity_handoff"]["unknown_authority"] = true.into(),
        }
        assert!(
            historical(&r, &f, &p, &serde_json::to_vec(&bad)?).is_err(),
            "target arm {arm}"
        );
    }
    for access in [AccessMode::ReadOnly, AccessMode::ReadWrite] {
        let mut a = a.clone();
        a.access = access;
        let raw = serde_json::to_vec(&Settings {
            version: 2,
            accounts: vec![a],
        })?;
        let mut reg = r.clone();
        reg.settings_sha256 = hex::encode(Sha256::digest(&raw));
        assert_eq!(account(&reg, &raw).is_ok(), access == AccessMode::ReadOnly);
    }
    let mut value = serde_json::to_value(&r)?;
    value["original_a"]["made_up_revision"] = true.into();
    let raw = serde_json::to_vec(&value)?;
    assert!(registered(&raw, &hex::encode(Sha256::digest(&raw)), 1001).is_err());
    Ok(())
}
#[test]
fn owned_calc_trash_registration_refuses_foreign_a_proof_digest_format_and_size() -> Result<()> {
    let (r, f, p, u, _, _temp) = fixture()?;
    for arm in 0..8 {
        let mut value: serde_json::Value = serde_json::from_slice(&p)?;
        match arm {
            0 => value["run"] = Uuid::new_v4().to_string().into(),
            1 => value["account"] = Uuid::new_v4().to_string().into(),
            2 => value["item"] = "FILE::com.apple.CloudDocs::foreign".into(),
            3 => value["manifest_sha256"] = "b".repeat(64).into(),
            4 => value["format"] = "numbers".into(),
            5 => value["size"] = 5281.into(),
            6 => value["content_identity_verified"] = false.into(),
            _ => value["unknown_confirmation"] = true.into(),
        }
        assert!(
            historical(&r, &f, &serde_json::to_vec(&value)?, &u).is_err(),
            "A proof arm {arm}"
        );
    }
    let mut reg = r.clone();
    reg.prior_a_fixture.sha256 = "b".repeat(64);
    assert!(historical(&reg, &f, &p, &u).is_err());
    let mut reg = r.clone();
    reg.source_a.root = Some("Source.xlsx".into());
    assert!(decode_registration(&reg).is_err());
    Ok(())
}
#[test]
fn owned_calc_trash_registration_refuses_window_duplicate_and_changed_source() -> Result<()> {
    let (r, _, _, _, _, _temp) = fixture()?;
    for clock in [999, 1070, 1090] {
        assert!(active_remaining(&r, clock).is_err());
    }
    let mut bad = r.clone();
    bad.deadline_unix_seconds = 1091;
    assert!(decode_registration(&bad).is_err());
    let retained_root = tempfile::tempdir()?.keep();
    std::fs::set_permissions(&retained_root, std::fs::Permissions::from_mode(0o700))?;
    let path = retained_root.join("once.json");
    let value = serde_json::json!({"run":r.observer_run});
    immutable_record(&path, &value)?;
    assert!(immutable_record(&path, &value).is_err());
    assert_eq!(
        std::fs::metadata(&path)?.permissions().mode() & 0o777,
        0o400
    );
    let raw = retained_root.join("source-a.xlsx");
    std::fs::write(&raw, b"old")?;
    std::fs::set_permissions(&raw, std::fs::Permissions::from_mode(0o400))?;
    let mut reg = r.clone();
    reg.source_a.path = raw.clone();
    let mut held = open_private(&raw, false, LIMIT)?;
    source(&reg, &mut held)?;
    std::fs::set_permissions(&raw, std::fs::Permissions::from_mode(0o600))?;
    assert!(source(&reg, &mut held).is_err());
    std::fs::set_permissions(&raw, std::fs::Permissions::from_mode(0o400))?;
    reg.source_a.sha256 = "f".repeat(64);
    assert!(source(&reg, &mut held).is_err());
    Ok(())
}
