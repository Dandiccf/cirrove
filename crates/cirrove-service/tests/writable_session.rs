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
struct NativePublicationPause {
    item: String,
    ready: Arc<Notify>,
    release: Arc<Notify>,
}
#[derive(Default)]
struct Cloud {
    native_publication_pause: Mutex<Option<NativePublicationPause>>,
    refused_write_target: Mutex<Option<String>>,
    admission_targets: Mutex<Vec<String>>,
    hold_admission: AtomicBool,
    admission_entered: Notify,
    admission_release: Notify,
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
    /// Refuse folder removal at the provider only, the way a remote that moved
    /// between the mount's read and the delete does. Changing the stored eTag
    /// instead would not model it: the fixture's listing hands the mount the new
    /// value, so the mount builds a removal that matches and nothing conflicts.
    refuse_folder_removal: AtomicBool,
    folder_entered: Notify,
    folder_release: Notify,
    /// Refuse every content read, so an offline claim can be tested rather than
    /// asserted. Nothing else in this fixture can make the provider unreachable.
    offline: AtomicBool,
    google_names: AtomicBool,
    icloud_identity: AtomicBool,
    cross_parent_folder_move: AtomicBool,
    write_calls: AtomicUsize,
}
fn root() -> Node {
    Node {
        package: false,
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
        if self.icloud_identity.load(Ordering::SeqCst) {
            "icloud"
        } else {
            "fixture"
        }
    }
    async fn changes(
        &self,
        _: &Scope,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> Result<ChangePage, ProviderError> {
        let mut nodes = vec![Change::Upsert(self.root())];
        nodes.extend(
            self.remote
                .lock()
                .unwrap()
                .files
                .values()
                .map(|(n, _)| Change::Upsert(self.read_node(n))),
        );
        Ok(ChangePage {
            changes: nodes,
            checkpoint: Checkpoint::Complete(Cursor("fixture".into())),
        })
    }
}
#[async_trait]
impl ReadProvider for Cloud {
    async fn validate_write_target(
        &self,
        _: &Scope,
        node: &Node,
        _: &CancellationToken,
    ) -> Result<(), ProviderError> {
        self.admission_targets.lock().unwrap().push(node.id.clone());
        if self.hold_admission.swap(false, Ordering::SeqCst) {
            self.admission_entered.notify_one();
            self.admission_release.notified().await;
        }
        if self.refused_write_target.lock().unwrap().as_deref() == Some(node.id.as_str()) {
            return Err(ProviderError::Permission);
        }
        Ok(())
    }
    fn supports_cross_parent_folder_move(&self) -> bool {
        self.cross_parent_folder_move.load(Ordering::SeqCst)
    }

    /// OneDrive's rules, so the mount is tested against the real ones.
    fn name_problem(&self, name: &str) -> Option<cirrove_core::NameProblem> {
        cirrove_onedrive::naming::name_problem(name)
    }
    async fn node(
        &self,
        _: &Scope,
        id: &str,
        _: &CancellationToken,
    ) -> Result<Node, ProviderError> {
        if id == self.root().id {
            return Ok(self.root());
        }
        let pause = self
            .native_publication_pause
            .lock()
            .unwrap()
            .as_ref()
            .filter(|p| p.item == id)
            .map(|p| (p.ready.clone(), p.release.clone()));
        if let Some((ready, release)) = pause {
            ready.notify_one();
            release.notified().await;
        }
        self.remote
            .lock()
            .unwrap()
            .files
            .get(id)
            .map(|(n, _)| self.read_node(n))
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
        if !self.has_parent(&remote, parent) {
            return Err(ProviderError::NotFound);
        }
        Ok(DirectoryPage {
            nodes: remote
                .files
                .values()
                .filter(|(n, _)| n.parent_id.as_deref() == Some(parent))
                .map(|(n, _)| self.read_node(n))
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
        if self.offline.load(Ordering::SeqCst) {
            return Err(ProviderError::Unavailable);
        }
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
    fn root(&self) -> Node {
        let mut node = root();
        if self.icloud_identity.load(Ordering::SeqCst) {
            node.id = "FOLDER::com.apple.CloudDocs::root".into();
        }
        node
    }

    fn read_node(&self, node: &Node) -> Node {
        let mut node = node.clone();
        if self.google_names.load(Ordering::SeqCst) && node.id != "root" {
            let (stem, extension) = node
                .name
                .rsplit_once('.')
                .filter(|(stem, extension)| !stem.is_empty() && extension.len() <= 20)
                .map_or((node.name.as_str(), String::new()), |(stem, extension)| {
                    (stem, format!(".{extension}"))
                });
            node.name = format!("{stem} [{}]{extension}", node.id);
        }
        node
    }

    fn has_parent(&self, remote: &Remote, parent: &str) -> bool {
        parent == self.root().id
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
        self.write_calls.fetch_add(1, Ordering::SeqCst);
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
        self.write_calls.fetch_add(1, Ordering::SeqCst);
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
        self.write_calls.fetch_add(1, Ordering::SeqCst);
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
        self.write_calls.fetch_add(1, Ordering::SeqCst);
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
        if !self.has_parent(&remote, &parent) {
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
            package: false,
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
        _: Option<&SecretString>,
        _: &CancellationToken,
    ) -> upload::Result<Reconciliation> {
        self.write_calls.fetch_add(1, Ordering::SeqCst);
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
    fn present_observation(&self, acknowledged: &Node, observed: Node) -> Node {
        if self.google_names.load(Ordering::SeqCst) {
            let mut raw = observed.clone();
            raw.name = acknowledged.name.clone();
            if self.read_node(&raw).name == observed.name {
                return raw;
            }
        }
        observed
    }

    async fn mutate(
        &self,
        request: &MutationRequest,
        _: &CancellationToken,
    ) -> mutation::Result<MutationReceipt> {
        self.write_calls.fetch_add(1, Ordering::SeqCst);
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
            if !self.has_parent(&remote, parent) {
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
            // The stored folder keeps a *different* eTag from the one the
            // receipt carries. That is measured OneDrive behaviour, not
            // pessimism: a folder's eTag in the create response is not the one
            // the item has a moment later, so anything that conditions a later
            // change on the create receipt loses its precondition.
            //
            // A fixture that echoed the receipt's eTag back would accept exactly
            // the chained folder removal that a live drive rejects -- which is
            // how a wrong fix got past this suite and stranded fourteen folders
            // in a real account.
            let mut settled = node.clone();
            settled.etag = Some(format!("settled-{}", uuid::Uuid::new_v4()));
            remote.files.insert(node.id.clone(), (settled, vec![]));
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
        if let MutationIntent::RemoveFolder { before } = &request.intent {
            if self.refuse_folder_removal.load(Ordering::SeqCst) {
                return Err(MutationError::Conflict);
            }
            let mut remote = self.remote.lock().unwrap();
            let (node, _) = remote
                .files
                .get(&before.id)
                .ok_or(ProviderError::NotFound)?;
            if node.etag != before.etag || node.kind != NodeKind::Folder {
                return Err(MutationError::Conflict);
            }
            // The real adapter checks emptiness immediately before deleting,
            // because Graph's DELETE on a folder is recursive. This fixture
            // refuses a populated folder for the same reason: a mock that
            // cascaded silently would hide the absence of that check.
            if remote
                .files
                .values()
                .any(|(n, _)| n.parent_id.as_ref() == Some(&before.id))
            {
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
        if !self.has_parent(&remote, parent) {
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
        self.write_calls.fetch_add(1, Ordering::SeqCst);
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
        registration: AppRegistration::Microsoft {
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

async fn handed_off(journal: &Arc<Mutex<UploadJournal>>) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if journal
                .lock()
                .unwrap()
                .namespace_objects()
                .unwrap()
                .iter()
                .all(|object| object.follows_remote)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; Google-style read names must survive ordinary mounted writes"]
async fn real_google_style_names_remain_natural_after_create_edit_rename_move_and_handoff() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let account = account(&mount);
    let cloud = Arc::new(Cloud::default());
    cloud.google_names.store(true, Ordering::SeqCst);
    let vault = Arc::new(Vault::default());
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&temp.path().join("journal"), &account.id, 1024 * 1024).unwrap(),
    ));
    let engine = Engine::new(account, cloud.clone(), temp.path().join("state"))
        .await
        .unwrap();
    let session = WritableSession::mount(engine, journal.clone(), cloud, vault)
        .await
        .unwrap();

    let root = mount.clone();
    tokio::task::spawn_blocking(move || {
        std::fs::write(root.join("report.txt"), b"one").unwrap();
    })
    .await
    .unwrap();
    acknowledged(&session, 1).await;
    handed_off(&journal).await;
    assert!(mount.join("report.txt").exists());
    assert!(
        std::fs::read_dir(&mount).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains('['))
    );

    let report = mount.join("report.txt");
    tokio::task::spawn_blocking(move || std::fs::write(report, b"two").unwrap())
        .await
        .unwrap();
    acknowledged(&session, 2).await;
    handed_off(&journal).await;

    let root = mount.clone();
    tokio::task::spawn_blocking(move || {
        std::fs::rename(root.join("report.txt"), root.join("renamed.txt")).unwrap();
        std::fs::create_dir(root.join("folder")).unwrap();
    })
    .await
    .unwrap();
    mutations_applied(&session, 2).await;
    handed_off(&journal).await;

    let root = mount.clone();
    tokio::task::spawn_blocking(move || {
        std::fs::rename(root.join("renamed.txt"), root.join("folder/final.txt")).unwrap();
    })
    .await
    .unwrap();
    mutations_applied(&session, 3).await;
    handed_off(&journal).await;
    assert_eq!(
        std::fs::read(mount.join("folder/final.txt")).unwrap(),
        b"two"
    );

    let root = mount.clone();
    tokio::task::spawn_blocking(move || {
        std::fs::remove_file(root.join("folder/final.txt")).unwrap();
        std::fs::remove_dir(root.join("folder")).unwrap();
    })
    .await
    .unwrap();
    mutations_applied(&session, 5).await;
    session.shutdown().await.unwrap();
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
#[ignore = "requires synthetic kernel FUSE; setattr and fsync through an actual mount"]
async fn real_truncate_and_fsync_reach_the_journal_through_the_kernel() {
    // The journal's truncate path has tests; the FUSE handlers that reach it do
    // not. `setattr` and `fsync` were the two of five named handlers with no
    // coverage through a mount at all -- getxattr and listxattr turned out to be
    // covered already, inside a differently named read-only test.
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let account = account(&mount);
    let cloud = Arc::new(Cloud::default());
    let vault = Arc::new(Vault::default());
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&temp.path().join("journal"), &account.id, 1024 * 1024).unwrap(),
    ));
    let engine = Engine::new(account.clone(), cloud.clone(), temp.path().join("state"))
        .await
        .unwrap();
    let session = WritableSession::mount(engine, journal, cloud.clone(), vault)
        .await
        .unwrap();

    let path = mount.join("truncated.txt");
    let observed = tokio::task::spawn_blocking(move || {
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        std::io::Write::write_all(&mut file, b"twelve chars").unwrap();
        // fsync: the handler must acknowledge rather than fail, and the bytes
        // must survive it.
        file.sync_all().unwrap();
        let after_sync = file.metadata().unwrap().len();

        // setattr with a size: shorten, then extend into a hole.
        file.set_len(5).unwrap();
        let shortened = std::fs::read(&path).unwrap();
        file.set_len(9).unwrap();
        file.sync_data().unwrap();
        let extended = std::fs::read(&path).unwrap();
        (after_sync, shortened, extended)
    })
    .await
    .unwrap();

    let (after_sync, shortened, extended) = observed;
    assert_eq!(after_sync, 12, "fsync must not lose or alter written bytes");
    assert_eq!(shortened, b"twelv", "truncate must shorten in place");
    assert_eq!(
        extended, b"twelv\0\0\0\0",
        "extending must read back as a hole, not as stale bytes"
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
        package: false,
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
            package: false,
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
            let records: Vec<_> = session
                .mutations(0, 100)
                .await
                .unwrap()
                .into_iter()
                .filter(|r| r.state != MutationState::Resolved)
                .collect();
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
#[ignore = "requires synthetic kernel FUSE; a moved folder must retain its nested route"]
async fn real_cross_parent_folder_move_keeps_nested_files_and_refuses_cycles() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let account = account(&mount);
    let cloud = Arc::new(Cloud::default());
    cloud.cross_parent_folder_move.store(true, Ordering::SeqCst);
    {
        let mut remote = cloud.remote.lock().unwrap();
        for (id, parent, name) in [
            ("source", "root", "Source"),
            ("destination", "root", "Destination"),
            ("moved", "source", "Moved"),
            ("nested", "moved", "Nested"),
        ] {
            let folder = Node {
                id: id.into(),
                parent_id: Some(parent.into()),
                name: name.into(),
                etag: Some(format!("etag-{id}")),
                ..root()
            };
            remote.files.insert(id.into(), (folder, vec![]));
        }
        let file = Node {
            id: "child-file".into(),
            parent_id: Some("nested".into()),
            name: "child.txt".into(),
            kind: NodeKind::File,
            size: 13,
            etag: Some("etag-child".into()),
            content_version: Some("content-child".into()),
            ..root()
        };
        remote
            .files
            .insert(file.id.clone(), (file, b"nested bytes!".to_vec()));
    }
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&temp.path().join("journal"), &account.id, 1024 * 1024).unwrap(),
    ));
    let engine = Engine::new(account, cloud.clone(), temp.path().join("state"))
        .await
        .unwrap();
    let session = WritableSession::mount(
        engine.clone(),
        journal,
        cloud.clone(),
        Arc::new(Vault::default()),
    )
    .await
    .unwrap();
    refresh_fixture(&engine, &cloud).await;
    let root = mount.clone();
    tokio::task::spawn_blocking(move || {
        assert_eq!(
            std::fs::read(root.join("Source/Moved/Nested/child.txt")).unwrap(),
            b"nested bytes!"
        );
        std::fs::rename(root.join("Source/Moved"), root.join("Destination/Moved")).unwrap();
        assert_eq!(
            std::fs::read(root.join("Destination/Moved/Nested/child.txt")).unwrap(),
            b"nested bytes!"
        );
        assert!(!root.join("Source/Moved").exists());
        let cycle = std::fs::rename(
            root.join("Destination/Moved"),
            root.join("Destination/Moved/Nested/Cycle"),
        )
        .unwrap_err();
        assert_eq!(cycle.raw_os_error(), Some(libc::EINVAL));
    })
    .await
    .unwrap();
    mutations_applied(&session, 1).await;
    {
        let remote = cloud.remote.lock().unwrap();
        assert_eq!(remote.moves.len(), 1);
        let moved = &remote.files.get("moved").unwrap().0;
        assert_eq!(moved.parent_id.as_deref(), Some("destination"));
        assert_eq!(
            remote.files.get("nested").unwrap().0.parent_id.as_deref(),
            Some("moved")
        );
        assert_eq!(remote.files.get("child-file").unwrap().1, b"nested bytes!");
    }
    session.shutdown().await.unwrap();
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
/// Hurry the mount into observing what the test changed behind its back.
///
/// The engine refreshes the same scope on its own schedule, and a round is
/// exclusive: if its loop opens and closes one between this call's begin and its
/// pages, the store answers `NoRefresh` -- "no refresh is in progress" -- and the
/// fixture blames the daemon for a race the fixture started. Seen once on a CI
/// runner, never in repeated local runs, which is the shape of a race that needs
/// a loaded machine.
///
/// Retried rather than tolerated. The engine's own refresh is doing the same
/// work, so losing the race is not a failure to observe anything; it only means
/// waiting a moment. A refusal that is not the race still panics with its own
/// error, so this covers the one collision it names and nothing else.
async fn refresh_fixture(engine: &Engine, cloud: &Cloud) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        match cirrove_service::refresh(
            cloud,
            &engine.scope("drive"),
            &engine.db,
            false,
            &engine.cancel,
            None,
        )
        .await
        {
            Ok(_) => break,
            Err(error) => {
                let raced = format!("{error:#}").contains("no refresh is in progress");
                assert!(
                    raced && std::time::Instant::now() < deadline,
                    "fixture refresh failed: {error:#}"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }
    engine.changed.metadata();
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
    engine.changed.metadata();
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
            target: Some(Box::new(RemoteRef {
                collection: "shared".into(),
                item: "shared-root".into(),
                kind: Some(NodeKind::Folder),
            })),
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
        engine.changed.metadata();
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
        target: Some(Box::new(RemoteRef {
            collection: "drive".into(),
            item: remote.id.clone(),
            kind: Some(NodeKind::File),
        })),
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
    engine.changed.metadata();
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

async fn applied(session: &WritableSession, count: usize) {
    use cirrove_service::journal::MutationState;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let rows = session.mutations(0, 100).await.unwrap();
            assert!(!rows.iter().any(|r| matches!(
                r.state,
                MutationState::Failed | MutationState::Conflict | MutationState::NeedsReview
            )));
            if rows.len() == count && rows.iter().all(|r| r.state == MutationState::Applied) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

/// Creating a directory and removing it again actually removes it from the
/// provider, rather than reporting success and leaving it there.
///
/// This is the outcome, asserted at the provider rather than at the errno, and
/// it exists because the errno was not enough. A fix that let the removal chain
/// behind its own creation made `rmdir` return success while the conditional
/// DELETE lost its precondition and landed in `Conflict` -- fourteen empty
/// folders left in a real OneDrive, invisible in the mount that had just said
/// they were gone. Every unit test passed throughout, because the fixture echoed
/// the create receipt's eTag back and a live drive does not.
///
/// So this asserts the thing that was actually wrong: after the dust settles,
/// the folder is gone from the provider and nothing is sitting in `Conflict`.
/// Re-chaining folder removals in `validate_mutation_base` makes it fail.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; a created-then-removed directory really leaves the provider"]
async fn real_a_directory_created_and_removed_again_is_gone_from_the_provider() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let account = account(&mount);
    let cloud = Arc::new(Cloud::default());
    namespace_fixture(&cloud);
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

    let target = mount.join("made-and-unmade");
    let path = target.clone();
    tokio::task::spawn_blocking(move || {
        std::fs::create_dir(&path).unwrap();
        // Retrying is the contract: an unsettled creation refuses as EBUSY, and
        // every refusal on the way must stay that -- never a malformed request.
        for _ in 0..100 {
            match std::fs::remove_dir(&path) {
                Ok(()) => return,
                Err(error) => assert_eq!(
                    error.raw_os_error(),
                    Some(libc::EBUSY),
                    "a directory waiting for its creation to settle must refuse as busy: {error}"
                ),
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("the directory never became removable");
    })
    .await
    .unwrap();

    applied(&session, 2).await;
    let remote = cloud.remote.lock().unwrap();
    assert!(
        !remote
            .files
            .values()
            .any(|(node, _)| node.name == "made-and-unmade"),
        "rmdir reported success and the folder is still at the provider"
    );
}

/// A delete the provider refused leaves the item hidden locally and present in
/// the account -- and there has to be a way out of that.
///
/// This is the shape of the incident that produced it: fourteen folder removals
/// ended in `Conflict` on a live drive, the mount said they were gone, the
/// account still had them, and nothing could clear it. `request_mutation_retry`
/// refuses `Conflict` by design and nothing else touched one.
///
/// The way out discards rather than retries. A conflict means the remote moved
/// under us, so re-sending the delete would act on whatever is there now. This
/// drops the local intent instead: the item comes back into view, matching what
/// the provider actually has, and deleting it again is an ordinary `rmdir` built
/// from current state -- asserted here by doing exactly that and watching it
/// reach the provider.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; a refused delete can be abandoned and the item returns"]
async fn real_a_delete_the_provider_refused_can_be_abandoned_and_the_folder_returns() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let account = account(&mount);
    let cloud = Arc::new(Cloud::default());
    namespace_fixture(&cloud);
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&temp.path().join("journal"), &account.id, 1024 * 1024).unwrap(),
    ));
    let engine = Engine::new(account, cloud.clone(), temp.path().join("state"))
        .await
        .unwrap();
    let session = WritableSession::mount(
        engine.clone(),
        journal,
        cloud.clone(),
        Arc::new(Vault::default()),
    )
    .await
    .unwrap();

    // The provider refuses the removal, and the item it refused to remove has
    // moved on -- which is what "the remote changed under us" means and is the
    // only honest reason a conditional delete is refused. Both together, because
    // the second half is what makes the *next* attempt interesting: whatever the
    // mount held as the item's ETag is now stale.
    cloud.refuse_folder_removal.store(true, Ordering::SeqCst);

    // Created through the mount, deliberately: the fixture stores a folder with
    // a different eTag from the one its create receipt carried, which is
    // measured OneDrive behaviour. So the local copy is stale from the moment it
    // exists, and a discard that restored it as-is would hand the second
    // removal the same doomed precondition -- which is what happened on a live
    // drive to four of fourteen folders.
    let created = mount.join("made-then-refused");
    let make = created.clone();
    tokio::task::spawn_blocking(move || {
        std::fs::create_dir(&make).unwrap();
    })
    .await
    .unwrap();
    applied(&session, 1).await;

    let path = created.clone();
    let target = path.clone();
    tokio::task::spawn_blocking(move || {
        // The kernel call succeeds: the local namespace released the name. What
        // the provider does with it is decided afterwards, which is the whole
        // hazard this test is about.
        for _ in 0..100 {
            if std::fs::remove_dir(&target).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!target.exists(), "the mount hides it immediately");
    })
    .await
    .unwrap();

    tokio::time::timeout(Duration::from_secs(10), async {
        while session.stuck_changes().await == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the refused delete was never counted as stuck");
    assert!(
        cloud
            .remote
            .lock()
            .unwrap()
            .files
            .values()
            .any(|(n, _)| n.name == "made-then-refused"),
        "the provider still has it, which is why this matters"
    );

    assert_eq!(session.discard_stuck().await.unwrap(), 1);
    assert_eq!(session.stuck_changes().await, 0);
    // Whatever was wrong at the provider is over. What this asserts is that a
    // restored item is ordinary again -- not that its ETag is fresh, which it
    // need not be: see `discard_stuck_removal` for what a discard does and does
    // not fix.
    cloud.refuse_folder_removal.store(false, Ordering::SeqCst);

    // Back in view, because that is the truth, and removable again for real.
    let restored = path.clone();
    tokio::task::spawn_blocking(move || {
        for _ in 0..100 {
            if restored.is_dir() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("the folder never came back into view");
    })
    .await
    .unwrap();

    // What happens next is one of exactly two things, and the test says which
    // two rather than picking the happier one.
    //
    // `discard_stuck_removal` restores visibility and deliberately not
    // freshness: the restored object keeps whatever eTag the removal was built
    // with, so the next removal may be refused for the original reason and
    // become stuck in its turn. That is measured behaviour -- of fourteen
    // abandoned removals on a live drive, ten deleted cleanly and four
    // conflicted again until the delta feed caught up.
    //
    // So the guarantee is not "the next removal works". It is that the system
    // lands in one of two honest states: the provider has lost the folder, or
    // the folder is stuck again and therefore discardable again. What must never
    // happen is the third state -- the mount hiding an item the provider still
    // has, with nothing counted as stuck -- because that is the shape of the
    // original incident, where fourteen removals were invisible and
    // unrecoverable.
    //
    // This clause used to assert only the happy one, and lost about one run in
    // ten. Raising the budget from ten seconds to forty-five had hidden how
    // little it measured: at the moment the wait expired, `stuck_changes` was 1,
    // so the removal had been refused rather than delayed, and no amount of
    // waiting could help. Driving the feed first and retrying still failed about
    // one run in fifty, and four full turns of discard, remove and refresh did
    // not converge -- see docs/benchmarks/discard-then-remove-convergence.json,
    // which is the open question this test deliberately stops short of.
    let again = path.clone();
    tokio::task::spawn_blocking(move || {
        for _ in 0..200 {
            if std::fs::remove_dir(&again).is_ok() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    })
    .await
    .unwrap()
    .then_some(())
    .expect("the restored folder never became removable");

    let settled = tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            let at_provider = cloud
                .remote
                .lock()
                .unwrap()
                .files
                .values()
                .any(|(n, _)| n.name == "made-then-refused");
            if !at_provider {
                return "the provider lost it";
            }
            if session.stuck_changes().await > 0 {
                return "it is stuck again";
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the second removal neither landed nor was counted as stuck");

    if settled == "it is stuck again" {
        // Recoverable, which is the whole point of the row this test exists for.
        assert_eq!(
            session.discard_stuck().await.unwrap(),
            1,
            "a removal that conflicted again must be discardable in its turn"
        );
        assert_eq!(session.stuck_changes().await, 0);
        let back = path.clone();
        tokio::task::spawn_blocking(move || {
            for _ in 0..200 {
                if back.is_dir() {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            false
        })
        .await
        .unwrap()
        .then_some(())
        .expect("the folder must come back into view again after the second discard");
    } else {
        assert_eq!(
            session.stuck_changes().await,
            0,
            "nothing is left stuck once the removal has landed"
        );
    }
    session.shutdown().await.unwrap();
}

/// A trash directory that is already in the drive cannot be used as one either.
///
/// The `mkdir` guard stops one being created. It does nothing about a drive that
/// already has one -- left by an earlier Cirrove, or by another tool -- and
/// trashing is a *rename* into `.Trash-$uid/files/`, not a mkdir. Without this
/// the guard would hold only for drives that never had a wastebasket, which is
/// precisely not the drives that need it.
///
/// Renaming back out stays allowed. A user whose drive already contains one must
/// be able to recover what is in it, and a guard that trapped those files would
/// be worse than the wastebasket.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; an existing trash directory refuses to be filled"]
async fn real_an_existing_trash_directory_refuses_renames_into_it_but_not_out_of_it() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let account = account(&mount);
    let cloud = Arc::new(Cloud::default());
    namespace_fixture(&cloud);
    // A wastebasket that is already in the drive, with the layout the trash
    // specification gives it and a file the user would want back.
    {
        let mut remote = cloud.remote.lock().unwrap();
        for (id, name, parent) in [
            (".trash", ".Trash-1000", "root"),
            (".trash-files", "files", ".trash"),
        ] {
            let node = Node {
                id: id.into(),
                parent_id: Some(parent.into()),
                name: name.into(),
                etag: Some(format!("{id}-etag")),
                ..root()
            };
            remote.files.insert(node.id.clone(), (node, vec![]));
        }
        let stranded = Node {
            package: false,
            id: "stranded".into(),
            parent_id: Some(".trash-files".into()),
            name: "stranded.txt".into(),
            kind: NodeKind::File,
            size: 8,
            modified_unix: 1,
            etag: Some("stranded-etag".into()),
            content_version: Some("stranded-content".into()),
            target: None,
        };
        remote
            .files
            .insert(stranded.id.clone(), (stranded, b"recovery".to_vec()));
    }

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

    let root = mount.clone();
    tokio::task::spawn_blocking(move || {
        let trash = root.join(".Trash-1000");
        // Into the wastebasket, at both depths a file manager uses.
        for destination in [
            trash.join("occupied.bin"),
            trash.join("files").join("occupied.bin"),
        ] {
            let error = std::fs::rename(root.join("occupied.bin"), &destination).unwrap_err();
            assert_eq!(
                error.raw_os_error(),
                Some(libc::EOPNOTSUPP),
                "renaming into {destination:?}: {error}"
            );
        }
        // The file it was supposed to swallow is untouched and still readable.
        assert_eq!(
            std::fs::read(root.join("occupied.bin")).unwrap(),
            b"foreign"
        );
        // Out of the wastebasket is how a user recovers, and must keep working.
        std::fs::rename(
            trash.join("files").join("stranded.txt"),
            root.join("stranded.txt"),
        )
        .unwrap();
        assert_eq!(
            std::fs::read(root.join("stranded.txt")).unwrap(),
            b"recovery"
        );
    })
    .await
    .unwrap();

    mutations_applied(&session, 1).await;
    {
        let remote = cloud.remote.lock().unwrap();
        let (node, _) = remote.files.get("stranded").unwrap();
        assert_eq!(node.parent_id.as_deref(), Some("root"));
        assert_eq!(node.name, "stranded.txt");
    }
    session.shutdown().await.unwrap();
}

/// Removing a directory the provider has not acknowledged yet reports that it is
/// busy, and says so in a word the caller can act on.
///
/// `Writeback::rmdir` already refuses this case deliberately: an unacknowledged
/// directory has no ETag, so no conditional removal can be expressed against it,
/// and cancelling an in-flight creation is a different operation. The refusal is
/// right. Which errno carries it is what this pins down -- found on a live mount,
/// where creating a folder and immediately removing it produced "Invalid
/// argument", a message that describes nothing the caller did and suggests no
/// way forward. `EBUSY` says the one true thing: not now, try again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; an unconfirmed directory refuses removal as busy"]
async fn real_rmdir_of_an_unconfirmed_directory_reports_busy_rather_than_invalid() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let account = account(&mount);
    let cloud = Arc::new(Cloud::default());
    namespace_fixture(&cloud);
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&temp.path().join("journal"), &account.id, 1024 * 1024).unwrap(),
    ));
    let engine = Engine::new(account, cloud.clone(), temp.path().join("state"))
        .await
        .unwrap();
    // Hold the provider inside the folder creation, so the directory exists
    // locally and has no remote identity for as long as the test needs.
    cloud.hold_folder.store(true, Ordering::SeqCst);
    let session =
        WritableSession::mount(engine, journal, cloud.clone(), Arc::new(Vault::default()))
            .await
            .unwrap();

    let target = mount.join("unconfirmed");
    let created = target.clone();
    tokio::task::spawn_blocking(move || std::fs::create_dir(&created).unwrap())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), cloud.folder_entered.notified())
        .await
        .expect("the provider never entered the folder creation");

    let pending = target.clone();
    let error = tokio::task::spawn_blocking(move || std::fs::remove_dir(&pending).unwrap_err())
        .await
        .unwrap();
    assert_eq!(
        error.raw_os_error(),
        Some(libc::EBUSY),
        "an unconfirmed directory must refuse removal as busy, not as a malformed \
         request the caller cannot act on: {error}"
    );
    // The refusal must leave the directory alone rather than half-removing it.
    assert!(target.is_dir(), "the directory went away on a refusal");

    // Once the provider acknowledges, the same removal succeeds. Without this the
    // test would pass just as well against a mount that never removes anything.
    cloud.folder_release.notify_one();
    mutations_applied(&session, 1).await;
    let confirmed = target.clone();
    tokio::task::spawn_blocking(move || {
        // Every refusal on the way must stay actionable. On a live mount this
        // band -- after the provider acknowledged, before the creation had
        // settled -- answered EINVAL for about two seconds, which tells the
        // caller its request was malformed when the only true answer was "not
        // yet".
        for _ in 0..50 {
            match std::fs::remove_dir(&confirmed) {
                Ok(()) => return,
                Err(error) => assert_eq!(
                    error.raw_os_error(),
                    Some(libc::EBUSY),
                    "a directory waiting for its remote identity must refuse as \
                     busy, not as a malformed request: {error}"
                ),
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("a confirmed directory never became removable");
    })
    .await
    .unwrap();
    session.shutdown().await.unwrap();
}

/// The mount root refuses to become a local wastebasket, and only the root does.
///
/// This is not hypothetical. On a writable mount the first Delete in GNOME Files
/// creates `.Trash-1000/files` and `.Trash-1000/info` at the top of the
/// filesystem and renames the file into it -- so before this guard, deleting a
/// file through the file manager put a second wastebasket *inside the user's
/// cloud drive*, synced to every other device, while the provider's own recycle
/// bin stayed empty and the file manager reported the deletion as undoable. It
/// was found as a real directory in a real OneDrive, not by reading the spec.
///
/// Removing the `is_trash_directory` check in `mkdir` makes the first assertion
/// fail with a created directory instead of `Unsupported`.
///
/// The second half is the other half of the bug: a guard that refused the name
/// everywhere would cost the user an ordinary folder name for nothing, because
/// no trash implementation looks anywhere but the top of the filesystem.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; the mount root refuses a trash directory"]
async fn real_mount_root_refuses_a_trash_directory_but_a_subdirectory_keeps_the_name() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let account = account(&mount);
    let cloud = Arc::new(Cloud::default());
    namespace_fixture(&cloud);
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

    let root = mount.clone();
    tokio::task::spawn_blocking(move || {
        for name in [".Trash", ".Trash-1000"] {
            let error = std::fs::create_dir(root.join(name)).unwrap_err();
            assert_eq!(
                error.kind(),
                std::io::ErrorKind::Unsupported,
                "{name} at the mount root: {error}"
            );
            assert!(!root.join(name).exists(), "{name} was created anyway");
        }
        // Inside the drive the name is the user's to use.
        std::fs::create_dir(root.join("folder").join(".Trash-1000")).unwrap();
    })
    .await
    .unwrap();

    // Exactly one namespace change reached the provider: the nested folder. The
    // refusals must not have queued anything to undo later.
    applied(&session, 1).await;
    {
        let remote = cloud.remote.lock().unwrap();
        let names: Vec<&str> = remote
            .files
            .values()
            .map(|(node, _)| node.name.as_str())
            .collect();
        assert_eq!(
            names.iter().filter(|n| n.starts_with(".Trash")).count(),
            1,
            "remote names: {names:?}"
        );
    }
    session.shutdown().await.unwrap();
}

/// `rmdir` keeps its POSIX promise: a populated directory is refused, an empty
/// one is removed, and the removal reaches the provider.
///
/// `ENOTEMPTY` comes from the mount's own listing. It has to, because nothing the
/// provider offers can carry it: Graph's DELETE on a folder is recursive and a
/// folder's eTag does not move when a child is added, so a precondition cannot
/// express "only if still empty". Removing the listing check in `rmdir` makes the
/// first assertion here fail, and the synthetic cloud then refuses the delete --
/// which is the second line of defence doing its job, not a passing test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; rmdir refuses a populated directory and removes an empty one"]
async fn real_rmdir_refuses_a_populated_directory_and_removes_an_empty_one() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let account = account(&mount);
    let cloud = Arc::new(Cloud::default());
    namespace_fixture(&cloud);
    {
        let inside = Node {
            package: false,
            id: "inside".into(),
            parent_id: Some("folder".into()),
            name: "inside.txt".into(),
            kind: NodeKind::File,
            size: 3,
            modified_unix: 1,
            etag: Some("inside-etag".into()),
            content_version: Some("inside-content".into()),
            target: None,
        };
        cloud
            .remote
            .lock()
            .unwrap()
            .files
            .insert(inside.id.clone(), (inside, b"abc".to_vec()));
    }
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

    let path = mount.join("folder");
    let populated = path.clone();
    tokio::task::spawn_blocking(move || {
        let error = std::fs::remove_dir(&populated).unwrap_err();
        assert_eq!(
            error.kind(),
            std::io::ErrorKind::DirectoryNotEmpty,
            "populated directory: {error}"
        );
        // The refusal must not have removed anything on the way.
        assert_eq!(std::fs::read(populated.join("inside.txt")).unwrap(), b"abc");
        std::fs::remove_file(populated.join("inside.txt")).unwrap();
        std::fs::remove_dir(&populated).unwrap();
        assert!(!populated.exists());
    })
    .await
    .unwrap();

    applied(&session, 2).await;
    {
        let remote = cloud.remote.lock().unwrap();
        assert!(!remote.files.contains_key("folder"));
        assert!(!remote.files.contains_key("inside"));
    }
    session.shutdown().await.unwrap();
}

/// The account manager mounts writable only for an account carrying a write
/// grant, and read-only for every other one.
///
/// Until this landed the daemon called `CloudFs::new` unconditionally, so a write
/// grant changed the recorded permission and nothing else: every mount stayed
/// read-only and `reauth --write-access` bought nothing. Restoring that makes the
/// ReadWrite row below fail on the mkdir.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; the manager selects a writable mount from the grant"]
async fn real_manager_mounts_writable_only_for_an_account_with_a_write_grant() {
    use cirrove_auth::AccessMode;
    use cirrove_service::accounts::Settings;
    use cirrove_service::manager::{Manager, ProviderFactory, WriteFactory};
    for (access, writable) in [(AccessMode::ReadWrite, true), (AccessMode::ReadOnly, false)] {
        let temp = tempfile::tempdir().unwrap();
        let mount = temp.path().join("mount");
        std::fs::create_dir(&mount).unwrap();
        let state = temp.path().join("state");
        cirrove_service::private_dir(&state).unwrap();
        let mut config = account(&mount);
        config.access = access;
        config.enabled = true;
        config.cache_bytes = 64 * 1024 * 1024;
        // The manager loads through `Settings`, which enforces the label rules the
        // rest of these fixtures never go through.
        config.label = "writable-fixture".into();
        std::fs::write(
            state.join("accounts.json"),
            serde_json::to_vec(&Settings {
                version: 2,
                accounts: vec![config],
            })
            .unwrap(),
        )
        .unwrap();
        let cloud = Arc::new(Cloud::default());
        cloud.google_names.store(true, Ordering::SeqCst);
        namespace_fixture(&cloud);
        let reads = cloud.clone();
        let writes = cloud.clone();
        let read_factory: ProviderFactory = Arc::new(move |_| Ok(reads.clone()));
        let factory_state = state.clone();
        let refuse_writes = Arc::new(AtomicBool::new(false));
        let factory_refusal = refuse_writes.clone();
        let write_factory: WriteFactory = Arc::new(move |account, context| {
            let directory = factory_state.join("accounts").join(&account.id);
            assert_eq!(context.state(), factory_state);
            assert_eq!(context.metadata_db(), directory.join("metadata.db"));
            assert!(context.journal().try_lock().is_ok());
            assert!(
                UploadJournal::open(&directory.join("journal"), &account.id, 64 * 1024 * 1024)
                    .is_err(),
                "write factory was given an unopened or foreign journal"
            );
            assert!(
                directory.join("metadata.db").is_file(),
                "write factory ran before account storage was opened"
            );
            assert!(
                cirrove_service::accounts::account_lock(&directory).is_err(),
                "write factory ran without the account owner lock"
            );
            anyhow::ensure!(
                !factory_refusal.load(Ordering::SeqCst),
                "synthetic writer unavailable"
            );
            Ok(writes.clone())
        });
        let cancel = CancellationToken::new();
        let (manager, worker) = Manager::start_with_providers(
            state.clone(),
            cancel.clone(),
            read_factory,
            Some(write_factory),
        );
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            let seen: Vec<_> = manager
                .status
                .read()
                .await
                .iter()
                .map(|s| (s.state.clone(), s.mounted))
                .collect();
            if seen.first().is_some_and(|(_, mounted)| *mounted) {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "never mounted for {access:?}; status {seen:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let target = mount.join("made-by-the-daemon");
        let created = tokio::task::spawn_blocking(move || std::fs::create_dir(&target))
            .await
            .unwrap();
        match (writable, created) {
            (true, Ok(())) => {}
            (false, Err(error)) => assert_eq!(
                error.kind(),
                std::io::ErrorKind::ReadOnlyFilesystem,
                "read-only account: {error}"
            ),
            (expected, result) => {
                panic!("writable={expected} but mkdir gave {result:?} for {access:?}")
            }
        }
        let mut control_server = None;
        if writable {
            let socket = state.join("control.sock");
            let server = tokio::spawn(cirrove_service::serve_managed(
                state.join("control.db"),
                socket.clone(),
                cancel.clone(),
                Some(manager.clone()),
            ));
            tokio::time::timeout(Duration::from_secs(2), async {
                while !socket.exists() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            control_server = Some(server);
            let path = mount.join("made-by-the-daemon/pinned.txt");
            tokio::task::spawn_blocking(move || std::fs::write(path, b"pin through visible path"))
                .await
                .unwrap()
                .unwrap();
            let request = cirrove_service::PinRequest {
                label: "writable-fixture".into(),
                path: Some("made-by-the-daemon/pinned.txt".into()),
                ..Default::default()
            };
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            let reply = loop {
                let reply = cirrove_service::pin(&socket, &request).await.unwrap();
                if reply.accepted {
                    break reply;
                }
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "the visible natural Google-style path never resolved for pinning: {reply:?}"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            };
            assert!(reply.reserved > 0);
            let engine = manager.engine("writable-fixture").await.unwrap();
            assert!(
                engine
                    .pin_status()
                    .await
                    .unwrap()
                    .iter()
                    .any(|pin| pin.item == reply.item && pin.reserved == reply.reserved),
                "the accepted visible-path pin was not recorded"
            );
            let states = cirrove_service::paths(
                &socket,
                &cirrove_service::PathsRequest {
                    label: "writable-fixture".into(),
                    paths: vec!["made-by-the-daemon/pinned.txt".into()],
                },
            )
            .await
            .unwrap();
            assert_eq!(states.states.len(), 1);
            assert_eq!(states.states[0].item, reply.item);
            assert_eq!(states.states[0].pinned.as_deref(), Some("direct"));
            assert_eq!(states.states[0].refusal, None);
            let unpinned = cirrove_service::unpin(&socket, &request).await.unwrap();
            assert!(
                unpinned.accepted,
                "visible path did not unpin: {unpinned:?}"
            );
        }
        // An unavailable writer must not silently remount a granted account
        // read-only and hide journal-backed edits. Clean up before asserting.
        let remount_result: anyhow::Result<()> = async {
            if !writable {
                return Ok(());
            }
            refuse_writes.store(true, Ordering::SeqCst);
            let unmounted = tokio::process::Command::new("fusermount3")
                .args(["-u", "-z", "--"])
                .arg(&mount)
                .status()
                .await?;
            anyhow::ensure!(unmounted.success(), "synthetic ejection failed");
            tokio::time::timeout(Duration::from_secs(20), async {
                loop {
                    let rows = manager.status.read().await;
                    if rows
                        .first()
                        .is_some_and(|row| !row.mounted && row.state.contains("mount unavailable"))
                    {
                        break;
                    }
                    drop(rows);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .map_err(|_| anyhow::anyhow!("writer failure was hidden by a readonly fallback"))?;
            refuse_writes.store(false, Ordering::SeqCst);
            tokio::time::timeout(Duration::from_secs(20), async {
                loop {
                    if manager
                        .status
                        .read()
                        .await
                        .first()
                        .is_some_and(|row| row.mounted)
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .map_err(|_| anyhow::anyhow!("writer did not recover after remount"))?;
            let path = mount.join("after-remount");
            tokio::task::spawn_blocking(move || std::fs::create_dir(path)).await??;
            Ok(())
        }
        .await;
        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(20), worker).await;
        if let Some(server) = control_server {
            let _ = tokio::time::timeout(Duration::from_secs(5), server).await;
        }
        remount_result.unwrap();
    }
}

/// A refused save must be explicable by something other than the kernel.
///
/// `writeback::error` maps a full budget and a full disk to the same `ENOSPC`,
/// which is right -- that is what an application can act on -- and it is also
/// everything the application is told. The remedies are opposite: a budget
/// clears itself as uploads drain, a disk does not. writeback.rs carried a
/// comment saying the difference was "reported through status"; `AccountStatus`
/// had no such field and nothing filled one, so the distinction died at the
/// syscall boundary.
///
/// This drives a real mount into its budget through the kernel and asserts the
/// engine can name which of the two happened.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; a refused save names the budget rather than only ENOSPC"]
async fn real_a_refused_save_is_reported_as_a_budget_and_not_only_as_enospc() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let account = account(&mount);
    let cloud = Arc::new(Cloud::default());
    // Nothing drains, so the budget cannot recover underneath the assertion.
    cloud.stall.store(true, Ordering::SeqCst);
    let vault = Arc::new(Vault::default());
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&temp.path().join("journal"), &account.id, 64 * 1024).unwrap(),
    ));
    let engine = Engine::new(account, cloud.clone(), temp.path().join("state"))
        .await
        .unwrap();
    let watched = engine.clone();
    let session = WritableSession::mount(engine, journal.clone(), cloud.clone(), vault)
        .await
        .unwrap();

    assert!(
        watched.save_refusals.latest().is_none(),
        "nothing has been refused yet; a field that is already set would make the \
         assertion below meaningless"
    );

    // Write past the budget through the kernel. Which write crosses it is not
    // fixed -- the journal spends the budget on its own bookkeeping too -- so
    // the loop asserts that one of them does, and that it is ENOSPC.
    let mut refused = None;
    let mut accepted = 0;
    for index in 0..64 {
        let path = mount.join(format!("file-{index}.bin"));
        match std::fs::write(&path, vec![b'x'; 8 * 1024]) {
            Ok(()) => accepted += 1,
            Err(failure) => {
                refused = Some(failure);
                break;
            }
        }
    }
    let refused = refused.expect("a 64 KiB budget cannot hold 512 KiB of writes");
    assert!(
        accepted > 0,
        "a mount that refuses the first write is broken rather than full, and          would satisfy every assertion below for the wrong reason"
    );
    assert_eq!(
        refused.raw_os_error(),
        Some(libc::ENOSPC),
        "the kernel must still report ENOSPC, which is what an application acts on: {refused}"
    );

    let refusal = watched
        .save_refusals
        .latest()
        .expect("a refused save must leave something status can report");
    assert_eq!(
        refusal.kind, "budget",
        "the budget is full and the disk is not; reporting `device` would send a \
         user to free space that would not help"
    );
    assert!(
        refusal.message.contains("cache_bytes"),
        "the budget message has to say what makes room now: {}",
        refusal.message
    );
    session.shutdown().await.unwrap();
}

/// A pinned file edited with the provider unreachable, through the kernel.
///
/// `engine::pinning::a_pinned_file_can_be_edited_offline_and_both_survive_a_restart`
/// makes this claim at the durability layer, and makes it well: the cache's view
/// and the journal's are rebuilt by separate paths that do not consult each
/// other, so their surviving together has to be asserted rather than assumed.
/// What it never touches is the kernel. This does the same thing through a real
/// writable mount, which is the only place an ordinary application lives.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; a pinned file is edited with the provider unreachable"]
async fn real_a_pinned_file_is_edited_offline_and_both_survive_through_the_mount() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let mut account = account(&mount);
    // The pin floor is eight blocks; the fixture's own budget cannot hold one.
    account.cache_bytes = 64 * 1024 * 1024;
    let cloud = Arc::new(Cloud::default());
    let original = b"the bytes that were there before".to_vec();
    {
        let node = Node {
            package: false,
            id: "pinned".into(),
            parent_id: Some("root".into()),
            name: "pinned.txt".into(),
            kind: NodeKind::File,
            size: original.len() as u64,
            modified_unix: 1,
            etag: Some("original-etag".into()),
            content_version: Some("original-content".into()),
            target: None,
        };
        cloud
            .remote
            .lock()
            .unwrap()
            .files
            .insert(node.id.clone(), (node, original.clone()));
    }
    let vault = Arc::new(Vault::default());
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&temp.path().join("journal"), &account.id, 4 * 1024 * 1024).unwrap(),
    ));
    let engine = Engine::new(account.clone(), cloud.clone(), temp.path().join("state"))
        .await
        .unwrap();
    let pin = cirrove_service::PinRequest {
        path: Some("pinned.txt".into()),
        ..Default::default()
    };
    assert!(
        engine.apply_pin_request(&pin).await.unwrap().accepted,
        "the budget holds one small file"
    );
    let session = WritableSession::mount(engine, journal.clone(), cloud.clone(), vault)
        .await
        .unwrap();

    // From here the provider answers nothing. Both halves happen offline.
    cloud.offline.store(true, Ordering::SeqCst);
    let path = mount.join("pinned.txt");
    let read_back = {
        let path = path.clone();
        tokio::task::spawn_blocking(move || std::fs::read(path))
            .await
            .unwrap()
            .expect("the pinned file must read offline through the mount")
    };
    assert_eq!(read_back, original, "offline read returned the wrong bytes");

    let edited = b"the bytes an application wrote while offline".to_vec();
    {
        let path = path.clone();
        let edited = edited.clone();
        tokio::task::spawn_blocking(move || std::fs::write(path, edited))
            .await
            .unwrap()
            .expect("editing a pinned file offline must be accepted locally");
    }
    session.shutdown().await.unwrap();

    // Rebuilt from disk: the cache's registry and the journal's files are
    // reconstructed by paths that do not consult each other.
    let engine = Engine::new(account, cloud.clone(), temp.path().join("state"))
        .await
        .unwrap();
    let pins = engine.pin_status().await.unwrap();
    assert_eq!(pins.len(), 1, "the pin must survive the restart");
    assert!(
        pins[0].reserved > 0,
        "the pin lost its reservation across the restart: {:?}",
        pins[0]
    );
    let unsent = journal.lock().unwrap().list(0, 100).unwrap();
    assert!(
        !unsent.is_empty(),
        "the offline edit must still be waiting to upload after a restart"
    );
    engine.stop().await;
}

/// Unsent changes must survive disabling an account, and removal must refuse
/// while they exist.
///
/// This was the milestone's open clause and it could not be tested, because
/// there was no removal path: a test would have asserted that a thing which
/// does not exist does not delete a journal, and would have passed before and
/// after any change for the same reason. `accounts::forget` exists now, so the
/// clause has a subject.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; unsent work survives disable and blocks removal"]
async fn real_unsent_changes_survive_disabling_and_refuse_removal() {
    use cirrove_service::accounts::{Settings, forget, set_enabled};
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let state = temp.path().join("state");
    cirrove_service::private_dir(&state).unwrap();
    let mut config = account(&mount);
    config.label = "removable".into();
    config.enabled = false;
    std::fs::write(
        state.join("accounts.json"),
        serde_json::to_vec(&Settings {
            version: 2,
            accounts: vec![config.clone()],
        })
        .unwrap(),
    )
    .unwrap();

    // Uploads stall, so what the application writes stays unsent.
    let cloud = Arc::new(Cloud::default());
    cloud.stall.store(true, Ordering::SeqCst);
    let vault = Arc::new(Vault::default());
    let account_dir = state.join("accounts").join(&config.id);
    cirrove_service::private_dir(&account_dir).unwrap();
    let journal_dir = account_dir.join("journal");
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&journal_dir, &config.id, 4 * 1024 * 1024).unwrap(),
    ));
    let engine = Engine::new(config.clone(), cloud.clone(), account_dir.clone())
        .await
        .unwrap();
    let session = WritableSession::mount(engine, journal.clone(), cloud.clone(), vault)
        .await
        .unwrap();
    let written = b"work that never reached the cloud".to_vec();
    {
        let path = mount.join("unsent.txt");
        let written = written.clone();
        tokio::task::spawn_blocking(move || std::fs::write(path, written))
            .await
            .unwrap()
            .unwrap();
    }
    session.shutdown().await.unwrap();
    drop(journal);

    // Disabling is a settings flag. Nothing in that path may touch the journal.
    set_enabled(&state, "removable", false).unwrap();
    let reopened = UploadJournal::open(&journal_dir, &config.id, 4 * 1024 * 1024).unwrap();
    let rows = reopened.list(0, 100).unwrap();
    assert!(
        !rows.is_empty(),
        "disabling an account must not discard work that has not been sent"
    );
    let mut payload = Vec::new();
    std::io::Read::read_to_end(&mut reopened.payload(rows[0].id).unwrap(), &mut payload).unwrap();
    assert_eq!(
        payload, written,
        "the unsent bytes changed across a disable"
    );
    drop(reopened);

    // Removal must refuse, and say how much is at stake.
    let refused = forget(&state, "removable", false).expect_err("removal must refuse");
    let message = refused.to_string();
    assert!(
        message.contains("have not reached the cloud") && message.contains("discard-unsent"),
        "a refusal has to name what is at stake and the way past it: {message}"
    );
    assert!(
        journal_dir.exists(),
        "a refused removal must leave the journal exactly where it was"
    );

    // Asked explicitly, it moves the data aside rather than deleting it.
    let done = forget(&state, "removable", true).expect("explicit discard must be accepted");
    assert!(done.contains("moved to"), "unexpected report: {done}");
    assert!(!account_dir.exists(), "the account directory must be gone");
    let removed: Vec<_> = std::fs::read_dir(state.join("removed"))
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    assert_eq!(removed.len(), 1, "the data must be kept, not deleted");
    assert!(
        removed[0].path().join("journal").exists(),
        "the journal must be inside what was moved aside"
    );
    assert!(
        Settings::load(&state).unwrap().accounts.is_empty(),
        "the account must be out of the settings"
    );
    assert!(
        mount.exists(),
        "removal must never touch the mount directory itself"
    );
}

/// A save refused by a physically full filesystem, through the kernel.
///
/// `a_genuinely_full_filesystem_explains_what_to_free` drives the journal
/// directly, and `real_a_refused_save_is_reported_as_a_budget_and_not_only_as_enospc`
/// drives a mount into its budget. The device case has never been driven through
/// a mount, which is why the ledger records it as a mapping rather than a
/// journey. This closes that: a writable mount whose state lives on a filesystem
/// with no free block.
///
/// Ignored and configured by one variable, like its sibling: it consumes every
/// free block of the filesystem it is given, so it gets one of its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "set CIRROVE_FULL_DISK_DIR to a directory on a small, disposable filesystem"]
async fn real_a_save_on_a_full_device_is_reported_as_a_device_and_not_a_budget() {
    let Some(root) = std::env::var_os("CIRROVE_FULL_DISK_DIR").map(std::path::PathBuf::from) else {
        panic!("CIRROVE_FULL_DISK_DIR is required; this test fills the filesystem it names");
    };
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let account = account(&mount);
    let cloud = Arc::new(Cloud::default());
    cloud.stall.store(true, Ordering::SeqCst);
    let vault = Arc::new(Vault::default());
    // State and journal on the small filesystem; the mount point itself stays on
    // the ordinary one, because it is the saves that must meet the full device.
    //
    // Named for this test rather than `state` and `journal`, because CI hands
    // the same loop image to the journal-level full-disk test first and a
    // journal belongs to one account: opening its directory under a different
    // account id fails with JournalError::Account, which is what happened.
    let state = root.join("device-state");
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&root.join("device-journal"), &account.id, 1 << 30).unwrap(),
    ));
    let engine = Engine::new(account, cloud.clone(), state).await.unwrap();
    let watched = engine.clone();
    let session = WritableSession::mount(engine, journal, cloud.clone(), vault)
        .await
        .unwrap();
    assert!(
        watched.save_refusals.latest().is_none(),
        "a refusal recorded before the disk is full would make the assertion below meaningless"
    );

    // One save while there is still room. Without this a mount that refuses
    // everything -- a broken write path, a mount that never came up -- would
    // satisfy every assertion below for the wrong reason.
    {
        let path = mount.join("accepted-before-the-disk-filled.bin");
        tokio::task::spawn_blocking(move || std::fs::write(path, vec![b'a'; 64 * 1024]))
            .await
            .unwrap()
            .expect("a save must be possible before the device is full");
    }

    // Likewise named for this test: the sibling leaves its own ballast path
    // behind, and two tests racing one filename on one filesystem is not a
    // thing to leave to ordering.
    let ballast = root.join("device-ballast");
    let mut sink = std::fs::File::create(&ballast).unwrap();
    for chunk in [1 << 20usize, 4096, 512, 1] {
        let block = vec![0u8; chunk];
        while std::io::Write::write_all(&mut sink, &block).is_ok() {}
    }
    let _ = sink.sync_all();
    drop(sink);

    // Checked outside the filesystem code, so a refusal below cannot be what
    // persuaded us the device was full.
    let probe = root.join("device-probe");
    let refusal = std::fs::File::create(&probe).and_then(|mut file| {
        std::io::Write::write_all(&mut file, &vec![0u8; 65536])?;
        file.sync_all()
    });
    let errno = refusal.as_ref().err().and_then(|e| e.raw_os_error());
    let _ = std::fs::remove_file(&probe);
    assert_eq!(
        errno,
        Some(libc::ENOSPC),
        "the filesystem is not full, so nothing below would be attributable"
    );

    let mut accepted = 0;
    let mut refused = None;
    for index in 0..64u32 {
        let path = mount.join(format!("save-{index}.bin"));
        match tokio::task::spawn_blocking(move || std::fs::write(path, vec![b'x'; 256 * 1024]))
            .await
            .unwrap()
        {
            Ok(()) => accepted += 1,
            Err(error) => {
                refused = Some(error);
                break;
            }
        }
    }
    let refused = refused.expect("a full device must refuse a save through the mount");
    assert_eq!(
        refused.raw_os_error(),
        Some(libc::ENOSPC),
        "the kernel must still report ENOSPC, which is what an application acts on: {refused}"
    );

    let recorded = watched
        .save_refusals
        .latest()
        .expect("a refused save must leave something status can report");
    assert_eq!(
        recorded.kind, "device",
        "the disk is full and the budget is not; reporting `budget` would send a user to \
         wait for uploads that will never make room: {recorded:?}"
    );
    assert!(
        recorded.message.contains("freed"),
        "the device message must name the action that is actually required: {}",
        recorded.message
    );
    println!(
        "FULL_DEVICE_MOUNT accepted={accepted} kind={} readable_after={}",
        recorded.kind,
        std::fs::remove_file(&ballast).is_ok()
    );
    session.shutdown().await.unwrap();
}

