use super::*;

fn source() -> Account {
    Account {
        id: Uuid::new_v4().to_string(),
        credential_id: Uuid::new_v4().to_string(),
        label: "iCloudGuiValidation".into(),
        registration: AppRegistration::ICloud,
        identity: cirrove_auth::Identity {
            tenant_id: String::new(),
            subject: "synthetic".into(),
            username: "synthetic@example.invalid".into(),
            graph_user_id: String::new(),
            display_name: "Synthetic".into(),
        },
        access: AccessMode::ReadOnly,
        drive: cirrove_onedrive::DriveInfo {
            id: "drive".into(),
            name: "iCloud Drive".into(),
            drive_type: "icloud_drive".into(),
            web_url: String::new(),
        },
        root_id: ROOT_ID.into(),
        mount_path: "/synthetic/unused".into(),
        enabled: true,
        poll_seconds: 60,
        cache_bytes: 64 * 1024 * 1024,
    }
}

#[test]
fn bootstrap_clone_has_independent_ids_and_preserves_source() {
    let source = source();
    let before = serde_json::to_vec(&source).unwrap();
    let clone = cloned_account(&source, Path::new("/synthetic/new"));
    assert_ne!(clone.id, source.id);
    assert_ne!(clone.credential_id, source.credential_id);
    assert_eq!(clone.identity.username, source.identity.username);
    assert_eq!(clone.access, AccessMode::ReadWrite);
    assert!(clone.enabled);
    assert_eq!(clone.mount_path, Path::new("/synthetic/new/mount"));
    assert_eq!(clone.root_id, ROOT_ID);
    assert_eq!(serde_json::to_vec(&source).unwrap(), before);
}

#[test]
fn bootstrap_source_selection_rejects_wrong_modes_and_duplicates() {
    let mut settings = Settings {
        version: 2,
        accounts: vec![source()],
    };
    assert!(source_account(&settings).is_ok());
    settings.accounts[0].access = AccessMode::ReadWrite;
    assert!(source_account(&settings).is_err());
    settings.accounts[0].access = AccessMode::ReadOnly;
    settings.accounts[0].enabled = false;
    assert!(source_account(&settings).is_err());
    settings.accounts[0].enabled = true;
    settings.accounts.push(settings.accounts[0].clone());
    assert!(source_account(&settings).is_err());
}

#[test]
fn bootstrap_claim_refuses_existing_directory_or_symlink() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("run");
    claim(&dir).unwrap();
    std::fs::write(dir.join("evidence"), b"retained").unwrap();
    assert!(claim(&dir).is_err());
    assert_eq!(std::fs::read(dir.join("evidence")).unwrap(), b"retained");
    let link = temp.path().join("link");
    std::os::unix::fs::symlink(&dir, &link).unwrap();
    assert!(claim(&link).is_err());
    assert!(check_private(&link).is_err());
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(check_private(&dir).is_err());
}

#[test]
fn bootstrap_settings_roundtrip_and_never_overwrite() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    claim(&state).unwrap();
    let account = cloned_account(&source(), temp.path());
    publish_settings(&state, account.clone()).unwrap();
    let first = std::fs::read(state.join("accounts.json")).unwrap();
    let loaded = Settings::load(&state).unwrap();
    assert_eq!(loaded.accounts[0].id, account.id);
    assert_eq!(loaded.accounts[0].access, AccessMode::ReadWrite);
    assert!(publish_settings(&state, cloned_account(&source(), temp.path())).is_err());
    assert_eq!(std::fs::read(state.join("accounts.json")).unwrap(), first);
    assert_eq!(
        std::fs::metadata(state.join("accounts.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[tokio::test]
async fn bootstrap_rejects_unregistered_run_before_io_or_keyring() {
    let error = icloud_public_native_bootstrap(Uuid::nil())
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "run is not the preregistered public native import"
    );
}

#[test]
fn bootstrap_atomic_publication_refuses_existing_settings() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    claim(&state).unwrap();
    std::fs::write(state.join("accounts.json"), b"original evidence").unwrap();
    assert!(publish_settings(&state, cloned_account(&source(), temp.path())).is_err());
    assert_eq!(
        std::fs::read(state.join("accounts.json")).unwrap(),
        b"original evidence"
    );
}

#[test]
fn bootstrap_storage_rejects_ram_and_unknown_or_failed_detection() {
    assert_eq!(checked_filesystem(true, b"btrfs\n").unwrap(), "btrfs");
    for output in [
        b"tmpfs\n".as_slice(),
        b"ramfs\n",
        b"",
        b"overlay\n",
        b"btrfs\ntmpfs\n",
    ] {
        assert!(checked_filesystem(true, output).is_err());
    }
    assert!(checked_filesystem(false, b"btrfs\n").is_err());
}

#[test]
fn bootstrap_refuses_another_apple_identity_before_session_clone() {
    let gui = source();
    let mut retained = gui.clone();
    retained.label = "iCloudOwnedPackageValidation".into();
    retained.enabled = false;
    assert!(source_identity_matches(&gui, &retained).is_ok());
    let mut changed = gui.clone();
    changed.identity.username = "other@example.invalid".into();
    assert!(source_identity_matches(&changed, &retained).is_err());
    changed = gui.clone();
    changed.identity.subject = "other-subject".into();
    assert!(source_identity_matches(&changed, &retained).is_err());
}

#[test]
fn fresh_native_trash_bootstrap_refuses_old_runs_arbitrary_names_and_paths() {
    let run = Uuid::new_v4();
    let name = format!("Cirrove Public Trash {run}.pages");
    assert_eq!(
        trash_directory(run, &name).unwrap(),
        PathBuf::from(format!("/var/tmp/cirrove-public-native-trash-{run}"))
    );
    for bad in [
        "../elsewhere.pages",
        "Existing.pages",
        "Cirrove Public Trash other.pages",
    ] {
        assert!(trash_directory(run, bad).is_err());
    }
    for old in [
        Uuid::nil(),
        Uuid::parse_str(RUN).unwrap(),
        Uuid::parse_str("ac9e5456-bd10-4b7d-9215-21bbb85dde69").unwrap(),
    ] {
        assert!(trash_directory(old, &format!("Cirrove Public Trash {old}.pages")).is_err());
    }
}
