//! Keep subprocess launch separate from immediate lock-release assertions in
//! local_export: fork/exec may briefly retain other threads' lock descriptors.
//! The observed transient Busy has not been conclusively attributed to that race.
#![allow(clippy::unwrap_used)]
use cirrove_core::{CancellationToken, Scope};
use cirrove_service::journal::{UploadIntent, UploadJournal};
use std::{fs, os::unix::fs::PermissionsExt};

#[test]
fn disabled_account_recovery_cli_preserves_state_and_refuses_active_owners() {
    use cirrove_service::accounts::{OfflineRecovery, Settings, account_lock};
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    cirrove_service::private_dir(&state).unwrap();
    let mut settings: Settings =
        serde_json::from_str(include_str!("../../cirrove-desktop/fixtures/accounts.json")).unwrap();
    settings.accounts.truncate(1);
    let account = &mut settings.accounts[0];
    account.enabled = false;
    account.mount_path = temp.path().join("mount");
    let id = account.id.clone();
    let directory = state.join("accounts").join(&id);
    let mut journal = UploadJournal::open(&directory.join("journal"), &id, 4096).unwrap();
    let row = journal
        .enqueue(
            Scope {
                account: id.clone(),
                provider: "icloud".into(),
                collection: "drive".into(),
            },
            UploadIntent::Create {
                parent: "root".into(),
                name: "odd ' file\n.txt".into(),
            },
            &b"offline bytes"[..],
        )
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
    let before = fs::read(state.join("accounts.json")).unwrap();
    let held = account_lock(&directory).unwrap();
    assert!(OfflineRecovery::open(&state, "work").is_err());
    drop(held);
    let recovery = OfflineRecovery::open(&state, "work").unwrap();
    assert!(account_lock(&directory).is_err());
    assert!(
        recovery
            .export(
                row.id,
                &state.join("forbidden"),
                &CancellationToken::new(),
                |_| {}
            )
            .is_err()
    );
    assert!(
        recovery
            .export(
                row.id,
                &temp.path().join("mount/forbidden"),
                &CancellationToken::new(),
                |_| {}
            )
            .is_err()
    );
    let rows = recovery.list(0, 200).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].operation, Some(row.id));
    assert_eq!(rows[0].name, "odd ' file\n.txt");
    assert!(recovery.list(rows[0].sequence, 200).unwrap().is_empty());
    drop(recovery);
    let listed = std::process::Command::new(env!("CARGO_BIN_EXE_cirrove"))
        .env_remove("HOME")
        .env_remove("XDG_STATE_HOME")
        .args(["recovery-saves", "--label", "work", "--state"])
        .arg(&state)
        .output()
        .unwrap();
    assert!(
        listed.status.success(),
        "{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let listed: Vec<cirrove_service::recent::LocalChange> =
        serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(listed[0].operation, Some(row.id));
    let destination = temp.path().join("rescued.txt");
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_cirrove"))
        .env_remove("HOME")
        .env_remove("XDG_STATE_HOME")
        .args(["export-save", "--offline", "--label", "work", "--state"])
        .arg(&state)
        .arg("--operation")
        .arg(row.id.to_string())
        .arg("--destination")
        .arg(&destination)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(fs::read(destination).unwrap(), b"offline bytes");
    assert_eq!(fs::read(state.join("accounts.json")).unwrap(), before);
    settings.accounts[0].enabled = true;
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
    assert!(OfflineRecovery::open(&state, "work").is_err());
}
