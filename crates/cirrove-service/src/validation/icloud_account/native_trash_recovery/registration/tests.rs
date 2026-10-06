//! Pure admission controls; never invokes load(), a service, vault or provider.
#![allow(clippy::unwrap_used)]
use super::*;
use std::{
    io::Write,
    os::unix::fs::{PermissionsExt, symlink},
};

struct Fixture {
    directory: PathBuf,
    source: Source,
    raw: Vec<u8>,
}
impl Fixture {
    fn new() -> Self {
        // tempfile uses the test command's disk-backed TMPDIR. Retain every
        // fixture, including refusals, rather than deleting its evidence.
        let directory = tempfile::Builder::new()
            .prefix("native-trash-registration-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap()
            .keep();
        let raw =
            crate::native_import::synthetic_package_archive("Document", b"owned Numbers content");
        let path = directory.join("source.numbers");
        write(&path, &raw, 0o400);
        let receipt = cirrove_icloud::PackageDownload {
            size: raw.len() as u64,
            sha256: hash(&raw),
        };
        let semantic = cirrove_icloud::package_flat_archive_semantic_identity_v2(
            &File::open(&path).unwrap(),
            &receipt,
            &CancellationToken::new(),
        )
        .unwrap();
        let source = Source {
            path,
            size: receipt.size,
            sha256: receipt.sha256,
            source_layout: PackageSourceLayout::FlatNumbers,
            root: None,
            semantic,
        };
        Self {
            directory,
            source,
            raw,
        }
    }
    fn unchanged(&self) {
        assert_eq!(std::fs::read(&self.source.path).unwrap(), self.raw);
        assert_eq!(
            std::fs::symlink_metadata(&self.source.path).unwrap().mode() & 0o777,
            0o400
        );
    }
    fn registration(&self) -> serde_json::Value {
        let run = Uuid::new_v4();
        let root = PathBuf::from(format!("/var/tmp/cirrove-native-trash-{run}"));
        let parent = Node {
            id: format!("FOLDER::com.apple.CloudDocs::{run}"),
            parent_id: Some(cirrove_icloud::ROOT_ID.into()),
            name: format!("Cirrove-Native-Trash-{run}"),
            kind: NodeKind::Folder,
            size: 0,
            modified_unix: 0,
            etag: Some("parent-E1".into()),
            content_version: None,
            target: None,
            package: false,
        };
        let original = Node {
            id: format!("FILE::com.apple.CloudDocs::{run}"),
            parent_id: Some(parent.id.clone()),
            name: format!("Cirrove-Numbers-Trash-{run}.numbers"),
            kind: NodeKind::Folder,
            size: 42,
            modified_unix: 0,
            etag: Some("selected-E1".into()),
            content_version: None,
            target: None,
            package: true,
        };
        let mut source = serde_json::to_value(&self.source).unwrap();
        source["path"] = serde_json::json!(root.join("source.numbers"));
        let clock = now();
        serde_json::json!({"version":1,"run":run,"account":Uuid::new_v4(),
            "label":"iCloudNativeTrashLossValidation","session_directory":root,
            "tmpdir":format!("/home/dandiccf/Storage/cirrove-tmp/nt-{}",&run.to_string()[..8]),
            "settings_sha256":"a".repeat(64),"authorization_sha256":"b".repeat(64),
            "executable_sha256":"c".repeat(64),"preflight_sha256":"d".repeat(64),
            "started_unix":clock.saturating_sub(2),"deadline_unix":clock+598,
            "parent_creation":Uuid::new_v4(),"import":Uuid::new_v4(),
            "parent":parent,"original":original,"source":source,"recovery":null})
    }
}
fn write(path: &Path, data: &[u8], mode: u32) {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
        .unwrap();
    file.write_all(data).unwrap();
    file.sync_all().unwrap();
}
fn registration(value: serde_json::Value) -> Result<Registration> {
    let reg: Registration = serde_json::from_value(value)?;
    let phase = if reg.recovery.is_some() {
        "recovery"
    } else {
        "loss"
    };
    reg.validate(
        &reg.session_directory
            .join(format!("{phase}-registration.json")),
    )?;
    Ok(reg)
}

#[test]
fn native_trash_registration_explicit_flat_source_and_full_content_authority() {
    let f = Fixture::new();
    f.source.check(&f.directory).unwrap();
    assert_eq!(
        serde_json::to_value(&f.source).unwrap()["root"],
        serde_json::Value::Null
    );
    for arm in 0..9 {
        let mut value = serde_json::to_value(&f.source).unwrap();
        match arm {
            0 => {
                value.as_object_mut().unwrap().remove("root");
            }
            1 => value["root"] = serde_json::json!("Source.numbers"),
            2 => {
                value.as_object_mut().unwrap().remove("source_layout");
            }
            3 => value["source_layout"] = serde_json::json!("wrapped"),
            4 => value["path"] = serde_json::json!(f.directory.join("other.numbers")),
            5 => value["sha256"] = serde_json::json!("0".repeat(64)),
            6 => value["size"] = serde_json::json!(f.source.size + 1),
            7 => value["semantic"]["sha256"] = serde_json::json!("0".repeat(64)),
            _ => value["semantic"]["version"] = serde_json::json!(1),
        }
        let refused = serde_json::from_value::<Source>(value)
            .map_err(anyhow::Error::from)
            .and_then(|source| source.check(&f.directory));
        assert!(refused.is_err(), "source authority arm {arm}");
        f.unchanged();
    }
}

#[test]
fn native_trash_registration_finite_window_exact_owned_identity_and_pins() {
    let f = Fixture::new();
    let value = f.registration();
    let valid = registration(value.clone()).unwrap();
    assert!(valid.request().validate().is_ok());
    assert_eq!(valid.scope().provider, "icloud");
    assert_eq!(valid.scope().collection, "drive");
    assert!(
        valid
            .validate(&valid.session_directory.join("recovery-registration.json"))
            .is_err()
    );
    for arm in 0..21 {
        let mut changed = value.clone();
        let clock = now();
        match arm {
            0 => changed["run"] = serde_json::json!(Uuid::nil()),
            1 => changed["account"] = serde_json::json!(Uuid::nil()),
            2 => changed["label"] = serde_json::json!("foreign connection"),
            3 => changed["session_directory"] = serde_json::json!("/var/tmp/foreign-run"),
            4 => changed["started_unix"] = serde_json::json!(clock + 120),
            5 => changed["deadline_unix"] = serde_json::json!(clock.saturating_sub(1)),
            6 => {
                changed["started_unix"] = serde_json::json!(clock.saturating_sub(1));
                changed["deadline_unix"] = serde_json::json!(clock + 600);
            }
            7 => changed["deadline_unix"] = changed["started_unix"].clone(),
            8 => changed["parent_creation"] = changed["import"].clone(),
            9 => changed["import"] = serde_json::json!(Uuid::nil()),
            10 => changed["parent"]["name"] = serde_json::json!("foreign owned name"),
            11 => {
                changed["parent"]["parent_id"] =
                    serde_json::json!("FOLDER::com.apple.CloudDocs::foreign-root")
            }
            12 => {
                changed["original"]["parent_id"] =
                    serde_json::json!("FOLDER::com.apple.CloudDocs::foreign-parent")
            }
            13 => changed["original"]["name"] = serde_json::json!("different.numbers"),
            14 => changed["original"]["package"] = serde_json::json!(false),
            15 => changed["original"]["kind"] = serde_json::json!("file"),
            16 => changed["original"]["etag"] = serde_json::Value::Null,
            17 => changed["original"]["content_version"] = serde_json::json!("unbound-content"),
            18 => changed["settings_sha256"] = serde_json::json!("A".repeat(64)),
            19 => changed["authorization_sha256"] = serde_json::json!("bad"),
            _ => changed["preflight_sha256"] = serde_json::json!("g".repeat(64)),
        }
        assert!(
            registration(changed).is_err(),
            "registration authority arm {arm}"
        );
        f.unchanged();
    }
}

#[test]
fn native_trash_registration_recovery_cannot_select_import_or_parent_or_unknown_schema() {
    let f = Fixture::new();
    let mut value = f.registration();
    value["recovery"] = serde_json::json!({"operation":Uuid::new_v4(),
        "marker_sha256":"e".repeat(64),"checkpoint_cipher_sha256":"f".repeat(64)});
    registration(value.clone()).unwrap();
    for arm in 0..9 {
        let mut changed = value.clone();
        match arm {
            0 => changed["recovery"]["operation"] = changed["import"].clone(),
            1 => changed["recovery"]["operation"] = changed["parent_creation"].clone(),
            2 => changed["recovery"]["operation"] = serde_json::json!(Uuid::nil()),
            3 => {
                changed["recovery"]["checkpoint_cipher_sha256"] = serde_json::json!("F".repeat(64))
            }
            4 => changed["recovery"]["retry"] = serde_json::json!(true),
            5 => changed["scope"] = serde_json::json!({"provider":"foreign"}),
            6 => changed["source"]["infer_layout"] = serde_json::json!(true),
            7 => {
                changed["source"].as_object_mut().unwrap().remove("root");
            }
            _ => changed["standing_write_override"] = serde_json::json!(true),
        }
        assert!(registration(changed).is_err(), "recovery/schema arm {arm}");
        f.unchanged();
    }
    let auth = serde_json::json!({"version":1,"run":Uuid::new_v4(),"account":Uuid::new_v4(),
        "root":"/var/tmp/unused","scope":"owned-native-trash-confirmation-loss-and-inspection-recovery",
        "deadline_unix":now()+120,"standing_user_write_authorization":true,"retry":true});
    assert!(serde_json::from_value::<Authorization>(auth).is_err());
}

#[test]
fn native_trash_registration_private_inputs_refuse_links_ancestors_and_public_permissions() {
    let f = Fixture::new();
    assert_eq!(bytes(&f.source.path, LIMIT).unwrap(), f.raw);
    let final_link = f.directory.join("final-link.numbers");
    symlink(&f.source.path, &final_link).unwrap();
    assert!(private(&final_link, false, LIMIT).is_err());
    let ancestor = f.directory.join("ancestor");
    symlink(&f.directory, &ancestor).unwrap();
    assert!(private(&ancestor.join("source.numbers"), false, LIMIT).is_err());
    let public_file = f.directory.join("public.numbers");
    write(&public_file, &f.raw, 0o644);
    std::fs::set_permissions(&public_file, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(private(&public_file, false, LIMIT).is_err());
    assert!(private(Path::new("relative.numbers"), false, LIMIT).is_err());
    assert!(
        private(
            &f.directory.join("ancestor/../source.numbers"),
            false,
            LIMIT
        )
        .is_err()
    );
    assert!(private(&f.source.path, false, f.source.size - 1).is_err());
    let hard_original = f.directory.join("hard-original.numbers");
    write(&hard_original, &f.raw, 0o400);
    let hard_alias = f.directory.join("hard-alias.numbers");
    std::fs::hard_link(&hard_original, &hard_alias).unwrap();
    assert!(private(&hard_original, false, LIMIT).is_err());
    assert!(private(&hard_alias, false, LIMIT).is_err());
    assert_eq!(std::fs::read(&hard_original).unwrap(), f.raw);
    assert_eq!(std::fs::read(&hard_alias).unwrap(), f.raw);
    f.unchanged();
}

#[test]
fn native_trash_registration_changed_raw_and_independent_semantic_mismatch_preserve_evidence() {
    let f = Fixture::new();
    let changed = tempfile::Builder::new()
        .prefix("native-trash-changed-source-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
        .keep();
    let raw = crate::native_import::synthetic_package_archive("Document", b"other Numbers content");
    let path = changed.join("source.numbers");
    write(&path, &raw, 0o400);
    let mut source = f.source.clone();
    source.path = path.clone();
    assert!(source.check(&changed).is_err());
    // Even a matching new raw receipt cannot reuse another archive's semantic
    // authority. The same parser receives valid changed ZIP bytes here.
    source.size = raw.len() as u64;
    source.sha256 = hash(&raw);
    assert!(source.check(&changed).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), raw);
    assert_eq!(
        std::fs::symlink_metadata(&path).unwrap().mode() & 0o777,
        0o400
    );
    f.unchanged();
}
