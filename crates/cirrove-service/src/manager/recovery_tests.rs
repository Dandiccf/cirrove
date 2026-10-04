//! Recovery is available on an enabled read-only account without opening writers.
use super::*;
use crate::journal::{JournalError, UploadJournal};
use cirrove_core::{Node, NodeKind};
use sha2::Digest;
fn account(mount_path: PathBuf, budget: u64) -> Account {
    Account {
        id: uuid::Uuid::new_v4().to_string(),
        label: "write-budget-fixture".into(),
        registration: cirrove_auth::AppRegistration::Microsoft {
            client_id: uuid::Uuid::new_v4().to_string(),
            authority: "common".into(),
        },
        identity: cirrove_auth::Identity {
            tenant_id: String::new(),
            subject: "synthetic".into(),
            username: "fixture@example.invalid".into(),
            graph_user_id: "fixture".into(),
            display_name: "Fixture".into(),
        },
        credential_id: uuid::Uuid::new_v4().to_string(),
        access: cirrove_auth::AccessMode::ReadWrite,
        drive: cirrove_core::CollectionInfo {
            id: "fixture".into(),
            name: "Fixture".into(),
            drive_type: "business".into(),
            web_url: "https://example.invalid".into(),
        },
        root_id: "root".into(),
        mount_path,
        enabled: false,
        poll_seconds: 3600,
        cache_bytes: budget,
    }
}

