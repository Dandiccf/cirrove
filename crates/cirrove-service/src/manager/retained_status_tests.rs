//! A read-only restart must still report retained outcomes; it cannot retry them.
use super::*;
use crate::journal::{MutationState, UploadState};
use cirrove_core::{
    ChangePage, Cursor, DirectoryPage, MetadataProvider, Node, ProviderError, Scope,
};
use sha2::Digest;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct LocalOnly {
    content_reads: AtomicUsize,
}
#[async_trait::async_trait]
impl MetadataProvider for LocalOnly {
    fn provider_id(&self) -> &'static str {
        "onedrive"
    }
    async fn changes(
        &self,
        _: &Scope,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<ChangePage, ProviderError> {
        // Pure synthetic failure, not a request to a loopback or cloud server.
        Err(ProviderError::Unavailable)
    }
}
#[async_trait::async_trait]
impl ReadProvider for LocalOnly {
    async fn node(
        &self,
        _: &Scope,
        _: &str,
        _: &CancellationToken,
    ) -> std::result::Result<Node, ProviderError> {
        Err(ProviderError::Unavailable)
    }
    async fn children(
        &self,
        _: &Scope,
        _: &str,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<DirectoryPage, ProviderError> {
        Err(ProviderError::Unavailable)
    }
    async fn read_range(
        &self,
        _: &Scope,
        _: &Node,
        _: u64,
        _: u32,
        _: &CancellationToken,
    ) -> std::result::Result<Vec<u8>, ProviderError> {
        self.content_reads.fetch_add(1, Ordering::SeqCst);
        Err(ProviderError::Unavailable)
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    state: PathBuf,
    journal: PathBuf,
    account: Account,
    provider: Arc<LocalOnly>,
    before: Vec<(PathBuf, u64, String)>,
}
impl Fixture {
    async fn new(retained: bool) -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let state = temp.path().join("state");
        crate::private_dir(&state)?;
        let mount_path = temp.path().join("mount");
        crate::private_dir(&mount_path)?;
        // The real manager attempts its normal mount admission, which refuses
        // this nonempty directory before opening FUSE or spawning any worker.
        std::fs::write(mount_path.join("local-sentinel"), b"preserve")?;
        let mut account = Account {
            id: uuid::Uuid::new_v4().to_string(),
            label: "retained-status-fixture".into(),
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
            enabled: true,
            poll_seconds: 3600,
            cache_bytes: 64 * 1024 * 1024,
        };
        let provider = Arc::new(LocalOnly::default());
        let engine = Engine::new(account.clone(), provider.clone(), state.clone()).await?;
        let journal = engine
            .db
            .parent()
            .context("account directory")?
            .join("journal");
        if retained {
            let context = WriteContext::open(&engine, &state).await?;
            {
                let journal_arc = context.journal();
                let mut writer = journal_arc.lock().expect("fixture journal");
                for (name, outcome) in [
                    ("failed-save.txt", UploadState::Failed),
                    ("conflicted-save.txt", UploadState::Conflict),
                    ("unconfirmed-save.txt", UploadState::VerifyRequired),
                ] {
                    let row = writer.enqueue(
                        engine.scope("fixture"),
                        cirrove_core::upload::UploadIntent::Create {
                            parent: "root".into(),
                            name: name.into(),
                        },
                        &b"retained-bytes"[..],
                    )?;
                    let claimed = writer.claim_next()?.context("claimed upload")?;
                    assert_eq!(claimed.id, row.id);
                    writer.stop_attempt(
                        row.id,
                        claimed.attempt.context("upload attempt")?,
                        outcome,
                    )?;
                }
                for (name, outcome) in [
                    ("failed-folder", MutationState::Failed),
                    ("conflicted-folder", MutationState::Conflict),
                    ("review-folder", MutationState::NeedsReview),
                    ("unconfirmed-folder", MutationState::VerifyRequired),
                ] {
                    let row = writer.enqueue_mutation(cirrove_core::mutation::MutationRequest {
                        scope: engine.scope("fixture"),
                        intent: cirrove_core::mutation::MutationIntent::CreateFolder {
                            parent: "root".into(),
                            name: name.into(),
                        },
                    })?;
                    let claimed = writer.claim_mutation()?.context("claimed mutation")?;
                    assert_eq!(claimed.id, row.id);
                    writer.defer_mutation(
                        row.id,
                        claimed.attempt.context("mutation attempt")?,
                        outcome,
                        Duration::from_secs(3600),
                    )?;
                }
                // Pending work must not inflate failure or uncertainty notices.
                writer.enqueue(
                    engine.scope("fixture"),
                    cirrove_core::upload::UploadIntent::Create {
                        parent: "root".into(),
                        name: "pending-save.txt".into(),
                    },
                    &b"pending-bytes"[..],
                )?;
                writer.enqueue_mutation(cirrove_core::mutation::MutationRequest {
                    scope: engine.scope("fixture"),
                    intent: cirrove_core::mutation::MutationIntent::CreateFolder {
                        parent: "root".into(),
                        name: "pending-folder".into(),
                    },
                })?;
                assert_eq!(writer.stuck_mutations()?, 3);
                assert_eq!(writer.unconfirmed_changes()?, 2);
                assert_eq!(writer.failed_uploads()?, 2);
            }
            drop(context);
        }
        drop(engine);
        // Same account, collection and credential identity; only mode changes.
        account.access = cirrove_auth::AccessMode::ReadOnly;
        let settings = Settings {
            version: 2,
            accounts: vec![account.clone()],
        };
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(state.join("accounts.json"))?;
        std::io::Write::write_all(&mut file, &serde_json::to_vec(&settings)?)?;
        file.sync_all()?;
        assert_eq!(Settings::load(&state)?.accounts[0].id, account.id);
        let mut fixture = Self {
            _temp: temp,
            state,
            journal,
            account,
            provider,
            before: Vec::new(),
        };
        fixture.before = fixture.snapshot()?;
        Ok(fixture)
    }
    fn snapshot(&self) -> Result<Vec<(PathBuf, u64, String)>> {
        let mut paths = vec![
            self.state.join("accounts.json"),
            self.account.mount_path.join("local-sentinel"),
        ];
        if self.journal.exists() {
            for entry in std::fs::read_dir(&self.journal)? {
                let path = entry?.path();
                // A read-only WAL-mode SQLite connection may create empty WAL
                // and SHM coordination files. They are not retained edits;
                // a nonempty unexpected WAL must still fail preservation.
                if path
                    .file_name()
                    .is_some_and(|name| name == "uploads.db-shm")
                {
                    continue;
                }
                if path
                    .file_name()
                    .is_some_and(|name| name == "uploads.db-wal")
                {
                    assert_eq!(
                        std::fs::metadata(&path)?.len(),
                        0,
                        "unexpected retained WAL writes"
                    );
                    continue;
                }
                if path.is_file() {
                    paths.push(path);
                } else if path
                    .file_name()
                    .is_some_and(|name| name == "objects" || name == "working")
                {
                    for entry in std::fs::read_dir(path)? {
                        paths.push(entry?.path());
                    }
                }
            }
        }
        paths.sort();
        paths
            .into_iter()
            .map(|path| {
                let bytes = std::fs::read(&path)?;
                Ok((
                    path,
                    bytes.len() as u64,
                    hex::encode(sha2::Sha256::digest(bytes)),
                ))
            })
            .collect()
    }
    async fn start(
        &self,
    ) -> Result<(Arc<Manager>, CancellationToken, tokio::task::JoinHandle<()>)> {
        let provider = self.provider.clone();
        let cancel = CancellationToken::new();
        let (manager, task) = Manager::start_with_provider(
            self.state.clone(),
            cancel.clone(),
            Arc::new(move |_| Ok(provider.clone())),
        );
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if manager
                    .status
                    .read()
                    .await
                    .iter()
                    .any(|status| status.local_recovery)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        if result.is_err() {
            cancel.cancel();
            task.await?;
            bail!("read-only manager did not publish its status");
        }
        Ok((manager, cancel, task))
    }
    fn unchanged(&self) -> Result<()> {
        assert_eq!(
            self.snapshot()?,
            self.before,
            "read-only status changed retained journal/settings/payload bytes"
        );
        assert_eq!(self.provider.content_reads.load(Ordering::SeqCst), 0);
        Ok(())
    }
}

#[tokio::test]
async fn readonly_retained_status_reports_failed_stuck_and_unconfirmed_without_replay() -> Result<()>
{
    let fixture = Fixture::new(true).await?;
    let (manager, cancel, task) = fixture.start().await?;
    let status = manager.status.read().await[0].clone();
    let events = crate::events::prime(std::slice::from_ref(&status));
    let no_writers = manager.writers.read().await.is_empty();
    let retry = manager.retry_stuck(&fixture.account.label).await;
    let discard = manager.discard_stuck(&fixture.account.label).await;
    cancel.cancel();
    task.await?;
    fixture.unchanged()?;
    assert!(no_writers && retry.is_err() && discard.is_err());
    assert!(!status.mounted && status.local_recovery);
    assert_eq!(
        (
            status.stuck_changes,
            status.unconfirmed_changes,
            status.failed_uploads
        ),
        (3, 2, 2),
        "read-only retained outcomes must remain visible in actual manager status"
    );
    assert!(
        events.into_iter().any(|event| matches!(
            event,
            crate::events::Event::Account {
                stuck_changes: 3,
                unconfirmed_changes: 2,
                failed_uploads: 2,
                ..
            }
        )),
        "event priming must carry the same retained counts"
    );
    Ok(())
}

#[tokio::test]
async fn readonly_retained_status_recent_names_retained_failures_without_replay() -> Result<()> {
    let fixture = Fixture::new(true).await?;
    let (manager, cancel, task) = fixture.start().await?;
    let recent = manager.recent(&fixture.account.label, 200).await;
    cancel.cancel();
    task.await?;
    fixture.unchanged()?;
    let recent = recent?;
    assert_eq!(
        recent.local.len(),
        4,
        "all four immutable saves remain retained"
    );
    assert_eq!(
        (recent.stuck.len(), recent.failed.len()),
        (3, 2),
        "read-only retained failures must remain named in actual manager recent reply"
    );
    let mut names: Vec<_> = recent
        .failed
        .iter()
        .map(|change| change.name.as_str())
        .collect();
    names.sort();
    assert_eq!(names, ["conflicted-save.txt", "failed-save.txt"]);
    assert!(
        recent
            .stuck
            .iter()
            .all(|change| change.name.ends_with("-folder"))
    );
    Ok(())
}

#[tokio::test]
async fn readonly_retained_status_missing_journal_stays_empty_and_does_not_create_it() -> Result<()>
{
    let fixture = Fixture::new(false).await?;
    let (manager, cancel, task) = fixture.start().await?;
    let status = manager.status.read().await[0].clone();
    let recent = manager.recent(&fixture.account.label, 200).await;
    let no_writers = manager.writers.read().await.is_empty();
    cancel.cancel();
    task.await?;
    fixture.unchanged()?;
    let recent = recent?;
    assert_eq!(
        (
            status.stuck_changes,
            status.unconfirmed_changes,
            status.failed_uploads
        ),
        (0, 0, 0)
    );
    assert!(recent.local.is_empty() && recent.stuck.is_empty() && recent.failed.is_empty());
    assert!(no_writers && !fixture.journal.exists());
    assert_eq!(
        std::fs::metadata(&fixture.state)?.permissions().mode() & 0o077,
        0
    );
    Ok(())
}

#[tokio::test]
async fn readonly_retained_status_busy_reader_preserves_counts_and_recovers_availability()
-> Result<()> {
    let fixture = Fixture::new(true).await?;
    let (manager, cancel, task) = fixture.start().await?;
    let first = manager.status.read().await[0].clone();
    let observed = async {
        // A second genuine exclusive, read-only recovery owner blocks the
        // manager's next inspection. It neither migrates nor changes records.
        let held = crate::journal::RecoveryJournal::open(&fixture.journal, &fixture.account.id)?;
        let unavailable = tokio::time::timeout(Duration::from_secs(12), async {
            loop {
                let status = manager.status.read().await[0].clone();
                if !status.local_recovery {
                    break status;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .context("busy journal must withdraw recovery availability")?;
        let recent_refused = manager.recent(&fixture.account.label, 200).await.is_err();
        drop(held);
        let recovered = tokio::time::timeout(Duration::from_secs(12), async {
            loop {
                let status = manager.status.read().await[0].clone();
                if status.local_recovery {
                    break status;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .context("released journal must restore recovery availability")?;
        Ok::<_, anyhow::Error>((unavailable, recent_refused, recovered))
    }
    .await;
    let no_writers = manager.writers.read().await.is_empty();
    cancel.cancel();
    task.await?;
    fixture.unchanged()?;
    let (unavailable, recent_refused, recovered) = observed?;
    assert_eq!(
        (
            first.stuck_changes,
            first.unconfirmed_changes,
            first.failed_uploads
        ),
        (3, 2, 2)
    );
    assert_eq!(
        (
            unavailable.stuck_changes,
            unavailable.unconfirmed_changes,
            unavailable.failed_uploads
        ),
        (3, 2, 2),
        "busy read-only inspection must preserve last known retained outcome counts"
    );
    assert!(
        recent_refused && no_writers && !unavailable.local_recovery && recovered.local_recovery
    );
    assert_eq!(
        (
            recovered.stuck_changes,
            recovered.unconfirmed_changes,
            recovered.failed_uploads
        ),
        (3, 2, 2)
    );
    Ok(())
}

// This public Manager path deliberately mounts read-only without changing a
// recorded ReadWrite grant. No FUSE/kernel capability is needed: the retained
// sentinel rejects mount admission before any filesystem session is spawned.
#[tokio::test]
async fn ordinary_handoff_effective_readonly_manager_repairs_with_readwrite_grant() -> Result<()> {
    use cirrove_core::NodeKind;
    use cirrove_store::Store;
    use std::io::Read;

    let Fixture {
        _temp,
        state,
        journal: journal_root,
        mut account,
        provider,
        ..
    } = Fixture::new(false).await?;
    let root = _temp.keep(); // Preserve this fixture; no recursive cleanup.
    eprintln!(
        "retained effective read-only publication fixture: {}",
        root.display()
    );
    account.access = cirrove_auth::AccessMode::ReadWrite;
    let settings = serde_json::to_vec(&Settings {
        version: 2,
        accounts: vec![account.clone()],
    })?;
    std::fs::write(state.join("accounts.json"), &settings)?;
    let engine = Engine::new(account.clone(), provider.clone(), state.clone()).await?;
    let metadata = engine.db.clone();
    let scope = engine.scope("fixture");
    let original = Node {
        id: "old-A".into(),
        parent_id: Some("root".into()),
        name: "owned.txt".into(),
        kind: NodeKind::File,
        size: 3,
        modified_unix: 1,
        etag: Some("revision-A".into()),
        content_version: Some("revision-A".into()),
        target: None,
        package: false,
    };
    let current = Node {
        id: "new-B".into(),
        size: 8,
        etag: Some("revision-B".into()),
        content_version: Some("revision-B".into()),
        ..original.clone()
    };
    let backup = Node {
        parent_id: Some("trash".into()),
        etag: Some("trash-A".into()),
        content_version: Some("trash-A".into()),
        ..original.clone()
    };
    let mut journal =
        crate::journal::UploadJournal::open(&journal_root, &account.id, account.cache_bytes)?;
    let working = journal.create_working(scope.clone(), original.clone(), false, &b"old"[..])?;
    journal.write_working(working.id, 0, b"new-body")?;
    let uploaded = journal.seal_working(working.id)?.context("sealed B")?;
    let claimed = journal.claim_next()?.context("claimed B")?;
    assert_eq!(claimed.id, uploaded.id);
    let attempt = claimed.attempt.context("B attempt")?;
    journal.reserve_identity_handoff(
        uploaded.id,
        attempt,
        cirrove_core::upload::RecoveryLocation::Trash {
            local_name: "recovery-A".into(),
            parent: "trash".into(),
        },
    )?;
    journal.acknowledge_identity_handoff(uploaded.id, attempt, current.clone(), backup.clone())?;
    assert_eq!(journal.get(uploaded.id)?.state, UploadState::Uploaded);
    assert_eq!(
        journal.get(uploaded.id)?.ordinary_handoff_receipt(),
        Some((&current, &backup))
    );
    // A metadata-only reader must not normalize or claim this newer save.
    journal.write_working(working.id, 0, b"pending-C")?;
    let later = journal.seal_working(working.id)?.context("sealed C")?;
    let later_claim = journal.claim_next()?.context("claimed C")?;
    assert_eq!(later_claim.id, later.id);
    assert_eq!(later_claim.state, UploadState::Uploading);
    let mut b_bytes = Vec::new();
    journal.payload(uploaded.id)?.read_to_end(&mut b_bytes)?;
    assert_eq!(b_bytes, b"new-body");
    let mut c_bytes = Vec::new();
    journal.payload(later.id)?.read_to_end(&mut c_bytes)?;
    assert_eq!(c_bytes, b"pending-C");
    let working_before = serde_json::to_value(journal.working_file(working.id)?)?;
    drop(journal);
    let mut store = Store::open(&metadata)?;
    store.observe_directory(&scope, "root", &[original.clone(), current.clone()])?;
    store.observe_node(&scope, &current)?;
    assert_eq!(
        store
            .children(&scope, "root")?
            .context("cached original and current listing")?
            .len(),
        2
    );
    drop(store);
    drop(engine); // Release the actual account owner before public Manager startup.

    let db_path = journal_root.join("uploads.db");
    let rows = || -> Result<Vec<(String, String)>> {
        let db = rusqlite::Connection::open(&db_path)?;
        let mut statement = db.prepare("SELECT id,body FROM uploads ORDER BY sequence")?;
        Ok(statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    };
    let before_uploads = rows()?;
    let cancel = CancellationToken::new();
    let supplied = provider.clone();
    let (manager, task) = Manager::start_with_provider(
        state.clone(),
        cancel.clone(),
        Arc::new(move |_| Ok(supplied.clone())),
    );
    let waited = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let done: bool = rusqlite::Connection::open(&db_path)?.query_row(
                "SELECT done FROM ordinary_metadata_publication WHERE operation=?1",
                [uploaded.id.to_string()],
                |row| row.get(0),
            )?;
            if done {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    let launched_grant = manager
        .engines
        .read()
        .await
        .get(&account.id)
        .map(|engine| engine.account.access);
    let no_writers = manager.writers.read().await.is_empty();
    cancel.cancel();
    task.await?; // Stop/join before any desired assertion, including baseline failure.
    assert_eq!(launched_grant, Some(cirrove_auth::AccessMode::ReadWrite));
    assert!(no_writers);
    assert_eq!(
        rows()?,
        before_uploads,
        "metadata repair changed an upload frontier"
    );
    assert_eq!(std::fs::read(state.join("accounts.json"))?, settings);
    assert_eq!(
        std::fs::read(account.mount_path.join("local-sentinel"))?,
        b"preserve"
    );
    assert_eq!(provider.content_reads.load(Ordering::SeqCst), 0);
    assert_eq!(
        std::fs::metadata(state.join("accounts.json"))?
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let db = rusqlite::Connection::open(&db_path)?;
    let working_after: String = db.query_row(
        "SELECT body FROM working_files WHERE id=?1",
        [working.id.to_string()],
        |row| row.get(0),
    )?;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&working_after)?,
        working_before
    );
    assert_eq!(
        std::fs::read(journal_root.join("objects").join(uploaded.id.to_string()))?,
        b_bytes
    );
    assert_eq!(
        std::fs::read(journal_root.join("objects").join(later.id.to_string()))?,
        c_bytes
    );
    drop(db);
    let done: bool = rusqlite::Connection::open(&db_path)?.query_row(
        "SELECT done FROM ordinary_metadata_publication WHERE operation=?1",
        [uploaded.id.to_string()],
        |row| row.get(0),
    )?;
    let store = Store::open(&metadata)?;
    assert_eq!(
        store.node(&scope, &original.id)?,
        Some(backup),
        "effective read-only Manager must repair the retained ordinary receipt even with a ReadWrite grant"
    );
    assert!(done && matches!(waited, Ok(Ok(()))));
    assert_eq!(store.children(&scope, "root")?, Some(vec![current]));
    Ok(())
}
