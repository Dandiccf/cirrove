//! Automatic mounted uploads and shutdown against an in-memory cloud provider.
#![allow(clippy::unwrap_used)]
use async_trait::async_trait;
use cirrove_auth::{AccessMode, AppRegistration, CredentialVault, Identity};
use cirrove_core::mutation::{
    MutationError, MutationIntent, MutationProvider, MutationReceipt, MutationReconciliation,
    MutationRequest,
};
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
        atomic::{AtomicBool, AtomicUsize, Ordering},
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
    moves: Vec<MutationRequest>,
    deletes: Vec<MutationRequest>,
}
#[derive(Default)]
struct Cloud {
    remote: Mutex<Remote>,
    pause_once: AtomicBool,
    reads: AtomicUsize,
    lose_move_once: AtomicBool,
    foreign_after_lost_move: AtomicBool,
    stall: AtomicBool,
    entered: Notify,
    release: Notify,
    hold_read: AtomicBool,
    read_entered: Notify,
    read_release: Notify,
    hold_folder: AtomicBool,
    folder_entered: Notify,
    folder_release: Notify,
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
        let remote = self.remote.lock().unwrap();
        if !Self::has_parent(&remote, parent) {
            return Err(ProviderError::NotFound);
        }
        Ok(DirectoryPage {
            nodes: remote
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
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.hold_read.swap(false, Ordering::SeqCst) {
            self.read_entered.notify_one();
            self.read_release.notified().await;
        }
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
    fn has_parent(remote: &Remote, parent: &str) -> bool {
        parent == "root"
            || remote
                .files
                .get(parent)
                .is_some_and(|(n, _)| n.kind == NodeKind::Folder)
    }
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
        if !Self::has_parent(&remote, &parent) {
            return Err(ProviderError::NotFound.into());
        }
        let bytes = remote
            .sessions
            .remove(checkpoint.expose_secret())
            .ok_or(UploadError::SessionGone)?;
        assert_eq!(bytes.len() as u64, request.size);
        assert_eq!(hex::encode(Sha256::digest(&bytes)), request.sha256);
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
                    && hex::encode(Sha256::digest(bytes)) == request.sha256 =>
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
#[async_trait]
impl MutationProvider for Cloud {
    async fn mutate(
        &self,
        request: &MutationRequest,
        _: &CancellationToken,
    ) -> mutation::Result<MutationReceipt> {
        if self.stall.load(Ordering::SeqCst) {
            self.entered.notify_one();
            std::future::pending::<()>().await;
        }
        request.validate()?;
        if let MutationIntent::CreateFolder { parent, name } = &request.intent {
            if self.hold_folder.swap(false, Ordering::SeqCst) {
                self.folder_entered.notify_one();
                self.folder_release.notified().await;
            }
            let mut remote = self.remote.lock().unwrap();
            if !Self::has_parent(&remote, parent) {
                return Err(ProviderError::NotFound.into());
            }
            if remote.files.values().any(|(n, _)| {
                n.parent_id.as_ref() == Some(parent) && n.name.to_lowercase() == name.to_lowercase()
            }) {
                return Err(MutationError::Conflict);
            }
            let node = Node {
                id: uuid::Uuid::new_v4().to_string(),
                parent_id: Some(parent.clone()),
                name: name.clone(),
                etag: Some(uuid::Uuid::new_v4().to_string()),
                ..root()
            };
            remote.files.insert(node.id.clone(), (node.clone(), vec![]));
            return Ok(MutationReceipt::Upsert(node));
        }
        if let MutationIntent::RemoveFile { before } = &request.intent {
            let mut remote = self.remote.lock().unwrap();
            let (node, _) = remote
                .files
                .get(&before.id)
                .ok_or(ProviderError::NotFound)?;
            if node.etag != before.etag || node.kind != NodeKind::File {
                return Err(MutationError::Conflict);
            }
            remote.files.remove(&before.id);
            remote.deletes.push(request.clone());
            return Ok(MutationReceipt::Removed {
                item: before.id.clone(),
            });
        }
        let MutationIntent::Relocate {
            before,
            parent,
            name,
        } = &request.intent
        else {
            return Err(MutationError::Unsupported("fixture only relocates files"));
        };
        let mut remote = self.remote.lock().unwrap();
        if !Self::has_parent(&remote, parent) {
            return Err(ProviderError::NotFound.into());
        }
        if remote.files.values().any(|(n, _)| {
            n.id != before.id
                && n.parent_id.as_ref() == Some(parent)
                && n.name.to_lowercase() == name.to_lowercase()
        }) {
            return Err(MutationError::Conflict);
        }
        let (node, _) = remote
            .files
            .get_mut(&before.id)
            .ok_or(ProviderError::NotFound)?;
        if node.etag != before.etag {
            return Err(MutationError::Conflict);
        }
        node.name = name.clone();
        node.parent_id = Some(parent.clone());
        node.etag = Some(uuid::Uuid::new_v4().to_string());
        let result = node.clone();
        remote.moves.push(request.clone());
        if self.lose_move_once.swap(false, Ordering::SeqCst) {
            if self.foreign_after_lost_move.load(Ordering::SeqCst) {
                let (node, bytes) = remote.files.get_mut(&before.id).unwrap();
                *bytes = b"someone else's save".to_vec();
                node.size = bytes.len() as u64;
                node.etag = Some("foreign-etag".into());
                node.content_version = Some("foreign-content".into());
            }
            return Err(MutationError::Uncertain);
        }
        Ok(MutationReceipt::Upsert(result))
    }
    async fn reconcile_mutation(
        &self,
        request: &MutationRequest,
        _: &CancellationToken,
    ) -> mutation::Result<MutationReconciliation> {
        let MutationIntent::Relocate {
            before,
            parent,
            name,
        } = &request.intent
        else {
            return Ok(MutationReconciliation::Indeterminate);
        };
        let remote = self.remote.lock().unwrap();
        Ok(match remote.files.get(&before.id) {
            Some((node, _)) if node.name == *name && node.parent_id.as_ref() == Some(parent) => {
                MutationReconciliation::Applied(MutationReceipt::Upsert(node.clone()))
            }
            Some((node, _)) if node == before => MutationReconciliation::Uncommitted,
            Some(_) => MutationReconciliation::Conflict,
            None => MutationReconciliation::Indeterminate,
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
    let stopped_engine = Arc::downgrade(&engine);
    drop(engine);
    {
        let j = journal.lock().unwrap();
        let last = j.list(0, 100).unwrap().pop().unwrap();
        // A forked helper may close its inherited application descriptor and
        // submit FLUSH before shutdown, allowing the stalled worker to claim it.
        // Either state must retain the exact unsent bytes and resume safely.
        assert!(matches!(
            last.state,
            UploadState::Pending | UploadState::VerifyRequired
        ));
        let mut bytes = vec![];
        std::io::Read::read_to_end(&mut j.payload(last.id).unwrap(), &mut bytes).unwrap();
        assert_eq!(bytes, b"third! save");
    }
    let engine = reopened_engine(account, cloud.clone(), &state, stopped_engine).await;
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
    // The application owns its descriptor in a separate process. Concurrent
    // fusermount children of this test process must not inherit it and generate
    // unrelated FLUSH requests that look like admission of the intended write.
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let mut app = tokio::process::Command::new("python3")
        .args([
            "-c",
            r#"
import os,sys
fd=os.open(sys.argv[1], os.O_CREAT|os.O_EXCL|os.O_WRONLY, 0o600)
assert os.write(fd,b'before')==6
os.fsync(fd)
print('ready',flush=True)
assert sys.stdin.readline()=='write\n'
os.lseek(fd,0,os.SEEK_SET)
assert os.write(fd,b'accepted edit')==13
print('written',flush=True)
assert sys.stdin.readline()=='exit\n'
os._exit(0)
"#,
        ])
        .arg(mount.join("saved.txt"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut input = app.stdin.take().unwrap();
    let mut output = BufReader::new(app.stdout.take().unwrap()).lines();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), output.next_line())
            .await
            .unwrap()
            .unwrap()
            .as_deref(),
        Some("ready")
    );
    tokio::time::timeout(Duration::from_secs(2), cloud.entered.notified())
        .await
        .unwrap();
    // The initial fsync can reply just before its admission token is released.
    // Wait for it to finish; the child cannot send another edit before our command.
    tokio::time::timeout(Duration::from_secs(2), async {
        while session.pending_local_requests() != 0 {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    let guard = journal.lock().unwrap();
    input.write_all(b"write\n").await.unwrap();
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
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), output.next_line())
            .await
            .unwrap()
            .unwrap()
            .as_deref(),
        Some("written")
    );
    tokio::time::timeout(Duration::from_secs(2), stopping)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    // Exit with the descriptor still open, after proving shutdown did not wait
    // for the application. Process teardown need not submit another explicit flush.
    input.write_all(b"exit\n").await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), app.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
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

fn namespace_fixture(cloud: &Cloud) {
    let huge = Node {
        id: "online".into(),
        parent_id: Some("root".into()),
        name: "online.bin".into(),
        kind: NodeKind::File,
        size: 500 * 1024 * 1024 * 1024,
        modified_unix: 1,
        etag: Some("original-etag".into()),
        content_version: Some("original-content".into()),
        target: None,
    };
    let folder = Node {
        id: "folder".into(),
        parent_id: Some("root".into()),
        name: "folder".into(),
        etag: Some("folder-etag".into()),
        ..root()
    };
    let occupied = Node {
        id: "occupied".into(),
        name: "occupied.bin".into(),
        size: 7,
        ..huge.clone()
    };
    let mut remote = cloud.remote.lock().unwrap();
    for (node, bytes) in [
        (huge, vec![]),
        (folder, vec![]),
        (occupied, b"foreign".to_vec()),
    ] {
        remote.files.insert(node.id.clone(), (node, bytes));
    }
}
async fn application(mount: &Path, source: &str) {
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new("python3")
            .arg("-c")
            .arg(source)
            .arg(mount)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        output.status.success(),
        "application failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn replacement_fixture(cloud: &Cloud) {
    let mut remote = cloud.remote.lock().unwrap();
    for (id, name, bytes) in [
        ("source", "source.txt", b"new"),
        ("target", "document.txt", b"old"),
    ] {
        let node = Node {
            id: id.into(),
            name: name.into(),
            parent_id: Some("root".into()),
            kind: NodeKind::File,
            size: 3,
            etag: Some(format!("{id}-original")),
            content_version: Some(format!("{id}-original")),
            modified_unix: 1,
            target: None,
        };
        remote.files.insert(id.into(), (node, bytes.to_vec()));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; editor atomic saves and retained descriptors"]
async fn real_replacement_preserves_old_descriptors_across_two_atomic_saves() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let account = account(&mount);
    let cloud = Arc::new(Cloud::default());
    cloud.pause_once.store(true, Ordering::SeqCst);
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&temp.path().join("journal"), &account.id, 1024 * 1024).unwrap(),
    ));
    let engine = Engine::new(account, cloud.clone(), temp.path().join("state"))
        .await
        .unwrap();
    let session = WritableSession::mount(
        engine,
        journal.clone(),
        cloud.clone(),
        Arc::new(Vault::default()),
    )
    .await
    .unwrap();
    application(
        &mount,
        r#"
import os,sys
os.chdir(sys.argv[1])
old=os.open('document.txt',os.O_CREAT|os.O_EXCL|os.O_RDWR,0o600)
os.write(old,b'original');os.fsync(old)
first=os.open('.temporary-one',os.O_CREAT|os.O_EXCL|os.O_RDWR,0o600)
os.write(first,b'first save');os.fsync(first)
first_inode=os.fstat(first).st_ino
os.replace('.temporary-one','document.txt')
assert os.stat('document.txt').st_ino==first_inode
assert os.fstat(old).st_nlink==0
assert os.pread(old,100,0)==b'original'
assert open('document.txt','rb').read()==b'first save'
second=os.open('.temporary-two',os.O_CREAT|os.O_EXCL|os.O_RDWR,0o600)
os.write(second,b'second save');os.fsync(second)
os.replace('.temporary-two','document.txt')
assert os.fstat(first).st_nlink==0
assert os.pread(first,100,0)==b'first save'
os.ftruncate(old,0);os.pwrite(old,b'retained recovery data',0);os.fsync(old)
assert os.pread(old,100,0)==b'retained recovery data'
assert open('document.txt','rb').read()==b'second save'
os.close(old);os.close(first);os.close(second)
assert os.listdir('.')==['document.txt']
"#,
    )
    .await;
    cloud.release.notify_one();
    acknowledged(&session, 5).await;
    mutations_applied(&session, 2).await;
    {
        let remote = cloud.remote.lock().unwrap();
        assert_eq!(remote.files.len(), 1);
        let (_, (node, bytes)) = remote.files.iter().next().unwrap();
        assert_eq!(node.name, "document.txt");
        assert_eq!(bytes, b"second save");
        assert_eq!(remote.deletes.len(), 2);
        assert!(
            !remote
                .history
                .iter()
                .any(|b| b == b"retained recovery data")
        );
    }
    assert!(session.namespace_conflicts().unwrap().is_empty());
    session.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; deferred online replacement leaves directory calls responsive"]
async fn real_replacement_of_online_source_returns_before_download_and_preserves_victim() {
    use std::os::unix::fs::MetadataExt;
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let account = account(&mount);
    let cloud = Arc::new(Cloud::default());
    replacement_fixture(&cloud);
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&temp.path().join("journal"), &account.id, 1024 * 1024).unwrap(),
    ));
    let engine = Engine::new(account, cloud.clone(), temp.path().join("state"))
        .await
        .unwrap();
    let session = WritableSession::mount(
        engine,
        journal.clone(),
        cloud.clone(),
        Arc::new(Vault::default()),
    )
    .await
    .unwrap();
    let path = mount.clone();
    let (old, source_inode) = tokio::task::spawn_blocking(move || {
        (
            std::fs::File::open(path.join("document.txt")).unwrap(),
            std::fs::metadata(path.join("source.txt")).unwrap().ino(),
        )
    })
    .await
    .unwrap();
    cloud.hold_read.store(true, Ordering::SeqCst);
    let path = mount.clone();
    tokio::time::timeout(
        Duration::from_secs(2),
        tokio::task::spawn_blocking(move || {
            std::fs::rename(path.join("source.txt"), path.join("document.txt"))
        }),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    tokio::time::timeout(Duration::from_secs(3), cloud.read_entered.notified())
        .await
        .unwrap();
    assert!(
        session
            .uploads(0, 100)
            .await
            .unwrap()
            .iter()
            .any(|u| u.state == UploadState::Preparing)
    );
    application(
        &mount,
        r#"
import os,sys
os.chdir(sys.argv[1]);assert os.listdir('.')==['document.txt']
with open('independent.txt','wb',buffering=0) as f:f.write(b'independent');os.fsync(f.fileno())
assert open('independent.txt','rb').read()==b'independent'
"#,
    )
    .await;
    assert_eq!(cloud.remote.lock().unwrap().files["target"].1, b"old");
    assert!(cloud.remote.lock().unwrap().deletes.is_empty());
    cloud.read_release.notify_one();
    acknowledged(&session, 2).await;
    mutations_applied(&session, 1).await;
    let path = mount.clone();
    tokio::task::spawn_blocking(move || {
        use std::os::unix::fs::FileExt;
        let mut bytes = [0; 3];
        old.read_exact_at(&mut bytes, 0).unwrap();
        assert_eq!(&bytes, b"old");
        assert_eq!(old.metadata().unwrap().nlink(), 0);
        assert_eq!(
            std::fs::metadata(path.join("document.txt")).unwrap().ino(),
            source_inode
        );
        assert_eq!(std::fs::read(path.join("document.txt")).unwrap(), b"new");
    })
    .await
    .unwrap();
    assert_eq!(cloud.remote.lock().unwrap().files["target"].1, b"new");
    assert!(!cloud.remote.lock().unwrap().files.contains_key("source"));
    session.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; replacement waits for an existing victim read without holding directory locks"]
async fn real_replacement_waits_for_old_range_while_other_saves_continue() {
    use std::io::Read;
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let account = account(&mount);
    let cloud = Arc::new(Cloud::default());
    replacement_fixture(&cloud);
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&temp.path().join("journal"), &account.id, 1024 * 1024).unwrap(),
    ));
    let engine = Engine::new(account, cloud.clone(), temp.path().join("state"))
        .await
        .unwrap();
    let session =
        WritableSession::mount(engine, journal, cloud.clone(), Arc::new(Vault::default()))
            .await
            .unwrap();
    cloud.hold_read.store(true, Ordering::SeqCst);
    let path = mount.join("document.txt");
    let reading = tokio::task::spawn_blocking(move || {
        let mut f = std::fs::File::open(path).unwrap();
        let mut bytes = vec![];
        f.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"old");
        f
    });
    tokio::time::timeout(Duration::from_secs(3), cloud.read_entered.notified())
        .await
        .unwrap();
    application(
        &mount,
        r#"
import os,sys
os.chdir(sys.argv[1])
with open('temporary.txt','wb',buffering=0) as f:
    f.write(b'newest');os.fsync(f.fileno())
    os.replace('temporary.txt','document.txt')
with open('independent.txt','wb',buffering=0) as f:f.write(b'other');os.fsync(f.fileno())
assert open('document.txt','rb').read()==b'newest'
assert 'source.txt' in os.listdir('.')
"#,
    )
    .await;
    assert_eq!(cloud.remote.lock().unwrap().files["target"].1, b"old");
    assert!(cloud.remote.lock().unwrap().deletes.is_empty());
    cloud.read_release.notify_one();
    let old = tokio::time::timeout(Duration::from_secs(5), reading)
        .await
        .unwrap()
        .unwrap();
    acknowledged(&session, 3).await;
    mutations_applied(&session, 1).await;
    tokio::task::spawn_blocking(move || {
        use std::os::unix::fs::{FileExt, MetadataExt};
        let mut bytes = [0; 3];
        old.read_exact_at(&mut bytes, 0).unwrap();
        assert_eq!(&bytes, b"old");
        assert_eq!(old.metadata().unwrap().nlink(), 0);
    })
    .await
    .unwrap();
    assert_eq!(cloud.remote.lock().unwrap().files["target"].1, b"newest");
    session.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; interrupted source capture resumes after remount"]
