//! Actual flat-v2 preparation, TLS handoff, sealed checkpoint and journal restart.
//! The acknowledgement is deliberately withheld locally after the genuine receipt.
use super::*;
use crate::validation::native_final_recovery::Receipt;
use cirrove_core::upload::{PackageHandoffReceipt, UploadRepresentation, UploadStep};

fn receipt(value: &PackageHandoffReceipt) -> Receipt {
    Receipt {
        original: value.original.clone(),
        current: value.current.remote.clone(),
        backup: value.backup.remote.clone(),
        current_semantic: value.current.semantic.clone(),
        backup_semantic: value.backup.semantic.clone(),
    }
}

#[tokio::test]
async fn native_final_flat_v2_sealed_receipt_loss_reopens_and_inspects_without_mutation() {
    tokio::time::timeout(Duration::from_secs(40), async {
        let fixture = Fixture::new_layout(true, true).await;
        let Fixture {
            state,
            directory,
            _staging,
            engine,
            context,
            account,
            row,
            server,
            keys,
            completed,
            folder: _,
        } = fixture;
        // Retain every new fixture even on the desired baseline assertion.
        let state = state.keep();
        let directory = directory.keep();
        let staging = _staging.keep();
        let raw = std::fs::read(staging.join("source.zip")).unwrap();
        let source_before = std::fs::read(staging.join("source-a.zip")).unwrap();
        assert!(matches!(
            completed[0].representation,
            UploadRepresentation::FlatNumbersArchive { .. }
        ));
        let req = request(&row);
        let UploadRepresentation::FlatNumbersReplacementArchive { semantic, .. } =
            &req.representation
        else {
            panic!("explicit flat fixture required")
        };
        let router = crate::icloud_writes::ICloudWriteProvider::new(&account, &context)
            .unwrap()
            .synthetic_native_transport(server.client.clone());
        let vault =
            SealedUploadCheckpointVault::with_test_key_vault(&state, &account.id, keys.clone())
                .unwrap();
        let journal = context.journal();
        let claimed = journal.lock().unwrap().claim_next().unwrap().unwrap();
        assert_eq!(claimed.id, row.id);
        let attempt = claimed.attempt.unwrap();
        let op = row.id.to_string();
        let cancel = CancellationToken::new();
        let location = router.staged_recovery_location(&op, &req).unwrap();
        journal
            .lock()
            .unwrap()
            .reserve_identity_handoff(row.id, attempt, location)
            .unwrap();
        let payload = journal.lock().unwrap().payload(row.id).unwrap();
        let mut step = router
            .begin_upload_from_payload_for_operation(&op, &req, payload, &cancel)
            .await
            .unwrap();
        let UploadStep::Allocate(initial) = &step else {
            panic!("fresh flat allocation checkpoint required")
        };
        let initial: serde_json::Value = serde_json::from_str(initial.expose_secret()).unwrap();
        let inner = &initial["phase"]["Stage"]["inner"];
        assert_eq!(inner["version"], 2);
        assert_eq!(inner["wire"]["version"], 1);
        assert_eq!(
            inner["wire"]["expected_root"],
            format!("staged-by-cirrove-{}.numbers", row.id)
        );
        let wire_size = inner["wire"]["size"].as_u64().unwrap();
        let wire_sha = inner["wire"]["sha256"].as_str().unwrap().to_owned();
        assert!(wire_size > 0 && wire_size <= 64 * 1024 * 1024);
        assert_ne!(wire_sha, req.sha256);
        server.state.lock().unwrap().flat_wire = Some((wire_size, wire_sha, semantic.clone()));
        let key = format!("upload/{}", row.id);
        let mut final_checkpoint = None;
        let mut final_receipt = None;
        for _ in 0..10 {
            step = match step {
                UploadStep::Allocate(saved) => {
                    vault.save(&key, saved.clone()).await.unwrap();
                    journal
                        .lock()
                        .unwrap()
                        .record_session(row.id, attempt, row.id, 0)
                        .unwrap();
                    router
                        .allocate_upload_for_operation(&op, &req, &saved, &cancel)
                        .await
                        .unwrap()
                }
                UploadStep::Stream(saved) => {
                    vault.save(&key, saved.clone()).await.unwrap();
                    journal
                        .lock()
                        .unwrap()
                        .record_session(row.id, attempt, row.id, 0)
                        .unwrap();
                    let payload = journal.lock().unwrap().payload(row.id).unwrap();
                    router
                        .upload_stream_for_operation(&op, &req, &saved, payload, &cancel)
                        .await
                        .unwrap()
                }
                UploadStep::Commit(saved) => {
                    vault.save(&key, saved.clone()).await.unwrap();
                    journal
                        .lock()
                        .unwrap()
                        .record_session(row.id, attempt, row.id, req.size)
                        .unwrap();
                    final_checkpoint = Some(saved.clone());
                    router
                        .commit_upload_for_operation(&op, &req, &saved, &cancel)
                        .await
                        .unwrap()
                }
                UploadStep::PackageHandoffComplete(value) => {
                    final_receipt = Some(receipt(&value));
                    break;
                }
                _ => panic!("unexpected flat replacement continuation"),
            };
        }
        let saved = final_checkpoint.unwrap();
        let actual = final_receipt.unwrap();
        let adapter = router
            .native_package_adapter(&op, &req, Some(&saved))
            .await
            .unwrap();
        assert_eq!(
            adapter
                .native_checkpoint_diagnostic(&op, &req, &saved)
                .unwrap()["phase"],
            "handoff-install-armed"
        );
        assert_eq!(actual.current.id, NEW);
        assert_eq!(actual.backup.id, OLD);
        assert_eq!(actual.backup.parent_id.as_deref(), Some(TRASH_ROOT));
        assert_eq!(actual.current_semantic, *semantic);
        let checkpoint_sha = hex::encode(Sha256::digest(saved.expose_secret().as_bytes()));
        let sealed_path = state
            .join("accounts")
            .join(&account.id)
            .join("upload-checkpoints")
            .join(&op)
            .join("checkpoint.sealed");
        let sealed_before = std::fs::read(&sealed_path).unwrap();
        journal
            .lock()
            .unwrap()
            .stop_attempt(row.id, attempt, UploadState::VerifyRequired)
            .unwrap();
        let retained = journal.lock().unwrap().get(row.id).unwrap();
        assert!(
            retained.remote.is_none()
                && retained.package_completion.is_none()
                && retained.native_replacement_receipt().is_none()
        );
        assert_eq!(retained.session_key, Some(row.id));
        assert_eq!(retained.transferred_bytes, req.size);
        assert_eq!(
            std::fs::read(journal.lock().unwrap().objects.join(&op)).unwrap(),
            raw
        );
        drop(adapter);
        drop(router);
        drop(journal);
        drop(context);
        drop(vault);
        let context = WriteContext::open(&engine, &state).await.unwrap();
        let vault = Arc::new(
            SealedUploadCheckpointVault::with_test_key_vault(&state, &account.id, keys).unwrap(),
        );
        let reopened = vault.load(&key).await.unwrap().unwrap();
        assert_eq!(
            hex::encode(Sha256::digest(reopened.expose_secret().as_bytes())),
            checkpoint_sha
        );
        assert_eq!(std::fs::read(&sealed_path).unwrap(), sealed_before);
        assert_eq!(
            std::fs::read(staging.join("source-a.zip")).unwrap(),
            source_before
        );
        assert_eq!(std::fs::read(staging.join("source.zip")).unwrap(), raw);
        assert_eq!(
            context.journal().lock().unwrap().get(row.id).unwrap().state,
            UploadState::VerifyRequired
        );
        let before = counts(&server);
        assert_eq!(before, (1, 1, 1, 1, 1));
        let guard = Guard::new(
            crate::icloud_writes::ICloudWriteProvider::new(&account, &context)
                .unwrap()
                .synthetic_native_transport(server.client.clone()),
            req,
            context.journal(),
            completed,
            directory,
            Uuid::new_v4(),
            Mode::Recover {
                operation: row.id,
                checkpoint_sha256: checkpoint_sha,
                receipt: Box::new(actual.clone()),
            },
        );
        // Baseline reaches this only after actual final receipt, persisted AEAD
        // checkpoint and successful exclusive journal reopen with preserved bytes.
        assert!(
            guard.is_ok(),
            "flat-v2 terminal inspection must be admitted after durable restart"
        );
        let guard = Arc::new(guard.unwrap());
        let worker = TransferWorker::new(
            context.journal(),
            guard.clone(),
            vault.clone(),
            CancellationToken::new(),
        );
        let result = worker.run_once().await.unwrap().unwrap();
        assert_eq!(result.state, UploadState::Uploaded);
        assert_eq!(counts(&server), before);
        assert_eq!(guard.counts.inspections.load(Ordering::SeqCst), 1);
        assert_eq!(guard.counts.refused_uploads.load(Ordering::SeqCst), 0);
        let uploaded = context.journal().lock().unwrap().get(row.id).unwrap();
        let (original, current, backup) = uploaded.native_replacement_receipt().unwrap();
        assert_eq!(original, &actual.original);
        assert_eq!(current, &actual.current);
        assert_eq!(backup, &actual.backup);
        assert!(vault.load(&key).await.unwrap().is_none());
        assert_eq!(
            std::fs::read(staging.join("source-a.zip")).unwrap(),
            source_before
        );
        assert_eq!(std::fs::read(staging.join("source.zip")).unwrap(), raw);
    })
    .await
    .unwrap();
}