async fn fixture() -> Result<(tempfile::TempDir, Manager, Arc<Engine>)> {
    let temp = tempfile::tempdir()?;
    let mut account = account(temp.path().join("mount"), 64 * 1024 * 1024);
    account.access = cirrove_auth::AccessMode::ReadOnly;
    account.enabled = true;
    let read = Arc::new(cirrove_onedrive::OneDrive::synthetic_loopback(
        account.id.clone(),
        "http://127.0.0.1:9/",
    )?);
    let engine = Engine::new(account.clone(), read, temp.path().to_owned()).await?;
    let manager = Manager::default();
    manager.status.write().await.push(AccountStatus {
        account_id: account.id.clone(),
        label: account.label.clone(),
        enabled: true,
        mount_path: account.mount_path.clone(),
        ..Default::default()
    });
    manager
        .engines
        .write()
        .await
        .insert(account.id.clone(), engine.clone());
    Ok((temp, manager, engine))
}
fn node() -> Node {
    Node {
        package: false,
        id: "retained-remote-id".into(),
        parent_id: Some("root".into()),
        name: "private.txt".into(),
        kind: NodeKind::File,
        size: 6,
        modified_unix: 1,
        etag: Some("old".into()),
        content_version: Some("old".into()),
        target: None,
    }
}
async fn completed(engine: &Engine, initial: &crate::jobs::Job) -> crate::jobs::Job {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let job = engine.jobs.find(&initial.id).expect("registered job");
            if !job.running() && engine.recovery_journal.lock().await.strong_count() == 0 {
                // Weak expiry precedes payload destruction; wait for the actual
                // close boundary before assertions that open the raw journal.
                let gate = engine.recovery_journal_gate.clone();
                tokio::task::spawn_blocking(move || {
                    drop(gate.lock().unwrap_or_else(|e| e.into_inner()));
                })
                .await
                .expect("recovery close barrier");
                return job;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("bounded local copy")
}
#[tokio::test]
async fn read_only_missing_journal_is_empty_without_creating_it() -> Result<()> {
    let (_temp, manager, engine) = fixture().await?;
    let label = &engine.account.label;
    assert!(manager.recent(label, usize::MAX).await?.local.is_empty());
    assert!(
        manager
            .recovery_working(&crate::RecoveryWorkingRequest {
                label: label.clone(),
                after: None,
                limit: 200
            })
            .await?
            .0
            .is_empty()
    );
    assert!(
        !engine
            .db
            .parent()
            .expect("fixture database has an account directory")
            .join("journal")
            .exists()
    );
    assert!(manager.writers.read().await.is_empty());
    assert!(manager.retry_stuck(label).await.is_err());
    assert!(manager.discard_stuck(label).await.is_err());
    Ok(())
}
#[tokio::test]
async fn read_only_exports_exact_saved_and_dirty_versions_without_changing_journal() -> Result<()> {
    let (temp, manager, engine) = fixture().await?;
    let root = engine
        .db
        .parent()
        .expect("fixture database has an account directory")
        .join("journal");
    let mut journal = UploadJournal::open(&root, &engine.account.id, 1024 * 1024)?;
    for name in ["older-a", "older-b"] {
        journal.enqueue(
            engine.scope("fixture"),
            cirrove_core::upload::UploadIntent::Create {
                parent: "root".into(),
                name: name.into(),
            },
            &b"old"[..],
        )?;
    }
    let file = journal.create_working(engine.scope("fixture"), node(), false, &b"sealed"[..])?;
    journal.write_working(file.id, 0, b"sealed")?;
    let saved = journal.seal_working(file.id)?.expect("saved");
    let (_, dirty) = journal.write_working(file.id, 0, b"latest")?;
    let before_records = serde_json::to_value(journal.list(0, 200)?)?;
    drop(journal);
    let before_db = std::fs::read(root.join("uploads.db"))?;
    let label = engine.account.label.clone();
    let recent = manager
        .recent(&label, 1)
        .await
        .context("recovery stage: recent")?;
    assert_eq!(recent.local[0].operation, Some(saved.id));
    assert_eq!(recent.local[0].name, "private.txt"); // persisted working name, no provider lookup
    let listed = manager
        .recovery_working(&crate::RecoveryWorkingRequest {
            label: label.clone(),
            after: None,
            limit: 1,
        })
        .await
        .context("recovery stage: working listing")?;
    assert_eq!(listed.0.len(), 1);
    let export = crate::ExportSaveRequest {
        label: label.clone(),
        operation: saved.id,
        destination: temp.path().join("saved-copy"),
    };
    let initial = manager
        .export_save(&export)
        .await
        .context("recovery stage: saved export")?;
    let job = completed(&engine, &initial).await;
    let receipt = job.export.expect("saved receipt");
    assert_eq!(receipt.operation, saved.id);
    assert_eq!(receipt.sha256, saved.sha256);
    assert_eq!(std::fs::read(&export.destination)?, b"sealed");
    let request = crate::ExportWorkingRequest {
        label: label.clone(),
        file: dirty.id,
        generation: dirty.generation,
        destination: temp.path().join("working-copy"),
    };
    let initial = manager
        .export_working(&request)
        .await
        .context("recovery stage: working export")?;
    let job = completed(&engine, &initial).await;
    let receipt = request
        .confirmed_receipt(&initial, &job)
        .expect("exact working receipt");
    assert_eq!(receipt.source.generation, dirty.generation);
    assert_eq!(receipt.source.size, 6);
    assert_eq!(receipt.sha256, hex::encode(sha2::Sha256::digest(b"latest")));
    assert_eq!(std::fs::read(&request.destination)?, b"latest");
    assert_eq!(std::fs::read(root.join("uploads.db"))?, before_db);
    assert!(manager.writers.read().await.is_empty());
    let recovery = crate::journal::RecoveryJournal::open(&root, &engine.account.id)
        .context("recovery stage: final raw journal reopen")?;
    assert_eq!(
        serde_json::to_value(recovery.list(0, 200)?)?,
        before_records
    );
    Ok(())
}
#[tokio::test]
async fn read_only_control_retains_both_owners_and_rejects_stopped_engine() -> Result<()> {
    let (_temp, manager, engine) = fixture().await?;
    let root = engine
        .db
        .parent()
        .expect("fixture database has an account directory")
        .join("journal");
    drop(UploadJournal::open(&root, &engine.account.id, 1024)?);
    let a = manager.recovery_control(engine.clone()).await?;
    let b = manager.recovery_control(engine.clone()).await?;
    manager.engines.write().await.clear();
    let account_dir = engine
        .db
        .parent()
        .expect("fixture database has an account directory")
        .to_owned();
    let account_id = engine.account.id.clone();
    engine.cancel.cancel();
    assert!(manager.recovery_control(engine.clone()).await.is_err());
    drop(engine);
    assert!(crate::accounts::account_lock(&account_dir).is_err());
    assert!(matches!(
        UploadJournal::open(&root, &account_id, 1024),
        Err(JournalError::Busy)
    ));
    drop(a);
    assert!(crate::accounts::account_lock(&account_dir).is_err());
    drop(b);
    assert!(crate::accounts::account_lock(&account_dir).is_ok());
    assert!(UploadJournal::open(&root, &account_id, 1024).is_ok());
    Ok(())
}

#[tokio::test]
async fn detached_blocking_recovery_holds_owners_until_it_actually_returns() -> Result<()> {
    let (_temp, manager, engine) = fixture().await?;
    let root = engine
        .db
        .parent()
        .expect("fixture database has an account directory")
        .join("journal");
    drop(UploadJournal::open(&root, &engine.account.id, 1024)?);
    let control = manager.recovery_control(engine.clone()).await?;
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let worker = tokio::task::spawn_blocking(move || {
        let _control = control;
        let _ = started_tx.send(());
        release_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("release blocked local operation");
    });
    started_rx.await?;
    worker.abort(); // A running blocking operation cannot be aborted.
    manager.engines.write().await.clear();
    let account_dir = engine
        .db
        .parent()
        .expect("fixture database has an account directory")
        .to_owned();
    let account_id = engine.account.id.clone();
    engine.cancel.cancel();
    drop(engine);
    assert!(crate::accounts::account_lock(&account_dir).is_err());
    assert!(matches!(
        UploadJournal::open(&root, &account_id, 1024),
        Err(JournalError::Busy)
    ));
    release_tx.send(())?;
    worker.await?;
    assert!(crate::accounts::account_lock(&account_dir).is_ok());
    assert!(UploadJournal::open(&root, &account_id, 1024).is_ok());
    Ok(())
}

#[tokio::test]
async fn read_only_busy_or_invalid_journal_is_not_silently_treated_as_empty() -> Result<()> {
    let (_temp, manager, engine) = fixture().await?;
    let root = engine
        .db
        .parent()
        .expect("fixture database has an account directory")
        .join("journal");
    let journal = UploadJournal::open(&root, &engine.account.id, 1024)?;
    assert!(manager.recent(&engine.account.label, 10).await.is_err());
    drop(journal);
    let wrong_account = "different-account";
    assert!(crate::journal::RecoveryJournal::open(&root, wrong_account).is_err());
    assert!(
        manager
            .recent(&engine.account.label, 10)
            .await?
            .local
            .is_empty()
    );
    Ok(())
}
#[tokio::test]
async fn read_only_replacement_names_use_local_namespace_then_index_then_id() -> Result<()> {
    let (_temp, manager, engine) = fixture().await?;
    let root = engine
        .db
        .parent()
        .expect("fixture database has an account directory")
        .join("journal");
    let mut journal = UploadJournal::open(&root, &engine.account.id, 1024 * 1024)?;
    let scope = engine.scope("fixture");
    let mut store = cirrove_store::Store::open(&engine.db)?;
    let mut namespace = node();
    namespace.id = "namespace-id".into();
    namespace.name = "Locally renamed.txt".into();
    journal.observe_namespace_file(scope.clone(), namespace.clone())?;
    let mut older = namespace.clone();
    older.name = "Old indexed name.txt".into();
    store.observe_node(&scope, &older)?;
    let mut indexed = node();
    indexed.id = "indexed-id".into();
    indexed.name = "Indexed Grüße.txt".into();
    store.observe_node(&scope, &indexed)?;
    // Same raw ID in another collection must not confer a display name.
    let mut unrelated = node();
    unrelated.id = "unknown-id".into();
    unrelated.name = "Wrong collection.txt".into();
    store.observe_node(&engine.scope("another-collection"), &unrelated)?;
    for id in ["namespace-id", "indexed-id", "unknown-id"] {
        journal.enqueue(
            scope.clone(),
            cirrove_core::upload::UploadIntent::Replace {
                item: id.into(),
                expected_etag: "old".into(),
            },
            &b"retained"[..],
        )?;
    }
    drop(journal);
    let before = std::fs::read(root.join("uploads.db"))?;
    let recent = manager.recent(&engine.account.label, 3).await?;
    let names: std::collections::HashMap<_, _> = recent
        .local
        .into_iter()
        .map(|record| (record.item.expect("replace identity"), record.name))
        .collect();
    assert_eq!(names["namespace-id"], "Locally renamed.txt");
    assert_eq!(names["indexed-id"], "Indexed Grüße.txt");
    assert_eq!(names["unknown-id"], "unknown-id");
    assert_eq!(std::fs::read(root.join("uploads.db"))?, before);
    assert!(manager.writers.read().await.is_empty());
    Ok(())
}

/// Pause after last-strong expiry, with the old journal's flock still held.
/// A new control must wait for close, not mistake Weak expiry for lock release.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expired_recovery_weak_waits_for_actual_journal_close_before_reopening() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (_temp, manager, engine) = fixture().await?;
        let root = engine
            .db
            .parent()
            .context("account directory")?
            .join("journal");
        drop(UploadJournal::open(&root, &engine.account.id, 1024)?);
        let control = manager.recovery_control(engine.clone()).await?;
        let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *engine
            .recovery_test_hooks
            .closing
            .lock()
            .expect("recovery test hook is not poisoned") =
            Some(crate::recovery::RecoveryCloseProbe {
                entered: closed_tx,
                release: release_rx,
            });
        let closing = tokio::task::spawn_blocking(move || drop(control));
        closed_rx.await?;
        assert!(engine.recovery_journal.lock().await.upgrade().is_none());
        assert!(
            matches!(
                UploadJournal::open(&root, &engine.account.id, 1024),
                Err(JournalError::Busy)
            ),
            "old payload must still hold its journal lock"
        );
        let (opening_tx, opening_rx) = tokio::sync::oneshot::channel();
        *engine
            .recovery_test_hooks
            .opening
            .lock()
            .expect("recovery test hook is not poisoned") = Some(opening_tx);
        let next_engine = engine.clone();
        let mut reopening =
            tokio::spawn(
                async move { crate::recovery::RecoveryControl::read_only(next_engine).await },
            );
        opening_rx.await?;
        // A controlled held-destructor interval, not a scheduling sleep or a
        // retry of Busy. Removing the opener gate completes immediately with Busy.
        let early = tokio::time::timeout(Duration::from_millis(200), &mut reopening).await;
        release_tx.send(())?;
        closing.await?;
        assert!(
            early.is_err(),
            "recovery reopen did not wait for old journal teardown"
        );
        let next = reopening.await??;
        assert!(
            matches!(
                UploadJournal::open(&root, &engine.account.id, 1024),
                Err(JournalError::Busy)
            ),
            "new control must own the journal"
        );
        drop(next);
        drop(UploadJournal::open(&root, &engine.account.id, 1024)?);
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("bounded recovery teardown fixture")?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recovery_payload_close_keeps_the_account_lease_until_journal_release() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (_temp, manager, engine) = fixture().await?;
        let directory = engine.db.parent().context("account directory")?.to_owned();
        let root = directory.join("journal");
        let account = engine.account.id.clone();
        drop(UploadJournal::open(&root, &account, 1024)?);
        let control = manager.recovery_control(engine.clone()).await?;
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *engine
            .recovery_test_hooks
            .closing
            .lock()
            .expect("recovery test hook is not poisoned") =
            Some(crate::recovery::RecoveryCloseProbe {
                entered: entered_tx,
                release: release_rx,
            });
        manager.engines.write().await.clear();
        engine.cancel.cancel();
        drop(engine);
        let closing = tokio::task::spawn_blocking(move || drop(control));
        entered_rx.await?;
        let account_locked = crate::accounts::account_lock(&directory).is_err();
        let journal_locked = matches!(
            UploadJournal::open(&root, &account, 1024),
            Err(JournalError::Busy)
        );
        release_tx.send(())?;
        closing.await?;
        assert!(
            account_locked,
            "account lease ended before journal teardown"
        );
        assert!(journal_locked, "teardown probe did not retain the journal");
        assert!(crate::accounts::account_lock(&directory).is_ok());
        drop(UploadJournal::open(&root, &account, 1024)?);
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("bounded account/journal teardown fixture")?
}

