//! Service/journal/credential recovery with synthetic providers; no cloud access.
#![allow(clippy::unwrap_used)]
use async_trait::async_trait;
use cirrove_auth::CredentialVault;
use cirrove_core::upload::{
    Reconciliation, RecoveryLocation, Result as UploadResult, UploadError, UploadIntent,
    UploadProgress, UploadProvider, UploadRequest, UploadStep,
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
struct LockProbe {
    journal: Mutex<Weak<Mutex<UploadJournal>>>,
    checkpoint_saves: Mutex<Vec<String>>,
}
impl LockProbe {
    fn attach(&self, journal: &Arc<Mutex<UploadJournal>>) {
        *self.journal.lock().unwrap() = Arc::downgrade(journal);
    }
    fn check(&self) {
        if let Some(journal) = self.journal.lock().unwrap().upgrade() {
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
        self.probe
            .checkpoint_saves
            .lock()
            .unwrap()
            .push(value.expose_secret().to_owned());
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
    reconciliation_had_checkpoint: bool,
    committed: Option<Node>,
    backup: Option<Node>,
}
struct Provider {
    state: Mutex<Remote>,
    operation_begins: Mutex<Vec<String>>,
    probe: Arc<LockProbe>,
    fail_offset_once: AtomicU64,
    lose_success: AtomicBool,
    deferred: AtomicBool,
    conflict_commit: AtomicBool,
    fail_begin_once: AtomicBool,
    prepare_once: AtomicBool,
    fail_inspect_once: AtomicBool,
    reject_inspection_session: AtomicBool,
    storage_inspection_error: AtomicU64,
    uncertain_inspect_when_committed: AtomicBool,
    restart_prepare_once: AtomicBool,
    complete_on_inspect_once: AtomicBool,
    require_reconcile_checkpoint: AtomicBool,
    mutation_free_begin: AtomicBool,
    pause_offset: AtomicU64,
    entered: Notify,
    handoff: AtomicBool,
    trash_handoff: AtomicBool,
    handoff_name: Mutex<Option<String>>,
    reserved_at_begin: AtomicBool,
    staged_commits: AtomicBool,
    staged_commit_steps: AtomicU64,
    commits: AtomicU64,
    inspection_timeout_ms: AtomicU64,
    inspection_pending: AtomicBool,
    reconciliation_uncertain: AtomicBool,
    commit_timeout_ms: AtomicU64,
    commit_delay_ms: AtomicU64,
    stream: AtomicBool,
    stream_staged: AtomicBool,
    streams: AtomicU64,
}
impl Provider {
    fn new(probe: Arc<LockProbe>) -> Self {
        Self {
            state: Mutex::new(Remote::default()),
            operation_begins: Mutex::new(Vec::new()),
            probe,
            fail_offset_once: AtomicU64::new(u64::MAX),
            lose_success: AtomicBool::new(false),
            deferred: AtomicBool::new(false),
            conflict_commit: AtomicBool::new(false),
            fail_begin_once: AtomicBool::new(false),
            prepare_once: AtomicBool::new(false),
            fail_inspect_once: AtomicBool::new(false),
            reject_inspection_session: AtomicBool::new(false),
            storage_inspection_error: AtomicU64::new(0),
            uncertain_inspect_when_committed: AtomicBool::new(false),
            restart_prepare_once: AtomicBool::new(false),
            complete_on_inspect_once: AtomicBool::new(false),
            require_reconcile_checkpoint: AtomicBool::new(false),
            mutation_free_begin: AtomicBool::new(false),
            pause_offset: AtomicU64::new(u64::MAX),
            entered: Notify::new(),
            handoff: AtomicBool::new(false),
            trash_handoff: AtomicBool::new(false),
            handoff_name: Mutex::new(None),
            reserved_at_begin: AtomicBool::new(false),
            staged_commits: AtomicBool::new(false),
            staged_commit_steps: AtomicU64::new(2),
            commits: AtomicU64::new(0),
            inspection_timeout_ms: AtomicU64::new(125_000),
            inspection_pending: AtomicBool::new(false),
            reconciliation_uncertain: AtomicBool::new(false),
            commit_timeout_ms: AtomicU64::new(125_000),
            commit_delay_ms: AtomicU64::new(0),
            stream: AtomicBool::new(false),
            stream_staged: AtomicBool::new(false),
            streams: AtomicU64::new(0),
        }
    }
    fn more(&self, request: &UploadRequest) -> UploadStep {
        let offset = self.state.lock().unwrap().data.len() as u64;
        if offset == request.size {
            return UploadStep::Commit(SecretString::from(SECRET));
        }
        if self.stream.load(Ordering::SeqCst) {
            return UploadStep::Stream(SecretString::from(SECRET));
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
            package: false,
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
    fn handoff_nodes(&self, request: &UploadRequest) -> (Node, Node) {
        let UploadIntent::Replace { item, .. } = &request.intent else {
            panic!("handoff fixture requires a replacement");
        };
        let recovery_name = self.handoff_name.lock().unwrap().clone().unwrap();
        let mut current = Self::node(request);
        current.id = format!("staged-{item}");
        let mut backup = Self::node(request);
        backup.name = recovery_name;
        if self.trash_handoff.load(Ordering::SeqCst) {
            backup.parent_id = Some("trash-root".into());
            backup.name = "Saved.txt".into();
        }
        backup.size = 3;
        backup.etag = Some("renamed-old".into());
        backup.content_version = Some("old-content".into());
        (current, backup)
    }
}
#[async_trait]
impl UploadProvider for Provider {
    fn inspection_timeout(&self, _: &UploadRequest) -> Duration {
        Duration::from_millis(self.inspection_timeout_ms.load(Ordering::SeqCst))
    }

    fn commit_timeout(&self, _: &UploadRequest) -> Duration {
        Duration::from_millis(self.commit_timeout_ms.load(Ordering::SeqCst))
    }

    async fn begin_upload_for_operation(
        &self,
        operation: &str,
        request: &UploadRequest,
        cancel: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        self.operation_begins.lock().unwrap().push(operation.into());
        self.begin_upload(request, cancel).await
    }

    fn begin_is_mutation_free_until_checkpoint(&self, _request: &UploadRequest) -> bool {
        self.mutation_free_begin.load(Ordering::SeqCst)
    }

    fn staged_recovery_location(
        &self,
        operation: &str,
        request: &UploadRequest,
    ) -> Option<RecoveryLocation> {
        let name = self.staged_recovery_name(operation, request)?;
        if self.trash_handoff.load(Ordering::SeqCst) {
            Some(RecoveryLocation::Trash {
                local_name: name,
                parent: "trash-root".into(),
            })
        } else {
            Some(RecoveryLocation::Sibling { name })
        }
    }

    fn staged_recovery_name(&self, operation: &str, request: &UploadRequest) -> Option<String> {
        if !self.handoff.load(Ordering::SeqCst)
            || !matches!(request.intent, UploadIntent::Replace { .. })
        {
            return None;
        }
        let name = format!("recovery-{operation}.txt");
        *self.handoff_name.lock().unwrap() = Some(name.clone());
        Some(name)
    }

    async fn begin_upload(
        &self,
        request: &UploadRequest,
        _: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        self.probe.check();
        if self.handoff.load(Ordering::SeqCst) {
            let journal = self.probe.journal.lock().unwrap().upgrade().unwrap();
            let reserved = journal
                .lock()
                .unwrap()
                .namespace_objects()
                .unwrap()
                .iter()
                .any(|o| o.unlinked && !o.remote_owned && o.node.id.starts_with("local-recovery-"));
            self.reserved_at_begin.store(reserved, Ordering::SeqCst);
            assert!(
                reserved,
                "provider request preceded the local ID reservation"
            );
        }
        {
            let mut state = self.state.lock().unwrap();
            state.begins += 1;
            state.data.clear();
            state.committed = None;
            state.backup = None;
        }
        if self.fail_begin_once.swap(false, Ordering::SeqCst) {
            return Err(ProviderError::Unavailable.into());
        }
        if self.prepare_once.swap(false, Ordering::SeqCst) {
            return Ok(UploadStep::Prepared(SecretString::from(SECRET)));
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
        match self.storage_inspection_error.load(Ordering::SeqCst) {
            1 => return Err(UploadError::Quota),
            2 => return Err(UploadError::InsufficientStorage),
            _ => {}
        }
        if self.reject_inspection_session.load(Ordering::SeqCst) {
            return Err(ProviderError::Authentication.into());
        }
        if self.inspection_pending.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        {
            let mut state = self.state.lock().unwrap();
            state.inspections += 1;
            if checkpoint.expose_secret() != SECRET {
                return Err(UploadError::CheckpointInvalid);
            }
            if state.committed.is_some()
                && self
                    .uncertain_inspect_when_committed
                    .swap(false, Ordering::SeqCst)
            {
                return Err(UploadError::Uncertain);
            }
            if state.committed.is_some() {
                return Err(UploadError::SessionGone);
            }
        }
        if self.fail_inspect_once.swap(false, Ordering::SeqCst) {
            return Err(UploadError::Uncertain);
        }
        if self.restart_prepare_once.swap(false, Ordering::SeqCst) {
            return Ok(UploadStep::Prepared(SecretString::from(SECRET)));
        }
        if self.complete_on_inspect_once.swap(false, Ordering::SeqCst) {
            let node = Self::node(request);
            let mut state = self.state.lock().unwrap();
            state.committed = Some(node);
            return Err(UploadError::SessionGone);
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
            if self.handoff.load(Ordering::SeqCst) {
                let (current, backup) = self.handoff_nodes(request);
                let mut state = self.state.lock().unwrap();
                state.committed = Some(current.clone());
                state.backup = Some(backup.clone());
                drop(state);
                if self.lose_success.swap(false, Ordering::SeqCst) {
                    return Err(UploadError::Uncertain);
                }
                return Ok(UploadStep::HandoffComplete { current, backup });
            }
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
    async fn upload_stream(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
        mut file: std::fs::File,
        _: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        self.probe.check();
        self.streams.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            self.probe
                .checkpoint_saves
                .lock()
                .unwrap()
                .last()
                .map(String::as_str),
            Some(checkpoint.expose_secret()),
            "stream started before its checkpoint was saved"
        );
        let mut data = Vec::new();
        file.read_to_end(&mut data).unwrap();
        assert_eq!(data.len() as u64, request.size);
        let node = Self::node(request);
        let mut state = self.state.lock().unwrap();
        state.data = data;
        if self.stream_staged.load(Ordering::SeqCst) {
            return Ok(UploadStep::Commit(SecretString::from(SECRET)));
        }
        state.committed = Some(node.clone());
        drop(state);
        if self.lose_success.swap(false, Ordering::SeqCst) {
            return Err(UploadError::Uncertain);
        }
        Ok(UploadStep::Complete(node))
    }
    async fn commit_upload(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
        _: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        self.probe.check();
        let delay = self.commit_delay_ms.load(Ordering::SeqCst);
        if delay == u64::MAX {
            std::future::pending::<()>().await;
        } else if delay > 0 {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
        if self.stream_staged.load(Ordering::SeqCst) {
            assert_eq!(
                self.probe
                    .checkpoint_saves
                    .lock()
                    .unwrap()
                    .last()
                    .map(String::as_str),
                Some(checkpoint.expose_secret()),
                "registration started before its receipt checkpoint was saved"
            );
            let node = Self::node(request);
            self.state.lock().unwrap().committed = Some(node.clone());
            if self.lose_success.swap(false, Ordering::SeqCst) {
                return Err(UploadError::Uncertain);
            }
            return Ok(UploadStep::Complete(node));
        }
        if self.staged_commits.load(Ordering::SeqCst) {
            assert_eq!(
                self.probe
                    .checkpoint_saves
                    .lock()
                    .unwrap()
                    .last()
                    .map(String::as_str),
                Some(checkpoint.expose_secret()),
                "the next commit ran before its checkpoint reached the vault"
            );
            let stage = self.commits.fetch_add(1, Ordering::SeqCst);
            let steps = self.staged_commit_steps.load(Ordering::SeqCst);
            let expected = if stage == 0 {
                SECRET.into()
            } else {
                format!("phase-{}", stage + 1)
            };
            if stage >= steps || checkpoint.expose_secret() != expected {
                return Err(UploadError::CheckpointInvalid);
            }
            if stage + 1 < steps {
                return Ok(UploadStep::Commit(SecretString::from(format!(
                    "phase-{}",
                    stage + 2
                ))));
            }
        }
        if self.conflict_commit.load(Ordering::SeqCst) {
            return Err(UploadError::Conflict);
        }
        if self.handoff.load(Ordering::SeqCst) {
            let (current, backup) = self.handoff_nodes(request);
            let mut state = self.state.lock().unwrap();
            state.committed = Some(current.clone());
            state.backup = Some(backup.clone());
            return Ok(UploadStep::HandoffComplete { current, backup });
        }
        let node = Self::node(request);
        self.state.lock().unwrap().committed = Some(node.clone());
        Ok(UploadStep::Complete(node))
    }
    async fn reconcile_upload(
        &self,
        request: &UploadRequest,
        checkpoint: Option<&SecretString>,
        _: &CancellationToken,
    ) -> UploadResult<Reconciliation> {
        self.probe.check();
        let mut state = self.state.lock().unwrap();
        state.reconciliations += 1;
        if self.reconciliation_uncertain.load(Ordering::SeqCst) {
            return Err(UploadError::Uncertain);
        }
        state.reconciliation_had_checkpoint =
            checkpoint.is_some_and(|value| value.expose_secret() == SECRET);
        if self.require_reconcile_checkpoint.load(Ordering::SeqCst)
            && !state.reconciliation_had_checkpoint
        {
            return Err(UploadError::CheckpointInvalid);
        }
        Ok(match &state.committed {
            Some(current)
                if state.backup.is_some()
                    && hex::encode(Sha256::digest(&state.data)) == request.sha256 =>
            {
                Reconciliation::HandoffCommitted {
                    current: current.clone(),
                    backup: state.backup.clone().unwrap(),
                }
            }
            Some(node) if hex::encode(Sha256::digest(&state.data)) == request.sha256 => {
                Reconciliation::Committed(node.clone())
            }
            Some(_) => Reconciliation::Conflict,
            None => Reconciliation::Uncommitted,
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duplicate_upload_requests_keep_their_distinct_operation_ids() {
    let temp = tempfile::tempdir().unwrap();
    let (probe, provider, vault) = fixture();
    let journal = journal(&temp.path().join("journal"), &probe);
    let first = enqueue(&journal, "Saved.txt");
    let second = enqueue(&journal, "Saved.txt");
    assert_ne!(first, second);
    let worker = TransferWorker::new(
        journal.clone(),
        provider.clone(),
        vault,
        CancellationToken::new(),
    );
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        UploadState::Uploaded
    );
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        UploadState::Uploaded
    );
    assert_eq!(
        *provider.operation_begins.lock().unwrap(),
        vec![first.to_string(), second.to_string()]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_commit_deadline_preserves_checkpoint_and_payload_for_recovery() {
    let temp = tempfile::tempdir().unwrap();
    let (probe, provider, vault) = fixture();
    let journal = journal(&temp.path().join("journal"), &probe);
    let id = enqueue(&journal, "Saved.txt");
    provider.deferred.store(true, Ordering::SeqCst);
    provider.commit_timeout_ms.store(10, Ordering::SeqCst);
    provider.commit_delay_ms.store(u64::MAX, Ordering::SeqCst);
    let worker = TransferWorker::new(
        journal.clone(),
        provider.clone(),
        vault,
        CancellationToken::new(),
    );
    let result = tokio::time::timeout(Duration::from_secs(2), worker.run_once())
        .await
        .expect("worker ignored the provider commit deadline")
        .unwrap()
        .unwrap();
    assert_eq!(result.state, UploadState::VerifyRequired);
    assert!(
        journal
            .lock()
            .unwrap()
            .get(id)
            .unwrap()
            .session_key
            .is_some()
    );
    assert!(provider.state.lock().unwrap().committed.is_none());
    assert_local(&journal, id);
    provider.commit_delay_ms.store(0, Ordering::SeqCst);
    journal.lock().unwrap().request_retry(id).unwrap();
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        UploadState::Uploaded
    );
    assert_eq!(provider.state.lock().unwrap().begins, 1);
    assert_local(&journal, id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_inspection_deadline_preserves_uncertain_upload_without_replay() {
    let temp = tempfile::tempdir().unwrap();
    let (probe, provider, vault) = fixture();
    let journal = journal(&temp.path().join("journal"), &probe);
    let id = enqueue(&journal, "Saved.txt");
    provider.deferred.store(true, Ordering::SeqCst);
    provider.commit_timeout_ms.store(10, Ordering::SeqCst);
    provider.commit_delay_ms.store(u64::MAX, Ordering::SeqCst);
    let worker = TransferWorker::new(
        journal.clone(),
        provider.clone(),
        vault,
        CancellationToken::new(),
    );
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        UploadState::VerifyRequired
    );
    let checkpoint = journal.lock().unwrap().get(id).unwrap().session_key;
    assert!(checkpoint.is_some());
    provider.commit_delay_ms.store(0, Ordering::SeqCst);
    provider.inspection_timeout_ms.store(10, Ordering::SeqCst);
    provider.inspection_pending.store(true, Ordering::SeqCst);
    provider
        .reconciliation_uncertain
        .store(true, Ordering::SeqCst);
    journal.lock().unwrap().request_retry(id).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(2), worker.run_once())
        .await
        .expect("worker ignored the provider inspection deadline")
        .unwrap()
        .unwrap();
    assert_eq!(result.state, UploadState::VerifyRequired);
    assert_eq!(
        journal.lock().unwrap().get(id).unwrap().session_key,
        checkpoint
    );
    assert_eq!(provider.state.lock().unwrap().begins, 1);
    assert!(provider.state.lock().unwrap().committed.is_none());
    assert_local(&journal, id);
    provider.inspection_pending.store(false, Ordering::SeqCst);
    journal.lock().unwrap().request_retry(id).unwrap();
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        UploadState::Uploaded
    );
    assert_eq!(provider.state.lock().unwrap().begins, 1);
    assert_local(&journal, id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn staged_replacement_persists_a_checkpoint_before_each_commit() {
    for steps in [2, 4] {
        let temp = tempfile::tempdir().unwrap();
        let (probe, provider, vault) = fixture();
        let journal = journal(&temp.path().join("journal"), &probe);
        let id = enqueue(&journal, "Saved.txt");
        provider.deferred.store(true, Ordering::SeqCst);
        provider.staged_commits.store(true, Ordering::SeqCst);
        provider.staged_commit_steps.store(steps, Ordering::SeqCst);
        let worker = TransferWorker::new(
            journal.clone(),
            provider.clone(),
            vault.clone(),
            CancellationToken::new(),
        );
        assert_eq!(
            worker.run_once().await.unwrap().unwrap().state,
            UploadState::Uploaded
        );
        assert_eq!(provider.commits.load(Ordering::SeqCst), steps);
        assert_eq!(
            journal.lock().unwrap().get(id).unwrap().state,
            UploadState::Uploaded
        );
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
    enqueue_data(j, name, DATA)
}
fn enqueue_data(j: &Arc<Mutex<UploadJournal>>, name: &str, data: &[u8]) -> uuid::Uuid {
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
            data,
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
async fn staged_two_id_receipt_is_reserved_before_network_and_reconciled_after_loss() {
    staged_two_id_receipt(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn trash_backed_two_id_receipt_survives_lost_response() {
    staged_two_id_receipt(true).await;
}

async fn staged_two_id_receipt(trash: bool) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let (probe, provider, vault) = fixture();
    provider.handoff.store(true, Ordering::SeqCst);
    provider.trash_handoff.store(trash, Ordering::SeqCst);
    provider.lose_success.store(true, Ordering::SeqCst);
    let first_journal = journal(&root, &probe);
    let upload = {
        let mut journal = first_journal.lock().unwrap();
        let working = journal
            .create_working(
                Scope {
                    account: "fixture".into(),
                    provider: "fixture".into(),
                    collection: "drive".into(),
                },
                Node {
                    package: false,
                    id: "old-item".into(),
                    parent_id: Some("root".into()),
                    name: "Saved.txt".into(),
                    kind: NodeKind::File,
                    size: 3,
                    modified_unix: 0,
                    etag: Some("old-tag".into()),
                    content_version: Some("old-content".into()),
                    target: None,
                },
                false,
                b"old".as_slice(),
            )
            .unwrap();
        journal.write_working(working.id, 0, DATA).unwrap();
        journal.seal_working(working.id).unwrap().unwrap()
    };
    // Simulate process death after claiming the upload but before the worker
    // could reserve either identity or contact the provider.
    first_journal.lock().unwrap().claim_next().unwrap().unwrap();
    drop(first_journal);
    let journal = journal(&root, &probe);
    let worker = TransferWorker::new(
        journal.clone(),
        provider.clone(),
        vault.clone(),
        CancellationToken::new(),
    );
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        UploadState::Pending
    );
    assert_eq!(provider.state.lock().unwrap().begins, 0);
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        UploadState::VerifyRequired
    );
    assert!(provider.reserved_at_begin.load(Ordering::SeqCst));
    drop(worker);
    drop(journal);
    let journal = self::journal(&root, &probe);
    let worker = TransferWorker::new(
        journal.clone(),
        provider.clone(),
        vault,
        CancellationToken::new(),
    );
    {
        let mut journal = journal.lock().unwrap();
        journal.request_retry(upload.id).unwrap();
    }
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        UploadState::Uploaded
    );
    let journal = journal.lock().unwrap();
    let current = journal
        .namespace_by_remote(&upload.scope, "staged-old-item")
        .unwrap()
        .unwrap();
    let backup = journal
        .namespace_by_remote(&upload.scope, "old-item")
        .unwrap()
        .unwrap();
    assert_eq!(current.node.id, "old-item");
    assert!(backup.unlinked);
    assert!(backup.remote_owned);
    if trash {
        assert_eq!(
            backup.remote.as_ref().unwrap().parent_id.as_deref(),
            Some("trash-root")
        );
        assert_eq!(backup.remote.as_ref().unwrap().name, "Saved.txt");
        assert_eq!(backup.node.name, format!("recovery-{}.txt", upload.id));
    }
    assert_eq!(journal.get(upload.id).unwrap().state, UploadState::Uploaded);
    assert_eq!(provider.state.lock().unwrap().begins, 1);
    assert_eq!(provider.state.lock().unwrap().reconciliations, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prepared_identity_survives_a_lost_session_start_response() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let (probe, p, v) = fixture();
    let j = journal(&root, &probe);
    let id = enqueue(&j, "Saved.txt");
    p.prepare_once.store(true, Ordering::SeqCst);
    p.fail_inspect_once.store(true, Ordering::SeqCst);

    let worker = TransferWorker::new(j.clone(), p.clone(), v.clone(), CancellationToken::new());
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        UploadState::VerifyRequired
    );
    let record = j.lock().unwrap().get(id).unwrap();
    assert_eq!(record.transferred_bytes, 0);
    assert!(record.session_key.is_some());
    assert_eq!(p.state.lock().unwrap().begins, 1);
    assert_eq!(p.state.lock().unwrap().inspections, 1);
    assert!(p.state.lock().unwrap().offsets.is_empty());
    assert_eq!(
        v.values
            .lock()
            .unwrap()
            .get(&format!("upload/{id}"))
            .unwrap()
            .expose_secret(),
        SECRET
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
    let state = p.state.lock().unwrap();
    assert_eq!(state.begins, 1);
    assert_eq!(state.inspections, 2);
    assert_eq!(state.offsets, [0, 4, 8]);
    drop(state);
    assert!(v.values.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prepared_identity_is_available_when_a_lost_session_is_reconciled() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let (probe, p, v) = fixture();
    let j = journal(&root, &probe);
    let id = enqueue_data(&j, "Empty.txt", &[]);
    p.prepare_once.store(true, Ordering::SeqCst);
    p.complete_on_inspect_once.store(true, Ordering::SeqCst);
    p.require_reconcile_checkpoint.store(true, Ordering::SeqCst);

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
    let state = p.state.lock().unwrap();
    assert_eq!(state.begins, 1);
    assert_eq!(state.inspections, 2);
    assert_eq!(state.reconciliations, 1);
    assert!(state.reconciliation_had_checkpoint);
    assert!(state.offsets.is_empty());
    drop(state);
    assert!(v.values.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verification_can_restart_a_gone_session_with_the_same_prepared_identity() {
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
    p.restart_prepare_once.store(true, Ordering::SeqCst);
    j.lock().unwrap().request_retry(id).unwrap();
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        UploadState::Uploaded
    );
    let state = p.state.lock().unwrap();
    assert_eq!(state.begins, 1);
    assert_eq!(state.inspections, 2);
    assert_eq!(state.offsets, [0, 4, 4, 8]);
    assert_eq!(state.data, DATA);
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
async fn sealed_stream_saves_checkpoint_and_reconciles_lost_receipt_without_resending() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let (probe, provider, vault) = fixture();
    provider.stream.store(true, Ordering::SeqCst);
    provider.lose_success.store(true, Ordering::SeqCst);
    let first = journal(&root, &probe);
    let id = enqueue(&first, "stream.bin");
    let worker = TransferWorker::new(
        first.clone(),
        provider.clone(),
        vault.clone(),
        CancellationToken::new(),
    );
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        UploadState::VerifyRequired
    );
    assert_eq!(provider.streams.load(Ordering::SeqCst), 1);
    drop(worker);
    drop(first);
    let restarted = journal(&root, &probe);
    restarted.lock().unwrap().request_retry(id).unwrap();
    let worker = TransferWorker::new(
        restarted.clone(),
        provider.clone(),
        vault,
        CancellationToken::new(),
    );
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        UploadState::Uploaded
    );
    assert_eq!(provider.streams.load(Ordering::SeqCst), 1);
    assert_eq!(provider.state.lock().unwrap().reconciliations, 1);
    assert_local(&restarted, id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streamed_content_receipt_is_persisted_before_registration_and_reconciles() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let (probe, provider, vault) = fixture();
    provider.stream.store(true, Ordering::SeqCst);
    provider.stream_staged.store(true, Ordering::SeqCst);
    provider.lose_success.store(true, Ordering::SeqCst);
    let first = journal(&root, &probe);
    let id = enqueue(&first, "streamed-phase.bin");
    let worker = TransferWorker::new(
        first.clone(),
        provider.clone(),
        vault.clone(),
        CancellationToken::new(),
    );
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        UploadState::VerifyRequired
    );
    assert_eq!(provider.streams.load(Ordering::SeqCst), 1);
    drop(worker);
    drop(first);
    let restarted = journal(&root, &probe);
    restarted.lock().unwrap().request_retry(id).unwrap();
    let worker = TransferWorker::new(
        restarted.clone(),
        provider.clone(),
        vault,
        CancellationToken::new(),
    );
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        UploadState::Uploaded
    );
    assert_eq!(provider.streams.load(Ordering::SeqCst), 1);
    assert_eq!(provider.state.lock().unwrap().reconciliations, 1);
    assert_local(&restarted, id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_uncertain_committed_session_is_reconciled_without_uploading_twice() {
    let temp = tempfile::tempdir().unwrap();
    let (probe, provider, vault) = fixture();
    let journal = journal(&temp.path().join("journal"), &probe);
    let id = enqueue(&journal, "Saved.txt");
    provider.lose_success.store(true, Ordering::SeqCst);
    let worker = TransferWorker::new(
        journal.clone(),
        provider.clone(),
        vault,
        CancellationToken::new(),
    );
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        UploadState::VerifyRequired
    );
    journal.lock().unwrap().request_retry(id).unwrap();
    provider
        .uncertain_inspect_when_committed
        .store(true, Ordering::SeqCst);
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        UploadState::Uploaded
    );
    let state = provider.state.lock().unwrap();
    assert_eq!(state.begins, 1);
    assert_eq!(state.reconciliations, 1);
    assert_eq!(state.offsets, [0, 4, 8]);
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
async fn mutation_free_preflight_restarts_only_when_no_checkpoint_was_ever_recorded() {
    let temp = tempfile::tempdir().unwrap();
    let (probe, provider, vault) = fixture();
    let journal = journal(&temp.path().join("never-recorded"), &probe);
    let id = enqueue(&journal, "New.txt");
    provider.mutation_free_begin.store(true, Ordering::SeqCst);
    provider
        .require_reconcile_checkpoint
        .store(true, Ordering::SeqCst);
    provider.prepare_once.store(true, Ordering::SeqCst);
    vault.fail_save.store(true, Ordering::SeqCst);
    let worker = TransferWorker::new(
        journal.clone(),
        provider.clone(),
        vault.clone(),
        CancellationToken::new(),
    );
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        UploadState::VerifyRequired
    );
    assert!(
        journal
            .lock()
            .unwrap()
            .get(id)
            .unwrap()
            .session_key
            .is_none()
    );
    vault.fail_save.store(false, Ordering::SeqCst);
    journal.lock().unwrap().request_retry(id).unwrap();
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        UploadState::Pending
    );
    assert_eq!(provider.state.lock().unwrap().reconciliations, 0);
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        UploadState::Uploaded
    );

    let (probe, provider, vault) = fixture();
    let journal = self::journal(&temp.path().join("checkpoint-lost"), &probe);
    let id = enqueue(&journal, "Old.txt");
    provider.mutation_free_begin.store(true, Ordering::SeqCst);
    provider
        .require_reconcile_checkpoint
        .store(true, Ordering::SeqCst);
    provider.prepare_once.store(true, Ordering::SeqCst);
    provider.fail_inspect_once.store(true, Ordering::SeqCst);
    let worker = TransferWorker::new(
        journal.clone(),
        provider.clone(),
        vault.clone(),
        CancellationToken::new(),
    );
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        UploadState::VerifyRequired
    );
    assert!(
        journal
            .lock()
            .unwrap()
            .get(id)
            .unwrap()
            .session_key
            .is_some()
    );
    vault.values.lock().unwrap().clear();
    journal.lock().unwrap().request_retry(id).unwrap();
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        UploadState::VerifyRequired
    );
    assert_eq!(provider.state.lock().unwrap().begins, 1);
    assert_eq!(provider.state.lock().unwrap().reconciliations, 1);
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
            package: false,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejected_session_keeps_uncertain_commit_and_local_bytes_until_verified_after_sign_in() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let (probe, provider, vault) = fixture();
    let j = journal(&root, &probe);
    let id = enqueue(&j, "Saved.txt");
    provider.lose_success.store(true, Ordering::SeqCst);
    let worker = TransferWorker::new(
        j.clone(),
        provider.clone(),
        vault.clone(),
        CancellationToken::new(),
    );
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        UploadState::VerifyRequired
    );
    let checkpoint = j.lock().unwrap().get(id).unwrap().session_key;
    assert!(checkpoint.is_some());
    j.lock().unwrap().request_retry(id).unwrap();
    provider
        .reject_inspection_session
        .store(true, Ordering::SeqCst);
    let result = worker.run_once().await.unwrap().unwrap();
    assert_eq!(result.state, UploadState::Failed);
    assert_eq!(
        result.issue.as_deref(),
        Some(ProviderError::Authentication.to_string().as_str())
    );
    assert_local(&j, id);
    assert_eq!(j.lock().unwrap().get(id).unwrap().session_key, checkpoint);
    assert!(vault.load(&format!("upload/{id}")).await.unwrap().is_some());
    assert!(
        worker.run_once().await.unwrap().is_none(),
        "rejected sessions must not spin"
    );
    drop(worker);
    drop(j);
    let j = journal(&root, &probe);
    assert_local(&j, id);
    j.lock().unwrap().request_retry(id).unwrap();
    assert_eq!(j.lock().unwrap().get(id).unwrap().session_key, checkpoint);
    provider
        .reject_inspection_session
        .store(false, Ordering::SeqCst);
    let worker = TransferWorker::new(
        j.clone(),
        provider.clone(),
        vault.clone(),
        CancellationToken::new(),
    );
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        UploadState::Uploaded
    );
    assert_local(&j, id);
    assert!(vault.load(&format!("upload/{id}")).await.unwrap().is_none());
    let state = provider.state.lock().unwrap();
    assert_eq!(state.begins, 1);
    assert_eq!(state.offsets, [0, 4, 8]);
    assert_eq!(state.reconciliations, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn storage_refusal_keeps_checkpoint_and_exports_bytes_until_explicit_verified_retry() {
    for (mode, error) in [
        (1, UploadError::Quota),
        (2, UploadError::InsufficientStorage),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("journal");
        let (probe, provider, vault) = fixture();
        let j = journal(&root, &probe);
        let id = enqueue(&j, "Saved.txt");
        provider.lose_success.store(true, Ordering::SeqCst);
        let worker = TransferWorker::new(
            j.clone(),
            provider.clone(),
            vault.clone(),
            CancellationToken::new(),
        );
        assert_eq!(
            worker.run_once().await.unwrap().unwrap().state,
            UploadState::VerifyRequired
        );
        let checkpoint = j.lock().unwrap().get(id).unwrap().session_key;
        assert!(checkpoint.is_some());
        j.lock().unwrap().request_retry(id).unwrap();
        provider
            .storage_inspection_error
            .store(mode, Ordering::SeqCst);
        let result = worker.run_once().await.unwrap().unwrap();
        assert_eq!(result.state, UploadState::Failed);
        assert_eq!(result.issue.as_deref(), Some(error.to_string().as_str()));
        assert_eq!(j.lock().unwrap().get(id).unwrap().session_key, checkpoint);
        assert!(vault.load(&format!("upload/{id}")).await.unwrap().is_some());
        assert!(worker.run_once().await.unwrap().is_none());
        let source = j.lock().unwrap().local_export_source(id).unwrap();
        let export = temp.path().join("recovered.txt");
        let receipt = source
            .copy_to(&export, &CancellationToken::new(), |_| {})
            .unwrap();
        assert_eq!(receipt.operation, id);
        assert_eq!(std::fs::read(&export).unwrap(), DATA);
        assert_eq!(
            j.lock().unwrap().get(id).unwrap().state,
            UploadState::Failed
        );
        drop(worker);
        drop(j);
        let j = journal(&root, &probe);
        assert_local(&j, id);
        let worker = TransferWorker::new(
            j.clone(),
            provider.clone(),
            vault.clone(),
            CancellationToken::new(),
        );
        provider.storage_inspection_error.store(0, Ordering::SeqCst);
        assert!(
            worker.run_once().await.unwrap().is_none(),
            "storage becoming available is not user consent to retry"
        );
        j.lock().unwrap().request_retry(id).unwrap();
        assert_eq!(
            worker.run_once().await.unwrap().unwrap().state,
            UploadState::Uploaded
        );
        assert!(vault.load(&format!("upload/{id}")).await.unwrap().is_none());
        assert_local(&j, id);
        let state = provider.state.lock().unwrap();
        assert_eq!(state.begins, 1);
        assert_eq!(state.offsets, [0, 4, 8]);
        assert_eq!(state.reconciliations, 1);
    }
}
