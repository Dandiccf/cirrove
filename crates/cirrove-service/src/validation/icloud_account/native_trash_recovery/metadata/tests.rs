//! No session, service, provider or credential reads: these controls exercise typed admission.
#![allow(clippy::unwrap_used)]
use super::*;
fn registration() -> serde_json::Value {
    let run = Uuid::new_v4();
    let root = format!("/var/tmp/cirrove-native-trash-{run}");
    let clock = now();
    serde_json::json!({"version":1,"run":run,"account":Uuid::new_v4(),"label":"iCloudNativeTrashLossValidation",
        "session_directory":root,"settings_sha256":"a".repeat(64),"started_unix":clock.saturating_sub(1),"deadline_unix":clock+599,
        "source":{"path":format!("{root}/source.numbers"),"size":123,"sha256":"b".repeat(64),"source_layout":"flat_numbers","root":null,
        "semantic":{"version":2,"entries":2,"files":1,"expanded_bytes":123,"sha256":"c".repeat(64)}}})
}
fn admit(v: &serde_json::Value) -> Result<Registration> {
    let data = serde_json::to_vec(v)?;
    registered(&data, &hash(&data), now())
}
fn entry(r: &Registration, parent: Option<&str>) -> DriveEntry {
    let name = if parent.is_none() {
        format!("Cirrove-Native-Trash-{}", r.run)
    } else {
        format!("Cirrove-Numbers-Trash-{}", r.run)
    };
    serde_json::from_value(serde_json::json!({"drivewsid":format!("{}::com.apple.CloudDocs::{}",if parent.is_none(){"FOLDER"}else{"FILE"},r.run),
        "docwsid":r.run.to_string(),"item_id":"owned","zone":"com.apple.CloudDocs","name":name,"extension":if parent.is_none(){""}else{"numbers"},
        "parentId":parent.unwrap_or(ROOT_ID),"etag":"E1","type":if parent.is_none(){"FOLDER"}else{"FILE"},"size":123,"items":[]})).unwrap()
}
#[test]
fn native_trash_metadata_rejects_foreign_scope_layout_window_and_unknown_fields() {
    let good = registration();
    admit(&good).unwrap();
    for (key, value) in [
        ("run", serde_json::json!(Uuid::nil())),
        ("account", serde_json::json!(Uuid::nil())),
        ("session_directory", serde_json::json!("/var/tmp/foreign")),
        ("label", serde_json::json!("iCloudGuiValidation")),
        ("deadline_unix", serde_json::json!(now() + 1000)),
        ("started_unix", serde_json::json!(now() + 5)),
        ("foreign", serde_json::json!(true)),
    ] {
        let mut v = good.clone();
        v[key] = value;
        assert!(admit(&v).is_err(), "{key}");
    }
    for (key, value) in [
        ("source_layout", serde_json::json!("wrapped")),
        ("root", serde_json::json!("Fake.numbers")),
        ("path", serde_json::json!("/var/tmp/foreign/source.numbers")),
        ("size", serde_json::json!(67108865)),
    ] {
        let mut v = good.clone();
        v["source"][key] = value;
        assert!(admit(&v).is_err(), "{key}");
    }
    let mut missing = good;
    missing["source"].as_object_mut().unwrap().remove("root");
    assert!(admit(&missing).is_err());
}
#[test]
fn native_trash_metadata_selects_only_unique_owned_typed_identities() {
    let r = admit(&registration()).unwrap();
    let p = entry(&r, None);
    let d = entry(&r, Some(&p.drivewsid));
    assert_eq!(selected(std::slice::from_ref(&p), &r, None).unwrap(), p);
    assert_eq!(
        selected(std::slice::from_ref(&d), &r, Some(&p.drivewsid)).unwrap(),
        d
    );
    assert!(selected(&[d.clone(), d.clone()], &r, Some(&p.drivewsid)).is_err());
    for (key, value) in [
        ("parentId", "foreign"),
        ("etag", "*"),
        ("zone", "foreign"),
        ("type", "FOLDER"),
        ("drivewsid", "FILE::foreign::item"),
    ] {
        let mut v = serde_json::to_value(&d).unwrap();
        v[key] = serde_json::json!(value);
        let bad: DriveEntry = serde_json::from_value(v).unwrap();
        assert!(selected(&[bad], &r, Some(&p.drivewsid)).is_err(), "{key}");
    }
    let n = node(&d, true);
    assert_eq!(n.id, d.drivewsid);
    assert_eq!(n.etag.as_deref(), Some("E1"));
    assert!(n.package);
}
#[test]
fn native_trash_metadata_account_cannot_substitute_route_or_write_capability() {
    let mut v = registration();
    let r = admit(&v).unwrap();
    let a = serde_json::json!({"id":r.account,"label":r.label,"registration":{"provider":"i_cloud"},
        "identity":{"tenant_id":"synthetic","subject":"synthetic","username":"synthetic@example.test","graph_user_id":"synthetic","display_name":"Synthetic"},"credential_id":Uuid::new_v4(),"access":"read_write",
        "drive":{"id":"drive","name":"Synthetic","driveType":"icloud_drive"},"root_id":ROOT_ID,
        "mount_path":r.session_directory.join("mount"),"enabled":true,"poll_seconds":60,"cache_bytes":1048576});
    let good = serde_json::json!({"version":2,"accounts":[a]});
    let data = serde_json::to_vec(&good).unwrap();
    v["settings_sha256"] = serde_json::json!(hash(&data));
    let r = admit(&v).unwrap();
    account(&r, &data).unwrap();
    for (key, value) in [
        ("id", serde_json::json!(Uuid::new_v4())),
        ("access", serde_json::json!("read_only")),
        ("mount_path", serde_json::json!("/var/tmp/foreign/mount")),
        ("enabled", serde_json::json!(false)),
        ("label", serde_json::json!("foreign")),
    ] {
        let mut bad = good.clone();
        bad["accounts"][0][key] = value;
        let data = serde_json::to_vec(&bad).unwrap();
        let mut registration = v.clone();
        registration["settings_sha256"] = serde_json::json!(hash(&data));
        let r = admit(&registration).unwrap();
        assert!(account(&r, &data).is_err(), "{key}");
    }
}
