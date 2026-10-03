//! A real kernel save feeds the actual native HTTPS coordinator and journal worker.
//! Metadata/hydration are concrete synthetic fixtures; no Apple or application claim.
use super::*;
use crate::{accounts::Account, engine::Engine, filesystem::CloudFs};
use cirrove_auth::{AccessMode, AppRegistration, Identity};
use cirrove_core::{
    Change, ChangePage, Checkpoint, Cursor, DirectoryPage, MetadataProvider, ProviderError,
    ReadProvider,
    reads::{NativeArchiveBinding, ReadIdentity, ReadSession},
};
use std::{
    io::{Read, Seek, SeekFrom},
    sync::atomic::{AtomicUsize, Ordering},
};

struct ArchiveSnapshot {
    identity: ReadIdentity,
    bytes: Vec<u8>,
}
#[async_trait::async_trait]
impl ReadSession for ArchiveSnapshot {
    fn identity(&self) -> &ReadIdentity {
        &self.identity
    }
    async fn read_range(
        &self,
        offset: u64,
        length: u32,
        _: &CancellationToken,
    ) -> std::result::Result<Vec<u8>, ProviderError> {
        let start = (offset as usize).min(self.bytes.len());
        let end = start.saturating_add(length as usize).min(self.bytes.len());
        Ok(self.bytes[start..end].to_vec())
    }
}
struct MountedArchive {
    initial: NativeArchiveBinding,
    initial_snapshot: Arc<ArchiveSnapshot>,
    committed: NativeArchiveBinding,
    committed_snapshot: Arc<ArchiveSnapshot>,
    // FUSE allocates the operation before the unchanged server knows its stage name.
    server: Mutex<Option<Arc<Mutex<State>>>>,
    resolutions: AtomicUsize,
}
impl MountedArchive {
    fn installed(&self) -> bool {
        self.server
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|s| s.lock().unwrap().installed)
    }
    fn trashed(&self) -> bool {
        self.server
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|s| s.lock().unwrap().trashed)
    }
    fn current(&self) -> &NativeArchiveBinding {
        if self.installed() {
            &self.committed
        } else {
            &self.initial
        }
    }
}
#[async_trait::async_trait]
impl MetadataProvider for MountedArchive {
    fn provider_id(&self) -> &'static str {
        "icloud"
    }
    async fn changes(
        &self,
        _: &Scope,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<ChangePage, ProviderError> {
        Ok(ChangePage {
            changes: if self.installed() {
                vec![
                    Change::Delete { id: OLD.into() },
                    Change::Upsert(self.committed.source.clone()),
                ]
            } else {
                vec![Change::Upsert(self.initial.source.clone())]
            },
            checkpoint: Checkpoint::Complete(Cursor("synthetic-native-fuse-baseline".into())),
        })
    }
}
#[async_trait::async_trait]
impl ReadProvider for MountedArchive {
    async fn node(
        &self,
        scope: &Scope,
        id: &str,
        _: &CancellationToken,
    ) -> std::result::Result<Node, ProviderError> {
        if scope != &self.initial.scope {
            return Err(ProviderError::NotFound);
        }
        if id == FOLDER {
            return Ok(parent());
        }
        if id == OLD && !self.trashed() {
            return Ok(self.initial.source.clone());
        }
        if id == NEW && self.installed() {
            return Ok(self.committed.source.clone());
        }
        if id == self.current().archive.id {
            return Ok(self.current().archive.clone());
        }
        Err(ProviderError::NotFound)
    }
    async fn children(
        &self,
        scope: &Scope,
        parent_id: &str,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<DirectoryPage, ProviderError> {
        if scope != &self.initial.scope {
            return Err(ProviderError::NotFound);
        }
        let current = self.current();
        let nodes = if parent_id == FOLDER {
            vec![current.source.clone()]
        } else if parent_id == current.source.id {
            vec![current.archive.clone()]
        } else {
            vec![]
        };
        Ok(DirectoryPage { nodes, next: None })
    }
    async fn read_range(
        &self,
        _: &Scope,
        _: &Node,
        _: u64,
        _: u32,
        _: &CancellationToken,
    ) -> std::result::Result<Vec<u8>, ProviderError> {
        panic!("native kernel content must use its exact staged snapshot")
    }
    async fn resolve_native_archive(
        &self,
        scope: &Scope,
        node: &Node,
        _: &CancellationToken,
    ) -> std::result::Result<Option<NativeArchiveBinding>, ProviderError> {
        self.resolutions.fetch_add(1, Ordering::SeqCst);
        let binding = self.current();
        if scope != &binding.scope || node != &binding.archive {
            return Err(ProviderError::VersionChanged);
        }
        Ok(Some(binding.clone()))
    }
    async fn staged_content_session(
        &self,
        scope: &Scope,
        node: &Node,
        _: &CancellationToken,
    ) -> std::result::Result<Option<Arc<dyn ReadSession>>, ProviderError> {
        if self.resolutions.load(Ordering::SeqCst) == 0 {
            return Ok(None);
        }
        let snapshot = if node.id == self.initial.archive.id {
            self.initial_snapshot.clone()
        } else if self.installed() && node.id == self.committed.archive.id {
            self.committed_snapshot.clone()
        } else {
            return Ok(None);
        };
        if ReadIdentity::new(scope, node)? != snapshot.identity {
            return Ok(None);
        }
        Ok(Some(snapshot))
    }
}
fn binding(scope: &Scope, source: Node, old: bool) -> (NativeArchiveBinding, Arc<ArchiveSnapshot>) {
    let bytes = archive(&source.name, old, false);
    let raw_sha256 = hex::encode(Sha256::digest(&bytes));
    let artifact = Node {
        id: format!("icloud-artifact:{}", source.id),
        parent_id: Some(source.id.clone()),
        name: source.name.clone(),
        kind: NodeKind::File,
        size: bytes.len() as u64,
        modified_unix: 0,
        etag: None,
        content_version: Some(format!(
            "icloud-artifact-v2:{}",
            json!({"source_etag":source.etag,"source_parent":source.parent_id,"source_size":source.size,"sha256":raw_sha256})
        )),
        target: None,
        package: false,
    };
    let snapshot = Arc::new(ArchiveSnapshot {
        identity: ReadIdentity::new(scope, &artifact).unwrap(),
        bytes,
    });
    (
        NativeArchiveBinding {
            scope: scope.clone(),
            semantic: semantic(&source.name, old, 2),
            source,
            archive: artifact,
        },
        snapshot,
    )
}
fn account(mount: &Path) -> Account {
    Account {
        id: Uuid::new_v4().to_string(),
        label: "native FUSE HTTPS fixture".into(),
        registration: AppRegistration::ICloud,
        identity: Identity {
            tenant_id: "fixture".into(),
            subject: "synthetic".into(),
            username: "fixture@example.invalid".into(),
            graph_user_id: "fixture".into(),
            display_name: "Fixture".into(),
        },
        credential_id: Uuid::new_v4().to_string(),
        access: AccessMode::ReadWrite,
        drive: cirrove_core::CollectionInfo {
            id: "drive".into(),
            name: "Fixture".into(),
            drive_type: "icloud_drive".into(),
            web_url: "https://example.invalid".into(),
        },
        root_id: FOLDER.into(),
        mount_path: mount.into(),
        enabled: false,
        poll_seconds: 3600,
        cache_bytes: 8 * 1024 * 1024,
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE and loopback TLS; no Apple credentials"]
async fn real_native_fuse_save_executes_https_package_worker_and_reopens_confirmed_bytes() {
    let root = directory();
    let staging = directory();
    let state = directory();
    let mount = root.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let account = account(&mount);
    let scope = Scope {
        account: account.id.clone(),
        provider: "icloud".into(),
        collection: "drive".into(),
    };
    let old_bytes = archive("Target.pages", true, false);
    let new_bytes = archive("Target.pages", false, false);
    let (initial, initial_snapshot) = binding(&scope, original(), true);
    let confirmed = Node {
        id: NEW.into(),
        etag: Some("new-v2".into()),
        ..original()
    };
    let (committed, committed_snapshot) = binding(&scope, confirmed.clone(), false);
    let metadata = Arc::new(MountedArchive {
        initial,
        initial_snapshot,
        committed,
        committed_snapshot,
        server: Mutex::new(None),
        resolutions: AtomicUsize::new(0),
    });
    let journal_root = root.path().join("journal");
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&journal_root, &account.id, 8 * 1024 * 1024).unwrap(),
    ));
    let engine = Engine::new(
        account.clone(),
        metadata.clone(),
        state.path().to_path_buf(),
    )
    .await
    .unwrap();
    engine.start().await.unwrap();
    // No background transfer pump: observe the exact FUSE-created Pending row first.
    let fs = CloudFs::new_experimental_writable(engine.clone(), journal.clone())
        .await
        .unwrap();
    let control = fs.write_control().unwrap();
    let session = tokio::task::spawn_blocking({
        let mount = mount.clone();
        move || fs.mount(&mount)
    })
    .await
    .unwrap()
    .unwrap();
    let path = mount.join("Target.pages").join("Target.pages");
    let (held, edit) = tokio::task::spawn_blocking({
        let path = path.clone();
        let bytes = new_bytes.clone();
        move || {
            // Do not read the original before completion: cached bytes cannot hide a lost reader.
            let held = std::fs::File::open(&path).unwrap();
            let mut edit = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .unwrap();
            edit.set_len(0).unwrap();
            edit.write_all(&bytes).unwrap();
            edit.sync_all()
                .expect("native FUSE save must seal a typed operation");
            assert_eq!(std::fs::read(path).unwrap(), bytes);
            (held, edit)
        }
    })
    .await
    .unwrap();
    let queued = {
        let j = journal.lock().unwrap();
        let rows = j.list(0, 10).unwrap();
        assert_eq!(rows.len(), 1);
        let row = rows[0].clone();
        assert_eq!(row.state, UploadState::Pending);
        assert!(row.attempt.is_none());
        assert!(row.remote.is_none());
        assert!(row.package_completion.is_none());
        assert!(row.base.is_none());
        let UploadRepresentation::PackageReplacementArchive {
            original: before,
            original_semantic,
            semantic: new_semantic,
            ..
        } = &row.representation
        else {
            panic!("FUSE save downgraded to file bytes")
        };
        assert_eq!(before.as_ref(), &original());
        assert_eq!(original_semantic, &semantic("Target.pages", true, 2));
        assert_eq!(new_semantic, &semantic("Target.pages", false, 2));
        assert_eq!(row.sha256, hex::encode(Sha256::digest(&new_bytes)));
        row
    };
    let server = Server::start(
        Plan {
            target_name: "Target.pages".into(),
            staged_name: format!("staged-by-cirrove-{}.pages", queued.id),
        },
        new_bytes.clone(),
        false,
        false,
        false,
        None,
    )
    .await;
    *metadata.server.lock().unwrap() = Some(server.state.clone());
    assert_eq!(counts(&server), (0, 0, 0, 0, 0));
    let keys = Arc::new(WrappingKeys::default());
    let vault = Arc::new(
        SealedUploadCheckpointVault::with_test_key_vault(state.path(), &account.id, keys).unwrap(),
    );
    let worker = TransferWorker::new(
        journal.clone(),
        provider(&server, staging.path(), &queued),
        vault,
        CancellationToken::new(),
    );
    let result = tokio::time::timeout(Duration::from_secs(30), worker.run_once())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result.id, queued.id);
    assert_eq!(result.state, UploadState::Uploaded);
    assert!(result.issue.is_none());
    drop(worker);
    {
        let j = journal.lock().unwrap();
        let rows = j.list(0, 10).unwrap();
        assert_eq!(rows.len(), 1);
        let saved = &rows[0];
        assert_eq!(saved.id, queued.id);
        assert!(
            saved.representation == queued.representation,
            "sealed upload representation changed during native worker completion"
        );
        assert_eq!(
            saved.package_completion,
            Some(semantic("Target.pages", false, 2))
        );
        let (before, current, backup) = saved
            .native_replacement_receipt()
            .expect("actual HTTPS worker must return a typed native handoff");
        assert_eq!(before, &original());
        assert_eq!(current, &confirmed);
        assert_eq!(backup.id, OLD);
        assert_eq!(backup.parent_id.as_deref(), Some(TRASH_ROOT));
        assert_eq!(backup.etag.as_deref(), Some("trash-v2"));
        assert_eq!(backup.name, "Target.pages");
        assert!(backup.package);
        let mut payload = Vec::new();
        j.payload(queued.id)
            .unwrap()
            .read_to_end(&mut payload)
            .unwrap();
        assert_eq!(payload, new_bytes);
    }
    assert_eq!(counts(&server), (1, 1, 1, 1, 1));
    assert!(
        control.publish_completed_package().await.unwrap(),
        "actual receipt must converge through exact fresh metadata observation"
    );
    {
        let j = journal.lock().unwrap();
        assert_eq!(
            j.package_publication_status(queued.id).unwrap(),
            PackagePublicationStatus::Present(confirmed.clone())
        );
    }
    tokio::task::spawn_blocking({
        let path = path.clone();
        let original = old_bytes.clone();
        let current = new_bytes.clone();
        move || {
            let mut held = held;
            let edit = edit;
            held.seek(SeekFrom::Start(0)).unwrap();
            let mut retained = Vec::new();
            held.read_to_end(&mut retained).unwrap();
            assert_eq!(retained, original);
            assert_eq!(std::fs::read(path).unwrap(), current);
            drop(edit);
            drop(held);
        }
    })
    .await
    .unwrap();
    engine.stop().await;
    tokio::task::spawn_blocking(move || session.umount_and_join())
        .await
        .unwrap()
        .unwrap();
    drop(control);
    drop(engine);
    drop(journal);
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&journal_root, &account.id, 8 * 1024 * 1024).unwrap(),
    ));
    let engine = Engine::new(account, metadata, state.path().to_path_buf())
        .await
        .unwrap();
    engine.start().await.unwrap();
    let fs = CloudFs::new_experimental_writable(engine.clone(), journal.clone())
        .await
        .unwrap();
    let session = tokio::task::spawn_blocking({
        let mount = mount.clone();
        move || fs.mount(&mount)
    })
    .await
    .unwrap()
    .unwrap();
    tokio::task::spawn_blocking({
        let path = path.clone();
        let bytes = new_bytes.clone();
        move || {
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .unwrap();
            file.sync_all().unwrap();
        }
    })
    .await
    .unwrap();
    {
        let rows = journal.lock().unwrap().list(0, 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, queued.id);
        assert_eq!(rows[0].state, UploadState::Uploaded);
        assert_eq!(rows[0].remote.as_ref(), Some(&confirmed));
    }
    assert_eq!(
        counts(&server),
        (1, 1, 1, 1, 1),
        "remount and clean fsync must not replay a cloud mutation"
    );
    engine.stop().await;
    tokio::task::spawn_blocking(move || session.umount_and_join())
        .await
        .unwrap()
        .unwrap();
}

#[path = "fuse_https/chain.rs"]
mod chain;
