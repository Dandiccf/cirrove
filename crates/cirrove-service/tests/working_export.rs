//! Recover retained mutable bytes without opening a provider or sealing a save.
#![allow(clippy::unwrap_used)]
use cirrove_core::{Node, NodeKind, Scope};
use cirrove_service::{accounts::Settings, journal::UploadJournal};
use sha2::Digest;
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
    assert_eq!(next, None);
    assert_eq!(rows[0].size, 7);
    assert_eq!(
        rows[0].recorded_size,
        b"unsealed working bytes".len() as u64
    );
    assert!(recovery.working_list(Some(id), 1).unwrap().0.is_empty());
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
    assert_eq!(next, None);
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

#[test]
fn working_pagination_looks_ahead_without_skipping_the_next_file() {
    use cirrove_service::journal::RecoveryJournal;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let mut journal = UploadJournal::open(&root, "owned", 1048576).unwrap();
    for index in 0..201 {
        let node = Node {
            id: format!("local-{index}"),
            parent_id: Some("root".into()),
            name: format!("draft-{index}"),
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
                    account: "owned".into(),
                    provider: "icloud".into(),
                    collection: "drive".into(),
                },
                node,
                true,
                &b""[..],
            )
            .unwrap();
        journal.write_working(working.id, 0, b"x").unwrap();
    }
    drop(journal);
    let recovery = RecoveryJournal::open(&root, "owned").unwrap();
    let (first, cursor) = recovery.working_list(None, 200).unwrap();
    assert_eq!(first.len(), 200);
    assert!(cursor.is_some());
    let (last, end) = recovery.working_list(cursor, 200).unwrap();
    assert_eq!(last.len(), 1);
    assert!(end.is_none());
    let ids: std::collections::HashSet<_> =
        first.into_iter().chain(last).map(|row| row.file).collect();
    assert_eq!(ids.len(), 201);
}