/// Cancelling the async opener must not release its cache guard while the
/// blocking worker owns a live journal which has not yet been published.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_recovery_opener_publishes_before_successor_can_open() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (_temp, _manager, engine) = fixture().await?;
        let root = engine
            .db
            .parent()
            .context("account directory")?
            .join("journal");
        drop(UploadJournal::open(&root, &engine.account.id, 1024)?);
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *engine
            .recovery_test_hooks
            .publishing
            .lock()
            .expect("recovery test hook is not poisoned") =
            Some(crate::recovery::RecoveryCloseProbe {
                entered: entered_tx,
                release: release_rx,
            });
        let opening_engine = engine.clone();
        let first = tokio::spawn(async move {
            crate::recovery::RecoveryControl::read_only(opening_engine).await
        });
        entered_rx.await?; // the real journal is open, but its Weak is unpublished
        first.abort();
        assert!(matches!(first.await, Err(error) if error.is_cancelled()));
        let cache_guard_retained = engine.recovery_journal.try_lock().is_err();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let next_engine = engine.clone();
        let next = tokio::spawn(async move {
            let _ = started_tx.send(());
            crate::recovery::RecoveryControl::read_only(next_engine).await
        });
        started_rx.await?;
        release_tx.send(())?;
        let control = next.await??;
        assert!(
            cache_guard_retained,
            "cancelled caller exposed an unpublished live journal"
        );
        assert!(matches!(
            UploadJournal::open(&root, &engine.account.id, 1024),
            Err(JournalError::Busy)
        ));
        // Existing control-retirement tests cover idle reopening after all leases
        // finish. Here the acceptance endpoint is successful successor ownership.
        drop(control);
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("bounded cancelled recovery opener fixture")?
}

