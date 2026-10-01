use super::*;
fn fixture() -> (Account, Account, OwnedPackagePlan, Preregistration) {
    let source = Account {
        id: Uuid::from_u128(21).to_string(),
        credential_id: Uuid::from_u128(22).to_string(),
        label: "iCloudOwnedPackageValidation".into(),
        registration: AppRegistration::ICloud,
        identity: cirrove_auth::Identity {
            tenant_id: String::new(),
            subject: "fixture".into(),
            username: "fixture@example.invalid".into(),
            graph_user_id: String::new(),
            display_name: "Fixture".into(),
        },
        access: AccessMode::ReadOnly,
        drive: cirrove_onedrive::DriveInfo {
            id: "drive".into(),
            name: "iCloud".into(),
            drive_type: "icloud_drive".into(),
            web_url: String::new(),
        },
        root_id: ROOT_ID.into(),
        mount_path: "/var/tmp/unused-package-fixture".into(),
        enabled: false,
        poll_seconds: 60,
        cache_bytes: 64 * 1024 * 1024,
    };
    let mut account = source.clone();
    account.id = Uuid::from_u128(23).to_string();
    account.credential_id = Uuid::from_u128(24).to_string();
    let entry:DriveEntry=serde_json::from_value(serde_json::json!({"drivewsid":"FILE::com.apple.CloudDocs::source","docwsid":"source","zone":"com.apple.CloudDocs","parentId":"FOLDER::com.apple.CloudDocs::old-owned","name":format!("Cirrove Package Source {SOURCE}"),"extension":"pages","etag":"source-revision","size":16,"type":"FILE"})).unwrap();
    let plan = OwnedPackagePlan {
        scope: scope(&source),
        operation: Uuid::parse_str(SOURCE).unwrap(),
        parent: entry.parent_id.clone(),
        parent_name: format!("Cirrove Package Validation {SOURCE}"),
        source: entry,
        destination: format!("Cirrove Package Import {SOURCE}.pages"),
        archive_size: 1234,
        archive_sha256: "a".repeat(64),
    };
    let run = Uuid::parse_str(RUN).unwrap();
    let prereg = Preregistration {
        version: 1,
        purpose: PURPOSE.into(),
        run,
        source_run: plan.operation,
        account: account.id.clone(),
        source_account: source.id.clone(),
        apple_account: source.identity.username.clone(),
        source_drive: plan.source.drivewsid.clone(),
        source_document: plan.source.docwsid.clone(),
        source_revision: plan.source.etag.clone(),
        source_root: plan.source.display_name(),
        source_size: plan.archive_size,
        source_sha256: plan.archive_sha256.clone(),
        semantic: PackageSemanticIdentity {
            version: 1,
            sha256: "b".repeat(64),
            entries: 1,
            files: 1,
            expanded_bytes: 16,
        },
        parent_name: format!("Cirrove Package Validation {run}"),
        source_name: format!("Cirrove Package Source {run}.pages"),
        target_name: format!("Cirrove Package Import {run}.pages"),
    };
    (account, source, plan, prereg)
}
#[test]
fn native_trash_preregistration_rejects_foreign_accounts_sources_and_prior_imports() {
    let (account, source, plan, prereg) = fixture();
    prereg.validate(&account, &source, &plan).unwrap();
    for arm in 0..12 {
        let mut changed = prereg.clone();
        match arm {
            0 => changed.run = Uuid::parse_str(SOURCE).unwrap(),
            1 => changed.run = Uuid::new_v4(),
            2 => changed.source_run = Uuid::new_v4(),
            3 => changed.source_drive = "personal-id".into(),
            4 => changed.source_document = "personal-document".into(),
            5 => changed.source_revision = "new-revision".into(),
            6 => changed.source_sha256 = "c".repeat(64),
            7 => changed.source_root = "Personal.pages".into(),
            8 => changed.account = source.id.clone(),
            9 => changed.apple_account = "another@example.invalid".into(),
            10 => changed.target_name = "Personal.pages".into(),
            _ => changed.purpose = "different-purpose".into(),
        }
        assert!(
            changed.validate(&account, &source, &plan).is_err(),
            "foreign provenance arm {arm} accepted"
        );
    }
    let mut changed = account.clone();
    changed.identity.subject = "different-account".into();
    assert!(prereg.validate(&changed, &source, &plan).is_err());
    changed = account.clone();
    changed.enabled = true;
    assert!(prereg.validate(&changed, &source, &plan).is_err());
    changed = account.clone();
    changed.credential_id = source.credential_id.clone();
    assert!(prereg.validate(&changed, &source, &plan).is_err());
    let mut changed_plan = plan.clone();
    changed_plan.scope.account = Uuid::new_v4().to_string();
    assert!(prereg.validate(&account, &source, &changed_plan).is_err());
}
#[test]
fn native_trash_phase_receipts_are_durable_ordered_and_cannot_be_replayed() {
    let (_, _, _, prereg) = fixture();
    let dir = tempfile::tempdir().unwrap();
    assert!(
        advance(
            dir.path(),
            &prereg,
            Phase::Preregistered,
            Phase::FolderArmed
        )
        .is_err(),
        "missing preregistration admitted"
    );
    record(
        &dir.path()
            .join(format!("{}.json", Phase::Preregistered.name())),
        &PhaseReceipt {
            run: prereg.run,
            account: prereg.account.clone(),
            phase: Phase::Preregistered,
        },
    )
    .unwrap();
    assert!(
        advance(
            dir.path(),
            &prereg,
            Phase::Preregistered,
            Phase::SourceArmed
        )
        .is_err(),
        "phase skip admitted"
    );
    advance(
        dir.path(),
        &prereg,
        Phase::Preregistered,
        Phase::FolderArmed,
    )
    .unwrap();
    assert!(
        advance(
            dir.path(),
            &prereg,
            Phase::Preregistered,
            Phase::FolderArmed
        )
        .is_err(),
        "armed phase replay admitted"
    );
    let persisted: PhaseReceipt = read_json(
        &dir.path()
            .join(format!("{}.json", Phase::FolderArmed.name())),
        4096,
    )
    .unwrap();
    assert!(persisted.phase == Phase::FolderArmed && persisted.run == prereg.run);
    let mut foreign = prereg;
    foreign.account = Uuid::new_v4().to_string();
    assert!(
        advance(
            dir.path(),
            &foreign,
            Phase::FolderArmed,
            Phase::FolderCreated
        )
        .is_err()
    );
}
#[test]
fn native_trash_cli_run_is_fixed_and_old_source_is_never_sacrificial() {
    check_run(Uuid::parse_str(RUN).unwrap()).unwrap();
    assert!(check_run(Uuid::parse_str(SOURCE).unwrap()).is_err());
    assert!(check_run(Uuid::new_v4()).is_err());
}

