#![allow(clippy::unwrap_used)]
use super::*;
fn plan(arm: Arm) -> Registration {
    let run = Uuid::new_v4();
    let account = Uuid::new_v4();
    let parent:Node=serde_json::from_value(serde_json::json!({"id":"FOLDER::com.apple.CloudDocs::owned",
        "parent_id":cirrove_icloud::ROOT_ID,"name":format!("Cirrove-Native-{run}"),"kind":"folder","size":0,"etag":"creator-e1"})).unwrap();
    let (dir, label, name, root, file) = match arm {
        Arm::PagesDesktop => (
            format!("/var/tmp/cirrove-pages-desktop-{run}"),
            "iCloudPagesDesktopValidation",
            format!("Cirrove-Pages-Desktop-{run}.pages"),
            format!("Cirrove-Pages-Desktop-{run}.pages"),
            "mounted-import.pages",
        ),
        Arm::NativeFinalPreflight | Arm::NativeFinalPostflight => (
            format!("/var/tmp/cirrove-native-final-{run}"),
            "iCloudNativeFinalValidation",
            format!("Cirrove-Numbers-Parent-{run}.numbers"),
            "Source.numbers".into(),
            "source-a.numbers",
        ),
    };
    let document:Node=serde_json::from_value(serde_json::json!({"id":"FILE::com.apple.CloudDocs::owned-document",
        "parent_id":parent.id,"name":name,"kind":"folder","package":true,"size":100,"etag":"file-e2"})).unwrap();
    Registration {
        version: 1,
        arm,
        run,
        account,
        label: label.into(),
        session_directory: PathBuf::from(&dir),
        settings_sha256: "a".repeat(64),
        parent_creation: Uuid::new_v4(),
        operation: Uuid::new_v4(),
        parent,
        document,
        source: Source {
            path: PathBuf::from(&dir).join(file),
            size: 123,
            sha256: "b".repeat(64),
            root,
            semantic: PackageSemanticIdentity {
                version: 2,
                entries: 2,
                files: 1,
                expanded_bytes: 5,
                sha256: "c".repeat(64),
            },
        },
        recovered: None,
        started_unix_seconds: 1000,
        deadline_unix_seconds: 1300,
    }
}
fn entries(r: &Registration) -> (DriveEntry, DriveEntry) {
    let parent:DriveEntry=serde_json::from_value(serde_json::json!({"drivewsid":r.parent.id,"docwsid":"parent-doc",
        "item_id":"actual-parent-item","zone":"com.apple.CloudDocs","name":r.parent.name,"type":"FOLDER",
        "etag":"actual-current-parent-e5","size":0,"numberOfItems":1})).unwrap();
    let suffix = if matches!(r.arm, Arm::PagesDesktop) {
        "pages"
    } else {
        "numbers"
    };
    let name = r.document.name.strip_suffix(&format!(".{suffix}")).unwrap();
    let doc:DriveEntry=serde_json::from_value(serde_json::json!({"drivewsid":r.document.id,"docwsid":"owned-document",
        "item_id":"actual-file-item","zone":"com.apple.CloudDocs","name":name,"extension":suffix,"parentId":r.parent.id,
        "etag":r.document.etag,"type":"FILE","size":r.document.size,"numberOfItems":7,
        "items":[{"drivewsid":"FILE::com.apple.CloudDocs::typed-child","type":"FILE","name":"typed-child"}]})).unwrap();
    (parent, doc)
}
#[test]
fn native_fixture_registration_binds_two_owned_arm_shapes() -> Result<()> {
    for arm in [Arm::PagesDesktop, Arm::NativeFinalPreflight] {
        let r = plan(arm);
        r.validate(1001)?;
        let bytes = serde_json::to_vec(&r)?;
        let hash = hex::encode(Sha256::digest(&bytes));
        registered(&bytes, &hash, 1001)?;
        let mut unknown: serde_json::Value = serde_json::from_slice(&bytes)?;
        unknown["unregistered"] = true.into();
        let extra = serde_json::to_vec(&unknown)?;
        assert!(registered(&extra, &hex::encode(Sha256::digest(&extra)), 1001).is_err());
        for case in 0..13 {
            let mut bad = r.clone();
            match case {
                0 => bad.run = Uuid::nil(),
                1 => bad.account = Uuid::nil(),
                2 => bad.label = "foreign".into(),
                3 => bad.session_directory = PathBuf::from("/var/tmp/other"),
                4 => bad.source.path = bad.source.path.with_file_name("other"),
                5 => bad.source.root = "Other.pages".into(),
                6 => bad.document.name = "other.pages".into(),
                7 => bad.document.parent_id = Some("foreign".into()),
                8 => bad.document.package = false,
                9 => bad.document.kind = NodeKind::File,
                10 => bad.document.etag = Some("*".into()),
                11 => bad.parent.id = cirrove_icloud::ROOT_ID.into(),
                _ => bad.source.semantic.version = 1,
            }
            assert!(
                bad.validate(1001).is_err(),
                "registered shape substitution {case} accepted"
            );
        }
        assert!(registered(&bytes, &"0".repeat(64), 1001).is_err());
    }
    Ok(())
}
#[test]
fn native_fixture_registration_rejects_expired_or_excessive_window() -> Result<()> {
    let r = plan(Arm::PagesDesktop);
    r.validate(1001)?;
    let mut excessive = r.clone();
    excessive.deadline_unix_seconds = 2801;
    assert!(
        excessive.validate(1001).is_err(),
        "excessive original lifetime accepted"
    );
    assert!(r.validate(1300).is_err(), "expired lifetime accepted");
    assert!(r.validate(999).is_err(), "future original start accepted");
    Ok(())
}
#[test]
fn native_fixture_actual_entries_preserve_optional_metadata() -> Result<()> {
    for arm in [Arm::PagesDesktop, Arm::NativeFinalPreflight] {
        let r = plan(arm);
        let (parent, document) = entries(&r);
        let actual_parent = exact_parent(&[parent], &r)?;
        let actual_doc = exact_entry(&[document.clone()], &r.document)?;
        assert_eq!(actual_parent.item_id, "actual-parent-item");
        assert_eq!(actual_parent.etag, "actual-current-parent-e5");
        assert!(actual_parent.number_of_items.is_none());
        assert!(actual_parent.items.is_empty());
        let encoded = serde_json::to_vec(&fixture_value(&r, &actual_parent, &actual_doc))?;
        let f: Fixture = serde_json::from_slice(&encoded)?;
        validate(&f)?;
        assert_eq!(f.document, document);
        assert_eq!(f.document.item_id, "actual-file-item");
        assert_eq!(f.document.number_of_items, Some(7));
        assert_eq!(f.document.items.len(), 1);
    }
    Ok(())
}
#[test]
fn native_fixture_actual_entries_reject_identity_and_revision_changes() -> Result<()> {
    let r = plan(Arm::PagesDesktop);
    let (parent, doc) = entries(&r);
    assert!(exact_parent(&[parent.clone(), parent.clone()], &r).is_err());
    assert!(exact_entry(&[doc.clone(), doc.clone()], &r.document).is_err());
    for case in 0..7 {
        let mut bad = doc.clone();
        match case {
            0 => bad.drivewsid = "foreign".into(),
            1 => bad.parent_id = "foreign".into(),
            2 => bad.etag = "changed".into(),
            3 => bad.size += 1,
            4 => bad.name = "other".into(),
            5 => bad.kind = "FOLDER".into(),
            _ => bad.zone = "foreign".into(),
        }
        assert!(
            exact_entry(&[bad], &r.document).is_err(),
            "actual entry substitution {case} accepted"
        );
    }
    Ok(())
}
fn account_bytes(r: &Registration) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(
        &serde_json::json!({"version":2,"accounts":[{"id":r.account,"label":r.label,
        "registration":{"provider":"i_cloud"},"identity":{"tenant_id":"icloud","subject":"synthetic","username":"synthetic.invalid","graph_user_id":"synthetic","display_name":"Synthetic"},
        "credential_id":"synthetic-only","access":"read_write","drive":{"id":"drive","name":"Drive","driveType":"icloud_drive"},
        "root_id":cirrove_icloud::ROOT_ID,"mount_path":r.session_directory.join("mount"),"enabled":true,"poll_seconds":2,"cache_bytes":1048576}]}),
    )?)
}
#[test]
fn native_fixture_source_and_settings_guards_refuse_changed_bytes() -> Result<()> {
    let mut r = plan(Arm::PagesDesktop);
    let bytes = account_bytes(&r)?;
    r.settings_sha256 = hex::encode(Sha256::digest(&bytes));
    bound_settings(&r, &bytes)?;
    let mut appended = bytes.clone();
    appended.push(b' ');
    assert!(bound_settings(&r, &appended).is_err());
    let mut foreign: serde_json::Value = serde_json::from_slice(&bytes)?;
    foreign["accounts"][0]["id"] = Uuid::new_v4().to_string().into();
    let foreign = serde_json::to_vec(&foreign)?;
    let mut registered_foreign = r.clone();
    registered_foreign.settings_sha256 = hex::encode(Sha256::digest(&foreign));
    assert!(bound_settings(&registered_foreign, &foreign).is_err());
    let mut foreign_type: serde_json::Value = serde_json::from_slice(&bytes)?;
    foreign_type["accounts"][0]["drive"]["driveType"] = "personal".into();
    let foreign_type = serde_json::to_vec(&foreign_type)?;
    let mut registered_type = r.clone();
    registered_type.settings_sha256 = hex::encode(Sha256::digest(&foreign_type));
    assert!(
        bound_settings(&registered_type, &foreign_type).is_err(),
        "foreign collection type accepted"
    );
    let dir = tempfile::tempdir()?;
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))?;
    let archive = zip_archive("Source.pages/Index/Document.iwa", b"synthetic content");
    let path = dir.path().join("source");
    std::fs::write(&path, &archive)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    let file = File::open(&path)?;
    let receipt = PackageDownload {
        size: archive.len() as u64,
        sha256: hex::encode(Sha256::digest(&archive)),
    };
    let semantic = cirrove_icloud::package_archive_semantic_identity_versioned(
        &file,
        &receipt,
        "Source.pages",
        2,
        &CancellationToken::new(),
    )?;
    let source = Source {
        path: path.clone(),
        size: receipt.size,
        sha256: receipt.sha256,
        root: "Source.pages".into(),
        semantic,
    };
    source_verified(&source)?;
    let mut altered = source.clone();
    altered.sha256 = "0".repeat(64);
    assert!(
        source_verified(&altered).is_err(),
        "registered source SHA substitution accepted"
    );
    let mut altered = source.clone();
    altered.semantic.sha256 = "0".repeat(64);
    assert!(source_verified(&altered).is_err());
    let alias = dir.path().join("alias");
    std::os::unix::fs::symlink(&path, &alias)?;
    let mut altered = source;
    altered.path = alias;
    assert!(source_verified(&altered).is_err());
    Ok(())
}