async fn real_replacement_restarts_interrupted_source_preparation() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let account = account(&mount);
    let cloud = Arc::new(Cloud::default());
    replacement_fixture(&cloud);
    cloud.hold_read.store(true, Ordering::SeqCst);
    let spool = temp.path().join("journal");
    let state = temp.path().join("state");
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&spool, &account.id, 1024 * 1024).unwrap(),
    ));
    let engine = Engine::new(account.clone(), cloud.clone(), state.clone())
        .await
        .unwrap();
    let stopped = Arc::downgrade(&engine);
    let vault = Arc::new(Vault::default());
    let session = WritableSession::mount(engine, journal.clone(), cloud.clone(), vault.clone())
        .await
        .unwrap();
    application(
        &mount,
        r#"
import os,sys
os.chdir(sys.argv[1]);os.replace('source.txt','document.txt')
assert os.listdir('.')==['document.txt']
"#,
    )
    .await;
    tokio::time::timeout(Duration::from_secs(3), cloud.read_entered.notified())
        .await
        .unwrap();
    let operation = session.uploads(0, 10).await.unwrap().remove(0).id;
    tokio::time::timeout(Duration::from_secs(5), session.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        journal.lock().unwrap().get(operation).unwrap().state,
        UploadState::Preparing
    );
    assert_eq!(journal.lock().unwrap().retained_bytes().unwrap(), 0);
    assert!(cloud.remote.lock().unwrap().history.is_empty());
    assert!(cloud.remote.lock().unwrap().deletes.is_empty());
    drop(journal);
    let journal = Arc::new(Mutex::new(
        reopened_journal(&spool, &account.id, 1024 * 1024).await,
    ));
    assert!(
        !journal
            .lock()
            .unwrap()
            .replacement(operation)
            .unwrap()
            .local_ready
    );
    cloud.hold_read.store(true, Ordering::SeqCst);
    let engine = reopened_engine(account, cloud.clone(), &state, stopped).await;
    let session = WritableSession::mount(engine, journal, cloud.clone(), vault)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), cloud.read_entered.notified())
        .await
        .unwrap();
    let path = mount.join("document.txt");
    let (opened, received) = tokio::sync::oneshot::channel();
    let reading = tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut file = std::fs::File::open(path).unwrap();
        opened.send(()).unwrap();
        let mut bytes = vec![];
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"new");
        file
    });
    tokio::time::timeout(Duration::from_secs(2), received)
        .await
        .unwrap()
        .unwrap();
    assert!(cloud.remote.lock().unwrap().history.is_empty());
    assert!(cloud.remote.lock().unwrap().deletes.is_empty());
    cloud.read_release.notify_one();
    let file = tokio::time::timeout(Duration::from_secs(5), reading)
        .await
        .unwrap()
        .unwrap();
    acknowledged(&session, 1).await;
    mutations_applied(&session, 1).await;
    drop(file);
    assert_eq!(cloud.remote.lock().unwrap().files.len(), 1);
    assert_eq!(cloud.remote.lock().unwrap().files["target"].1, b"new");
    session.shutdown().await.unwrap();
}
async fn reopened_journal(path: &Path, owner: &str, quota: u64) -> UploadJournal {
    // Other parallel kernel fixtures fork application helpers. A just-forked
    // child can briefly retain the old lease fd until exec closes CLOEXEC fds.
    let until = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        match UploadJournal::open(path, owner, quota) {
            Err(cirrove_service::journal::JournalError::Busy)
                if tokio::time::Instant::now() < until =>
            {
                tokio::time::sleep(Duration::from_millis(2)).await
            }
            result => return result.unwrap(),
        }
    }
}
async fn reopened_engine(
    account: Account,
    cloud: Arc<Cloud>,
    state: &Path,
    stopped: std::sync::Weak<Engine>,
) -> Arc<Engine> {
    assert!(
        stopped.upgrade().is_none(),
        "shutdown retained an in-process account owner"
    );
    // Parallel FUSE fixtures fork helpers, briefly retaining the same open
    // description as the released account flock until exec. Require the actual
    // engine to be gone above, then allow only bounded lock contention here.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        match Engine::new(account.clone(), cloud.clone(), state.into()).await {
            Ok(engine) => return engine,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::WouldBlock)
                    && tokio::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            Err(error) => panic!("account was not released: {error:#}"),
        }
    }
}
async fn mutations_applied(session: &WritableSession, count: usize) {
    use cirrove_service::journal::MutationState;
    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            let records = session.mutations(0, 100).await.unwrap();
            assert!(
                !records.iter().any(|r| matches!(
                    r.state,
                    MutationState::Failed | MutationState::Conflict | MutationState::NeedsReview
                )),
                "namespace worker failed: {:?}",
                session.worker_issue()
            );
            if records.len() == count && records.iter().all(|r| r.state == MutationState::Applied) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; metadata-only moves, crash reconciliation and subsequent saves"]
async fn real_online_file_moves_without_hydration_and_resumes_its_receipt_chain() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let state = temp.path().join("state");
    let account = account(&mount);
    let cloud = Arc::new(Cloud::default());
    namespace_fixture(&cloud);
    cloud.stall.store(true, Ordering::SeqCst);
    let vault = Arc::new(Vault::default());
    let spool = temp.path().join("journal");
    // The virtual file is 500 GiB, whereas the spool can only hold 1 MiB.
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&spool, &account.id, 1024 * 1024).unwrap(),
    ));
    let engine = Engine::new(account.clone(), cloud.clone(), state.clone())
        .await
        .unwrap();
    let stopped_engine = Arc::downgrade(&engine);
    let session = WritableSession::mount(engine, journal.clone(), cloud.clone(), vault.clone())
        .await
        .unwrap();
    application(
        &mount,
        r#"
import ctypes,errno,os,sys
os.chdir(sys.argv[1])
before=os.stat('online.bin').st_ino
libc=ctypes.CDLL(None,use_errno=True)
assert libc.renameat2(-100,b'online.bin',-100,b'occupied.bin',1)==-1
assert ctypes.get_errno()==errno.EEXIST
os.rename('online.bin','renamed.bin')
assert not os.path.exists('online.bin')
assert os.stat('renamed.bin').st_ino==before
os.rename('renamed.bin','folder/final.bin')
assert not os.path.exists('renamed.bin')
assert os.stat('folder/final.bin').st_ino==before
assert os.stat('folder/final.bin').st_size==500*1024**3
assert sorted(os.listdir('folder'))==['final.bin']
"#,
    )
    .await;
    assert_eq!(cloud.reads.load(Ordering::SeqCst), 0);
    assert!(journal.lock().unwrap().working_files().unwrap().is_empty());
    assert!(session.uploads(0, 100).await.unwrap().is_empty());
    assert_eq!(session.mutations(0, 100).await.unwrap().len(), 2);
    tokio::time::timeout(Duration::from_secs(5), session.shutdown())
        .await
        .unwrap()
        .unwrap();
    drop(journal);
    // Reopen the database, not just its in-memory projection. The first retry's
    // response is deliberately lost; reconciliation must not replay that move.
    cloud.stall.store(false, Ordering::SeqCst);
    cloud.lose_move_once.store(true, Ordering::SeqCst);
    let journal = Arc::new(Mutex::new(
        reopened_journal(&spool, &account.id, 1024 * 1024).await,
    ));
    let engine = reopened_engine(account, cloud.clone(), &state, stopped_engine).await;
    let session = WritableSession::mount(engine, journal.clone(), cloud.clone(), vault)
        .await
        .unwrap();
    mutations_applied(&session, 2).await;
    assert_eq!(cloud.reads.load(Ordering::SeqCst), 0);
    assert_eq!(cloud.remote.lock().unwrap().moves.len(), 2);
    application(
        &mount,
        r#"
import os,sys
os.chdir(sys.argv[1])
assert not os.path.exists('online.bin')
assert not os.path.exists('renamed.bin')
assert sorted(os.listdir('folder'))==['final.bin']
with open('folder/final.bin','wb',buffering=0) as f:
    f.write(b'edited after move')
    os.fsync(f.fileno())
with open('folder/final.bin','rb') as f: assert f.read()==b'edited after move'
"#,
    )
    .await;
    acknowledged(&session, 1).await;
    assert_eq!(cloud.reads.load(Ordering::SeqCst), 0);
    assert!(session.namespace_conflicts().unwrap().is_empty());
    {
        let remote = cloud.remote.lock().unwrap();
        let (node, bytes) = remote.files.get("online").unwrap();
        assert_eq!(node.name, "final.bin");
        assert_eq!(node.parent_id.as_deref(), Some("folder"));
        assert_eq!(bytes, b"edited after move");
        assert_eq!(&remote.files.get("occupied").unwrap().1, b"foreign");
    }
    session.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; an uncertain move must not authorize overwriting a foreign save"]
