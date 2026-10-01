use super::*;

fn fixture() -> (
    Account,
    OwnedPackagePlan,
    PackageSemanticIdentity,
    UploadRecord,
    DriveEntry,
) {
    let account = Account {
        id: Uuid::new_v4().to_string(),
        credential_id: Uuid::new_v4().to_string(),
        label: LABEL.into(),
        registration: AppRegistration::ICloud,
        identity: cirrove_auth::Identity {
            tenant_id: String::new(),
            subject: "fixture".into(),
            username: "fixture@example.invalid".into(),
            graph_user_id: String::new(),
            display_name: "Fixture".into(),
        },
        access: AccessMode::ReadWrite,
        drive: cirrove_onedrive::DriveInfo {
            id: "drive".into(),
            name: "iCloud".into(),
            drive_type: "icloud_drive".into(),
            web_url: String::new(),
        },
        root_id: ROOT_ID.into(),
        mount_path: Path::new(PUBLIC).join("mount"),
        enabled: true,
        poll_seconds: 60,
        cache_bytes: 64 * 1024 * 1024,
    };
    let source: DriveEntry = serde_json::from_value(serde_json::json!({
        "drivewsid":"FILE::com.apple.CloudDocs::source", "docwsid":"source", "zone":"com.apple.CloudDocs",
        "type":"FILE", "parentId":"FOLDER::com.apple.CloudDocs::owned", "name":format!("Cirrove Package Source {SOURCE}"),
        "extension":"pages", "etag":"source-r1", "size":99
    })).unwrap();
    let plan = OwnedPackagePlan {
        scope: scope(&account),
        operation: Uuid::parse_str(SOURCE).unwrap(),
        parent: source.parent_id.clone(),
        parent_name: format!("Cirrove Package Validation {SOURCE}"),
        source,
        destination: "old import.pages".into(),
        archive_size: 1234,
        archive_sha256: "a".repeat(64),
    };
    let semantic = PackageSemanticIdentity {
        version: 1,
        sha256: "b".repeat(64),
        entries: 1,
        files: 1,
        expanded_bytes: 12,
    };
    let name = format!("Cirrove Public Import {RUN}.pages");
    let node = Node {
        id: "FILE::com.apple.CloudDocs::new".into(),
        parent_id: Some(plan.parent.clone()),
        name: name.clone(),
        kind: NodeKind::Folder,
        size: 99,
        modified_unix: 0,
        etag: Some("r1".into()),
        content_version: Some("r1".into()),
        target: None,
        package: true,
    };
    let row: UploadRecord = serde_json::from_value(serde_json::json!({
        "representation": UploadRepresentation::PackageArchive { expected_root: plan.source.display_name(), semantic: semantic.clone() },
        "package_completion": semantic, "id":Uuid::new_v4(), "sequence":1,
        "scope":scope(&account), "intent":UploadIntent::Create { parent:plan.parent.clone(), name },
        "state":UploadState::Uploaded, "size":plan.archive_size, "sha256":plan.archive_sha256,
        "attempt":null, "remote":node
    })).unwrap();
    let entry: DriveEntry = serde_json::from_value(serde_json::json!({
        "drivewsid":"FILE::com.apple.CloudDocs::new", "docwsid":"new", "zone":"com.apple.CloudDocs",
        "type":"FILE", "parentId":plan.parent, "name":format!("Cirrove Public Import {RUN}"),
        "extension":"pages", "etag":"r1", "size":99
    }))
    .unwrap();
    (account, plan, semantic, row, entry)
}

#[test]
fn public_verifier_accepts_only_bound_completed_package_receipt() {
    let (account, plan, semantic, row, _) = fixture();
    assert!(receipt_binding(&row, &account, &plan, &semantic).is_ok());
    for arm in 0..10 {
        let mut changed = row.clone();
        match arm {
            0 => changed.scope.account = Uuid::new_v4().to_string(),
            1 => changed.state = UploadState::VerifyRequired,
            2 => changed.representation = UploadRepresentation::FileBytes,
            3 => changed.package_completion = None,
            4 => changed.size += 1,
            5 => changed.sha256 = "c".repeat(64),
            6 => changed.remote.as_mut().unwrap().name = "unrelated.pages".into(),
            7 => changed.remote.as_mut().unwrap().id = plan.source.drivewsid.clone(),
            8 => changed.remote.as_mut().unwrap().package = false,
            _ => {
                changed.intent = UploadIntent::Replace {
                    item: "unrelated".into(),
                    expected_etag: "r1".into(),
                }
            }
        }
        assert!(
            receipt_binding(&changed, &account, &plan, &semantic).is_err(),
            "arm {arm}"
        );
    }
}

