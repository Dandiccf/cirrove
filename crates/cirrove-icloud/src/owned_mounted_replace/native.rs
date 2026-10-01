//! Native archives use the existing replacement owner's Stage -> Handoff
//! lifecycle. No service routing is enabled here. A saved mutation phase is
//! inspection-only after restart; only a fresh worker commit dispatches it.
use super::*;
use crate::{
    HandoffObserved, ICloudPackageCreate, handoff_transport::packages::NativePackageProof,
    owned_handoff::Phase as HandoffPhase,
};
use cirrove_core::upload::{PackageHandoffReceipt, PackageUploadReceipt, UploadRepresentation};
use serde_json::Value;
use std::path::PathBuf;

const LIMIT: usize = 96 * 1024;
const HEADER_LIMIT: usize = 12 * 1024;
const INNER_LIMIT: usize = 80 * 1024;
pub(super) struct Context {
    request: UploadRequest,
    provider: ICloudPackageCreate,
    staging: PathBuf,
    account_hash: String,
    #[cfg(any(test, feature = "test-support"))]
    fixture_transport: Option<(reqwest::Client, url::Url)>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    version: u8,
    operation: Uuid,
    request: UploadRequest,
    parent: Node,
    account_hash: String,
    phase: NativePhase,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum NativePhase {
    Stage {
        inner: Value,
    },
    Handoff {
        plan: Box<HandoffPlan>,
        phase: HandoffPhase,
    },
}
fn pack(inner: SecretString) -> UploadResult<Value> {
    if inner.expose_secret().len() > LIMIT {
        return Err(UploadError::CheckpointInvalid);
    }
    let mut value: Value =
        serde_json::from_str(inner.expose_secret()).map_err(|_| UploadError::CheckpointInvalid)?;
    // PackageCreate stores this JSON fragment as text. Embed it structurally,
    // avoiding a second escaping expansion in the single outer envelope.
    if let Some(text) = value.get("registration").and_then(Value::as_str) {
        let registration: Value =
            serde_json::from_str(text).map_err(|_| UploadError::CheckpointInvalid)?;
        if !registration.is_object() {
            return Err(UploadError::CheckpointInvalid);
        }
        value["registration"] = registration;
    }
    if serde_json::to_vec(&value)
        .map_err(|_| UploadError::Invalid)?
        .len()
        > INNER_LIMIT
    {
        return Err(UploadError::Uncertain);
    }
    Ok(value)
}
fn unpack(mut inner: Value) -> UploadResult<SecretString> {
    if let Some(value) = inner.get("registration").filter(|v| !v.is_null()) {
        if !value.is_object() {
            return Err(UploadError::CheckpointInvalid);
        }
        let text = serde_json::to_string(value).map_err(|_| UploadError::CheckpointInvalid)?;
        inner["registration"] = Value::String(text);
    }
    let text = serde_json::to_string(&inner).map_err(|_| UploadError::CheckpointInvalid)?;
    if text.len() > LIMIT {
        return Err(UploadError::CheckpointInvalid);
    }
    Ok(text.into())
}
impl ICloudFileReplace {
    /// Synthetic HTTPS only, absent from default/release builds. The fixed
    /// fixture origin carries no account credentials; caller maps it to loopback.
    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn synthetic_native_package(
        request: UploadRequest,
        folder: Node,
        operation: Uuid,
        staging: &Path,
        client: reqwest::Client,
    ) -> UploadResult<Self> {
        Self::native_identity(&request, &folder)?;
        let endpoint: url::Url = "https://fixture.icloud-content.com/"
            .parse()
            .map_err(|_| UploadError::Invalid)?;
        let account_hash =
            crate::account_hash("fixture@example.com").map_err(|_| UploadError::Invalid)?;
        let mut session =
            ICloudReadSession::new().map_err(crate::file_create::map_session_error)?;
        session.account_hash = Some(account_hash.clone());
        session.http = client.clone();
        session.drive_endpoint = Some(endpoint.clone());
        session.docs_endpoint = Some(endpoint.clone());
        let provider = ICloudPackageCreate::native_handoff_test_provider(
            request.scope.clone(),
            folder.clone(),
            staging,
            session,
        );
        let mut owner = Self::with_native(
            request,
            folder,
            operation,
            SessionSource::Snapshot {
                apple_id: "fixture@example.com".into(),
                snapshot: SecretString::from("synthetic-only"),
            },
            provider,
            staging,
            account_hash,
        )?;
        owner
            .native
            .as_mut()
            .ok_or(UploadError::Invalid)?
            .fixture_transport = Some((client, endpoint));
        Ok(owner)
    }
    /// Explicit archive replacement only. Normal filesystem writes do not use
    /// this constructor. The old identity is recovered in Trash, not overwritten.
    #[allow(clippy::too_many_arguments)]
    pub fn native_package_from_session_snapshot(
        request: UploadRequest,
        folder: Node,
        operation: Uuid,
        apple_id: String,
        snapshot: SecretString,
        staging: &Path,
    ) -> UploadResult<Self> {
        Self::native_identity(&request, &folder)?;
        let provider = ICloudPackageCreate::from_session_snapshot(
            request.scope.clone(),
            &apple_id,
            &snapshot,
            folder.clone(),
            staging,
        )?;
        let account_hash = crate::account_hash(&apple_id).map_err(|_| UploadError::Invalid)?;
        Self::with_native(
            request,
            folder,
            operation,
            SessionSource::Snapshot { apple_id, snapshot },
            provider,
            staging,
            account_hash,
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub fn native_package_from_sealed_session(
        request: UploadRequest,
        folder: Node,
        operation: Uuid,
        sign_in: ICloudSealedSignIn,
        state: &Path,
        staging: &Path,
    ) -> UploadResult<Self> {
        Self::native_identity(&request, &folder)?;
        let provider = ICloudPackageCreate::from_sealed_session(
            request.scope.clone(),
            sign_in.apple_id.clone(),
            sign_in.credential_id.clone(),
            state,
            folder.clone(),
            staging,
        )?;
        let account_hash =
            crate::account_hash(&sign_in.apple_id).map_err(|_| UploadError::Invalid)?;
        let vault = SealedSessionVault::new(state, &request.scope.account)
            .map_err(|_| UploadError::Invalid)?;
        Self::with_native(
            request,
            folder,
            operation,
            SessionSource::Vault {
                apple_id: sign_in.apple_id,
                credential_id: sign_in.credential_id,
                vault: Arc::new(vault),
            },
            provider,
            staging,
            account_hash,
        )
    }
    /// Local restore binds the persisted parent without querying mutable cloud
    /// metadata. Parsing never loads credentials or authorizes a new allocation.
    #[allow(clippy::too_many_arguments)]
    pub fn restore_native_package_from_sealed_checkpoint(
        request: UploadRequest,
        operation: Uuid,
        sign_in: ICloudSealedSignIn,
        state: &Path,
        staging: &Path,
        checkpoint: &SecretString,
    ) -> UploadResult<Self> {
        if checkpoint.expose_secret().len() > LIMIT {
            return Err(UploadError::CheckpointInvalid);
        }
        let saved: Saved = serde_json::from_str(checkpoint.expose_secret())
            .map_err(|_| UploadError::CheckpointInvalid)?;
        if saved.version != 1
            || saved.operation != operation
            || saved.request != request
            || saved.account_hash
                != crate::account_hash(&sign_in.apple_id).map_err(|_| UploadError::Invalid)?
        {
            return Err(UploadError::CheckpointInvalid);
        }
        let owner = Self::native_package_from_sealed_session(
            request.clone(),
            saved.parent,
            operation,
            sign_in,
            state,
            staging,
        )?;
        owner.native_decode(&operation.to_string(), &request, checkpoint)?;
        Ok(owner)
    }
    #[allow(clippy::too_many_arguments)]
    fn with_native(
        request: UploadRequest,
        folder: Node,
        operation: Uuid,
        session: SessionSource,
        provider: ICloudPackageCreate,
        staging: &Path,
        account_hash: String,
    ) -> UploadResult<Self> {
        let UploadRepresentation::PackageReplacementArchive { original, .. } =
            &request.representation
        else {
            return Err(UploadError::Invalid);
        };
        let this = Self {
            scope: request.scope.clone(),
            folder,
            original: *original.clone(),
            original_sha256: None,
            operation,
            stage_name: format!("staged-by-cirrove-{operation}.pages"),
            recovery_name: format!("recovery-by-cirrove-{operation}.pages"),
            session,
            stage: None,
            native: Some(Context {
                request,
                provider,
                staging: staging.into(),
                account_hash,
                #[cfg(any(test, feature = "test-support"))]
                fixture_transport: None,
            }),
        };
        // Reserve fixed header capacity before allocation. Oversized provider
        // continuations still fail closed against the previously persisted arm;
        // a missing allocation response is never authority to allocate again.
        if this
            .native_encode(NativePhase::Stage { inner: Value::Null })?
            .expose_secret()
            .len()
            > HEADER_LIMIT
        {
            return Err(UploadError::Invalid);
        }
        Ok(this)
    }
    fn native_identity(request: &UploadRequest, parent: &Node) -> UploadResult<()> {
        request.validate()?;
        let UploadRepresentation::PackageReplacementArchive {
            original,
            expected_root,
            ..
        } = &request.representation
        else {
            return Err(UploadError::Invalid);
        };
        if request.scope.provider != "icloud"
            || request.scope.collection != "drive"
            || Uuid::parse_str(&request.scope.account).is_err()
            || !expected_root.ends_with(".pages")
            || parent.name.is_empty()
            || parent.name.len() > 255
            || parent.name.contains(['/', '\0', '\r', '\n'])
            || matches!(parent.name.as_str(), "." | "..")
            || (parent.id != ROOT_ID
                && parent.parent_id.as_deref().is_none_or(|id| {
                    !id.starts_with("FOLDER::com.apple.CloudDocs::")
                        || id == parent.id
                        || id == TRASH_ROOT
                }))
            || parent.kind != NodeKind::Folder
            || parent.package
            || parent.target.is_some()
            || !parent.id.starts_with("FOLDER::com.apple.CloudDocs::")
            || parent.id == TRASH_ROOT
            || original.parent_id.as_deref() != Some(parent.id.as_str())
            || !original.id.starts_with("FILE::com.apple.CloudDocs::")
            || original.name.len() > 255
            || !original.name.ends_with(".pages")
            || original
                .name
                .chars()
                .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
            || request.size == 0
            || request.size > 64 * 1024 * 1024
        {
            return Err(UploadError::Invalid);
        }
        Ok(())
    }
    fn native_check(&self, op: &str, request: &UploadRequest) -> UploadResult<&Context> {
        let native = self.native.as_ref().ok_or(UploadError::Invalid)?;
        if op != self.operation.to_string() || request != &native.request {
            return Err(UploadError::CheckpointInvalid);
        }
        Self::native_identity(request, &self.folder)?;
        Ok(native)
    }
    fn native_stage_request(&self) -> UploadResult<UploadRequest> {
        let context = self.native.as_ref().ok_or(UploadError::Invalid)?;
        let UploadRepresentation::PackageReplacementArchive {
            expected_root,
            semantic,
            ..
        } = &context.request.representation
        else {
            return Err(UploadError::Invalid);
        };
        Ok(UploadRequest {
            representation: UploadRepresentation::PackageArchive {
                expected_root: expected_root.clone(),
                semantic: semantic.clone(),
            },
            scope: self.scope.clone(),
            intent: UploadIntent::Create {
                parent: self.folder.id.clone(),
                name: self.stage_name.clone(),
            },
            size: context.request.size,
            sha256: context.request.sha256.clone(),
        })
    }
    fn native_encode(&self, phase: NativePhase) -> UploadResult<SecretString> {
        let native = self.native.as_ref().ok_or(UploadError::Invalid)?;
        let saved = Saved {
            version: 1,
            operation: self.operation,
            request: native.request.clone(),
            parent: self.folder.clone(),
            account_hash: native.account_hash.clone(),
            phase,
        };
        let text = serde_json::to_string(&saved).map_err(|_| UploadError::Invalid)?;
        if text.len() > LIMIT {
            return Err(UploadError::Uncertain);
        }
        Ok(text.into())
    }
    fn native_decode(
        &self,
        op: &str,
        request: &UploadRequest,
        checkpoint: &SecretString,
    ) -> UploadResult<NativePhase> {
        let context = self.native_check(op, request)?;
        if checkpoint.expose_secret().len() > LIMIT {
            return Err(UploadError::CheckpointInvalid);
        }
        let saved: Saved = serde_json::from_str(checkpoint.expose_secret())
            .map_err(|_| UploadError::CheckpointInvalid)?;
        if saved.version != 1
            || saved.operation != self.operation
            || saved.request != *request
            || saved.parent != self.folder
            || saved.account_hash != context.account_hash
        {
            return Err(UploadError::CheckpointInvalid);
        }
        if let NativePhase::Handoff { plan, phase } = &saved.phase {
            if *phase == HandoffPhase::InstallNew {
                return Err(UploadError::CheckpointInvalid);
            }
            self.native_check_plan(plan)?;
        }
        Ok(saved.phase)
    }
    pub(super) fn native_recovery_location(
        &self,
        op: &str,
        r: &UploadRequest,
    ) -> Option<RecoveryLocation> {
        self.native_check(op, r).ok()?;
        Some(RecoveryLocation::Trash {
            local_name: self.recovery_name.clone(),
            parent: TRASH_ROOT.into(),
        })
    }
    fn native_check_plan(&self, plan: &HandoffPlan) -> UploadResult<()> {
        let context = self.native.as_ref().ok_or(UploadError::Invalid)?;
        let UploadRepresentation::PackageReplacementArchive {
            semantic,
            original_semantic,
            ..
        } = &context.request.representation
        else {
            return Err(UploadError::Invalid);
        };
        plan.validate()
            .map_err(|_| UploadError::CheckpointInvalid)?;
        let proof = plan
            .package
            .as_ref()
            .ok_or(UploadError::CheckpointInvalid)?;
        if plan.version != if self.folder.id == ROOT_ID { 7 } else { 6 }
            || plan.folder_id != self.folder.id
            || plan.folder_name != self.folder.name
            || plan.folder_parent_id != self.folder.parent_id.clone().unwrap_or_default()
            || plan.original_id != self.original.id
            || Some(plan.original_etag.as_str()) != self.original.etag.as_deref()
            || plan.target_name != self.original.name
            || plan.staged_name != self.stage_name
            || plan.recovery_name != self.recovery_name
            || proof.account_hash != context.account_hash
            || proof.original != *original_semantic
            || proof.staged != *semantic
            || proof.original_size != self.original.size
        {
            return Err(UploadError::CheckpointInvalid);
        }
        Ok(())
    }
    fn native_plan(&self, receipt: PackageUploadReceipt) -> UploadResult<HandoffPlan> {
        let context = self.native.as_ref().ok_or(UploadError::Invalid)?;
        let UploadRepresentation::PackageReplacementArchive {
            semantic,
            original_semantic,
            ..
        } = &context.request.representation
        else {
            return Err(UploadError::Invalid);
        };
        let staged = receipt.remote;
        if receipt.semantic != *semantic
            || staged.kind != NodeKind::Folder
            || !staged.package
            || staged.target.is_some()
            || staged.content_version.is_some()
            || staged.parent_id.as_deref() != Some(self.folder.id.as_str())
            || staged.name != self.stage_name
            || staged.id == self.original.id
        {
            return Err(UploadError::Conflict);
        }
        let plan = HandoffPlan {
            version: if self.folder.id == ROOT_ID { 7 } else { 6 },
            folder_parent_id: self.folder.parent_id.clone().unwrap_or_default(),
            folder_id: self.folder.id.clone(),
            folder_name: self.folder.name.clone(),
            original_id: self.original.id.clone(),
            original_doc_id: self
                .original
                .id
                .rsplit("::")
                .next()
                .unwrap_or_default()
                .into(),
            original_etag: self.original.etag.clone().ok_or(UploadError::Invalid)?,
            staged_doc_id: staged.id.rsplit("::").next().unwrap_or_default().into(),
            staged_id: staged.id,
            staged_etag: staged.etag.ok_or(UploadError::Conflict)?,
            staged_name: self.stage_name.clone(),
            recovery_name: self.recovery_name.clone(),
            target_name: self.original.name.clone(),
            original_sha256: String::new(),
            staged_sha256: String::new(),
            package: Some(NativePackageProof {
                version: 1,
                account_hash: context.account_hash.clone(),
                original: original_semantic.clone(),
                staged: semantic.clone(),
                original_size: self.original.size,
                staged_size: staged.size,
            }),
        };
        self.native_check_plan(&plan)?;
        Ok(plan)
    }
    async fn native_session(&self, cancel: &CancellationToken) -> UploadResult<ICloudReadSession> {
        #[cfg(any(test, feature = "test-support"))]
        if let Some((client, endpoint)) = self
            .native
            .as_ref()
            .and_then(|n| n.fixture_transport.as_ref())
        {
            if cancel.is_cancelled() {
                return Err(UploadError::Uncertain);
            }
            let mut session =
                ICloudReadSession::new().map_err(crate::file_create::map_session_error)?;
            session.account_hash = self.native.as_ref().map(|n| n.account_hash.clone());
            session.http = client.clone();
            session.drive_endpoint = Some(endpoint.clone());
            session.docs_endpoint = Some(endpoint.clone());
            return Ok(session);
        }
        let session = self.load_session().await?;
        if cancel.is_cancelled() {
            return Err(UploadError::Uncertain);
        }
        if session.account_hash.as_deref() != self.native.as_ref().map(|v| v.account_hash.as_str())
        {
            return Err(UploadError::Invalid);
        }
        Ok(session)
    }
    async fn native_before_stage(&self, cancel: &CancellationToken) -> UploadResult<()> {
        let context = self.native.as_ref().ok_or(UploadError::Invalid)?;
        let UploadRepresentation::PackageReplacementArchive {
            original_semantic, ..
        } = &context.request.representation
        else {
            return Err(UploadError::Invalid);
        };
        self.native_session(cancel)
            .await?
            .verify_native_replacement_original(
                &self.folder,
                &self.original,
                original_semantic,
                &context.staging,
                cancel,
            )
            .await
            .map_err(crate::file_create::map_session_error)
    }
    async fn native_wrap_stage(
        &self,
        step: UploadStep,
        cancel: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        Ok(match step {
            UploadStep::Allocate(inner) => {
                UploadStep::Allocate(self.native_encode(NativePhase::Stage {
                    inner: pack(inner)?,
                })?)
            }
            UploadStep::Stream(inner) => {
                UploadStep::Stream(self.native_encode(NativePhase::Stage {
                    inner: pack(inner)?,
                })?)
            }
            UploadStep::Commit(inner) => {
                UploadStep::Commit(self.native_encode(NativePhase::Stage {
                    inner: pack(inner)?,
                })?)
            }
            UploadStep::PackageComplete(receipt) => {
                let plan = self.native_plan(receipt)?;
                let context = self.native.as_ref().ok_or(UploadError::Invalid)?;
                let state = self
                    .native_session(cancel)
                    .await?
                    .inspect_native_package_handoff(&plan, &context.staging, cancel)
                    .await
                    .map_err(crate::file_create::map_session_error)?;
                if state != HandoffObserved::Prepared {
                    return Err(UploadError::Conflict);
                }
                UploadStep::Commit(self.native_encode(NativePhase::Handoff {
                    plan: Box::new(plan),
                    phase: HandoffPhase::MoveOld,
                })?)
            }
            _ => return Err(UploadError::Invalid),
        })
    }
    async fn native_observe(
        &self,
        plan: &HandoffPlan,
        cancel: &CancellationToken,
    ) -> UploadResult<(HandoffObserved, Option<UploadStep>)> {
        self.native_check_plan(plan)?;
        let context = self.native.as_ref().ok_or(UploadError::Invalid)?;
        let (state, nodes) = self
            .native_session(cancel)
            .await?
            .inspect_native_handoff_inner(plan, &context.staging, cancel)
            .await
            .map_err(crate::file_create::map_session_error)?;
        let receipt = nodes
            .map(|(current, backup)| {
                let proof = plan
                    .package
                    .as_ref()
                    .ok_or(UploadError::CheckpointInvalid)?;
                if current.size != proof.staged_size || backup.size != proof.original_size {
                    return Err(UploadError::Conflict);
                }
                Ok(UploadStep::PackageHandoffComplete(Box::new(
                    PackageHandoffReceipt {
                        original: self.original.clone(),
                        current: PackageUploadReceipt {
                            remote: current,
                            semantic: proof.staged.clone(),
                        },
                        backup: PackageUploadReceipt {
                            remote: backup,
                            semantic: proof.original.clone(),
                        },
                    },
                )))
            })
            .transpose()?;
        Ok((state, receipt))
    }
    async fn native_resume_handoff(
        &self,
        plan: Box<HandoffPlan>,
        phase: HandoffPhase,
        cancel: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        let (state, receipt) = self.native_observe(&plan, cancel).await?;
        if let Some(receipt) = receipt {
            return Ok(receipt);
        }
        match (phase, state) {
            (HandoffPhase::MoveOld, HandoffObserved::OldAtRecovery) => Ok(UploadStep::Commit(
                self.native_encode(NativePhase::Handoff {
                    plan,
                    phase: HandoffPhase::InspectInstall,
                })?,
            )),
            (HandoffPhase::InspectInstall, HandoffObserved::OldAtRecovery) => Ok(
                UploadStep::Commit(self.native_encode(NativePhase::Handoff {
                    plan,
                    phase: HandoffPhase::InstallInspected,
                })?),
            ),
            (_, HandoffObserved::Diverged) => Err(UploadError::Conflict),
            _ => Err(UploadError::Uncertain),
        }
    }
    pub(super) async fn native_begin(
        &self,
        op: &str,
        r: &UploadRequest,
        c: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        let context = self.native_check(op, r)?;
        tokio::select! {biased; _ = c.cancelled() => Err(UploadError::Uncertain), result = async {
            self.native_before_stage(c).await?;
            let step = context.provider.begin_upload_for_operation(op, &self.native_stage_request()?, c).await?;
            self.native_wrap_stage(step,c).await
        } => result}
    }
    pub(super) async fn native_allocate(
        &self,
        op: &str,
        r: &UploadRequest,
        checkpoint: &SecretString,
        c: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        let NativePhase::Stage { inner } = self.native_decode(op, r, checkpoint)? else {
            return Err(UploadError::CheckpointInvalid);
        };
        let context = self.native_check(op, r)?;
        tokio::select! {biased; _ = c.cancelled() => Err(UploadError::Uncertain), result = async {
            self.native_before_stage(c).await?;
            let step = context.provider.allocate_upload_for_operation(op,&self.native_stage_request()?,&unpack(inner)?,c).await?;
            self.native_wrap_stage(step,c).await
        } => result}
    }
    pub(super) async fn native_stream(
        &self,
        op: &str,
        r: &UploadRequest,
        checkpoint: &SecretString,
        file: File,
        c: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        let NativePhase::Stage { inner } = self.native_decode(op, r, checkpoint)? else {
            return Err(UploadError::CheckpointInvalid);
        };
        let context = self.native_check(op, r)?;
        let step = context
            .provider
            .upload_stream_for_operation(
                op,
                &self.native_stage_request()?,
                &unpack(inner)?,
                file,
                c,
            )
            .await?;
        self.native_wrap_stage(step, c).await
    }
    pub(super) async fn native_inspect(
        &self,
        op: &str,
        r: &UploadRequest,
        checkpoint: &SecretString,
        c: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        let phase = self.native_decode(op, r, checkpoint)?;
        tokio::select! {biased; _ = c.cancelled() => Err(UploadError::Uncertain), result = async {
            match phase {
                NativePhase::Stage {inner} => {
                    let context = self.native_check(op,r)?;
                    let step = context.provider.inspect_upload_for_operation(op,&self.native_stage_request()?,&unpack(inner)?,c).await?;
                    self.native_wrap_stage(step,c).await
                },
                NativePhase::Handoff {plan,phase} => self.native_resume_handoff(plan,phase,c).await,
            }
        } => result}
    }
    pub(super) async fn native_commit(
        &self,
        op: &str,
        r: &UploadRequest,
        checkpoint: &SecretString,
        c: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        let phase = self.native_decode(op, r, checkpoint)?;
        tokio::select! {biased; _ = c.cancelled() => Err(UploadError::Uncertain), result = async {
            match phase {
                NativePhase::Stage {inner} => {
                    let context = self.native_check(op,r)?;
                    let step = context.provider.commit_upload_for_operation(op,&self.native_stage_request()?,&unpack(inner)?,c).await?;
                    self.native_wrap_stage(step,c).await
                },
                NativePhase::Handoff {plan,phase} => {
                    let context = self.native_check(op,r)?;
                    match phase {
                        HandoffPhase::MoveOld => {
                            let mut session = self.native_session(c).await?;
                            if c.is_cancelled() {return Err(UploadError::Uncertain)}
                            if !session.trash_native_handoff_original(&plan,&context.staging,c).await.map_err(crate::file_create::map_session_error)? {return Err(UploadError::Conflict)}
                            self.native_resume_handoff(plan,phase,c).await
                        },
                        HandoffPhase::InspectInstall => self.native_resume_handoff(plan,phase,c).await,
                        HandoffPhase::InstallInspected => {
                            let mut session = self.native_session(c).await?;
                            if c.is_cancelled() {return Err(UploadError::Uncertain)}
                            if !session.install_native_handoff_stage(&plan,&context.staging,c).await.map_err(crate::file_create::map_session_error)? {return Err(UploadError::Conflict)}
                            self.native_resume_handoff(plan,phase,c).await
                        },
                        HandoffPhase::InstallNew => Err(UploadError::CheckpointInvalid),
                    }
                }
            }
        } => result}
    }
    pub(super) async fn native_reconcile(
        &self,
        op: &str,
        r: &UploadRequest,
        checkpoint: Option<&SecretString>,
        c: &CancellationToken,
    ) -> UploadResult<Reconciliation> {
        self.native_check(op, r)?;
        // Even without a returned allocation slot, dispatch might have reached
        // Apple. Never convert missing evidence to permission to create again.
        let Some(checkpoint) = checkpoint else {
            return Err(UploadError::Uncertain);
        };
        match self.native_inspect(op, r, checkpoint, c).await {
            Ok(UploadStep::PackageHandoffComplete(receipt)) => {
                Ok(Reconciliation::PackageHandoffCommitted(receipt))
            }
            Err(UploadError::Conflict) => Ok(Reconciliation::Conflict),
            Ok(_) => Err(UploadError::Uncertain),
            Err(e) => Err(e),
        }
    }
}
#[cfg(test)]
mod tests;

#[cfg(feature = "write-probe")]
impl ICloudFileReplace {
    /// Sanitized local evidence only. Does not resume or advance any checkpoint.
    pub fn native_checkpoint_diagnostic(
        &self,
        operation: &str,
        request: &UploadRequest,
        checkpoint: &SecretString,
    ) -> UploadResult<serde_json::Value> {
        let phase = self.native_decode(operation, request, checkpoint)?;
        let (phase, staged) = match phase {
            NativePhase::Stage { inner } => self.native_stage_diagnostic(operation, &inner)?,
            NativePhase::Handoff { plan, phase } => {
                let category = match phase {
                    HandoffPhase::MoveOld => "handoff-move-old-armed",
                    HandoffPhase::InspectInstall => "handoff-inspect-install",
                    HandoffPhase::InstallInspected => "handoff-install-armed",
                    HandoffPhase::InstallNew => return Err(UploadError::CheckpointInvalid),
                };
                (category, Some(plan.staged_id))
            }
        };
        Ok(
            serde_json::json!({"phase":phase,"original_id":self.original.id,"staged_id":staged,"parent_id":self.folder.id,"stage_semantically_verified_before_handoff":phase.starts_with("handoff-"),"captured_original_etag_has_wildcard":self.original.etag.as_ref().is_some_and(|v|v.contains('*'))}),
        )
    }
    fn native_stage_diagnostic(
        &self,
        operation: &str,
        inner: &Value,
    ) -> UploadResult<(&'static str, Option<String>)> {
        if inner.is_null() {
            return Ok(("stage-unallocated", None));
        }
        let context = self.native.as_ref().ok_or(UploadError::Invalid)?;
        let summary = context.provider.diagnostic_checkpoint(
            operation,
            &self.native_stage_request()?,
            &unpack(inner.clone())?,
            &context.account_hash,
        )?;
        if summary.1.as_ref() == Some(&self.original.id) {
            return Err(UploadError::CheckpointInvalid);
        }
        Ok(summary)
    }
    /// Two exact owned-ID metadata reads; a mismatch/absence makes no claim of
    /// successful removal. Returned fields contain no remote names or values.
    pub async fn native_checkpoint_locations_read_only(
        &self,
        operation: &str,
        request: &UploadRequest,
        checkpoint: &SecretString,
        cancel: &CancellationToken,
    ) -> UploadResult<serde_json::Value> {
        let phase = self.native_decode(operation, request, checkpoint)?;
        let mut targets = vec![(
            "original",
            self.original.id.clone(),
            self.original.etag.clone(),
            self.original.name.clone(),
            Some(self.original.size),
        )];
        match phase {
            NativePhase::Stage { inner } => {
                if let Some(id) = self.native_stage_diagnostic(operation, &inner)?.1 {
                    targets.push(("staged", id, None, self.stage_name.clone(), None));
                }
            }
            NativePhase::Handoff { plan, .. } => {
                let size = plan.package.as_ref().map(|p| p.staged_size);
                targets.push((
                    "staged",
                    plan.staged_id,
                    Some(plan.staged_etag),
                    self.stage_name.clone(),
                    size,
                ));
            }
        }
        let mut session = self.native_session(cancel).await?;
        let mut rows = Vec::new();
        for (role, id, etag, name, size) in targets {
            if cancel.is_cancelled() {
                return Err(UploadError::Uncertain);
            }
            let observed = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                session.item_details(&id),
            )
            .await;
            let summary = match observed {
                Ok(Ok(value)) => {
                    diagnostic_location(value, &id, etag.as_deref(), &self.folder.id, &name, size)
                }
                _ => serde_json::json!({"category":"unavailable-no-location-conclusion"}),
            };
            rows.push(serde_json::json!({"role":role,"observation":summary}));
        }
        Ok(serde_json::json!({"items":rows,"metadata_only":true}))
    }
    /// Inspect an armed stage without advancing any phase. Source bytes must be
    /// the exact request-bound archive; this method never saves a continuation.
    pub async fn inspect_native_stage_read_only(
        &self,
        operation: &str,
        request: &UploadRequest,
        checkpoint: &SecretString,
        source: std::fs::File,
        cancel: &CancellationToken,
    ) -> UploadResult<Value> {
        let phase = self.native_decode(operation, request, checkpoint)?;
        let NativePhase::Stage { inner } = phase else {
            return Ok(serde_json::json!({"category":"not-stage"}));
        };
        // Validates inner account/request/operation before loading credentials.
        self.native_stage_diagnostic(operation, &inner)?;
        let context = self.native.as_ref().ok_or(UploadError::Invalid)?;
        let UploadRepresentation::PackageReplacementArchive {
            expected_root,
            semantic,
            ..
        } = &request.representation
        else {
            return Err(UploadError::Invalid);
        };
        let source_check = source.try_clone().map_err(|_| UploadError::Invalid)?;
        let root = expected_root.clone();
        let version = semantic.version;
        let source_receipt = crate::PackageDownload {
            size: request.size,
            sha256: request.sha256.clone(),
        };
        let token = cancel.clone();
        let actual = tokio::task::spawn_blocking(move || {
            crate::package_archive_semantic_identity_versioned(
                &source_check,
                &source_receipt,
                &root,
                version,
                &token,
            )
        })
        .await
        .map_err(|_| UploadError::Uncertain)??;
        if &actual != semantic {
            return Err(UploadError::Invalid);
        }
        let (verification, receipt) = context
            .provider
            .diagnostic_inspect(
                operation,
                &self.native_stage_request()?,
                &unpack(inner)?,
                source,
                expected_root.clone(),
                cancel,
            )
            .await?;
        let Some(receipt) = receipt else {
            return Ok(
                serde_json::json!({"stage_verification":verification,"handoff":"not-inspected-stage-unverified"}),
            );
        };
        let plan = match self.native_plan(receipt) {
            Ok(plan) => plan,
            Err(_) => {
                return Ok(
                    serde_json::json!({"stage_verification":verification,"handoff":"plan-binding-refused"}),
                );
            }
        };
        let handoff = match self.native_observe(&plan, cancel).await {
            Ok((HandoffObserved::Prepared, _)) => "original-active-stage-verified",
            Ok((HandoffObserved::OldAtRecovery, _)) => "original-recoverable-stage-verified",
            Ok((HandoffObserved::Complete, _)) => "replacement-and-recovery-verified",
            Ok((HandoffObserved::Diverged, _)) => "diverged",
            Err(_) => "observation-unavailable",
        };
        Ok(serde_json::json!({"stage_verification":verification,"handoff":handoff}))
    }
    /// Handoff inspection uses metadata/download/semantic verification ONLY.
    /// Deliberately does not invoke commit, resume, save, retry or returned steps.
    pub async fn inspect_native_checkpoint_read_only(
        &self,
        operation: &str,
        request: &UploadRequest,
        checkpoint: &SecretString,
        cancel: &CancellationToken,
    ) -> UploadResult<&'static str> {
        match self.native_decode(operation, request, checkpoint)? {
            NativePhase::Stage { .. } => Ok("stage-not-inspected"),
            NativePhase::Handoff { plan, .. } => {
                let (state, _) = self.native_observe(&plan, cancel).await?;
                Ok(match state {
                    HandoffObserved::Prepared => "original-active-stage-verified",
                    HandoffObserved::OldAtRecovery => "original-recoverable-stage-verified",
                    HandoffObserved::Complete => "replacement-and-recovery-verified",
                    HandoffObserved::Diverged => "diverged-no-location-conclusion",
                })
            }
        }
    }
}

#[cfg(feature = "write-probe")]
fn diagnostic_location(
    value: serde_json::Value,
    id: &str,
    etag: Option<&str>,
    parent: &str,
    name: &str,
    logical_size: Option<u64>,
) -> serde_json::Value {
    let restore = value.get("restorePath").is_some_and(|v| !v.is_null());
    let Ok(entry) = serde_json::from_value::<crate::DriveEntry>(value) else {
        return serde_json::json!({"category":"invalid-no-location-conclusion"});
    };
    if entry.drivewsid != id
        || entry.docwsid != id.rsplit("::").next().unwrap_or_default()
        || entry.zone != "com.apple.CloudDocs"
        || entry.kind != "FILE"
    {
        return serde_json::json!({"category":"identity-refused-no-location-conclusion","expected_doc_id_matches":entry.docwsid==id.rsplit("::").next().unwrap_or_default()});
    }
    let location = if entry.parent_id == TRASH_ROOT {
        "trash-parent"
    } else if entry.parent_id == parent {
        "owned-active-parent"
    } else {
        "other-parent"
    };
    serde_json::json!({"category":location,"captured_revision_matches":etag.map(|v|entry.etag==v),"captured_etag_has_wildcard":etag.map(|v|v.contains('*')),"observed_etag_has_wildcard":entry.etag.contains('*'),"restore_marker_present":restore,"expected_name_matches":entry.display_name()==name,"expected_logical_size_matches":logical_size.map(|n|entry.size==n),"expected_doc_id_matches":true})
}
#[cfg(all(test, feature = "write-probe"))]
mod diagnostic_tests {
    use super::*;
    #[test]
    fn diagnostic_metadata_is_exact_and_never_serializes_untrusted_values() {
        let id = "FILE::com.apple.CloudDocs::owned";
        let value = serde_json::json!({"drivewsid":id,"docwsid":"owned","zone":"com.apple.CloudDocs","type":"FILE","parentId":TRASH_ROOT,"etag":"PRIVATE-REVISION","name":"PRIVATE-NAME","url":"PRIVATE-URL","restorePath":"PRIVATE-PATH"});
        let summary = diagnostic_location(
            value.clone(),
            id,
            Some("captured"),
            "FOLDER::com.apple.CloudDocs::owned",
            "expected-name",
            Some(0),
        );
        assert_eq!(summary["category"], "trash-parent");
        assert_eq!(summary["captured_revision_matches"], false);
        assert!(!summary.to_string().contains("PRIVATE"));
        assert_eq!(
            diagnostic_location(
                value,
                "FILE::com.apple.CloudDocs::different",
                Some("captured"),
                "parent",
                "expected-name",
                None
            )["category"],
            "identity-refused-no-location-conclusion"
        );
    }
}

pub(super) mod abandon;
