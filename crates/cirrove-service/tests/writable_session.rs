//! Automatic mounted uploads and shutdown against an in-memory cloud provider.
#![allow(clippy::unwrap_used)]
use async_trait::async_trait;
use cirrove_auth::{AccessMode, AppRegistration, CredentialVault, Identity};
use cirrove_core::upload::{
    Reconciliation, UploadError, UploadIntent, UploadProgress, UploadProvider, UploadRequest,
    UploadStep,
};
use cirrove_core::*;
use cirrove_onedrive::DriveInfo;
use cirrove_service::{
    accounts::Account,
    engine::Engine,
    journal::{UploadJournal, UploadState},
    writable::WritableSession,
};
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::{Seek, SeekFrom, Write},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;

#[derive(Default)]
struct Vault {
    values: Mutex<HashMap<String, SecretString>>,
    stall: AtomicBool,
    entered: Notify,
}
#[async_trait]
impl CredentialVault for Vault {
    async fn load(&self, key: &str) -> anyhow::Result<Option<SecretString>> {
        Ok(self.values.lock().unwrap().get(key).cloned())
    }
    async fn save(&self, key: &str, value: SecretString) -> anyhow::Result<()> {
        if self.stall.load(Ordering::SeqCst) {
            self.entered.notify_one();
            std::future::pending::<()>().await;
        }
        self.values.lock().unwrap().insert(key.into(), value);
        Ok(())
    }
    async fn remove(&self, key: &str) -> anyhow::Result<()> {
        self.values.lock().unwrap().remove(key);
        Ok(())
    }
}
#[derive(Default)]
struct Remote {
    files: HashMap<String, (Node, Vec<u8>)>,
    sessions: HashMap<String, Vec<u8>>,
    history: Vec<Vec<u8>>,
}
#[derive(Default)]
struct Cloud {
    remote: Mutex<Remote>,
    pause_once: AtomicBool,
    stall: AtomicBool,
    entered: Notify,
    release: Notify,
}
fn root() -> Node {
    Node {
        id: "root".into(),
        parent_id: None,
        name: "root".into(),
        kind: NodeKind::Folder,
        size: 0,
        modified_unix: 1,
        etag: None,
        content_version: None,
        target: None,
    }
}
#[async_trait]
impl MetadataProvider for Cloud {
    fn provider_id(&self) -> &'static str {
        "fixture"
    }
    async fn changes(
        &self,
        _: &Scope,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> Result<ChangePage, ProviderError> {
        let mut nodes = vec![Change::Upsert(root())];
        nodes.extend(
            self.remote
                .lock()
                .unwrap()
                .files
                .values()
                .map(|(n, _)| Change::Upsert(n.clone())),
        );
        Ok(ChangePage {
            changes: nodes,
            checkpoint: Checkpoint::Complete(Cursor("fixture".into())),
        })
    }
}
#[async_trait]
impl ReadProvider for Cloud {
    async fn node(
        &self,
        _: &Scope,
        id: &str,
        _: &CancellationToken,
    ) -> Result<Node, ProviderError> {
        if id == "root" {
            return Ok(root());
        }
        self.remote
            .lock()
            .unwrap()
            .files
            .get(id)
            .map(|(n, _)| n.clone())
            .ok_or(ProviderError::NotFound)
    }
    async fn children(
        &self,
        _: &Scope,
        parent: &str,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> Result<DirectoryPage, ProviderError> {
        Ok(DirectoryPage {
            nodes: self
                .remote
                .lock()
                .unwrap()
                .files
                .values()
                .filter(|(n, _)| n.parent_id.as_deref() == Some(parent))
                .map(|(n, _)| n.clone())
                .collect(),
            next: None,
        })
    }
    async fn read_range(
        &self,
        _: &Scope,
        node: &Node,
        offset: u64,
        length: u32,
        _: &CancellationToken,
    ) -> Result<Vec<u8>, ProviderError> {
        let remote = self.remote.lock().unwrap();
        let (current, bytes) = remote.files.get(&node.id).ok_or(ProviderError::NotFound)?;
        if node.content_revision() != current.content_revision() {
            return Err(ProviderError::VersionChanged);
        }
        let start = (offset as usize).min(bytes.len());
        let end = start.saturating_add(length as usize).min(bytes.len());
        Ok(bytes[start..end].to_vec())
    }
}
impl Cloud {
    fn step(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
    ) -> upload::Result<UploadStep> {
        let remote = self.remote.lock().unwrap();
        let bytes = remote
            .sessions
            .get(checkpoint.expose_secret())
            .ok_or(UploadError::SessionGone)?;
        let offset = bytes.len() as u64;
        Ok(if offset == request.size {
            UploadStep::Commit(checkpoint.clone())
        } else {
            UploadStep::Continue(UploadProgress {
                checkpoint: checkpoint.clone(),
                offset,
                length: (request.size - offset).min(4) as u32,
            })
        })
    }
    fn target<'a>(remote: &'a Remote, intent: &UploadIntent) -> Option<&'a (Node, Vec<u8>)> {
        match intent {
            UploadIntent::Create { name, parent } => remote
                .files
                .values()
                .find(|(n, _)| n.name == *name && n.parent_id.as_ref() == Some(parent)),
            UploadIntent::Replace { item, .. } => remote.files.get(item),
        }
    }
}
#[async_trait]
impl UploadProvider for Cloud {
    async fn begin_upload(
        &self,
        request: &UploadRequest,
        _: &CancellationToken,
    ) -> upload::Result<UploadStep> {
        if self.stall.load(Ordering::SeqCst) {
            self.entered.notify_one();
            std::future::pending::<()>().await;
        }
        if self.pause_once.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        let checkpoint = SecretString::from(uuid::Uuid::new_v4().to_string());
        self.remote
            .lock()
            .unwrap()
            .sessions
            .insert(checkpoint.expose_secret().into(), vec![]);
        self.step(request, &checkpoint)
    }
    async fn inspect_upload(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
        _: &CancellationToken,
    ) -> upload::Result<UploadStep> {
        self.step(request, checkpoint)
    }
    async fn upload_part(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
        offset: u64,
        bytes: Vec<u8>,
        _: &CancellationToken,
    ) -> upload::Result<UploadStep> {
        {
            let mut remote = self.remote.lock().unwrap();
            let data = remote
                .sessions
                .get_mut(checkpoint.expose_secret())
                .ok_or(UploadError::SessionGone)?;
            assert_eq!(data.len() as u64, offset);
            data.extend(bytes);
        }
        self.step(request, checkpoint)
    }
    async fn commit_upload(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
        _: &CancellationToken,
    ) -> upload::Result<UploadStep> {
        let mut remote = self.remote.lock().unwrap();
        let before = Self::target(&remote, &request.intent).map(|(n, _)| n.clone());
        let (id, parent, name) = match (&request.intent, before) {
            (UploadIntent::Create { parent, name }, None) => (
                uuid::Uuid::new_v4().to_string(),
                parent.clone(),
                name.clone(),
            ),
            (
                UploadIntent::Replace {
                    item,
                    expected_etag,
                },
                Some(n),
            ) if n.etag.as_ref() == Some(expected_etag) => {
                (item.clone(), n.parent_id.unwrap(), n.name)
            }
            _ => return Err(UploadError::Conflict),
        };
        let bytes = remote
            .sessions
            .remove(checkpoint.expose_secret())
            .ok_or(UploadError::SessionGone)?;
        assert_eq!(bytes.len() as u64, request.size);
        assert_eq!(format!("{:x}", Sha256::digest(&bytes)), request.sha256);
        let tag = format!("version-{}", remote.history.len());
        let node = Node {
            id: id.clone(),
            parent_id: Some(parent),
            name,
            kind: NodeKind::File,
            size: request.size,
            modified_unix: 1,
            etag: Some(tag.clone()),
            content_version: Some(tag),
            target: None,
        };
        remote.history.push(bytes.clone());
        remote.files.insert(id, (node.clone(), bytes));
        Ok(UploadStep::Complete(node))
    }
    async fn reconcile_upload(
        &self,
        request: &UploadRequest,
        _: &CancellationToken,
    ) -> upload::Result<Reconciliation> {
        let remote = self.remote.lock().unwrap();
        let target = Self::target(&remote, &request.intent);
        Ok(match target {
            Some((node, bytes))
                if bytes.len() as u64 == request.size
                    && format!("{:x}", Sha256::digest(bytes)) == request.sha256 =>
            {
                Reconciliation::Committed(node.clone())
            }
            None => Reconciliation::Uncommitted,
            Some((node, _)) if matches!(&request.intent,UploadIntent::Replace {expected_etag,..} if node.etag.as_ref()==Some(expected_etag)) => {
                Reconciliation::Uncommitted
            }
            _ => Reconciliation::Conflict,
        })
    }
}
fn account(mount: &Path) -> Account {
    Account {
        id: "00000000-0000-4000-8000-000000000005".into(),
        label: "writable fixture".into(),
        registration: AppRegistration {
            client_id: "00000000-0000-4000-8000-000000000002".into(),
            authority: "common".into(),
        },
        identity: Identity {
            tenant_id: "00000000-0000-4000-8000-000000000003".into(),
            subject: "fixture".into(),
            username: "fixture@example.invalid".into(),
            graph_user_id: "fixture".into(),
            display_name: "fixture".into(),
        },
        credential_id: "00000000-0000-4000-8000-000000000004".into(),
        access: AccessMode::ReadWrite,
        drive: DriveInfo {
            id: "drive".into(),
            name: "fixture".into(),
            drive_type: "business".into(),
            web_url: "https://example.invalid".into(),
        },
        root_id: "root".into(),
        mount_path: mount.into(),
        enabled: false,
        poll_seconds: 3600,
        cache_bytes: 8 * 1024 * 1024,
    }
}
async fn acknowledged(session: &WritableSession, count: usize) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let rows = session.uploads(0, 100).await.unwrap();
            assert!(
                !rows
                    .iter()
                    .any(|r| matches!(r.state, UploadState::Failed | UploadState::Conflict))
            );
            if rows.len() == count && rows.iter().all(|r| r.state == UploadState::Uploaded) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; mounted writes upload automatically and resume after restart"]