async fn real_save_after_lost_move_preserves_both_local_and_foreign_content() {
    use cirrove_service::journal::MutationState;
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let account = account(&mount);
    let cloud = Arc::new(Cloud::default());
    namespace_fixture(&cloud);
    cloud.lose_move_once.store(true, Ordering::SeqCst);
    cloud.foreign_after_lost_move.store(true, Ordering::SeqCst);
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&temp.path().join("journal"), &account.id, 1024 * 1024).unwrap(),
    ));
    let engine = Engine::new(account, cloud.clone(), temp.path().join("state"))
        .await
        .unwrap();
    let session = WritableSession::mount(
        engine,
        journal.clone(),
        cloud.clone(),
        Arc::new(Vault::default()),
    )
    .await
    .unwrap();
    application(
        &mount,
        r#"
import os,sys
os.chdir(sys.argv[1])
os.rename('online.bin','renamed.bin')
with open('renamed.bin','wb',buffering=0) as f:
    f.write(b'my local save')
    os.fsync(f.fileno())
"#,
    )
    .await;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let records = session.mutations(0, 100).await.unwrap();
            if records.len() == 1 && records[0].state == MutationState::Conflict {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let records = session.uploads(0, 100).await.unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].state, UploadState::Pending);
    assert!(!records[0].base.as_ref().unwrap().resolved);
    assert_eq!(cloud.reads.load(Ordering::SeqCst), 0);
    application(
        &mount,
        r#"
import os,sys
os.chdir(sys.argv[1])
assert not os.path.exists('online.bin')
with open('renamed.bin','rb') as f: assert f.read()==b'my local save'
"#,
    )
    .await;
    {
        let remote = cloud.remote.lock().unwrap();
        assert!(remote.history.is_empty());
        assert_eq!(remote.moves.len(), 1);
        assert_eq!(
            &remote.files.get("online").unwrap().1,
            b"someone else's save"
        );
    }
    session.shutdown().await.unwrap();
    let record = journal
        .lock()
        .unwrap()
        .working_files()
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(
        journal
            .lock()
            .unwrap()
            .read_working(record.id, 0, 100)
            .unwrap(),
        b"my local save"
    );
}

