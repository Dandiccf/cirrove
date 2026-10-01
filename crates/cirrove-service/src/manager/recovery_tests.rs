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
    let recent = manager.recent(&label, 1).await?;
    assert_eq!(recent.local[0].operation, Some(saved.id));
    assert_eq!(recent.local[0].name, "private.txt"); // persisted working name, no provider lookup
    let listed = manager
        .recovery_working(&crate::RecoveryWorkingRequest {
            label: label.clone(),
            after: None,
            limit: 1,
        })
        .await?;
    assert_eq!(listed.0.len(), 1);
    let export = crate::ExportSaveRequest {
        label: label.clone(),
        operation: saved.id,
        destination: temp.path().join("saved-copy"),
    };
    let initial = manager.export_save(&export).await?;
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
    let initial = manager.export_working(&request).await?;
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
    let recovery = crate::journal::RecoveryJournal::open(&root, &engine.account.id)?;
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
