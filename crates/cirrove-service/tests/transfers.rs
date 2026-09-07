//! Service/journal/credential recovery with synthetic providers; no cloud access.
#![allow(clippy::unwrap_used)]
use async_trait::async_trait;
use cirrove_auth::CredentialVault;
use cirrove_core::upload::{
    Reconciliation, Result as UploadResult, UploadError, UploadIntent, UploadProgress,
    UploadProvider, UploadRequest, UploadStep,
};
use cirrove_core::{CancellationToken, Node, NodeKind, ProviderError, Scope};
use cirrove_service::{
    journal::{UploadJournal, UploadState},
    transfers::TransferWorker,
};
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::Read,
    path::Path,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;

const DATA: &[u8] = b"abcdefghij";
const SECRET: &str = "https://fixture.invalid/session?PRIVATE-CHECKPOINT";
#[derive(Default)]
struct LockProbe(Mutex<Weak<Mutex<UploadJournal>>>);
impl LockProbe {
    fn attach(&self, journal: &Arc<Mutex<UploadJournal>>) {
        *self.0.lock().unwrap() = Arc::downgrade(journal);
    }
    fn check(&self) {
        if let Some(journal) = self.0.lock().unwrap().upgrade() {
            assert!(
                journal.try_lock().is_ok(),
                "network or keyring call held the local journal lock"
            );
        }
    }
}
#[derive(Default)]
struct Vault {
    values: Mutex<HashMap<String, SecretString>>,
    probe: Arc<LockProbe>,
    fail_save: AtomicBool,
    save_then_fail: AtomicBool,
    fail_remove: AtomicBool,
}
#[async_trait]
impl CredentialVault for Vault {
    async fn load(&self, key: &str) -> anyhow::Result<Option<SecretString>> {
        self.probe.check();
        Ok(self.values.lock().unwrap().get(key).cloned())
    }
    async fn save(&self, key: &str, value: SecretString) -> anyhow::Result<()> {
        self.probe.check();
        if self.fail_save.load(Ordering::SeqCst) {
            anyhow::bail!("PRIVATE-VAULT-ERROR");
        }
        self.values.lock().unwrap().insert(key.into(), value);
        if self.save_then_fail.swap(false, Ordering::SeqCst) {
            anyhow::bail!("PRIVATE-VAULT-ERROR");
        }
        Ok(())
    }
    async fn remove(&self, key: &str) -> anyhow::Result<()> {
        self.probe.check();
        if self.fail_remove.load(Ordering::SeqCst) {
            anyhow::bail!("PRIVATE-VAULT-ERROR");
        }
        self.values.lock().unwrap().remove(key);
        Ok(())
    }
}
#[derive(Default)]
struct Remote {
    data: Vec<u8>,
    offsets: Vec<u64>,
    begins: usize,
    inspections: usize,
    reconciliations: usize,
    committed: Option<Node>,
}
struct Provider {
    state: Mutex<Remote>,
    probe: Arc<LockProbe>,
    fail_offset_once: AtomicU64,
    lose_success: AtomicBool,
    deferred: AtomicBool,
    conflict_commit: AtomicBool,
    fail_begin_once: AtomicBool,
    pause_offset: AtomicU64,
    entered: Notify,
}
impl Provider {
    fn new(probe: Arc<LockProbe>) -> Self {
        Self {
            state: Mutex::new(Remote::default()),
            probe,
            fail_offset_once: AtomicU64::new(u64::MAX),
            lose_success: AtomicBool::new(false),
            deferred: AtomicBool::new(false),
            conflict_commit: AtomicBool::new(false),
            fail_begin_once: AtomicBool::new(false),
            pause_offset: AtomicU64::new(u64::MAX),
            entered: Notify::new(),
        }
    }
    fn more(&self, request: &UploadRequest) -> UploadStep {
        let offset = self.state.lock().unwrap().data.len() as u64;
        if offset == request.size {
            return UploadStep::Commit(SecretString::from(SECRET));
        }
        UploadStep::Continue(UploadProgress {
            checkpoint: SecretString::from(SECRET),
            offset,
            length: (request.size - offset).min(4) as u32,
        })
    }
    fn node(request: &UploadRequest) -> Node {
        let (id, parent, name) = match &request.intent {
            UploadIntent::Create { parent, name } => {
                ("created-id".into(), parent.clone(), name.clone())
            }
            UploadIntent::Replace { item, .. } => (item.clone(), "root".into(), "Saved.txt".into()),
        };
        Node {
            id,
            parent_id: Some(parent),
            name,
            kind: NodeKind::File,
            size: request.size,
            modified_unix: 0,
            etag: Some("new-tag".into()),
            content_version: Some("content-v2".into()),
            target: None,
        }
    }
}
#[async_trait]
impl UploadProvider for Provider {
    async fn begin_upload(
        &self,
        request: &UploadRequest,
        _: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        self.probe.check();
        {
            let mut state = self.state.lock().unwrap();
            state.begins += 1;
            state.data.clear();
            state.committed = None;
        }
        if self.fail_begin_once.swap(false, Ordering::SeqCst) {
            return Err(ProviderError::Unavailable.into());
        }
        Ok(self.more(request))
    }
    async fn inspect_upload(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
        _: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        self.probe.check();
        {
            let mut state = self.state.lock().unwrap();
            state.inspections += 1;
            if checkpoint.expose_secret() != SECRET {
                return Err(UploadError::CheckpointInvalid);
            }
            if state.committed.is_some() {
                return Err(UploadError::SessionGone);
            }
        }
        Ok(self.more(request))
    }
    async fn upload_part(
        &self,
        request: &UploadRequest,
        _: &SecretString,
        offset: u64,
        bytes: Vec<u8>,
        cancel: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        self.probe.check();
        if self.pause_offset.load(Ordering::SeqCst) == offset {
            self.entered.notify_one();
            cancel.cancelled().await;
            return Err(UploadError::Uncertain);
        }
        self.state.lock().unwrap().offsets.push(offset);
        if self
            .fail_offset_once
            .compare_exchange(offset, u64::MAX, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return Err(UploadError::Uncertain);
        }
        let complete = {
            let mut state = self.state.lock().unwrap();
            assert_eq!(state.data.len() as u64, offset);
            state.data.extend_from_slice(&bytes);
            state.data.len() as u64 == request.size
        };
        if complete && !self.deferred.load(Ordering::SeqCst) {
            let node = Self::node(request);
            self.state.lock().unwrap().committed = Some(node.clone());
            if self.lose_success.swap(false, Ordering::SeqCst) {
                return Err(UploadError::Uncertain);
            }
            Ok(UploadStep::Complete(node))
        } else {
            Ok(self.more(request))
        }
    }
    async fn commit_upload(
        &self,
        request: &UploadRequest,
        _: &SecretString,
        _: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        self.probe.check();
        if self.conflict_commit.load(Ordering::SeqCst) {
            return Err(UploadError::Conflict);
        }
        let node = Self::node(request);
        self.state.lock().unwrap().committed = Some(node.clone());
        Ok(UploadStep::Complete(node))
    }
    async fn reconcile_upload(
        &self,
        request: &UploadRequest,
        _: &CancellationToken,
    ) -> UploadResult<Reconciliation> {
        self.probe.check();
        let mut state = self.state.lock().unwrap();
        state.reconciliations += 1;
        Ok(match &state.committed {
            Some(node) if hex::encode(Sha256::digest(&state.data)) == request.sha256 => {
                Reconciliation::Committed(node.clone())
            }
            Some(_) => Reconciliation::Conflict,
            None => Reconciliation::Uncommitted,
        })
    }
}
fn journal(root: &Path, probe: &LockProbe) -> Arc<Mutex<UploadJournal>> {
    let result = Arc::new(Mutex::new(
        UploadJournal::open(root, "fixture", 1024 * 1024).unwrap(),
    ));
    probe.attach(&result);
    result
}
fn enqueue(j: &Arc<Mutex<UploadJournal>>, name: &str) -> uuid::Uuid {
    j.lock()
        .unwrap()
        .enqueue(
            Scope {
                account: "fixture".into(),
                provider: "onedrive".into(),
                collection: "drive".into(),
            },
            UploadIntent::Create {
                parent: "root".into(),
                name: name.into(),
            },
            DATA,
        )
        .unwrap()
        .id
}
fn assert_local(j: &Arc<Mutex<UploadJournal>>, id: uuid::Uuid) {
    let mut data = vec![];
    j.lock()
        .unwrap()
        .payload(id)
        .unwrap()
        .read_to_end(&mut data)
        .unwrap();
    assert_eq!(data, DATA);
}
fn fixture() -> (Arc<LockProbe>, Arc<Provider>, Arc<Vault>) {
    let probe = Arc::new(LockProbe::default());
    let p = Arc::new(Provider::new(probe.clone()));
    let v = Arc::new(Vault {
        probe: probe.clone(),
        ..Default::default()
    });
    (probe, p, v)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupted_upload_resumes_from_remote_offset_after_journal_restart() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let (probe, p, v) = fixture();
    let j = journal(&root, &probe);
    let id = enqueue(&j, "Saved.txt");
    p.fail_offset_once.store(4, Ordering::SeqCst);
    let worker = TransferWorker::new(j.clone(), p.clone(), v.clone(), CancellationToken::new());
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        UploadState::VerifyRequired
    );
    assert_eq!(j.lock().unwrap().get(id).unwrap().transferred_bytes, 4);
    for entry in std::fs::read_dir(&root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            assert!(
                !std::fs::read(path)
                    .unwrap()
                    .windows(b"PRIVATE-CHECKPOINT".len())
                    .any(|b| b == b"PRIVATE-CHECKPOINT")
            );
        }
    }
    drop(worker);
    drop(j);
    let j = journal(&root, &probe);
    j.lock().unwrap().request_retry(id).unwrap();
    let worker = TransferWorker::new(j.clone(), p.clone(), v.clone(), CancellationToken::new());
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        UploadState::Uploaded
    );
    {
        let state = p.state.lock().unwrap();
        assert_eq!(state.begins, 1);
        assert_eq!(state.inspections, 1);
        assert_eq!(state.offsets, [0, 4, 4, 8]);
        assert_eq!(state.data, DATA);
    }
    assert_local(&j, id);
    assert_eq!(
        j.lock().unwrap().get(id).unwrap().transferred_bytes,
        DATA.len() as u64
    );
    assert!(v.values.lock().unwrap().is_empty());
    assert!(worker.run_once().await.unwrap().is_none());
}