async fn wait_for_cleanup(journal: &Arc<Mutex<UploadJournal>>, spool: &Path, working: usize) {
    // Maintenance handles one object per quiet interval, then removes its bytes
    // in the next pass. The nested-folder fixture deliberately has six objects.
    let result = tokio::time::timeout(Duration::from_secs(24), async {
        loop {
            if journal.lock().unwrap().working_files().unwrap().len() == working
                && std::fs::read_dir(spool.join("objects")).unwrap().count() == 0
                && std::fs::read_dir(spool.join("working")).unwrap().count() == working
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    if result.is_err() {
        let j = journal.lock().unwrap();
        let objects = j
            .namespace_objects()
            .unwrap()
            .into_iter()
            .map(|o| {
                (
                    o.node.name.clone(),
                    o.node.kind.clone(),
                    j.namespace_is_clean(&o).unwrap(),
                    o.follows_remote,
                    o.working_file.is_some(),
                )
            })
            .collect::<Vec<_>>();
        panic!("cleanup did not finish: {objects:?}");
    }
}
async fn refresh_fixture(engine: &Engine, cloud: &Cloud) {
    cirrove_service::refresh(
        cloud,
        &engine.scope("drive"),
        &engine.db,
        false,
        &engine.cancel,
    )
    .await
    .unwrap();
    engine.changed.notify_waiters();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; clean copies hand off to remote metadata without losing local identity"]
async fn real_closed_uploaded_file_follows_remote_edits_and_reclaims_working_storage() {
    use std::os::unix::fs::MetadataExt;
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let state = temp.path().join("state");
    let spool = temp.path().join("journal");
    let account = account(&mount);
    let cloud = Arc::new(Cloud::default());
    let vault = Arc::new(Vault::default());
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&spool, &account.id, 1024 * 1024).unwrap(),
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
    let path = mount.join("local.txt");
    let (mut file, inode) = tokio::task::spawn_blocking(move || {
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
            .unwrap();
        file.write_all(b"mine").unwrap();
        file.sync_all().unwrap();
        let inode = file.metadata().unwrap().ino();
        (file, inode)
    })
    .await
    .unwrap();
    acknowledged(&session, 1).await;
    let upload = session.uploads(0, 10).await.unwrap().pop().unwrap();
    let remote_id = upload.remote.unwrap().id;
    let local_id = journal.lock().unwrap().working_files().unwrap()[0]
        .node
        .id
        .clone();
    assert_ne!(remote_id, local_id);
    // Cleanup actually ran (the acknowledged snapshot is gone), but the open
    // application still owns its working copy and exact local bytes.
    wait_for_cleanup(&journal, &spool, 1).await;
    assert_eq!(journal.lock().unwrap().retained_bytes().unwrap(), 4);
    {
        let mut remote = cloud.remote.lock().unwrap();
        let (node, bytes) = remote.files.get_mut(&remote_id).unwrap();
        *bytes = b"foreign".to_vec();
        node.size = 7;
        node.name = "external.txt".into();
        node.etag = Some("external".into());
        node.content_version = Some("external".into());
    }
    refresh_fixture(&engine, &cloud).await;
    file = tokio::task::spawn_blocking(move || {
        let mut bytes = vec![];
        file.seek(SeekFrom::Start(0)).unwrap();
        std::io::Read::read_to_end(&mut file, &mut bytes).unwrap();
        assert_eq!(bytes, b"mine");
        file
    })
    .await
    .unwrap();
    assert_eq!(journal.lock().unwrap().working_files().unwrap().len(), 1);
    drop(file);
    wait_for_cleanup(&journal, &spool, 0).await;
    assert_eq!(journal.lock().unwrap().retained_bytes().unwrap(), 0);
    application(
        &mount,
        &format!(
            r#"
import os,sys
os.chdir(sys.argv[1])
assert not os.path.exists('local.txt')
assert os.stat('external.txt').st_ino=={inode}
assert open('external.txt','rb').read()==b'foreign'
with open('external.txt','r+b') as f:
    f.write(b'updated');f.flush();os.fsync(f.fileno())
"#
        ),
    )
    .await;
    acknowledged(&session, 2).await;
    let second = session.uploads(0, 10).await.unwrap().pop().unwrap();
    assert_eq!(
        second.intent,
        UploadIntent::Replace {
            item: remote_id.clone(),
            expected_etag: "external".into()
        }
    );
    assert!(second.base.is_none());
    assert_eq!(cloud.remote.lock().unwrap().files[&remote_id].1, b"updated");
    wait_for_cleanup(&journal, &spool, 0).await;
    let stopped = Arc::downgrade(&engine);
    session.shutdown().await.unwrap();
    drop(engine);
    drop(journal);
    let journal = Arc::new(Mutex::new(
        reopened_journal(&spool, &account.id, 1024 * 1024).await,
    ));
    let engine = reopened_engine(account, cloud.clone(), &state, stopped).await;
    let session = WritableSession::mount(engine.clone(), journal.clone(), cloud.clone(), vault)
        .await
        .unwrap();
    application(
        &mount,
        &format!(
            r#"
import os,sys
os.chdir(sys.argv[1])
assert os.stat('external.txt').st_ino=={inode}
assert open('external.txt','rb').read()==b'updated'
"#
        ),
    )
    .await;
    let alias = journal
        .lock()
        .unwrap()
        .namespace_by_remote(&engine.scope("drive"), &remote_id)
        .unwrap()
        .unwrap();
    assert_eq!(alias.node.id, local_id);
    assert!(alias.follows_remote);
    let reads = cloud.reads.load(Ordering::SeqCst);
    application(
        &mount,
        &format!(
            r#"
import os,sys
os.chdir(sys.argv[1])
os.rename('external.txt','renamed.txt')
assert not os.path.exists('external.txt')
assert os.stat('renamed.txt').st_ino=={inode}
assert open('renamed.txt','rb').read()==b'updated'
"#
        ),
    )
    .await;
    mutations_applied(&session, 1).await;
    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            if journal
                .lock()
                .unwrap()
                .namespace_by_remote(&engine.scope("drive"), &remote_id)
                .unwrap()
                .unwrap()
                .follows_remote
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        cloud.reads.load(Ordering::SeqCst),
        reads,
        "metadata-only reactivation downloaded content"
    );
    cloud.remote.lock().unwrap().files.remove(&remote_id);
    let mut db = cirrove_store::Store::open(&engine.db).unwrap();
    let scope = engine.scope("drive");
    let cursor = db.begin(&scope, false).unwrap();
    db.stage(
        &scope,
        cursor.as_ref(),
        &ChangePage {
            changes: vec![Change::Delete { id: remote_id }],
            checkpoint: Checkpoint::Complete(Cursor("deleted".into())),
        },
    )
    .unwrap();
    engine.changed.notify_waiters();
    application(
        &mount,
        r#"
import os,sys,time
os.chdir(sys.argv[1])
for _ in range(100):
    if not os.path.exists('renamed.txt') and 'renamed.txt' not in os.listdir('.'):
        break
    time.sleep(.01)
else:
    raise AssertionError('retired local entry hid a remote deletion')
"#,
    )
    .await;
    // Another acknowledged file disappears before its open application closes.
    // A newer cached external version forces handoff revalidation, whose 404
    // must invalidate that cached directory entry instead of retaining a ghost.
    let path = mount.join("deleted-before-close.txt");
    let open = tokio::task::spawn_blocking(move || {
        let mut f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
            .unwrap();
        f.write_all(b"mine").unwrap();
        f.sync_all().unwrap();
        f
    })
    .await
    .unwrap();
    acknowledged(&session, 3).await;
    let vanished = session
        .uploads(0, 10)
        .await
        .unwrap()
        .pop()
        .unwrap()
        .remote
        .unwrap()
        .id;
    {
        let mut remote = cloud.remote.lock().unwrap();
        let (node, bytes) = remote.files.get_mut(&vanished).unwrap();
        *bytes = b"external".to_vec();
        node.size = 8;
        node.etag = Some("vanishing".into());
        node.content_version = Some("vanishing".into());
    }
    refresh_fixture(&engine, &cloud).await;
    cloud.remote.lock().unwrap().files.remove(&vanished);
    drop(open);
    wait_for_cleanup(&journal, &spool, 0).await;
    application(
        &mount,
        r#"
import os,sys
os.chdir(sys.argv[1])
assert not os.path.exists('deleted-before-close.txt')
assert 'deleted-before-close.txt' not in os.listdir('.')
"#,
    )
    .await;
    session.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; unlink preserves open streams and releases names"]
async fn real_unlinked_handles_keep_their_bytes_and_do_not_upload_later_writes() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let state = temp.path().join("state");
    let spool = temp.path().join("journal");
    let account = account(&mount);
    let cloud = Arc::new(Cloud::default());
    let vault = Arc::new(Vault::default());
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&spool, &account.id, 1024 * 1024).unwrap(),
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
    application(
        &mount,
        r#"
import os,sys
os.chdir(sys.argv[1])
f=os.open('same.txt',os.O_CREAT|os.O_EXCL|os.O_RDWR,0o600)
os.write(f,b'original'); os.fsync(f)
old_inode=os.fstat(f).st_ino
os.unlink('same.txt')
assert not os.path.exists('same.txt')
assert os.fstat(f).st_nlink==0
assert os.pread(f,100,0)==b'original'
g=os.open('same.txt',os.O_CREAT|os.O_EXCL|os.O_RDWR,0o600)
os.write(g,b'replacement'); os.fsync(g)
assert os.fstat(g).st_ino!=old_inode
os.ftruncate(f,0); os.write(f,b'local old stream'); os.fsync(f)
assert os.pread(g,100,0)==b'replacement'
os.close(f); os.close(g)
assert open('same.txt','rb').read()==b'replacement'
"#,
    )
    .await;
    acknowledged(&session, 2).await;
    mutations_applied(&session, 1).await;
    {
        let remote = cloud.remote.lock().unwrap();
        assert_eq!(remote.files.len(), 1);
        assert_eq!(remote.deletes.len(), 1);
        assert_eq!(remote.files.values().next().unwrap().1, b"replacement");
        assert_eq!(
            remote.history,
            vec![b"original".to_vec(), b"replacement".to_vec()]
        );
    }
    let old = journal
        .lock()
        .unwrap()
        .working_files()
        .unwrap()
        .into_iter()
        .find(|w| w.unlinked)
        .unwrap();
    // os.write retains the old descriptor offset after truncate, as on a local FS.
    assert_eq!(
        journal
            .lock()
            .unwrap()
            .read_working(old.id, 0, 100)
            .unwrap(),
        [&[0u8; 8][..], b"local old stream"].concat()
    );
    assert!(old.dirty);
    session.shutdown().await.unwrap();
    engine.cancel.cancel();
    let released = Arc::downgrade(&engine);
    drop(engine);
    assert!(released.upgrade().is_none());
    drop(journal);
    let journal = Arc::new(Mutex::new(
        reopened_journal(&spool, &account.id, 1024 * 1024).await,
    ));
    assert_eq!(
        journal
            .lock()
            .unwrap()
            .read_working(old.id, 0, 100)
            .unwrap(),
        [&[0u8; 8][..], b"local old stream"].concat()
    );
    let engine = reopened_engine(account, cloud.clone(), &state, released).await;
    let session = WritableSession::mount(engine, journal, cloud, vault)
        .await
        .unwrap();
    application(
        &mount,
        r#"
import os,sys
os.chdir(sys.argv[1]); assert os.listdir('.')==['same.txt']
assert open('same.txt','rb').read()==b'replacement'
"#,
    )
    .await;
    session.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; unopened online deletion needs no download"]
async fn real_unopened_online_file_is_deleted_without_hydration() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let account = account(&mount);
    let cloud = Arc::new(Cloud::default());
    namespace_fixture(&cloud);
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&temp.path().join("journal"), &account.id, 1024).unwrap(),
    ));
    let engine = Engine::new(account, cloud.clone(), temp.path().join("state"))
        .await
        .unwrap();
    let session = WritableSession::mount(
        engine,
        journal.clone(),
        cloud.clone(),
        Arc::new(Vault::default()),
    )
    .await
    .unwrap();
    application(
        &mount,
        r#"
import os,sys
os.chdir(sys.argv[1]); os.unlink('online.bin')
assert not os.path.exists('online.bin')
try: os.unlink('folder')
except IsADirectoryError: pass
else: raise AssertionError('directory must not be removed')
assert os.path.isdir('folder')
"#,
    )
    .await;
    mutations_applied(&session, 1).await;
    assert_eq!(cloud.reads.load(Ordering::SeqCst), 0);
    assert_eq!(journal.lock().unwrap().retained_bytes().unwrap(), 0);
    assert!(!cloud.remote.lock().unwrap().files.contains_key("online"));
    assert!(cloud.remote.lock().unwrap().files.contains_key("folder"));
    session.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; a read in flight is retained before cloud deletion"]