/// Ordinary FileBytes with native-looking names are synthetic DATA-shaped
/// metadata, not Apple archives or an application/export-fidelity acceptance.
#[tokio::test]
async fn read_only_recent_completed_trash_handoff_uses_document_name_not_hidden_alias() -> Result<()>
{
    let mut observed = Vec::new();
    let expected = ["Report.txt", "Owned.pages", "Owned.numbers", "Owned.key"];
    for name in expected {
        let (_temp, manager, engine) = fixture().await?;
        let root = engine
            .db
            .parent()
            .context("account directory")?
            .join("journal");
        let scope = engine.scope("fixture");
        let mut journal = UploadJournal::open(&root, &engine.account.id, 1024 * 1024)?;
        let mut original = node();
        original.name = name.into();
        original.size = 3;
        let working =
            journal.create_working(scope.clone(), original.clone(), false, &b"old"[..])?;
        journal.write_working(working.id, 0, b"new!")?;
        let saved = journal
            .seal_working(working.id)?
            .context("sealed replacement")?;
        let claimed = journal.claim_next()?.context("claimed replacement")?;
        assert_eq!(claimed.id, saved.id);
        let attempt = claimed.attempt.context("upload attempt")?;
        let alias = format!("recovery-by-cirrove-{}.txt", saved.id);
        let recovery_id = journal.reserve_identity_handoff(
            saved.id,
            attempt,
            cirrove_core::upload::RecoveryLocation::Trash {
                local_name: alias.clone(),
                parent: "synthetic-trash-root".into(),
            },
        )?;
        let mut current = original.clone();
        current.id = "replacement-remote-id".into();
        current.size = saved.size;
        current.etag = Some("current-v2".into());
        current.content_version = Some("current-v2".into());
        let mut backup = original.clone();
        backup.parent_id = Some("synthetic-trash-root".into());
        backup.etag = Some("trash-v2".into());
        backup.content_version = Some("trash-v2".into());
        journal.acknowledge_identity_handoff(saved.id, attempt, current.clone(), backup.clone())?;
        let recorded = journal.get(saved.id)?;
        assert_eq!(recorded.state, crate::journal::UploadState::Uploaded);
        assert_eq!(recorded.remote.as_ref(), Some(&current));
        let owner = journal
            .namespace_for_operation(saved.id)?
            .context("operation owner")?;
        assert_eq!(owner.scope, scope);
        assert_eq!(owner.remote.as_ref(), Some(&current));
        let recovery = journal.namespace_object(recovery_id)?;
        assert_eq!(recovery.scope, scope);
        assert!(recovery.unlinked && recovery.remote_owned);
        assert_eq!(recovery.node.name, alias);
        assert_eq!(recovery.remote.as_ref(), Some(&backup));
        assert_eq!(backup.name, name);
        let records_before = serde_json::to_value(journal.list(0, 200)?)?;
        drop(journal);
        let db_before = std::fs::read(root.join("uploads.db"))?;
        let recent = manager.recent(&engine.account.label, 1).await?;
        assert_eq!(recent.local.len(), 1);
        assert_eq!(recent.local[0].operation, Some(saved.id));
        assert_eq!(recent.local[0].item.as_deref(), Some(original.id.as_str()));
        assert_eq!(recent.local[0].state, "uploaded");
        observed.push(recent.local[0].name.clone());
        assert!(manager.writers.read().await.is_empty());
        assert_eq!(std::fs::read(root.join("uploads.db"))?, db_before);
        let recovery = crate::journal::RecoveryJournal::open(&root, &engine.account.id)?;
        assert_eq!(
            serde_json::to_value(recovery.list(0, 200)?)?,
            records_before
        );
    }
    assert_eq!(
        observed, expected,
        "all four completed activities must name their documents, not hidden Trash aliases"
    );
    Ok(())
}

