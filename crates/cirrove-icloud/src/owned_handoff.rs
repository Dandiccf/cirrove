//! Two-ID replacement of one Cirrove-owned fixture through the shared worker.
//! Apple rename is not conditional, so this is deliberately unavailable to the
//! ordinary mount. Every uncertain phase requires exact-ID reconciliation.
use super::{HandoffObserved, HandoffPlan, ICloudReadSession, write_probe::TRASH_ROOT};
use async_trait::async_trait;
use cirrove_core::upload::{
    Reconciliation, RecoveryLocation, Result as UploadResult, UploadError, UploadIntent,
    UploadProvider, UploadRequest, UploadStep,
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

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RecoveryMode {
    #[default]
    Rename,
    Trash,
}

#[derive(Deserialize, Serialize)]
struct Checkpoint {
    version: u8,
    scope: Scope,
    operation: Uuid,
    size: u64,
    phase: Phase,
    #[serde(default)]
    recovery_mode: RecoveryMode,
    plan: HandoffPlan,
}

pub struct ICloudOwnedFixtureHandoff {
    scope: Scope,
    operation: Uuid,
    plan: HandoffPlan,
    staged_size: u64,
    session: Mutex<ICloudReadSession>,
    discard_old_receipt: AtomicBool,
    delay_old_receipt_past_worker_deadline: AtomicBool,
    old_receipt_delay_started: AtomicBool,
    discard_new_receipt: AtomicBool,
    inject_intervening_edit: AtomicBool,
    stale_trash_refusal_verified: AtomicBool,
    reconciliation_only: bool,
    recovery_mode: RecoveryMode,
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
            || staged_size > 32 * 1024 * 1024
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
            delay_old_receipt_past_worker_deadline: AtomicBool::new(false),
            old_receipt_delay_started: AtomicBool::new(false),
            discard_new_receipt: AtomicBool::new(false),
            inject_intervening_edit: AtomicBool::new(false),
            stale_trash_refusal_verified: AtomicBool::new(false),
            reconciliation_only: false,
            recovery_mode: RecoveryMode::Rename,
        })
    }

    /// Only for a new Cirrove-owned two-file fixture. Ordinary mounts never
    /// construct this upload provider.
    pub fn new_conditional_trash(
        scope: Scope,
        operation: Uuid,
        plan: HandoffPlan,
        staged_size: u64,
        session: ICloudReadSession,
    ) -> UploadResult<Self> {
        let mut provider = Self::new(scope, operation, plan, staged_size, session)?;
        provider.recovery_mode = RecoveryMode::Trash;
        Ok(provider)
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
        let mut provider = Self::new(
            request.scope.clone(),
            operation,
            saved.plan,
            saved.size,
            session,
        )?;
        provider.recovery_mode = saved.recovery_mode;
        provider.check_checkpoint(request, checkpoint)?;
        Ok(provider)
    }

    /// One-shot validation fault after Apple has responded to the old rename.
    pub fn with_discarded_old_receipt(self) -> Self {
        self.discard_old_receipt.store(true, Ordering::Release);
        self
    }

    /// Validation only: hold an accepted Trash response beyond the shared
    /// worker's 125-second provider deadline. The worker cancels this future;
    /// a fresh process must reconcile the exact old ID before proceeding.
    pub fn with_delayed_old_receipt(self) -> Self {
        self.delay_old_receipt_past_worker_deadline
            .store(true, Ordering::Release);
        self
    }

    pub fn old_receipt_delay_started(&self) -> bool {
        self.old_receipt_delay_started.load(Ordering::Acquire)
    }

    pub fn with_discarded_new_receipt(self) -> Self {
        self.discard_new_receipt.store(true, Ordering::Release);
        self
    }

    /// Validation only: change the owned old fixture after the worker's last
    /// prepared observation, immediately before its conditional Trash call.
    pub fn with_intervening_old_edit(self) -> Self {
        self.inject_intervening_edit.store(true, Ordering::Release);
        self
    }

    pub fn stale_trash_refusal_verified(&self) -> bool {
        self.stale_trash_refusal_verified.load(Ordering::Acquire)
    }

    /// True only while a configured one-shot fault has not yet reached the
    /// staged rename response. The live probe uses this to distinguish an
    /// earlier uncertain phase from the intended lost-response exercise.
    pub fn new_receipt_discard_pending(&self) -> bool {
        self.discard_new_receipt.load(Ordering::Acquire)
    }

    /// Read-only phase inspection of this exact owned fixture. It never
    /// advances the worker or sends a mutation request.
    pub async fn inspect_owned_fixture(&self) -> anyhow::Result<HandoffObserved> {
        let mut session = self.session.lock().await;
        match self.recovery_mode {
            RecoveryMode::Rename => session.inspect_durable_handoff(&self.plan).await,
            RecoveryMode::Trash => session.inspect_durable_trash_handoff(&self.plan).await,
        }
    }

    /// Read-only full receipt construction for diagnosing a finished owned
    /// fixture; the returned IDs and bytes are never printed by the probe.
    pub async fn inspect_owned_receipt(
        &self,
    ) -> anyhow::Result<(cirrove_core::Node, cirrove_core::Node)> {
        let mut session = self.session.lock().await;
        match self.recovery_mode {
            RecoveryMode::Rename => session.verified_handoff_nodes(&self.plan).await,
            RecoveryMode::Trash => session.verified_trash_handoff_nodes(&self.plan).await,
        }
    }

    /// A restarted verifier cannot send either rename in this validation mode.
    pub fn reconciliation_only(mut self) -> Self {
        self.reconciliation_only = true;
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
            recovery_mode: self.recovery_mode,
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
            || saved.recovery_mode != self.recovery_mode
            || saved.plan.version != self.plan.version
            || saved.plan.folder_parent() != self.plan.folder_parent()
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
            || saved.plan.target_name != self.plan.target_name
        {
            return Err(UploadError::CheckpointInvalid);
        }
        Ok(saved.phase)
    }

    async fn observed(&self) -> UploadResult<HandoffObserved> {
        let mut session = self.session.lock().await;
        match self.recovery_mode {
            RecoveryMode::Rename => session.inspect_durable_handoff(&self.plan).await,
            RecoveryMode::Trash => session.inspect_durable_trash_handoff(&self.plan).await,
        }
        .map_err(|_| UploadError::Uncertain)
    }

    async fn receipt(&self) -> UploadResult<UploadStep> {
        let mut session = self.session.lock().await;
        let (current, backup) = match self.recovery_mode {
            RecoveryMode::Rename => session.verified_handoff_nodes(&self.plan).await,
            RecoveryMode::Trash => session.verified_trash_handoff_nodes(&self.plan).await,
        }
        .map_err(|_| UploadError::Uncertain)?;
        if current.size != self.staged_size {
            return Err(UploadError::Conflict);
        }
        Ok(UploadStep::HandoffComplete { current, backup })
    }

    async fn observed_with_receipt(&self) -> UploadResult<(HandoffObserved, Option<UploadStep>)> {
        if self.recovery_mode == RecoveryMode::Trash {
            let (state, nodes) = self
                .session
                .lock()
                .await
                .inspect_trash_handoff_with_receipt(&self.plan)
                .await
                .map_err(|_| UploadError::Uncertain)?;
            let receipt = nodes
                .map(|(current, backup)| {
                    if current.size != self.staged_size {
                        Err(UploadError::Conflict)
                    } else {
                        Ok(UploadStep::HandoffComplete { current, backup })
                    }
                })
                .transpose()?;
            return Ok((state, receipt));
        }
        let state = self.observed().await?;
        let receipt = if state == HandoffObserved::Complete {
            Some(self.receipt().await?)
        } else {
            None
        };
        Ok((state, receipt))
    }
}

