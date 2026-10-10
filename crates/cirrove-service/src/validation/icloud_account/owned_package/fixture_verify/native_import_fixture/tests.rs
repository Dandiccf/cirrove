#![allow(clippy::unwrap_used)]
use super::*;
use std::{io::Write, os::unix::fs::DirBuilderExt};

struct NativeFinalFlatLocal {
    registration: Registration,
    files: Vec<(PathBuf, Vec<u8>)>,
    source_path: PathBuf,
}
fn native_final_flat_local(postflight: bool) -> Result<NativeFinalFlatLocal> {
    let mut registration = if postflight {
        postflight_plan()
    } else {
        plan(Arm::NativeFinalPreflight)
    };
    // Keep local byte fixtures on Root's registered TMPDIR. The registration
    // retains the exact NativeFinal namespace; no live scope is created here.
    let directory = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()?
        .keep();
    let mut files = Vec::new();
    let mut prepare = |source: &mut Source, content: &[u8]| -> Result<PathBuf> {
        let path = directory.join(source.path.file_name().context("source filename missing")?);
        let raw = zip_archive("Index/Document.iwa", content);
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        file.write_all(&raw)?;
        file.sync_all()?;
        source.size = raw.len() as u64;
        source.sha256 = hex::encode(Sha256::digest(&raw));
        source.root = None;
        source.semantic = cirrove_icloud::package_flat_archive_semantic_identity_v2(
            &file,
            &PackageDownload {
                size: source.size,
                sha256: source.sha256.clone(),
            },
            &CancellationToken::new(),
        )?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400))?;
        let mut local = source.clone();
        local.path = path.clone();
        source_verified(&local)?;
        files.push((path.clone(), raw));
        Ok(path)
    };
    let source_path = prepare(&mut registration.source, b"owned flat current B")?;
    if let Some(recovered) = &mut registration.recovered {
        prepare(&mut recovered.source_a, b"owned flat original A")?;
    }
    let bytes = serde_json::to_vec(&registration)?;
    let path = directory.join("registration.json");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o400)
        .open(&path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    files.push((path, bytes));
    Ok(NativeFinalFlatLocal {
        registration,
        files,
        source_path,
    })
}
fn native_final_flat_preserved(local: &NativeFinalFlatLocal) -> Result<()> {
    for (path, bytes) in &local.files {
        assert_eq!(&std::fs::read(path)?, bytes);
        assert_eq!(std::fs::metadata(path)?.permissions().mode() & 0o777, 0o400);
    }
    Ok(())
}
fn native_final_flat_consumer(local: &NativeFinalFlatLocal) -> Result<Registration> {
    let bytes = serde_json::to_vec(&local.registration)?;
    let desired = registered(&bytes, &hex::encode(Sha256::digest(&bytes)), 1001);
    // Actual local flat proof and unchanged bytes/modes precede the desired
    // admission assertion. Baseline refusal is collected, not a setup error.
    native_final_flat_preserved(local)?;
    assert!(
        desired.is_ok(),
        "NativeFinal explicit-null flat metadata registration must be admitted"
    );
    let registration = desired?;
    assert!(registration.subject_run.is_none());
    assert_eq!(
        registration.document.name,
        format!("Cirrove-Numbers-Parent-{}.numbers", registration.run)
    );
    let (parent, document) = entries(&registration);
    let parent = exact_parent(&[parent], &registration)?;
    let mut document = exact_entry(&[document], &registration.document)?;
    document.items.clear(); // Same documented narrow entry as the producer.
    let encoded = serde_json::to_vec(&fixture_value(&registration, &parent, &document))?;
    let fixture = super::super::decode(&encoded, &hex::encode(Sha256::digest(&encoded)))?;
    validate(&fixture)?;
    assert_eq!(fixture.run, registration.run);
    assert_eq!(fixture.document, document);
    assert!(fixture.source_root.is_none());
    assert_eq!(fixture.expected_root, registration.document.name);
    let file = File::open(&local.source_path)?;
    proof(
        &fixture,
        &file,
        &PackageDownload {
            size: registration.source.size,
            sha256: registration.source.sha256.clone(),
        },
        true,
    )?;
    native_final_flat_preserved(local)?;
    Ok(registration)
}

