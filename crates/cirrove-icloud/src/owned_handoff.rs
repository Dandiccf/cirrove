//! Two-ID replacement through the shared worker. Ordinary write grants remain
//! gated: Apple rename is not conditional. Uncertain phases require exact-ID
//! reconciliation; fault injection is available only to isolated probes.
use super::{HandoffObserved, HandoffPlan, ICloudReadSession, write_transport::TRASH_ROOT};
use async_trait::async_trait;
use cirrove_core::upload::{
    Reconciliation, RecoveryLocation, Result as UploadResult, UploadError, UploadIntent,
    UploadProvider, UploadRequest, UploadStep,
};
use cirrove_core::{CancellationToken, Scope};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(feature = "write-probe")]
use std::time::Instant;
use tokio::sync::Mutex;
use uuid::Uuid;

const MAX_CHECKPOINT: usize = 8192;

/// Isolated-probe timing only. A dropped future has no known provider result.
#[cfg(feature = "write-probe")]
struct HandoffTiming {
    phase: &'static str,
    started: Instant,
    finished: bool,
}

#[cfg(feature = "write-probe")]
impl HandoffTiming {
    fn start(phase: &'static str) -> Self {
        eprintln!("iCloud handoff {phase}: start");
        Self {
            phase,
            started: Instant::now(),
            finished: false,
        }
    }

    fn finish(mut self, success: bool) {
        self.finished = true;
        eprintln!(
            "iCloud handoff {}: end {:.1}s success={success}",
            self.phase,
            self.started.elapsed().as_secs_f64()
        );
    }
}

#[cfg(feature = "write-probe")]
impl Drop for HandoffTiming {
    fn drop(&mut self) {
        if !self.finished {
            eprintln!(
                "iCloud handoff {}: dropped {:.1}s",
                self.phase,
                self.started.elapsed().as_secs_f64()
            );
        }
    }
}

