#![allow(clippy::unwrap_used)]
use super::*;
use std::os::unix::fs::PermissionsExt;

fn source_json(path: &Path, content: &[u8], flat: bool) -> serde_json::Value {
    let archive = crate::native_import::synthetic_package_archive(
        if flat {
            "Document"
        } else {
            "Source.numbers/Document"
        },
        content,
    );
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o400)
        .open(path)
        .unwrap();
    file.write_all(&archive).unwrap();
    file.sync_all().unwrap();
    drop(file);
    let file = File::open(path).unwrap();
    let receipt = cirrove_icloud::PackageDownload {
        size: archive.len() as u64,
        sha256: hash(&archive),
    };
    let semantic = if flat {
        cirrove_icloud::package_flat_archive_semantic_identity_v2(
            &file,
            &receipt,
            &CancellationToken::new(),
        )
        .unwrap()
    } else {
        cirrove_icloud::package_archive_semantic_identity_versioned(
            &file,
            &receipt,
            "Source.numbers",
            2,
            &CancellationToken::new(),
        )
        .unwrap()
    };
    let mut value = serde_json::json!({"path":path,"size":receipt.size,"sha256":receipt.sha256,"root":if flat { None } else { Some("Source.numbers") },"semantic":semantic});
    if flat {
        value["source_layout"] = serde_json::json!("flat_numbers");
    }
    value
}

#[test]
fn native_final_flat_registration_requires_explicit_layout_and_present_null_root() {
    let directory = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
        .keep();
    let a = source_json(&directory.join("source-a.numbers"), b"owned before", true);
    let b = source_json(&directory.join("source-b.numbers"), b"owned after", true);
    let before_a = std::fs::read(directory.join("source-a.numbers")).unwrap();
    let before_b = std::fs::read(directory.join("source-b.numbers")).unwrap();
    let parsed: Source = serde_json::from_value(a.clone()).unwrap();
    source(&parsed, &directory, "source-a.numbers").unwrap();
    assert_eq!(serde_json::to_value(&parsed).unwrap(), a);
    for arm in 0..5 {
        let mut changed = a.clone();
        match arm {
            0 => {
                changed.as_object_mut().unwrap().remove("root");
            }
            1 => {
                changed["root"] = serde_json::json!("Source.numbers");
            }
            2 => {
                changed.as_object_mut().unwrap().remove("source_layout");
            }
            3 => {
                changed["source_layout"] = serde_json::json!("wrapped");
            }
            _ => {
                changed["source_layout"] = serde_json::json!("unknown");
            }
        }
        assert!(
            serde_json::from_value::<Source>(changed)
                .and_then(|s| s.check_layout().map_err(serde::de::Error::custom))
                .is_err(),
            "source layout arm {arm}"
        );
    }
    let wrapped = source_json(&directory.join("wrapped.numbers"), b"owned wrapped", false);
    let parsed: Source = serde_json::from_value(wrapped.clone()).unwrap();
    source(&parsed, &directory, "wrapped.numbers").unwrap();
    assert_eq!(serde_json::to_value(&parsed).unwrap(), wrapped);
    let mut wrong_wrapped = wrapped.clone();
    wrong_wrapped["root"] = serde_json::Value::Null;
    assert!(
        serde_json::from_value::<Source>(wrong_wrapped)
            .unwrap()
            .check_layout()
            .is_err()
    );

    let run = Uuid::new_v4();
    let root = PathBuf::from(format!("/var/tmp/cirrove-native-final-{run}"));
    let parent = Node {
        id: format!("FOLDER::com.apple.CloudDocs::{run}"),
        parent_id: Some(cirrove_icloud::ROOT_ID.into()),
        name: format!("Cirrove-Native-{run}"),
        kind: NodeKind::Folder,
        size: 0,
        modified_unix: 0,
        etag: None,
        content_version: None,
        target: None,
        package: false,
    };
    let original = Node {
        id: format!("FILE::com.apple.CloudDocs::{run}"),
        parent_id: Some(parent.id.clone()),
        name: format!("Cirrove-Numbers-Parent-{run}.numbers"),
        kind: NodeKind::Folder,
        size: 12,
        modified_unix: 0,
        etag: Some("original-v1".into()),
        content_version: None,
        target: None,
        package: true,
    };
    let value = serde_json::json!({"version":1,"run":run,"account":Uuid::new_v4(),"label":"iCloudNativeFinalValidation","session_directory":root,"tmpdir":directory,"settings_sha256":"a".repeat(64),"authorization_sha256":"b".repeat(64),"executable_sha256":"c".repeat(64),"started_unix":now(),"deadline_unix":now()+120,"parent_creation":Uuid::new_v4(),"import":Uuid::new_v4(),"parent":parent,"original":original,"source_a":a,"source_b":b,"preflight_sha256":"d".repeat(64),"recovery":null});
    let registration: Registration = serde_json::from_value(value.clone()).unwrap();
    registration
        .validate(&root.join("loss-registration.json"))
        .unwrap();
    assert!(matches!(
        registration.request().representation,
        UploadRepresentation::FlatNumbersReplacementArchive { .. }
    ));
    let mut incompatible = value;
    incompatible["source_b"] = wrapped;
    assert!(
        serde_json::from_value::<Registration>(incompatible)
            .unwrap()
            .validate(&root.join("loss-registration.json"))
            .is_err()
    );
    assert_eq!(
        std::fs::read(directory.join("source-a.numbers")).unwrap(),
        before_a
    );
    assert_eq!(
        std::fs::read(directory.join("source-b.numbers")).unwrap(),
        before_b
    );
}
