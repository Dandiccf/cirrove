//! Durable namespace execution, not yet enabled in mounted filesystems.
use crate::journal::{JournalError, MutationState, UploadJournal};
use cirrove_core::mutation::{MutationError, MutationProvider, MutationReconciliation};
use cirrove_core::{CancellationToken, ProviderError};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;

pub struct MutationWorker {
    journal: Arc<Mutex<UploadJournal>>,
    provider: Arc<dyn MutationProvider>,
    cancel: CancellationToken,
}
#[derive(Debug)]
pub struct MutationResult {
    pub id: Uuid,
    pub state: MutationState,
    pub issue: Option<String>,
}
impl MutationWorker {
    pub fn new(
        journal: Arc<Mutex<UploadJournal>>,
        provider: Arc<dyn MutationProvider>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            journal,
            provider,
            cancel,
        }
    }
    async fn local<T: Send + 'static>(
        &self,
        action: impl FnOnce(&mut UploadJournal) -> Result<T, JournalError> + Send + 'static,
    ) -> Result<T, JournalError> {
        let journal = self.journal.clone();
        tokio::task::spawn_blocking(move || {
            action(&mut *journal.lock().map_err(|_| JournalError::Storage)?)
        })
        .await
        .map_err(|_| JournalError::Storage)?
    }
    pub async fn run_once(&self) -> Result<Option<MutationResult>, JournalError> {
        if self.cancel.is_cancelled() {
            return Ok(None);
        }
        let Some(record) = self.local(|j| j.claim_mutation()).await? else {
            return Ok(None);
        };
        let id = record.id;
        let attempt = record.attempt.ok_or(JournalError::Stale)?;
        let result = tokio::select! { biased;
            _=self.cancel.cancelled()=>Err(MutationError::Uncertain),
            result=tokio::time::timeout(Duration::from_secs(130),async {
                if record.state==MutationState::Verifying {
                    self.provider.reconcile_mutation(&record.request,&self.cancel).await
                } else {
                    self.provider.mutate(&record.request,&self.cancel).await.map(MutationReconciliation::Applied)
                }
            })=>result.unwrap_or(Err(MutationError::Uncertain))
        };
        let (state, issue) = match result {
            Ok(MutationReconciliation::Applied(receipt)) => {
                self.local(move |j| j.acknowledge_mutation(id, attempt, receipt))
                    .await?;
                (MutationState::Applied, None)
            }
            Ok(MutationReconciliation::Uncommitted) => {
                self.local(move |j| j.retry_uncommitted_mutation(id, attempt))
                    .await?;
                (MutationState::Pending, None)
            }
            other => {
                let (state, issue, delay) = match other {
                    Ok(MutationReconciliation::Conflict) => (
                        MutationState::Conflict,
                        "remote item changed; resolve the conflict".into(),
                        Duration::ZERO,
                    ),
                    Ok(MutationReconciliation::Indeterminate) => (
                        MutationState::NeedsReview,
                        "cannot establish the remote result; review the retained operation".into(),
                        Duration::ZERO,
                    ),
                    Err(error) => {
                        let state = match &error {
                            MutationError::Conflict => MutationState::Conflict,
                            MutationError::Invalid
                            | MutationError::Unsupported(_)
                            | MutationError::Quota
                            | MutationError::Provider(
                                ProviderError::Permission
                                | ProviderError::Authentication
                                | ProviderError::NotFound,
                            ) => MutationState::Failed,
                            _ => MutationState::VerifyRequired,
                        };
                        let delay = match &error {
                            MutationError::Provider(ProviderError::Throttled(delay)) => {
                                delay.saturating_add(Duration::from_secs(1))
                            }
                            _ => Duration::from_secs(2u64.pow(record.failed_attempts.min(6) + 1)),
                        };
                        (state, error.to_string(), delay)
                    }
                    _ => return Err(JournalError::Stale),
                };
                self.local(move |j| j.defer_mutation(id, attempt, state, delay))
                    .await?;
                (state, Some(issue))
            }
        };
        Ok(Some(MutationResult { id, state, issue }))
    }
}
