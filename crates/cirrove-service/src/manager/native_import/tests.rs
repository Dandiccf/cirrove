#![allow(clippy::unwrap_used)]
use super::*;
use cirrove_core::{
    ChangePage, Cursor, DirectoryPage, MetadataProvider, Node, NodeKind, ProviderError, Scope,
};
use std::os::unix::fs::PermissionsExt;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};
const ROOT: &str = "FOLDER::com.apple.CloudDocs::root";
struct Provider {
    nodes: Mutex<Vec<Node>>,
    roots: AtomicUsize,
    pause: Mutex<Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>>,
    reads: AtomicUsize,
}
#[async_trait::async_trait]
impl MetadataProvider for Provider {
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
impl ReadProvider for Provider {
    fn unknown_directories_require_fetch(&self) -> bool {
        true
    }
    async fn node(
        &self,
        _: &Scope,
        id: &str,
        _: &CancellationToken,
    ) -> std::result::Result<Node, ProviderError> {
        if id == ROOT && self.roots.fetch_add(1, Ordering::SeqCst) == 1 {
            let gate = self.pause.lock().unwrap().clone();
            if let Some((ready, release)) = gate {
                ready.notify_one();
                release.notified().await;
            }
        }
        self.nodes
            .lock()
            .unwrap()
            .iter()
            .find(|n| n.id == id)
            .cloned()
            .ok_or(ProviderError::NotFound)
    }
    async fn children(
        &self,
        _: &Scope,
        parent: &str,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<DirectoryPage, ProviderError> {
        Ok(DirectoryPage {
            nodes: self
                .nodes
                .lock()
                .unwrap()
                .iter()
                .filter(|n| n.parent_id.as_deref() == Some(parent))
                .cloned()
                .collect(),
            next: None,
        })
    }
    async fn read_range(
        &self,
        _: &Scope,
        _: &Node,
        _: u64,
        _: u32,
        _: &CancellationToken,
    ) -> std::result::Result<Vec<u8>, ProviderError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Err(ProviderError::Unavailable)
    }
}
fn node(id: &str, parent: Option<&str>, name: &str) -> Node {
    Node {
        id: id.into(),
        parent_id: parent.map(str::to_owned),
        name: name.into(),
        kind: NodeKind::Folder,
        size: 0,
        modified_unix: 0,
        etag: Some("v1".into()),
        content_version: None,
        target: None,
        package: false,
    }
}
struct Fixture {
    _temp: tempfile::TempDir,
    manager: Arc<Manager>,
    engine: Arc<Engine>,
    control: WriteControl,
    journal: Arc<Mutex<crate::journal::UploadJournal>>,
    provider: Arc<Provider>,
    source: PathBuf,
}
impl Fixture {
    async fn new() -> Self {
        Self::with_journal("uploads").await
    }
    async fn with_journal(journal_name: &str) -> Self {
        let tmp = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| "/var/tmp".into());
        let temp = tempfile::tempdir_in(tmp).unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let state = temp.path().join("state");
        let mount = temp.path().join("mount");
        std::fs::create_dir(&mount).unwrap();
        let account = Account {
            id: uuid::Uuid::new_v4().to_string(),
            label: "native-import-test".into(),
            registration: cirrove_auth::AppRegistration::ICloud,
            identity: cirrove_auth::Identity {
                tenant_id: String::new(),
                subject: "owned".into(),
                username: "fixture@example.invalid".into(),
                graph_user_id: "owned".into(),
                display_name: "Synthetic".into(),
            },
            credential_id: uuid::Uuid::new_v4().to_string(),
            access: cirrove_auth::AccessMode::ReadWrite,
            drive: cirrove_core::CollectionInfo {
                id: "drive".into(),
                name: "Synthetic".into(),
                drive_type: "icloud".into(),
                web_url: String::new(),
            },
            root_id: ROOT.into(),
            mount_path: mount,
            enabled: true,
            poll_seconds: 3600,
            cache_bytes: 16 * 1024 * 1024,
        };
        let provider = Arc::new(Provider {
            nodes: Mutex::new(vec![node(ROOT, None, "Root")]),
            roots: AtomicUsize::new(0),
            pause: Mutex::new(None),
            reads: AtomicUsize::new(0),
        });
        let engine = Engine::new(account.clone(), provider.clone(), state)
            .await
            .unwrap();
        let journal = Arc::new(Mutex::new(
            crate::journal::UploadJournal::open(
                &engine.db.parent().unwrap().join(journal_name),
                &account.id,
                16 * 1024 * 1024,
            )
            .unwrap(),
        ));
        let fs = CloudFs::new_experimental_writable(engine.clone(), journal.clone())
            .await
            .unwrap();
        let control = fs.write_control().unwrap();
        let manager = Arc::new(Manager::default());
        manager.status.write().await.push(AccountStatus {
            account_id: account.id.clone(),
            label: account.label.clone(),
            mount_path: account.mount_path.clone(),
            enabled: true,
            mounted: true,
            ..Default::default()
        });
        manager
            .engines
            .write()
            .await
            .insert(account.id.clone(), engine.clone());
        manager
            .writers
            .write()
            .await
            .insert(account.id, control.clone());
        let source = temp.path().join("source.pages");
        std::fs::write(
            &source,
            crate::native_import::synthetic_package_archive(
                "Source.pages/Metadata/data",
                b"synthetic contents",
            ),
        )
        .unwrap();
        Self {
            _temp: temp,
            manager,
            engine,
            control,
            journal,
            provider,
            source,
        }
    }
    fn input(&self) -> NativeImportInput {
        NativeImportInput {
            source: self.source.clone(),
            expected_root: "Source.pages".into(),
            parent: String::new(),
            name: "Imported.pages".into(),
        }
    }
    fn empty(&self) {
        assert!(
            self.journal
                .lock()
                .unwrap()
                .list(0, 100)
                .unwrap()
                .is_empty()
        );
        assert_eq!(self.provider.reads.load(Ordering::SeqCst), 0);
    }
}
#[tokio::test]
async fn ordinary_import_enqueues_once_and_exposes_only_exact_account_record() {
    let f = Fixture::new().await;
    let record = f
        .manager
        .enqueue_native_package(f.engine.clone(), f.input(), CancellationToken::new())
        .await
        .unwrap();
    assert!(matches!(
        record.representation,
        UploadRepresentation::PackageArchive { .. }
    ));
    assert_eq!(record.state, crate::journal::UploadState::Pending);
    assert_eq!(
        f.manager
            .native_import_record(&f.engine, record.id)
            .await
            .unwrap()
            .id,
        record.id
    );
    assert!(
        f.manager
            .enqueue_native_package(f.engine.clone(), f.input(), CancellationToken::new())
            .await
            .is_err()
    );
    assert_eq!(f.journal.lock().unwrap().list(0, 100).unwrap().len(), 1);
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn disabled_stopped_busy_and_foreign_engine_refuse_before_capture() {
    let f = Fixture::new().await;
    let slot = f
        .manager
        .native_import_slots
        .clone()
        .acquire_owned()
        .await
        .unwrap();
    assert!(
        f.manager
            .enqueue_native_package(f.engine.clone(), f.input(), CancellationToken::new())
            .await
            .is_err()
    );
    drop(slot);
    f.manager.status.write().await[0].enabled = false;
    assert!(
        f.manager
            .enqueue_native_package(f.engine.clone(), f.input(), CancellationToken::new())
            .await
            .is_err()
    );
    f.manager.status.write().await[0].enabled = true;
    let other = Fixture::new().await;
    assert!(
        f.manager
            .enqueue_native_package(other.engine.clone(), f.input(), CancellationToken::new())
            .await
            .is_err()
    );
    f.control.freeze();
    assert!(
        f.manager
            .enqueue_native_package(f.engine.clone(), f.input(), CancellationToken::new())
            .await
            .is_err()
    );
    f.empty();
}
#[tokio::test]
async fn stopped_or_replaced_after_capture_does_not_publish() {
    for race in 0..3 {
        let f = Fixture::new().await;
        let ready = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        *f.provider.pause.lock().unwrap() = Some((ready.clone(), release.clone()));
        let manager = f.manager.clone();
        let engine = f.engine.clone();
        let input = f.input();
        let task = tokio::spawn(async move {
            manager
                .enqueue_native_package(engine, input, CancellationToken::new())
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), ready.notified())
            .await
            .unwrap();
        if race == 1 {
            f.manager.writers.write().await.remove(&f.engine.account.id);
        } else if race == 0 {
            f.control.freeze();
        } else {
            f.journal
                .lock()
                .unwrap()
                .create_namespace_directory(f.engine.scope("drive"), ROOT.into(), "changed".into())
                .unwrap();
        }
        release.notify_one();
        assert!(task.await.unwrap().is_err());
        f.empty();
    }
}
#[tokio::test]
async fn package_parent_and_mount_or_state_sources_are_refused() {
    let f = Fixture::new().await;
    let mut package = node(
        "FOLDER::com.apple.CloudDocs::package",
        Some(ROOT),
        "Package",
    );
    package.package = true;
    f.provider.nodes.lock().unwrap().push(package);
    let mut input = f.input();
    input.parent = "Package".into();
    assert!(
        f.manager
            .enqueue_native_package(f.engine.clone(), input, CancellationToken::new())
            .await
            .is_err()
    );
    for root in [
        f.engine.account.mount_path.clone(),
        f.engine.db.parent().unwrap().to_owned(),
    ] {
        let path = root.join("source.pages");
        std::fs::copy(&f.source, &path).unwrap();
        let mut input = f.input();
        input.source = path;
        assert!(
            f.manager
                .enqueue_native_package(f.engine.clone(), input, CancellationToken::new())
                .await
                .is_err()
        );
    }
    f.empty();
}
#[test]
fn initial_profile_is_pages_only_and_rejects_unsafe_name_or_path() {
    let mut input = NativeImportInput {
        source: "/owned/file".into(),
        expected_root: "Source.pages".into(),
        parent: String::new(),
        name: "Import.PAGES".into(),
    };
    assert!(validate_input(&input).is_ok());
    for name in [
        "Import.numbers",
        "Import.unknown",
        "../Import.pages",
        "bad\n.pages",
    ] {
        input.name = name.into();
        assert!(validate_input(&input).is_err());
    }
    input.name = "Import.pages".into();
    input.expected_root = "Source.key".into();
    assert!(validate_input(&input).is_err());
}

mod socket;
mod watch;

#[tokio::test]
async fn retained_import_receipt_refuses_same_engine_remount_during_journal_wait() {
    let f = Fixture::new().await;
    let row = f
        .manager
        .enqueue_native_package(f.engine.clone(), f.input(), CancellationToken::new())
        .await
        .unwrap();
    let replacement_fs = CloudFs::new_experimental_writable(f.engine.clone(), f.journal.clone())
        .await
        .unwrap();
    let replacement = replacement_fs.write_control().unwrap();
    assert!(!replacement.same_mount(&f.control));
    let (locked_tx, locked_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let journal = f.journal.clone();
    let holder = tokio::task::spawn_blocking(move || {
        let _held = journal.lock().unwrap();
        locked_tx.send(()).unwrap();
        release_rx.recv().unwrap();
    });
    locked_rx.await.unwrap();
    let mut read = Box::pin(f.manager.native_import_record(&f.engine, row.id));
    // All async registry locks are free. The read has captured the old control
    // and submitted its local journal lookup before yielding here.
    std::future::poll_fn(|cx| {
        assert!(std::future::Future::poll(read.as_mut(), cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    f.manager
        .writers
        .write()
        .await
        .insert(f.engine.account.id.clone(), replacement);
    // Leave the original control open: Engine identity alone must not suffice.
    f.control.native_import_open().unwrap();
    release_tx.send(()).unwrap();
    assert!(
        read.await.is_err(),
        "receipt from withdrawn mount was accepted"
    );
    holder.await.unwrap();
    assert_eq!(
        f.journal.lock().unwrap().get(row.id).unwrap().state,
        crate::journal::UploadState::Pending
    );
    f.engine.stop().await;
}

#[path = "native_trash_tests.rs"]
mod native_trash_tests;

#[path = "native_trash_list_tests.rs"]
mod native_trash_list_tests;

#[path = "native_replace_tests.rs"]
mod native_replace_tests;

#[path = "native_replace_observer_tests.rs"]
mod native_replace_observer_tests;

mod native_abandon_socket;
#[path = "tests/replacement_socket.rs"]
mod replacement_socket;