fn zip_archive(name: &str, bytes: &[u8]) -> Vec<u8> {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    crc = !crc;
    let size = u32::try_from(bytes.len()).unwrap();
    let len = u16::try_from(name.len()).unwrap();
    let mut out = Vec::new();
    out.extend(0x04034b50u32.to_le_bytes());
    for value in [20u16, 0, 0, 0, 33] {
        out.extend(value.to_le_bytes());
    }
    for value in [crc, size, size] {
        out.extend(value.to_le_bytes());
    }
    for value in [len, 0] {
        out.extend(value.to_le_bytes());
    }
    out.extend(name.as_bytes());
    out.extend(bytes);
    let offset = u32::try_from(out.len()).unwrap();
    out.extend(0x02014b50u32.to_le_bytes());
    for value in [20u16, 20, 0, 0, 0, 33] {
        out.extend(value.to_le_bytes());
    }
    for value in [crc, size, size] {
        out.extend(value.to_le_bytes());
    }
    for value in [len, 0, 0, 0, 0] {
        out.extend(value.to_le_bytes());
    }
    for value in [0u32, 0] {
        out.extend(value.to_le_bytes());
    }
    out.extend(name.as_bytes());
    let central = u32::try_from(out.len()).unwrap() - offset;
    out.extend(0x06054b50u32.to_le_bytes());
    for value in [0u16, 0, 1, 1] {
        out.extend(value.to_le_bytes());
    }
    for value in [central, offset] {
        out.extend(value.to_le_bytes());
    }
    out.extend(0u16.to_le_bytes());
    out
}

