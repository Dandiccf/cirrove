//! Real Manager lifecycle and local exports; no credentials, provider requests or FUSE.
use super::*;
use crate::journal::{UploadJournal, UploadState};
use cirrove_auth::AccessMode;
use cirrove_core::upload::RecoveryLocation;
use cirrove_core::{
    ChangePage, Cursor, DirectoryPage, MetadataProvider, Node, NodeKind, ProviderError,
};
use sha2::Digest;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct LocalOnly {
    content_reads: AtomicUsize,
    publication_reads: AtomicUsize,
}
#[async_trait::async_trait]
impl MetadataProvider for LocalOnly {
    fn provider_id(&self) -> &'static str {
        "icloud"
    }
    async fn changes(
        &self,
        _: &cirrove_core::Scope,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<ChangePage, ProviderError> {
        Err(ProviderError::Unavailable)
    }
}
#[async_trait::async_trait]
impl ReadProvider for LocalOnly {
    async fn node(
        &self,
        _: &cirrove_core::Scope,
        id: &str,
        _: &CancellationToken,
    ) -> std::result::Result<Node, ProviderError> {
        if id == "publication-original" {
            self.publication_reads.fetch_add(1, Ordering::SeqCst);
        }
        Err(ProviderError::Unavailable)
    }
    async fn children(
        &self,
        _: &cirrove_core::Scope,
        _: &str,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<DirectoryPage, ProviderError> {
        Err(ProviderError::Unavailable)
    }
    async fn read_range(
        &self,
        _: &cirrove_core::Scope,
        _: &Node,
        _: u64,
        _: u32,
        _: &CancellationToken,
    ) -> std::result::Result<Vec<u8>, ProviderError> {
        self.content_reads.fetch_add(1, Ordering::SeqCst);
        panic!("local recovery must not download retained content")
    }
}

type Cells = Vec<(String, Vec<Vec<rusqlite::types::Value>>)>;
type RetainedFile = (PathBuf, u64, u64, u32, u32, u64, Vec<u8>);
#[derive(Debug, PartialEq)]
struct JournalSnapshot {
    version: u32,
    ddl: Vec<(String, String, String, Option<String>)>,
    tables: Cells,
    // Full retained file membership, identity, mode, length and exact bytes.
    files: Vec<RetainedFile>,
}
fn snapshot(root: &Path) -> Result<JournalSnapshot> {
    let db = rusqlite::Connection::open_with_flags(
        root.join("uploads.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    db.execute_batch("PRAGMA query_only=ON; BEGIN")?;
    let version = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
    let mut q =
        db.prepare("SELECT type,name,tbl_name,sql FROM sqlite_master ORDER BY type,name")?;
    let ddl = q
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<Vec<(String, String, String, Option<String>)>>>()?;
    drop(q);
    let mut tables = Vec::new();
    for (kind, name, _, _) in &ddl {
        if kind != "table" {
            continue;
        }
        let mut q = db.prepare(&format!(
            "SELECT * FROM \"{}\" ORDER BY rowid",
            name.replace('"', "\"\"")
        ))?;
        let width = q.column_count();
        let cells = q
            .query_map([], |r| {
                (0..width)
                    .map(|c| r.get(c))
                    .collect::<rusqlite::Result<Vec<rusqlite::types::Value>>>()
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        tables.push((name.clone(), cells));
    }
    db.execute_batch("COMMIT")?;
    drop(db);
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let path = entry?.path();
        let meta = std::fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            for entry in std::fs::read_dir(path)? {
                paths.push(entry?.path());
            }
        } else {
            paths.push(path);
        }
    }
    paths.sort();
    let mut files = Vec::new();
    for path in paths {
        let m = std::fs::symlink_metadata(&path)?;
        anyhow::ensure!(
            m.is_file() && !m.file_type().is_symlink(),
            "unexpected retained file type"
        );
        files.push((
            path.strip_prefix(root)?.to_owned(),
            m.dev(),
            m.ino(),
            m.mode(),
            m.uid(),
            m.nlink(),
            std::fs::read(path)?,
        ));
    }
    Ok(JournalSnapshot {
        version,
        ddl,
        tables,
        files,
    })
}
type Publication = (String, i64, i64, i64, String, i64, String, i64, i64);
fn publication(root: &Path, operation: uuid::Uuid) -> Result<Publication> {
    let db = rusqlite::Connection::open_with_flags(
        root.join("uploads.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    db.execute_batch("PRAGMA query_only=ON")?;
    Ok(db.query_row("SELECT p.body,p.done,p.retry_after,p.failures,u.body,u.sequence,u.state,q.sequence,q.complete
        FROM ordinary_metadata_publication p JOIN uploads u ON u.id=p.operation
        JOIN write_queue q ON q.id=u.id WHERE p.operation=?1", [operation.to_string()],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?)))?)
}
fn account(mount_path: PathBuf) -> Account {
    Account {
        id: uuid::Uuid::new_v4().to_string(),
        label: "recovery-only-fixture".into(),
        registration: cirrove_auth::AppRegistration::ICloud,
        identity: cirrove_auth::Identity {
            tenant_id: String::new(),
            subject: "synthetic".into(),
            username: "fixture@example.invalid".into(),
            graph_user_id: String::new(),
            display_name: "Fixture".into(),
        },
        credential_id: uuid::Uuid::new_v4().to_string(),
        access: AccessMode::ReadWrite,
        drive: cirrove_core::CollectionInfo {
            id: "drive".into(),
            name: "Fixture".into(),
            drive_type: "icloud_drive".into(),
            web_url: "https://example.invalid".into(),
        },
        root_id: "FOLDER::com.apple.CloudDocs::root".into(),
        mount_path,
        enabled: true,
        poll_seconds: 3600,
        cache_bytes: 64 * 1024 * 1024,
    }
}
async fn running(
    manager: &Manager,
    task: &tokio::task::JoinHandle<()>,
    label: &str,
) -> Result<Arc<Engine>> {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            anyhow::ensure!(
                !task.is_finished(),
                "Manager exited before publishing a running account"
            );
            if let Ok(engine) = manager.engine(label).await {
                let statuses = manager.status.read().await;
                if statuses.iter().any(|status| {
                    status.account_id == engine.account.id
                        && status.label == label
                        && status.recovery_only
                        && status.local_recovery
                        && status.enabled
                        && status.unconfirmed_changes == 1
                }) {
                    return Ok(engine);
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?
}
async fn retired_owner(
    manager: &Manager,
    state: &Path,
    account: &Account,
) -> Result<std::fs::File> {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if manager.engine(&account.label).await.is_err()
                && let Ok(owner) =
                    crate::accounts::account_lock(&state.join("accounts").join(&account.id))
            {
                return Ok::<_, anyhow::Error>(owner);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?
}
async fn completed(engine: &Engine, id: &str) -> Result<crate::jobs::Job> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(job) = engine.jobs.find(id)
                && !job.running()
            {
                return Ok(job);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?
}
async fn readonly(manager: &Manager, engine: &Arc<Engine>, account: &Account) -> Result<()> {
    anyhow::ensure!(
        engine.account.access == AccessMode::ReadOnly,
        "recovery-only effective grant escaped read-only"
    );
    let mut effective = account.clone();
    effective.access = AccessMode::ReadOnly;
    anyhow::ensure!(
        serde_json::to_value(&engine.account)? == serde_json::to_value(effective)?,
        "effective account changed identity/session/preferences"
    );
    let statuses = manager.status.read().await;
    let status = statuses
        .iter()
        .find(|s| s.account_id == account.id)
        .context("account status")?;
    anyhow::ensure!(
        status.recovery_only && status.local_recovery && status.enabled,
        "recovery-only status did not truthfully expose local recovery"
    );
    anyhow::ensure!(
        status.unconfirmed_changes == 1,
        "uncertain retained upload disappeared from status"
    );
    drop(statuses);
    anyhow::ensure!(
        manager.writers.read().await.is_empty(),
        "recovery-only exposed a writer"
    );
    anyhow::ensure!(
        !native_import::eligible(engine),
        "recovery-only admitted native mutation"
    );
    anyhow::ensure!(
        manager.retry_stuck(&account.label).await.is_err()
            && manager.discard_stuck(&account.label).await.is_err()
            && manager
                .delete_permanently(&account.label, &["private.txt".into()], true)
                .await
                .is_err(),
        "recovery-only admitted cloud or journal mutation"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recovery_only_manager_preserves_rw_retained_bytes_across_enable_and_heal() -> Result<()> {
    let root = tempfile::tempdir()?.keep(); // Retain controlled evidence; no recursive cleanup.
    let state = root.join("state");
    crate::private_dir(&state)?;
    let mount = root.join("mount");
    crate::private_dir(&mount)?;
    std::fs::write(mount.join("sentinel"), b"no-FUSE")?;
    let account = account(mount.clone());
    let settings = Settings {
        version: 2,
        accounts: vec![account.clone()],
    };
    settings.validate()?;
    let mut settings_file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(state.join("accounts.json"))?;
    std::io::Write::write_all(&mut settings_file, &serde_json::to_vec(&settings)?)?;
    settings_file.sync_all()?;
    drop(settings_file);
    let before_settings = std::fs::read(state.join("accounts.json"))?;
    let session = state.join("synthetic-opaque-session");
    std::fs::write(&session, b"opaque fixture; not a credential")?;
    std::fs::set_permissions(&session, std::fs::Permissions::from_mode(0o600))?;
    let before_session = std::fs::read(&session)?;
    let provider = Arc::new(LocalOnly::default());
    let seed = Engine::new(account.clone(), provider.clone(), state.clone()).await?;
    let journal_root = seed
        .db
        .parent()
        .context("account directory")?
        .join("journal");
    let mut journal = UploadJournal::open(&journal_root, &account.id, account.cache_bytes)?;
    // A genuine completed handoff leaves a due metadata publication. Accidental
    // ordinary repair can acknowledge it or change retry/failure state even while
    // every sealed/dirty upload remains intact; the sentinel does not block this.
    let original = Node {
        id: "publication-original".into(),
        parent_id: Some(account.root_id.clone()),
        name: "Completed.txt".into(),
        kind: NodeKind::File,
        package: false,
        size: 3,
        modified_unix: 0,
        etag: Some("old-v1".into()),
        content_version: Some("old-content".into()),
        target: None,
    };
    let completed_working =
        journal.create_working(seed.scope("drive"), original.clone(), false, &b"old"[..])?;
    journal.write_working(completed_working.id, 0, b"new")?;
    let uploaded = journal
        .seal_working(completed_working.id)?
        .context("handoff sealed generation")?;
    let upload_attempt = journal.claim_next()?.context("handoff actual claim")?;
    anyhow::ensure!(
        upload_attempt.id == uploaded.id,
        "wrong completed handoff claim"
    );
    let attempt = upload_attempt.attempt.context("handoff attempt")?;
    journal.reserve_identity_handoff(
        uploaded.id,
        attempt,
        RecoveryLocation::Sibling {
            name: "recovery-A.txt".into(),
        },
    )?;
    journal.acknowledge_identity_handoff(
        uploaded.id,
        attempt,
        Node {
            id: "publication-current".into(),
            etag: Some("new-v2".into()),
            content_version: Some("new-content".into()),
            ..original.clone()
        },
        Node {
            name: "recovery-A.txt".into(),
            etag: Some("backup-v2".into()),
            ..original
        },
    )?;
    anyhow::ensure!(
        journal.get(uploaded.id)?.state == UploadState::Uploaded,
        "handoff acknowledgement missing"
    );
    let working = journal.create_working(
        seed.scope("drive"),
        Node {
            id: "retained-item".into(),
            parent_id: Some(account.root_id.clone()),
            name: "private.txt".into(),
            kind: NodeKind::File,
            package: false,
            size: 6,
            modified_unix: 1,
            etag: Some("original-revision".into()),
            content_version: None,
            target: None,
        },
        false,
        &b"sealed"[..],
    )?;
    journal.write_working(working.id, 0, b"sealed")?;
    let saved = journal
        .seal_working(working.id)?
        .context("sealed working generation")?;
    let claimed = journal.claim_next()?.context("real saved attempt")?;
    anyhow::ensure!(claimed.id == saved.id, "claimed wrong fixture operation");
    journal.stop_attempt(
        saved.id,
        claimed.attempt.context("actual attempt")?,
        UploadState::VerifyRequired,
    )?;
    journal.write_working(working.id, 0, b"latest")?;
    let dirty = journal.working_file(working.id)?;
    anyhow::ensure!(
        dirty.dirty && dirty.latest == Some(saved.id),
        "dirty retained generation"
    );
    anyhow::ensure!(
        journal.get(saved.id)?.state == UploadState::VerifyRequired,
        "uncertain actual operation"
    );
    drop(journal);
    drop(seed);
    // Isolated fixture deliberately uses DELETE after seeding: SQLite read-only
    // coordination cannot change WAL/SHM bytes and obscure full-file equality.
    // This is not a production migration or a WAL-mode acceptance claim.
    let db = rusqlite::Connection::open(journal_root.join("uploads.db"))?;
    let mode: String = db.pragma_update_and_check(None, "journal_mode", "DELETE", |r| r.get(0))?;
    anyhow::ensure!(mode == "delete", "fixture journal mode not admitted");
    drop(db);
    let before_publication = publication(&journal_root, uploaded.id)?;
    anyhow::ensure!(
        before_publication.1 == 0
            && before_publication.2 == 0
            && before_publication.3 == 0
            && before_publication.6 == "uploaded"
            && before_publication.5 == before_publication.7
            && before_publication.8 == 1,
        "completed due publication fixture not admitted"
    );
    let proof: crate::journal::OrdinaryHandoffMetadata =
        serde_json::from_str(&before_publication.0)?;
    anyhow::ensure!(
        proof.operation == uploaded.id
            && proof.scope == uploaded.scope
            && !proof.native
            && proof.original.id == "publication-original"
            && proof.current.id == "publication-current"
            && proof.backup.id == "publication-original",
        "completed publication receipt authority"
    );
    let before = snapshot(&journal_root)?;
    let writes_called = Arc::new(AtomicUsize::new(0));
    let called = writes_called.clone();
    let writes: WriteFactory = Arc::new(move |_, _| {
        called.fetch_add(1, Ordering::SeqCst);
        panic!("recovery-only WriteFactory must never run")
    });
    let launches = Arc::new(AtomicUsize::new(0));
    let read_launches = launches.clone();
    let reads = provider.clone();
    let cancel = CancellationToken::new();
    let (manager, task) = Manager::start_with_providers_mode(
        state.clone(),
        cancel.clone(),
        ProviderSelection::Injected(Arc::new(move |a| {
            anyhow::ensure!(
                a.access == AccessMode::ReadWrite,
                "read provider lost saved grant"
            );
            read_launches.fetch_add(1, Ordering::SeqCst);
            Ok(reads.clone())
        })),
        Some(writes),
        true,
    );
    let outcome: Result<()> = async {
        let engine = running(&manager, &task, &account.label).await?;
        readonly(&manager, &engine, &account).await?;
        let job = manager
            .export_save(&crate::ExportSaveRequest {
                label: account.label.clone(),
                operation: saved.id,
                destination: root.join("sealed-export"),
            })
            .await?;
        let finished = completed(&engine, &job.id).await?;
        anyhow::ensure!(
            finished.state == crate::jobs::JobState::Succeeded && finished.issue.is_none(),
            "sealed recovery job failed"
        );
        let receipt = finished.export.context("sealed recovery receipt")?;
        anyhow::ensure!(
            receipt.operation == saved.id
                && receipt.sha256 == saved.sha256
                && receipt.size == 6
                && receipt.destination == root.join("sealed-export")
                && std::fs::read(&receipt.destination)? == b"sealed",
            "sealed recovery parity"
        );
        let request = crate::ExportWorkingRequest {
            label: account.label.clone(),
            file: dirty.id,
            generation: dirty.generation,
            destination: root.join("dirty-export"),
        };
        let job = manager.export_working(&request).await?;
        let finished = completed(&engine, &job.id).await?;
        let receipt = request
            .confirmed_receipt(&job, &finished)
            .context("working recovery receipt")?;
        anyhow::ensure!(
            receipt.sha256 == hex::encode(sha2::Sha256::digest(b"latest"))
                && std::fs::read(&request.destination)? == b"latest",
            "dirty recovery parity"
        );
        anyhow::ensure!(
            std::fs::read(state.join("accounts.json"))? == before_settings,
            "startup/export changed desired settings"
        );
        drop(engine);
        crate::accounts::set_enabled(&state, &account.label, false)?;
        drop(retired_owner(&manager, &state, &account).await?);
        crate::accounts::set_enabled(&state, &account.label, true)?;
        let engine = running(&manager, &task, &account.label).await?;
        readonly(&manager, &engine, &account).await?;
        drop(engine);
        // A live sign-in owns this operation lock; healing waits for its release.
        crate::accounts::set_enabled(&state, &account.label, false)?;
        let operation = crate::accounts::account_operation(&state, &account.id)?;
        let marker = crate::accounts::restore_marker(&state, &account.id);
        std::fs::write(&marker, b"{\"enabled\":true}")?;
        std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o600))?;
        drop(retired_owner(&manager, &state, &account).await?);
        anyhow::ensure!(
            !Settings::load(&state)?.accounts[0].enabled && marker.exists(),
            "healer overrode live sign-in"
        );
        drop(operation);
        let engine = running(&manager, &task, &account.label).await?;
        readonly(&manager, &engine, &account).await?;
        anyhow::ensure!(!marker.exists(), "interrupted sign-in was not healed");
        // The existing healer restores enabled, not access or the session.
        anyhow::ensure!(
            serde_json::to_value(&Settings::load(&state)?.accounts[0])?
                == serde_json::to_value(&account)?,
            "healing changed desired access/session/identity"
        );
        anyhow::ensure!(
            launches.load(Ordering::SeqCst) == 3,
            "unexpected or missing Manager relaunch"
        );
        Ok(())
    }
    .await;
    cancel.cancel();
    // Join even when an assertion precursor returned an error; a counterfactual
    // panic is also consumed before the desired WriteFactory-count assertion.
    let joined = tokio::time::timeout(Duration::from_secs(10), task).await;
    assert_eq!(
        writes_called.load(Ordering::SeqCst),
        0,
        "recovery-only must not invoke WriteFactory"
    );
    joined.context("Manager did not join")??;
    outcome?;
    anyhow::ensure!(
        manager.engines.read().await.is_empty() && manager.writers.read().await.is_empty(),
        "Manager retained running owners"
    );
    let owner = retired_owner(&manager, &state, &account).await?;
    assert_eq!(
        publication(&journal_root, uploaded.id)?,
        before_publication,
        "recovery-only remount must not repair retained metadata publication"
    );
    assert_eq!(
        provider.publication_reads.load(Ordering::SeqCst),
        0,
        "recovery-only dispatched retained metadata publication read"
    );
    assert_eq!(
        snapshot(&journal_root)?,
        before,
        "recovery-only changed full journal/retained files"
    );
    assert_eq!(std::fs::read(&session)?, before_session);
    assert_eq!(
        serde_json::to_value(&Settings::load(&state)?.accounts[0])?,
        serde_json::to_value(account)?
    );
    assert_eq!(std::fs::read(mount.join("sentinel"))?, b"no-FUSE");
    assert_eq!(provider.content_reads.load(Ordering::SeqCst), 0);
    drop(owner);
    // Exclusive read-only recovery can reopen after the joined Manager releases.
    drop(crate::journal::RecoveryJournal::open(
        &journal_root,
        &Settings::load(&state)?.accounts[0].id,
    )?);
    Ok(())
}
