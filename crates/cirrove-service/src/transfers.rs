//! Journal-driven transfer execution. Neither credential operations nor provider
//! requests run while the SQLite/spool mutex is held. Not enabled by the daemon yet.
use crate::journal::{JournalError, UploadJournal, UploadRecord, UploadState};
use cirrove_auth::CredentialVault;
use cirrove_core::upload::{
    Reconciliation, UploadError, UploadProvider, UploadRequest, UploadStep,
};
use cirrove_core::{CancellationToken, ProviderError};
use secrecy::SecretString;
use std::{
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum TransferError {
    #[error(transparent)]
    Journal(#[from] JournalError),
    #[error(transparent)]
    Provider(#[from] UploadError),
    #[error("upload credential storage is unavailable")]
    Vault,
    #[error("local upload worker is unavailable")]
    Worker,
}
type Result<T> = std::result::Result<T, TransferError>;
#[derive(Debug)]
pub struct TransferResult {
    pub id: Uuid,
    pub state: UploadState,
    pub issue: Option<String>,
}

pub struct TransferWorker {
    journal: Arc<Mutex<UploadJournal>>,
    provider: Arc<dyn UploadProvider>,
    vault: Arc<dyn CredentialVault>,
    cancel: CancellationToken,
}
impl TransferWorker {
    pub fn new(
        journal: Arc<Mutex<UploadJournal>>,
        provider: Arc<dyn UploadProvider>,
        vault: Arc<dyn CredentialVault>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            journal,
            provider,
            vault,
            cancel,
        }
    }
    async fn local<T: Send + 'static>(
        &self,
        action: impl FnOnce(&mut UploadJournal) -> std::result::Result<T, JournalError> + Send + 'static,
    ) -> Result<T> {
        let journal = self.journal.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = journal.lock().map_err(|_| JournalError::Storage)?;
            action(&mut guard)
        })
        .await
        .map_err(|_| TransferError::Worker)?
        .map_err(Into::into)
    }
    async fn remote<T>(
        &self,
        deadline: Duration,
        call: impl Future<Output = cirrove_core::upload::Result<T>>,
    ) -> Result<T> {
        tokio::select! {
            biased;
            _ = self.cancel.cancelled()=>Err(UploadError::Uncertain.into()),
            result=tokio::time::timeout(deadline,call)=>result.map_err(|_|TransferError::Provider(UploadError::Uncertain))?.map_err(Into::into),
        }
    }
    async fn credential<T>(&self, call: impl Future<Output = anyhow::Result<T>>) -> Result<T> {
        tokio::select! {
            biased;
            _ = self.cancel.cancelled()=>Err(UploadError::Uncertain.into()),
            result=tokio::time::timeout(Duration::from_secs(30),call)=>result.map_err(|_|TransferError::Vault)?.map_err(|_|TransferError::Vault),
        }
    }
    fn vault_key(id: Uuid) -> String {
        format!("upload/{id}")
    }
    async fn load_checkpoint(&self, id: Uuid) -> Result<Option<SecretString>> {
        self.credential(self.vault.load(&Self::vault_key(id))).await
    }
    async fn checkpoint(
        &self,
        record: &UploadRecord,
        value: SecretString,
        offset: u64,
    ) -> Result<()> {
        let id = record.id;
        let attempt = record.attempt.ok_or(TransferError::Worker)?;
        // A deterministic per-operation key also recovers the narrow window
        // between saving a session credential and committing its journal reference.
        self.credential(self.vault.save(&Self::vault_key(id), value))
            .await?;
        self.local(move |j| j.record_session(id, attempt, id, offset))
            .await
    }
    async fn clean_checkpoint(&self, id: Uuid) {
        if self.cancel.is_cancelled() {
            return;
        }
        if self
            .credential(self.vault.remove(&Self::vault_key(id)))
            .await
            .is_err()
            && !self.cancel.is_cancelled()
        {
            tracing::warn!("upload session credential is awaiting cleanup");
        }
    }
    /// Performs one eligible operation. Retry deadlines are durable and per file,
    /// so an unavailable transfer cannot monopolize the rest of the queue.
    pub async fn run_once(&self) -> Result<Option<TransferResult>> {
        if self.cancel.is_cancelled() {
            return Ok(None);
        }
        let record = self
            .local(|j| match j.claim_next_verification()? {
                Some(r) => Ok(Some(r)),
                None => j.claim_next(),
            })
            .await?;
        let Some(record) = record else {
            return Ok(None);
        };
        let result = self.execute(&record).await;
        let id = record.id;
        match result {
            Ok(state) => Ok(Some(TransferResult {
                id,
                state,
                issue: None,
            })),
            Err(error) => {
                let state = match &error {
                    TransferError::Provider(UploadError::Conflict) => UploadState::Conflict,
                    TransferError::Provider(
                        UploadError::Invalid
                        | UploadError::Unsupported(_)
                        | UploadError::Quota
                        | UploadError::InsufficientStorage
                        | UploadError::Provider(
                            ProviderError::Permission
                            | ProviderError::NotFound
                            | ProviderError::Authentication,
                        ),
                    ) => UploadState::Failed,
                    TransferError::Journal(JournalError::Corrupt) => UploadState::Failed,
                    _ => UploadState::VerifyRequired,
                };
                let delay = match &error {
                    TransferError::Provider(UploadError::Provider(ProviderError::Throttled(
                        delay,
                    ))) => delay.saturating_add(Duration::from_secs(1)),
                    _ => Duration::from_secs(2u64.pow(record.failed_attempts.min(6) + 1)),
                };
                let attempt = record.attempt.ok_or(TransferError::Worker)?;
                self.local(move |j| j.defer_attempt(id, attempt, state, delay))
                    .await?;
                Ok(Some(TransferResult {
                    id,
                    state,
                    issue: Some(error.to_string()),
                }))
            }
        }
    }
    async fn execute(&self, record: &UploadRecord) -> Result<UploadState> {
        let id = record.id;
        let operation = id.to_string();
        let attempt = record.attempt.ok_or(TransferError::Worker)?;
        let request = UploadRequest {
            representation: record.representation.clone(),
            scope: record.scope.clone(),
            intent: record.intent.clone(),
            size: record.size,
            sha256: record.sha256.clone(),
        };
        if let Some(location) = self.provider.staged_recovery_location(&operation, &request) {
            // The provider is still untouched. Reserve the old-ID owner before
            // begin, inspect or reconcile can make a remote request.
            self.local(move |j| j.reserve_identity_handoff(id, attempt, location))
                .await?;
        }
        let mut saved_checkpoint = None;
        // Track provenance, not journal state: only a direct fresh begin may
        // authorize the one-shot allocation callback in this execution.
        let mut allocation_allowed = false;
        let mut step = if record.state == UploadState::Verifying {
            let checkpoint = self
                .load_checkpoint(record.session_key.unwrap_or(id))
                .await?;
            match checkpoint {
                Some(checkpoint) => {
                    saved_checkpoint = Some(checkpoint.clone());
                    match self
                        .remote(
                            self.provider.inspection_timeout(&request),
                            self.provider.inspect_upload_for_operation(
                                &operation,
                                &request,
                                &checkpoint,
                                &self.cancel,
                            ),
                        )
                        .await
                    {
                        Ok(step) => Some(step),
                        // A saved session can report a stale success receipt
                        // after the file has already advanced. Exact-ID and
                        // digest reconciliation decides whether it committed;
                        // an uncertain inspection must never trigger a blind
                        // second upload.
                        Err(TransferError::Provider(
                            UploadError::SessionGone
                            | UploadError::CheckpointInvalid
                            | UploadError::Uncertain,
                        )) => None,
                        Err(error) => return Err(error),
                    }
                }
                None if record.session_key.is_none()
                    && self
                        .provider
                        .begin_is_mutation_free_until_checkpoint(&request) =>
                {
                    // This provider's begin returned no remote side effect, and
                    // the journal never confirmed a checkpoint. A checkpoint
                    // saved just before a failed journal update also precedes
                    // every remote mutation. A *recorded* key that later went
                    // missing must instead remain uncertain.
                    self.clean_checkpoint(id).await;
                    self.local(move |j| {
                        j.stop_attempt(id, attempt, UploadState::VerifyRequired)?;
                        j.retry_verified_uncommitted(id)
                    })
                    .await?;
                    return Ok(UploadState::Pending);
                }
                None => None,
            }
        } else {
            let next = if self.provider.requires_begin_payload(&request) {
                let file = self.local(move |j| j.payload(id)).await?;
                self.remote(
                    Duration::from_secs(125),
                    self.provider.begin_upload_from_payload_for_operation(
                        &operation,
                        &request,
                        file,
                        &self.cancel,
                    ),
                )
                .await?
            } else {
                self.remote(
                    Duration::from_secs(125),
                    self.provider
                        .begin_upload_for_operation(&operation, &request, &self.cancel),
                )
                .await?
            };
            allocation_allowed = matches!(next, UploadStep::Allocate(_));
            Some(next)
        };
        if step.is_none() {
            match self
                .remote(
                    Duration::from_secs(15 * 60),
                    self.provider.reconcile_upload_for_operation(
                        &operation,
                        &request,
                        saved_checkpoint.as_ref(),
                        &self.cancel,
                    ),
                )
                .await?
            {
                Reconciliation::PackageHandoffCommitted(receipt) => {
                    step = Some(UploadStep::PackageHandoffComplete(receipt));
                }
                Reconciliation::PackageCommitted(receipt) => {
                    step = Some(UploadStep::PackageComplete(receipt))
                }
                Reconciliation::Committed(node) => step = Some(UploadStep::Complete(node)),
                Reconciliation::HandoffCommitted { current, backup } => {
                    step = Some(UploadStep::HandoffComplete { current, backup });
                }
                Reconciliation::Conflict => return Err(UploadError::Conflict.into()),
                Reconciliation::Uncommitted => {
                    // Keep the local snapshot. A new attempt will get a fresh
                    // remote session and independently enforce its preconditions.
                    // Clean while this attempt still owns the operation. Making
                    // it pending first could delete a new worker's checkpoint.
                    self.clean_checkpoint(id).await;
                    self.local(move |j| {
                        j.stop_attempt(id, attempt, UploadState::VerifyRequired)?;
                        j.retry_verified_uncommitted(id)
                    })
                    .await?;
                    return Ok(UploadState::Pending);
                }
            }
        }
        let mut step = step.ok_or(TransferError::Worker)?;
        // A verifier may replace one definitely gone transport session while
        // retaining the provider identity saved inside its prepared checkpoint.
        // One transition per run bounds a provider that returns Prepared again.
        let mut prepared_allowed = true;
        // A staged provider may need more than one externally visible commit
        // (for example, moving the old iCloud ID to recovery before installing
        // the new ID). Persist each phase checkpoint before its next request.
        let mut commit_steps = 0u8;
        let mut payload = None;
        loop {
            if self.cancel.is_cancelled() {
                return Err(UploadError::Uncertain.into());
            }
            step = match step {
                UploadStep::Allocate(checkpoint) if allocation_allowed => {
                    allocation_allowed = false;
                    prepared_allowed = false;
                    self.checkpoint(record, checkpoint.clone(), 0).await?;
                    if self.cancel.is_cancelled() {
                        return Err(UploadError::Uncertain.into());
                    }
                    let next = self
                        .remote(
                            Duration::from_secs(125),
                            self.provider.allocate_upload_for_operation(
                                &operation,
                                &request,
                                &checkpoint,
                                &self.cancel,
                            ),
                        )
                        .await?;
                    if matches!(next, UploadStep::Allocate(_) | UploadStep::Prepared(_)) {
                        return Err(UploadError::Uncertain.into());
                    }
                    next
                }
                UploadStep::Allocate(_) => return Err(UploadError::Uncertain.into()),
                UploadStep::Prepared(checkpoint) if prepared_allowed => {
                    prepared_allowed = false;
                    self.checkpoint(record, checkpoint.clone(), 0).await?;
                    self.remote(
                        self.provider.inspection_timeout(&request),
                        self.provider.inspect_upload_for_operation(
                            &operation,
                            &request,
                            &checkpoint,
                            &self.cancel,
                        ),
                    )
                    .await?
                }
                UploadStep::Prepared(_) => return Err(UploadError::Invalid.into()),
                UploadStep::PackageHandoffComplete(receipt) => {
                    self.local(move |j| j.acknowledge_package_handoff(id, attempt, *receipt))
                        .await?;
                    self.clean_checkpoint(id).await;
                    return Ok(UploadState::Uploaded);
                }
                UploadStep::PackageComplete(receipt) => {
                    self.local(move |j| j.acknowledge_package(id, attempt, receipt))
                        .await?;
                    self.clean_checkpoint(id).await;
                    return Ok(UploadState::Uploaded);
                }
                UploadStep::Complete(node) => {
                    self.local(move |j| j.acknowledge(id, attempt, node))
                        .await?;
                    self.clean_checkpoint(id).await;
                    // Payload retention is a separate policy decision after the
                    // durable receipt; this worker never deletes an edited file.
                    return Ok(UploadState::Uploaded);
                }
                UploadStep::HandoffComplete { current, backup } => {
                    self.local(move |j| {
                        j.acknowledge_identity_handoff(id, attempt, current, backup)
                    })
                    .await?;
                    self.clean_checkpoint(id).await;
                    return Ok(UploadState::Uploaded);
                }
                UploadStep::Commit(checkpoint) => {
                    prepared_allowed = false;
                    commit_steps = commit_steps.saturating_add(1);
                    if commit_steps > 4 {
                        return Err(UploadError::Invalid.into());
                    }
                    self.checkpoint(record, checkpoint.clone(), request.size)
                        .await?;
                    let next = self
                        .remote(
                            self.provider.commit_timeout(&request),
                            self.provider.commit_upload_for_operation(
                                &operation,
                                &request,
                                &checkpoint,
                                &self.cancel,
                            ),
                        )
                        .await?;
                    if !matches!(
                        next,
                        UploadStep::Commit(_)
                            | UploadStep::Complete(_)
                            | UploadStep::PackageComplete(_)
                            | UploadStep::PackageHandoffComplete(_)
                            | UploadStep::HandoffComplete { .. }
                    ) {
                        return Err(UploadError::Uncertain.into());
                    }
                    next
                }
                UploadStep::Continue(progress) => {
                    prepared_allowed = false;
                    if progress.length == 0
                        || progress.length > 16 * 1024 * 1024
                        || progress.offset >= request.size
                        || progress.length as u64 > request.size - progress.offset
                    {
                        return Err(UploadError::Invalid.into());
                    }
                    self.checkpoint(record, progress.checkpoint.clone(), progress.offset)
                        .await?;
                    if payload.is_none() {
                        payload = Some(tokio::fs::File::from_std(
                            self.local(move |j| j.payload(id)).await?,
                        ));
                    }
                    let file = payload.as_mut().ok_or(TransferError::Worker)?;
                    file.seek(std::io::SeekFrom::Start(progress.offset))
                        .await
                        .map_err(|_| TransferError::Worker)?;
                    let mut bytes = vec![0; progress.length as usize];
                    file.read_exact(&mut bytes)
                        .await
                        .map_err(|_| TransferError::Worker)?;
                    let next = self
                        .remote(
                            Duration::from_secs(125),
                            self.provider.upload_part_for_operation(
                                &operation,
                                &request,
                                &progress.checkpoint,
                                progress.offset,
                                bytes,
                                &self.cancel,
                            ),
                        )
                        .await?;
                    if let UploadStep::Continue(next) = &next
                        && next.offset < progress.offset + u64::from(progress.length)
                    {
                        return Err(UploadError::Uncertain.into());
                    }
                    next
                }
                UploadStep::Stream(checkpoint) => {
                    prepared_allowed = false;
                    self.checkpoint(record, checkpoint.clone(), 0).await?;
                    let file = self.local(move |j| j.payload(id)).await?;
                    let next = self
                        .remote(
                            Duration::from_secs(900),
                            self.provider.upload_stream_for_operation(
                                &operation,
                                &request,
                                &checkpoint,
                                file,
                                &self.cancel,
                            ),
                        )
                        .await?;
                    if !matches!(
                        next,
                        UploadStep::Commit(_)
                            | UploadStep::Complete(_)
                            | UploadStep::PackageComplete(_)
                            | UploadStep::PackageHandoffComplete(_)
                    ) {
                        return Err(UploadError::Uncertain.into());
                    }
                    next
                }
            };
        }
    }
}