async fn real_unlink_waits_for_an_inflight_read_without_blocking_other_files() {
    use std::io::Read;
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let account = account(&mount);
    let cloud = Arc::new(Cloud::default());
    namespace_fixture(&cloud);
    cloud.hold_read.store(true, Ordering::SeqCst);
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&temp.path().join("journal"), &account.id, 1024 * 1024).unwrap(),
    ));
    let engine = Engine::new(account, cloud.clone(), temp.path().join("state"))
        .await
        .unwrap();
    let session =
        WritableSession::mount(engine, journal, cloud.clone(), Arc::new(Vault::default()))
            .await
            .unwrap();
    let path = mount.join("occupied.bin");
    let reading = tokio::task::spawn_blocking(move || {
        let mut file = std::fs::File::open(path).unwrap();
        let mut bytes = vec![];
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"foreign");
        file
    });
    tokio::time::timeout(Duration::from_secs(3), cloud.read_entered.notified())
        .await
        .unwrap();
    let path = mount.join("occupied.bin");
    let deletion = tokio::task::spawn_blocking(move || std::fs::remove_file(path));
    application(
        &mount,
        r#"
import os,sys
os.chdir(sys.argv[1]); assert 'folder' in os.listdir('.')
f=os.open('unrelated.txt',os.O_CREAT|os.O_EXCL|os.O_RDWR,0o600)
os.write(f,b'independent'); os.fsync(f); os.close(f)
"#,
    )
    .await;
    assert!(cloud.remote.lock().unwrap().deletes.is_empty());
    tokio::time::timeout(Duration::from_secs(2), deletion)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    cloud.read_release.notify_one();
    let mut file = tokio::time::timeout(Duration::from_secs(5), reading)
        .await
        .unwrap()
        .unwrap();
    mutations_applied(&session, 1).await;
    file = tokio::task::spawn_blocking(move || {
        use std::os::unix::fs::MetadataExt;
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut bytes = vec![];
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"foreign");
        assert_eq!(file.metadata().unwrap().nlink(), 0);
        file
    })
    .await
    .unwrap();
    drop(file);
    assert!(!cloud.remote.lock().unwrap().files.contains_key("occupied"));
    session.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; quota failure preserves an open remote stream"]
