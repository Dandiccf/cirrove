//! Actual FUSE in-place editing and held-reader invariants. Provider completion
//! is a synthetic typed receipt injected while the fixture uploader is paused;
//! this test does not claim Apple transport or application save acceptance.
use super::*;
use cirrove_core::reads::{NativeArchiveBinding, ReadIdentity, ReadSession};
use cirrove_core::upload::{
    PackageHandoffReceipt, PackageUploadReceipt, RecoveryLocation, UploadRepresentation,
};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

fn archive(body: &[u8]) -> Vec<u8> {
    let name = b"Owned.pages/Document";
    let mut crc = !0u32;
    for byte in body {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 == 1 { 0xedb88320 } else { 0 };
        }
    }
    crc = !crc;
    let mut out = Vec::new();
    let u16s = |out: &mut Vec<u8>, values: &[u16]| {
        for v in values {
            out.extend_from_slice(&v.to_le_bytes());
        }
    };
    let u32s = |out: &mut Vec<u8>, values: &[u32]| {
        for v in values {
            out.extend_from_slice(&v.to_le_bytes());
        }
    };
    u32s(&mut out, &[0x04034b50]);
    u16s(&mut out, &[20, 0, 0, 0, 33]);
    u32s(&mut out, &[crc, body.len() as u32, body.len() as u32]);
    u16s(&mut out, &[name.len() as u16, 0]);
    out.extend_from_slice(name);
    out.extend_from_slice(body);
    let central = out.len() as u32;
    u32s(&mut out, &[0x02014b50]);
    u16s(&mut out, &[0x0314, 20, 0, 0, 0, 33]);
    u32s(&mut out, &[crc, body.len() as u32, body.len() as u32]);
    u16s(&mut out, &[name.len() as u16, 0, 0, 0, 0]);
    u32s(&mut out, &[0o100600 << 16, 0]);
    out.extend_from_slice(name);
    let length = out.len() as u32 - central;
    u32s(&mut out, &[0x06054b50]);
    u16s(&mut out, &[0, 0, 1, 1]);
    u32s(&mut out, &[length, central]);
    u16s(&mut out, &[0]);
    out
}
struct Snapshot {
    identity: ReadIdentity,
    bytes: Vec<u8>,
}
#[async_trait]
impl ReadSession for Snapshot {
    fn identity(&self) -> &ReadIdentity {
        &self.identity
    }
    async fn read_range(
        &self,
        offset: u64,
        length: u32,
        _: &CancellationToken,
    ) -> Result<Vec<u8>, ProviderError> {
        let start = (offset as usize).min(self.bytes.len());
        let end = start.saturating_add(length as usize).min(self.bytes.len());
        Ok(self.bytes[start..end].to_vec())
    }
}
struct NativeCloud {
    cloud: Arc<Cloud>,
    binding: NativeArchiveBinding,
    snapshot: Arc<Snapshot>,
    resolutions: AtomicUsize,
}
#[async_trait]
impl MetadataProvider for NativeCloud {
    fn provider_id(&self) -> &'static str {
        "icloud"
    }
    async fn changes(
        &self,
        s: &Scope,
        c: Option<&Cursor>,
        t: &CancellationToken,
    ) -> Result<ChangePage, ProviderError> {
        self.cloud.changes(s, c, t).await
    }
}
#[async_trait]
impl ReadProvider for NativeCloud {
    async fn node(
        &self,
        s: &Scope,
        id: &str,
        t: &CancellationToken,
    ) -> Result<Node, ProviderError> {
        self.cloud.node(s, id, t).await
    }
    async fn children(
        &self,
        s: &Scope,
        p: &str,
        c: Option<&Cursor>,
        t: &CancellationToken,
    ) -> Result<DirectoryPage, ProviderError> {
        self.cloud.children(s, p, c, t).await
    }
    async fn read_range(
        &self,
        s: &Scope,
        n: &Node,
        o: u64,
        l: u32,
        t: &CancellationToken,
    ) -> Result<Vec<u8>, ProviderError> {
        self.cloud.read_range(s, n, o, l, t).await
    }
    async fn resolve_native_archive(
        &self,
        s: &Scope,
        n: &Node,
        _: &CancellationToken,
    ) -> Result<Option<NativeArchiveBinding>, ProviderError> {
        self.resolutions.fetch_add(1, Ordering::SeqCst);
        if s != &self.binding.scope || n != &self.binding.archive {
            return Err(ProviderError::VersionChanged);
        }
        Ok(Some(self.binding.clone()))
    }
    async fn staged_content_session(
        &self,
        s: &Scope,
        n: &Node,
        _: &CancellationToken,
    ) -> Result<Option<Arc<dyn ReadSession>>, ProviderError> {
        // Do not prepopulate the shared content cache during initial listing.
        // Only explicit native resolution makes the immutable session available.
        if n.id != self.binding.archive.id || self.resolutions.load(Ordering::SeqCst) == 0 {
            return Ok(None);
        }
        if ReadIdentity::new(s, n)? != self.snapshot.identity {
            return Ok(None);
        }
        Ok(Some(self.snapshot.clone()))
    }
}
fn publication_done(db: &Path, operation: uuid::Uuid) -> bool {
    let db = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap();
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM package_metadata_publication WHERE operation=?1 AND done=1)",
        [operation.to_string()],
        |r| r.get(0),
    )
    .unwrap()
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE"]
async fn real_native_archive_first_open_in_place_fsync_retains_old_reader_and_reopens_local() {
    native_archive_edit(FirstEdit::Handle).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE"]
async fn real_native_archive_pathname_truncate_fsync_retains_old_reader() {
    native_archive_edit(FirstEdit::Path).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE"]
async fn real_native_archive_open_truncate_fsync_retains_old_reader() {
    native_archive_edit(FirstEdit::Open).await;
}
#[derive(Clone, Copy)]
enum FirstEdit {
    Atomic,
    Handle,
    Path,
    Open,
}
async fn native_archive_edit(first: FirstEdit) {
    let temp = tempfile::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let mut account = account(&mount);
    account.registration = AppRegistration::ICloud;
    account.root_id = "FOLDER::com.apple.CloudDocs::root".into();
    let scope = Scope {
        account: account.id.clone(),
        provider: "icloud".into(),
        collection: "drive".into(),
    };
    let original = Node {
        id: "FILE::com.apple.CloudDocs::owned-native".into(),
        parent_id: Some(account.root_id.clone()),
        name: "Owned.pages".into(),
        kind: NodeKind::Folder,
        size: 17,
        modified_unix: 1,
        etag: Some("old-v1".into()),
        content_version: None,
        target: None,
        package: true,
    };
    let oldbytes = archive(&vec![17u8; 128 * 1024]);
    let newbytes = archive(&vec![29u8; 96 * 1024]);
    let disk = temp.path().join("original.zip");
    std::fs::write(&disk, &oldbytes).unwrap();
    let raw = cirrove_icloud::PackageDownload {
        size: oldbytes.len() as u64,
        sha256: hex::encode(Sha256::digest(&oldbytes)),
    };
    let semantic = cirrove_icloud::package_archive_semantic_identity_versioned(
        &std::fs::File::open(&disk).unwrap(),
        &raw,
        "Owned.pages",
        2,
        &CancellationToken::new(),
    )
    .unwrap();
    let artifact = Node {
        id: format!("icloud-artifact:{}", original.id),
        parent_id: Some(original.id.clone()),
        name: original.name.clone(),
        kind: NodeKind::File,
        size: raw.size,
        modified_unix: 1,
        etag: None,
        content_version: Some(format!(
            "icloud-artifact-v2:{}",
            serde_json::json!({"source_etag":"old-v1","source_parent":original.parent_id,"source_size":original.size,"sha256":raw.sha256})
        )),
        target: None,
        package: false,
    };
    let cloud = Arc::new(Cloud::default());
    cloud.icloud_identity.store(true, Ordering::SeqCst);
    cloud.stall.store(true, Ordering::SeqCst);
    {
        let mut remote = cloud.remote.lock().unwrap();
        remote
            .files
            .insert(original.id.clone(), (original.clone(), vec![]));
        remote
            .files
            .insert(artifact.id.clone(), (artifact.clone(), oldbytes.clone()));
    }
    let provider = Arc::new(NativeCloud {
        cloud: cloud.clone(),
        binding: NativeArchiveBinding {
            scope: scope.clone(),
            source: original.clone(),
            archive: artifact.clone(),
            semantic: semantic.clone(),
        },
        snapshot: Arc::new(Snapshot {
            identity: ReadIdentity::new(&scope, &artifact).unwrap(),
            bytes: oldbytes.clone(),
        }),
        resolutions: AtomicUsize::new(0),
    });
    let journalroot = temp.path().join("journal");
    let db = journalroot.join("uploads.db");
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&journalroot, &account.id, 8 * 1024 * 1024).unwrap(),
    ));
    let engine = Engine::new(account.clone(), provider.clone(), temp.path().join("state"))
        .await
        .unwrap();
    let session = WritableSession::mount(
        engine.clone(),
        journal.clone(),
        cloud.clone(),
        Arc::new(Vault::default()),
    )
    .await
    .unwrap();
    let path = mount.join("Owned.pages/Owned.pages");
    let (held, mut edit, old_writer) = tokio::task::spawn_blocking({
        let path = path.clone();
        let bytes = newbytes.clone();
        let journal = journal.clone();
        move || {
            // Hold without reading: content cache must not mask missing snapshot retention.
            let held = std::fs::File::open(&path).unwrap();
            if matches!(first, FirstEdit::Atomic) {
                let temp_path = path.parent().unwrap().join(".editor-save");
                let mut temporary = std::fs::OpenOptions::new()
                    .create_new(true)
                    .read(true)
                    .write(true)
                    .open(&temp_path)
                    .expect("native local temp creation");
                temporary.write_all(b"incomplete archive").unwrap();
                temporary.sync_all().unwrap();
                assert!(
                    std::fs::rename(&temp_path, &path).is_err(),
                    "invalid native archive was renamed over canonical"
                );
                assert!(temp_path.exists());
                assert!(journal.lock().unwrap().list(0, 10).unwrap().is_empty());
                assert!(
                    std::fs::rename(
                        &temp_path,
                        path.parent().unwrap().parent().unwrap().join("escape")
                    )
                    .is_err(),
                    "native temp escaped its source owner"
                );
                let reopen = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&temp_path)
                    .expect("reopen exact local temp");
                reopen.sync_all().unwrap();
                drop(reopen);
                temporary.set_len(0).unwrap();
                temporary.seek(SeekFrom::Start(0)).unwrap();
                temporary.write_all(&bytes).unwrap();
                temporary
                    .sync_all()
                    .expect("native temp fsync is local only");
                assert!(
                    journal.lock().unwrap().list(0, 10).unwrap().is_empty(),
                    "temporary fsync submitted cloud bytes"
                );
                let mut old = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&path)
                    .unwrap();
                let old_inode = old.metadata().unwrap().ino();
                std::fs::rename(&temp_path, &path).expect("native canonical rename-over");
                assert!(!temp_path.exists());
                assert_ne!(std::fs::metadata(&path).unwrap().ino(), old_inode);
                assert_eq!(old.metadata().unwrap().nlink(), 0);
                old.set_len(0).unwrap();
                old.write_all(&archive(b"detached late edit")).unwrap();
                old.sync_all().expect("detached old fd fsync stays local");
                assert_eq!(std::fs::read(&path).unwrap(), bytes);
                assert_eq!(journal.lock().unwrap().list(0, 10).unwrap().len(), 1);
                return (held, temporary, Some(old));
            }

            if matches!(first, FirstEdit::Path) {
                // Python os.truncate(path,0) calls the pathname syscall. A
                // coreutils truncate invocation could hide an open/ftruncate.
                let status = std::process::Command::new("python3")
                    .args(["-c", "import os,sys; os.truncate(sys.argv[1],0)"])
                    .arg(&path)
                    .status()
                    .expect("synthetic pathname truncate process");
                assert!(status.success(), "first pathname native truncate failed");
                assert_eq!(
                    std::fs::metadata(&path).unwrap().len(),
                    0,
                    "pathname truncate did not publish empty working bytes"
                );
            }
            let mut edit = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .truncate(matches!(first, FirstEdit::Open))
                .open(&path)
                .expect("first writable original artifact open");
            if matches!(first, FirstEdit::Handle) {
                edit.set_len(0).unwrap();
            }
            assert_eq!(
                edit.metadata().unwrap().len(),
                0,
                "truncate did not affect the admitted local native stream"
            );
            edit.write_all(&bytes).unwrap();
            edit.sync_all().expect("typed native fsync");
            (held, edit, None)
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(10), cloud.entered.notified())
        .await
        .unwrap();
    let row = {
        let mut j = journal.lock().unwrap();
        let rows = j.list(0, 10).unwrap();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        let UploadRepresentation::PackageReplacementArchive {
            original: before,
            original_semantic,
            semantic: newsemantic,
            ..
        } = &row.representation
        else {
            panic!("native fsync downgraded into FileBytes")
        };
        assert_eq!(before.as_ref(), &original);
        assert_eq!(original_semantic, &semantic);
        let newsemantic = newsemantic.clone();
        let id = row.id;
        let attempt = row.attempt.unwrap();
        let current = Node {
            id: "FILE::com.apple.CloudDocs::confirmed-successor".into(),
            etag: Some("new-v1".into()),
            size: 31,
            ..original.clone()
        };
        j.reserve_identity_handoff(
            id,
            attempt,
            RecoveryLocation::Trash {
                parent: "FOLDER::com.apple.CloudDocs::TRASH_ROOT".into(),
                local_name: format!("recovery-by-cirrove-{id}.pages"),
            },
        )
        .unwrap();
        {
            let mut remote = cloud.remote.lock().unwrap();
            remote.files.remove(&original.id);
            remote.files.remove(&artifact.id);
            remote
                .files
                .insert(current.id.clone(), (current.clone(), vec![]));
        }
        j.acknowledge_package_handoff(
            id,
            attempt,
            PackageHandoffReceipt {
                original: original.clone(),
                current: PackageUploadReceipt {
                    remote: current,
                    semantic: newsemantic,
                },
                backup: PackageUploadReceipt {
                    remote: Node {
                        parent_id: Some("FOLDER::com.apple.CloudDocs::TRASH_ROOT".into()),
                        etag: Some("trash-v2".into()),
                        ..original.clone()
                    },
                    semantic: semantic.clone(),
                },
            },
        )
        .unwrap();
        j.get(id).unwrap()
    };
    cloud.offline.store(true, Ordering::SeqCst);
    tokio::time::timeout(Duration::from_secs(10), async {
        while !publication_done(&db, row.id) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("native working receipt publication did not complete");
    let reads = cloud.reads.load(Ordering::SeqCst);
    tokio::task::spawn_blocking({
        let old = oldbytes.clone();
        let new = newbytes.clone();
        let path = path.clone();
        move || {
            let mut held = held;
            let mut bytes = vec![];
            held.read_to_end(&mut bytes).unwrap();
            assert_eq!(
                bytes, old,
                "held original reader changed or attempted remote refetch"
            );
            edit.seek(SeekFrom::Start(0)).unwrap();
            bytes.clear();
            edit.read_to_end(&mut bytes).unwrap();
            assert_eq!(bytes, new);
            assert_eq!(
                std::fs::read(&path).unwrap(),
                new,
                "new read-only open did not use local native stream"
            );
            let reopen = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .unwrap();
            reopen.sync_all().unwrap();
        }
    })
    .await
    .unwrap();
    assert_eq!(
        cloud.reads.load(Ordering::SeqCst),
        reads,
        "held/new readers fetched removed cloud content"
    );
    assert_eq!(provider.resolutions.load(Ordering::SeqCst), 1);
    assert_eq!(
        journal.lock().unwrap().list(0, 10).unwrap().len(),
        1,
        "clean reopen/fsync replayed save"
    );
    if let Some(mut old) = old_writer {
        tokio::task::spawn_blocking(move || {
            old.seek(std::io::SeekFrom::Start(0)).unwrap();
            let mut bytes = Vec::new();
            old.read_to_end(&mut bytes).unwrap();
            assert_eq!(bytes, archive(b"detached late edit"));
        })
        .await
        .unwrap();
    }
    session.shutdown().await.unwrap();
    // The same provider data must never confer a write grant on a RO mount.
    {
        let mut remote = cloud.remote.lock().unwrap();
        remote.files.clear();
        remote
            .files
            .insert(original.id.clone(), (original.clone(), vec![]));
        remote
            .files
            .insert(artifact.id.clone(), (artifact.clone(), oldbytes));
    }
    cloud.offline.store(false, Ordering::SeqCst);
    let romount = temp.path().join("readonly");
    std::fs::create_dir(&romount).unwrap();
    account.mount_path = romount.clone();
    account.access = AccessMode::ReadOnly;
    let roengine = Engine::new(account, provider, temp.path().join("ro-state"))
        .await
        .unwrap();
    roengine.start().await.unwrap();
    let fs = cirrove_service::filesystem::CloudFs::new(roengine.clone()).unwrap();
    let rosession = tokio::task::spawn_blocking({
        let romount = romount.clone();
        move || fs.mount(&romount)
    })
    .await
    .unwrap()
    .unwrap();
    tokio::task::spawn_blocking(move || {
        let path = romount.join("Owned.pages/Owned.pages");
        assert!(std::fs::metadata(&path).unwrap().is_file());
        if matches!(first,FirstEdit::Path) {
            let status=std::process::Command::new("python3")
                .args(["-c","import errno,os,sys\ntry: os.truncate(sys.argv[1],0)\nexcept OSError as e: assert e.errno == errno.EROFS\nelse: raise AssertionError('read-only native pathname truncate succeeded')"])
                .arg(&path).status().expect("read-only pathname truncate process");
            assert!(status.success(),"read-only pathname truncate did not refuse EROFS");
        }
        let error = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .truncate(matches!(first,FirstEdit::Open))
            .open(&path)
            .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EROFS));
    })
    .await
    .unwrap();
    roengine.stop().await;
    tokio::task::spawn_blocking(move || rosession.umount_and_join())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires /dev/fuse"]
