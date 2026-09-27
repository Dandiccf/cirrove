//! Two-ID replacement of one Cirrove-owned fixture through the shared worker.
//! Apple rename is not conditional, so this is deliberately unavailable to the
//! ordinary mount. Every uncertain phase requires exact-ID reconciliation.
use super::{HandoffObserved, HandoffPlan, ICloudReadSession};
use async_trait::async_trait;
use cirrove_core::upload::{
    Reconciliation, Result as UploadResult, UploadError, UploadIntent, UploadProvider,
    UploadRequest, UploadStep,
};
use cirrove_core::{CancellationToken, Scope};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Mutex;
use uuid::Uuid;

const MAX_CHECKPOINT: usize = 8192;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Phase {
    MoveOld,
    InstallNew,
}

#[derive(Deserialize, Serialize)]
struct Checkpoint {
    version: u8,
    scope: Scope,
    operation: Uuid,
    size: u64,
    phase: Phase,
    plan: HandoffPlan,
}

pub struct ICloudOwnedFixtureHandoff {
    scope: Scope,
    operation: Uuid,
    plan: HandoffPlan,
    staged_size: u64,
    session: Mutex<ICloudReadSession>,
    discard_old_receipt: AtomicBool,
}

impl ICloudOwnedFixtureHandoff {
    pub fn new(
        scope: Scope,
        operation: Uuid,
        plan: HandoffPlan,
        staged_size: u64,
        session: ICloudReadSession,
    ) -> UploadResult<Self> {
        if scope.account.is_empty()
            || scope.provider != "icloud"
            || scope.collection != "drive"
            || session.account_hash.is_none()
            || staged_size == 0
            || staged_size > 4096
            || plan.validate().is_err()
        {
            return Err(UploadError::Invalid);
        }
        Ok(Self {
            scope,
            operation,
            plan,
            staged_size,
            session: Mutex::new(session),
            discard_old_receipt: AtomicBool::new(false),
        })
    }

    /// Rebuild an exact fixture from the previously saved worker checkpoint.
    /// The request and account must still match before any remote observation.
    pub fn from_checkpoint(
        request: &UploadRequest,
        operation: Uuid,
        checkpoint: &SecretString,
        session: ICloudReadSession,
    ) -> UploadResult<Self> {
        if checkpoint.expose_secret().len() > MAX_CHECKPOINT {
            return Err(UploadError::CheckpointInvalid);
        }
        let saved: Checkpoint = serde_json::from_str(checkpoint.expose_secret())
            .map_err(|_| UploadError::CheckpointInvalid)?;
        let provider = Self::new(
            request.scope.clone(),
            operation,
            saved.plan,
            saved.size,
            session,
        )?;
        provider.check_checkpoint(request, checkpoint)?;
        Ok(provider)
    }

    /// One-shot validation fault after Apple has responded to the old rename.
    pub fn with_discarded_old_receipt(self) -> Self {
        self.discard_old_receipt.store(true, Ordering::Release);
        self
    }

    fn check_request(&self, request: &UploadRequest) -> UploadResult<()> {
        request.validate()?;
        let UploadIntent::Replace {
            item,
            expected_etag,
        } = &request.intent
        else {
            return Err(UploadError::Invalid);
        };
        if request.scope != self.scope
            || item != &self.plan.original_id
            || expected_etag != &self.plan.original_etag
            || request.size != self.staged_size
            || request.sha256 != self.plan.staged_sha256
        {
            return Err(UploadError::Invalid);
        }
        Ok(())
    }

    fn checkpoint(&self, phase: Phase) -> UploadResult<SecretString> {
        let value = Checkpoint {
            version: 1,
            scope: self.scope.clone(),
            operation: self.operation,
            size: self.staged_size,
            phase,
            plan: self.plan.clone(),
        };
        let text = serde_json::to_string(&value).map_err(|_| UploadError::Invalid)?;
        if text.len() > MAX_CHECKPOINT {
            return Err(UploadError::Invalid);
        }
        Ok(SecretString::from(text))
    }