/// Ordinary FileBytes with native-looking names are synthetic DATA-shaped
/// metadata, not Apple archives or an application/export-fidelity acceptance.
#[tokio::test]
async fn read_only_recent_locally_created_then_replaced_uses_receipt_name_not_hidden_alias()
-> Result<()> {
    let mut observed = Vec::new();
    let expected = ["Report.txt", "Owned.pages", "Owned.numbers", "Owned.key"];
    for name in expected {
        let (_temp, manager, engine) = fixture().await?;
        let root = engine
            .db
            .parent()
            .context("account directory")?
            .join("journal");
        let scope = engine.scope("fixture");
        let mut journal = UploadJournal::open(&root, &engine.account.id, 1024 * 1024)?;
        let mut original = node();
        original.name = name.into();
        original.size = 3;
        let mut local_create = original.clone();
        local_create.size = 0;
        local_create.etag = None;
        local_create.content_version = None;
        let working = journal.create_working(scope.clone(), local_create, true, &b""[..])?;
        assert_ne!(working.node.id, original.id);
        journal.write_working(working.id, 0, b"old")?;
        let created = journal
            .seal_working(working.id)?
            .context("sealed local create")?;
        assert!(
            matches!(&created.intent, cirrove_core::upload::UploadIntent::Create { name: target, .. } if target == name)
        );
        let create_attempt = journal.claim_next()?.context("claimed local create")?;
        assert_eq!(create_attempt.id, created.id);
        journal.acknowledge(
            created.id,
            create_attempt.attempt.context("create attempt")?,
            original.clone(),
        )?;
        let created_owner = journal
            .namespace_for_operation(created.id)?
            .context("created owner")?;
        assert_eq!(created_owner.scope, scope);
        assert_eq!(created_owner.node.id, working.node.id);
        assert_eq!(created_owner.remote.as_ref(), Some(&original));
        journal.write_working(working.id, 0, b"new!")?;
        let saved = journal
            .seal_working(working.id)?
            .context("sealed replacement")?;
        let claimed = journal.claim_next()?.context("claimed replacement")?;
        assert_eq!(claimed.id, saved.id);
        let attempt = claimed.attempt.context("upload attempt")?;
        let alias = format!("recovery-by-cirrove-{}.txt", saved.id);
        let recovery_id = journal.reserve_identity_handoff(
            saved.id,
            attempt,
            cirrove_core::upload::RecoveryLocation::Trash {
                local_name: alias.clone(),
                parent: "synthetic-trash-root".into(),
            },
        )?;
        let mut current = original.clone();
        current.id = "replacement-remote-id".into();
        current.size = saved.size;
        current.etag = Some("current-v2".into());
        current.content_version = Some("current-v2".into());
        let mut backup = original.clone();
        backup.parent_id = Some("synthetic-trash-root".into());
        backup.etag = Some("trash-v2".into());
        backup.content_version = Some("trash-v2".into());
        journal.acknowledge_identity_handoff(saved.id, attempt, current.clone(), backup.clone())?;
        let recorded = journal.get(saved.id)?;
        assert_eq!(recorded.state, crate::journal::UploadState::Uploaded);
        assert_eq!(recorded.remote.as_ref(), Some(&current));
        let owner = journal
            .namespace_for_operation(saved.id)?
            .context("operation owner")?;
        assert_eq!(owner.scope, scope);
        assert_eq!(owner.remote.as_ref(), Some(&current));
        assert_eq!(owner.node.id, working.node.id);
        assert_ne!(owner.node.id, original.id);
        assert!(journal.namespace_by_local(&scope, &original.id)?.is_none());
        let recovery = journal.namespace_object(recovery_id)?;
        assert_eq!(recovery.scope, scope);
        assert!(recovery.unlinked && recovery.remote_owned);
        assert_eq!(recovery.node.name, alias);
        assert_eq!(recovery.remote.as_ref(), Some(&backup));
        assert_eq!(backup.name, name);
        let records_before = serde_json::to_value(journal.list(0, 200)?)?;
        drop(journal);
        let db_before = std::fs::read(root.join("uploads.db"))?;
        let recent = manager.recent(&engine.account.label, 1).await?;
        assert_eq!(recent.local.len(), 1);
        assert_eq!(recent.local[0].operation, Some(saved.id));
        assert_eq!(recent.local[0].item.as_deref(), Some(original.id.as_str()));
        assert_eq!(recent.local[0].state, "uploaded");
        observed.push(recent.local[0].name.clone());
        assert!(manager.writers.read().await.is_empty());
        assert_eq!(std::fs::read(root.join("uploads.db"))?, db_before);
        let recovery = crate::journal::RecoveryJournal::open(&root, &engine.account.id)?;
        assert_eq!(
            serde_json::to_value(recovery.list(0, 200)?)?,
            records_before
        );
    }
    assert_eq!(
        observed, expected,
        "all four locally created replacement activities must name their documents, not hidden Trash aliases"
    );
    Ok(())
}

