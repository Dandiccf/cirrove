//! Recover retained mutable bytes without opening a provider or sealing a save.
#![allow(clippy::unwrap_used)]
use cirrove_core::{Node, NodeKind, Scope};
use cirrove_service::{accounts::Settings, journal::UploadJournal};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};
fn fixture(state: &Path) -> (String, uuid::Uuid, u64) {
    cirrove_service::private_dir(state).unwrap();
    let mut settings: Settings =
        serde_json::from_str(include_str!("../../cirrove-desktop/fixtures/accounts.json")).unwrap();
    settings.accounts.truncate(1);
    let account = &mut settings.accounts[0];
    account.enabled = false;
    account.mount_path = state.parent().unwrap().join("mount");
    let id = account.id.clone();
    let mut journal = UploadJournal::open(
        &state.join("accounts").join(&id).join("journal"),
        &id,
        1048576,
    )
    .unwrap();
    let node = Node {
        id: "local-file".into(),
        parent_id: Some("root".into()),
        name: "unsaved ' edit\n.txt".into(),
        kind: NodeKind::File,
        size: 0,
        modified_unix: 0,
        etag: None,
        content_version: None,
        target: None,
        package: false,
    };
    let working = journal
        .create_working(
            Scope {
                account: id.clone(),
                provider: "icloud".into(),
                collection: "drive".into(),
            },
            node,
            true,
            &b""[..],
        )
        .unwrap();
    let (_, working) = journal
        .write_working(working.id, 0, b"unsealed working bytes")
        .unwrap();
    drop(journal);
    fs::write(
        state.join("accounts.json"),
        serde_json::to_vec(&settings).unwrap(),
    )
    .unwrap();
    fs::set_permissions(
        state.join("accounts.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    (id, working.id, working.generation)
}
fn cli(state: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cirrove"));
    command.env_remove("HOME").env_remove("XDG_STATE_HOME");
    command
        .arg("export-working")
        .args(["--label", "work", "--state"])
        .arg(state);
    command
}
#[test]
fn offline_cli_exports_unsealed_bytes_without_sealing_or_replaying() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let (account, id, generation) = fixture(&state);
    let root = state.join("accounts").join(account).join("journal");
    let before = fs::read(root.join("uploads.db")).unwrap();
    let destination = temp.path().join("recovered.txt");
    let output = cli(&state)
        .arg("--file")
        .arg(id.to_string())
        .arg("--generation")
        .arg(generation.to_string())
        .arg("--destination")
        .arg(&destination)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(destination).unwrap(), b"unsealed working bytes");
    assert_eq!(fs::read(root.join("uploads.db")).unwrap(), before);
}

#[test]
fn unexpected_working_file_mutation_during_copy_is_not_published() {
    use cirrove_core::CancellationToken;
    use cirrove_service::journal::RecoveryJournal;
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let (account, id, generation) = fixture(&state);
    let root = state.join("accounts").join(account).join("journal");
    let recovery = RecoveryJournal::open(
        &root,
        root.parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap(),
    )
    .unwrap();
    let destination = temp.path().join("raced.txt");
    let result = recovery.export_working(
        id,
        generation,
        &destination,
        &CancellationToken::new(),
        |_| {
            // Simulate an out-of-contract writer bypassing the journal lease.
            fs::write(
                root.join("working").join(id.to_string()),
                b"changed! working bytes",
            )
            .unwrap();
        },
    );
    assert!(result.is_err(), "changed mutable source must not publish");
    assert!(!destination.exists());
}