fn postflight_plan() -> Registration {
    let mut r = plan(Arm::NativeFinalPostflight);
    r.source.path = r.session_directory.join("source-b.numbers");
    r.source.semantic.entries = 2;
    r.document.size = 100; // Provider logical size is independent from semantic expansion.
    let mut original = r.document.clone();
    original.id = "FILE::com.apple.CloudDocs::original-A".into();
    original.size = 77; // Deliberately differs from source-A expanded bytes.
    original.etag = Some("A-v1".into());
    let mut backup = original.clone();
    backup.parent_id = Some("FOLDER::com.apple.CloudDocs::TRASH_ROOT".into());
    backup.etag = Some("A-trash-v2".into());
    let mut source_a = r.source.clone();
    source_a.path = r.session_directory.join("source-a.numbers");
    source_a.sha256 = "d".repeat(64);
    source_a.semantic.sha256 = "e".repeat(64);
    source_a.semantic.expanded_bytes = 7;
    r.recovered = Some(Recovered {
        original,
        backup,
        source_a,
    });
    r
}
#[test]
fn native_fixture_postflight_requires_two_exact_owned_source_and_receipt_shapes() -> Result<()> {
    let r = postflight_plan();
    r.validate(1001)?;
    assert_ne!(r.document.size, r.source.semantic.expanded_bytes);
    assert_ne!(
        r.recovered.as_ref().unwrap().original.size,
        r.recovered
            .as_ref()
            .unwrap()
            .source_a
            .semantic
            .expanded_bytes
    );
    let (parent, doc) = entries(&r);
    let parent = exact_parent(&[parent], &r)?;
    let actual = exact_entry(&[doc], &r.document)?;
    let fixture: Fixture = serde_json::from_value(fixture_value(&r, &parent, &actual))?;
    validate(&fixture)?;
    let bytes = serde_json::to_vec(&r)?;
    registered(&bytes, &hex::encode(Sha256::digest(&bytes)), 1001)?;
    assert_eq!(r.artifact_stem(), "native-final-postflight");
    for case in 0..16 {
        let mut bad = r.clone();
        match case {
            0 => bad.recovered = None,
            1 => bad.arm = Arm::NativeFinalPreflight,
            2 => bad.arm = Arm::PagesDesktop,
            3 => bad.source.path = bad.session_directory.join("source-a.numbers"),
            4 => {
                bad.recovered.as_mut().unwrap().source_a.path =
                    bad.session_directory.join("source-b.numbers")
            }
            5 => bad.recovered.as_mut().unwrap().original.id = bad.document.id.clone(),
            6 => bad.recovered.as_mut().unwrap().original.parent_id = Some("foreign".into()),
            7 => bad.recovered.as_mut().unwrap().original.name = "foreign.numbers".into(),
            8 => {
                bad.recovered.as_mut().unwrap().backup.id =
                    "FILE::com.apple.CloudDocs::foreign".into()
            }
            9 => bad.recovered.as_mut().unwrap().backup.parent_id = Some(bad.parent.id.clone()),
            10 => bad.recovered.as_mut().unwrap().backup.name = "foreign.numbers".into(),
            11 => bad.recovered.as_mut().unwrap().backup.size += 1,
            12 => bad.recovered.as_mut().unwrap().backup.etag = Some("*".into()),
            13 => bad.recovered.as_mut().unwrap().source_a.semantic.version = 1,
            14 => bad.recovered.as_mut().unwrap().source_a.semantic = bad.source.semantic.clone(),
            _ => bad.recovered.as_mut().unwrap().source_a.sha256 = bad.source.sha256.clone(),
        }
        assert!(
            bad.validate(1001).is_err(),
            "postflight substitution {case} accepted"
        );
    }
    let mut absent = serde_json::to_value(&r)?;
    absent.as_object_mut().unwrap().remove("recovered");
    let absent = serde_json::to_vec(&absent)?;
    assert!(registered(&absent, &hex::encode(Sha256::digest(&absent)), 1001).is_err());
    let mut unknown = serde_json::to_value(&r)?;
    unknown["recovered"]["foreign"] = true.into();
    let unknown = serde_json::to_vec(&unknown)?;
    assert!(registered(&unknown, &hex::encode(Sha256::digest(&unknown)), 1001).is_err());
    Ok(())
}
#[test]
fn native_fixture_postflight_trash_binding_rejects_revision_and_semantic_substitution() -> Result<()>
{
    let r = postflight_plan();
    let old = r.recovered.as_ref().unwrap();
    let mut actual = cirrove_icloud::VerifiedPackageTrash {
        archive: PackageDownload {
            size: 321,
            sha256: "f".repeat(64),
        },
        semantic: old.source_a.semantic.clone(),
        trash_etag: old.backup.etag.clone().unwrap(),
    };
    trash_binding(&old.backup, &old.source_a, &actual)?;
    actual.trash_etag = "changed".into();
    assert!(trash_binding(&old.backup, &old.source_a, &actual).is_err());
    actual.trash_etag = old.backup.etag.clone().unwrap();
    actual.semantic = r.source.semantic.clone();
    assert!(trash_binding(&old.backup, &old.source_a, &actual).is_err());
    Ok(())
}
#[test]
fn native_fixture_postflight_source_a_scanner_rejects_raw_or_semantic_tamper() -> Result<()> {
    let dir = tempfile::tempdir()?;
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))?;
    let raw = zip_archive(
        "Source.numbers/Index/Document.iwa",
        b"independent original A",
    );
    let path = dir.path().join("source-a.numbers");
    std::fs::write(&path, &raw)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    let file = File::open(&path)?;
    let receipt = PackageDownload {
        size: raw.len() as u64,
        sha256: hex::encode(Sha256::digest(&raw)),
    };
    let semantic = cirrove_icloud::package_archive_semantic_identity_versioned(
        &file,
        &receipt,
        "Source.numbers",
        2,
        &CancellationToken::new(),
    )?;
    let source = Source {
        path,
        size: receipt.size,
        sha256: receipt.sha256,
        root: "Source.numbers".into(),
        semantic,
    };
    source_verified(&source)?;
    let mut wrong = source.clone();
    wrong.sha256 = "0".repeat(64);
    assert!(source_verified(&wrong).is_err());
    let mut wrong = source.clone();
    wrong.semantic.sha256 = "0".repeat(64);
    assert!(source_verified(&wrong).is_err());
    let mut wrong = source;
    wrong.root = "Other.numbers".into();
    assert!(source_verified(&wrong).is_err());
    Ok(())
}