#[cfg(not(feature = "write-probe"))]
struct HandoffTiming;
#[cfg(not(feature = "write-probe"))]
impl HandoffTiming {
    fn start(_: &'static str) -> Self {
        Self
    }
    fn finish(self, _: bool) {}
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Phase {
    MoveOld,
    // Legacy checkpoint: the full preflight or the rename may have run.
    InstallNew,
    // Version 2: this commit performs reads only, so restart may repeat it.
    InspectInstall,
    // Version 2: the full preflight completed before this marker was saved.
    // On restart this is potentially sent, never permission to replay rename.
    InstallInspected,
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

pub struct ICloudHandoff {
    scope: Scope,
    operation: Uuid,
    plan: HandoffPlan,
    staged_size: u64,
    session: Mutex<ICloudReadSession>,
    discard_old_receipt: AtomicBool,
    delay_old_receipt_past_worker_deadline: AtomicBool,
    old_receipt_delay_started: AtomicBool,
    discard_new_receipt: AtomicBool,
    #[cfg(feature = "write-probe")]
    inject_intervening_edit: AtomicBool,
    #[cfg(feature = "write-probe")]
    stale_trash_refusal_verified: AtomicBool,
    reconciliation_only: bool,
    recovery_mode: RecoveryMode,
}

impl ICloudHandoff {
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
            || staged_size > crate::MAX_WRITE_FILE_SIZE
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
            #[cfg(feature = "write-probe")]
            inject_intervening_edit: AtomicBool::new(false),
            #[cfg(feature = "write-probe")]
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
    #[cfg(feature = "write-probe")]
    pub fn with_discarded_old_receipt(self) -> Self {
        self.discard_old_receipt.store(true, Ordering::Release);
        self
    }

    /// Validation only: hold an accepted Trash response beyond the shared
    /// worker's 125-second provider deadline. The worker cancels this future;
    /// a fresh process must reconcile the exact old ID before proceeding.
    #[cfg(feature = "write-probe")]
    pub fn with_delayed_old_receipt(self) -> Self {
        self.delay_old_receipt_past_worker_deadline
            .store(true, Ordering::Release);
        self
    }

    #[cfg(feature = "write-probe")]
    pub fn old_receipt_delay_started(&self) -> bool {
        self.old_receipt_delay_started.load(Ordering::Acquire)
    }

    #[cfg(feature = "write-probe")]
    pub fn with_discarded_new_receipt(self) -> Self {
        self.discard_new_receipt.store(true, Ordering::Release);
        self
    }

    /// Validation only: change the owned old fixture after the worker's last
    /// prepared observation, immediately before its conditional Trash call.
    #[cfg(feature = "write-probe")]
    pub fn with_intervening_old_edit(self) -> Self {
        self.inject_intervening_edit.store(true, Ordering::Release);
        self
    }

    #[cfg(feature = "write-probe")]
    pub fn stale_trash_refusal_verified(&self) -> bool {
        self.stale_trash_refusal_verified.load(Ordering::Acquire)
    }

    /// True only while a configured one-shot fault has not yet reached the
    /// staged rename response. The live probe uses this to distinguish an
    /// earlier uncertain phase from the intended lost-response exercise.
    #[cfg(feature = "write-probe")]
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
            version: 2,
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
        if !matches!(saved.version, 1 | 2)
            || (saved.version == 1
                && matches!(saved.phase, Phase::InspectInstall | Phase::InstallInspected))
            || (self.recovery_mode != RecoveryMode::Trash
                && matches!(saved.phase, Phase::InspectInstall | Phase::InstallInspected))
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

    fn next_install_phase(&self) -> Phase {
        if self.recovery_mode == RecoveryMode::Trash {
            Phase::InspectInstall
        } else {
            Phase::InstallNew
        }
    }

    fn step_after_inspection(
        &self,
        phase: Phase,
        state: HandoffObserved,
        receipt: Option<UploadStep>,
    ) -> UploadResult<UploadStep> {
        match (phase, state) {
            (_, HandoffObserved::Complete) => receipt.ok_or(UploadError::Uncertain),
            (Phase::MoveOld, HandoffObserved::OldAtRecovery) => Ok(UploadStep::Commit(
                self.checkpoint(self.next_install_phase())?,
            )),
            (Phase::InspectInstall, HandoffObserved::OldAtRecovery) => {
                // Inspection itself just completed the full preflight. Persist
                // the potentially-sent boundary before permitting the rename.
                Ok(UploadStep::Commit(
                    self.checkpoint(Phase::InstallInspected)?,
                ))
            }
            (_, HandoffObserved::Diverged) => Err(UploadError::Conflict),
            _ => Err(UploadError::Uncertain),
        }
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
            let timing = HandoffTiming::start("inspect with receipt");
            let observation = self
                .session
                .lock()
                .await
                .inspect_trash_handoff_with_receipt(&self.plan)
                .await;
            timing.finish(observation.is_ok());
            let (state, nodes) = observation.map_err(|_| UploadError::Uncertain)?;
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
impl UploadProvider for ICloudHandoff {
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
        self.step_after_inspection(phase, state, receipt)
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
                let timing = HandoffTiming::start("move old preflight");
                let before = match self.recovery_mode {
                    RecoveryMode::Rename => session.inspect_durable_handoff(&self.plan).await,
                    RecoveryMode::Trash => session.inspect_durable_trash_handoff(&self.plan).await,
                };
                timing.finish(before.is_ok());
                let before = before.map_err(|_| UploadError::Uncertain)?;
                if before != HandoffObserved::Prepared {
                    return Err(UploadError::Conflict);
                }
                #[cfg(feature = "write-probe")]
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
                let timing = HandoffTiming::start("move old request");
                let accepted = match self.recovery_mode {
                    RecoveryMode::Rename => session.move_old_to_recovery(&self.plan).await,
                    RecoveryMode::Trash => {
                        session
                            .send_trash(&self.plan.original_id, &self.plan.original_etag)
                            .await
                    }
                };
                timing.finish(accepted.is_ok());
                let accepted = accepted.map_err(|_| UploadError::Uncertain)?;
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
                let timing = HandoffTiming::start("move old postflight");
                let after = match self.recovery_mode {
                    RecoveryMode::Rename => session.inspect_durable_handoff(&self.plan).await,
                    RecoveryMode::Trash => session.inspect_durable_trash_handoff(&self.plan).await,
                };
                timing.finish(after.is_ok());
                let after = after.map_err(|_| UploadError::Uncertain)?;
                if !accepted || after != HandoffObserved::OldAtRecovery {
                    return Err(UploadError::Uncertain);
                }
                Ok(UploadStep::Commit(
                    self.checkpoint(self.next_install_phase())?,
                ))
            }
            Phase::InspectInstall => {
                let timing = HandoffTiming::start("install read-only preflight");
                let before = session.inspect_durable_trash_handoff(&self.plan).await;
                timing.finish(before.is_ok());
                let before = before.map_err(|_| UploadError::Uncertain)?;
                if before != HandoffObserved::OldAtRecovery {
                    return Err(UploadError::Conflict);
                }
                // The worker persists this new marker before calling commit again.
                // This arm has sent no mutation, including if its future is dropped.
                Ok(UploadStep::Commit(
                    self.checkpoint(Phase::InstallInspected)?,
                ))
            }
            Phase::InstallNew | Phase::InstallInspected => {
                if phase == Phase::InstallNew {
                    let timing = HandoffTiming::start("install new preflight");
                    let before = match self.recovery_mode {
                        RecoveryMode::Rename => session.inspect_durable_handoff(&self.plan).await,
                        RecoveryMode::Trash => {
                            session.inspect_durable_trash_handoff(&self.plan).await
                        }
                    };
                    timing.finish(before.is_ok());
                    let before = before.map_err(|_| UploadError::Uncertain)?;
                    if before != HandoffObserved::OldAtRecovery {
                        return Err(UploadError::Conflict);
                    }
                }
                let timing = HandoffTiming::start("install new request");
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
                };
                timing.finish(accepted.is_ok());
                let accepted = accepted.map_err(|_| UploadError::Uncertain)?;
                if self.discard_new_receipt.swap(false, Ordering::AcqRel) {
                    return Err(UploadError::Uncertain);
                }
                if !accepted {
                    return Err(UploadError::Uncertain);
                }
                drop(session);
                let timing = HandoffTiming::start("install new receipt");
                let receipt = self.receipt().await;
                timing.finish(receipt.is_ok());
                receipt
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

    fn fixture() -> (ICloudHandoff, UploadRequest) {
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
            ICloudHandoff::new(scope, operation, plan, 3, session).unwrap(),
            request,
        )
    }

    #[test]
    fn empty_handoff_keeps_zero_size_and_exact_payload_across_checkpoint_restore() {
        let (provider, mut request) = fixture();
        let mut plan = provider.plan.clone();
        plan.staged_sha256 = hex::encode(Sha256::digest(b""));
        request.size = 0;
        request.sha256 = plan.staged_sha256.clone();
        let session = || {
            let mut session = ICloudReadSession::new().expect("fixture session");
            session.account_hash = Some("synthetic-account".into());
            session
        };
        let empty = ICloudHandoff::new(
            request.scope.clone(),
            provider.operation,
            plan,
            0,
            session(),
        )
        .expect("empty handoff");
        let checkpoint = empty.checkpoint(Phase::MoveOld).expect("checkpoint");
        let restored =
            ICloudHandoff::from_checkpoint(&request, provider.operation, &checkpoint, session())
                .expect("restore");
        assert_eq!(restored.staged_size, 0);
        assert_eq!(
            restored
                .check_checkpoint(&request, &checkpoint)
                .expect("binding"),
            Phase::MoveOld
        );
        request.size = 1;
        assert!(restored.check_checkpoint(&request, &checkpoint).is_err());
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
        let restarted =
            ICloudHandoff::from_checkpoint(&request, provider.operation, &first, restarted_session)
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
            ICloudHandoff::from_checkpoint(
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
        let resumed =
            ICloudHandoff::from_checkpoint(&request, provider.operation, &checkpoint, session)
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

    #[test]
    fn restart_repeats_only_read_only_install_preflight() {
        let (mut provider, request) = fixture();
        provider.recovery_mode = RecoveryMode::Trash;
        for phase in [
            Phase::InspectInstall,
            Phase::InstallInspected,
            Phase::InstallNew,
        ] {
            let checkpoint = provider.checkpoint(phase).unwrap();
            let mut session = ICloudReadSession::new().unwrap();
            session.account_hash = Some("synthetic-account".into());
            let resumed =
                ICloudHandoff::from_checkpoint(&request, provider.operation, &checkpoint, session)
                    .unwrap();
            let saved = resumed.check_checkpoint(&request, &checkpoint).unwrap();
            let next = resumed.step_after_inspection(saved, HandoffObserved::OldAtRecovery, None);
            if phase == Phase::InspectInstall {
                let UploadStep::Commit(ready) =
                    next.expect("unsent read-only preflight must resume")
                else {
                    panic!("expected a persisted mutation boundary");
                };
                assert_eq!(
                    resumed.check_checkpoint(&request, &ready).unwrap(),
                    Phase::InstallInspected
                );
            } else {
                assert!(matches!(next, Err(UploadError::Uncertain)));
            }
            assert!(matches!(
                resumed.step_after_inspection(saved, HandoffObserved::Diverged, None),
                Err(UploadError::Conflict)
            ));
            assert!(matches!(
                resumed.step_after_inspection(saved, HandoffObserved::Complete, None),
                Err(UploadError::Uncertain)
            ));
        }
    }

    #[tokio::test]
    async fn interrupted_install_preflight_sends_only_a_metadata_read() {
        use std::time::Duration;
        use tokio::io::AsyncReadExt;
        use tokio::net::TcpListener;

        let (mut provider, request) = fixture();
        provider.recovery_mode = RecoveryMode::Trash;
        let checkpoint = provider.checkpoint(Phase::InspectInstall).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        provider.session.lock().await.drive_endpoint =
            Some(url::Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap());
        let (seen, received) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut peer, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            while !bytes.windows(4).any(|part| part == b"\r\n\r\n") {
                let mut chunk = [0; 4096];
                let n = peer.read(&mut chunk).await.unwrap();
                assert!(n > 0 && bytes.len() + n < 16 * 1024);
                bytes.extend_from_slice(&chunk[..n]);
            }
            assert!(bytes.starts_with(b"POST /retrieveItemDetailsInFolders HTTP/1.1\r\n"));
            seen.send(()).unwrap();
            released.await.unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(50), listener.accept())
                    .await
                    .is_err()
            );
        });
        let cancel = CancellationToken::new();
        {
            let commit = provider.commit_upload(&request, &checkpoint, &cancel);
            tokio::pin!(commit);
            tokio::select! {
                result = &mut commit => panic!("preflight returned before metadata: {result:?}"),
                result = tokio::time::timeout(Duration::from_secs(2), received) => {
                    result.unwrap().unwrap();
                }
            }
            // Drop the in-flight read exactly as a worker deadline or shutdown would.
        }
        release.send(()).unwrap();
        server.await.unwrap();
        assert_eq!(
            provider.check_checkpoint(&request, &checkpoint).unwrap(),
            Phase::InspectInstall
        );
        // The dropped future must also release the session for later inspection.
        assert!(provider.session.try_lock().is_ok());
    }