#[test]
fn native_fixture_native_final_flat_preflight_accepts_null_source_and_generic_consumer()
-> Result<()> {
    let local = native_final_flat_local(false)?;
    let r = native_final_flat_consumer(&local)?;
    assert!(matches!(r.arm, Arm::NativeFinalPreflight));
    assert!(r.recovered.is_none());
    assert_eq!(r.artifact_stem(), "native-import-fixture");
    let mut value = serde_json::to_value(&r)?;
    value["source"].as_object_mut().unwrap().remove("root");
    assert!(registered_value(&value).is_err());
    let mut wrong = r.clone();
    wrong.source.root = Some("Other.numbers".into());
    assert!(wrong.validate(1001).is_err());
    wrong = r.clone();
    wrong.subject_run = Some(Uuid::new_v4());
    assert!(wrong.validate(1001).is_err());
    wrong = r;
    wrong.document.name = format!("Cirrove-Numbers-Editor-{}.numbers", wrong.run);
    assert!(wrong.validate(1001).is_err());
    native_final_flat_preserved(&local)
}

#[test]
fn native_fixture_native_final_flat_postflight_accepts_null_sources_and_exact_trash_binding()
-> Result<()> {
    let local = native_final_flat_local(true)?;
    let r = native_final_flat_consumer(&local)?;
    assert!(matches!(r.arm, Arm::NativeFinalPostflight));
    assert_eq!(r.artifact_stem(), "native-final-postflight");
    let recovered = r.recovered.as_ref().unwrap();
    assert!(recovered.source_a.root.is_none());
    assert_ne!(recovered.source_a.semantic, r.source.semantic);
    let observed = cirrove_icloud::VerifiedPackageTrash {
        archive: PackageDownload {
            size: 321,
            sha256: "f".repeat(64),
        },
        semantic: recovered.source_a.semantic.clone(),
        trash_etag: recovered.backup.etag.clone().unwrap(),
    };
    trash_binding(&recovered.backup, &recovered.source_a, &observed)?;
    let mut bad = r.clone();
    bad.recovered.as_mut().unwrap().source_a.root = Some("Source.numbers".into());
    assert!(bad.validate(1001).is_err());
    bad = r.clone();
    bad.recovered.as_mut().unwrap().original.id = bad.document.id.clone();
    assert!(bad.validate(1001).is_err());
    let mut wrong_backup = recovered.backup.clone();
    wrong_backup.etag = Some("wrong-trash-revision".into());
    assert!(trash_binding(&wrong_backup, &recovered.source_a, &observed).is_err());
    let wrong_semantic = cirrove_icloud::VerifiedPackageTrash {
        semantic: r.source.semantic.clone(),
        ..observed
    };
    assert!(trash_binding(&recovered.backup, &recovered.source_a, &wrong_semantic).is_err());
    let mut value = serde_json::to_value(&r)?;
    value["recovered"]["source_a"]
        .as_object_mut()
        .unwrap()
        .remove("root");
    assert!(registered_value(&value).is_err());
    native_final_flat_preserved(&local)
}
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
        Arm::NativeFinalPreflight
        | Arm::NativeFinalPostflight
        | Arm::FlatNumbersPreflight
        | Arm::FlatNumbersPostflight => (
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
        subject_run: None,
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
            root: Some(root),
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
                5 => bad.source.root = Some("Other.pages".into()),
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
        root: Some("Source.pages".into()),
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
        root: Some("Source.numbers".into()),
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
    wrong.root = Some("Other.numbers".into());
    assert!(source_verified(&wrong).is_err());
    Ok(())
}

