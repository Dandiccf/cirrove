//! A recorded native removal must finish metadata publication on effective-RO startup.
use super::*;
use cirrove_core::mutation::{
    MutationProvider, MutationReceipt, MutationReconciliation, MutationRequest,
};
use cirrove_core::{
    Change, ChangePage, Checkpoint, Cursor, DirectoryPage, MetadataProvider, Node, NodeKind,
    ProviderError, Scope,
};
use cirrove_store::Store;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::sync::atomic::{AtomicUsize, Ordering};

struct OrderedAbsence {
    scope: Scope,
    item: String,
    exact_reads: AtomicUsize,
    content_reads: AtomicUsize,
    mutations: AtomicUsize,
}

#[async_trait::async_trait]
impl MetadataProvider for OrderedAbsence {
    fn provider_id(&self) -> &'static str {
        "icloud"
    }

    async fn changes(
        &self,
        scope: &Scope,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<ChangePage, ProviderError> {
        assert_eq!(scope, &self.scope);
        // An unrelated feed cannot provide the desired operation acknowledgement.
        Err(ProviderError::Unavailable)
    }
}

#[async_trait::async_trait]
impl ReadProvider for OrderedAbsence {
    async fn node(
        &self,
        scope: &Scope,
        item: &str,
        _: &CancellationToken,
    ) -> std::result::Result<Node, ProviderError> {
        assert_eq!(scope, &self.scope);
        if item != self.item {
            return Err(ProviderError::Unavailable);
        }
        self.exact_reads.fetch_add(1, Ordering::SeqCst);
        // The production refresh acquires its observation ticket before this await.
        tokio::task::yield_now().await;
        Err(ProviderError::NotFound)
    }

    async fn children(
        &self,
        scope: &Scope,
        _: &str,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<DirectoryPage, ProviderError> {
        assert_eq!(scope, &self.scope);
        // Preserve the cached complete directory: only the exact-ID publication
        // path may acknowledge absence, not a conveniently empty listing.
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
        panic!("metadata-only startup must not download native content")
    }
}

#[async_trait::async_trait]
impl MutationProvider for OrderedAbsence {
    async fn mutate(
        &self,
        _: &MutationRequest,
        _: &CancellationToken,
    ) -> cirrove_core::mutation::Result<MutationReceipt> {
        self.mutations.fetch_add(1, Ordering::SeqCst);
        panic!("read-only restart must not repeat the recorded native removal")
    }