async fn real_unlink_preservation_quota_failure_keeps_remote_bytes_until_last_close() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let account = account(&mount);
    let cloud = Arc::new(Cloud::default());
    namespace_fixture(&cloud);
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&temp.path().join("journal"), &account.id, 1024).unwrap(),
    ));
    let engine = Engine::new(account, cloud.clone(), temp.path().join("state"))
        .await
        .unwrap();
    let session = WritableSession::mount(
        engine,
        journal.clone(),
        cloud.clone(),
        Arc::new(Vault::default()),
    )
    .await
    .unwrap();
    let path = mount.join("online.bin");
    let file = tokio::task::spawn_blocking(move || {
        let file = std::fs::File::open(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        file
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(4), async {
        while session.worker_issue().is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(cloud.remote.lock().unwrap().files.contains_key("online"));
    assert_eq!(cloud.reads.load(Ordering::SeqCst), 0);
    assert_eq!(
        journal
            .lock()
            .unwrap()
            .unlinked_readers(0, 16)
            .unwrap()
            .len(),
        1
    );
    application(
        &mount,
        r#"
import os,sys
os.chdir(sys.argv[1]); assert 'online.bin' not in os.listdir('.')
assert os.path.isdir('folder')
"#,
    )
    .await;
    drop(file);
    mutations_applied(&session, 1).await;
    assert!(!cloud.remote.lock().unwrap().files.contains_key("online"));
    assert_eq!(cloud.reads.load(Ordering::SeqCst), 0);
    session.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; shutdown cancels a reader-preservation barrier"]
async fn real_unlink_shutdown_cancels_an_uncooperative_download_and_resumes_deletion() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let state = temp.path().join("state");
    let spool = temp.path().join("journal");
    let account = account(&mount);
    let cloud = Arc::new(Cloud::default());
    namespace_fixture(&cloud);
    cloud.hold_read.store(true, Ordering::SeqCst);
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&spool, &account.id, 1024 * 1024).unwrap(),
    ));
    let engine = Engine::new(account.clone(), cloud.clone(), state.clone())
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
    let path = mount.join("occupied.bin");
    let reading = tokio::task::spawn_blocking(move || std::fs::read(path));
    tokio::time::timeout(Duration::from_secs(3), cloud.read_entered.notified())
        .await
        .unwrap();
    application(
        &mount,
        r#"
import os,sys
os.chdir(sys.argv[1]); os.unlink('occupied.bin')
assert not os.path.exists('occupied.bin')
"#,
    )
    .await;
    assert_eq!(
        journal
            .lock()
            .unwrap()
            .unlinked_readers(0, 16)
            .unwrap()
            .len(),
        1
    );
    tokio::time::timeout(Duration::from_secs(2), session.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), reading)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert!(cloud.remote.lock().unwrap().files.contains_key("occupied"));
    let released = Arc::downgrade(&engine);
    drop(engine);
    tokio::time::timeout(Duration::from_secs(1), async {
        while released.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(journal);
    let journal = Arc::new(Mutex::new(
        reopened_journal(&spool, &account.id, 1024 * 1024).await,
    ));
    assert!(
        journal
            .lock()
            .unwrap()
            .unlinked_readers(0, 16)
            .unwrap()
            .is_empty()
    );
    let engine = reopened_engine(account, cloud.clone(), &state, released).await;
    let session =
        WritableSession::mount(engine, journal, cloud.clone(), Arc::new(Vault::default()))
            .await
            .unwrap();
    mutations_applied(&session, 1).await;
    assert!(!cloud.remote.lock().unwrap().files.contains_key("occupied"));
    session.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; pending folders, sibling saves, cleanup and remount"]
async fn real_new_directories_accept_children_before_confirmation_and_keep_identity() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let state = temp.path().join("state");
    let account = account(&mount);
    let cloud = Arc::new(Cloud::default());
    cloud.hold_folder.store(true, Ordering::SeqCst);
    let vault = Arc::new(Vault::default());
    let spool = temp.path().join("journal");
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&spool, &account.id, 1024 * 1024).unwrap(),
    ));
    let engine = Engine::new(account.clone(), cloud.clone(), state.clone())
        .await
        .unwrap();
    let stopped = Arc::downgrade(&engine);
    let session = WritableSession::mount(
        engine.clone(),
        journal.clone(),
        cloud.clone(),
        vault.clone(),
    )
    .await
    .unwrap();
    application(
        &mount,
        r#"
import os,sys
os.chdir(sys.argv[1])
os.mkdir('Grüße')
os.mkdir('Grüße/nested')
for path in ['Grüße/first.txt','Grüße/second.txt','Grüße/nested/deep.txt','independent.txt']:
    with open(path,'wb',buffering=0) as f:
        f.write(path.encode())
        os.fsync(f.fileno())
    assert open(path,'rb').read()==path.encode()
assert sorted(os.listdir('Grüße'))==['first.txt','nested','second.txt']
try: os.mkdir('Grüße/FIRST.txt')
except FileExistsError: pass
else: raise AssertionError('file/directory collision was accepted')
"#,
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), cloud.folder_entered.notified())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if cloud.remote.lock().unwrap().history.len() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        cloud.remote.lock().unwrap().files.len(),
        1,
        "children reached the provider before the parent existed"
    );
    let inode = tokio::fs::metadata(mount.join("Grüße")).await.unwrap();
    use std::os::unix::fs::MetadataExt;
    cloud.folder_release.notify_one();
    mutations_applied(&session, 2).await;
    acknowledged(&session, 4).await;
    wait_for_cleanup(&journal, &spool, 0).await;
    let (top, nested) = {
        let remote = cloud.remote.lock().unwrap();
        let top = remote
            .files
            .values()
            .find(|(n, _)| n.name == "Grüße")
            .unwrap()
            .0
            .clone();
        let nested = remote
            .files
            .values()
            .find(|(n, _)| n.name == "nested")
            .unwrap()
            .0
            .clone();
        assert_eq!(nested.parent_id.as_ref(), Some(&top.id));
        for (n, b) in remote
            .files
            .values()
            .filter(|(n, _)| n.kind == NodeKind::File)
        {
            let expected = if n.name == "deep.txt" {
                &nested.id
            } else if n.name == "independent.txt" {
                "root"
            } else {
                &top.id
            };
            assert_eq!(n.parent_id.as_deref(), Some(expected));
            assert!(!b.is_empty());
        }
        (top, nested)
    };
    refresh_fixture(&engine, &cloud).await;
    assert_eq!(
        tokio::fs::metadata(mount.join("Grüße"))
            .await
            .unwrap()
            .ino(),
        inode.ino()
    );
    drop(engine);
    session.shutdown().await.unwrap();
    drop(journal);
    let journal = Arc::new(Mutex::new(
        reopened_journal(&spool, &account.id, 1024 * 1024).await,
    ));
    let engine = reopened_engine(account, cloud.clone(), &state, stopped).await;
    let session = WritableSession::mount(engine.clone(), journal, cloud.clone(), vault)
        .await
        .unwrap();
    assert_eq!(
        tokio::fs::metadata(mount.join("Grüße"))
            .await
            .unwrap()
            .ino(),
        inode.ino()
    );
    // A foreign child arrives after the local folder's own overlay has retired.
    let node = Node {
        id: "foreign-child".into(),
        parent_id: Some(nested.id),
        name: "foreign.txt".into(),
        kind: NodeKind::File,
        size: 7,
        etag: Some("foreign-etag".into()),
        content_version: Some("foreign-content".into()),
        ..root()
    };
    cloud
        .remote
        .lock()
        .unwrap()
        .files
        .insert(node.id.clone(), (node, b"foreign".to_vec()));
    refresh_fixture(&engine, &cloud).await;
    application(
        &mount,
        r#"
import os,sys
os.chdir(sys.argv[1])
assert open('Grüße/first.txt','rb').read()==b'Gr\xc3\xbc\xc3\x9fe/first.txt'
assert open('Grüße/nested/foreign.txt','rb').read()==b'foreign'
with open('Grüße/nested/foreign.txt','ab',buffering=0) as f:
    f.write(b' plus local')
    os.fsync(f.fileno())
os.rename('Grüße/nested/foreign.txt','Grüße/moved.txt')
assert open('Grüße/moved.txt','rb').read()==b'foreign plus local'
"#,
    )
    .await;
    acknowledged(&session, 5).await;
    mutations_applied(&session, 3).await;
    assert!(session.namespace_conflicts().unwrap().is_empty());
    {
        let remote = cloud.remote.lock().unwrap();
        let (node, bytes) = remote.files.get("foreign-child").unwrap();
        assert_eq!(node.parent_id.as_ref(), Some(&top.id));
        assert_eq!(node.name, "moved.txt");
        assert_eq!(bytes, b"foreign plus local");
    }
    drop(engine);
    session.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; uncertain parent retains nested local data after restart"]