fn flat_registration(postflight: bool) -> Result<serde_json::Value> {
    let r = if postflight {
        postflight_plan()
    } else {
        plan(Arm::NativeFinalPreflight)
    };
    let subject = Uuid::new_v4();
    let dir = format!("/var/tmp/cirrove-numbers-flat-replacement-{}", r.run);
    let name = format!("Cirrove-Numbers-Editor-{subject}.numbers");
    let mut value = serde_json::to_value(&r)?;
    value["arm"] = if postflight {
        "flat_numbers_postflight".into()
    } else {
        "flat_numbers_preflight".into()
    };
    value["subject_run"] = subject.to_string().into();
    value["session_directory"] = dir.clone().into();
    value["label"] = "iCloudNumbersFlatReplacementValidation".into();
    value["parent"]["name"] = format!("Cirrove-Native-{subject}").into();
    value["document"]["name"] = name.clone().into();
    value["source"]["path"] = format!(
        "{dir}/source-{}.numbers",
        if postflight { "b" } else { "a" }
    )
    .into();
    value["source"]["root"] = serde_json::Value::Null;
    if postflight {
        value["recovered"]["original"]["name"] = name.clone().into();
        value["recovered"]["backup"]["name"] = name.into();
        value["recovered"]["source_a"]["path"] = format!("{dir}/source-a.numbers").into();
        value["recovered"]["source_a"]["root"] = serde_json::Value::Null;
    }
    Ok(value)
}

fn registered_value(value: &serde_json::Value) -> Result<Registration> {
    let bytes = serde_json::to_vec(value)?;
    registered(&bytes, &hex::encode(Sha256::digest(&bytes)), 1001)
}

fn flat_admission_and_fixture(postflight: bool) -> Result<()> {
    let value = flat_registration(postflight)?;
    let retained = serde_json::to_vec(&value)?;
    assert!(value["source"]["root"].is_null());
    assert_ne!(value["run"], value["subject_run"]);
    // Existing registration consumer is the desired endpoint; baseline refuses
    // the explicit flat arm before any session, artifact or provider access.
    let r = registered_value(&value)?;
    assert_eq!(serde_json::to_vec(&value)?, retained);
    let (parent, document) = entries(&r);
    let actual_parent = exact_parent(&[parent], &r)?;
    let actual_document = exact_entry(&[document.clone()], &r.document)?;
    let f: Fixture = serde_json::from_value(fixture_value(&r, &actual_parent, &actual_document))?;
    validate(&f)?;
    assert_eq!(serde_json::to_value(f.run)?, value["subject_run"]);
    assert_eq!(f.session_directory, r.session_directory);
    assert_eq!(f.document, document);
    assert!(f.source_root.is_none());
    assert_eq!(f.expected_root, r.document.name);
    assert_eq!(f.semantic.as_ref(), Some(&r.source.semantic));
    assert_eq!(
        r.artifact_stem(),
        if postflight {
            "flat-numbers-postflight"
        } else {
            "flat-numbers-preflight"
        }
    );
    Ok(())
}

#[test]
fn native_fixture_flat_preflight_admits_selected_subject_and_generates_valid_fixture() -> Result<()>
{
    flat_admission_and_fixture(false)
}

#[test]
fn native_fixture_flat_postflight_admits_selected_subject_and_generates_valid_fixture() -> Result<()>
{
    flat_admission_and_fixture(true)
}

#[test]
fn native_fixture_flat_registration_refuses_subject_scope_and_root_substitution() -> Result<()> {
    for postflight in [false, true] {
        let value = flat_registration(postflight)?;
        registered_value(&value)?;
        for case in 0..15 {
            let mut bad = value.clone();
            match case {
                0 => {
                    bad.as_object_mut().unwrap().remove("subject_run");
                }
                1 => bad["subject_run"] = serde_json::Value::Null,
                2 => bad["subject_run"] = Uuid::nil().to_string().into(),
                3 => bad["subject_run"] = bad["run"].clone(),
                4 => bad["subject_run"] = Uuid::new_v4().to_string().into(),
                5 => {
                    bad["source"].as_object_mut().unwrap().remove("root");
                }
                6 => bad["source"]["root"] = "Source.numbers".into(),
                7 => {
                    bad["session_directory"] = format!(
                        "/var/tmp/cirrove-numbers-flat-replacement-{}",
                        bad["subject_run"].as_str().unwrap()
                    )
                    .into()
                }
                8 => bad["label"] = "iCloudNativeFinalValidation".into(),
                9 => bad["source"]["path"] = "/var/tmp/foreign/source-a.numbers".into(),
                10 => {
                    bad["parent"]["name"] =
                        format!("Cirrove-Native-{}", bad["run"].as_str().unwrap()).into()
                }
                11 => {
                    bad["document"]["name"] = format!(
                        "Cirrove-Numbers-Editor-{}.numbers",
                        bad["run"].as_str().unwrap()
                    )
                    .into()
                }
                12 => bad["deadline_unix_seconds"] = 1001.into(),
                13 => bad["source"]["semantic"]["version"] = 1.into(),
                _ => bad["document"]["package"] = false.into(),
            }
            assert!(
                registered_value(&bad).is_err(),
                "flat scope/root substitution {postflight}/{case} accepted"
            );
        }
    }
    Ok(())
}