fn active_fixture(state: &Path) -> (UploadJournal, uuid::Uuid, u64) {
    let (account, id, generation) = fixture(state);
    let journal = UploadJournal::open(
        &state.join("accounts").join(&account).join("journal"),
        &account,
        4 * 1024 * 1024,
    )
    .unwrap();
    (journal, id, generation)
}
fn records(journal: &UploadJournal) -> serde_json::Value {
    serde_json::json!({
        "working": journal.working_files().unwrap(),
        "uploads": journal.list(0, 200).unwrap(),
    })
}
#[test]
fn active_working_export_copies_without_sealing_or_changing_journal() {
    let temp = tempfile::tempdir().unwrap();
    let (journal, id, generation) = active_fixture(&temp.path().join("state"));
    let before = records(&journal);
    let destination = temp.path().join("copy");
    let cancel = cirrove_core::CancellationToken::new();
    let source = journal.working_export_source(id, generation).unwrap();
    let prepared = source.prepare_copy(&destination, &cancel, |_| {}).unwrap();
    assert!(!destination.exists());
    let verified = journal.verify_working_export(prepared).unwrap();
    assert!(!destination.exists());
    let receipt = verified.publish(&cancel).unwrap();
    assert_eq!(receipt.source.file, id);
    assert_eq!(receipt.source.generation, generation);
    assert_eq!(receipt.destination, destination);
    assert_eq!(fs::read(&destination).unwrap(), b"unsealed working bytes");
    assert_eq!(
        receipt.sha256,
        hex::encode(sha2::Sha256::digest(b"unsealed working bytes"))
    );
    assert_eq!(records(&journal), before);
}
#[test]
fn active_working_export_refuses_edits_between_staging_and_verification() {
    // Staging has already finished, so its timestamp check cannot catch this edit.
    // Removing the journal generation check makes this regression fail.
    for truncate in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let (mut journal, id, generation) = active_fixture(&temp.path().join("state"));
        let destination = temp.path().join("copy");
        let cancel = cirrove_core::CancellationToken::new();
        let prepared = journal
            .working_export_source(id, generation)
            .unwrap()
            .prepare_copy(&destination, &cancel, |_| {})
            .unwrap();
        if truncate {
            journal.truncate_working(id, 0).unwrap();
        }
        // Restore identical bytes too: generation, not byte equality, owns selection.
        journal
            .write_working(id, 0, b"unsealed working bytes")
            .unwrap();
        let after_edit = records(&journal);
        assert!(journal.verify_working_export(prepared).is_err());
        assert!(!destination.exists());
        assert_eq!(records(&journal), after_edit);
        assert!(!fs::read_dir(temp.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".cirrove-export-")
        }));
    }
}
#[test]
fn active_working_export_refuses_a_source_changed_before_copying() {
    let temp = tempfile::tempdir().unwrap();
    let (mut journal, id, generation) = active_fixture(&temp.path().join("state"));
    let source = journal.working_export_source(id, generation).unwrap();
    journal
        .write_working(id, 0, b"different working data")
        .unwrap();
    let destination = temp.path().join("copy");
    let cancel = cirrove_core::CancellationToken::new();
    let prepared = source.prepare_copy(&destination, &cancel, |_| {}).unwrap();
    assert!(journal.verify_working_export(prepared).is_err());
    assert!(!destination.exists());
}
#[test]
fn active_working_export_refuses_mutation_during_copy_without_blocking_writes() {
    let temp = tempfile::tempdir().unwrap();
    let (mut journal, id, _) = active_fixture(&temp.path().join("state"));
    let bytes = vec![b'a'; 512 * 1024];
    let (_, working) = journal.write_working(id, 0, &bytes).unwrap();
    let source = journal
        .working_export_source(id, working.generation)
        .unwrap();
    let destination = temp.path().join("copy");
    let cancel = cirrove_core::CancellationToken::new();
    let mut edited = false;
    let prepared = source.prepare_copy(&destination, &cancel, |_| {
        if !edited {
            edited = true;
            journal.write_working(id, 256 * 1024, b"changed").unwrap();
        }
    });
    assert!(edited);
    // Either the source stamp or the generation validation must reject it.
    if let Ok(prepared) = prepared {
        assert!(journal.verify_working_export(prepared).is_err());
    }
    assert!(!destination.exists());
}
#[test]
fn active_working_export_publishes_selected_generation_after_a_later_edit() {
    let temp = tempfile::tempdir().unwrap();
    let (mut journal, id, generation) = active_fixture(&temp.path().join("state"));
    let destination = temp.path().join("copy");
    let cancel = cirrove_core::CancellationToken::new();
    let prepared = journal
        .working_export_source(id, generation)
        .unwrap()
        .prepare_copy(&destination, &cancel, |_| {})
        .unwrap();
    let verified = journal.verify_working_export(prepared).unwrap();
    journal.write_working(id, 0, b"newest").unwrap();
    let after_edit = records(&journal);
    let receipt = verified.publish(&cancel).unwrap();
    assert_eq!(receipt.source.generation, generation);
    assert_eq!(fs::read(destination).unwrap(), b"unsealed working bytes");
    assert_eq!(journal.read_working(id, 0, 6).unwrap(), b"newest");
    assert_eq!(records(&journal), after_edit);
}
#[test]
fn active_working_export_cancellation_after_verification_preserves_source() {
    let temp = tempfile::tempdir().unwrap();
    let (journal, id, generation) = active_fixture(&temp.path().join("state"));
    let before = records(&journal);
    let destination = temp.path().join("copy");
    let cancel = cirrove_core::CancellationToken::new();
    let prepared = journal
        .working_export_source(id, generation)
        .unwrap()
        .prepare_copy(&destination, &cancel, |_| {})
        .unwrap();
    let verified = journal.verify_working_export(prepared).unwrap();
    cancel.cancel();
    assert!(verified.publish(&cancel).is_err());
    assert!(!destination.exists());
    assert_eq!(records(&journal), before);
}
#[test]
fn active_working_export_rejects_a_different_journal_and_keeps_owner_alive() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let (journal, id, generation) = active_fixture(&state);
    let account = journal.working_file(id).unwrap().scope.account;
    let root = state.join("accounts").join(&account).join("journal");
    let source = journal.working_export_source(id, generation).unwrap();
    let cancel = cirrove_core::CancellationToken::new();
    let destination = temp.path().join("copy");
    let prepared = source.prepare_copy(&destination, &cancel, |_| {}).unwrap();
    drop(journal);
    assert!(UploadJournal::open(&root, &account, 4 * 1024 * 1024).is_err());
    let (other, _, _) = active_fixture(&temp.path().join("other"));
    assert!(other.verify_working_export(prepared).is_err());
    assert!(!destination.exists());
    assert!(UploadJournal::open(&root, &account, 4 * 1024 * 1024).is_ok());
}
#[test]
fn active_working_export_retains_unlinked_descriptor_bytes_without_a_save() {
    let temp = tempfile::tempdir().unwrap();
    let (mut journal, id, _) = active_fixture(&temp.path().join("state"));
    journal.seal_working(id).unwrap();
    let working = journal.working_file(id).unwrap();
    let object = journal
        .namespace_by_local(&working.scope, &working.node.id)
        .unwrap()
        .unwrap();
    journal
        .unlink_namespace_file(object.id, object.revision, true)
        .unwrap();
    let (_, working) = journal
        .write_working(id, 0, b"retained after unlink")
        .unwrap();
    let before = records(&journal);
    let destination = temp.path().join("copy");
    let cancel = cirrove_core::CancellationToken::new();
    let prepared = journal
        .working_export_source(id, working.generation)
        .unwrap()
        .prepare_copy(&destination, &cancel, |_| {})
        .unwrap();
    let receipt = journal
        .verify_working_export(prepared)
        .unwrap()
        .publish(&cancel)
        .unwrap();
    assert!(receipt.source.unlinked);
    assert_eq!(
        fs::read(destination).unwrap(),
        journal.read_working(id, 0, 100).unwrap()
    );
    assert_eq!(records(&journal), before);
}
#[test]
fn active_working_export_preserves_no_overwrite_and_parent_identity_after_validation() {
    use std::os::unix::fs::symlink;
    for renamed in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let (journal, id, generation) = active_fixture(&temp.path().join("state"));
        let output = temp.path().join("output");
        fs::create_dir(&output).unwrap();
        let destination = output.join("copy");
        let cancel = cirrove_core::CancellationToken::new();
        let prepared = journal
            .working_export_source(id, generation)
            .unwrap()
            .prepare_copy(&destination, &cancel, |_| {})
            .unwrap();
        let verified = journal.verify_working_export(prepared).unwrap();
        if renamed {
            let moved = temp.path().join("moved");
            fs::rename(&output, &moved).unwrap();
            let other = temp.path().join("other");
            fs::create_dir(&other).unwrap();
            symlink(&other, &output).unwrap();
            assert!(verified.publish(&cancel).is_err());
            assert!(!moved.join("copy").exists());
            assert!(!other.join("copy").exists());
        } else {
            fs::write(&destination, b"keep me").unwrap();
            assert!(verified.publish(&cancel).is_err());
            assert_eq!(fs::read(destination).unwrap(), b"keep me");
        }
    }
}

