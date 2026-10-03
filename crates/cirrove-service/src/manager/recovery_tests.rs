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