#[test]
fn public_verifier_refuses_duplicate_identity_names_and_changed_revision() {
    let (_, _, _, row, entry) = fixture();
    let node = row.remote.unwrap();
    assert!(exact_entry(std::slice::from_ref(&entry), &node).is_ok());
    let mut duplicate_id = entry.clone();
    duplicate_id.name = "Different name with the same ID".into();
    assert!(exact_entry(&[entry.clone(), duplicate_id], &node).is_err());
    let mut duplicate_name = entry.clone();
    duplicate_name.drivewsid.push_str("-other");
    assert!(exact_entry(&[entry.clone(), duplicate_name], &node).is_err());
    for arm in 0..4 {
        let mut changed = entry.clone();
        match arm {
            0 => changed.etag = "r2".into(),
            1 => changed.size += 1,
            2 => changed.parent_id = "other".into(),
            _ => changed.zone = "com.apple.Pages".into(),
        }
        let expected = match arm {
            0 => "public imported revision changed",
            1 => "public imported size changed",
            2 => "public imported parent changed",
            _ => "public imported zone changed",
        };
        assert_eq!(
            exact_entry(&[changed], &node).unwrap_err().to_string(),
            expected
        );
    }
}

#[tokio::test]
async fn public_verifier_rejects_wrong_run_before_any_io() {
    let error = icloud_public_native_verify(Uuid::nil()).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "run is not the preregistered public native import"
    );
}

#[test]
fn public_verifier_accepts_canonical_etag_receipt_and_exact_legacy_alias_only() {
    let (account, plan, semantic, mut row, _) = fixture();
    for legacy in [false, true] {
        let node = row.remote.as_mut().unwrap();
        node.content_version = if legacy { node.etag.clone() } else { None };
        let before = row.clone();
        assert!(
            receipt_binding(&row, &account, &plan, &semantic).is_ok(),
            "legacy={legacy}"
        );
        assert_eq!(row.remote, before.remote);
    }
    for bad in ["different-tag", ""] {
        row.remote.as_mut().unwrap().content_version = Some(bad.into());
        assert!(receipt_binding(&row, &account, &plan, &semantic).is_err());
    }
    row.remote.as_mut().unwrap().etag = None;
    row.remote.as_mut().unwrap().content_version = None;
    assert!(receipt_binding(&row, &account, &plan, &semantic).is_err());
}

#[tokio::test]
async fn public_manifest_wrong_run_refused_before_retained_state_or_network() {
    let error = icloud_public_native_manifest(Uuid::nil())
        .await
        .expect_err("unregistered run must not access retained fixture");
    assert_eq!(
        error.to_string(),
        "run is not the preregistered public native import"
    );
}

#[test]
fn manifest_current_revision_is_read_bound_without_weakening_import_verification() {
    let (_, _, _, row, mut entry) = fixture();
    let receipt = row.remote.unwrap();
    entry.etag = "current-revision".into();
    assert!(exact_entry(std::slice::from_ref(&entry), &receipt).is_err());
    let (_, current) = manifest_current_entry(std::slice::from_ref(&entry), &receipt).unwrap();
    assert_eq!(current.etag.as_deref(), Some("current-revision"));
    assert_ne!(current.etag, receipt.etag);
    assert!(exact_entry(std::slice::from_ref(&entry), &current).is_ok());
    for arm in 0..5 {
        let mut changed = entry.clone();
        match arm {
            0 => changed.etag.clear(),
            1 => changed.drivewsid.push_str("-other"),
            2 => changed.parent_id.push_str("-other"),
            3 => changed.name.push_str("-other"),
            _ => changed.size += 1,
        }
        assert!(manifest_current_entry(&[changed], &receipt).is_err());
    }
    entry.etag = "later-revision".into();
    assert!(exact_entry(&[entry], &current).is_err());
}
