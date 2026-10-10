use super::*;
use cirrove_auth::{AccessMode, Identity};
use cirrove_core::{CollectionInfo, NodeKind, Scope};

fn fixture() -> (Account, MutationRequest, PackageSemanticIdentity) {
    let account = Account {
        id: uuid::Uuid::new_v4().to_string(),
        label: "synthetic native Trash".into(),
        registration: AppRegistration::ICloud,
        identity: Identity {
            tenant_id: "fixture".into(),
            subject: "fixture".into(),
            username: "fixture@example.test".into(),
            graph_user_id: String::new(),
            display_name: "fixture".into(),
        },
        credential_id: uuid::Uuid::new_v4().to_string(),
        access: AccessMode::ReadOnly,
        drive: CollectionInfo {
            id: "drive".into(),
            name: "iCloud Drive".into(),
            drive_type: "icloud_drive".into(),
            web_url: String::new(),
        },
        root_id: ROOT_ID.into(),
        mount_path: "/var/tmp/synthetic-native-trash/mount".into(),
        enabled: true,
        poll_seconds: 30,
        cache_bytes: 1024,
    };
    let request = MutationRequest {
        scope: Scope {
            account: account.id.clone(),
            provider: "icloud".into(),
            collection: "drive".into(),
        },
        intent: MutationIntent::TrashNativeDocument {
            before: Node {
                id: format!("FILE::com.apple.CloudDocs::{}", uuid::Uuid::new_v4()),
                parent_id: Some(format!(
                    "FOLDER::com.apple.CloudDocs::{}",
                    uuid::Uuid::new_v4()
                )),
                name: "Owned.numbers".into(),
                kind: NodeKind::Folder,
                size: 123,
                modified_unix: 1,
                etag: Some("original-revision".into()),
                content_version: None,
                target: None,
                package: true,
            },
        },
    };
    let semantic = PackageSemanticIdentity {
        version: 2,
        sha256: "a".repeat(64),
        entries: 2,
        files: 1,
        expanded_bytes: 3,
    };
    (account, request, semantic)
}

#[test]
fn native_trash_observer_scope_and_original_guards() -> Result<()> {
    let (account, request, semantic) = fixture();
    assert!(binding(&account, &request, &semantic, 100, &"b".repeat(64)).is_ok());
    for arm in 0..8 {
        let mut a = account.clone();
        let mut r = request.clone();
        let MutationIntent::TrashNativeDocument { before } = &mut r.intent else {
            anyhow::bail!("synthetic intent missing");
        };
        match arm {
            0 => r.scope.account = uuid::Uuid::new_v4().to_string(),
            1 => r.scope.collection = "foreign".into(),
            2 => a.drive.drive_type = "foreign".into(),
            3 => before.package = false,
            4 => before.name = "Owned.pages".into(),
            5 => before.id = "FILE::com.apple.CloudDocs::not-a-uuid".into(),
            6 => before.content_version = Some("foreign-revision".into()),
            _ => before.etag = None,
        }
        assert!(
            binding(&a, &r, &semantic, 100, &"b".repeat(64)).is_err(),
            "arm {arm}"
        );
    }
    Ok(())
}

#[test]
fn native_trash_observer_semantic_and_raw_bounds() {
    let (account, request, semantic) = fixture();
    for arm in 0..5 {
        let mut identity = semantic.clone();
        let mut size = 100;
        let mut digest = "b".repeat(64);
        match arm {
            0 => identity.version = 1,
            1 => identity.files = identity.entries,
            2 => size = 0,
            3 => size = LIMIT + 1,
            _ => digest = "B".repeat(64),
        }
        assert!(
            binding(&account, &request, &identity, size, &digest).is_err(),
            "arm {arm}"
        );
    }
}

fn parent(original: &Node) -> Result<DriveEntry> {
    Ok(serde_json::from_value(serde_json::json!({
        "drivewsid":original.parent_id, "docwsid":"", "zone":"com.apple.CloudDocs",
        "name":"Owned parent", "extension":"", "parentId":ROOT_ID,
        "etag":"parent-revision", "type":"FOLDER", "size":0, "items":[]
    }))?)
}

#[test]
fn native_trash_observer_parent_and_active_identity_guards() -> Result<()> {
    let (account, request, semantic) = fixture();
    let original = binding(&account, &request, &semantic, 100, &"b".repeat(64))?;
    let parent = parent(original)?;
    assert!(parent_binding(std::slice::from_ref(&parent), original).is_ok());
    assert!(parent_binding(&[parent.clone(), parent.clone()], original).is_err());
    let mut foreign = parent.clone();
    foreign.parent_id = "foreign-root".into();
    assert!(parent_binding(&[foreign], original).is_err());
    assert!(absent(std::slice::from_ref(&parent), original).is_ok());
    let mut active = parent;
    active.drivewsid = original.id.clone();
    assert!(absent(&[active], original).is_err());
    Ok(())
}

#[test]
fn native_trash_observer_source_mode_and_byte_preservation() -> Result<()> {
    let directory = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()?
        .keep();
    let path = directory.join("source.numbers");
    let raw = b"retained synthetic bytes";
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o400)
        .open(&path)?;
    output.write_all(raw)?;
    output.sync_all()?;
    let digest = hex::encode(Sha256::digest(raw));
    let file = source_file(&path, raw.len() as u64)?;
    let original = stamp(&file.metadata()?);
    source_unchanged(&path, &file, original, raw.len() as u64, &digest)?;
    assert!(source_unchanged(&path, &file, original, raw.len() as u64, &"a".repeat(64)).is_err());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    assert!(source_file(&path, raw.len() as u64).is_err());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400))?;
    assert_eq!(std::fs::read(&path)?, raw);
    assert_eq!(std::fs::metadata(&path)?.mode() & 0o7777, 0o400);
    Ok(())
}

#[tokio::test]
async fn native_trash_observer_invalid_flat_archive_refuses_before_session() -> Result<()> {
    let (account, request, semantic) = fixture();
    let directory = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()?
        .keep();
    let path = directory.join("source.numbers");
    let raw = b"not a ZIP archive";
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o400)
        .open(&path)?;
    output.write_all(raw)?;
    output.sync_all()?;
    let initial = stamp(&std::fs::metadata(&path)?);
    let state = directory.join("absent-state");
    let digest = hex::encode(Sha256::digest(raw));
    let result = verify_removed(
        &state,
        &account,
        &request,
        &semantic,
        &path,
        raw.len() as u64,
        &digest,
        &directory,
    )
    .await;
    let error = result.err().context("invalid flat archive was admitted")?;
    assert!(
        error
            .downcast_ref::<cirrove_core::ProviderError>()
            .is_some(),
        "must refuse in source scanner, before absent sealed-session state"
    );
    assert!(!state.exists());
    assert!(
        !directory
            .join("native-trash-independent-proof.json")
            .exists()
    );
    assert_eq!(std::fs::read(&path)?, raw);
    assert_eq!(stamp(&std::fs::metadata(&path)?), initial);
    Ok(())
}
