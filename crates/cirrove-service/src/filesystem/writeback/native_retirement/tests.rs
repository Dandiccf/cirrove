#![allow(clippy::unwrap_used)]
use super::*;
use crate::journal::PackagePublicationStatus;
use cirrove_core::{
    ChangePage, Cursor, DirectoryPage, MetadataProvider, ReadProvider,
    reads::{NativeArchiveBinding, ReadIdentity, ReadSession},
    upload::{PackageHandoffReceipt, PackageUploadReceipt, RecoveryLocation, UploadRepresentation},
};
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
struct Provider {
    node: Mutex<Option<Node>>,
    hold: AtomicBool,
    calls: AtomicUsize,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
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
    async fn node(
        &self,
        _: &Scope,
        id: &str,
        _: &CancellationToken,
    ) -> std::result::Result<Node, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.hold.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        self.node
            .lock()
            .unwrap()
            .clone()
            .filter(|n| n.id == id)
            .ok_or(ProviderError::NotFound)
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
        panic!("retirement must not download content")
    }
}
fn bytes(value: &[u8]) -> Vec<u8> {
    crate::native_import::synthetic_package_archive("Owned.pages/Document", value)
}
fn binding(t: &tempfile::TempDir, scope: Scope, source: Node, data: &[u8]) -> NativeArchiveBinding {
    let path = t.path().join(format!("{}.zip", Uuid::new_v4()));
    std::fs::write(&path, data).unwrap();
    let archive = crate::native_import::ValidatedPackageArchive::capture(
        &path,
        t.path(),
        "Owned.pages",
        &CancellationToken::new(),
    )
    .unwrap();
    let (_, representation, _, sha) = archive.into_parts();
    let UploadRepresentation::PackageArchive { semantic, .. } = representation else {
        panic!("package")
    };
    NativeArchiveBinding {
        scope,
        semantic,
        archive: Node {
            id: format!("icloud-artifact:{}", source.id),
            parent_id: Some(source.id.clone()),
            name: source.name.clone(),
            kind: NodeKind::File,
            size: data.len() as u64,
            modified_unix: 0,
            etag: None,
            content_version: Some(format!(
                "icloud-artifact-v2:{}",
                serde_json::json!({"source_etag":source.etag,"source_size":source.size,"source_parent":source.parent_id,"sha256":sha})
            )),
            package: false,
            target: None,
        },
        source,
    }
}
fn hydrate(j: &mut UploadJournal, bound: NativeArchiveBinding, data: &[u8]) -> WorkingFile {
    let mut hydration = j.reserve_native_working(bound).unwrap();
    hydration.write_chunk(data).unwrap();
    let ready = hydration.validate(&CancellationToken::new()).unwrap();
    j.publish_native_working(ready).unwrap()
}
fn acknowledge(j: &mut UploadJournal, id: Uuid, value: &[u8]) -> Node {
    let data = bytes(value);
    j.truncate_working(id, 0).unwrap();
    j.write_working(id, 0, &data).unwrap();
    let captured = j
        .capture_native_working(id)
        .unwrap()
        .capture(&CancellationToken::new())
        .unwrap();
    j.seal_captured_native_working(captured, &CancellationToken::new())
        .unwrap();
    let row = j.claim_next().unwrap().unwrap();
    let UploadRepresentation::PackageReplacementArchive {
        original,
        semantic,
        original_semantic,
        ..
    } = &row.representation
    else {
        panic!("native")
    };
    let mut current = original.as_ref().clone();
    current.id = format!("FILE::com.apple.CloudDocs::{}", row.id);
    current.etag = Some(format!("v{}", row.sequence));
    let mut backup = original.as_ref().clone();
    backup.parent_id = Some("FOLDER::com.apple.CloudDocs::TRASH_ROOT".into());
    backup.etag = Some("trash".into());
    j.reserve_identity_handoff(
        row.id,
        row.attempt.unwrap(),
        RecoveryLocation::Trash {
            parent: backup.parent_id.clone().unwrap(),
            local_name: format!("recovery-{}.pages", row.id),
        },
    )
    .unwrap();
    j.acknowledge_package_handoff(
        row.id,
        row.attempt.unwrap(),
        PackageHandoffReceipt {
            original: original.as_ref().clone(),
            current: PackageUploadReceipt {
                remote: current.clone(),
                semantic: semantic.clone(),
            },
            backup: PackageUploadReceipt {
                remote: backup,
                semantic: original_semantic.clone(),
            },
        },
    )
    .unwrap();
    j.finish_package_publication(
        &j.get(row.id).unwrap(),
        PackagePublicationStatus::Present(current.clone()),
        0,
    )
    .unwrap();
    current
}
struct Fixture {
    temp: tempfile::TempDir,
    engine: Arc<Engine>,
    writer: Arc<Writeback>,
    journal: Arc<Mutex<UploadJournal>>,
    provider: Arc<Provider>,
    id: Uuid,
    owner: Uuid,
    original: NativeArchiveBinding,
}
impl Fixture {
    async fn new(hold: bool) -> Self {
        let temp = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in("/var/tmp")
            .unwrap();
        let account = crate::accounts::Account {
            id: "native-retire".into(),
            label: "Synthetic".into(),
            registration: cirrove_auth::AppRegistration::ICloud,
            identity: cirrove_auth::Identity {
                tenant_id: String::new(),
                subject: "fixture".into(),
                username: "fixture@example.invalid".into(),
                graph_user_id: "fixture".into(),
                display_name: "fixture".into(),
            },
            credential_id: "fixture".into(),
            access: cirrove_auth::AccessMode::ReadWrite,
            drive: cirrove_core::CollectionInfo {
                id: "drive".into(),
                name: "Synthetic".into(),
                drive_type: "icloud".into(),
                web_url: String::new(),
            },
            root_id: "FOLDER::com.apple.CloudDocs::root".into(),
            mount_path: temp.path().join("mount"),
            enabled: false,
            poll_seconds: 3600,
            cache_bytes: 16 * 1024 * 1024,
        };
        let scope = Scope {
            account: account.id.clone(),
            provider: "icloud".into(),
            collection: "drive".into(),
        };
        let source = Node {
            id: "FILE::com.apple.CloudDocs::original".into(),
            parent_id: Some(account.root_id.clone()),
            name: "Owned.pages".into(),
            kind: NodeKind::Folder,
            size: 17,
            modified_unix: 0,
            etag: Some("original".into()),
            content_version: None,
            target: None,
            package: true,
        };
        let data = bytes(b"initial");
        let original = binding(&temp, scope, source, &data);
        let mut j =
            UploadJournal::open(&temp.path().join("journal"), &account.id, 16 * 1024 * 1024)
                .unwrap();
        let file = hydrate(&mut j, original.clone(), &data);
        let current = acknowledge(&mut j, file.id, b"uploaded");
        let owner = j
            .namespace_object(file.id)
            .unwrap()
            .native_archive
            .unwrap()
            .source_owner;
        let provider = Arc::new(Provider {
            node: Mutex::new(Some(current.clone())),
            hold: AtomicBool::new(hold),
            calls: AtomicUsize::new(0),
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let engine = Engine::new(account, provider.clone(), temp.path().join("state"))
            .await
            .unwrap();
        Store::open(engine.db.clone())
            .unwrap()
            .observe_node(&original.scope, &current)
            .unwrap(); // cached equality cannot skip fresh provider call
        let journal = Arc::new(Mutex::new(j));
        let writer = Writeback::new(&engine, journal.clone()).await.unwrap();
        Self {
            temp,
            engine,
            writer,
            journal,
            provider,
            id: file.id,
            owner,
            original,
        }
    }
    fn start(&self) -> tokio::task::JoinHandle<Result<Option<bool>>> {
        let writer = self.writer.clone();
        let engine = self.engine.clone();
        let owner = self.owner;
        tokio::spawn(async move { writer.maintain_native(&engine, owner).await })
    }
    fn retained(&self) -> bool {
        self.journal.lock().unwrap().working_file(self.id).is_ok()
    }
}
#[tokio::test]
async fn native_maintenance_refreshes_even_cached_receipt_and_retires_repeated_reactivated_stream()
{
    let f = Fixture::new(false).await;
    let mut generation = f
        .journal
        .lock()
        .unwrap()
        .working_file(f.id)
        .unwrap()
        .generation;
    for cycle in 0..3 {
        assert_eq!(
            f.writer.maintain_native(&f.engine, f.owner).await.unwrap(),
            Some(true)
        );
        assert!(!f.retained());
        assert!(
            f.writer
                .working(&f.original.scope, &format!("local-native-archive-{}", f.id))
                .unwrap()
                .is_none()
        );
        assert_eq!(f.provider.calls.load(Ordering::SeqCst), cycle + 1);
        let current = f.provider.node.lock().unwrap().clone().unwrap();
        let data = bytes(b"uploaded");
        let bound = binding(&f.temp, f.original.scope.clone(), current, &data);
        {
            let mut j = f.journal.lock().unwrap();
            let file = hydrate(&mut j, bound, &data);
            assert_eq!(file.id, f.id);
            assert!(file.generation > generation);
            generation = file.generation;
            *f.provider.node.lock().unwrap() = Some(acknowledge(&mut j, f.id, b"uploaded"));
        }
        f.writer.refresh_projection().await.unwrap();
    }
}
#[tokio::test]
async fn native_maintenance_held_child_source_seal_and_immutable_reader_exclude_before_network() {
    let f = Fixture::new(false).await;
    let (scope, source, child) = {
        let j = f.journal.lock().unwrap();
        let child = j.namespace_object(f.id).unwrap();
        let owner = j.namespace_object(f.owner).unwrap();
        (child.scope, owner.node.id, child.node.id)
    };
    for item in [child, source] {
        let lease = f
            .writer
            .lease(&scope, &item, &f.engine.cancel)
            .await
            .unwrap();
        assert_eq!(
            f.writer.maintain_native(&f.engine, f.owner).await.unwrap(),
            None
        );
        drop(lease);
    }
    let sealing = f.writer.sealing.try_working(f.id).unwrap().unwrap();
    assert_eq!(
        f.writer.maintain_native(&f.engine, f.owner).await.unwrap(),
        None
    );
    drop(sealing);
    struct Snapshot(ReadIdentity);
    #[async_trait::async_trait]
    impl ReadSession for Snapshot {
        fn identity(&self) -> &ReadIdentity {
            &self.0
        }
        async fn read_range(
            &self,
            _: u64,
            _: u32,
            _: &CancellationToken,
        ) -> std::result::Result<Vec<u8>, ProviderError> {
            panic!("idle check cannot read")
        }
    }
    let session: Arc<dyn ReadSession> = Arc::new(Snapshot(
        ReadIdentity::new(&scope, &f.original.archive).unwrap(),
    ));
    f.writer
        .projection
        .lock()
        .unwrap()
        .native_readers
        .insert(session.identity().clone(), Arc::downgrade(&session));
    assert_eq!(
        f.writer.maintain_native(&f.engine, f.owner).await.unwrap(),
        None
    );
    drop(session);
    // A registered original-artifact read holds the OpenFile while a remote
    // flight runs, even after RELEASE removes the kernel handle.
    let fs = CloudFs::new(f.engine.clone()).unwrap();
    let root = fs.inner.view(1).unwrap();
    let mut view = fs
        .inner
        .insert(&root, f.original.archive.clone())
        .await
        .unwrap();
    view.node = Some(Arc::new(f.original.archive.clone()));
    let file = f
        .writer
        .register_open(OpenFile {
            view,
            node: f.original.archive.clone(),
            flags: libc::O_RDONLY,
            _lease: None,
            remote_reads: tokio_util::task::TaskTracker::new(),
            native_snapshot: std::sync::OnceLock::new(),
        })
        .unwrap();
    let ReadSource::Remote(_, flight) = f.writer.read_source(&file).unwrap() else {
        panic!("remote fixture")
    };
    let flight_owner = file.clone();
    drop(file);
    assert_eq!(
        f.writer.maintain_native(&f.engine, f.owner).await.unwrap(),
        None
    );
    drop(flight);
    drop(flight_owner);
    assert_eq!(f.provider.calls.load(Ordering::SeqCst), 0);
    assert!(f.retained());
    assert_eq!(
        f.writer.maintain_native(&f.engine, f.owner).await.unwrap(),
        Some(true)
    );
    assert!(!f.retained());
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_maintenance_dirty_race_and_late_reader_preserve_bytes_without_blocking_edits() {
    for dirty in [false, true] {
        let f = Fixture::new(true).await;
        let task = f.start();
        tokio::time::timeout(Duration::from_secs(2), f.provider.entered.notified())
            .await
            .unwrap();
        assert_eq!(
            f.writer.maintain_native(&f.engine, f.owner).await.unwrap(),
            None,
            "one provider observation at a time"
        );
        assert_eq!(f.provider.calls.load(Ordering::SeqCst), 1);
        let item = format!("local-native-archive-{}", f.id);
        let lease = tokio::time::timeout(
            Duration::from_millis(500),
            f.writer.lease(&f.original.scope, &item, &f.engine.cancel),
        )
        .await
        .unwrap()
        .unwrap();
        if dirty {
            f.writer
                .write(f.id, 0, b"accepted newer bytes".to_vec(), false)
                .await
                .unwrap();
            drop(lease);
        } else {
            f.provider.release.notify_one();
            task.await.unwrap().unwrap();
            assert!(f.retained());
            drop(lease);
            continue;
        }
        f.provider.release.notify_one();
        task.await.unwrap().unwrap();
        assert!(f.retained());
        assert!(f.journal.lock().unwrap().working_file(f.id).unwrap().dirty);
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_maintenance_cancellation_absence_and_changed_revision_never_retire() {
    let f = Fixture::new(true).await;
    let task = f.start();
    tokio::time::timeout(Duration::from_secs(2), f.provider.entered.notified())
        .await
        .unwrap();
    f.engine.cancel.cancel();
    assert_eq!(
        tokio::time::timeout(Duration::from_millis(500), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        Some(false)
    );
    assert!(f.retained());
    for absent in [false, true] {
        let f = Fixture::new(false).await;
        if absent {
            *f.provider.node.lock().unwrap() = None;
        } else {
            f.provider.node.lock().unwrap().as_mut().unwrap().etag = Some("different".into());
        }
        let result = f.writer.maintain_native(&f.engine, f.owner).await;
        if absent {
            assert!(result.is_err());
        } else {
            assert_eq!(result.unwrap(), Some(true));
        }
        assert!(f.retained());
        assert_eq!(f.provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            f.writer.maintain_native(&f.engine, f.owner).await.unwrap(),
            None,
            "provider refusal backoff survives idle passes"
        );
        assert_eq!(f.provider.calls.load(Ordering::SeqCst), 1);
    }
}