#[test]
fn native_fixture_flat_extension_preserves_legacy_subject_and_explicit_root_contracts() -> Result<()>
{
    for r in [
        plan(Arm::PagesDesktop),
        plan(Arm::NativeFinalPreflight),
        postflight_plan(),
    ] {
        let value = serde_json::to_value(&r)?;
        registered_value(&value)?;
        assert!(value.get("subject_run").is_none());
        for case in 0..3 {
            let mut bad = value.clone();
            match case {
                0 => bad["subject_run"] = Uuid::new_v4().to_string().into(),
                1 => bad["source"]["root"] = "Other.numbers".into(),
                _ => {
                    bad["source"].as_object_mut().unwrap().remove("root");
                }
            }
            assert!(
                registered_value(&bad).is_err(),
                "legacy subject/root substitution {case} accepted"
            );
        }
        // Pages still requires its exact wrapper. NativeFinal postflight
        // cannot mix a flat B with the registered wrapped original A.
        if matches!(r.arm, Arm::PagesDesktop | Arm::NativeFinalPostflight) {
            let mut bad = value.clone();
            bad["source"]["root"] = serde_json::Value::Null;
            assert!(registered_value(&bad).is_err());
        }
    }
    Ok(())
}

#[test]
fn native_fixture_flat_postflight_requires_distinct_current_original_and_flat_source_a()
-> Result<()> {
    let value = flat_registration(true)?;
    let r = registered_value(&value)?;
    let recovered = r.recovered.as_ref().context("postflight recovered A")?;
    let actual = cirrove_icloud::VerifiedPackageTrash {
        archive: PackageDownload {
            size: 321,
            sha256: "f".repeat(64),
        },
        semantic: recovered.source_a.semantic.clone(),
        trash_etag: recovered.backup.etag.clone().context("Trash revision")?,
    };
    trash_binding(&recovered.backup, &recovered.source_a, &actual)?;
    assert!(trash_binding(&recovered.backup, &r.source, &actual).is_err());
    for case in 0..10 {
        let mut bad = value.clone();
        match case {
            0 => {
                bad.as_object_mut().unwrap().remove("recovered");
            }
            1 => bad["recovered"]["source_a"]["root"] = "Source.numbers".into(),
            2 => {
                bad["recovered"]["source_a"]
                    .as_object_mut()
                    .unwrap()
                    .remove("root");
            }
            3 => bad["recovered"]["original"]["id"] = bad["document"]["id"].clone(),
            4 => bad["recovered"]["backup"]["id"] = bad["document"]["id"].clone(),
            5 => bad["recovered"]["backup"]["parent_id"] = bad["parent"]["id"].clone(),
            6 => bad["recovered"]["source_a"]["semantic"] = bad["source"]["semantic"].clone(),
            7 => bad["recovered"]["source_a"]["sha256"] = bad["source"]["sha256"].clone(),
            8 => bad["recovered"]["source_a"]["path"] = bad["source"]["path"].clone(),
            _ => bad["recovered"]["backup"]["etag"] = "*".into(),
        }
        assert!(
            registered_value(&bad).is_err(),
            "flat original/Trash/A substitution {case} accepted"
        );
    }
    let mut wrong = actual;
    wrong.trash_etag = "foreign-revision".into();
    assert!(trash_binding(&recovered.backup, &recovered.source_a, &wrong).is_err());
    Ok(())
}

