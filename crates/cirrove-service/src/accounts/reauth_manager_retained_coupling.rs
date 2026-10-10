// Couples local reauthentication completion to the real Manager lifecycle.
// Synthetic session-save outcomes; no keyring, HTTP, WriteFactory or FUSE success.
use super::*;
use crate::journal::UploadState;
use cirrove_core::{
    ChangePage, Cursor, DirectoryPage, MetadataProvider, Node, NodeKind, ProviderError,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[derive(Default)]
struct LocalOnly {
    content_reads: AtomicUsize,
}
#[async_trait::async_trait]
impl MetadataProvider for LocalOnly {
    fn provider_id(&self) -> &'static str {
        "icloud"
    }
    async fn changes(
        &self,
        _: &Scope,
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
        panic!("local reauthentication/recovery must not fetch content")
    }
}
type RetainedRows = Vec<(String, Vec<Vec<rusqlite::types::Value>>)>;

fn rows(path: &Path) -> Result<RetainedRows> {
    let db =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.execute_batch("PRAGMA query_only=ON; BEGIN")?;
    let mut query=db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")?;
    let tables = query
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(query);
    let mut out = Vec::new();
    for table in tables {
        let mut q = db.prepare(&format!(
            "SELECT * FROM \"{}\" ORDER BY rowid",
            table.replace('"', "\"\"")
        ))?;
        let width = q.column_count();
        let values = q
            .query_map([], |r| {
                (0..width)
                    .map(|c| r.get::<_, rusqlite::types::Value>(c))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        out.push((table, values));
    }
    db.execute_batch("COMMIT")?;
    Ok(out)
}
async fn engine_for(
    manager: &crate::manager::Manager,
    label: &str,
    access: AccessMode,
) -> Result<Arc<crate::engine::Engine>> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(engine) = manager.engine(label).await
                && engine.account.access == access
                && engine.account.enabled
            {
                return Ok::<_, anyhow::Error>(engine);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?
}
async fn finished(engine: &crate::engine::Engine, id: &str) -> Result<crate::jobs::Job> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(job) = engine.jobs.find(id)
                && !job.running()
            {
                return Ok::<_, anyhow::Error>(job);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?
}
// A withdrawn engine/expired Weak does not prove its File fields have closed.
// Match reauthentication's existing bounded acquisition of the actual owner.
async fn retired_account_owner<F, Fut>(directory: &Path, mut retired: F) -> Result<std::fs::File>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if retired().await
                && let Ok(owner) = account_lock(directory)
            {
                return Ok::<_, anyhow::Error>(owner);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .context("actual account owner did not release within the retirement bound")?
}
async fn coupled(session_saved: bool) -> Result<()> {
    let root = tempfile::tempdir()?.keep(); // Retain synthetic evidence on failure; no recursive cleanup.
    let state = root.join("state");
    crate::private_dir(&state)?;
    let mount = root.join("mount");
    crate::private_dir(&mount)?;
    std::fs::write(mount.join("sentinel"), b"no-FUSE-no-kernel-mount")?;
    let mut account = icloud_reauth_fixture();
    account.id = uuid::Uuid::new_v4().to_string();
    account.credential_id = uuid::Uuid::new_v4().to_string();
    account.access = AccessMode::ReadWrite;
    account.mount_path = mount.clone();
    account.poll_seconds = 3600;
    Settings {
        version: 2,
        accounts: vec![account.clone()],
    }
    .save(&state)?;
    let provider = Arc::new(LocalOnly::default());
    let seed = crate::engine::Engine::new(account.clone(), provider.clone(), state.clone()).await?;
    let journal_root = seed
        .db
        .parent()
        .context("account directory")?
        .join("journal");
    let scope = seed.scope("drive");
    let mut journal =
        crate::journal::UploadJournal::open(&journal_root, &account.id, account.cache_bytes)?;
    let working = journal.create_working(
        scope,
        Node {
            id: "FILE::com.apple.CloudDocs::synthetic-retained".into(),
            parent_id: Some(account.root_id.clone()),
            name: "retained.txt".into(),
            kind: NodeKind::File,
            package: false,
            size: 0,
            modified_unix: 1,
            etag: Some("synthetic-revision-1".into()),
            content_version: None,
            target: None,
        },
        false,
        &b""[..],
    )?;
    journal.write_working(working.id, 0, b"sealed before reauthentication")?;
    let sealed = journal
        .seal_working(working.id)?
        .context("sealed actual working generation")?;
    let claimed = journal.claim_next()?.context("active saved attempt")?;
    assert_eq!(claimed.id, sealed.id);
    journal.write_working(working.id, 0, b"dirty generation after sealing")?;
    let dirty = journal.working_file(working.id)?;
    assert_eq!(claimed.state, UploadState::Uploading);
    assert!(claimed.attempt.is_some());
    assert!(dirty.dirty && dirty.generation > working.generation);
    assert_eq!(dirty.latest, Some(sealed.id));
    assert_eq!(journal.get(sealed.id)?.attempt, claimed.attempt);
    let sealed_path = journal_root.join("objects").join(sealed.id.to_string());
    let dirty_path = journal_root.join("working").join(dirty.id.to_string());
    let sealed_bytes = std::fs::read(&sealed_path)?;
    let dirty_bytes = std::fs::read(&dirty_path)?;
    let before_rows = rows(&journal_root.join("uploads.db"))?;
    let db = rusqlite::Connection::open_with_flags(
        journal_root.join("uploads.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let schema: u32 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
    drop(db);
    drop(journal);
    drop(seed);
    let launches = Arc::new(AtomicUsize::new(0));
    let called = launches.clone();
    let reads = provider.clone();
    let cancel = CancellationToken::new();
    // Same real Manager API as production, effective RO because no WriteFactory.
    let (manager, task) = crate::manager::Manager::start_with_provider(
        state.clone(),
        cancel.clone(),
        Arc::new(move |_| {
            called.fetch_add(1, Ordering::SeqCst);
            Ok(reads.clone())
        }),
    );
    let outcome:Result<()>=async {
        let old=engine_for(&manager,&account.label,AccessMode::ReadWrite).await?;
        // Establish actual account ownership before testing its release.
        anyhow::ensure!(account_lock(&state.join("accounts").join(&account.id)).is_err(),
            "initial Manager engine did not retain the account owner");
        let old_weak=Arc::downgrade(&old);drop(old);
        let pending=begin_reauthenticate_icloud(state.clone(),account.label.clone(),Some(AccessMode::ReadOnly))?;
        anyhow::ensure!(pending.account_id()==account.id,"pending account identity changed");
        anyhow::ensure!(pending.access()==AccessMode::ReadOnly,"pending access changed");
        let key=probe_session_key(&account.identity.username)?;
        let snapshot=SecretString::from(serde_json::json!({"version":1,
            "account_hash":key.strip_prefix("icloud-probe-").context("synthetic account hash")?,
            "headers":{"scnt":"","session_id":"","session_token":"synthetic-not-a-credential","trust_token":"","account_country":"","auth_attributes":""},
            "drive_endpoint":"https://synthetic.icloud.com/","docs_endpoint":"https://synthetic.icloud.com/","cookies":[]}).to_string());
        pending.validate_snapshot(&snapshot)?;
        let (_operation,desired)=suspend_validated_icloud_account(&pending.state,&pending.original)?;
        anyhow::ensure!(!Settings::load(&state)?.accounts[0].enabled,"suspended account enabled before completion");
        // Keep the actual operation lock held; Manager's healer must not
        // undo this live sign-in's disable or hold the retired account owner.
        let owner=retired_account_owner(&state.join("accounts").join(&account.id),||async {
            old_weak.upgrade().is_none() && manager.engine(&account.label).await.is_err()
        }).await.context("retired Manager account owner acquisition")?;
        anyhow::ensure!(!Settings::load(&state)?.accounts[0].enabled,"suspended account enabled before completion");
        anyhow::ensure!(rows(&journal_root.join("uploads.db"))?==before_rows,
            "Manager retirement normalized retained active authority");
        anyhow::ensure!(std::fs::read(&sealed_path)?==sealed_bytes
            && std::fs::read(&dirty_path)?==dirty_bytes,
            "Manager retirement changed sealed or dirty bytes");
        drop(owner);
        // No fake successful session/provider call: only the persistence
        // result supplied to this existing local completion helper is synthetic.
        let result=complete_icloud_session_save(desired,pending.requested,
            if session_saved {Ok(())} else {Err(anyhow::anyhow!("synthetic save refusal"))});
        anyhow::ensure!(result.is_ok()==session_saved,"local session completion result changed");
        let expected=if session_saved {AccessMode::ReadOnly} else {AccessMode::ReadWrite};
        let mut wanted=account.clone();wanted.access=expected;
        anyhow::ensure!(serde_json::to_value(&Settings::load(&state)?.accounts[0])?==serde_json::to_value(&wanted)?,"restored account fields changed");
        anyhow::ensure!(!restore_marker(&state,&account.id).exists(),"restore marker not settled");
        let current=engine_for(&manager,&account.label,expected).await?;
        anyhow::ensure!(current.account.id==account.id && current.account.credential_id==account.credential_id,"relaunched account or credential changed");
        anyhow::ensure!(current.scope("drive")==Scope{account:account.id.clone(),provider:"icloud".into(),collection:"drive".into()},"relaunched scope changed");
        anyhow::ensure!(account_lock(&state.join("accounts").join(&account.id)).is_err(),
            "relaunched Manager engine did not retain the account owner");
        if session_saved {
            let saved=manager.export_save(&crate::ExportSaveRequest{label:account.label.clone(),operation:sealed.id,destination:root.join("sealed-export")}).await?;
            let completed=finished(&current,&saved.id).await?;
            anyhow::ensure!(saved.kind==crate::jobs::JobKind::ExportLocal
                && completed.kind==crate::jobs::JobKind::ExportLocal
                && completed.id==saved.id && completed.state==crate::jobs::JobState::Succeeded
                && completed.issue.is_none() && completed.working_export.is_none(),
                "sealed public export did not complete successfully");
            let receipt=completed.export.context("sealed export receipt")?;
            anyhow::ensure!(receipt.operation==sealed.id && receipt.sha256==sealed.sha256
                && receipt.size==sealed.size && receipt.destination==root.join("sealed-export"),
                "sealed receipt changed");
            anyhow::ensure!(std::fs::read(root.join("sealed-export"))?==sealed_bytes,"sealed export bytes changed");
            let request=crate::ExportWorkingRequest{label:account.label.clone(),file:dirty.id,generation:dirty.generation,destination:root.join("dirty-export")};
            let job=manager.export_working(&request).await?;
            let completed=finished(&current,&job.id).await?;
            anyhow::ensure!(request.confirmed_receipt(&job,&completed).is_some(),"working export receipt not bound");
            anyhow::ensure!(std::fs::read(&request.destination)?==dirty_bytes,"working export bytes changed");
        } else {
            // Configured RW without a WriteFactory has no public recovery writer.
            // Effective RO mounting does not grant the configured-RO recovery path.
            let save=crate::ExportSaveRequest{label:account.label.clone(),operation:sealed.id,destination:root.join("sealed-export")};
            let denied=manager.export_save(&save).await.err().context("configured RW unexpectedly allowed public sealed export")?;
            anyhow::ensure!(denied.to_string()=="this account has no active saved-change journal","public sealed refusal policy changed");
            let request=crate::ExportWorkingRequest{label:account.label.clone(),file:dirty.id,generation:dirty.generation,destination:root.join("dirty-export")};
            let denied=manager.export_working(&request).await.err().context("configured RW unexpectedly allowed public working export")?;
            anyhow::ensure!(denied.to_string()=="this account has no active saved-change journal","public working refusal policy changed");
            anyhow::ensure!(!save.destination.exists() && !request.destination.exists(),"refused public export created a destination");
        }
        anyhow::ensure!(launches.load(Ordering::SeqCst)==2,"old engine was reused or a foreign context launched");
        Ok(())
    }.await;
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(10), task).await??;
    outcome?;
    let owner = retired_account_owner(&state.join("accounts").join(&account.id), || async { true })
        .await
        .context("joined Manager account owner acquisition")?;
    if !session_saved {
        // After the original Manager joins, inspect/export explicitly under
        // the existing read-only owner lease; this is not a public RW job.
        let recovery = crate::journal::RecoveryJournal::open(&journal_root, &account.id)?;
        let receipt = recovery.local_export_source(sealed.id)?.copy_to(
            &root.join("sealed-export"),
            &CancellationToken::new(),
            |_| {},
        )?;
        assert_eq!(receipt.operation, sealed.id);
        assert_eq!(receipt.sha256, sealed.sha256);
        assert_eq!(std::fs::read(root.join("sealed-export"))?, sealed_bytes);
        let receipt = recovery.export_working(
            dirty.id,
            dirty.generation,
            &root.join("dirty-export"),
            &CancellationToken::new(),
            |_| {},
        )?;
        assert_eq!(receipt.source.file, dirty.id);
        assert_eq!(receipt.source.generation, dirty.generation);
        assert_eq!(std::fs::read(root.join("dirty-export"))?, dirty_bytes);
        drop(recovery);
    }
    assert_eq!(
        rows(&journal_root.join("uploads.db"))?,
        before_rows,
        "reauth/remount normalized or replayed the active saved attempt"
    );
    let db = rusqlite::Connection::open_with_flags(
        journal_root.join("uploads.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))?,
        schema,
        "reauth migrated retained authority"
    );
    assert_eq!(std::fs::read(sealed_path)?, sealed_bytes);
    assert_eq!(std::fs::read(dirty_path)?, dirty_bytes);
    assert_eq!(
        std::fs::read(mount.join("sentinel"))?,
        b"no-FUSE-no-kernel-mount"
    );
    assert_eq!(provider.content_reads.load(Ordering::SeqCst), 0);
    drop(owner);
    Ok(())
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn icloud_reauth_manager_downgrade_keeps_active_sealed_and_dirty_exports() -> Result<()> {
    coupled(true).await
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn icloud_reauth_manager_failed_save_restores_mode_and_retained_exports() -> Result<()> {
    coupled(false).await
}

#[tokio::test]
async fn icloud_reauth_manager_owner_acquisition_waits_for_actual_release() -> Result<()> {
    use std::future::Future;
    use std::task::{Poll, Waker};

    let root = tempfile::tempdir()?.keep();
    let directory = root.join("account");
    crate::private_dir(&directory)?;
    let account = uuid::Uuid::new_v4().to_string();
    let journal_root = directory.join("journal");
    let mut journal = crate::journal::UploadJournal::open(&journal_root, &account, 1024 * 1024)?;
    let working = journal.create_working(
        Scope { account, provider: "icloud".into(), collection: "drive".into() },
        Node {
            id: "FILE::com.apple.CloudDocs::synthetic-owner-retention".into(),
            parent_id: Some("FOLDER::com.apple.CloudDocs::root".into()),
            name: "retained.txt".into(),
            kind: NodeKind::File,
            package: false,
            size: 0,
            modified_unix: 1,
            etag: Some("synthetic-revision-1".into()),
            content_version: None,
            target: None,
        },
        false,
        &b""[..],
    )?;
    journal.write_working(working.id, 0, b"sealed owner retention")?;
    let sealed = journal.seal_working(working.id)?.context("sealed actual working generation")?;
    let active = journal.claim_next()?.context("actual active saved attempt")?;
    assert_eq!(active.id, sealed.id);
    assert_eq!(active.state, UploadState::Uploading);
    assert!(active.attempt.is_some());
    journal.write_working(working.id, 0, b"dirty owner retention")?;
    let dirty = journal.working_file(working.id)?;
    assert!(dirty.dirty);
    assert_eq!(dirty.latest, Some(sealed.id));
    drop(journal);
    let before_rows = rows(&journal_root.join("uploads.db"))?;
    let sealed_path = journal_root.join("objects").join(sealed.id.to_string());
    let dirty_path = journal_root.join("working").join(dirty.id.to_string());
    let sealed_bytes = std::fs::read(&sealed_path)?;
    let dirty_bytes = std::fs::read(&dirty_path)?;
    let held = account_lock(&directory)?;

    // Controlled boundary: retirement predicates have passed, but the real
    // filesystem lease is still held. No scheduler sleep supplies the proof.
    let mut acquiring = Box::pin(retired_account_owner(&directory, || async { true }));
    let first = acquiring.as_mut().poll(&mut std::task::Context::from_waker(Waker::noop()));
    assert!(matches!(first, Poll::Pending),
        "actual account-owner acquisition must remain pending while the lease is held");
    assert_eq!(rows(&journal_root.join("uploads.db"))?, before_rows);
    assert_eq!(std::fs::read(&sealed_path)?, sealed_bytes);
    assert_eq!(std::fs::read(&dirty_path)?, dirty_bytes);

    drop(held);
    let acquired = tokio::time::timeout(Duration::from_secs(5), acquiring)
        .await.context("actual owner acquisition after controlled release")??;
    assert!(account_lock(&directory).is_err(), "returned guard must own the actual filesystem lease");
    assert_eq!(rows(&journal_root.join("uploads.db"))?, before_rows);
    assert_eq!(std::fs::read(&sealed_path)?, sealed_bytes);
    assert_eq!(std::fs::read(&dirty_path)?, dirty_bytes);
    drop(acquired);
    let released = account_lock(&directory)?;
    drop(released);
    Ok(())
}