    fn check_checkpoint(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
    ) -> UploadResult<Phase> {
        self.check_request(request)?;
        if checkpoint.expose_secret().len() > MAX_CHECKPOINT {
            return Err(UploadError::CheckpointInvalid);
        }
        let saved: Checkpoint = serde_json::from_str(checkpoint.expose_secret())
            .map_err(|_| UploadError::CheckpointInvalid)?;
        if saved.version != 1
            || saved.scope != self.scope
            || saved.operation != self.operation
            || saved.size != self.staged_size
            || saved.plan.version != self.plan.version
            || saved.plan.folder_id != self.plan.folder_id
            || saved.plan.folder_name != self.plan.folder_name
            || saved.plan.original_id != self.plan.original_id
            || saved.plan.original_doc_id != self.plan.original_doc_id
            || saved.plan.original_etag != self.plan.original_etag
            || saved.plan.original_sha256 != self.plan.original_sha256
            || saved.plan.staged_id != self.plan.staged_id
            || saved.plan.staged_doc_id != self.plan.staged_doc_id
            || saved.plan.staged_etag != self.plan.staged_etag
            || saved.plan.staged_name != self.plan.staged_name
            || saved.plan.staged_sha256 != self.plan.staged_sha256
            || saved.plan.recovery_name != self.plan.recovery_name
        {
            return Err(UploadError::CheckpointInvalid);
        }
        Ok(saved.phase)
    }

    async fn observed(&self) -> UploadResult<HandoffObserved> {
        self.session
            .lock()
            .await
            .inspect_durable_handoff(&self.plan)
            .await
            .map_err(|_| UploadError::Uncertain)
    }

    async fn receipt(&self) -> UploadResult<UploadStep> {
        let (current, backup) = self
            .session
            .lock()
            .await
            .verified_handoff_nodes(&self.plan)
            .await
            .map_err(|_| UploadError::Uncertain)?;
        if current.size != self.staged_size {
            return Err(UploadError::Conflict);
        }
        Ok(UploadStep::HandoffComplete { current, backup })
    }
}

#[async_trait]
impl UploadProvider for ICloudOwnedFixtureHandoff {
    fn staged_recovery_name(&self, operation: &str, request: &UploadRequest) -> Option<String> {
        if operation != self.operation.to_string() || self.check_request(request).is_err() {
            return None;
        }
        Some(self.plan.recovery_name.clone())
    }

    async fn begin_upload(
        &self,
        request: &UploadRequest,
        cancel: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        self.check_request(request)?;
        if cancel.is_cancelled() {
            return Err(UploadError::Uncertain);
        }
        if self.observed().await? != HandoffObserved::Prepared {
            return Err(UploadError::Conflict);
        }
        Ok(UploadStep::Commit(self.checkpoint(Phase::MoveOld)?))
    }

    async fn inspect_upload(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
        _: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        let phase = self.check_checkpoint(request, checkpoint)?;
        match (phase, self.observed().await?) {
            (_, HandoffObserved::Complete) => self.receipt().await,
            (Phase::MoveOld, HandoffObserved::OldAtRecovery) => {
                Ok(UploadStep::Commit(self.checkpoint(Phase::InstallNew)?))
            }
            (_, HandoffObserved::Diverged) => Err(UploadError::Conflict),
            _ => Err(UploadError::Uncertain),
        }
    }

    async fn upload_part(
        &self,
        _: &UploadRequest,
        _: &SecretString,
        _: u64,
        _: Vec<u8>,
        _: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        Err(UploadError::Unsupported(
            "staged fixture bytes are already uploaded",
        ))
    }

    async fn commit_upload(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
        cancel: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        let phase = self.check_checkpoint(request, checkpoint)?;
        if cancel.is_cancelled() {
            return Err(UploadError::Uncertain);
        }
        let mut session = self.session.lock().await;
        match phase {
            Phase::MoveOld => {
                if session
                    .inspect_durable_handoff(&self.plan)
                    .await
                    .map_err(|_| UploadError::Uncertain)?
                    != HandoffObserved::Prepared
                {
                    return Err(UploadError::Conflict);
                }
                let accepted = session
                    .move_old_to_recovery(&self.plan)
                    .await
                    .map_err(|_| UploadError::Uncertain)?;
                if self.discard_old_receipt.swap(false, Ordering::AcqRel) {
                    return Err(UploadError::Uncertain);
                }
                if !accepted
                    || session
                        .inspect_durable_handoff(&self.plan)
                        .await
                        .map_err(|_| UploadError::Uncertain)?
                        != HandoffObserved::OldAtRecovery
                {
                    return Err(UploadError::Uncertain);
                }
                Ok(UploadStep::Commit(self.checkpoint(Phase::InstallNew)?))
            }
            Phase::InstallNew => {
                if session
                    .inspect_durable_handoff(&self.plan)
                    .await
                    .map_err(|_| UploadError::Uncertain)?
                    != HandoffObserved::OldAtRecovery
                {
                    return Err(UploadError::Conflict);
                }
                if !session
                    .move_staged_to_target(&self.plan)
                    .await
                    .map_err(|_| UploadError::Uncertain)?
                {
                    return Err(UploadError::Uncertain);
                }
                drop(session);
                self.receipt().await
            }
        }
    }