/// A name the cloud would refuse is refused by the mount at creation, with the
/// errno a local filesystem gives for a name it cannot hold, and nothing is
/// journalled for it. Before this, such a name was accepted, uploaded, refused
/// by the provider and left as a change the daemon had given up on -- long
/// after the application that chose it had moved on.
#[tokio::test]
#[ignore = "mounts a real FUSE filesystem"]
async fn real_a_name_the_cloud_would_refuse_is_refused_at_the_mount_before_anything_is_written() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let account = account(&mount);
    let cloud = Arc::new(Cloud::default());
    namespace_fixture(&cloud);
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

    let root = mount.clone();
    let outcomes = tokio::task::spawn_blocking(move || {
        let errno = |r: std::io::Result<()>| r.err().and_then(|e| e.raw_os_error());
        (
            errno(std::fs::write(root.join("bad:name.txt"), b"x")),
            errno(std::fs::write(root.join("CON"), b"x")),
            errno(std::fs::write(root.join("x".repeat(300)), b"x")),
            errno(std::fs::create_dir(root.join("bad|dir"))),
            errno(std::fs::write(root.join("ok.txt"), b"x")),
            errno(std::fs::rename(root.join("ok.txt"), root.join("what?"))),
            root.join("ok.txt").exists(),
            root.join("what?").exists(),
            std::fs::read_dir(&root)
                .unwrap()
                .filter_map(|e| e.ok())
                .filter(|e| {
                    let n = e.file_name();
                    let n = n.to_string_lossy();
                    n.contains(':') || n.contains('|') || n == "CON" || n.len() > 255
                })
                .count(),
        )
    })
    .await
    .unwrap();
    assert_eq!(
        outcomes.0,
        Some(libc::EINVAL),
        "a colon is refused at creation"
    );
    assert_eq!(
        outcomes.1,
        Some(libc::EINVAL),
        "a device name is refused at creation"
    );
    assert_eq!(outcomes.2, Some(libc::ENAMETOOLONG), "a limit is a limit");
    assert_eq!(
        outcomes.3,
        Some(libc::EINVAL),
        "a folder name is held to the same rules"
    );
    assert_eq!(outcomes.4, None, "an ordinary name is taken");
    assert_eq!(
        outcomes.5,
        Some(libc::EINVAL),
        "a rename to a refused name is refused"
    );
    assert!(
        outcomes.6,
        "the refused rename leaves the file where it was"
    );
    assert!(!outcomes.7);
    assert_eq!(
        outcomes.8, 0,
        "nothing with a refused name exists in the listing"
    );
    session.shutdown().await.unwrap();
}

