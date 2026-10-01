//! Real socket + Manager + journal; provider evidence is injected, never a desktop vault.
use super::*;
use crate::{
    jobs::JobState,
    native_abandon::{NativeAbandonReply, NativeAbandonRequest},
};
use cirrove_core::upload::{PackageSemanticIdentity, RecoveryLocation};
use cirrove_icloud::{NativeReplacementAbandonEvidence, NativeReplacementAbandonRecord};
fn conflict(f: &Fixture) -> crate::journal::UploadRecord {
    let archive = ValidatedPackageArchive::capture(
        &f.source,
        f._temp.path(),
        "Source.pages",
        &CancellationToken::new(),
    )
    .unwrap();
    let mut original = node(
        "FILE::com.apple.CloudDocs::owned",
        Some(ROOT),
        "Owned.pages",
    );
    original.kind = NodeKind::Folder;
    original.package = true;
    original.size = 123;
    original.etag = Some("v1".into());
    original.content_version = None;
    let mut j = f.journal.lock().unwrap();
    let row = j
        .enqueue_validated_package_replacement(
            f.engine.scope("drive"),
            original,
            PackageSemanticIdentity {
                version: 1,
                sha256: "a".repeat(64),
                entries: 1,
                files: 1,
                expanded_bytes: 123,
            },
            archive,
            &CancellationToken::new(),
        )
        .unwrap();
    let attempt = j.claim_next().unwrap().unwrap().attempt.unwrap();
    j.reserve_identity_handoff(
        row.id,
        attempt,
        RecoveryLocation::Trash {
            local_name: format!("recovery-by-cirrove-{}.pages", row.id),
            parent: "FOLDER::com.apple.CloudDocs::TRASH_ROOT".into(),
        },
    )
    .unwrap();
    j.record_session(row.id, attempt, row.id, row.size).unwrap();
    j.stop_attempt(row.id, attempt, crate::journal::UploadState::Conflict)
        .unwrap();
    j.get(row.id).unwrap()
}
fn evidence(
    request: cirrove_core::upload::UploadRequest,
    operation: uuid::Uuid,
) -> NativeReplacementAbandonEvidence {
    let UploadRepresentation::PackageReplacementArchive { original, .. } = &request.representation
    else {
        panic!("native fixture")
    };
    let original = original.as_ref().clone();
    let mut staged = original.clone();
    staged.id = "FILE::com.apple.CloudDocs::stage".into();
    staged.name = format!("staged-by-cirrove-{operation}.pages");
    staged.etag = Some("stage-v1".into());
    NativeReplacementAbandonEvidence::synthetic_for_test(NativeReplacementAbandonRecord {
        version: 1,
        operation,
        request,
        original,
        staged,
        checkpoint_sha256: "a".repeat(64),
        observed_unix: 1,
    })
    .unwrap()
}
async fn terminal(f: &Fixture, id: &str) -> crate::jobs::Job {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(job) = f.engine.jobs.find(id)
                && !job.running()
            {
                return job;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("abandonment job must finish within five seconds")
}
#[tokio::test]
async fn native_abandon_public_route_retains_stage_and_recovers_receipt_without_reinspection() {
    let f = Fixture::with_journal("journal").await;
    let row = conflict(&f);
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    *f.manager.native_abandon_inspector.lock().unwrap() =
        Some(Arc::new(move |request, operation, _| {
            count.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move { Ok(evidence(request, operation)) })
        }));
    let runtime = tempfile::Builder::new()
        .prefix("abandon-socket-")
        .tempdir_in("/var/tmp")
        .unwrap();
    std::fs::set_permissions(runtime.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = runtime.path().join("socket");
    let cancel = CancellationToken::new();
    let server = tokio::spawn(crate::serve_managed(
        runtime.path().join("socket.sqlite"),
        socket.clone(),
        cancel.clone(),
        Some(f.manager.clone()),
    ));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !socket.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let input = NativeAbandonRequest {
        label: f.engine.account.label.clone(),
        expected_account_id: f.engine.account.id.clone(),
        operation: row.id,
    };
    let mut wrong = input.clone();
    wrong.expected_account_id = uuid::Uuid::new_v4().to_string();
    assert!(
        crate::native_abandon::abandon_native_stage(&socket, &wrong)
            .await
            .unwrap()
            .refusal
            .is_some()
    );
    let mut forged = serde_json::to_value(&input).unwrap();
    forged["checkpoint"] = serde_json::json!("untrusted");
    let reply: NativeAbandonReply = crate::request(
        &socket,
        "abandon-native-stage",
        Some(forged),
        "synthetic boundary",
    )
    .await
    .unwrap();
    assert!(reply.refusal.is_some());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let started = crate::native_abandon::abandon_native_stage(&socket, &input)
        .await
        .unwrap();
    let job = terminal(&f, &started.job.unwrap().id).await;
    assert_eq!(job.state, JobState::Succeeded);
    let receipt = job.native_abandon.unwrap().receipt.unwrap();
    assert_eq!(receipt.operation, row.id);
    assert_ne!(receipt.original.id, receipt.retained_stage.id);
    assert_eq!(
        receipt.retained_stage.name,
        format!("staged-by-cirrove-{}.pages", row.id)
    );
    {
        let j = f.journal.lock().unwrap();
        let after = j.get(row.id).unwrap();
        assert_eq!(after.state, crate::journal::UploadState::Resolved);
        assert_eq!(after.session_key, row.session_key);
        assert!(after.remote.is_none());
        assert!(j.local_export_source(row.id).is_ok());
    }
    // Drop every writable owner, then reopen the same journal through the RO
    // recovery path. No jobs, writer, injected inspector or migration survives.
    f.engine.stop().await;
    cancel.cancel();
    server.await.unwrap().unwrap();
    let mut account = f.engine.account.clone();
    let Fixture {
        _temp,
        manager,
        engine,
        control,
        journal,
        provider,
        ..
    } = f;
    manager.writers.write().await.clear();
    manager.engines.write().await.clear();
    drop(control);
    drop(journal);
    drop(manager);
    drop(engine);
    account.access = cirrove_auth::AccessMode::ReadOnly;
    let engine = Engine::new(
        account.clone(),
        provider.clone(),
        _temp.path().join("state"),
    )
    .await
    .unwrap();
    let journal_path = engine.db.parent().unwrap().join("journal/uploads.db");
    let journal_before = std::fs::read(&journal_path).unwrap();
    let restarted = Arc::new(Manager::default());
    restarted
        .engines
        .write()
        .await
        .insert(account.id.clone(), engine.clone());
    restarted.status.write().await.push(AccountStatus {
        account_id: account.id.clone(),
        label: account.label.clone(),
        mount_path: account.mount_path.clone(),
        enabled: true,
        mounted: true,
        ..Default::default()
    });
    assert!(engine.jobs.list().is_empty());
    assert!(restarted.writers.read().await.is_empty());
    let cancel = CancellationToken::new();
    let server = tokio::spawn(crate::serve_managed(
        runtime.path().join("socket.sqlite"),
        socket.clone(),
        cancel.clone(),
        Some(restarted),
    ));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !socket.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let found = crate::native_abandon::native_stage_abandonment(&socket, &input)
        .await
        .unwrap();
    assert_eq!(found.receipt, Some(receipt));
    assert!(found.job.is_none());
    assert!(
        crate::native_abandon::native_stage_abandonment(&socket, &wrong)
            .await
            .unwrap()
            .refusal
            .is_some()
    );
    let again = crate::native_abandon::abandon_native_stage(&socket, &input)
        .await
        .unwrap();
    assert!(again.refusal.is_some() && again.job.is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(provider.reads.load(Ordering::SeqCst), 0);
    assert_eq!(std::fs::read(&journal_path).unwrap(), journal_before);
    cancel.cancel();
    server.await.unwrap().unwrap();
    engine.stop().await;
}
#[tokio::test]
async fn native_abandon_paused_evidence_rechecks_mount_cancel_and_journal() {
    for arm in ["mount", "mount-replaced", "cancel", "journal"] {
        let f = Fixture::new().await;
        let row = conflict(&f);
        let ready = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let (r, s) = (ready.clone(), release.clone());
        *f.manager.native_abandon_inspector.lock().unwrap() =
            Some(Arc::new(move |request, operation, _| {
                let (r, s) = (r.clone(), s.clone());
                Box::pin(async move {
                    r.notify_one();
                    s.notified().await;
                    Ok(evidence(request, operation))
                })
            }));
        let input = NativeAbandonRequest {
            label: f.engine.account.label.clone(),
            expected_account_id: f.engine.account.id.clone(),
            operation: row.id,
        };
        let job = f
            .manager
            .start_native_abandon(f.engine.clone(), input)
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), ready.notified())
            .await
            .unwrap();
        match arm {
            "mount" => {
                f.manager.writers.write().await.remove(&f.engine.account.id);
            }
            "mount-replaced" => {
                let replacement =
                    CloudFs::new_experimental_writable(f.engine.clone(), f.journal.clone())
                        .await
                        .unwrap()
                        .write_control()
                        .unwrap();
                assert!(replacement.belongs_to(&f.engine));
                assert!(!replacement.same_mount(&f.control));
                f.manager
                    .writers
                    .write()
                    .await
                    .insert(f.engine.account.id.clone(), replacement);
            }
            "cancel" => {
                assert_eq!(f.engine.jobs.stop(&job.id), crate::jobs::Stopped::Asked);
            }
            _ => {
                let _held = f.journal.lock().unwrap();
                let db = rusqlite::Connection::open(
                    f.engine.db.parent().unwrap().join("uploads/uploads.db"),
                )
                .unwrap();
                db.execute(
                    "UPDATE uploads SET body=json_set(body, '$.failed_attempts', 42) WHERE id=?1",
                    [row.id.to_string()],
                )
                .unwrap();
            }
        }
        release.notify_one();
        let done = terminal(&f, &job.id).await;
        assert_ne!(done.state, JobState::Succeeded, "{arm}");
        let j = f.journal.lock().unwrap();
        assert!(j.native_stage_abandonment(row.id).unwrap().is_none());
        assert_ne!(
            j.get(row.id).unwrap().state,
            crate::journal::UploadState::Resolved
        );
        assert!(j.local_export_source(row.id).is_ok());
    }
}