    async fn reconcile_mutation(
        &self,
        _: &MutationRequest,
        _: &CancellationToken,
    ) -> cirrove_core::mutation::Result<MutationReconciliation> {
        self.mutations.fetch_add(1, Ordering::SeqCst);
        panic!("metadata-only startup must not reconcile provider mutations")
    }
}

type RetainedRows = Vec<(String, Vec<Vec<rusqlite::types::Value>>)>;
type SchemaSnapshot = (i64, Vec<(String, String, Option<String>)>);

fn retained_rows(path: &Path) -> Result<RetainedRows> {
    let db =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut tables = db.prepare(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'
         AND name!='native_trash_metadata_publication' ORDER BY name",
    )?;
    let names = tables
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut result = Vec::new();
    for name in names {
        let sql = format!(
            "SELECT * FROM \"{}\" ORDER BY rowid",
            name.replace('"', "\"\"")
        );
        let mut statement = db.prepare(&sql)?;
        let columns = statement.column_count();
        let rows = statement
            .query_map([], |row| {
                (0..columns)
                    .map(|column| row.get::<_, rusqlite::types::Value>(column))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        result.push((name, rows));
    }
    Ok(result)
}

fn schema(path: &Path) -> Result<SchemaSnapshot> {
    let db =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let version = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let mut statement = db.prepare("SELECT type,name,sql FROM sqlite_master ORDER BY type,name")?;
    let objects = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((version, objects))
}

async fn native_trash_startup(access: cirrove_auth::AccessMode, observer_only: bool) -> Result<()> {
    use crate::journal::{MutationState, UploadJournal, UploadState};
    use cirrove_core::mutation::MutationIntent;

    let root = tempfile::tempdir_in("/var/tmp")?.keep();
    eprintln!(
        "retained native Trash read-only startup fixture: {}",
        root.display()
    );
    let state = root.join("state");
    crate::private_dir(&state)?;
    let mount = root.join("mount");
    crate::private_dir(&mount)?;
    // Exercise the real Manager launch without entering FUSE or running a writer.
    let sentinel = mount.join("local-sentinel");
    std::fs::write(&sentinel, b"preserve")?;
    let account = Account {
        id: uuid::Uuid::new_v4().to_string(),
        label: "native-trash-readonly-fixture".into(),
        registration: cirrove_auth::AppRegistration::ICloud,
        identity: cirrove_auth::Identity {
            tenant_id: String::new(),
            subject: "synthetic".into(),
            username: "fixture@example.invalid".into(),
            graph_user_id: String::new(),
            display_name: "Fixture".into(),
        },
        credential_id: uuid::Uuid::new_v4().to_string(),
        access,
        drive: cirrove_core::CollectionInfo {
            id: "drive".into(),
            name: "Fixture".into(),
            drive_type: "icloud_drive".into(),
            web_url: "https://example.invalid".into(),
        },
        root_id: cirrove_icloud::ROOT_ID.into(),
        mount_path: mount,
        enabled: true,
        poll_seconds: 3600,
        cache_bytes: 64 * 1024 * 1024,
    };
    let scope = Scope {
        account: account.id.clone(),
        provider: "icloud".into(),
        collection: "drive".into(),
    };
    let original = Node {
        id: "FILE::com.apple.CloudDocs::owned-native-trash".into(),
        parent_id: Some(account.root_id.clone()),
        name: "Owned.pages".into(),
        kind: NodeKind::Folder,
        size: 17,
        modified_unix: 1,
        etag: Some("original-E1".into()),
        content_version: Some("original-E1".into()),
        target: None,
        package: true,
    };
    let provider = Arc::new(OrderedAbsence {
        scope: scope.clone(),
        item: original.id.clone(),
        exact_reads: AtomicUsize::new(0),
        content_reads: AtomicUsize::new(0),
        mutations: AtomicUsize::new(0),
    });
    let engine = Engine::new(account.clone(), provider.clone(), state.clone()).await?;
    let metadata = engine.db.clone();
    let journal_root = metadata
        .parent()
        .context("account directory")?
        .join("journal");
    let db_path = journal_root.join("uploads.db");
    let mut journal = UploadJournal::open(&journal_root, &account.id, account.cache_bytes)?;
    let native = journal.enqueue_mutation(MutationRequest {
        scope: scope.clone(),
        intent: MutationIntent::TrashNativeDocument {
            before: original.clone(),
        },
    })?;
    let claimed = journal
        .claim_mutation()?
        .context("claimed native removal")?;
    assert_eq!(claimed.id, native.id);
    journal.acknowledge_mutation(
        native.id,
        claimed.attempt.context("native removal attempt")?,
        MutationReceipt::Removed {
            item: original.id.clone(),
        },
    )?;
    let completed = journal.mutation(native.id)?;
    assert_eq!(completed.state, MutationState::Applied);
    assert!(
        matches!(&completed.receipt, Some(MutationReceipt::Removed { item }) if item == &original.id)
    );
    assert_eq!(
        journal.native_trash_publication_status(native.id)?,
        crate::journal::PackagePublicationStatus::Pending
    );

    // A separate ordinary stream remains genuinely Uploading with immutable
    // sealed bytes and newer dirty working bytes. Startup may not normalize it.
    let ordinary = Node {
        id: "FILE::com.apple.CloudDocs::unrelated-working".into(),
        name: "retained.txt".into(),
        kind: NodeKind::File,
        package: false,
        size: 3,
        ..original.clone()
    };
    let working = journal.create_working(scope.clone(), ordinary, false, &b"old"[..])?;
    journal.write_working(working.id, 0, b"sealed-save")?;
    let upload = journal
        .seal_working(working.id)?
        .context("sealed unrelated save")?;
    let active = journal.claim_next()?.context("claimed unrelated save")?;
    assert_eq!(active.id, upload.id);
    assert_eq!(active.state, UploadState::Uploading);
    journal.write_working(working.id, 0, b"newer-dirty-working")?;
    let source = root.join("original-source.bin");
    let checkpoint = metadata
        .parent()
        .context("account directory")?
        .join("native-checkpoint-fixture.bin");
    for (path, bytes) in [
        (&source, b"immutable native source".as_slice()),
        (
            &checkpoint,
            b"opaque synthetic checkpoint retained, never decoded".as_slice(),
        ),
    ] {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o400)
            .open(path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    let mut store = Store::open(&metadata)?;
    store.begin_snapshot(&scope)?;
    store.stage(
        &scope,
        None,
        &ChangePage {
            changes: vec![Change::Upsert(original.clone())],
            checkpoint: Checkpoint::Complete(Cursor("synthetic-completed-cursor".into())),
        },
    )?;
    store.observe_directory(&scope, &account.root_id, std::slice::from_ref(&original))?;
    let restored = Node {
        etag: Some("restored-E2".into()),
        content_version: Some("restored-E2".into()),
        ..original.clone()
    };
    if observer_only {
        // This arm already has a coherent completed publication. A subsequent
        // positive observation must survive a purely historical watch.
        let ticket = store.node_observation(&scope, &original.id)?;
        assert!(matches!(
            store.publish_absence(&ticket)?,
            cirrove_store::AbsenceResult::Published { .. }
        ));
        journal.finish_native_trash_publication(
            &completed,
            crate::journal::PackagePublicationStatus::Absent,
            0,
        )?;
        store.observe_node(&scope, &restored)?;
    }
    let cursor_before = store.cursor(&scope)?;
    assert!(cursor_before.is_some());
    assert_eq!(
        store.node(&scope, &original.id)?,
        Some(if observer_only {
            restored.clone()
        } else {
            original.clone()
        })
    );
    drop(store);
    drop(journal);
    drop(engine);
    let settings = serde_json::to_vec(&Settings {
        version: 2,
        accounts: vec![account.clone()],
    })?;
    let settings_path = state.join("accounts.json");
    let mut settings_file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&settings_path)?;
    settings_file.write_all(&settings)?;
    settings_file.sync_all()?;
    drop(settings_file);
    Settings::load(&state)?.validate()?;
    let rows_before = retained_rows(&db_path)?;
    let schema_before = schema(&db_path)?;
    assert_eq!(schema_before.0, 21);
    let mut protected = vec![source, checkpoint, settings_path, sentinel];
    for directory in [journal_root.join("objects"), journal_root.join("working")] {
        for entry in std::fs::read_dir(directory)? {
            let path = entry?.path();
            if path.is_file() {
                protected.push(path);
            }
        }
    }
    protected.sort();
    let protected_before = protected
        .iter()
        .map(|path| {
            Ok((
                path.clone(),
                std::fs::read(path)?,
                std::fs::metadata(path)?.permissions().mode() & 0o777,
            ))
        })
        .collect::<Result<Vec<_>>>()?;

    let cancel = CancellationToken::new();
    let supplied = provider.clone();
    let (manager, task) = Manager::start_with_provider(
        state,
        cancel.clone(),
        Arc::new(move |_| Ok(supplied.clone())),
    );
    let waited = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let db = rusqlite::Connection::open_with_flags(
                &db_path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?;
            let done: bool = db.query_row(
                "SELECT done FROM native_trash_metadata_publication WHERE operation=?1",
                [native.id.to_string()],
                |row| row.get(0),
            )?;
            if done
                && manager
                    .engines
                    .read()
                    .await
                    .get(&account.id)
                    .is_some_and(|engine| engine.account.enabled && engine.scope("drive") == scope)
            {
                return Ok::<_, anyhow::Error>(());
            }
            drop(db);
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
    let reads_before_status = provider.exact_reads.load(Ordering::SeqCst);
    let historical_status = if observer_only {
        let observed_engine = manager.engines.read().await.get(&account.id).cloned();
        Some(match observed_engine {
            Some(engine) => {
                manager
                    .native_trash_status(&engine, &account.id, native.id)
                    .await
            }
            None => Err(anyhow::anyhow!("actual enabled Manager Engine unavailable")),
        })
    } else {
        None
    };
    cancel.cancel();
    task.await?; // Close actual startup before every baseline-sensitive assertion.

    assert_eq!(launched_grant, Some(access));
    assert!(no_writers);
    assert_eq!(
        retained_rows(&db_path)?,
        rows_before,
        "startup changed retained transfer, mutation or namespace authority"
    );
    assert_eq!(
        schema(&db_path)?,
        schema_before,
        "metadata-only startup migrated the journal"
    );
    for (path, bytes, mode) in protected_before {
        assert_eq!(std::fs::read(&path)?, bytes);
        assert_eq!(std::fs::metadata(&path)?.permissions().mode() & 0o777, mode);
    }
    assert_eq!(provider.content_reads.load(Ordering::SeqCst), 0);
    assert_eq!(provider.mutations.load(Ordering::SeqCst), 0);
    let store = Store::open(&metadata)?;
    assert_eq!(store.cursor(&scope)?, cursor_before);
    let recovery = crate::journal::RecoveryJournal::open(&journal_root, &account.id)?;
    let page = recovery.native_trash_list(&scope, None, 10)?;
    assert_eq!(page.operations.len(), 1);
    assert_eq!(page.operations[0].operation, native.id);
    assert!(page.operations[0].removal_receipt_recorded);
    let db = rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let done: bool = db.query_row(
        "SELECT done FROM native_trash_metadata_publication WHERE operation=?1",
        [native.id.to_string()],
        |row| row.get(0),
    )?;
    if let Some(status) = historical_status {
        assert_eq!(
            provider.exact_reads.load(Ordering::SeqCst),
            reads_before_status
        );
        assert_eq!(store.node(&scope, &original.id)?, Some(restored));
        assert!(done && page.operations[0].metadata_absence_recorded);
        assert!(
            status.is_ok(),
            "read-only historical native Trash status must not require an active writer"
        );
        let status = status?;
        assert_eq!(status.operation, native.id);
        assert_eq!(status.state, MutationState::Applied);
        assert!(status.removal_confirmed && status.metadata_removed);
        return Ok(());
    }
    assert!(
        done && matches!(waited, Ok(Ok(()))),
        "effective read-only startup must finish the pending exact native Trash publication"
    );
    assert!(page.operations[0].metadata_absence_recorded);
    assert_eq!(store.node(&scope, &original.id)?, None);
    assert_eq!(store.children(&scope, &account.root_id)?, Some(Vec::new()));
    assert!(provider.exact_reads.load(Ordering::SeqCst) > 0);
    Ok(())
}

#[tokio::test]
async fn native_trash_readonly_startup_finishes_applied_receipt_without_replay() -> Result<()> {
    native_trash_startup(cirrove_auth::AccessMode::ReadOnly, false).await
}

#[tokio::test]
async fn native_trash_readonly_startup_repairs_with_readwrite_grant_and_no_factory() -> Result<()> {
    native_trash_startup(cirrove_auth::AccessMode::ReadWrite, false).await
}

#[tokio::test]
async fn native_trash_readonly_startup_observes_completed_history_without_writer() -> Result<()> {
    native_trash_startup(cirrove_auth::AccessMode::ReadOnly, true).await
}

// Follow-on controls are independent of the three unchanged startup regressions.
struct ScopedAbsence {
    inner: OrderedAbsence,
    calls: std::sync::Mutex<Vec<(Scope, String)>>,
    pause: bool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[async_trait::async_trait]
impl MetadataProvider for ScopedAbsence {
    fn provider_id(&self) -> &'static str {
        "icloud"
    }
    async fn changes(
        &self,
        scope: &Scope,
        cursor: Option<&Cursor>,
        cancel: &CancellationToken,
    ) -> std::result::Result<ChangePage, ProviderError> {
        self.inner.changes(scope, cursor, cancel).await
    }
}

#[async_trait::async_trait]
impl ReadProvider for ScopedAbsence {
    async fn node(
        &self,
        scope: &Scope,
        item: &str,
        _: &CancellationToken,
    ) -> std::result::Result<Node, ProviderError> {
        self.calls
            .lock()
            .expect("synthetic provider calls")
            .push((scope.clone(), item.into()));
        // Record a wrongly dispatched scope instead of panicking in the provider:
        // the desired refusal belongs to the production Engine authority check.
        if self.pause {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Err(ProviderError::NotFound)
    }
    async fn children(
        &self,
        scope: &Scope,
        parent: &str,
        cursor: Option<&Cursor>,
        cancel: &CancellationToken,
    ) -> std::result::Result<DirectoryPage, ProviderError> {
        self.inner.children(scope, parent, cursor, cancel).await
    }
    async fn read_range(
        &self,
        scope: &Scope,
        node: &Node,
        offset: u64,
        length: u32,
        cancel: &CancellationToken,
    ) -> std::result::Result<Vec<u8>, ProviderError> {
        self.inner
            .read_range(scope, node, offset, length, cancel)
            .await
    }
}

#[async_trait::async_trait]
impl MutationProvider for ScopedAbsence {
    async fn mutate(
        &self,
        request: &MutationRequest,
        cancel: &CancellationToken,
    ) -> cirrove_core::mutation::Result<MutationReceipt> {
        self.inner.mutate(request, cancel).await
    }
    async fn reconcile_mutation(
        &self,
        request: &MutationRequest,
        cancel: &CancellationToken,
    ) -> cirrove_core::mutation::Result<MutationReconciliation> {
        self.inner.reconcile_mutation(request, cancel).await
    }
}

struct ScopedFixture {
    engine: Arc<Engine>,
    provider: Arc<ScopedAbsence>,
    journal: PathBuf,
    source: PathBuf,
    original: Node,
}

async fn scoped_fixture(pause: bool) -> Result<ScopedFixture> {
    let root = tempfile::tempdir_in("/var/tmp")?.keep();
    eprintln!("retained native Trash scope fixture: {}", root.display());
    let account = Account {
        id: uuid::Uuid::new_v4().to_string(),
        label: "native-trash-scope-fixture".into(),
        registration: cirrove_auth::AppRegistration::ICloud,
        identity: cirrove_auth::Identity {
            tenant_id: String::new(),
            subject: "synthetic".into(),
            username: "fixture@example.invalid".into(),
            graph_user_id: String::new(),
            display_name: "Fixture".into(),
        },
        credential_id: uuid::Uuid::new_v4().to_string(),
        access: cirrove_auth::AccessMode::ReadOnly,
        drive: cirrove_core::CollectionInfo {
            id: "drive".into(),
            name: "Fixture".into(),
            drive_type: "icloud_drive".into(),
            web_url: "https://example.invalid".into(),
        },
        root_id: cirrove_icloud::ROOT_ID.into(),
        mount_path: root.join("mount"),
        enabled: true,
        poll_seconds: 3600,
        cache_bytes: 64 * 1024 * 1024,
    };
    let scope = Scope {
        account: account.id.clone(),
        provider: "icloud".into(),
        collection: "drive".into(),
    };
    let original = Node {
        id: "FILE::com.apple.CloudDocs::scope-valid-native".into(),
        parent_id: Some(account.root_id.clone()),
        name: "Valid.pages".into(),
        kind: NodeKind::Folder,
        size: 17,
        modified_unix: 1,
        etag: Some("original-E1".into()),
        content_version: Some("original-E1".into()),
        target: None,
        package: true,
    };
    let provider = Arc::new(ScopedAbsence {
        inner: OrderedAbsence {
            scope: scope.clone(),
            item: original.id.clone(),
            exact_reads: AtomicUsize::new(0),
            content_reads: AtomicUsize::new(0),
            mutations: AtomicUsize::new(0),
        },
        calls: std::sync::Mutex::new(Vec::new()),
        pause,
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let engine = Engine::new(account, provider.clone(), root.join("state")).await?;
    let journal = engine
        .db
        .parent()
        .context("scope account directory")?
        .join("journal");
    let source = root.join("source-preserved.bin");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o400)
        .open(&source)?;
    file.write_all(b"preserved native source")?;
    file.sync_all()?;
    let mut store = Store::open(&engine.db)?;
    store.begin_snapshot(&scope)?;
    store.stage(
        &scope,
        None,
        &ChangePage {
            changes: vec![Change::Upsert(original.clone())],
            checkpoint: Checkpoint::Complete(Cursor("synthetic-preserved-cursor".into())),
        },
    )?;
    store.observe_directory(
        &scope,
        &engine.account.root_id,
        std::slice::from_ref(&original),
    )?;
    Ok(ScopedFixture {
        engine,
        provider,
        journal,
        source,
        original,
    })
}

fn applied_native(
    journal: &mut crate::journal::UploadJournal,
    scope: Scope,
    before: Node,
) -> Result<crate::journal::MutationRecord> {
    let item = before.id.clone();
    let row = journal.enqueue_mutation(MutationRequest {
        scope,
        intent: cirrove_core::mutation::MutationIntent::TrashNativeDocument { before },
    })?;
    let claimed = journal.claim_mutation()?.context("claimed scope removal")?;
    assert_eq!(claimed.id, row.id);
    journal.acknowledge_mutation(
        row.id,
        claimed.attempt.context("scope removal attempt")?,
        MutationReceipt::Removed { item },
    )?;
    Ok(journal.mutation(row.id)?)
}

fn publication_row(path: &Path, id: uuid::Uuid) -> Result<(bool, i64, i64)> {
    let db =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    Ok(db.query_row("SELECT done,failures,retry_after FROM native_trash_metadata_publication WHERE operation=?1", [id.to_string()], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?)
}

async fn foreign_scope_cooldown(foreign_provider: bool) -> Result<()> {
    let f = scoped_fixture(false).await?;
    let expected_scope = f.engine.scope(&f.engine.account.drive.id);
    let mut foreign_scope = expected_scope.clone();
    if foreign_provider {
        foreign_scope.provider = "onedrive".into();
    } else {
        foreign_scope.collection = "foreign-collection".into();
    }
    let bad_node = Node {
        id: "FILE::com.apple.CloudDocs::scope-foreign-native".into(),
        name: "Foreign.pages".into(),
        ..f.original.clone()
    };
    let mut journal = crate::journal::UploadJournal::open(
        &f.journal,
        &f.engine.account.id,
        f.engine.account.cache_bytes,
    )?;
    let bad = applied_native(&mut journal, foreign_scope.clone(), bad_node.clone())?;
    let good = applied_native(&mut journal, expected_scope.clone(), f.original.clone())?;
    drop(journal);
    let mut store = Store::open(&f.engine.db)?;
    store.observe_node(&foreign_scope, &bad_node)?;
    let cursor = store.cursor(&expected_scope)?;
    drop(store);
    let path = f.journal.join("uploads.db");
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let db = rusqlite::Connection::open(&path)?;
    // Deterministic due ordering: bad first, then this sibling before the bad
    // job's future cooldown, irrespective of UUID ordering or elapsed wall time.
    db.execute(
        "UPDATE native_trash_metadata_publication SET retry_after=?2 WHERE operation=?1",
        rusqlite::params![
            good.id.to_string(),
            i64::try_from(started.saturating_sub(1))?
        ],
    )?;
    drop(db);
    let rows = retained_rows(&path)?;
    let schema_before = schema(&path)?;
    let bytes = std::fs::read(&f.source)?;
    let first = f
        .engine
        .repair_native_trash_metadata_once_at(f.journal.clone())
        .await;
    let first_calls = f
        .provider
        .calls
        .lock()
        .expect("synthetic provider calls")
        .clone();
    let bad_after_first = publication_row(&path, bad.id)?;
    let second = f
        .engine
        .repair_native_trash_metadata_once_at(f.journal.clone())
        .await;
    f.engine.stop().await;
    assert_eq!(retained_rows(&path)?, rows);
    assert_eq!(schema(&path)?, schema_before);
    assert_eq!(std::fs::read(&f.source)?, bytes);
    assert_eq!(
        std::fs::metadata(&f.source)?.permissions().mode() & 0o777,
        0o400
    );
    assert_eq!(f.provider.inner.content_reads.load(Ordering::SeqCst), 0);
    assert_eq!(f.provider.inner.mutations.load(Ordering::SeqCst), 0);
    let store = Store::open(&f.engine.db)?;
    assert_eq!(store.cursor(&expected_scope)?, cursor);
    assert!(
        first.is_err(),
        "a same-account foreign provider or collection must fail the configured Engine scope fence"
    );
    assert!(
        first_calls.is_empty(),
        "foreign scope publication must refuse before an exact-ID provider read"
    );
    assert!(
        !bad_after_first.0 && bad_after_first.1 == 1 && bad_after_first.2 > i64::try_from(started)?,
        "a rejected scope must receive durable cooldown instead of starving the valid sibling"
    );
    assert_eq!(store.node(&foreign_scope, &bad_node.id)?, Some(bad_node));
    assert!(matches!(second, Ok(true)));
    assert!(publication_row(&path, good.id)?.0);
    assert_eq!(store.node(&expected_scope, &f.original.id)?, None);
    assert_eq!(
        f.provider
            .calls
            .lock()
            .expect("synthetic provider calls")
            .as_slice(),
        &[(expected_scope, f.original.id.clone())]
    );
    Ok(())
}

#[tokio::test]
async fn native_trash_scope_foreign_provider_cools_down_without_starving_valid_sibling()
-> Result<()> {
    foreign_scope_cooldown(true).await
}

#[tokio::test]
async fn native_trash_scope_foreign_collection_cools_down_without_provider_read() -> Result<()> {
    foreign_scope_cooldown(false).await
}

#[tokio::test]
async fn native_trash_scope_ordered_absence_releases_owner_and_preserves_newer_restore()
-> Result<()> {
    let f = scoped_fixture(true).await?;
    let scope = f.engine.scope(&f.engine.account.drive.id);
    let mut journal = crate::journal::UploadJournal::open(
        &f.journal,
        &f.engine.account.id,
        f.engine.account.cache_bytes,
    )?;
    let removal = applied_native(&mut journal, scope.clone(), f.original.clone())?;
    drop(journal);
    let path = f.journal.join("uploads.db");
    let rows = retained_rows(&path)?;
    let schema_before = schema(&path)?;
    let bytes = std::fs::read(&f.source)?;
    let cursor = Store::open(&f.engine.db)?.cursor(&scope)?;
    let engine = f.engine.clone();
    let journal_root = f.journal.clone();
    let task = tokio::spawn(async move {
        engine
            .repair_native_trash_metadata_once_at(journal_root)
            .await
    });
    let entered = tokio::time::timeout(Duration::from_secs(3), f.provider.entered.notified()).await;
    // Acquire the real exclusive metadata journal owner while the provider is
    // paused. Do not hold it across the Store write or resume the pending read.
    let owner = crate::journal::MetadataPublicationJournal::open(&f.journal, &f.engine.account.id);
    let owner_released = owner.is_ok();
    drop(owner);
    let restored = Node {
        etag: Some("restored-E2".into()),
        content_version: Some("restored-E2".into()),
        ..f.original.clone()
    };
    let restoration = (|| -> Result<()> {
        Store::open(&f.engine.db)?.observe_node(&scope, &restored)?;
        Ok(())
    })();
    f.provider.release.notify_one();
    let result = task.await?;
    f.engine.stop().await;
    assert_eq!(retained_rows(&path)?, rows);
    assert_eq!(schema(&path)?, schema_before);
    assert_eq!(std::fs::read(&f.source)?, bytes);
    assert_eq!(
        std::fs::metadata(&f.source)?.permissions().mode() & 0o777,
        0o400
    );
    assert_eq!(f.provider.inner.content_reads.load(Ordering::SeqCst), 0);
    assert_eq!(f.provider.inner.mutations.load(Ordering::SeqCst), 0);
    assert!(entered.is_ok());
    assert!(
        owner_released,
        "read-only publication must release its exclusive journal owner before awaiting the provider"
    );
    restoration?;
    assert!(matches!(result, Ok(true)));
    let store = Store::open(&f.engine.db)?;
    assert_eq!(store.cursor(&scope)?, cursor);
    assert_eq!(store.node(&scope, &restored.id)?, Some(restored.clone()));
    let publication = publication_row(&path, removal.id)?;
    assert!(!publication.0 && publication.1 == 1);
    let metadata =
        crate::journal::MetadataPublicationJournal::open(&f.journal, &f.engine.account.id)?;
    assert!(metadata.due_native_trash(0)?.is_none());
    drop(metadata);
    let db =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let observed: String = db.query_row(
        "SELECT observed FROM native_trash_metadata_publication WHERE operation=?1",
        [removal.id.to_string()],
        |row| row.get(0),
    )?;
    assert_eq!(serde_json::from_str::<Node>(&observed)?, restored);
    assert_eq!(
        f.provider
            .calls
            .lock()
            .expect("synthetic provider calls")
            .as_slice(),
        &[(scope, f.original.id)]
    );
    Ok(())
}