    #[test]
    fn legacy_checkpoints_cannot_claim_a_read_only_phase() {
        let (mut provider, request) = fixture();
        provider.recovery_mode = RecoveryMode::Trash;
        for phase in [
            Phase::MoveOld,
            Phase::InstallNew,
            Phase::InspectInstall,
            Phase::InstallInspected,
        ] {
            let checkpoint = provider.checkpoint(phase).unwrap();
            let mut value: serde_json::Value =
                serde_json::from_str(checkpoint.expose_secret()).unwrap();
            value["version"] = 1.into();
            let legacy = SecretString::from(serde_json::to_string(&value).unwrap());
            assert_eq!(
                provider.check_checkpoint(&request, &legacy).is_ok(),
                matches!(phase, Phase::MoveOld | Phase::InstallNew)
            );
        }
    }

    #[tokio::test]
    async fn restarted_verifier_cannot_send_either_rename() {
        for mode in [RecoveryMode::Rename, RecoveryMode::Trash] {
            let (mut provider, request) = fixture();
            provider.recovery_mode = mode;
            let checkpoint = provider.checkpoint(Phase::InstallNew).unwrap();
            let mut session = ICloudReadSession::new().unwrap();
            session.account_hash = Some("synthetic-account".into());
            let verifier =
                ICloudHandoff::from_checkpoint(&request, provider.operation, &checkpoint, session)
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