async fn real_native_archive_atomic_temp_fsync_rename_keeps_old_descriptors_local() {
    native_archive_edit(FirstEdit::Atomic).await;
}

/// Kernel namespace/durable queue boundary only: no upload worker is started.
/// Real worker/transport tests are separate, and no synthetic receipt is needed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires /dev/fuse"]
async fn real_native_atomic_pending_chain_reopens_mount_without_submission() {
    native_pending_chain(false).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires /dev/fuse"]
async fn real_native_backup_first_gap_rollback_promote_reopen_and_suffix_reuse() {
    native_pending_chain(true).await;
}
async fn native_pending_chain(backup_first: bool) {
    let temp = tempfile::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let mut account = account(&mount);
    account.registration = AppRegistration::ICloud;
    account.root_id = "FOLDER::com.apple.CloudDocs::root".into();
    let scope = Scope {
        account: account.id.clone(),
        provider: "icloud".into(),
        collection: "drive".into(),
    };
    let original = Node {
        id: "FILE::com.apple.CloudDocs::owned-native".into(),
        parent_id: Some(account.root_id.clone()),
        name: "Owned.pages".into(),
        kind: NodeKind::Folder,
        size: 17,
        modified_unix: 1,
        etag: Some("old-v1".into()),
        content_version: None,
        target: None,
        package: true,
    };
    let oldbytes = archive(&vec![17u8; 128 * 1024]);
    let disk = temp.path().join("original.zip");
    std::fs::write(&disk, &oldbytes).unwrap();
    let raw = cirrove_icloud::PackageDownload {
        size: oldbytes.len() as u64,
        sha256: hex::encode(Sha256::digest(&oldbytes)),
    };
    let semantic = cirrove_icloud::package_archive_semantic_identity_versioned(
        &std::fs::File::open(&disk).unwrap(),
        &raw,
        "Owned.pages",
        2,
        &CancellationToken::new(),
    )
    .unwrap();
    let artifact = Node {
        id: format!("icloud-artifact:{}", original.id),
        parent_id: Some(original.id.clone()),
        name: original.name.clone(),
        kind: NodeKind::File,
        size: raw.size,
        modified_unix: 1,
        etag: None,
        content_version: Some(format!(
            "icloud-artifact-v2:{}",
            serde_json::json!({"source_etag":"old-v1","source_parent":original.parent_id,"source_size":original.size,"sha256":raw.sha256})
        )),
        target: None,
        package: false,
    };
    let cloud = Arc::new(Cloud::default());
    cloud.icloud_identity.store(true, Ordering::SeqCst);
    cloud.stall.store(true, Ordering::SeqCst);
    {
        let mut remote = cloud.remote.lock().unwrap();
        remote
            .files
            .insert(original.id.clone(), (original.clone(), vec![]));
        remote
            .files
            .insert(artifact.id.clone(), (artifact.clone(), oldbytes.clone()));
    }
    let provider = Arc::new(NativeCloud {
        cloud: cloud.clone(),
        binding: NativeArchiveBinding {
            scope: scope.clone(),
            source: original.clone(),
            archive: artifact.clone(),
            semantic: semantic.clone(),
        },
        snapshot: Arc::new(Snapshot {
            identity: ReadIdentity::new(&scope, &artifact).unwrap(),
            bytes: oldbytes.clone(),
        }),
        resolutions: AtomicUsize::new(0),
    });
    let journalroot = temp.path().join("journal");
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&journalroot, &account.id, 8 * 1024 * 1024).unwrap(),
    ));
    let engine = Engine::new(account.clone(), provider.clone(), temp.path().join("state"))
        .await
        .unwrap();

    engine.start().await.unwrap();
    let session = cirrove_service::filesystem::CloudFs::new_experimental_writable(
        engine.clone(),
        journal.clone(),
    )
    .await
    .unwrap()
    .mount(&mount)
    .unwrap();
    let path = mount.join("Owned.pages/Owned.pages");
    let final_bytes = archive(b"C retained after restart");
    tokio::task::spawn_blocking({
        let path = path.clone();
        let journal = journal.clone();
        let final_bytes = final_bytes.clone();
        let original = oldbytes.clone();
        move || {
            let mut held = std::fs::File::open(&path).unwrap();
            let backup = path.with_file_name("Owned.pages~");
            if backup_first {
                assert!(
                    std::fs::rename(&path, path.with_file_name("owned.PAGES")).is_err(),
                    "case-only alias is not a separate backup slot"
                );
                assert_eq!(std::fs::read(&path).unwrap(), original);
                std::fs::rename(&path, &backup).unwrap();
                assert!(!path.exists());
                assert_eq!(std::fs::read(&backup).unwrap(), original);
                let reopen = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&backup)
                    .unwrap();
                reopen.sync_all().unwrap();
                std::fs::rename(&backup, &path).unwrap();
                assert_eq!(std::fs::read(&path).unwrap(), original);
                assert_eq!(journal.lock().unwrap().list(0, 10).unwrap().len(), 0);
            }
            // Real local-only abort-save lifecycle before the pending A/B/C chain.
            let scratch = path.parent().unwrap().join(".aborted-save");
            let renamed = path.parent().unwrap().join(".aborted-renamed");
            let mut temp_handle = std::fs::OpenOptions::new()
                .create_new(true)
                .read(true)
                .write(true)
                .open(&scratch)
                .unwrap();
            temp_handle.write_all(b"partial document").unwrap();
            temp_handle.sync_all().unwrap();
            std::fs::rename(&scratch, &renamed).unwrap();
            assert!(!scratch.exists());
            assert_eq!(std::fs::read(&renamed).unwrap(), b"partial document");
            let occupied = path.parent().unwrap().join(".occupied-temp");
            let occupant = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&occupied)
                .unwrap();
            assert!(std::fs::rename(&renamed, &occupied).is_err());
            drop(occupant);
            std::fs::remove_file(&occupied).unwrap();
            std::fs::remove_file(&renamed).unwrap();
            assert!(!renamed.exists());
            assert_eq!(temp_handle.metadata().unwrap().nlink(), 0);
            temp_handle.write_all(b" late descriptor bytes").unwrap();
            temp_handle.sync_all().unwrap();
            temp_handle.rewind().unwrap();
            let mut retained = Vec::new();
            temp_handle.read_to_end(&mut retained).unwrap();
            assert_eq!(retained, b"partial document late descriptor bytes");
            let recreated = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&renamed)
                .unwrap();
            assert_ne!(
                recreated.metadata().unwrap().ino(),
                temp_handle.metadata().unwrap().ino()
            );
            drop(recreated);
            std::fs::remove_file(&renamed).unwrap();
            assert!(
                std::fs::remove_file(&path).is_err(),
                "canonical unlink escaped guard"
            );
            assert_eq!(journal.lock().unwrap().list(0, 10).unwrap().len(), 0);
            assert_eq!(std::fs::read(&path).unwrap(), original);
            for (index, bytes) in [
                archive(b"A pending"),
                archive(b"B pending"),
                final_bytes.clone(),
            ]
            .into_iter()
            .enumerate()
            {
                let temporary = path.parent().unwrap().join(".save");
                let mut old_writer = if backup_first {
                    let held = std::fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(&path)
                        .unwrap();
                    std::fs::rename(&path, &backup).unwrap();
                    assert!(!path.exists());
                    Some(held)
                } else {
                    None
                };
                let mut file = std::fs::OpenOptions::new()
                    .create_new(true)
                    .read(true)
                    .write(true)
                    .open(&temporary)
                    .unwrap();
                file.write_all(&bytes).unwrap();
                file.sync_all().unwrap();
                assert_eq!(
                    journal.lock().unwrap().list(0, 10).unwrap().len(),
                    index,
                    "temporary fsync submitted bytes"
                );
                std::fs::rename(&temporary, &path).unwrap();
                assert_eq!(std::fs::read(&path).unwrap(), bytes);
                assert!(!temporary.exists());
                if let Some(mut old) = old_writer.take() {
                    old.set_len(0).unwrap();
                    old.seek(SeekFrom::Start(0)).unwrap();
                    let late = archive(b"late backup descriptor bytes");
                    old.write_all(&late).unwrap();
                    old.sync_all().unwrap();
                    assert_eq!(std::fs::read(&backup).unwrap(), late);
                    let reopened = std::fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(&backup)
                        .unwrap();
                    reopened.sync_all().unwrap();
                    std::fs::remove_file(&backup).unwrap();
                    assert!(!backup.exists());
                    old.sync_all().unwrap();
                    assert_eq!(std::fs::read(&path).unwrap(), bytes);
                    assert_eq!(
                        journal.lock().unwrap().list(0, 10).unwrap().len(),
                        index + 1,
                        "backup lifecycle enqueued provider work"
                    );
                }
            }
            let mut old = Vec::new();
            held.read_to_end(&mut old).unwrap();
            assert_eq!(old, original);
        }
    })
    .await
    .unwrap();
    let ids: Vec<_> = journal
        .lock()
        .unwrap()
        .list(0, 10)
        .unwrap()
        .into_iter()
        .map(|r| {
            assert!(r.package_completion.is_none());
            assert!(r.remote.is_none());
            r.id
        })
        .collect();
    assert_eq!(ids.len(), 3);
    assert_eq!(cloud.write_calls.load(Ordering::SeqCst), 0);
    engine.stop().await;
    tokio::task::spawn_blocking(move || session.umount_and_join())
        .await
        .unwrap()
        .unwrap();
    drop(engine);
    drop(journal);
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&journalroot, &account.id, 8 * 1024 * 1024).unwrap(),
    ));
    let engine = Engine::new(account.clone(), provider.clone(), temp.path().join("state"))
        .await
        .unwrap();
    engine.start().await.unwrap();
    let session = cirrove_service::filesystem::CloudFs::new_experimental_writable(
        engine.clone(),
        journal.clone(),
    )
    .await
    .unwrap()
    .mount(&mount)
    .unwrap();
    tokio::task::spawn_blocking(move || {
        assert_eq!(std::fs::read(&path).unwrap(), final_bytes);
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        file.sync_all().unwrap();
    })
    .await
    .unwrap();
    let retained: Vec<_> = journal
        .lock()
        .unwrap()
        .list(0, 10)
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(
        retained, ids,
        "reopen/clean fsync enqueued or lost pending lineage"
    );
    assert_eq!(cloud.write_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.resolutions.load(Ordering::SeqCst), 1);
    engine.stop().await;
    tokio::task::spawn_blocking(move || session.umount_and_join())
        .await
        .unwrap()
        .unwrap();
}