#[test]
fn native_trash_local_capture_accepts_repository_parent_components_without_following_symlinks() {
    use std::os::unix::fs::symlink;
    let fixture = tempfile::tempdir_in("/var/tmp").unwrap();
    let root = fixture.path().canonicalize().unwrap();
    let crates = root.join("crates/service");
    std::fs::create_dir_all(&crates).unwrap();
    let source = root.join("retained");
    let run = root.join("run");
    for directory in [&source, &run, &run.join("capture")] {
        std::fs::create_dir(directory).unwrap();
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let bytes =
        crate::native_import::synthetic_package_archive("Owned.pages/Document", b"synthetic");
    let mut archive = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(source.join("source.zip"))
        .unwrap();
    archive.write_all(&bytes).unwrap();
    drop(archive);
    let relative_source = crates.join("../../retained");
    let relative_run = crates.join("../../run");
    let cancel = CancellationToken::new();
    assert!(
        ValidatedPackageArchive::capture(
            &relative_source.join("source.zip"),
            &relative_run.join("capture"),
            "Owned.pages",
            &cancel
        )
        .is_err(),
        "fixture must reproduce the old staging-path refusal"
    );
    capture_local_source(&relative_source, &relative_run, "Owned.pages", &cancel).unwrap();
    assert!(
        !canonical_private_directory(&relative_run)
            .unwrap()
            .components()
            .any(|c| c == std::path::Component::ParentDir)
    );
    let alias = root.join("alias");
    symlink(&root, &alias).unwrap();
    assert!(
        capture_local_source(
            &alias.join("retained"),
            &relative_run,
            "Owned.pages",
            &cancel
        )
        .is_err(),
        "canonicalization hid a source ancestor symlink"
    );
    assert!(
        capture_local_source(&relative_source, &alias.join("run"), "Owned.pages", &cancel).is_err(),
        "canonicalization hid a staging ancestor symlink"
    );
    std::fs::remove_file(source.join("source.zip")).unwrap();
    symlink(root.join("missing.zip"), source.join("source.zip")).unwrap();
    assert!(
        capture_local_source(&relative_source, &relative_run, "Owned.pages", &cancel).is_err(),
        "source no-follow guard was lost"
    );
}