#[test]
fn native_fixture_flat_source_scanner_preserves_bytes_and_refuses_wrapped_or_changed_proof()
-> Result<()> {
    let dir = tempfile::tempdir()?.keep();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    let raw = zip_archive("Index/Document.iwa", b"synthetic flat Numbers source");
    let path = dir.join("source-a.numbers");
    std::fs::write(&path, &raw)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    let receipt = PackageDownload {
        size: raw.len() as u64,
        sha256: hex::encode(Sha256::digest(&raw)),
    };
    let semantic = cirrove_icloud::package_flat_archive_semantic_identity_v2(
        &File::open(&path)?,
        &receipt,
        &CancellationToken::new(),
    )?;
    let source = Source {
        path: path.clone(),
        size: receipt.size,
        sha256: receipt.sha256,
        root: None,
        semantic,
    };
    source_verified(&source)?;
    for case in 0..4 {
        let mut bad = source.clone();
        match case {
            0 => bad.root = Some("Source.numbers".into()),
            1 => bad.sha256 = "0".repeat(64),
            2 => bad.semantic.sha256 = "0".repeat(64),
            _ => bad.size += 1,
        }
        assert!(
            source_verified(&bad).is_err(),
            "flat scanner substitution {case} accepted"
        );
        assert_eq!(std::fs::read(&path)?, raw);
        assert_eq!(
            std::fs::metadata(&path)?.permissions().mode() & 0o777,
            0o600
        );
    }
    let wrapped = zip_archive(
        "Source.numbers/Index/Document.iwa",
        b"synthetic wrapped source",
    );
    let wrapped_path = dir.join("wrapped.numbers");
    std::fs::write(&wrapped_path, &wrapped)?;
    std::fs::set_permissions(&wrapped_path, std::fs::Permissions::from_mode(0o600))?;
    let wrapped_receipt = PackageDownload {
        size: wrapped.len() as u64,
        sha256: hex::encode(Sha256::digest(&wrapped)),
    };
    let wrapped_semantic = cirrove_icloud::package_archive_semantic_identity_versioned(
        &File::open(&wrapped_path)?,
        &wrapped_receipt,
        "Source.numbers",
        2,
        &CancellationToken::new(),
    )?;
    let wrapped_source = Source {
        path: wrapped_path,
        size: wrapped_receipt.size,
        sha256: wrapped_receipt.sha256,
        root: Some("Source.numbers".into()),
        semantic: wrapped_semantic,
    };
    source_verified(&wrapped_source)?;
    let mut mismatched = wrapped_source.clone();
    mismatched.root = None;
    assert!(source_verified(&mismatched).is_err());
    assert_eq!(std::fs::read(&wrapped_source.path)?, wrapped);
    Ok(())
}

#[tokio::test]
async fn native_fixture_public_failure_keeps_only_fixed_admission_stage() -> Result<()> {
    let value = flat_registration(false)?;
    let mut r: Registration = serde_json::from_value(value)?;
    let now = clock()?;
    r.started_unix_seconds = now;
    r.deadline_unix_seconds = now + 60;
    // Retain this synthetic owned fixture; never recurse through a mount during cleanup.
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&r.session_directory)?;
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(r.session_directory.join("state"))?;
    let raw = zip_archive("Index/Document.iwa", b"synthetic diagnostic source");
    let mut source_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&r.source.path)?;
    source_file.write_all(&raw)?;
    source_file.sync_all()?;
    let receipt = PackageDownload {
        size: raw.len() as u64,
        sha256: hex::encode(Sha256::digest(&raw)),
    };
    r.source.size = receipt.size;
    r.source.sha256 = receipt.sha256.clone();
    r.source.semantic = cirrove_icloud::package_flat_archive_semantic_identity_v2(
        &source_file,
        &receipt,
        &CancellationToken::new(),
    )?;
    source_verified(&r.source)?;
    let settings = account_bytes(&r)?;
    r.settings_sha256 = hex::encode(Sha256::digest(&settings));
    bound_settings(&r, &settings)?;
    // This real source guard must refuse before attempt publication/session access.
    r.source.sha256 = "0".repeat(64);
    assert!(source_verified(&r.source).is_err());
    let registration = serde_json::to_vec(&r)?;
    let digest = hex::encode(Sha256::digest(&registration));
    registered(&registration, &digest, now)?;
    let path = r
        .session_directory
        .join("flat-numbers-preflight-registration.json");
    let settings_path = r.session_directory.join("state/accounts.json");
    for (target, bytes) in [(&path, &registration), (&settings_path, &settings)] {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(target)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    let error = icloud_owned_native_import_fixture_verify(&path, &digest)
        .await
        .err()
        .context("invalid source unexpectedly accepted")?;
    // Preservation and the no-session/no-output boundary precede the desired assertion.
    for (target, bytes) in [
        (&path, &registration),
        (&settings_path, &settings),
        (&r.source.path, &raw),
    ] {
        assert_eq!(&std::fs::read(target)?, bytes);
        assert_eq!(
            std::fs::metadata(target)?.permissions().mode() & 0o777,
            0o600
        );
    }
    assert!(
        !r.session_directory
            .join("flat-numbers-preflight.attempt.json")
            .exists()
    );
    assert!(
        !r.session_directory
            .join("flat-numbers-preflight.json")
            .exists()
    );
    assert!(!r.session_directory.join("state/accounts").exists());
    assert_eq!(
        error.chain().count(),
        1,
        "underlying diagnostic error leaked"
    );
    assert_eq!(
        error.to_string(),
        "owned native fixture verification refused (stage=fixture_admission)",
        "public observer erased the fixed failure stage",
    );
    Ok(())
}

