#![allow(clippy::unwrap_used)]
use super::*;
fn plan() -> Registration {
    let run = Uuid::new_v4();
    let root = PathBuf::from(format!("/var/tmp/cirrove-numbers-browser-editor-{run}"));
    Registration {
        version: 1,
        phase: Phase::A,
        prior_a_sha256: None,
        run,
        account: Uuid::new_v4(),
        session_directory: root.clone(),
        root_dev: 31,
        root_ino: 123,
        settings_sha256: "a".repeat(64),
        representation: FixtureRepresentation::Data,
        source: Source {
            path: root.join("source-a.numbers"),
            size: 3,
            sha256: hex::encode(Sha256::digest(b"abc")),
            root: Some("ActualExport.numbers".into()),
            semantic: None,
        },
        expected_parent_id: None,
        expected_document_id: None,
        expected_document_etag: None,
        started_unix_seconds: 1000,
        deadline_unix_seconds: 1300,
    }
}
fn entries(r: &Registration) -> (DriveEntry, DriveEntry) {
    let p: DriveEntry = serde_json::from_value(serde_json::json!({"drivewsid":"FOLDER::com.apple.CloudDocs::owned",
        "docwsid":"parent-doc","item_id":"actual-parent-optional","zone":"com.apple.CloudDocs","type":"FOLDER",
        "parentId":cirrove_icloud::ROOT_ID,"name":format!("Cirrove-Native-{}",r.run),"etag":"parent-e1","size":91,"numberOfItems":1})).unwrap();
    let d: DriveEntry = serde_json::from_value(serde_json::json!({"drivewsid":"FILE::com.apple.CloudDocs::owned-document",
        "docwsid":"owned-document","item_id":"actual-document-optional","zone":"com.apple.CloudDocs","type":"FILE",
        "parentId":p.drivewsid,"name":format!("Cirrove-Numbers-Editor-{}",r.run),"extension":"numbers",
        "etag":"document-e1","size":777,"numberOfItems":7})).unwrap();
    (p, d)
}
#[test]
fn editor_metadata_registration_scope_window_and_phase_are_exact() -> Result<()> {
    let r = plan();
    let valid = serde_json::to_vec(&r)?;
    registered(&valid, &hex::encode(Sha256::digest(&valid)), 1001)?;
    assert!(registered(&valid, &"b".repeat(64), 1001).is_err());
    for arm in 0..12 {
        let mut bad = r.clone();
        match arm {
            0 => bad.run = Uuid::nil(),
            1 => bad.account = Uuid::nil(),
            2 => bad.session_directory = PathBuf::from("/var/tmp/foreign"),
            3 => bad.source.path = bad.session_directory.join("source-b.numbers"),
            4 => bad.source.root = Some("unregistered/export.numbers".into()),
            5 => bad.deadline_unix_seconds = 2801,
            6 => bad.started_unix_seconds = 1002,
            7 => bad.representation = FixtureRepresentation::Package,
            8 => bad.prior_a_sha256 = Some("c".repeat(64)),
            9 => bad.phase = Phase::B,
            10 => bad.expected_parent_id = Some(cirrove_icloud::ROOT_ID.into()),
            _ => bad.expected_document_etag = Some("*".into()),
        }
        let bytes = serde_json::to_vec(&bad)?;
        assert!(
            registered(&bytes, &hex::encode(Sha256::digest(&bytes)), 1001).is_err(),
            "scope arm {arm}"
        );
    }
    assert!(registered(&valid, &hex::encode(Sha256::digest(&valid)), 1300).is_err());
    Ok(())
}
#[test]
fn editor_metadata_typed_selection_preserves_actual_fields_and_refuses_foreign_entries()
-> Result<()> {
    let r = plan();
    let (p, d) = entries(&r);
    let parent = select_parent(std::slice::from_ref(&p), &r)?;
    assert_eq!(parent.item_id, p.item_id);
    assert_eq!(parent.size, 91);
    assert!(parent.items.is_empty() && parent.number_of_items.is_none());
    assert_eq!(select_document(std::slice::from_ref(&d), &parent, &r)?, d);
    assert!(select_parent(&[p.clone(), p.clone()], &r).is_err());
    assert!(select_document(&[d.clone(), d.clone()], &parent, &r).is_err());
    for arm in 0..8 {
        let mut bad = d.clone();
        match arm {
            0 => bad.name = "foreign".into(),
            1 => bad.parent_id = "FOLDER::com.apple.CloudDocs::foreign".into(),
            2 => bad.zone = "foreign".into(),
            3 => bad.kind = "FOLDER".into(),
            4 => bad.etag = String::new(),
            5 => bad.etag = "*".into(),
            6 => bad.docwsid = "different".into(),
            _ => bad.extension = "pages".into(),
        }
        assert!(
            select_document(&[bad], &parent, &r).is_err(),
            "metadata arm {arm}"
        );
    }
    let mut pinned = r.clone();
    pinned.expected_document_etag = Some("other".into());
    assert!(select_document(&[d], &parent, &pinned).is_err());
    Ok(())
}
#[test]
fn editor_metadata_settings_require_the_exact_read_only_account() -> Result<()> {
    let mut r = plan();
    let value = serde_json::json!({"version":2,"accounts":[{"id":r.account,"label":"iCloudNumbersBrowserValidation",
        "registration":{"provider":"i_cloud"},"identity":{"tenant_id":"icloud","subject":"synthetic","username":"synthetic.invalid","graph_user_id":"synthetic","display_name":"Synthetic"},
        "credential_id":"synthetic-only","access":"read_only","drive":{"id":"drive","name":"Drive","driveType":"icloud_drive"},
        "root_id":cirrove_icloud::ROOT_ID,"mount_path":r.session_directory.join("mount"),"enabled":true,"poll_seconds":2,"cache_bytes":1048576}]});
    let bytes = serde_json::to_vec(&value)?;
    r.settings_sha256 = hex::encode(Sha256::digest(&bytes));
    account(&r, &bytes)?;
    let mut changed = bytes.clone();
    changed.push(b' ');
    assert!(account(&r, &changed).is_err());
    for arm in 0..6 {
        let mut bad = value.clone();
        match arm {
            0 => bad["accounts"][0]["access"] = "read_write".into(),
            1 => bad["accounts"][0]["id"] = Uuid::new_v4().to_string().into(),
            2 => bad["accounts"][0]["label"] = "foreign".into(),
            3 => bad["accounts"][0]["drive"]["driveType"] = "personal".into(),
            4 => bad["accounts"][0]["mount_path"] = "/var/tmp/foreign".into(),
            _ => bad["accounts"][0]["enabled"] = false.into(),
        }
        let bytes = serde_json::to_vec(&bad)?;
        let mut bound = r.clone();
        bound.settings_sha256 = hex::encode(Sha256::digest(&bytes));
        assert!(account(&bound, &bytes).is_err(), "account arm {arm}");
    }
    Ok(())
}
#[test]
fn editor_metadata_phase_b_keeps_original_identity_and_requires_changed_revision() -> Result<()> {
    let a = plan();
    let (raw_parent, document) = entries(&a);
    let parent = select_parent(&[raw_parent], &a)?;
    let digest = "d".repeat(64);
    let value = serde_json::json!({"version":1,"phase":"a","run":a.run,"account":a.account,
        "registration_sha256":digest,"parent":parent,"document":document,
        "fixture_path":a.session_directory.join("editor-a-fixture.json"),"fixture_sha256":"e".repeat(64),
        "source":a.source,"expected_representation":"data","prior_a_sha256":null,"prior_document_etag":null,
        "metadata_acquired":true,"representation_verified":false,"current_content_verified":false,"gui_fidelity_verified":false,
        "journal_receipt_verified":false,"cloud_mutated":false,"automatic_retry":false});
    let m: Metadata = serde_json::from_value(value.clone())?;
    let mut b = a.clone();
    b.phase = Phase::B;
    b.prior_a_sha256 = Some("f".repeat(64));
    b.source.path = b.session_directory.join("source-b.numbers");
    prior_binding(&b, &a, &m, &digest)?;
    let mut edited = m.document.clone();
    edited.etag = "document-e2".into();
    edited.size = 1000;
    continuity(Some(&m), &m.parent, &edited)?;
    assert!(continuity(Some(&m), &m.parent, &m.document).is_err());
    let mut foreign = edited.clone();
    foreign.drivewsid = "FILE::com.apple.CloudDocs::foreign".into();
    assert!(continuity(Some(&m), &m.parent, &foreign).is_err());
    for arm in 0..5 {
        let mut bad = value.clone();
        match arm {
            0 => bad["account"] = Uuid::new_v4().to_string().into(),
            1 => bad["registration_sha256"] = "a".repeat(64).into(),
            2 => bad["source"]["sha256"] = "b".repeat(64).into(),
            3 => bad["journal_receipt_verified"] = true.into(),
            _ => bad["representation_verified"] = true.into(),
        }
        let bad: Metadata = serde_json::from_value(bad)?;
        assert!(
            prior_binding(&b, &a, &bad, &digest).is_err(),
            "prior-A arm {arm}"
        );
    }
    let mut overlong = b.clone();
    overlong.deadline_unix_seconds += 1;
    assert!(prior_binding(&overlong, &a, &m, &digest).is_err());
    Ok(())
}
#[test]
fn editor_metadata_source_proof_refuses_changed_raw_or_invalid_package_without_rewriting()
-> Result<()> {
    let dir = tempfile::tempdir()?.keep();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    let path = dir.join("actual-export.numbers");
    std::fs::write(&path, b"abc")?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400))?;
    let mut r = plan();
    r.source.path = path.clone();
    source_verified(&r)?;
    let before = std::fs::read(&path)?;
    let mut bad = r.clone();
    bad.source.sha256 = "b".repeat(64);
    assert!(source_verified(&bad).is_err());
    r.representation = FixtureRepresentation::Package;
    r.source.semantic = Some(PackageSemanticIdentity {
        version: 2,
        sha256: "c".repeat(64),
        entries: 2,
        files: 1,
        expanded_bytes: 3,
    });
    assert!(
        source_verified(&r).is_err(),
        "invalid native source accepted without real semantic scan"
    );
    assert_eq!(std::fs::read(&path)?, before);
    assert_eq!(
        std::fs::metadata(&path)?.permissions().mode() & 0o777,
        0o400
    );
    Ok(())
}

