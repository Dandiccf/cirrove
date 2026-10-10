//! One-shot remote allocation boundaries; synthetic providers and private disk only.
#![allow(clippy::unwrap_used)]
use async_trait::async_trait;
use cirrove_auth::CredentialVault;
use cirrove_core::{
    CancellationToken, Node, NodeKind, Scope,
    upload::{
        Reconciliation, Result as UploadResult, UploadError, UploadIntent, UploadProgress,
        UploadProvider, UploadRequest, UploadStep,
    },
};
use cirrove_service::{
    journal::{UploadJournal, UploadState},
    transfers::TransferWorker,
};
use secrecy::{ExposeSecret, SecretString};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use uuid::Uuid;
const ARMED: &str = "PRIVATE-allocation-armed";
const ALLOCATED: &str = "PRIVATE-allocated-identity";
const DATA: &[u8] = b"sealed";

struct Vault {
    root: PathBuf,
    mode: AtomicUsize,
    cancel: CancellationToken,
}
#[async_trait]
impl CredentialVault for Vault {
    async fn load(&self, _: &str) -> anyhow::Result<Option<SecretString>> {
        match std::fs::read_to_string(self.root.join("checkpoint")) {
            Ok(value) => Ok(Some(value.into())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    async fn save(&self, _: &str, value: SecretString) -> anyhow::Result<()> {
        if self.mode.load(Ordering::SeqCst) == 1 {
            anyhow::bail!("PRIVATE-vault-refusal");
        }
        let mut file = File::create(self.root.join("checkpoint"))?;
        file.write_all(value.expose_secret().as_bytes())?;
        file.sync_all()?;
        File::open(&self.root)?.sync_all()?;
        match self.mode.load(Ordering::SeqCst) {
            2 => anyhow::bail!("PRIVATE-save-response-lost"),
            3 => self.cancel.cancel(),
            _ => (),
        }
        Ok(())
    }
    async fn remove(&self, _: &str) -> anyhow::Result<()> {
        match std::fs::remove_file(self.root.join("checkpoint")) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}
struct Provider {
    root: PathBuf,
    journal: Weak<Mutex<UploadJournal>>,
    begins: AtomicUsize,
    allocations: AtomicUsize,
    inspections: AtomicUsize,
    mode: AtomicUsize,
    payload_mode: AtomicUsize,
    payload_begins: AtomicUsize,
}
fn node(request: &UploadRequest) -> Node {
    let UploadIntent::Create { parent, name } = &request.intent else {
        panic!("fixture intent")
    };
    Node {
        id: "allocated".into(),
        parent_id: Some(parent.clone()),
        name: name.clone(),
        kind: NodeKind::File,
        size: request.size,
        modified_unix: 0,
        etag: Some("verified".into()),
        content_version: None,
        target: None,
        package: false,
    }
}
#[async_trait]
impl UploadProvider for Provider {
    fn requires_begin_payload(&self, _: &UploadRequest) -> bool {
        self.payload_mode.load(Ordering::SeqCst) != 0
    }
    async fn begin_upload_from_payload_for_operation(
        &self,
        operation: &str,
        request: &UploadRequest,
        mut file: File,
        cancel: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        let id: Uuid = operation.parse().unwrap();
        let journal = self.journal.upgrade().unwrap();
        {
            let guard = journal
                .try_lock()
                .expect("payload begin holds no journal lock");
            let row = guard.get(id).unwrap();
            assert_eq!(row.scope, request.scope);
            assert_eq!(row.intent, request.intent);
            assert!(row.representation == request.representation);
            assert_eq!(row.sha256, request.sha256);
            assert_eq!(row.size, request.size);
            assert_eq!(row.session_key, None);
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, DATA);
        assert_eq!(self.allocations.load(Ordering::SeqCst), 0);
        self.payload_begins.fetch_add(1, Ordering::SeqCst);
        if self.payload_mode.load(Ordering::SeqCst) == 2 {
            cancel.cancel();
        }
        self.begin_upload(request, cancel).await
    }
    fn begin_is_mutation_free_until_checkpoint(&self, _: &UploadRequest) -> bool {
        true
    }
    async fn begin_upload(
        &self,
        _: &UploadRequest,
        _: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        self.begins.fetch_add(1, Ordering::SeqCst);
        Ok(UploadStep::Allocate(SecretString::from(ARMED)))
    }
    async fn allocate_upload_for_operation(
        &self,
        operation: &str,
        _: &UploadRequest,
        checkpoint: &SecretString,
        _: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        assert_eq!(checkpoint.expose_secret(), ARMED);
        assert_eq!(
            std::fs::read_to_string(self.root.join("checkpoint")).unwrap(),
            ARMED
        );
        let id: Uuid = operation.parse().unwrap();
        {
            let journal = self.journal.upgrade().unwrap();
            let guard = journal
                .try_lock()
                .expect("allocation must not hold journal lock");
            assert_eq!(
                guard.get(id).unwrap().session_key,
                Some(id),
                "allocation preceded durable journal binding"
            );
        }
        self.allocations.fetch_add(1, Ordering::SeqCst);
        let mut marker = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.root.join("allocated"))
            .expect("allocation replayed");
        marker.write_all(operation.as_bytes()).unwrap();
        marker.sync_all().unwrap();
        File::open(&self.root).unwrap().sync_all().unwrap();
        match self.mode.load(Ordering::SeqCst) {
            1 => Err(UploadError::Uncertain),
            2 => {
                File::create(self.root.join("ready"))
                    .unwrap()
                    .sync_all()
                    .unwrap();
                std::future::pending().await
            }
            5 => Ok(UploadStep::Allocate(SecretString::from(ARMED))),
            _ => Ok(UploadStep::Continue(UploadProgress {
                checkpoint: SecretString::from(ALLOCATED),
                offset: 0,
                length: DATA.len() as u32,
            })),
        }
    }
    async fn inspect_upload(
        &self,
        _: &UploadRequest,
        _: &SecretString,
        _: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        let count = self.inspections.fetch_add(1, Ordering::SeqCst);
        match self.mode.load(Ordering::SeqCst) {
            3 => Ok(UploadStep::Allocate(SecretString::from(ARMED))),
            4 if count == 0 => Ok(UploadStep::Prepared(SecretString::from(ARMED))),
            4 => Ok(UploadStep::Allocate(SecretString::from(ARMED))),
            _ => Err(UploadError::Uncertain),
        }
    }
    async fn upload_part(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
        offset: u64,
        bytes: Vec<u8>,
        _: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        assert_eq!(checkpoint.expose_secret(), ALLOCATED);
        assert_eq!(
            std::fs::read_to_string(self.root.join("checkpoint")).unwrap(),
            ALLOCATED
        );
        assert_eq!(offset, 0);
        assert_eq!(bytes, DATA);
        Ok(UploadStep::Complete(node(request)))
    }
    async fn commit_upload(
        &self,
        _: &UploadRequest,
        _: &SecretString,
        _: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        panic!("unexpected commit")
    }
    async fn reconcile_upload(
        &self,
        _: &UploadRequest,
        checkpoint: Option<&SecretString>,
        _: &CancellationToken,
    ) -> UploadResult<Reconciliation> {
        assert!(checkpoint.is_some());
        Err(UploadError::Uncertain)
    }
}
fn journal(root: &Path) -> Arc<Mutex<UploadJournal>> {
    Arc::new(Mutex::new(
        UploadJournal::open(&root.join("journal"), "allocation", 1024).unwrap(),
    ))
}
fn enqueue(j: &Arc<Mutex<UploadJournal>>) -> Uuid {
    j.lock()
        .unwrap()
        .enqueue(
            Scope {
                account: "allocation".into(),
                provider: "fixture".into(),
                collection: "drive".into(),
            },
            UploadIntent::Create {
                parent: "root".into(),
                name: "owned".into(),
            },
            DATA,
        )
        .unwrap()
        .id
}
fn fixture(
    root: &Path,
    j: &Arc<Mutex<UploadJournal>>,
    cancel: CancellationToken,
) -> (Arc<Provider>, Arc<Vault>) {
    (
        Arc::new(Provider {
            root: root.into(),
            journal: Arc::downgrade(j),
            begins: AtomicUsize::new(0),
            allocations: AtomicUsize::new(0),
            inspections: AtomicUsize::new(0),
            mode: AtomicUsize::new(0),
            payload_mode: AtomicUsize::new(0),
            payload_begins: AtomicUsize::new(0),
        }),
        Arc::new(Vault {
            root: root.into(),
            mode: AtomicUsize::new(0),
            cancel,
        }),
    )
}
#[tokio::test]
async fn allocation_persists_armed_checkpoint_and_journal_binding_before_single_call() {
    let temp = tempfile::tempdir().unwrap();
    let j = journal(temp.path());
    let id = enqueue(&j);
    let cancel = CancellationToken::new();
    let (p, v) = fixture(temp.path(), &j, cancel.clone());
    let result = TransferWorker::new(j.clone(), p.clone(), v, cancel)
        .run_once()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.state, UploadState::Uploaded);
    assert_eq!(result.id, id);
    assert_eq!(p.allocations.load(Ordering::SeqCst), 1);
    assert_eq!(p.inspections.load(Ordering::SeqCst), 0);
    assert!(!temp.path().join("checkpoint").exists());
    assert!(!format!("{:?}", UploadStep::Allocate(SecretString::from(ARMED))).contains(ARMED));
}
#[tokio::test]
async fn fresh_payload_begin_verifies_source_before_allocation_and_stops_on_damage_or_cancel() {
    for arm in ["success", "corrupt", "cancel", "vault-refusal"] {
        let root = tempfile::tempdir().unwrap().keep();
        let j = journal(&root);
        let id = enqueue(&j);
        let cancel = CancellationToken::new();
        let (p, v) = fixture(&root, &j, cancel.clone());
        p.payload_mode
            .store(if arm == "cancel" { 2 } else { 1 }, Ordering::SeqCst);
        if arm == "corrupt" {
            let payload = root.join("journal/objects").join(id.to_string());
            std::fs::set_permissions(&payload, std::fs::Permissions::from_mode(0o600)).unwrap();
            std::fs::write(&payload, b"damage").unwrap();
            std::fs::set_permissions(&payload, std::fs::Permissions::from_mode(0o400)).unwrap();
        }
        if arm == "vault-refusal" {
            v.mode.store(1, Ordering::SeqCst);
        }
        let before = std::fs::read(root.join("journal/objects").join(id.to_string())).unwrap();
        let outcome = TransferWorker::new(j.clone(), p.clone(), v, cancel)
            .run_once()
            .await;
        assert_eq!(
            std::fs::read(root.join("journal/objects").join(id.to_string())).unwrap(),
            before
        );
        if arm == "corrupt" {
            assert!(matches!(
                outcome,
                Err(cirrove_service::transfers::TransferError::Journal(
                    cirrove_service::journal::JournalError::Corrupt
                ))
            ));
            assert_eq!(p.payload_begins.load(Ordering::SeqCst), 0);
            assert_eq!(p.begins.load(Ordering::SeqCst), 0);
            assert_eq!(p.allocations.load(Ordering::SeqCst), 0);
            assert!(!root.join("allocated").exists());
            continue;
        }
        let result = outcome.unwrap().unwrap();
        assert_eq!(result.id, id);
        if arm == "success" {
            assert_eq!(result.state, UploadState::Uploaded);
            assert_eq!(p.allocations.load(Ordering::SeqCst), 1);
        } else {
            assert_eq!(
                result.state,
                if arm == "corrupt" {
                    UploadState::Failed
                } else {
                    UploadState::VerifyRequired
                }
            );
            assert_eq!(p.allocations.load(Ordering::SeqCst), 0);
            assert!(!root.join("allocated").exists());
        }
        assert_eq!(
            p.payload_begins.load(Ordering::SeqCst),
            usize::from(arm != "corrupt")
        );
        assert_eq!(
            p.begins.load(Ordering::SeqCst),
            usize::from(arm != "corrupt")
        );
    }
}
#[tokio::test]
async fn payload_begin_lost_allocation_recovery_never_prepares_or_allocates_again() {
    for recovery_mode in [0, 3, 4] {
        let root = tempfile::tempdir().unwrap().keep();
        let j = journal(&root);
        let id = enqueue(&j);
        let (p, v) = fixture(&root, &j, CancellationToken::new());
        p.payload_mode.store(1, Ordering::SeqCst);
        p.mode.store(1, Ordering::SeqCst);
        let before = std::fs::read(root.join("journal/objects").join(id.to_string())).unwrap();
        assert_eq!(
            TransferWorker::new(j.clone(), p.clone(), v.clone(), CancellationToken::new())
                .run_once()
                .await
                .unwrap()
                .unwrap()
                .state,
            UploadState::VerifyRequired
        );
        p.mode.store(recovery_mode, Ordering::SeqCst);
        j.lock().unwrap().request_retry(id).unwrap();
        assert_eq!(
            TransferWorker::new(j.clone(), p.clone(), v, CancellationToken::new())
                .run_once()
                .await
                .unwrap()
                .unwrap()
                .state,
            UploadState::VerifyRequired
        );
        assert_eq!(
            std::fs::read(root.join("journal/objects").join(id.to_string())).unwrap(),
            before
        );
        assert_eq!(p.payload_begins.load(Ordering::SeqCst), 1);
        assert_eq!(p.begins.load(Ordering::SeqCst), 1);
        assert_eq!(p.allocations.load(Ordering::SeqCst), 1);
        assert_eq!(j.lock().unwrap().get(id).unwrap().session_key, Some(id));
    }
}
#[tokio::test]
async fn allocation_never_runs_after_vault_failure_or_cancellation_at_armed_save() {
    for mode in [1, 2, 3] {
        let temp = tempfile::tempdir().unwrap();
        let j = journal(temp.path());
        let id = enqueue(&j);
        let cancel = CancellationToken::new();
        let (p, v) = fixture(temp.path(), &j, cancel.clone());
        v.mode.store(mode, Ordering::SeqCst);
        let result = TransferWorker::new(j.clone(), p.clone(), v.clone(), cancel)
            .run_once()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.state, UploadState::VerifyRequired);
        assert_eq!(p.allocations.load(Ordering::SeqCst), 0);
        assert!(!temp.path().join("allocated").exists());
        assert!(!result.issue.unwrap().contains("PRIVATE"));
        if mode == 2 {
            assert_eq!(j.lock().unwrap().get(id).unwrap().session_key, None);
            assert_eq!(
                std::fs::read_to_string(temp.path().join("checkpoint")).unwrap(),
                ARMED
            );
        }
        if mode != 1 {
            v.mode.store(0, Ordering::SeqCst);
            j.lock().unwrap().request_retry(id).unwrap();
            let result = TransferWorker::new(j, p.clone(), v, CancellationToken::new())
                .run_once()
                .await
                .unwrap()
                .unwrap();
            assert_eq!(result.state, UploadState::VerifyRequired);
            assert_eq!(p.allocations.load(Ordering::SeqCst), 0);
            assert_eq!(p.begins.load(Ordering::SeqCst), 1);
            assert_eq!(p.inspections.load(Ordering::SeqCst), 1);
        }
    }
}
#[tokio::test]
async fn allocation_lost_result_and_recovery_cannot_reenter_allocation_even_via_prepared() {
    for recovery_mode in [0, 3, 4] {
        let temp = tempfile::tempdir().unwrap();
        let j = journal(temp.path());
        let id = enqueue(&j);
        let cancel = CancellationToken::new();
        let (p, v) = fixture(temp.path(), &j, cancel.clone());
        p.mode.store(1, Ordering::SeqCst);
        assert_eq!(
            TransferWorker::new(j.clone(), p.clone(), v.clone(), cancel)
                .run_once()
                .await
                .unwrap()
                .unwrap()
                .state,
            UploadState::VerifyRequired
        );
        p.mode.store(recovery_mode, Ordering::SeqCst);
        j.lock().unwrap().request_retry(id).unwrap();
        assert_eq!(
            TransferWorker::new(j.clone(), p.clone(), v, CancellationToken::new())
                .run_once()
                .await
                .unwrap()
                .unwrap()
                .state,
            UploadState::VerifyRequired
        );
        assert_eq!(p.allocations.load(Ordering::SeqCst), 1);
        assert_eq!(p.begins.load(Ordering::SeqCst), 1);
        assert_eq!(
            std::fs::read_to_string(temp.path().join("checkpoint")).unwrap(),
            ARMED
        );
        assert_eq!(j.lock().unwrap().get(id).unwrap().session_key, Some(id));
    }
}
#[tokio::test]
async fn allocation_callback_cannot_schedule_a_second_allocation() {
    let temp = tempfile::tempdir().unwrap();
    let j = journal(temp.path());
    enqueue(&j);
    let cancel = CancellationToken::new();
    let (p, v) = fixture(temp.path(), &j, cancel.clone());
    p.mode.store(5, Ordering::SeqCst);
    assert_eq!(
        TransferWorker::new(j, p.clone(), v, cancel)
            .run_once()
            .await
            .unwrap()
            .unwrap()
            .state,
        UploadState::VerifyRequired
    );
    assert_eq!(p.allocations.load(Ordering::SeqCst), 1);
}
#[tokio::test]
#[ignore = "subprocess fixture only"]
async fn allocation_crash_child() {
    let root = PathBuf::from(std::env::var("CIRROVE_ALLOCATION_FIXTURE_ROOT").unwrap());
    let j = journal(&root);
    enqueue(&j);
    let cancel = CancellationToken::new();
    let (p, v) = fixture(&root, &j, cancel.clone());
    p.mode.store(2, Ordering::SeqCst);
    TransferWorker::new(j, p, v, cancel)
        .run_once()
        .await
        .unwrap();
    panic!("allocation child unexpectedly finished");
}
#[tokio::test]
async fn process_death_after_allocation_leaves_saved_uncertainty_without_replay() {
    let temp = tempfile::tempdir().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "allocation_crash_child", "--ignored"])
        .env("CIRROVE_ALLOCATION_FIXTURE_ROOT", temp.path())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !temp.path().join("ready").exists() {
        if Instant::now() >= deadline || child.try_wait().unwrap().is_some() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("allocation child never reached allocation");
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    child.kill().unwrap();
    child.wait().unwrap();
    let id: Uuid = std::fs::read_to_string(temp.path().join("allocated"))
        .unwrap()
        .parse()
        .unwrap();
    let j = journal(temp.path());
    let cancel = CancellationToken::new();
    let (p, v) = fixture(temp.path(), &j, cancel.clone());
    assert_eq!(j.lock().unwrap().get(id).unwrap().session_key, Some(id));
    assert_eq!(
        TransferWorker::new(j.clone(), p.clone(), v, cancel)
            .run_once()
            .await
            .unwrap()
            .unwrap()
            .state,
        UploadState::VerifyRequired
    );
    assert_eq!(p.allocations.load(Ordering::SeqCst), 0);
    assert_eq!(p.begins.load(Ordering::SeqCst), 0);
    assert_eq!(p.inspections.load(Ordering::SeqCst), 1);
    assert_eq!(
        std::fs::read_to_string(temp.path().join("checkpoint")).unwrap(),
        ARMED
    );
    assert!(j.lock().unwrap().payload(id).is_ok());
}