#[tokio::test]
async fn read_only_recent_same_identity_receipt_keeps_historical_name_after_namespace_rename()
-> Result<()> {
    let (_temp, manager, engine) = fixture().await?;
    let root = engine
        .db
        .parent()
        .context("account directory")?
        .join("journal");
    let mut journal = UploadJournal::open(&root, &engine.account.id, 1024 * 1024)?;
    let original = node();
    let working = journal.create_working(
        engine.scope("fixture"),
        original.clone(),
        false,
        &b"sealed"[..],
    )?;
    journal.write_working(working.id, 0, b"edited")?;
    let saved = journal.seal_working(working.id)?.context("sealed save")?;
    let claimed = journal.claim_next()?.context("claimed save")?;
    let mut current = original.clone();
    current.etag = Some("same-item-v2".into());
    current.content_version = Some("same-item-v2".into());
    journal.acknowledge(
        saved.id,
        claimed.attempt.context("attempt")?,
        current.clone(),
    )?;
    assert!(journal.get(saved.id)?.identity_handoff.is_none());
    let owner = journal
        .namespace_for_operation(saved.id)?
        .context("save owner")?;
    let mut observed_later = current.clone();
    observed_later.name = "Renamed-later.txt".into();
    observed_later.etag = Some("later-v3".into());
    let following = journal.handoff_namespace(owner.id, owner.revision, observed_later.clone())?;
    assert!(following.follows_remote);
    assert_eq!(following.remote.as_ref(), Some(&observed_later));
    assert_eq!(journal.get(saved.id)?.remote.as_ref(), Some(&current));
    drop(journal);
    let reply = manager.recent(&engine.account.label, 1).await?;
    assert_eq!(reply.local[0].name, original.name);
    assert_eq!(reply.local[0].item.as_deref(), Some(original.id.as_str()));
    assert_eq!(reply.local[0].operation, Some(saved.id));
    Ok(())
}