#[test]
fn editor_metadata_explicit_flat_source_preserves_expected_representation_only() -> Result<()> {
    let r = plan();
    let mut value = serde_json::to_value(&r)?;
    value["source"]["root"] = serde_json::Value::Null;
    let bytes = serde_json::to_vec(&value)?;
    let result = registered(&bytes, &hex::encode(Sha256::digest(&bytes)), 1001);
    assert_eq!(r.source.size, 3);
    assert_eq!(r.source.sha256, hex::encode(Sha256::digest(b"abc")));
    let flat = result.expect("metadata registration refuses explicit flat source layout");
    assert!(matches!(flat.representation, FixtureRepresentation::Data));
    assert!(flat.source.semantic.is_none());
    value["representation"] = "package".into();
    let raw = serde_json::to_vec(&value)?;
    assert!(
        registered(&raw, &hex::encode(Sha256::digest(&raw)), 1001).is_err(),
        "flat does not manufacture package semantics"
    );
    value["representation"] = "data".into();
    value["source"].as_object_mut().unwrap().remove("root");
    let raw = serde_json::to_vec(&value)?;
    assert!(
        registered(&raw, &hex::encode(Sha256::digest(&raw)), 1001).is_err(),
        "missing layout is not explicit flat"
    );
    Ok(())
}