async fn real_new_directories_keep_children_when_parent_confirmation_is_lost() {
    use cirrove_service::journal::MutationState;
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let state = temp.path().join("state");
    let account = account(&mount);
    let cloud = Arc::new(Cloud::default());
    cloud.hold_folder.store(true, Ordering::SeqCst);
    let vault = Arc::new(Vault::default());
    let spool = temp.path().join("journal");
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&spool, &account.id, 1024 * 1024).unwrap(),
    ));
    let engine = Engine::new(account.clone(), cloud.clone(), state.clone())
        .await
        .unwrap();
    let stopped = Arc::downgrade(&engine);
    let session = WritableSession::mount(engine, journal.clone(), cloud.clone(), vault.clone())
        .await
        .unwrap();
    application(
        &mount,
        r#"
import os,sys
os.chdir(sys.argv[1])
os.makedirs('pending/nested')
with open('pending/nested/keep.txt','wb',buffering=0) as f:
    f.write(b'recover these bytes')
    os.fsync(f.fileno())
"#,
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), cloud.folder_entered.notified())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), session.shutdown())
        .await
        .unwrap()
        .unwrap();
    drop(journal);
    let journal = Arc::new(Mutex::new(
        reopened_journal(&spool, &account.id, 1024 * 1024).await,
    ));
    let engine = reopened_engine(account, cloud.clone(), &state, stopped).await;
    let session = WritableSession::mount(engine, journal.clone(), cloud.clone(), vault)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if session
                .mutations(0, 100)
                .await
                .unwrap()
                .iter()
                .any(|r| r.state == MutationState::NeedsReview)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    application(
        &mount,
        r#"
import os,sys
os.chdir(sys.argv[1])
assert open('pending/nested/keep.txt','rb').read()==b'recover these bytes'
with open('other.txt','wb',buffering=0) as f:
    f.write(b'independent')
    os.fsync(f.fileno())
"#,
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if session
                .uploads(0, 100)
                .await
                .unwrap()
                .iter()
                .any(|r| r.state == UploadState::Uploaded)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(cloud.remote.lock().unwrap().files.len(), 1);
    let pending = session
        .uploads(0, 100)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.state == UploadState::Pending)
        .unwrap();
    assert!(journal.lock().unwrap().payload(pending.id).is_ok());
    session.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; remote deletion retains local routes across restart"]