#[tokio::test]
async fn read_only_recent_unconfirmed_saves_keep_document_name_and_state() -> Result<()> {
    use crate::journal::UploadState;
    for state in [
        UploadState::Pending,
        UploadState::Uploading,
        UploadState::VerifyRequired,
        UploadState::Conflict,
        UploadState::Failed,
    ] {
        let (_temp, manager, engine) = fixture().await?;
        let root = engine
            .db
            .parent()
            .context("account directory")?
            .join("journal");
        let mut journal = UploadJournal::open(&root, &engine.account.id, 1024 * 1024)?;
        let mut original = node();
        original.name = "Unconfirmed.numbers".into();
        let working = journal.create_working(
            engine.scope("fixture"),
            original.clone(),
            false,
            &b"sealed"[..],
        )?;
        journal.write_working(working.id, 0, b"edited")?;
        let saved = journal.seal_working(working.id)?.context("sealed save")?;
        if state != UploadState::Pending {
            let claimed = journal.claim_next()?.context("claimed save")?;
            if state != UploadState::Uploading {
                journal.stop_attempt(saved.id, claimed.attempt.context("attempt")?, state)?;
            }
        }
        assert!(journal.get(saved.id)?.remote.is_none());
        drop(journal);
        let before = std::fs::read(root.join("uploads.db"))?;
        let reply = manager.recent(&engine.account.label, 1).await?;
        assert_eq!(reply.local[0].name, original.name);
        assert_eq!(reply.local[0].operation, Some(saved.id));
        assert_eq!(
            reply.local[0].state,
            format!("{state:?}").to_ascii_lowercase()
        );
        assert_eq!(std::fs::read(root.join("uploads.db"))?, before);
        assert!(manager.writers.read().await.is_empty());
    }
    Ok(())
}

#[tokio::test]
async fn read_only_recent_rejects_foreign_scope_and_invalid_completed_receipt_name() -> Result<()> {
    for (path, value) in [
        ("$.scope.account", serde_json::json!("another-account")),
        ("$.scope.provider", serde_json::json!("another-provider")),
        ("$.scope.collection", serde_json::json!("")),
        ("$.remote.name", serde_json::json!("")),
        ("$.remote.name", serde_json::json!("not/a/basename.numbers")),
        ("$.remote.name", serde_json::json!("bad\0name.numbers")),
        ("$.remote", serde_json::Value::Null),
        (
            "$.identity_handoff.old_item",
            serde_json::json!("another-item"),
        ),
        ("$.identity_handoff.backup", serde_json::Value::Null),
    ] {
        let (_temp, manager, engine) = fixture().await?;
        let root = engine
            .db
            .parent()
            .context("account directory")?
            .join("journal");
        let mut journal = UploadJournal::open(&root, &engine.account.id, 1024 * 1024)?;
        let original = node();
        let working = journal.create_working(
            engine.scope("fixture"),
            original.clone(),
            false,
            &b"sealed"[..],
        )?;
        journal.write_working(working.id, 0, b"edited")?;
        let saved = journal.seal_working(working.id)?.context("sealed save")?;
        let claimed = journal.claim_next()?.context("claimed save")?;
        let attempt = claimed.attempt.context("attempt")?;
        journal.reserve_identity_handoff(
            saved.id,
            attempt,
            cirrove_core::upload::RecoveryLocation::Trash {
                local_name: format!("recovery-by-cirrove-{}.txt", saved.id),
                parent: "synthetic-trash-root".into(),
            },
        )?;
        let mut current = original.clone();
        current.id = "replacement-remote-id".into();
        current.etag = Some("current-v2".into());
        let mut backup = original;
        backup.parent_id = Some("synthetic-trash-root".into());
        backup.etag = Some("trash-v2".into());
        journal.acknowledge_identity_handoff(saved.id, attempt, current, backup)?;
        drop(journal);
        let db = rusqlite::Connection::open(root.join("uploads.db"))?;
        db.execute(
            "UPDATE uploads SET body=json_set(body,?2,json(?3)) WHERE id=?1",
            rusqlite::params![saved.id.to_string(), path, serde_json::to_string(&value)?],
        )?;
        drop(db);
        let before = std::fs::read(root.join("uploads.db"))?;
        assert!(
            manager.recent(&engine.account.label, 1).await.is_err(),
            "invalid scope or completed historical receipt must be refused"
        );
        assert_eq!(std::fs::read(root.join("uploads.db"))?, before);
    }
    Ok(())
}

