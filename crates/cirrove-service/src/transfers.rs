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
    fn vault_key(id: Uuid) -> String {
        format!("upload/{id}")
    }
    async fn load_checkpoint(&self, id: Uuid) -> Result<Option<SecretString>> {
        tokio::time::timeout(
            Duration::from_secs(30),
            self.vault.load(&Self::vault_key(id)),
        )
        .await
        .map_err(|_| TransferError::Vault)?
        .map_err(|_| TransferError::Vault)
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
        tokio::time::timeout(
            Duration::from_secs(30),
            self.vault.save(&Self::vault_key(id), value),
        )
        .await
        .map_err(|_| TransferError::Vault)?
        .map_err(|_| TransferError::Vault)?;
        self.local(move |j| j.record_session(id, attempt, id, offset))
            .await
    }
    async fn clean_checkpoint(&self, id: Uuid) {
        if !matches!(
            tokio::time::timeout(
                Duration::from_secs(30),
                self.vault.remove(&Self::vault_key(id))
            )
            .await,
            Ok(Ok(()))
        ) {
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
        let attempt = record.attempt.ok_or(TransferError::Worker)?;
        let request = UploadRequest {
            scope: record.scope.clone(),
            intent: record.intent.clone(),
            size: record.size,
            sha256: record.sha256.clone(),
        };
        let mut step = if record.state == UploadState::Verifying {
            let checkpoint = self
                .load_checkpoint(record.session_key.unwrap_or(id))
                .await?;
            match checkpoint {
                Some(checkpoint) => match self
                    .provider
                    .inspect_upload(&request, &checkpoint, &self.cancel)
                    .await
                {
                    Ok(step) => Some(step),
                    Err(UploadError::SessionGone | UploadError::CheckpointInvalid) => None,
                    Err(error) => return Err(error.into()),
                },
                None => None,
            }
        } else {
            Some(self.provider.begin_upload(&request, &self.cancel).await?)
        };
        if step.is_none() {
            match self
                .provider
                .reconcile_upload(&request, &self.cancel)
                .await?
            {
                Reconciliation::Committed(node) => step = Some(UploadStep::Complete(node)),
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
        let mut payload = None;
        loop {
            if self.cancel.is_cancelled() {
                return Err(UploadError::Uncertain.into());
            }
            step = match step {
                UploadStep::Complete(node) => {
                    self.local(move |j| j.acknowledge(id, attempt, node))
                        .await?;
                    self.clean_checkpoint(id).await;
                    // Payload retention is a separate policy decision after the
                    // durable receipt; this worker never deletes an edited file.
                    return Ok(UploadState::Uploaded);
                }
                UploadStep::Commit(checkpoint) => {
                    self.checkpoint(record, checkpoint.clone(), request.size)
                        .await?;
                    let next = self
                        .provider
                        .commit_upload(&request, &checkpoint, &self.cancel)
                        .await?;
                    if !matches!(next, UploadStep::Complete(_)) {
                        return Err(UploadError::Uncertain.into());
                    }
                    next
                }
                UploadStep::Continue(progress) => {
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
                        .provider
                        .upload_part(
                            &request,
                            &progress.checkpoint,
                            progress.offset,
                            bytes,
                            &self.cancel,
                        )
                        .await?;
                    if let UploadStep::Continue(next) = &next
                        && next.offset < progress.offset + u64::from(progress.length)
                    {
                        return Err(UploadError::Uncertain.into());
                    }
                    next
                }
            };
        }
    }
}