#[tokio::test]
async fn allocation_retry_after_failure_before_any_saved_checkpoint_allocates_once() {
    let temp = tempfile::tempdir().unwrap();
    let j = journal(temp.path());
    let id = enqueue(&j);
    let cancel = CancellationToken::new();
    let (p, v) = fixture(temp.path(), &j, cancel.clone());
    v.mode.store(1, Ordering::SeqCst);
    let worker = TransferWorker::new(j.clone(), p.clone(), v.clone(), cancel);
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        UploadState::VerifyRequired
    );
    assert_eq!(j.lock().unwrap().get(id).unwrap().session_key, None);
    assert!(!temp.path().join("checkpoint").exists());
    assert_eq!(p.allocations.load(Ordering::SeqCst), 0);
    v.mode.store(0, Ordering::SeqCst);
    j.lock().unwrap().request_retry(id).unwrap();
    // Recovery proves no checkpoint was ever persisted. This execution only
    // resets the journal; allocation happens on the next fresh begin.
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        UploadState::Pending
    );
    assert_eq!(p.begins.load(Ordering::SeqCst), 1);
    assert_eq!(p.allocations.load(Ordering::SeqCst), 0);
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        UploadState::Uploaded
    );
    assert_eq!(p.begins.load(Ordering::SeqCst), 2);
    assert_eq!(p.allocations.load(Ordering::SeqCst), 1);
    assert_eq!(p.inspections.load(Ordering::SeqCst), 0);
}
