#![cfg(feature = "icloud-write-probe")]
//! Actual Clap dispatch refusal, safe before any saved state or provider access.
use anyhow::Result;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
#[test]
fn editor_metadata_cli_refuses_invalid_registration_before_state_access() -> Result<()> {
    let root = tempfile::tempdir()?.keep();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    let registration = root.join("invalid-registration.json");
    std::fs::write(&registration, b"{}")?;
    std::fs::set_permissions(&registration, std::fs::Permissions::from_mode(0o600))?;
    let before = std::fs::read(&registration)?;
    let unused = root.join("never-created-state");
    let output = Command::new(env!("CARGO_BIN_EXE_cirrove"))
        .args(["owned-icloud-editor-metadata", "--registration"])
        .arg(&registration)
        .arg("--sha256")
        .arg("a".repeat(64))
        .env("XDG_STATE_HOME", &unused)
        .output()?;
    // These run before the desired endpoint assertion on original source too.
    assert!(!unused.exists());
    assert_eq!(std::fs::read(&registration)?, before);
    assert_eq!(std::fs::read_dir(&root)?.count(), 1);
    assert!(output.stdout.is_empty());
    assert!(output.stderr.len() <= 4096);
    let error = std::str::from_utf8(&output.stderr)?;
    if output.status.code() == Some(2) {
        assert!(
            error.contains("unrecognized subcommand")
                && error.contains("owned-icloud-editor-metadata"),
            "not the original missing-subcommand endpoint"
        );
    }
    assert_eq!(
        output.status.code(),
        Some(1),
        "read-only metadata subcommand is unavailable"
    );
    assert_eq!(error.trim(), "Error: owned iCloud editor metadata refused");
    assert!(!error.contains(&registration.display().to_string()));
    Ok(())
}