async fn real_pending_local_files_keep_deleted_ancestors_and_sharepoint_routes() {
    use cirrove_store::Store;
    for linked in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let mount = temp.path().join("mount");
        std::fs::create_dir(&mount).unwrap();
        let state = temp.path().join("state");
        let account = account(&mount);
        let cloud = Arc::new(Cloud::default());
        cloud.stall.store(true, Ordering::SeqCst);
        let outer = Node {
            id: "outer".into(),
            parent_id: Some("root".into()),
            name: "Documents".into(),
            etag: Some("outer-etag".into()),
            ..root()
        };
        let mut inner = Node {
            id: "inner".into(),
            parent_id: Some("outer".into()),
            name: "Projects".into(),
            etag: Some("inner-etag".into()),
            ..root()
        };
        let link = Node {
            id: "link".into(),
            parent_id: Some("root".into()),
            name: "SharePoint".into(),
            kind: NodeKind::Shortcut,
            target: Some(RemoteRef {
                collection: "shared".into(),
                item: "shared-root".into(),
                kind: Some(NodeKind::Folder),
            }),
            etag: Some("link-etag".into()),
            ..root()
        };
        let shared = Node {
            id: "shared-root".into(),
            name: "shared-root".into(),
            ..root()
        };
        if linked {
            inner.parent_id = Some(shared.id.clone());
        }
        {
            let mut remote = cloud.remote.lock().unwrap();
            for n in [outer, inner, link, shared] {
                remote.files.insert(n.id.clone(), (n, vec![]));
            }
        }
        let vault = Arc::new(Vault::default());
        let spool = temp.path().join("journal");
        let journal = Arc::new(Mutex::new(
            UploadJournal::open(&spool, &account.id, 1024 * 1024).unwrap(),
        ));
        let engine = Engine::new(account.clone(), cloud.clone(), state.clone())
            .await
            .unwrap();
        let stopped = Arc::downgrade(&engine);
        let session = WritableSession::mount(
            engine.clone(),
            journal.clone(),
            cloud.clone(),
            vault.clone(),
        )
        .await
        .unwrap();
        let route = if linked {
            "SharePoint/Projects"
        } else {
            "Documents/Projects"
        };
        let create = format!(
            r#"
import os,sys
os.chdir(sys.argv[1])
with open('{route}/keep.txt','wb',buffering=0) as f:
    f.write(b'local work survives')
    os.fsync(f.fileno())
"#
        );
        application(&mount, &create).await;
        assert_eq!(session.uploads(0, 100).await.unwrap().len(), 1);
        assert!(
            session.mutations(0, 100).await.unwrap().is_empty(),
            "ancestor snapshots must never create cloud mutations"
        );
        cloud.remote.lock().unwrap().files.clear();
        // Publish an actual complete replacement baseline with the folders and
        // the originating link absent. This does not merely clear fixture data.
        for collection in ["drive", "shared"] {
            let scope = engine.scope(collection);
            let mut store = Store::open(&engine.db).unwrap();
            store.begin(&scope, true).unwrap();
            store
                .stage(
                    &scope,
                    None,
                    &ChangePage {
                        changes: vec![Change::Upsert(root())],
                        checkpoint: Checkpoint::Complete(Cursor("after-deletion".into())),
                    },
                )
                .unwrap();
        }
        engine.changed.notify_waiters();
        let read = format!(
            r#"
import os,sys
os.chdir(sys.argv[1])
assert open('{route}/keep.txt','rb').read()==b'local work survives'
assert 'keep.txt' in os.listdir('{route}')
"#
        );
        application(&mount, &read).await;
        drop(engine);
        session.shutdown().await.unwrap();
        drop(journal);
        let journal = Arc::new(Mutex::new(
            reopened_journal(&spool, &account.id, 1024 * 1024).await,
        ));
        let engine = reopened_engine(account, cloud.clone(), &state, stopped).await;
        let session = WritableSession::mount(engine, journal.clone(), cloud.clone(), vault)
            .await
            .unwrap();
        application(&mount, &read).await;
        assert!(session.mutations(0, 100).await.unwrap().is_empty());
        assert!(cloud.remote.lock().unwrap().files.is_empty());
        session.shutdown().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; file links resolve provider IDs to retained local streams"]
async fn real_file_link_retains_current_local_owner_after_remote_deletion() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let state = temp.path().join("state");
    let account = account(&mount);
    let cloud = Arc::new(Cloud::default());
    let vault = Arc::new(Vault::default());
    let spool = temp.path().join("journal");
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&spool, &account.id, 1024 * 1024).unwrap(),
    ));
    let engine = Engine::new(account.clone(), cloud.clone(), state.clone())
        .await
        .unwrap();
    let stopped = Arc::downgrade(&engine);
    let session = WritableSession::mount(
        engine.clone(),
        journal.clone(),
        cloud.clone(),
        vault.clone(),
    )
    .await
    .unwrap();
    application(
        &mount,
        r#"
import os,sys
os.chdir(sys.argv[1])
with open('original.txt','wb',buffering=0) as f:
    f.write(b'original')
    os.fsync(f.fileno())
"#,
    )
    .await;
    acknowledged(&session, 1).await;
    wait_for_cleanup(&journal, &spool, 0).await;
    let remote = cloud
        .remote
        .lock()
        .unwrap()
        .files
        .values()
        .next()
        .unwrap()
        .0
        .clone();
    let object = journal
        .lock()
        .unwrap()
        .namespace_objects()
        .unwrap()
        .into_iter()
        .find(|o| o.remote.is_some())
        .unwrap();
    assert_ne!(
        object.node.id, remote.id,
        "fixture must distinguish local and provider identities"
    );
    let link = Node {
        id: "file-link".into(),
        parent_id: Some("root".into()),
        name: "Shortcut".into(),
        kind: NodeKind::Shortcut,
        target: Some(RemoteRef {
            collection: "drive".into(),
            item: remote.id.clone(),
            kind: Some(NodeKind::File),
        }),
        ..remote
    };
    cloud
        .remote
        .lock()
        .unwrap()
        .files
        .insert(link.id.clone(), (link.clone(), vec![]));
    refresh_fixture(&engine, &cloud).await;
    cloud.stall.store(true, Ordering::SeqCst);
    application(
        &mount,
        r#"
import os,sys
os.chdir(sys.argv[1])
with open('Shortcut','ab',buffering=0) as f:
    f.write(b' plus local')
    os.fsync(f.fileno())
"#,
    )
    .await;
    cloud.remote.lock().unwrap().files.clear();
    {
        let mut store = cirrove_store::Store::open(&engine.db).unwrap();
        let s = engine.scope("drive");
        let cursor = store.begin(&s, true).unwrap();
        store
            .stage(
                &s,
                cursor.as_ref(),
                &ChangePage {
                    changes: vec![Change::Upsert(root())],
                    checkpoint: Checkpoint::Complete(Cursor("deleted".into())),
                },
            )
            .unwrap();
    }
    engine.changed.notify_waiters();
    let read = r#"
import os,sys
os.chdir(sys.argv[1])
assert open('Shortcut','rb').read()==b'original plus local'
assert open('original.txt','rb').read()==b'original plus local'
"#;
    application(&mount, read).await;
    drop(engine);
    session.shutdown().await.unwrap();
    drop(journal);
    let journal = Arc::new(Mutex::new(
        reopened_journal(&spool, &account.id, 1024 * 1024).await,
    ));
    let engine = reopened_engine(account, cloud.clone(), &state, stopped).await;
    let session = WritableSession::mount(engine.clone(), journal, cloud.clone(), vault)
        .await
        .unwrap();
    application(&mount, read).await;
    assert!(session.mutations(0, 100).await.unwrap().is_empty());
    assert!(cloud.remote.lock().unwrap().files.is_empty());
    // A dangling cloud shortcut cannot reopen a locally removed target, even
    // through a still-cached dentry. Already open descriptors keep their bytes.
    cloud
        .remote
        .lock()
        .unwrap()
        .files
        .insert(link.id.clone(), (link, vec![]));
    refresh_fixture(&engine, &cloud).await;
    application(
        &mount,
        r#"
import os,sys
os.chdir(sys.argv[1])
old=open('Shortcut','rb')
os.unlink('original.txt')
try: open('Shortcut','rb')
except FileNotFoundError: pass
else: raise AssertionError('dangling link reopened an unlinked target')
assert old.read()==b'original plus local'
old.close()
"#,
    )
    .await;
    assert_eq!(session.mutations(0, 100).await.unwrap().len(), 1);
    assert!(cloud.remote.lock().unwrap().deletes.is_empty());
    drop(engine);
    session.shutdown().await.unwrap();
}