#[tokio::test]
async fn empty_checkpoint_reconciles_success_or_conflict_without_reuploading() {
    for competing in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let (probe, p, v) = fixture();
        let j = journal(&temp.path().join("journal"), &probe);
        let id = enqueue(&j, "Saved.txt");
        p.lose_success.store(true, Ordering::SeqCst);
        let worker = TransferWorker::new(j.clone(), p.clone(), v.clone(), CancellationToken::new());
        assert_eq!(
            worker.run_once().await.unwrap().unwrap().state,
            UploadState::VerifyRequired
        );
        v.values
            .lock()
            .unwrap()
            .insert(format!("upload/{id}"), SecretString::from(""));
        if competing {
            p.state.lock().unwrap().data = b"competing version".to_vec();
        }
        j.lock().unwrap().request_retry(id).unwrap();
        assert_eq!(
            worker.run_once().await.unwrap().unwrap().state,
            if competing {
                UploadState::Conflict
            } else {
                UploadState::Uploaded
            }
        );
        assert_eq!(p.state.lock().unwrap().begins, 1);
        assert_eq!(p.state.lock().unwrap().reconciliations, 1);
        assert_local(&j, id);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lost_success_response_is_reconciled_without_uploading_twice() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let (probe, p, v) = fixture();
    let j = journal(&root, &probe);
    let id = enqueue(&j, "Saved.txt");
    v.values.lock().unwrap().insert(
        "unrelated-credential".into(),
        SecretString::from("do-not-touch"),
    );
    p.lose_success.store(true, Ordering::SeqCst);
    let worker = TransferWorker::new(j.clone(), p.clone(), v.clone(), CancellationToken::new());
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        UploadState::VerifyRequired
    );
    drop(worker);
    drop(j);
    let j = journal(&root, &probe);
    j.lock().unwrap().request_retry(id).unwrap();
    let worker = TransferWorker::new(j.clone(), p.clone(), v.clone(), CancellationToken::new());
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        UploadState::Uploaded
    );
    {
        let state = p.state.lock().unwrap();
        assert_eq!(state.begins, 1);
        assert_eq!(state.reconciliations, 1);
        assert_eq!(state.offsets, [0, 4, 8]);
    }
    assert_local(&j, id);
    assert_eq!(v.values.lock().unwrap().len(), 1);
    assert!(
        v.values
            .lock()
            .unwrap()
            .contains_key("unrelated-credential")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_bytes_are_uploaded_until_the_checkpoint_is_saved_and_lost_save_replies_recover() {
    for written in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("journal");
        let (probe, p, v) = fixture();
        let j = journal(&root, &probe);
        let id = enqueue(&j, "Saved.txt");
        v.fail_save.store(!written, Ordering::SeqCst);
        v.save_then_fail.store(written, Ordering::SeqCst);
        let worker = TransferWorker::new(j.clone(), p.clone(), v.clone(), CancellationToken::new());
        let result = worker.run_once().await.unwrap().unwrap();
        assert_eq!(result.state, UploadState::VerifyRequired);
        assert!(!result.issue.unwrap().contains("PRIVATE"));
        assert!(p.state.lock().unwrap().offsets.is_empty());
        assert!(j.lock().unwrap().get(id).unwrap().session_key.is_none());
        drop(worker);
        drop(j);
        let j = journal(&root, &probe);
        j.lock().unwrap().request_retry(id).unwrap();
        v.fail_save.store(false, Ordering::SeqCst);
        let worker = TransferWorker::new(j.clone(), p.clone(), v.clone(), CancellationToken::new());
        let result = worker.run_once().await.unwrap().unwrap();
        if written {
            assert_eq!(result.state, UploadState::Uploaded);
            assert_eq!(p.state.lock().unwrap().begins, 1);
        } else {
            assert_eq!(result.state, UploadState::Pending);
            assert_eq!(
                worker.run_once().await.unwrap().unwrap().state,
                UploadState::Uploaded
            );
            assert_eq!(p.state.lock().unwrap().begins, 2);
        }
        assert_local(&j, id);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn durable_backoff_leaves_independent_files_eligible() {
    let temp = tempfile::tempdir().unwrap();
    let (probe, p, v) = fixture();
    let j = journal(&temp.path().join("journal"), &probe);
    let first = enqueue(&j, "First.txt");
    let second = enqueue(&j, "Second.txt");
    p.fail_begin_once.store(true, Ordering::SeqCst);
    let worker = TransferWorker::new(j.clone(), p, v, CancellationToken::new());
    assert_eq!(worker.run_once().await.unwrap().unwrap().id, first);
    let result = worker.run_once().await.unwrap().unwrap();
    assert_eq!(result.id, second);
    assert_eq!(result.state, UploadState::Uploaded);
    assert_eq!(
        j.lock().unwrap().get(first).unwrap().state,
        UploadState::VerifyRequired
    );
    assert!(j.lock().unwrap().get(first).unwrap().retry_at > 0);
    assert_local(&j, first);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_transfer_progress_is_not_success_when_the_conditional_commit_conflicts() {
    let temp = tempfile::tempdir().unwrap();
    let (probe, p, v) = fixture();
    let j = journal(&temp.path().join("journal"), &probe);
    let id = enqueue(&j, "Saved.txt");
    p.deferred.store(true, Ordering::SeqCst);
    p.conflict_commit.store(true, Ordering::SeqCst);
    let worker = TransferWorker::new(j.clone(), p.clone(), v, CancellationToken::new());
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        UploadState::Conflict
    );
    let record = j.lock().unwrap().get(id).unwrap();
    assert_eq!(record.transferred_bytes, DATA.len() as u64);
    assert!(record.remote.is_none());
    assert!(p.state.lock().unwrap().committed.is_none());
    assert!(j.lock().unwrap().prune_uploaded_payload(id).is_err());
    assert_local(&j, id);
    assert!(worker.run_once().await.unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_releases_a_stalled_transfer_and_preserves_local_data() {
    let temp = tempfile::tempdir().unwrap();
    let (probe, p, v) = fixture();
    let j = journal(&temp.path().join("journal"), &probe);
    let id = enqueue(&j, "Saved.txt");
    p.pause_offset.store(0, Ordering::SeqCst);
    let cancel = CancellationToken::new();
    let worker = Arc::new(TransferWorker::new(j.clone(), p.clone(), v, cancel.clone()));
    let running = {
        let worker = worker.clone();
        tokio::spawn(async move { worker.run_once().await })
    };
    tokio::time::timeout(Duration::from_secs(2), p.entered.notified())
        .await
        .unwrap();
    assert_eq!(
        j.try_lock().unwrap().get(id).unwrap().state,
        UploadState::Uploading
    );
    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(2), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result.state, UploadState::VerifyRequired);
    assert_local(&j, id);
    assert!(p.state.lock().unwrap().data.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_secret_cleanup_does_not_undo_a_durable_upload_receipt() {
    let temp = tempfile::tempdir().unwrap();
    let (probe, p, v) = fixture();
    let j = journal(&temp.path().join("journal"), &probe);
    let id = enqueue(&j, "Saved.txt");
    v.fail_remove.store(true, Ordering::SeqCst);
    let worker = TransferWorker::new(j.clone(), p, v.clone(), CancellationToken::new());
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        UploadState::Uploaded
    );
    assert!(j.lock().unwrap().get(id).unwrap().remote.is_some());
    assert_eq!(v.values.lock().unwrap().len(), 1);
    assert_local(&j, id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn saving_again_during_a_transfer_preserves_both_generations_through_recovery() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let (probe, provider, vault) = fixture();
    let j = journal(&root, &probe);
    let working = {
        let mut j = j.lock().unwrap();
        let node = Node {
            id: String::new(),
            parent_id: Some("root".into()),
            name: "Saved.txt".into(),
            kind: NodeKind::File,
            size: 0,
            modified_unix: 0,
            etag: None,
            content_version: None,
            target: None,
        };
        let w = j
            .create_working(
                Scope {
                    account: "fixture".into(),
                    provider: "onedrive".into(),
                    collection: "drive".into(),
                },
                node,
                true,
                &b""[..],
            )
            .unwrap();
        j.write_working(w.id, 0, DATA).unwrap();
        j.seal_working(w.id).unwrap();
        w
    };
    provider.pause_offset.store(4, Ordering::SeqCst);
    let cancel = CancellationToken::new();
    let worker = TransferWorker::new(j.clone(), provider.clone(), vault.clone(), cancel.clone());
    let upload = tokio::spawn(async move { worker.run_once().await });
    tokio::time::timeout(Duration::from_secs(2), provider.entered.notified())
        .await
        .unwrap();
    let second = {
        let mut j = j.lock().unwrap();
        j.write_working(working.id, 0, b"newer-edit").unwrap();
        j.seal_working(working.id).unwrap().unwrap()
    };
    assert_eq!(provider.state.lock().unwrap().data, b"abcd");
    cancel.cancel();
    upload.await.unwrap().unwrap();
    drop(j);
    let j = journal(&root, &probe);
    provider.pause_offset.store(u64::MAX, Ordering::SeqCst);
    let worker = TransferWorker::new(j.clone(), provider.clone(), vault, CancellationToken::new());
    // Explicit retry retains the existing verification requirement and session.
    {
        let mut j = j.lock().unwrap();
        let first = j.list(0, 100).unwrap()[0].id;
        j.request_retry(first).unwrap();
    }
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        UploadState::Uploaded
    );
    assert_eq!(provider.state.lock().unwrap().data, DATA);
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        UploadState::Uploaded
    );
    assert_eq!(provider.state.lock().unwrap().data, b"newer-edit");
    let j = j.lock().unwrap();
    assert_eq!(j.get(second.id).unwrap().state, UploadState::Uploaded);
    assert_eq!(j.read_working(working.id, 0, 100).unwrap(), b"newer-edit");
}
