#![cfg(feature = "icloud-write-probe")]
use anyhow::Result;
use std::{os::unix::fs::PermissionsExt, process::Command};
#[test]
fn editor_source_proof_cli_refuses_invalid_registration_without_state_access() -> Result<()> {
    let root = tempfile::tempdir()?.keep();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    let path = root.join("invalid-source-registration.json");
    std::fs::write(&path, b"{}")?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    let unused = root.join("never-created-state");
    let output = Command::new(env!("CARGO_BIN_EXE_cirrove"))
        .args(["owned-icloud-editor-source-proof", "--registration"])
        .arg(&path)
        .arg("--sha256")
        .arg("a".repeat(64))
        .env("XDG_STATE_HOME", &unused)
        .output()?;
    assert!(!unused.exists());
    assert_eq!(std::fs::read(&path)?, b"{}");
    assert_eq!(std::fs::read_dir(&root)?.count(), 1);
    assert!(output.stdout.is_empty());
    assert!(output.stderr.len() <= 4096);
    let error = std::str::from_utf8(&output.stderr)?;
    if output.status.code() == Some(2) {
        assert!(
            error.contains("unrecognized subcommand")
                && error.contains("owned-icloud-editor-source-proof")
        );
    }
    assert_eq!(
        output.status.code(),
        Some(1),
        "offline source proof subcommand is unavailable"
    );
    assert_eq!(
        error.trim(),
        "Error: owned iCloud editor source proof refused"
    );
    assert!(!error.contains(&path.display().to_string()));
    Ok(())
}