#[test]
fn active_working_list_pages_past_clean_rows_without_sealing() {
    let temp = tempfile::tempdir().unwrap();
    let (mut journal, id, _) = active_fixture(&temp.path().join("state"));
    journal.seal_working(id).unwrap();
    let before = records(&journal);
    let (files, next) = journal.working_recovery_list(None, 1).unwrap();
    assert!(files.is_empty());
    assert!(next.is_none());
    let original = journal.working_file(id).unwrap();
    for index in 0..3 {
        let mut node = original.node.clone();
        node.name = format!("other-{index}.txt");
        node.size = 0;
        journal
            .create_working(original.scope.clone(), node, true, &b""[..])
            .unwrap();
    }
    let mut after = None;
    let mut found = Vec::new();
    let before_listing = records(&journal);
    for _ in 0..5 {
        let (files, next) = journal.working_recovery_list(after, 1).unwrap();
        assert!(files.len() <= 1);
        found.extend(files.into_iter().map(|file| file.file));
        if next.is_none() {
            break;
        }
        assert_ne!(after, next);
        after = next;
    }
    assert_eq!(found.len(), 3);
    assert!(!found.contains(&id));
    assert_eq!(records(&journal), before_listing);
    assert_eq!(before["uploads"], before_listing["uploads"]);
}
#[test]
fn active_working_cli_requires_explicit_mode_and_rejects_offline_state() {
    for args in [
        vec!["recovery-working", "--label", "work", "--socket", "/absent"],
        vec![
            "recovery-working",
            "--label",
            "work",
            "--active",
            "--state",
            "/absent",
        ],
        vec![
            "export-working",
            "--label",
            "work",
            "--file",
            "00000000-0000-0000-0000-000000000001",
            "--generation",
            "1",
            "--destination",
            "/copy",
            "--socket",
            "/absent",
        ],
        vec![
            "export-working",
            "--label",
            "work",
            "--file",
            "00000000-0000-0000-0000-000000000001",
            "--generation",
            "1",
            "--destination",
            "/copy",
            "--active",
            "--state",
            "/absent",
        ],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_cirrove"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(2),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
#[tokio::test]
async fn working_job_receipt_requires_exact_selection_and_preserves_sealed_compatibility() {
    use cirrove_service::{
        ExportWorkingRequest,
        jobs::{JobKind, JobState, Jobs},
    };
    use std::sync::Arc;
    let request = ExportWorkingRequest {
        label: "work".into(),
        file: uuid::Uuid::new_v4(),
        generation: 8,
        destination: "/local/copy".into(),
    };
    let jobs = Arc::new(Jobs::default());
    let handle = jobs.start(
        JobKind::ExportLocal,
        "copy".into(),
        1,
        3,
        &cirrove_core::CancellationToken::new(),
    );
    let initial = jobs.find(handle.id()).unwrap();
    let receipt = cirrove_service::journal::WorkingExportReceipt {
        source: cirrove_service::journal::WorkingRecovery {
            file: request.file,
            generation: 8,
            name: "dirty".into(),
            size: 3,
            recorded_size: 3,
            unlinked: false,
        },
        sha256: "a".repeat(64),
        destination: request.destination.clone(),
    };
    handle.exported_working(receipt.clone());
    let completed = jobs.wait(&initial.id).await.unwrap();
    assert_eq!(
        request.confirmed_receipt(&initial, &completed),
        Some(&receipt)
    );
    assert!(completed.export.is_none());
    assert_eq!((completed.files_done, completed.bytes_done), (1, 3));
    for variant in 0..8 {
        let mut wrong = completed.clone();
        match variant {
            0 => wrong.id = "another-job".into(),
            1 => wrong.state = JobState::Failed,
            2 => wrong.working_export = None,
            3 => wrong.working_export.as_mut().unwrap().source.generation += 1,
            4 => wrong.working_export.as_mut().unwrap().source.file = uuid::Uuid::new_v4(),
            5 => wrong.working_export.as_mut().unwrap().source.size += 1,
            6 => wrong.working_export.as_mut().unwrap().destination = "/different".into(),
            _ => wrong.working_export.as_mut().unwrap().sha256 = "not-a-digest".into(),
        }
        assert!(request.confirmed_receipt(&initial, &wrong).is_none());
    }
    let mut legacy = serde_json::to_value(&initial).unwrap();
    legacy.as_object_mut().unwrap().remove("working_export");
    assert!(
        serde_json::from_value::<cirrove_service::jobs::Job>(legacy)
            .unwrap()
            .working_export
            .is_none()
    );
}