#[tokio::test]
async fn read_only_recent_keeps_legitimate_linked_collection_receipts_distinct() -> Result<()> {
    let (_temp, manager, engine) = fixture().await?;
    let root = engine
        .db
        .parent()
        .context("account directory")?
        .join("journal");
    let mut journal = UploadJournal::open(&root, &engine.account.id, 1024 * 1024)?;
    let mut expected = Vec::new();
    for (collection, name) in [
        ("fixture", "Primary.txt"),
        ("linked-drive", "Linked.numbers"),
    ] {
        let scope = engine.scope(collection);
        let mut original = node();
        // Item strings can coincide across collections; the durable scope is
        // the identity, not the basename or the engine's primary drive.
        original.name = name.into();
        let working =
            journal.create_working(scope.clone(), original.clone(), false, &b"sealed"[..])?;
        journal.write_working(working.id, 0, b"edited")?;
        let saved = journal.seal_working(working.id)?.context("sealed save")?;
        let claimed = journal.claim_next()?.context("claimed save")?;
        assert_eq!(claimed.id, saved.id);
        let mut current = original.clone();
        current.etag = Some("current-v2".into());
        journal.acknowledge(
            saved.id,
            claimed.attempt.context("attempt")?,
            current.clone(),
        )?;
        assert_eq!(journal.get(saved.id)?.scope, scope);
        assert_eq!(journal.get(saved.id)?.remote.as_ref(), Some(&current));
        expected.push((saved.id, original.id, name));
    }
    drop(journal);
    let before = std::fs::read(root.join("uploads.db"))?;
    let reply = manager.recent(&engine.account.label, 2).await?;
    assert_eq!(reply.local.len(), 2);
    for (operation, item, name) in expected {
        let shown = reply
            .local
            .iter()
            .find(|change| change.operation == Some(operation))
            .context("exact scoped operation displayed")?;
        assert_eq!(shown.name, name);
        assert_eq!(shown.item.as_deref(), Some(item.as_str()));
        assert_eq!(shown.state, "uploaded");
    }
    assert_eq!(std::fs::read(root.join("uploads.db"))?, before);
    assert!(manager.writers.read().await.is_empty());
    Ok(())
}

#[tokio::test]
async fn read_only_recent_google_receipt_keeps_permitted_lf_and_tab_basenames() -> Result<()> {
    for name in ["Line\nBreak.txt", "Tab\tName.txt"] {
        let temp = tempfile::tempdir()?;
        let mut account = account(temp.path().join("mount"), 64 * 1024 * 1024);
        account.registration = cirrove_auth::AppRegistration::Google {
            client_id: uuid::Uuid::new_v4().to_string(),
        };
        account.access = cirrove_auth::AccessMode::ReadOnly;
        account.enabled = true;
        let provider = Arc::new(cirrove_googledrive::GoogleDrive::synthetic_loopback(
            account.id.clone(),
            account.drive.id.clone(),
            "http://127.0.0.1:9/",
        )?);
        let engine = Engine::new(account.clone(), provider, temp.path().to_owned()).await?;
        assert_eq!(engine.provider.provider_id(), "googledrive");
        assert!(matches!(
            engine.account.registration,
            cirrove_auth::AppRegistration::Google { .. }
        ));
        let manager = Manager::default();
        manager.status.write().await.push(AccountStatus {
            account_id: account.id.clone(),
            label: account.label.clone(),
            enabled: true,
            mount_path: account.mount_path,
            ..Default::default()
        });
        manager
            .engines
            .write()
            .await
            .insert(account.id.clone(), engine.clone());
        let root = engine
            .db
            .parent()
            .context("account directory")?
            .join("journal");
        let mut journal = UploadJournal::open(&root, &engine.account.id, 1024 * 1024)?;
        let mut original = node();
        original.name = name.into();
        let working = journal.create_working(
            engine.scope("fixture"),
            original.clone(),
            false,
            &b"sealed"[..],
        )?;
        journal.write_working(working.id, 0, b"edited")?;
        let saved = journal
            .seal_working(working.id)?
            .context("sealed Google save")?;
        let claimed = journal.claim_next()?.context("claimed Google save")?;
        let mut current = original.clone();
        current.etag = Some("google-current-v2".into());
        journal.acknowledge(saved.id, claimed.attempt.context("attempt")?, current)?;
        drop(journal);
        let before = std::fs::read(root.join("uploads.db"))?;
        let reply = manager.recent(&engine.account.label, 1).await?;
        assert_eq!(reply.local[0].operation, Some(saved.id));
        assert_eq!(reply.local[0].item.as_deref(), Some(original.id.as_str()));
        assert_eq!(reply.local[0].name, name);
        assert_eq!(reply.local[0].state, "uploaded");
        assert_eq!(std::fs::read(root.join("uploads.db"))?, before);
        assert!(manager.writers.read().await.is_empty());
    }
    Ok(())
}
