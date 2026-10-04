//! Exact executable report; no daemon, provider, installed state or HOME changes.
use anyhow::Result;
use std::process::Command;

#[test]
fn daemon_storage_format_reports_compiled_constants_without_state_initialization() -> Result<()> {
    let root = tempfile::tempdir()?.keep();
    let unused = root.join("never-created-state");
    let output = Command::new(env!("CARGO_BIN_EXE_cirroved"))
        .arg("--storage-format-json")
        .env("XDG_STATE_HOME", &unused)
        .output()?;
    // Preservation precedes the success assertion, including on the original
    // executable where this report flag has not yet been implemented.
    assert!(!unused.exists());
    assert_eq!(std::fs::read_dir(&root)?.count(), 0);
    assert!(output.stdout.len() <= 4096);
    if !output.status.success() {
        assert_eq!(
            output.status.code(),
            Some(2),
            "not the original unknown-flag endpoint"
        );
        let error = std::str::from_utf8(&output.stderr)?;
        assert!(error.contains("unexpected argument") && error.contains("--storage-format-json"));
    }
    assert!(
        output.status.success(),
        "compiled-format report is unavailable"
    );
    assert!(output.stderr.is_empty());
    let reply: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    // The registered current journal writer format is19; this baseline test
    // intentionally calls no new API so it compiles against original source.
    assert_eq!(
        reply,
        serde_json::json!({
            "version": 1, "product": "cirroved", "journal_schema": 19,
            "metadata_schema": cirrove_store::SCHEMA_VERSION,
        })
    );
    Ok(())
}

#[test]
fn daemon_storage_format_rejects_state_and_socket_routes_before_opening_them() -> Result<()> {
    let root = tempfile::tempdir()?.keep();
    for option in ["--state-dir", "--socket"] {
        let path = root.join("must-not-open");
        let output = Command::new(env!("CARGO_BIN_EXE_cirroved"))
            .args(["--storage-format-json", option])
            .arg(&path)
            .output()?;
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!path.exists());
        assert_eq!(std::fs::read_dir(&root)?.count(), 0);
    }
    Ok(())
}
