#![allow(clippy::unwrap_used)]
use super::*;
fn plan(phase: Phase) -> Registration {
    let run = Uuid::new_v4();
    let account = Uuid::new_v4();
    let root = PathBuf::from(format!("/var/tmp/cirrove-calc-editor-{run}"));
    let parent=serde_json::from_value(serde_json::json!({"id":"FOLDER::com.apple.CloudDocs::owned",
        "parent_id":cirrove_icloud::ROOT_ID,"name":format!("Cirrove-Native-{run}"),"kind":"folder","size":0,"etag":"historical-create-etag"})).unwrap();
    let document=serde_json::from_value(serde_json::json!({"id":"FILE::com.apple.CloudDocs::actual-item",
        "parent_id":"FOLDER::com.apple.CloudDocs::owned","name":format!("Cirrove-Calc-{run}.xlsx"),"kind":"file","size":3,"etag":"receipt-v1"})).unwrap();
    Registration {
        version: 1,
        phase,
        run,
        account,
        session_directory: root.clone(),
        root_dev: 31,
        root_ino: 12345,
        settings_sha256: "a".repeat(64),
        parent,
        document,
        source: Source {
            path: root.join(format!("source-{}.xlsx", phase.letter())),
            size: 3,
            sha256: "b".repeat(64),
            root: None,
        },
        started_unix_seconds: 1000,
        deadline_unix_seconds: 1300,
    }
}
fn decode_value(value: &serde_json::Value, clock: u64) -> Result<Registration> {
    let bytes = serde_json::to_vec(value)?;
    registered(&bytes, &hex::encode(Sha256::digest(&bytes)), clock)
}
fn entries(r: &Registration) -> (DriveEntry, DriveEntry) {
    let parent=serde_json::from_value(serde_json::json!({"drivewsid":r.parent.id,"docwsid":"actual-folder-doc",
        "item_id":"actual-folder-item","parentId":cirrove_icloud::ROOT_ID,"name":r.parent.name,"type":"FOLDER","zone":"com.apple.CloudDocs",
        "etag":"fresh-after-child-creation","size":7,"numberOfItems":1})).unwrap();
    let document=serde_json::from_value(serde_json::json!({"drivewsid":r.document.id,"docwsid":"actual-item","item_id":"actual-optional-item",
        "parentId":r.parent.id,"name":r.document.name.strip_suffix(".xlsx").unwrap(),"extension":"xlsx","type":"FILE","zone":"com.apple.CloudDocs",
        "etag":r.document.etag,"size":r.document.size,"numberOfItems":7})).unwrap();
    (parent, document)
}
#[test]
fn owned_calc_metadata_accepts_phase_a_b_raw_receipt_registration() -> Result<()> {
    for phase in [Phase::A, Phase::B] {
        let mut r = plan(phase);
        if matches!(phase, Phase::B) {
            r.document.id = "FILE::com.apple.CloudDocs::replacement-item".into();
            r.document.etag = Some("replacement-revision".into());
        }
        let value = serde_json::to_value(&r)?;
        decode_value(&value, 1001)?;
        let (p, mut d) = entries(&r);
        if matches!(phase, Phase::B) {
            d.docwsid = "replacement-item".into();
        }
        let actual = parent(std::slice::from_ref(&p), &r)?;
        assert_ne!(Some(actual.etag.as_str()), r.parent.etag.as_deref());
        assert!(actual.items.is_empty() && actual.number_of_items.is_none());
        assert_eq!(exact_entry(std::slice::from_ref(&d), &r.document)?, d);
        let f: Fixture = serde_json::from_value(
            serde_json::json!({"version":1,"run":r.run,"account":r.account,"session_directory":r.session_directory,
            "settings_sha256":r.settings_sha256,"parent":actual,"document":d,"format":"xlsx","representation":"data",
            "source":r.source.path,"source_size":r.source.size,"source_sha256":r.source.sha256,"source_root":null,"expected_root":r.document.name,"semantic":null}),
        )?;
        validate(&f)?;
    }
    Ok(())
}
#[test]
fn owned_calc_metadata_schema_refuses_wrapped_missing_root_and_package_proof() -> Result<()> {
    let original = serde_json::to_value(plan(Phase::A))?;
    for arm in 0..6 {
        let mut v = original.clone();
        match arm {
            0 => v["source"]["root"] = "Export.xlsx".into(),
            1 => {
                v["source"].as_object_mut().unwrap().remove("root");
            }
            2 => v["source"]["semantic"] = serde_json::json!({"version":2}),
            3 => v["representation"] = "package".into(),
            4 => v["source"]["size"] = 0.into(),
            _ => v["source"]["size"] = (LIMIT + 1).into(),
        }
        assert!(decode_value(&v, 1001).is_err(), "schema arm {arm}");
    }
    let bytes = serde_json::to_vec(&original)?;
    assert!(registered(&bytes, &"f".repeat(64), 1001).is_err());
    Ok(())
}
#[test]
fn owned_calc_metadata_route_receipt_and_finite_window_refuse_drift() -> Result<()> {
    let original = serde_json::to_value(plan(Phase::A))?;
    for arm in 0..12 {
        let mut v = original.clone();
        match arm {
            0 => v["account"] = Uuid::nil().to_string().into(),
            1 => v["run"] = Uuid::new_v4().to_string().into(),
            2 => v["root_dev"] = 0.into(),
            3 => v["parent"]["parent_id"] = "foreign".into(),
            4 => v["parent"]["package"] = true.into(),
            5 => v["document"]["parent_id"] = "foreign".into(),
            6 => v["document"]["name"] = "Foreign.xlsx".into(),
            7 => v["document"]["package"] = true.into(),
            8 => v["document"]["etag"] = "*".into(),
            9 => v["document"]["size"] = 4.into(),
            10 => v["source"]["path"] = "/var/tmp/foreign.xlsx".into(),
            _ => v["deadline_unix_seconds"] = 3000.into(),
        }
        assert!(decode_value(&v, 1001).is_err(), "route arm {arm}");
    }
    assert!(decode_value(&original, 999).is_err());
    assert!(decode_value(&original, 1300).is_err());
    Ok(())
}
#[test]
fn owned_calc_metadata_exact_entries_refuse_duplicates_identity_revision_and_size() -> Result<()> {
    let r = plan(Phase::A);
    let (p, d) = entries(&r);
    assert!(parent(&[p.clone(), p.clone()], &r).is_err());
    assert!(exact_entry(&[d.clone(), d.clone()], &r.document).is_err());
    for arm in 0..4 {
        let mut bad = d.clone();
        match arm {
            0 => bad.drivewsid = "FILE::com.apple.CloudDocs::foreign".into(),
            1 => bad.parent_id = "FOLDER::com.apple.CloudDocs::foreign".into(),
            2 => bad.etag = "new-revision".into(),
            _ => bad.size += 1,
        }
        assert!(
            exact_entry(&[bad], &r.document).is_err(),
            "document arm {arm}"
        );
    }
    let mut changed = p;
    changed.parent_id = "FOLDER::com.apple.CloudDocs::foreign".into();
    assert!(parent(&[changed], &r).is_err());
    Ok(())
}
#[test]
fn owned_calc_metadata_settings_refuse_foreign_account_drive_or_mount() -> Result<()> {
    let mut r = plan(Phase::A);
    let credential = Uuid::new_v4();
    let original = serde_json::json!({"version":2,"accounts":[{"id":r.account,"label":"iCloudCalcValidation","registration":{"provider":"i_cloud"},
        "identity":{"tenant_id":"icloud","subject":"synthetic","username":"synthetic.invalid","graph_user_id":"synthetic","display_name":"Synthetic"},
        "credential_id":credential,"access":"read_write","drive":{"id":"drive","name":"Drive","driveType":"icloud_drive"},
        "root_id":cirrove_icloud::ROOT_ID,"mount_path":r.session_directory.join("mount"),"enabled":true,"poll_seconds":300,"cache_bytes":1048576}]});
    for access in ["read_write", "read_only"] {
        let mut good = original.clone();
        good["accounts"][0]["access"] = access.into();
        let raw = serde_json::to_vec(&good)?;
        r.settings_sha256 = hex::encode(Sha256::digest(&raw));
        account(&r, &raw)?;
    }
    for arm in 0..5 {
        let mut v = original.clone();
        match arm {
            0 => v["accounts"][0]["id"] = Uuid::new_v4().to_string().into(),
            1 => v["accounts"][0]["drive"]["id"] = "foreign".into(),
            2 => v["accounts"][0]["mount_path"] = "/var/tmp/foreign".into(),
            3 => v["accounts"][0]["enabled"] = false.into(),
            _ => v["accounts"][0]["credential_id"] = Uuid::nil().to_string().into(),
        }
        let raw = serde_json::to_vec(&v)?;
        r.settings_sha256 = hex::encode(Sha256::digest(&raw));
        assert!(account(&r, &raw).is_err(), "account arm {arm}");
    }
    Ok(())
}