/// A OneNote notebook is a folder to Graph and one thing to a person, and
/// beneath it are section files that only OneNote knows how to write. Showing
/// the contents is right -- a person should be able to see and copy them. A
/// filesystem that lets an ordinary text editor save over one is offering to
/// corrupt a notebook, so the mount reads and refuses to change.
///
/// Refused at the mount rather than after journalling: the provider would
/// refuse it anyway, and a change that gets that far ends up stuck with the
/// mount already showing it as done, which is the failure mode this project
/// has already met once with fourteen folder removals.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE"]
async fn real_a_package_is_readable_and_refuses_every_change_inside_it() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let state = temp.path().join("state");
    let account = account(&mount);
    let cloud = Arc::new(Cloud::default());
    let vault = Arc::new(Vault::default());

    // A notebook, a section group inside it, and a section file inside that --
    // the shape that matters, because a section's immediate parent is the group
    // and not the package.
    {
        let mut remote = cloud.remote.lock().unwrap();
        let mut node = |id: &str, parent: &str, name: &str, kind, package| {
            remote.files.insert(
                id.to_owned(),
                (
                    Node {
                        package,
                        id: id.into(),
                        parent_id: Some(parent.into()),
                        name: name.into(),
                        kind,
                        size: 0,
                        modified_unix: 1,
                        etag: Some(format!("{id}-etag")),
                        content_version: None,
                        target: None,
                    },
                    Vec::new(),
                ),
            );
        };
        node("notebook", "root", "Team notes", NodeKind::Folder, true);
        node("group", "notebook", "Meetings", NodeKind::Folder, false);
        node("section", "group", "September.one", NodeKind::File, false);
    }

    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&temp.path().join("journal"), &account.id, 1024 * 1024).unwrap(),
    ));
    let engine = Engine::new(account.clone(), cloud.clone(), state.clone())
        .await
        .unwrap();
    let session = WritableSession::mount(engine, journal, cloud.clone(), vault)
        .await
        .unwrap();

    let notebook = mount.join("Team notes");
    let group = notebook.join("Meetings");
    let readable = tokio::task::spawn_blocking({
        let group = group.clone();
        move || {
            // Reading is the half that must keep working.
            let names: Vec<String> = std::fs::read_dir(&group)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names
        }
    })
    .await
    .unwrap();
    assert_eq!(
        readable,
        vec!["September.one".to_owned()],
        "a package's contents must still be listed"
    );

    let refusals = tokio::task::spawn_blocking({
        let notebook = notebook.clone();
        let group = group.clone();
        move || {
            let section = group.join("September.one");
            vec![
                (
                    "mkdir in the package",
                    std::fs::create_dir(notebook.join("New section")).err(),
                ),
                (
                    "mkdir below the package",
                    std::fs::create_dir(group.join("Deeper")).err(),
                ),
                (
                    "create a file below it",
                    std::fs::write(group.join("notes.txt"), b"x").err(),
                ),
                ("overwrite a section", std::fs::write(&section, b"x").err()),
                (
                    "rename a section",
                    std::fs::rename(&section, group.join("Other.one")).err(),
                ),
                ("delete a section", std::fs::remove_file(&section).err()),
                ("remove the package", std::fs::remove_dir(&notebook).err()),
            ]
        }
    })
    .await
    .unwrap();

    for (what, error) in refusals {
        let error = error.unwrap_or_else(|| panic!("{what} was allowed inside a package"));
        assert_eq!(
            error.raw_os_error(),
            Some(libc::EOPNOTSUPP),
            "{what} should be refused as unsupported, got {error}"
        );
    }
    session.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; keep both exposes cloud and local copies independently"]
