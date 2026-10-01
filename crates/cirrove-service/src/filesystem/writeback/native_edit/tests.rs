#![allow(clippy::unwrap_used)]
use super::*;
use cirrove_core::{
    ChangePage, Cursor, DirectoryPage, MetadataProvider, ReadProvider, reads::NativeArchiveBinding,
};
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Snapshot {
    identity: ReadIdentity,
    bytes: Vec<u8>,
}
#[async_trait::async_trait]
impl ReadSession for Snapshot {
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
struct Provider {
    binding: NativeArchiveBinding,
    parent: Node,
    session: Arc<Snapshot>,
    calls: AtomicUsize,
    pause: Mutex<Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>>,
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
        [&self.parent, &self.binding.source, &self.binding.archive]
            .into_iter()
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
            nodes: [&self.parent, &self.binding.source, &self.binding.archive]
                .into_iter()
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
        Err(ProviderError::Unavailable)
    }
    async fn resolve_native_archive(
        &self,
        scope: &Scope,
        node: &Node,
        _: &CancellationToken,
    ) -> std::result::Result<Option<NativeArchiveBinding>, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let pause = self.pause.lock().unwrap().clone();
        if let Some((ready, release)) = pause {
            ready.notify_one();
            release.notified().await;
        }
        if scope != &self.binding.scope || node != &self.binding.archive {
            return Err(ProviderError::VersionChanged);
        }
        Ok(Some(self.binding.clone()))
    }
    async fn staged_content_session(
        &self,
        scope: &Scope,
        node: &Node,
        _: &CancellationToken,
    ) -> std::result::Result<Option<Arc<dyn ReadSession>>, ProviderError> {
        if node.id != self.binding.archive.id {
            return Ok(None);
        }
        if ReadIdentity::new(scope, node)? != self.session.identity {
            return Err(ProviderError::VersionChanged);
        }
        Ok(Some(self.session.clone()))
    }
}
struct Fixture {
    _temp: tempfile::TempDir,
    fs: CloudFs,
    provider: Arc<Provider>,
    journal: Arc<Mutex<UploadJournal>>,
    view: View,
}
impl Fixture {
    async fn new(access: cirrove_auth::AccessMode) -> Self {
        let temp = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in("/var/tmp")
            .unwrap();
        let bytes = crate::native_import::synthetic_package_archive(
            "Owned.pages/Document",
            b"original immutable bytes",
        );
        let path = temp.path().join("source.zip");
        std::fs::write(&path, &bytes).unwrap();
        let validated = crate::native_import::ValidatedPackageArchive::capture(
            &path,
            temp.path(),
            "Owned.pages",
            &CancellationToken::new(),
        )
        .unwrap();
        let (_, representation, _, sha) = validated.into_parts();
        let cirrove_core::upload::UploadRepresentation::PackageArchive { semantic, .. } =
            representation
        else {
            panic!("archive")
        };
        let account = crate::accounts::Account {
            id: Uuid::new_v4().to_string(),
            label: "native edit fixture".into(),
            registration: cirrove_auth::AppRegistration::ICloud,
            identity: cirrove_auth::Identity {
                tenant_id: String::new(),
                subject: "owned".into(),
                username: "fixture@example.invalid".into(),
                graph_user_id: "owned".into(),
                display_name: "Synthetic".into(),
            },
            credential_id: Uuid::new_v4().to_string(),
            access,
            drive: cirrove_core::CollectionInfo {
                id: "drive".into(),
                name: "Synthetic".into(),
                drive_type: "icloud".into(),
                web_url: String::new(),
            },
            root_id: "FOLDER::com.apple.CloudDocs::root".into(),
            mount_path: temp.path().join("mount"),
            enabled: true,
            poll_seconds: 3600,
            cache_bytes: 16 * 1024 * 1024,
        };
        let scope = Scope {
            account: account.id.clone(),
            provider: "icloud".into(),
            collection: "drive".into(),
        };
        let parent = Node {
            id: "FOLDER::com.apple.CloudDocs::owned".into(),
            parent_id: Some(account.root_id.clone()),
            name: "Owned".into(),
            kind: NodeKind::Folder,
            size: 0,
            modified_unix: 0,
            etag: Some("parent-v1".into()),
            content_version: None,
            target: None,
            package: false,
        };
        let source = Node {
            id: "FILE::com.apple.CloudDocs::original".into(),
            parent_id: Some(parent.id.clone()),
            name: "Owned.pages".into(),
            kind: NodeKind::Folder,
            size: 17,
            modified_unix: 0,
            etag: Some("v1".into()),
            content_version: None,
            target: None,
            package: true,
        };
        let archive = Node {
            id: format!("icloud-artifact:{}", source.id),
            parent_id: Some(source.id.clone()),
            name: source.name.clone(),
            kind: NodeKind::File,
            size: bytes.len() as u64,
            modified_unix: 0,
            etag: None,
            content_version: Some(format!(
                "icloud-artifact-v2:{}",
                serde_json::json!({"source_etag":"v1","source_size":17,"source_parent":parent.id,"sha256":sha})
            )),
            target: None,
            package: false,
        };
        let provider = Arc::new(Provider {
            binding: NativeArchiveBinding {
                scope: scope.clone(),
                source,
                archive: archive.clone(),
                semantic,
            },
            parent,
            session: Arc::new(Snapshot {
                identity: ReadIdentity::new(&scope, &archive).unwrap(),
                bytes,
            }),
            calls: AtomicUsize::new(0),
            pause: Mutex::new(None),
        });
        let engine = Engine::new(account.clone(), provider.clone(), temp.path().join("state"))
            .await
            .unwrap();
        let journal = Arc::new(Mutex::new(
            UploadJournal::open(&temp.path().join("journal"), &account.id, 16 * 1024 * 1024)
                .unwrap(),
        ));
        let fs = if account.access == cirrove_auth::AccessMode::ReadWrite {
            CloudFs::new_experimental_writable(engine, journal.clone())
                .await
                .unwrap()
        } else {
            CloudFs::new(engine).unwrap()
        };
        let root = fs.inner.view(1).unwrap();
        let folder = fs
            .inner
            .insert(&root, provider.parent.clone())
            .await
            .unwrap();
        let source = fs
            .inner
            .insert(&folder, provider.binding.source.clone())
            .await
            .unwrap();
        let mut view = fs.inner.insert(&source, archive.clone()).await.unwrap();
        // RO production views elide metadata; preserve fixture selection for refusal assertions.
        view.node = Some(Arc::new(archive));
        Self {
            _temp: temp,
            fs,
            provider,
            journal,
            view,
        }
    }
    fn open(&self, flags: i32) -> Arc<OpenFile> {
        self.fs
            .inner
            .writeback
            .as_ref()
            .unwrap()
            .register_open(OpenFile {
                view: self.view.clone(),
                node: self.provider.binding.archive.clone(),
                flags,
                _lease: None,
                remote_reads: tokio_util::task::TaskTracker::new(),
                native_snapshot: std::sync::OnceLock::new(),
            })
            .unwrap()
    }
}
#[tokio::test]
async fn native_path_first_write_rebinds_handle_and_preserves_original_reader() {
    let f = Fixture::new(cirrove_auth::AccessMode::ReadWrite).await;
    let old = f.open(libc::O_RDONLY);
    let preparing = f.open(libc::O_RDWR);
    let inner = &f.fs.inner;
    let writer = inner.writeback.as_ref().unwrap();
    let record = inner
        .prepare_path_edit(&f.view, f.view.node.as_deref(), Some(0))
        .await
        .unwrap();
    let opened = writer
        .native_open_file(&preparing, record.clone(), &inner.cancel)
        .await
        .unwrap();
    assert_ne!(opened.view.id, old.view.id);
    assert_eq!(
        writer
            .working(&opened.view.scope, &opened.view.id)
            .unwrap()
            .unwrap()
            .id,
        record.id
    );
    let edited = crate::native_import::synthetic_package_archive(
        "Owned.pages/Document",
        b"new working bytes",
    );
    writer
        .write(record.id, 0, edited.clone(), false)
        .await
        .unwrap();
    writer.seal(record.id).await.unwrap();
    assert!(
        matches!(writer.read_source(&opened).unwrap(),ReadSource::Working(id) if id==record.id)
    );
    let ReadSource::Native(snapshot) = writer.read_source(&old).unwrap() else {
        panic!("old reader lost immutable snapshot")
    };
    assert_eq!(
        read_snapshot(snapshot, 0, 1_000_000, &inner.cancel)
            .await
            .unwrap(),
        f.provider.session.bytes
    );
    assert_eq!(writer.read(record.id, 0, 1_000_000).await.unwrap(), edited);
    let current = inner.view(f.view.inode).unwrap();
    assert_eq!(current.id.as_ref(), record.node.id.as_str());
    let reopened = inner
        .prepare_path_edit(&current, current.node.as_deref(), None)
        .await
        .unwrap();
    assert_eq!(reopened.id, record.id);
    assert_eq!(
        f.provider.calls.load(Ordering::SeqCst),
        1,
        "existing native stream re-resolved immutable original"
    );
    assert_eq!(f.journal.lock().unwrap().list(0, 10).unwrap().len(), 1);
}
#[tokio::test]
async fn native_path_refuses_readonly_foreign_scope_preview_and_shortcut_before_hydration() {
    let ro = Fixture::new(cirrove_auth::AccessMode::ReadOnly).await;
    assert!(
        ro.fs
            .inner
            .prepare_path_edit(&ro.view, ro.view.node.as_deref(), None)
            .await
            .is_err()
    );
    assert_eq!(ro.provider.calls.load(Ordering::SeqCst), 0);
    let f = Fixture::new(cirrove_auth::AccessMode::ReadWrite).await;
    for arm in 0..3 {
        let mut view = f.view.clone();
        match arm {
            0 => Arc::make_mut(&mut view.scope).account = "foreign".into(),
            1 => {
                let n = Arc::make_mut(view.node.as_mut().unwrap());
                n.name = "Preview.pdf".into();
            }
            _ => view.reference = true,
        }
        assert!(
            f.fs.inner
                .prepare_path_edit(&view, view.node.as_deref(), None)
                .await
                .is_err(),
            "arm {arm}"
        );
    }
    assert_eq!(f.provider.calls.load(Ordering::SeqCst), 0);
    assert!(
        f.journal
            .lock()
            .unwrap()
            .working_files()
            .unwrap()
            .is_empty()
    );
}
#[tokio::test]
async fn native_path_paused_resolution_rechecks_journal_and_cancellation_before_publish() {
    for cancelled in [false, true] {
        let f = Fixture::new(cirrove_auth::AccessMode::ReadWrite).await;
        let ready = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        *f.provider.pause.lock().unwrap() = Some((ready.clone(), release.clone()));
        let inner = f.fs.inner.clone();
        let view = f.view.clone();
        let task = tokio::spawn(async move {
            inner
                .prepare_path_edit(&view, view.node.as_deref(), Some(0))
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), ready.notified())
            .await
            .unwrap();
        if cancelled {
            f.fs.inner.cancel.cancel();
        } else {
            f.journal
                .lock()
                .unwrap()
                .create_namespace_directory(
                    f.provider.binding.scope.clone(),
                    f.provider.parent.id.clone(),
                    "raced".into(),
                )
                .unwrap();
        }
        release.notify_one();
        assert!(task.await.unwrap().is_err());
        assert!(
            f.journal
                .lock()
                .unwrap()
                .working_files()
                .unwrap()
                .is_empty()
        );
        assert!(f.journal.lock().unwrap().list(0, 10).unwrap().is_empty());
    }
}