#[test]
fn working_recovery_retains_lock_uses_actual_size_and_refuses_stale_or_unsafe_exports() {
    use cirrove_core::CancellationToken;
    use cirrove_service::accounts::OfflineRecovery;
    use sha2::{Digest, Sha256};
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let (account, id, generation) = fixture(&state);
    let root = state.join("accounts").join(&account).join("journal");
    // Interrupted write: actual size differs from the last committed metadata.
    fs::write(root.join("working").join(id.to_string()), b"partial").unwrap();
    let before = fs::read(root.join("uploads.db")).unwrap();
    let recovery = OfflineRecovery::open(&state, "work").unwrap();
    assert!(UploadJournal::open(&root, &account, 1048576).is_err());
    let (rows, next) = recovery.working_list(None, 1).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(next, Some(id));
    assert_eq!(rows[0].size, 7);
    assert_eq!(
        rows[0].recorded_size,
        b"unsealed working bytes".len() as u64
    );
    assert!(recovery.working_list(next, 1).unwrap().0.is_empty());
    let destination = temp.path().join("restored");
    let token = CancellationToken::new();
    assert!(
        recovery
            .export_working(id, generation + 1, &destination, &token, |_| {})
            .is_err()
    );
    assert!(!destination.exists());
    for target in [state.join("forbidden"), temp.path().join("mount/forbidden")] {
        assert!(
            recovery
                .export_working(id, generation, &target, &token, |_| {})
                .is_err()
        );
    }
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(
        recovery
            .export_working(id, generation, &destination, &cancelled, |_| {})
            .is_err()
    );
    assert!(!destination.exists());
    let receipt = recovery
        .export_working(id, generation, &destination, &token, |_| {
            assert!(UploadJournal::open(&root, &account, 1048576).is_err());
        })
        .unwrap();
    assert_eq!(receipt.source.size, 7);
    assert_eq!(receipt.sha256, hex::encode(Sha256::digest(b"partial")));
    assert_eq!(fs::read(&destination).unwrap(), b"partial");
    assert_eq!(
        fs::metadata(&destination).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(
        recovery
            .export_working(id, generation, &destination, &token, |_| {})
            .is_err()
    );
    assert_eq!(fs::read(root.join("uploads.db")).unwrap(), before);
}

#[test]
fn working_source_symlinks_and_enabled_accounts_are_refused() {
    use cirrove_core::CancellationToken;
    use cirrove_service::accounts::OfflineRecovery;
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let (account, id, generation) = fixture(&state);
    let root = state.join("accounts").join(&account).join("journal");
    let file = root.join("working").join(id.to_string());
    let retained = root.join("working/retained-test-bytes");
    // Ordinary synthetic files, no recursive cleanup or live mount involved.
    fs::rename(&file, &retained).unwrap();
    symlink(&retained, &file).unwrap();
    let recovery = OfflineRecovery::open(&state, "work").unwrap();
    assert!(recovery.working_list(None, 200).is_err());
    assert!(
        recovery
            .export_working(
                id,
                generation,
                &temp.path().join("unsafe"),
                &CancellationToken::new(),
                |_| {}
            )
            .is_err()
    );
    drop(recovery);
    let mut settings: Settings =
        serde_json::from_slice(&fs::read(state.join("accounts.json")).unwrap()).unwrap();
    settings.accounts[0].enabled = true;
    fs::write(
        state.join("accounts.json"),
        serde_json::to_vec(&settings).unwrap(),
    )
    .unwrap();
    assert!(OfflineRecovery::open(&state, "work").is_err());
}

#[test]
fn clean_working_rows_advance_pagination_and_unlinked_empty_bytes_can_be_recovered() {
    use cirrove_core::CancellationToken;
    use cirrove_service::accounts::OfflineRecovery;
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let (account, id, generation) = fixture(&state);
    let root = state.join("accounts").join(account).join("journal");
    let db = rusqlite::Connection::open(root.join("uploads.db")).unwrap();
    db.execute(
        "UPDATE working_files SET body=json_set(body,'$.dirty',json('false')) WHERE id=?1",
        [id.to_string()],
    )
    .unwrap();
    drop(db);
    let recovery = OfflineRecovery::open(&state, "work").unwrap();
    let (rows, next) = recovery.working_list(None, 1).unwrap();
    assert!(rows.is_empty());
    assert_eq!(next, Some(id));
    assert!(
        recovery
            .export_working(
                id,
                generation,
                &temp.path().join("clean"),
                &CancellationToken::new(),
                |_| {}
            )
            .is_err()
    );
    drop(recovery);
    let db = rusqlite::Connection::open(root.join("uploads.db")).unwrap();
    db.execute(
        "UPDATE working_files SET body=json_set(body,'$.unlinked',json('true')) WHERE id=?1",
        [id.to_string()],
    )
    .unwrap();
    drop(db);
    fs::write(root.join("working").join(id.to_string()), b"").unwrap();
    let recovery = OfflineRecovery::open(&state, "work").unwrap();
    let target = temp.path().join("unlinked");
    let result = recovery
        .export_working(id, generation, &target, &CancellationToken::new(), |_| {})
        .unwrap();
    assert!(result.source.unlinked);
    assert_eq!(result.source.size, 0);
    assert_eq!(fs::read(target).unwrap(), b"");
}