async fn real_keep_both_restores_the_remote_path_and_exposes_the_copy_before_upload() {
    keep_both_mount(false, false, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; autosave conflict rescue retains newest content"]
async fn real_keep_both_rescues_multiple_autosaves_without_replaying_them() {
    keep_both_mount(true, false, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; atomic-save conflict rescue preserves both identities"]
async fn real_keep_both_rescues_atomic_save_and_delays_temporary_cleanup() {
    keep_both_mount(false, true, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; chained atomic conflict rescue preserves each stream"]
async fn real_keep_both_rescues_chained_atomic_saves_without_early_cleanup() {
    keep_both_mount(false, true, true).await;
}

async fn keep_both_mount(autosave: bool, atomic: bool, chain: bool) {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let account = account(&mount);
    let cloud = Arc::new(Cloud::default());
    replacement_fixture(&cloud);
    cloud.pause_once.store(!atomic, Ordering::SeqCst);
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&temp.path().join("journal"), &account.id, 1024 * 1024).unwrap(),
    ));
    let engine = Engine::new(account.clone(), cloud.clone(), temp.path().join("state"))
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
    let old_descriptor = if atomic {
        application(
            &mount,
            "import pathlib,sys; pathlib.Path(sys.argv[1], 'source.txt').write_bytes(b'local')",
        )
        .await;
        acknowledged(&session, 1).await;
        let old = tokio::task::spawn_blocking({
            let path = mount.join("document.txt");
            move || std::fs::File::open(path).unwrap()
        })
        .await
        .unwrap();
        cloud.pause_once.store(true, Ordering::SeqCst);
        application(
            &mount,
            "import os,sys; os.chdir(sys.argv[1]); os.replace('source.txt','document.txt')",
        )
        .await;
        Some(old)
    } else {
        application(
            &mount,
            "import pathlib,sys; pathlib.Path(sys.argv[1], 'document.txt').write_bytes(b'local')",
        )
        .await;
        None
    };
    tokio::time::timeout(Duration::from_secs(5), cloud.entered.notified())
        .await
        .unwrap();
    {
        let mut remote = cloud.remote.lock().unwrap();
        let (node, bytes) = remote.files.get_mut("target").unwrap();
        *bytes = b"remote".to_vec();
        node.size = bytes.len() as u64;
        node.etag = Some("competing-edit".into());
        node.content_version = node.etag.clone();
    }
    cloud.release.notify_one();
    let refused = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let rows = session.uploads(0, 10).await.unwrap();
            if let Some(row) = rows.into_iter().find(|r| r.state == UploadState::Conflict) {
                break row;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let mut second_temporary = None;
    let intermediate = if chain {
        let held = tokio::task::spawn_blocking({
            let path = mount.join("document.txt");
            move || std::fs::File::open(path).unwrap()
        })
        .await
        .unwrap();
        application(
            &mount,
            "import pathlib,sys; pathlib.Path(sys.argv[1], 'second.txt').write_bytes(b'latest')",
        )
        .await;
        second_temporary = Some(tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let rows = session.uploads(0,10).await.unwrap();
                if let Some(row) = rows.into_iter().find(|r|matches!(&r.intent, UploadIntent::Create{name,..} if name=="second.txt")) {
                    assert!(!matches!(row.state,UploadState::Failed | UploadState::Conflict));
                    if row.state == UploadState::Uploaded { break row.remote.unwrap().id; }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.unwrap());
        application(
            &mount,
            "import os,sys; os.chdir(sys.argv[1]); os.replace('second.txt','document.txt')",
        )
        .await;
        Some(held)
    } else {
        None
    };
    let expected: &[u8] = if chain {
        b"latest"
    } else if autosave {
        b"local-3"
    } else {
        b"local"
    };
    if autosave {
        application(&mount, "import pathlib,sys; p=pathlib.Path(sys.argv[1], 'document.txt'); p.write_bytes(b'local-2'); p.write_bytes(b'local-3')").await;
    }
    let held = tokio::task::spawn_blocking({
        let path = mount.join("document.txt");
        move || std::fs::File::open(path).unwrap()
    })
    .await
    .unwrap();
    refresh_fixture(&engine, &cloud).await;
    cloud.pause_once.store(true, Ordering::SeqCst);
    assert_eq!(
        session
            .keep_both(vec![(
                refused.id,
                "root".into(),
                "document-copy.txt".into()
            )])
            .await
            .unwrap(),
        1
    );
    tokio::time::timeout(Duration::from_secs(5), cloud.entered.notified())
        .await
        .unwrap();
    // Both names must work even while the rescue upload cannot finish.
    application(
        &mount,
        &format!(
            r#"import pathlib,sys,time
p=pathlib.Path(sys.argv[1])
assert (p/'document-copy.txt').read_bytes()==b'{expected}'
deadline=time.monotonic()+5
while True:
    data=(p/'document.txt').read_bytes()
    if data==b'remote': break
    assert time.monotonic()<deadline, repr(data)
    time.sleep(.02)
"#,
            expected = std::str::from_utf8(expected).unwrap()
        ),
    )
    .await;
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut held = held;
        let mut bytes = Vec::new();
        held.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, expected, "an open descriptor keeps the rescued edit");
    })
    .await
    .unwrap();
    if let Some(mut intermediate) = intermediate {
        tokio::task::spawn_blocking(move || {
            use std::io::Read;
            let mut bytes = Vec::new();
            intermediate.read_to_end(&mut bytes).unwrap();
            assert_eq!(bytes, b"local");
        })
        .await
        .unwrap();
    }
    if let Some(mut old) = old_descriptor {
        tokio::task::spawn_blocking(move || {
            use std::io::Read;
            let mut bytes = Vec::new();
            old.read_to_end(&mut bytes).unwrap();
            assert_eq!(
                bytes, b"old",
                "the replaced descriptor retains its original stream"
            );
        })
        .await
        .unwrap();
        assert!(cloud.remote.lock().unwrap().files.contains_key("source"));
        assert!(cloud.remote.lock().unwrap().deletes.is_empty());
    }
    cloud.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if session
                .uploads(0, 10)
                .await
                .unwrap()
                .iter()
                .any(|r| matches!(&r.intent, UploadIntent::Create { name, .. } if name == "document-copy.txt") && r.state == UploadState::Uploaded)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        journal.lock().unwrap().get(refused.id).unwrap().state,
        UploadState::Resolved
    );
    assert_eq!(
        cloud.remote.lock().unwrap().files.get("target").unwrap().1,
        b"remote"
    );
    if atomic {
        mutations_applied(&session, if chain { 2 } else { 1 }).await;
        let remote = cloud.remote.lock().unwrap();
        assert!(!remote.files.contains_key("source"));
        let mut expected = vec!["source".to_owned()];
        if let Some(second) = second_temporary {
            assert!(!remote.files.contains_key(&second));
            expected.push(second);
        }
        expected.sort();
        let mut actual: Vec<_> = remote
            .deletes
            .iter()
            .map(|r| r.intent.before().unwrap().id.clone())
            .collect();
        actual.sort();
        assert_eq!(actual, expected);
    }
    session.shutdown().await.unwrap();
    drop(engine);
    let engine = Engine::new(account, cloud.clone(), temp.path().join("state"))
        .await
        .unwrap();
    let session = WritableSession::mount(engine, journal, cloud, Arc::new(Vault::default()))
        .await
        .unwrap();
    application(&mount, &format!("import pathlib,sys; p=pathlib.Path(sys.argv[1]); assert (p/'document-copy.txt').read_bytes()==b'{expected}'; assert (p/'document.txt').read_bytes()==b'remote'",expected=std::str::from_utf8(expected).unwrap())).await;
    session.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires /dev/fuse; synthetic account only"]
async fn real_recovery_export_uses_the_service_without_replaying_a_failed_save() {
    use cirrove_service::{
        accounts::Settings,
        manager::{Manager, ProviderFactory, WriteFactory},
    };
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let state = temp.path().join("state");
    cirrove_service::private_dir(&state).unwrap();
    let mut config = account(&mount);
    config.enabled = true;
    config.label = "export-fixture".into();
    config.access = cirrove_auth::AccessMode::ReadWrite;
    config.cache_bytes = 64 * 1024 * 1024;
    std::fs::write(
        state.join("accounts.json"),
        serde_json::to_vec(&Settings {
            version: 2,
            accounts: vec![config],
        })
        .unwrap(),
    )
    .unwrap();
    let cloud = Arc::new(Cloud::default());
    let reads = cloud.clone();
    let writes = cloud.clone();
    let read_factory: ProviderFactory = Arc::new(move |_| Ok(reads.clone()));
    let captured = Arc::new(Mutex::new(None));
    let captured_by_factory = captured.clone();
    let write_factory: WriteFactory = Arc::new(move |account, context| {
        let journal = context.journal();
        let mut journal = journal.lock().unwrap();
        let row = journal.enqueue(
            Scope {
                account: account.id.clone(),
                provider: "fixture".into(),
                collection: "home".into(),
            },
            UploadIntent::Create {
                parent: "root".into(),
                name: "Saved.txt".into(),
            },
            b"recover my saved bytes".as_slice(),
        )?;
        let attempt = journal.claim_next()?.unwrap();
        journal.stop_attempt(row.id, attempt.attempt.unwrap(), UploadState::Conflict)?;
        *captured_by_factory.lock().unwrap() = Some((row.id, context.journal()));
        Ok(writes.clone())
    });
    let cancel = CancellationToken::new();
    let (manager, worker) = Manager::start_with_providers(
        state.clone(),
        cancel.clone(),
        read_factory,
        Some(write_factory),
    );
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if manager.status.read().await.iter().any(|a| a.mounted) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let id = captured.lock().unwrap().as_ref().unwrap().0;
    let socket = temp.path().join("runtime/control.sock");
    let server_socket = socket.clone();
    let server_cancel = cancel.clone();
    let server_manager = manager.clone();
    let db = state.join("status.sqlite");
    let server = tokio::spawn(async move {
        cirrove_service::serve_managed(db, server_socket, server_cancel, Some(server_manager)).await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !socket.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let destination = temp.path().join("Recovered.txt");
    let reply = cirrove_service::export_save(
        &socket,
        &cirrove_service::ExportSaveRequest {
            label: "export-fixture".into(),
            operation: id,
            destination: destination.clone(),
        },
    )
    .await
    .unwrap();
    assert!(reply.refusal.is_none(), "{:?}", reply.refusal);
    let job = reply.job.unwrap();
    let complete = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = cirrove_service::status(&socket).await.unwrap();
            if let Some(job) = status
                .accounts
                .iter()
                .flat_map(|a| &a.jobs)
                .find(|j| j.id == job.id && !j.running())
            {
                break job.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(complete.state, cirrove_service::jobs::JobState::Succeeded);
    assert_eq!(complete.export.unwrap().operation, id);
    assert_eq!(
        std::fs::read(&destination).unwrap(),
        b"recover my saved bytes"
    );
    let recent = cirrove_service::recent(
        &socket,
        &cirrove_service::RecentRequest {
            label: "export-fixture".into(),
            limit: 5,
        },
    )
    .await
    .unwrap();
    assert_eq!(recent.local[0].operation, Some(id));
    assert_eq!(recent.local[0].state, "conflict");
    let cli_target = temp.path().join("CLI copy.txt");
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new(env!("CARGO_BIN_EXE_cirrove"))
            .arg("export-save")
            .arg("--label")
            .arg("export-fixture")
            .arg("--operation")
            .arg(id.to_string())
            .arg("--destination")
            .arg(&cli_target)
            .arg("--socket")
            .arg(&socket)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read(cli_target).unwrap(),
        b"recover my saved bytes"
    );
    let refused = cirrove_service::export_save(
        &socket,
        &cirrove_service::ExportSaveRequest {
            label: "export-fixture".into(),
            operation: id,
            destination: mount.join("not-an-export"),
        },
    )
    .await
    .unwrap();
    assert!(refused.refusal.is_some());
    assert!(!mount.join("not-an-export").exists());
    let source = captured
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .1
        .lock()
        .unwrap()
        .local_export_source(id)
        .unwrap();
    assert!(
        source
            .copy_to(
                &mount.join("direct-export"),
                &CancellationToken::new(),
                |_| {}
            )
            .is_err()
    );
    assert!(!mount.join("direct-export").exists());
    assert!(
        cloud.remote.lock().unwrap().files.is_empty(),
        "export must never publish cloud content"
    );
    // Keep an application descriptor open, with written but unsealed FUSE bytes.
    // Recovery must not fsync/release that descriptor or trigger an upload.
    let ready = temp.path().join("working-ready");
    let release = temp.path().join("working-release");
    let mut editor = tokio::process::Command::new("python3")
        .args([
            "-c",
            r#"import pathlib,sys,time
p,ready,release=map(pathlib.Path,sys.argv[1:])
with p.open('wb',buffering=0) as f:
    f.write(b'active unsaved bytes')
    ready.touch()
    end=time.monotonic()+30
    while not release.exists():
        if time.monotonic()>end: raise RuntimeError('test release deadline')
        time.sleep(.02)
"#,
        ])
        .arg(mount.join("Working.txt"))
        .arg(&ready)
        .arg(&release)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !ready.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let listed = cirrove_service::recovery_working(
        &socket,
        &cirrove_service::RecoveryWorkingRequest {
            label: "export-fixture".into(),
            after: None,
            limit: 200,
        },
    )
    .await
    .unwrap();
    assert!(listed.refusal.is_none());
    let working = listed
        .files
        .iter()
        .find(|file| file.name == "Working.txt")
        .unwrap()
        .clone();
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new(env!("CARGO_BIN_EXE_cirrove"))
            .args([
                "recovery-working",
                "--active",
                "--label",
                "export-fixture",
                "--socket",
            ])
            .arg(&socket)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let listing: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        listing["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|file| file["file"] == working.file.to_string())
    );
    let journal = captured.lock().unwrap().as_ref().unwrap().1.clone();
    let before =
        serde_json::to_value(journal.lock().unwrap().working_file(working.file).unwrap()).unwrap();
    let uploads_before =
        serde_json::to_value(journal.lock().unwrap().list(0, 200).unwrap()).unwrap();
    let target = temp.path().join("working-copy");
    let request = cirrove_service::ExportWorkingRequest {
        label: "export-fixture".into(),
        file: working.file,
        generation: working.generation,
        destination: target.clone(),
    };
    let initial = cirrove_service::export_working(&socket, &request)
        .await
        .unwrap()
        .job
        .unwrap();
    let complete = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = cirrove_service::status(&socket).await.unwrap();
            if let Some(job) = status
                .accounts
                .iter()
                .flat_map(|a| &a.jobs)
                .find(|job| job.id == initial.id && !job.running())
            {
                break job.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(request.confirmed_receipt(&initial, &complete).is_some());
    assert_eq!(std::fs::read(&target).unwrap(), b"active unsaved bytes");
    let cli_target = temp.path().join("working-cli-copy");
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new(env!("CARGO_BIN_EXE_cirrove"))
            .args(["export-working", "--active", "--label", "", "--socket"])
            .arg(&socket)
            .arg("--file")
            .arg(working.file.to_string())
            .arg("--generation")
            .arg(working.generation.to_string())
            .arg("--destination")
            .arg(&cli_target)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt: cirrove_service::journal::WorkingExportReceipt =
        serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(receipt.source.file, working.file);
    assert_eq!(receipt.source.generation, working.generation);
    assert_eq!(std::fs::read(cli_target).unwrap(), b"active unsaved bytes");
    let mut stale = request.clone();
    stale.generation += 1;
    stale.destination = temp.path().join("stale-copy");
    assert!(
        cirrove_service::export_working(&socket, &stale)
            .await
            .unwrap()
            .refusal
            .is_some()
    );
    assert!(!stale.destination.exists());
    stale.generation = request.generation;
    stale.destination = mount.join("unsafe-working-export");
    assert!(
        cirrove_service::export_working(&socket, &stale)
            .await
            .unwrap()
            .refusal
            .is_some()
    );
    assert!(!stale.destination.exists());
    assert_eq!(
        serde_json::to_value(journal.lock().unwrap().working_file(working.file).unwrap()).unwrap(),
        before
    );
    assert_eq!(
        serde_json::to_value(journal.lock().unwrap().list(0, 200).unwrap()).unwrap(),
        uploads_before
    );
    assert!(cloud.remote.lock().unwrap().files.is_empty());
    // The mounted source remains readable while the editor still owns its stream.
    application(&mount, "import pathlib,sys; assert (pathlib.Path(sys.argv[1])/'Working.txt').read_bytes()==b'active unsaved bytes'").await;
    std::fs::write(&release, b"release").unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), editor.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    cancel.cancel();
    worker.await.unwrap();
    server.await.unwrap().unwrap();
}

/// A permission downgrade must keep local recovery available without reopening
/// mutation workers. Working bytes here are written through the actual kernel
/// mount, then unlinked while open so shutdown cannot seal them for upload.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires /dev/fuse; synthetic RW-to-RO manager/socket lifecycle only"]
async fn real_manager_readonly_downgrade_exports_retained_saved_and_dirty_bytes()
-> anyhow::Result<()> {
    manager_readonly_downgrade_exports_retained_bytes(false).await
}

/// Exercise actual settings reload and manager ownership with iCloud identity.
/// Providers remain synthetic: this does not exercise Apple transport or login.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires /dev/fuse and the ordinary iCloud write settings gate"]
async fn real_manager_icloud_readonly_write_readonly_retains_recovery() -> anyhow::Result<()> {
    manager_readonly_downgrade_exports_retained_bytes(true).await
}

async fn manager_readonly_downgrade_exports_retained_bytes(icloud: bool) -> anyhow::Result<()> {
    use cirrove_service::{
        accounts::Settings,
        manager::{Manager, ProviderFactory, WriteFactory},
    };
    let temp = tempfile::tempdir()?;
    // Preserve this fixture on any failure; never recursively remove a mount.
    let root = temp.keep();
    let mount = root.join("mount");
    std::fs::create_dir(&mount)?;
    let state = root.join("state");
    cirrove_service::private_dir(&state)?;
    let mut config = account(&mount);
    config.enabled = true;
    config.access = if icloud {
        AccessMode::ReadOnly
    } else {
        AccessMode::ReadWrite
    };
    if icloud {
        config.registration = AppRegistration::ICloud;
        config.identity.tenant_id.clear();
        config.identity.graph_user_id.clear();
        config.drive.drive_type = "icloud_drive".into();
        config.root_id = "FOLDER::com.apple.CloudDocs::root".into();
    }
    config.label = "downgrade-fixture".into();
    config.cache_bytes = 64 * 1024 * 1024;
    let save_settings = |config: &Account| -> anyhow::Result<()> {
        let next = state.join("accounts.next.json");
        let settings = Settings {
            version: 2,
            accounts: vec![config.clone()],
        };
        settings.validate()?;
        std::fs::write(&next, serde_json::to_vec(&settings)?)?;
        std::fs::rename(next, state.join("accounts.json"))?;
        Ok(())
    };
    save_settings(&config)?;
    let cloud = Arc::new(Cloud::default());
    cloud.stall.store(true, Ordering::SeqCst);
    cloud.icloud_identity.store(icloud, Ordering::SeqCst);
    let reads = cloud.clone();
    let writes = cloud.clone();
    let read_factory: ProviderFactory = Arc::new(move |_| Ok(reads.clone()));
    let factory_calls = Arc::new(AtomicUsize::new(0));
    let calls = factory_calls.clone();
    let captured = Arc::new(Mutex::new(None));
    let capture = captured.clone();
    let write_factory: WriteFactory = Arc::new(move |account, context| {
        anyhow::ensure!(
            account.access == AccessMode::ReadWrite,
            "RO invoked write factory"
        );
        anyhow::ensure!(matches!(account.registration, AppRegistration::ICloud) == icloud);
        calls.fetch_add(1, Ordering::SeqCst);
        let journal = context.journal();
        let mut journal = journal.lock().unwrap();
        let record = journal.enqueue(
            Scope {
                account: account.id.clone(),
                provider: writes.provider_id().into(),
                collection: account.drive.id.clone(),
            },
            UploadIntent::Create {
                parent: account.root_id.clone(),
                name: "Saved.txt".into(),
            },
            &b"sealed before downgrade"[..],
        )?;
        let attempt = journal.claim_next()?.unwrap();
        journal.stop_attempt(record.id, attempt.attempt.unwrap(), UploadState::Conflict)?;
        *capture.lock().unwrap() = Some((record.id, record.sha256, context.journal()));
        Ok(writes.clone())
    });
    let cancel = CancellationToken::new();
    let (manager, worker) = Manager::start_with_providers(
        state.clone(),
        cancel.clone(),
        read_factory,
        Some(write_factory),
    );
    let socket = root.join("runtime/control.sock");
    let server_socket = socket.clone();
    let server_cancel = cancel.clone();
    let server_manager = manager.clone();
    let db = state.join("status.sqlite");
    let server = tokio::spawn(async move {
        cirrove_service::serve_managed(db, server_socket, server_cancel, Some(server_manager)).await
    });
    let outcome: anyhow::Result<()> = async {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if socket.exists() && manager.status.read().await.iter().any(|row| row.mounted) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await?;
        if icloud {
            let initial = manager.engine(&config.label).await?;
            anyhow::ensure!(initial.account.access == AccessMode::ReadOnly);
            anyhow::ensure!(initial.provider.provider_id() == "icloud");
            let initial_engine = Arc::downgrade(&initial);
            drop(initial);
            anyhow::ensure!(factory_calls.load(Ordering::SeqCst) == 0);
            anyhow::ensure!(cloud.write_calls.load(Ordering::SeqCst) == 0);
            let path = mount.join("initial-ro-must-not-write.txt");
            let error = tokio::task::spawn_blocking(move || std::fs::write(path, b"forbidden"))
                .await?
                .expect_err("initial RO mount accepted a write");
            anyhow::ensure!(error.raw_os_error() == Some(libc::EROFS));
            config.access = AccessMode::ReadWrite;
            save_settings(&config)?;
            tokio::time::timeout(Duration::from_secs(30), async {
                loop {
                    if let Ok(engine) = manager.engine(&config.label).await
                        && engine.account.access == AccessMode::ReadWrite
                        && manager.status.read().await.iter().any(|row| row.mounted)
                        && initial_engine.upgrade().is_none()
                        && factory_calls.load(Ordering::SeqCst) == 1
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await?;
        }
        let old_engine = Arc::downgrade(&manager.engine(&config.label).await?);
        // No fsync before unlink: no sealed copy can replace the dirty generation.
        let path = mount.join("Open then removed.txt");
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)?;
            file.write_all(b"initial")?;
            std::fs::remove_file(&path)?;
            file.set_len(0)?;
            file.seek(SeekFrom::Start(0))?;
            file.write_all(b"dirty retained after unlink")?;
            drop(file);
            Ok(())
        })
        .await??;
        let (saved, saved_hash, journal) = captured.lock().unwrap().take().unwrap();
        let dirty = journal
            .lock()
            .unwrap()
            .working_files()?
            .into_iter()
            .find(|file| file.unlinked && file.dirty)
            .ok_or_else(|| anyhow::anyhow!("kernel unlink did not retain dirty working bytes"))?;
        anyhow::ensure!(
            journal.lock().unwrap().read_working(dirty.id, 0, 128)?
                == b"dirty retained after unlink"
        );
        drop(journal); // The retiring RW owner must be able to close.
        config.access = AccessMode::ReadOnly;
        save_settings(&config)?;
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if let Ok(engine) = manager.engine(&config.label).await
                    && engine.account.access == AccessMode::ReadOnly
                    && manager
                        .status
                        .read()
                        .await
                        .iter()
                        .any(|row| row.mounted && row.local_recovery)
                    && old_engine.upgrade().is_none()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await?;
        let calls_after_downgrade = cloud.write_calls.load(Ordering::SeqCst);
        anyhow::ensure!(
            factory_calls.load(Ordering::SeqCst) == 1,
            "downgrade reopened a write factory"
        );
        let path = mount.join("must-not-write.txt");
        let error = tokio::task::spawn_blocking(move || std::fs::write(path, b"forbidden"))
            .await?
            .expect_err("RO mount accepted a write");
        anyhow::ensure!(
            error.raw_os_error() == Some(libc::EROFS),
            "wrong readonly write refusal: {error}"
        );
        let journal_root = state.join("accounts").join(&config.id).join("journal");
        let before_db = std::fs::read(journal_root.join("uploads.db"))?;
        let listed = cirrove_service::recovery_working(
            &socket,
            &cirrove_service::RecoveryWorkingRequest {
                label: config.label.clone(),
                after: None,
                limit: 200,
            },
        )
        .await?;
        anyhow::ensure!(
            listed.refusal.is_none(),
            "RO list refused: {:?}",
            listed.refusal
        );
        let retained = listed
            .files
            .iter()
            .find(|file| file.file == dirty.id)
            .ok_or_else(|| anyhow::anyhow!("dirty generation disappeared"))?;
        anyhow::ensure!(retained.generation == dirty.generation);
        let wait_job = |id: String| {
            let socket = socket.clone();
            async move {
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        let status = cirrove_service::status(&socket).await?;
                        if let Some(job) = status
                            .accounts
                            .iter()
                            .flat_map(|row| &row.jobs)
                            .find(|job| job.id == id && !job.running())
                        {
                            return Ok::<_, anyhow::Error>(job.clone());
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await?
            }
        };
        let saved_destination = root.join("sealed-export.txt");
        let saved_reply = cirrove_service::export_save(
            &socket,
            &cirrove_service::ExportSaveRequest {
                label: config.label.clone(),
                operation: saved,
                destination: saved_destination.clone(),
            },
        )
        .await?;
        let saved_job = saved_reply
            .job
            .ok_or_else(|| anyhow::anyhow!("RO saved export refused: {:?}", saved_reply.refusal))?;
        let complete = wait_job(saved_job.id).await?;
        let receipt = complete
            .export
            .ok_or_else(|| anyhow::anyhow!("missing sealed receipt"))?;
        anyhow::ensure!(receipt.operation == saved && receipt.sha256 == saved_hash);
        anyhow::ensure!(std::fs::read(saved_destination)? == b"sealed before downgrade");
        let request = cirrove_service::ExportWorkingRequest {
            label: config.label.clone(),
            file: dirty.id,
            generation: dirty.generation,
            destination: root.join("dirty-export.txt"),
        };
        let working_reply = cirrove_service::export_working(&socket, &request).await?;
        let initial = working_reply.job.ok_or_else(|| {
            anyhow::anyhow!("RO working export refused: {:?}", working_reply.refusal)
        })?;
        let complete = wait_job(initial.id.clone()).await?;
        let receipt = request
            .confirmed_receipt(&initial, &complete)
            .ok_or_else(|| anyhow::anyhow!("missing exact dirty receipt"))?;
        anyhow::ensure!(
            receipt.sha256 == hex::encode(Sha256::digest(b"dirty retained after unlink"))
        );
        anyhow::ensure!(std::fs::read(&request.destination)? == b"dirty retained after unlink");
        anyhow::ensure!(manager.retry_stuck(&config.label).await.is_err());
        anyhow::ensure!(manager.discard_stuck(&config.label).await.is_err());
        // Cross a manager refresh as well as the export completion boundaries.
        tokio::time::sleep(Duration::from_secs(6)).await;
        anyhow::ensure!(
            cloud.write_calls.load(Ordering::SeqCst) == calls_after_downgrade,
            "RO account replayed provider work"
        );
        anyhow::ensure!(factory_calls.load(Ordering::SeqCst) == 1);
        anyhow::ensure!(
            std::fs::read(journal_root.join("uploads.db"))? == before_db,
            "RO recovery changed the journal"
        );
        anyhow::ensure!(cloud.remote.lock().unwrap().history.is_empty());
        Ok(())
    }
    .await;
    cancel.cancel();
    let stopped = tokio::time::timeout(Duration::from_secs(20), worker).await;
    let served = tokio::time::timeout(Duration::from_secs(5), server).await;
    stopped??;
    served???;
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; selected representation admission precedes local edits"]
async fn real_unknown_package_admission_refuses_namespace_and_content_before_journalling() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let account = account(&mount);
    let cloud = Arc::new(Cloud::default());
    replacement_fixture(&cloud);
    // Metadata deliberately says ordinary file, with an unrecognized extension.
    cloud
        .remote
        .lock()
        .unwrap()
        .files
        .get_mut("target")
        .unwrap()
        .0
        .name = "bundle.unknown-native".into();
    let mut folder = root();
    folder.id = "folder".into();
    folder.parent_id = Some("root".into());
    folder.name = "folder".into();
    cloud
        .remote
        .lock()
        .unwrap()
        .files
        .insert("folder".into(), (folder, vec![]));
    *cloud.refused_write_target.lock().unwrap() = Some("target".into());
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
import os,sys,errno
os.chdir(sys.argv[1])
p='bundle.unknown-native'
def refused(action):
    try:
        result=action()
    except OSError as e:
        assert e.errno == errno.EACCES, e
    else:
        if isinstance(result,int): os.close(result)
        raise AssertionError('unclassified package edit accepted')
for action in [
    lambda: os.open(p,os.O_WRONLY),
    lambda: os.open(p,os.O_WRONLY|os.O_TRUNC),
    lambda: os.truncate(p,0),
    lambda: os.unlink(p),
    lambda: os.rename(p,'renamed'),
    lambda: os.rename(p,'folder/moved'),
    lambda: os.replace('source.txt',p),
    lambda: os.replace(p,'source.txt'),
]:
    refused(action)
    assert sorted(os.listdir('.')) == ['bundle.unknown-native','folder','source.txt']
    assert os.stat(p).st_size == 3
    assert os.stat('source.txt').st_size == 3
"#,
    )
    .await;
    {
        let j = journal.lock().unwrap();
        assert!(j.list(0, 64).unwrap().is_empty());
        assert!(j.list_mutations(0, 64).unwrap().is_empty());
        assert!(j.working_files().unwrap().is_empty());
    }
    assert_eq!(
        cloud.reads.load(Ordering::SeqCst),
        0,
        "admission must not download content"
    );
    assert!(cloud.remote.lock().unwrap().moves.is_empty());
    assert!(cloud.remote.lock().unwrap().deletes.is_empty());
    // Ordinary metadata remains editable; then its local alias must not bypass
    // a subsequent representation refusal for the canonical provider identity.
    application(&mount, "import os,sys; os.rename(os.path.join(sys.argv[1],'source.txt'),os.path.join(sys.argv[1],'alias.txt'))").await;
    mutations_applied(&session, 1).await;
    *cloud.refused_write_target.lock().unwrap() = Some("source".into());
    cloud.admission_targets.lock().unwrap().clear();
    application(
        &mount,
        r#"
import os,sys,errno
p=os.path.join(sys.argv[1],'alias.txt')
try: os.unlink(p)
except OSError as e: assert e.errno==errno.EACCES
else: raise AssertionError('local alias bypassed remote admission')
assert os.stat(p).st_size==3
"#,
    )
    .await;
    assert!(
        cloud
            .admission_targets
            .lock()
            .unwrap()
            .iter()
            .any(|id| id == "source")
    );
    assert_eq!(
        journal.lock().unwrap().list_mutations(0, 64).unwrap().len(),
        1
    );
    *cloud.refused_write_target.lock().unwrap() = None;
    cloud.hold_admission.store(true, Ordering::SeqCst);
    let raced_path = mount.join("alias.txt");
    let pending = tokio::task::spawn_blocking(move || {
        std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(raced_path)
    });
    tokio::time::timeout(Duration::from_secs(5), cloud.admission_entered.notified())
        .await
        .unwrap();
    {
        let mut j = journal.lock().unwrap();
        let row = j.list_mutations(0, 64).unwrap().remove(0);
        let object = j
            .namespace_by_remote(&row.request.scope, "source")
            .unwrap()
            .unwrap();
        // Reactivate an idle follows-remote alias through the same journal API
        // used by foreground materialization before constructing its successor.
        let object = j
            .observe_namespace_file(row.request.scope.clone(), object.remote.clone().unwrap())
            .unwrap();
        j.relocate_namespace_item(
            object.id,
            object.revision,
            "root".into(),
            "raced.txt".into(),
        )
        .unwrap();
    }
    cloud.admission_release.notify_one();
    let failure = tokio::time::timeout(Duration::from_secs(5), pending)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(
        failure.raw_os_error(),
        Some(libc::ESTALE),
        "binding changed while metadata validation awaited"
    );
    assert!(journal.lock().unwrap().working_files().unwrap().is_empty());
    session.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE; admission races with an existing working stream"]
async fn real_existing_working_admission_rechecks_open_and_path_truncate_atomically() {
    for truncate_path in [false, true] {
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
        let source = mount.join("source.txt");
        let old = tokio::task::spawn_blocking(move || {
            std::fs::OpenOptions::new()
                .write(true)
                .open(source)
                .unwrap()
        })
        .await
        .unwrap();
        let working = journal.lock().unwrap().working_files().unwrap();
        assert_eq!(working.len(), 1);
        assert_eq!(working[0].node.size, 3);
        let working_id = working[0].id;
        cloud.hold_admission.store(true, Ordering::SeqCst);
        let path = mount.clone();
        let pending = tokio::spawn(async move {
            application(
                &path,
                if truncate_path {
                    r#"
import os,sys,errno
try: os.truncate(os.path.join(sys.argv[1],'source.txt'),1)
# Linux may retry ESTALE by resolving the old pathname, which the deliberate
# concurrent rename removed. Both outcomes refuse the stale operation.
except OSError as e: assert e.errno in (errno.ESTALE,errno.ENOENT), e
else: raise AssertionError('stale pathname truncate accepted')
"#
                } else {
                    r#"
import os,sys,errno
try: fd=os.open(os.path.join(sys.argv[1],'source.txt'),os.O_WRONLY)
# Linux may retry ESTALE by resolving the old pathname, which the deliberate
# concurrent rename removed. Both outcomes refuse the stale operation.
except OSError as e: assert e.errno in (errno.ESTALE,errno.ENOENT), e
else:
    os.close(fd)
    raise AssertionError('stale existing-working open accepted')
"#
                },
            )
            .await;
        });
        tokio::time::timeout(Duration::from_secs(5), cloud.admission_entered.notified())
            .await
            .unwrap();
        {
            let mut j = journal.lock().unwrap();
            let object = j
                .namespace_objects()
                .unwrap()
                .into_iter()
                .find(|o| o.remote.as_ref().is_some_and(|n| n.id == "source"))
                .unwrap();
            j.relocate_namespace_item(
                object.id,
                object.revision,
                "root".into(),
                "raced.txt".into(),
            )
            .unwrap();
        }
        cloud.admission_release.notify_one();
        tokio::time::timeout(Duration::from_secs(5), pending)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            journal
                .lock()
                .unwrap()
                .working_file(working_id)
                .unwrap()
                .node
                .size,
            3,
            "refused pathname action must preserve the existing stream length"
        );
        // Previously admitted descriptors retain their stream semantics even if
        // another namespace operation renamed that stream while a new open waited.
        tokio::task::spawn_blocking(move || old.set_len(2).unwrap())
            .await
            .unwrap();
        assert_eq!(
            journal
                .lock()
                .unwrap()
                .working_file(working_id)
                .unwrap()
                .node
                .size,
            2
        );
        // The unified pathname path must still hydrate, shrink and extend an
        // ordinary file without changing the already-open-descriptor contract.
        application(
            &mount,
            r#"
import os,sys
p=os.path.join(sys.argv[1],'document.txt')
os.truncate(p,2)
assert open(p,'rb').read()==b'ol'
os.truncate(p,6)
assert open(p,'rb').read()==b'ol\0\0\0\0'
"#,
        )
        .await;
        session.shutdown().await.unwrap();
    }
}

/// Inject an already-confirmed receipt to isolate the kernel/publication boundary.
/// This does not validate Apple's Trash or its generated ZIP implementation.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE"]
async fn real_native_trash_publication_preserves_held_generated_archive_reader() {
    use std::io::Read;
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let account = account(&mount);
    let cloud = Arc::new(Cloud::default());
    let original = Node {
        id: "FILE::com.apple.CloudDocs::synthetic-native".into(),
        parent_id: Some("root".into()),
        name: "Held.pages".into(),
        kind: NodeKind::Folder,
        package: true,
        size: 5,
        modified_unix: 1,
        etag: Some("original-E1".into()),
        content_version: Some("original-E1".into()),
        target: None,
    };
    // Models already verified immutable artifact bytes, not native ZIP parsing.
    let bytes: Vec<u8> = (0..128 * 1024).map(|n| (n % 251) as u8).collect();
    let expected_sha256 = hex::encode(Sha256::digest(&bytes));
    let artifact = Node {
        id: format!("icloud-artifact:{}", original.id),
        parent_id: Some(original.id.clone()),
        name: original.name.clone(),
        kind: NodeKind::File,
        package: false,
        size: bytes.len() as u64,
        modified_unix: 1,
        etag: None,
        content_version: Some(format!("synthetic-verified-artifact:{expected_sha256}")),
        target: None,
    };
    {
        let mut remote = cloud.remote.lock().unwrap();
        remote
            .files
            .insert(original.id.clone(), (original.clone(), Vec::new()));
        remote
            .files
            .insert(artifact.id.clone(), (artifact.clone(), bytes.clone()));
    }
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&temp.path().join("journal"), &account.id, 1024 * 1024).unwrap(),
    ));
    let engine = Engine::new(account, cloud.clone(), temp.path().join("state"))
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
    let path = mount.join(&original.name).join(&artifact.name);
    let (mut held, initial) = tokio::task::spawn_blocking({
        let path = path.clone();
        move || {
            let mut held = std::fs::File::open(path).unwrap();
            let mut contents = Vec::new();
            held.read_to_end(&mut contents).unwrap();
            (held, contents)
        }
    })
    .await
    .unwrap();
    assert_eq!(initial, bytes);
    assert_eq!(hex::encode(Sha256::digest(&initial)), expected_sha256);
    let reads_before = cloud.reads.load(Ordering::SeqCst);
    assert!(reads_before > 0);
    let scope = engine.scope("drive");
    assert!(
        cirrove_store::Store::open(&engine.db)
            .unwrap()
            .node(&scope, &original.id)
            .unwrap()
            .is_some()
    );
    let row = {
        // Prevent workers from claiming between enqueue and acknowledgement.
        let mut journal = journal.lock().unwrap();
        let row = journal
            .enqueue_mutation(MutationRequest {
                scope: scope.clone(),
                intent: MutationIntent::TrashNativeDocument {
                    before: original.clone(),
                },
            })
            .unwrap();
        let claimed = journal.claim_mutation().unwrap().unwrap();
        assert_eq!(claimed.id, row.id);
        {
            let mut remote = cloud.remote.lock().unwrap();
            remote.files.remove(&original.id).unwrap();
            remote.files.remove(&artifact.id).unwrap();
        }
        journal
            .acknowledge_mutation(
                row.id,
                claimed.attempt.unwrap(),
                MutationReceipt::Removed {
                    item: original.id.clone(),
                },
            )
            .unwrap();
        journal.mutation(row.id).unwrap()
    };
    cloud.offline.store(true, Ordering::SeqCst);
    let receipt_before = serde_json::to_vec(&row).unwrap();
    // Engine's package rechecks may independently learn the same absence.
    // Require this operation's durable publication acknowledgement as well;
    // otherwise disabling the publication worker can falsely pass this test.
    let publication_db = temp.path().join("journal/uploads.db");
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let db = rusqlite::Connection::open_with_flags(
                &publication_db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            ).unwrap();
            let done: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM native_trash_metadata_publication WHERE operation=?1 AND done=1)",
                [row.id.to_string()], |r| r.get(0),
            ).unwrap();
            if done { break; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.expect("native Trash publication was not durably acknowledged");
    // Also require visible Store convergence without triggering a frontend fetch.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if cirrove_store::Store::open(&engine.db)
                .unwrap()
                .node(&scope, &original.id)
                .unwrap()
                .is_none()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let root = mount.clone();
    let name = original.name.clone();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let present = tokio::task::spawn_blocking({
                let root = root.clone();
                let name = name.clone();
                move || {
                    std::fs::read_dir(root)
                        .unwrap()
                        .any(|entry| entry.unwrap().file_name() == name.as_str())
                }
            })
            .await
            .unwrap();
            if !present {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let (contents, path_error) = tokio::task::spawn_blocking(move || {
        assert_eq!(held.seek(SeekFrom::Start(0)).unwrap(), 0);
        let mut contents = Vec::new();
        held.read_to_end(&mut contents).unwrap();
        let error = std::fs::File::open(path).expect_err("removed path reopened");
        (contents, error.raw_os_error())
    })
    .await
    .unwrap();
    assert_eq!(contents, bytes);
    assert_eq!(hex::encode(Sha256::digest(&contents)), expected_sha256);
    assert_eq!(path_error, Some(libc::ENOENT));
    assert_eq!(
        cloud.reads.load(Ordering::SeqCst),
        reads_before,
        "held reader must not refetch deleted content"
    );
    assert_eq!(
        cloud.write_calls.load(Ordering::SeqCst),
        0,
        "publication must not dispatch mutations"
    );
    assert_eq!(
        serde_json::to_vec(&journal.lock().unwrap().mutation(row.id).unwrap()).unwrap(),
        receipt_before
    );
    session.shutdown().await.unwrap();
}

#[path = "writable_session/native_replacement_publication.rs"]
mod native_replacement_publication;
