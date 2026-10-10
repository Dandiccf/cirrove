#![cfg(feature = "icloud-write-probe")]
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
#[test]
fn owned_icloud_calc_trash_original_cli_refuses_unregistered_without_state_io() -> anyhow::Result<()>
{
    let retained_root = tempfile::tempdir()?.keep();
    std::fs::set_permissions(&retained_root, std::fs::Permissions::from_mode(0o700))?;
    let sentinel = retained_root.join("preserve.txt");
    std::fs::write(&sentinel, b"preserve")?;
    let before = std::fs::read(&sentinel)?;
    let mut before_names = std::fs::read_dir(&retained_root)?
        .map(|e| e.map(|entry| entry.file_name()))
        .collect::<std::io::Result<Vec<_>>>()?;
    before_names.sort();
    let registration = retained_root.join("missing-registration.json");
    let output = Command::new(env!("CARGO_BIN_EXE_cirrove"))
        .arg("owned-icloud-calc-trash-original")
        .arg("--registration")
        .arg(&registration)
        .arg("--sha256")
        .arg("0".repeat(64))
        .output()?;
    // New-feature safety guard only; not a before-fix regression experiment.
    // Preserve owned fixture before checking the scoped refusal discriminator.
    assert_eq!(std::fs::read(&sentinel)?, before);
    let mut after_names = std::fs::read_dir(&retained_root)?
        .map(|e| e.map(|entry| entry.file_name()))
        .collect::<std::io::Result<Vec<_>>>()?;
    after_names.sort();
    assert_eq!(after_names, before_names);
    assert!(!registration.exists());
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("owned Calc Trash original refused; retained evidence; no retry"),
        "desired registered command refusal was not reached"
    );
    Ok(())
}