    async fn reconcile_upload(
        &self,
        request: &UploadRequest,
        checkpoint: Option<&SecretString>,
        _: &CancellationToken,
    ) -> UploadResult<Reconciliation> {
        self.check_checkpoint(request, checkpoint.ok_or(UploadError::CheckpointInvalid)?)?;
        match self.observed().await? {
            HandoffObserved::Complete => {
                let UploadStep::HandoffComplete { current, backup } = self.receipt().await? else {
                    return Err(UploadError::Uncertain);
                };
                Ok(Reconciliation::HandoffCommitted { current, backup })
            }
            HandoffObserved::Diverged => Ok(Reconciliation::Conflict),
            _ => Err(UploadError::Uncertain),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn fixture() -> (ICloudOwnedFixtureHandoff, UploadRequest) {
        let mut session = ICloudReadSession::new().unwrap();
        session.account_hash = Some("synthetic-account".into());
        let operation = Uuid::new_v4();
        let scope = Scope {
            account: Uuid::new_v4().to_string(),
            provider: "icloud".into(),
            collection: "drive".into(),
        };
        let plan = HandoffPlan {
            version: 2,
            folder_id: "FOLDER::com.apple.CloudDocs::folder".into(),
            folder_name: format!("Cirrove Write Validation-{}", Uuid::new_v4()),
            original_id: "FILE::com.apple.CloudDocs::old".into(),
            original_doc_id: "old".into(),
            original_etag: "old-etag".into(),
            staged_id: "FILE::com.apple.CloudDocs::new".into(),
            staged_doc_id: "new".into(),
            staged_etag: "new-etag".into(),
            staged_name: format!("staged-by-cirrove-{}.txt", Uuid::new_v4()),
            recovery_name: format!("recovery-by-cirrove-{operation}.txt"),
            original_sha256: hex::encode(Sha256::digest(b"old")),
            staged_sha256: hex::encode(Sha256::digest(b"new")),
        };
        let request = UploadRequest {
            scope: scope.clone(),
            intent: UploadIntent::Replace {
                item: plan.original_id.clone(),
                expected_etag: plan.original_etag.clone(),
            },
            size: 3,
            sha256: plan.staged_sha256.clone(),
        };
        (
            ICloudOwnedFixtureHandoff::new(scope, operation, plan, 3, session).unwrap(),
            request,
        )
    }

    #[test]
    fn checkpoint_and_recovery_name_bind_both_exact_ids_and_payload() {
        let (provider, request) = fixture();
        let first = provider.checkpoint(Phase::MoveOld).unwrap();
        assert_eq!(
            provider.check_checkpoint(&request, &first).unwrap(),
            Phase::MoveOld
        );
        assert_eq!(
            provider.staged_recovery_name(&provider.operation.to_string(), &request),
            Some(provider.plan.recovery_name.clone())
        );
        assert_eq!(
            provider.staged_recovery_name(&Uuid::new_v4().to_string(), &request),
            None
        );
        let mut different = request.clone();
        different.sha256 = hex::encode(Sha256::digest(b"bad"));
        assert!(provider.check_checkpoint(&different, &first).is_err());
        let mut altered: serde_json::Value = serde_json::from_str(first.expose_secret()).unwrap();
        altered["plan"]["staged_id"] = "FILE::com.apple.CloudDocs::foreign".into();
        let altered = SecretString::from(serde_json::to_string(&altered).unwrap());
        assert!(provider.check_checkpoint(&request, &altered).is_err());

        let mut restarted_session = ICloudReadSession::new().unwrap();
        restarted_session.account_hash = Some("synthetic-account".into());
        let restarted = ICloudOwnedFixtureHandoff::from_checkpoint(
            &request,
            provider.operation,
            &first,
            restarted_session,
        )
        .unwrap();
        assert_eq!(
            restarted.check_checkpoint(&request, &first).unwrap(),
            Phase::MoveOld
        );
        let mut different_account = request.clone();
        different_account.scope.account = Uuid::new_v4().to_string();
        let mut foreign_session = ICloudReadSession::new().unwrap();
        foreign_session.account_hash = Some("synthetic-account".into());
        assert!(
            ICloudOwnedFixtureHandoff::from_checkpoint(
                &different_account,
                provider.operation,
                &first,
                foreign_session,
            )
            .is_err()
        );
    }
}