#[tokio::test]
async fn native_path_existing_remote_read_token_fences_pin_before_hydration() {
    use std::future::Future;
    use std::task::Poll;
    let f = Fixture::new(cirrove_auth::AccessMode::ReadWrite).await;
    let old = f.open(libc::O_RDONLY);
    let inner = &f.fs.inner;
    let writer = inner.writeback.as_ref().unwrap();
    let ReadSource::Remote(_, inflight) = writer.read_source(&old).unwrap() else {
        panic!("fixture must begin an actual remote read before pinning")
    };
    assert!(old.native_snapshot.get().is_none());
    let mut pin = Box::pin(writer.pin_native_readers(
        &f.view.scope,
        &old.node,
        f.provider.session.clone(),
        &inner.cancel,
    ));
    // Poll the real pin operation exactly once. It has no asynchronous work
    // before TaskTracker::wait, so Pending proves this specific barrier; there
    // is no scheduler sleep or unrelated provider await to mask removing wait.
    std::future::poll_fn(|cx| {
        assert!(
            matches!(pin.as_mut().poll(cx), Poll::Pending),
            "native snapshot pin returned while an earlier remote read still held its token"
        );
        Poll::Ready(())
    })
    .await;
    assert!(old.native_snapshot.get().is_some());
    assert!(old.remote_reads.is_closed());
    assert!(
        matches!(writer.read_source(&old).unwrap(), ReadSource::Native(_)),
        "later reads must not obtain another remote token after close"
    );
    assert!(f.journal.lock().unwrap().list(0, 10).unwrap().is_empty());
    drop(inflight);
    tokio::time::timeout(std::time::Duration::from_secs(5), pin)
        .await
        .expect("pin must finish once the earlier read completes")
        .unwrap();
    let record = inner
        .prepare_path_edit(&f.view, f.view.node.as_deref(), Some(0))
        .await
        .unwrap();
    assert!(record.native);
    assert_eq!(
        writer.read(record.id, 0, 100).await.unwrap(),
        Vec::<u8>::new()
    );
    let ReadSource::Native(snapshot) = writer.read_source(&old).unwrap() else {
        panic!("original reader lost pin after admission")
    };
    assert_eq!(
        read_snapshot(snapshot, 0, 1_000_000, &inner.cancel)
            .await
            .unwrap(),
        f.provider.session.bytes
    );
}