#[async_trait]
impl UploadProvider for ICloudOwnedFixtureHandoff {
    fn staged_recovery_location(
        &self,
        operation: &str,
        request: &UploadRequest,
    ) -> Option<RecoveryLocation> {
        let name = self.staged_recovery_name(operation, request)?;
        Some(match self.recovery_mode {
            RecoveryMode::Rename => RecoveryLocation::Sibling { name },
            RecoveryMode::Trash => RecoveryLocation::Trash {
                local_name: name,
                parent: TRASH_ROOT.into(),
            },
        })
    }

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
        let (state, receipt) = self.observed_with_receipt().await?;
        match (phase, state) {
            (_, HandoffObserved::Complete) => receipt.ok_or(UploadError::Uncertain),
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
        if self.reconciliation_only {
            return Err(UploadError::Unsupported("reconciliation-only validation"));
        }
        if cancel.is_cancelled() {
            return Err(UploadError::Uncertain);
        }
        let mut session = self.session.lock().await;
        match phase {
            Phase::MoveOld => {
                let before = match self.recovery_mode {
                    RecoveryMode::Rename => session.inspect_durable_handoff(&self.plan).await,
                    RecoveryMode::Trash => session.inspect_durable_trash_handoff(&self.plan).await,
                }
                .map_err(|_| UploadError::Uncertain)?;
                if before != HandoffObserved::Prepared {
                    return Err(UploadError::Conflict);
                }
                if self.recovery_mode == RecoveryMode::Trash
                    && self.inject_intervening_edit.swap(false, Ordering::AcqRel)
                {
                    session
                        .probe_intervening_edit_rejects_trash(&self.plan)
                        .await
                        .map_err(|_| UploadError::Uncertain)?;
                    self.stale_trash_refusal_verified
                        .store(true, Ordering::Release);
                    return Err(UploadError::Conflict);
                }
                let accepted = match self.recovery_mode {
                    RecoveryMode::Rename => session.move_old_to_recovery(&self.plan).await,
                    RecoveryMode::Trash => {
                        session
                            .send_trash(&self.plan.original_id, &self.plan.original_etag)
                            .await
                    }
                }
                .map_err(|_| UploadError::Uncertain)?;
                if accepted
                    && self.recovery_mode == RecoveryMode::Trash
                    && self
                        .delay_old_receipt_past_worker_deadline
                        .swap(false, Ordering::AcqRel)
                {
                    self.old_receipt_delay_started
                        .store(true, Ordering::Release);
                    tokio::time::sleep(std::time::Duration::from_secs(130)).await;
                }
                if self.discard_old_receipt.swap(false, Ordering::AcqRel) {
                    return Err(UploadError::Uncertain);
                }
                let after = match self.recovery_mode {
                    RecoveryMode::Rename => session.inspect_durable_handoff(&self.plan).await,
                    RecoveryMode::Trash => session.inspect_durable_trash_handoff(&self.plan).await,
                }
                .map_err(|_| UploadError::Uncertain)?;
                if !accepted || after != HandoffObserved::OldAtRecovery {
                    return Err(UploadError::Uncertain);
                }
                Ok(UploadStep::Commit(self.checkpoint(Phase::InstallNew)?))
            }
            Phase::InstallNew => {
                let before = match self.recovery_mode {
                    RecoveryMode::Rename => session.inspect_durable_handoff(&self.plan).await,
                    RecoveryMode::Trash => session.inspect_durable_trash_handoff(&self.plan).await,
                }
                .map_err(|_| UploadError::Uncertain)?;
                if before != HandoffObserved::OldAtRecovery {
                    return Err(UploadError::Conflict);
                }
                let accepted = match self.recovery_mode {
                    RecoveryMode::Rename => session.move_staged_to_target(&self.plan).await,
                    RecoveryMode::Trash => {
                        session
                            .send_rename(
                                &self.plan.staged_id,
                                &self.plan.staged_etag,
                                &self.plan.target_name,
                            )
                            .await
                    }
                }
                .map_err(|_| UploadError::Uncertain)?;
                if self.discard_new_receipt.swap(false, Ordering::AcqRel) {
                    return Err(UploadError::Uncertain);
                }
                if !accepted {
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
        let (state, receipt) = self.observed_with_receipt().await?;
        match state {
            HandoffObserved::Complete => {
                let Some(UploadStep::HandoffComplete { current, backup }) = receipt else {
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
            folder_parent_id: crate::ROOT_ID.into(),
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
            target_name: "created-by-cirrove.txt".into(),
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
        let mut altered: serde_json::Value = serde_json::from_str(first.expose_secret()).unwrap();
        altered["plan"]["target_name"] = "other-destination.txt".into();
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

    #[test]
    fn conditional_trash_checkpoint_reserves_exact_trash_parent_across_restart() {
        let (mut provider, request) = fixture();
        provider.recovery_mode = RecoveryMode::Trash;
        let checkpoint = provider.checkpoint(Phase::MoveOld).unwrap();
        assert_eq!(
            provider.staged_recovery_location(&provider.operation.to_string(), &request),
            Some(RecoveryLocation::Trash {
                local_name: provider.plan.recovery_name.clone(),
                parent: TRASH_ROOT.into(),
            })
        );
        let mut session = ICloudReadSession::new().unwrap();
        session.account_hash = Some("synthetic-account".into());
        let resumed = ICloudOwnedFixtureHandoff::from_checkpoint(
            &request,
            provider.operation,
            &checkpoint,
            session,
        )
        .unwrap();
        assert_eq!(resumed.recovery_mode, RecoveryMode::Trash);
        assert_eq!(
            resumed.check_checkpoint(&request, &checkpoint).unwrap(),
            Phase::MoveOld
        );
        let mut altered: serde_json::Value =
            serde_json::from_str(checkpoint.expose_secret()).unwrap();
        altered["recovery_mode"] = "rename".into();
        let altered = SecretString::from(serde_json::to_string(&altered).unwrap());
        assert!(resumed.check_checkpoint(&request, &altered).is_err());
    }

    #[tokio::test]
    async fn restarted_verifier_cannot_send_either_rename() {
        for mode in [RecoveryMode::Rename, RecoveryMode::Trash] {
            let (mut provider, request) = fixture();
            provider.recovery_mode = mode;
            let checkpoint = provider.checkpoint(Phase::InstallNew).unwrap();
            let mut session = ICloudReadSession::new().unwrap();
            session.account_hash = Some("synthetic-account".into());
            let verifier = ICloudOwnedFixtureHandoff::from_checkpoint(
                &request,
                provider.operation,
                &checkpoint,
                session,
            )
            .unwrap()
            .reconciliation_only();
            let error = verifier
                .commit_upload(&request, &checkpoint, &CancellationToken::new())
                .await
                .err()
                .unwrap();
            assert!(matches!(error, UploadError::Unsupported(_)));
        }
    }
}