async fn real_automatic_uploads_preserve_generations_and_resume_a_shutdown_save() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let state = temp.path().join("state");
    let account = account(&mount);
    let cloud = Arc::new(Cloud::default());
    cloud.pause_once.store(true, Ordering::SeqCst);
    let vault = Arc::new(Vault::default());
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&temp.path().join("journal"), &account.id, 1024 * 1024).unwrap(),
    ));
    let engine = Engine::new(account.clone(), cloud.clone(), state.clone())
        .await
        .unwrap();
    let session = WritableSession::mount(
        engine.clone(),
        journal.clone(),
        cloud.clone(),
        vault.clone(),
    )
    .await
    .unwrap();
    let p = mount.join("saved.txt");
    let mut file = tokio::task::spawn_blocking(move || {
        let mut f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(p)
            .unwrap();
        f.write_all(b"first save").unwrap();
        f.sync_all().unwrap();
        f
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(2), cloud.entered.notified())
        .await
        .unwrap();
    let p = mount.join("saved.txt");
    file = tokio::task::spawn_blocking(move || {
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(b"second save").unwrap();
        file.sync_all().unwrap();
        assert_eq!(std::fs::read(p).unwrap(), b"second save");
        file
    })
    .await
    .unwrap();
    assert!(cloud.remote.lock().unwrap().history.is_empty());
    cloud.release.notify_one();
    acknowledged(&session, 2).await;
    assert_eq!(
        cloud.remote.lock().unwrap().history,
        vec![b"first save".to_vec(), b"second save".to_vec()]
    );
    file = tokio::task::spawn_blocking(move || {
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(b"third! save").unwrap();
        file
    })
    .await
    .unwrap();
    session.shutdown().await.unwrap();
    drop(file);
    drop(engine);
    assert_eq!(
        journal
            .lock()
            .unwrap()
            .list(0, 100)
            .unwrap()
            .last()
            .unwrap()
            .state,
        UploadState::Pending
    );
    let engine = Engine::new(account, cloud.clone(), state).await.unwrap();
    let session = WritableSession::mount(engine, journal, cloud.clone(), vault)
        .await
        .unwrap();
    acknowledged(&session, 3).await;
    assert_eq!(
        cloud.remote.lock().unwrap().history.last().unwrap(),
        b"third! save"
    );
    session.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; remote or credential futures deliberately ignore cancellation"]
async fn real_shutdown_cancels_stalled_uploads_and_keyring_without_losing_saved_bytes() {
    for keyring in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let mount = temp.path().join("mount");
        std::fs::create_dir(&mount).unwrap();
        let account = account(&mount);
        let cloud = Arc::new(Cloud::default());
        cloud.stall.store(!keyring, Ordering::SeqCst);
        let vault = Arc::new(Vault::default());
        vault.stall.store(keyring, Ordering::SeqCst);
        let journal = Arc::new(Mutex::new(
            UploadJournal::open(&temp.path().join("journal"), &account.id, 1024 * 1024).unwrap(),
        ));
        let engine = Engine::new(account, cloud.clone(), temp.path().join("state"))
            .await
            .unwrap();
        let session = WritableSession::mount(engine, journal.clone(), cloud.clone(), vault.clone())
            .await
            .unwrap();
        let p = mount.join("saved.txt");
        let file = tokio::task::spawn_blocking(move || {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(p)
                .unwrap();
            f.write_all(b"protected").unwrap();
            f.sync_all().unwrap();
            f
        })
        .await
        .unwrap();
        tokio::time::timeout(
            Duration::from_secs(2),
            if keyring {
                vault.entered.notified()
            } else {
                cloud.entered.notified()
            },
        )
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(2), session.shutdown())
            .await
            .unwrap()
            .unwrap();
        drop(file);
        let j = journal.lock().unwrap();
        let row = j.list(0, 100).unwrap().remove(0);
        assert_eq!(row.state, UploadState::VerifyRequired);
        let mut bytes = vec![];
        std::io::Read::read_to_end(&mut j.payload(row.id).unwrap(), &mut bytes).unwrap();
        assert_eq!(bytes, b"protected");
        assert!(
            !std::fs::read_to_string("/proc/self/mountinfo")
                .unwrap()
                .lines()
                .any(|line| line.split_whitespace().nth(4) == mount.to_str())
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; shutdown must await an already admitted write"]
#[allow(clippy::await_holding_lock)] // Deliberate blocked-storage fixture; never production locking.
async fn real_shutdown_drains_a_write_waiting_for_local_storage() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let account = account(&mount);
    let cloud = Arc::new(Cloud::default());
    cloud.stall.store(true, Ordering::SeqCst);
    let vault = Arc::new(Vault::default());
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&temp.path().join("journal"), &account.id, 1024 * 1024).unwrap(),
    ));
    let engine = Engine::new(account, cloud.clone(), temp.path().join("state"))
        .await
        .unwrap();
    let session = WritableSession::mount(engine, journal.clone(), cloud.clone(), vault)
        .await
        .unwrap();
    let p = mount.join("saved.txt");
    let mut file = tokio::task::spawn_blocking(move || {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(p)
            .unwrap();
        f.write_all(b"before").unwrap();
        f.sync_all().unwrap();
        f
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(2), cloud.entered.notified())
        .await
        .unwrap();
    // Deliberately block local storage, then observe admission before shutdown.
    let guard = journal.lock().unwrap();
    let write = tokio::task::spawn_blocking(move || {
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(b"accepted edit").unwrap();
        file
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while session.pending_local_requests() == 0 {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    let stopping = tokio::spawn(session.shutdown());
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!stopping.is_finished());
    assert!(
        std::fs::read_to_string("/proc/self/mountinfo")
            .unwrap()
            .lines()
            .any(|line| line.split_whitespace().nth(4) == mount.to_str())
    );
    drop(guard);
    let file = write.await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), stopping)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    drop(file);
    let j = journal.lock().unwrap();
    let working = j.working_files().unwrap().remove(0);
    assert!(!working.dirty);
    let saved = j.get(working.latest.unwrap()).unwrap();
    assert_eq!(saved.state, UploadState::Pending);
    let mut bytes = vec![];
    std::io::Read::read_to_end(&mut j.payload(saved.id).unwrap(), &mut bytes).unwrap();
    assert_eq!(bytes, b"accepted edit");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; failed shutdown sealing keeps unsent working data"]
async fn real_shutdown_reports_insufficient_snapshot_space_and_retains_the_dirty_copy() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let account = account(&mount);
    let cloud = Arc::new(Cloud::default());
    let vault = Arc::new(Vault::default());
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&temp.path().join("journal"), &account.id, 12).unwrap(),
    ));
    let engine = Engine::new(account, cloud.clone(), temp.path().join("state"))
        .await
        .unwrap();
    let session = WritableSession::mount(engine, journal.clone(), cloud, vault)
        .await
        .unwrap();
    let p = mount.join("saved.txt");
    let file = tokio::task::spawn_blocking(move || {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(p)
            .unwrap();
        f.write_all(b"protected").unwrap();
        f
    })
    .await
    .unwrap();
    assert!(session.shutdown().await.is_err());
    drop(file);
    let j = journal.lock().unwrap();
    assert!(j.list(0, 100).unwrap().is_empty());
    let working = j.working_files().unwrap().remove(0);
    assert!(working.dirty);
    assert_eq!(j.read_working(working.id, 0, 100).unwrap(), b"protected");
    assert!(
        !std::fs::read_to_string("/proc/self/mountinfo")
            .unwrap()
            .lines()
            .any(|line| line.split_whitespace().nth(4) == mount.to_str())
    );
}