#[test]
fn native_fixture_failure_stage_sanitizer_discards_details_and_context() {
    let secret = "synthetic-sensitive-detail-must-never-escape";
    for stage in [
        FailureStage::FixtureAdmission,
        FailureStage::Session,
        FailureStage::RootListing,
        FailureStage::ParentBinding,
        FailureStage::DocumentListing,
        FailureStage::DocumentBinding,
        FailureStage::ContentVerification,
        FailureStage::Trash,
        FailureStage::FinalFences,
        FailureStage::Deadline,
    ] {
        let original = anyhow::anyhow!(secret).context(format!("context-{secret}"));
        let typed = in_stage::<()>(stage, Err(original)).unwrap_err();
        let public = sanitized_failure(typed.context(format!("outer-{secret}")));
        assert_eq!(public.chain().count(), 1);
        assert_eq!(
            public.to_string(),
            format!(
                "owned native fixture verification refused (stage={})",
                stage.label(),
            )
        );
        assert!(!format!("{public:?}").contains(secret));
        assert!(!format!("{public:#}").contains(secret));
    }
    // Text resembling a phase never grants authority to expose an arbitrary error.
    let unknown =
        anyhow::anyhow!("stage=document_binding {secret}").context(format!("context-{secret}"));
    let public = sanitized_failure(unknown);
    assert_eq!(
        public.to_string(),
        "owned native fixture verification refused"
    );
    assert_eq!(public.chain().count(), 1);
    assert!(!format!("{public:?}").contains(secret));
}

#[test]
fn native_fixture_stale_revision_refuses_with_only_document_binding_stage() -> Result<()> {
    let r = plan(Arm::NativeFinalPreflight);
    let (_, original) = entries(&r);
    assert_eq!(
        exact_entry(std::slice::from_ref(&original), &r.document)?,
        original
    );
    let mut changed = original.clone();
    changed.etag = "synthetic-private-current-revision".into();
    let before = serde_json::to_vec(&changed)?;
    let failed = in_stage(
        FailureStage::DocumentBinding,
        exact_entry(std::slice::from_ref(&changed), &r.document),
    )
    .unwrap_err();
    let public = sanitized_failure(failed);
    // Real identity/revision guard and complete input preservation precede stage assertion.
    assert_eq!(serde_json::to_vec(&changed)?, before);
    assert_eq!(
        exact_entry(std::slice::from_ref(&original), &r.document)?,
        original
    );
    assert_eq!(public.chain().count(), 1);
    assert!(!format!("{public:?}").contains(&changed.etag));
    assert!(!format!("{public:#}").contains(&changed.etag));
    assert_eq!(
        public.to_string(),
        "owned native fixture verification refused (stage=document_binding)",
    );
    Ok(())
}
