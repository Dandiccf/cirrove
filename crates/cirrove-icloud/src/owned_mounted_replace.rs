//! Journal-checkpointed staging and two-ID replacement. Account grants remain
//! gated until the normal router and live acceptance cover this path.
use super::{
    HandoffPlan, ICloudFileCreate, ICloudHandoff, ICloudReadSession, ROOT_ID, SealedSessionVault,
    write_transport::TRASH_ROOT,
};
#[cfg(feature = "write-probe")]
use crate::ValidationFolder;
use async_trait::async_trait;
use cirrove_auth::CredentialVault;
use cirrove_core::upload::{
    Reconciliation, RecoveryLocation, Result as UploadResult, UploadError, UploadIntent,
    UploadProgress, UploadProvider, UploadRequest, UploadStep,
};
use cirrove_core::{CancellationToken, Node, NodeKind, Scope};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::{fs::File, path::Path, sync::Arc};
use uuid::Uuid;

const MAX_CHECKPOINT: usize = 32 * 1024;

#[derive(Serialize, Deserialize)]
enum Phase {
    Stage {
        inner: String,
    },
    Handoff {
        inner: String,
        plan: Box<HandoffPlan>,
    },
}

#[derive(Serialize, Deserialize)]
struct Checkpoint {
    version: u8,
    scope: Scope,
    operation: Uuid,
    original_id: String,
    original_etag: String,
    #[serde(default)]
    folder: Option<Node>,
    #[serde(default)]
    original: Option<Node>,
    #[serde(default)]
    original_sha256: String,
    size: u64,
    sha256: String,
    phase: Phase,
}

pub struct ICloudFileReplace {
    scope: Scope,
    folder: Node,
    original: Node,
    original_sha256: Option<String>,
    operation: Uuid,
    stage_name: String,
    recovery_name: String,
    session: SessionSource,
    stage: ICloudFileCreate,
}

enum SessionSource {
    Snapshot {
        apple_id: String,
        snapshot: SecretString,
    },
    Vault {
        apple_id: String,
        credential_id: String,
        vault: Arc<dyn CredentialVault>,
    },
}

pub struct ICloudSealedSignIn {
    pub apple_id: String,
    pub credential_id: String,
}

impl ICloudFileReplace {
    #[cfg(feature = "write-probe")]
    pub fn with_discarded_stage_registration_response(mut self) -> Self {
        self.stage = self.stage.with_discarded_registration_response();
        self
    }

    pub fn parent_id(&self) -> &str {
        &self.folder.id
    }

    pub fn recovery_location(operation: Uuid) -> RecoveryLocation {
        RecoveryLocation::Trash {
            local_name: format!("recovery-by-cirrove-{operation}.txt"),
            parent: TRASH_ROOT.into(),
        }
    }

    #[cfg(feature = "write-probe")]
    pub fn new(
        scope: Scope,
        folder: ValidationFolder,
        original: Node,
        original_sha256: String,
        operation: Uuid,
        apple_id: String,
        snapshot: SecretString,
    ) -> UploadResult<Self> {
        let folder = Node {
            id: folder.id().into(),
            parent_id: Some(ROOT_ID.into()),
            name: folder.name().into(),
            kind: NodeKind::Folder,
            size: 0,
            modified_unix: 0,
            etag: None,
            content_version: None,
            target: None,
            package: false,
        };
        Self::new_in_folder(
            scope,
            folder,
            original,
            original_sha256,
            operation,
            apple_id,
            snapshot,
        )
    }

    /// A confirmed child of the isolated owned fixture can use the same
    /// replacement journal without treating its name as its identity.
    pub fn new_in_folder(
        scope: Scope,
        folder: Node,
        original: Node,
        original_sha256: String,
        operation: Uuid,
        apple_id: String,
        snapshot: SecretString,
    ) -> UploadResult<Self> {
        Self::check_identity(&scope, &folder, &original, Some(&original_sha256))?;
        let stage = ICloudFileCreate::from_session_snapshot(
            scope.clone(),
            &apple_id,
            &snapshot,
            folder.clone(),
        )?;
        Ok(Self::with_stage(
            scope,
            folder,
            original,
            Some(original_sha256),
            operation,
            SessionSource::Snapshot { apple_id, snapshot },
            stage,
        ))
    }

    pub fn from_sealed_session_in_folder(
        scope: Scope,
        folder: Node,
        original: Node,
        original_sha256: String,
        operation: Uuid,
        sign_in: ICloudSealedSignIn,
        state: &Path,
    ) -> UploadResult<Self> {
        Self::from_sealed_session_with_digest(
            scope,
            folder,
            original,
            Some(original_sha256),
            operation,
            sign_in,
            state,
        )
    }

    /// Existing files have no prior Cirrove upload receipt. Obtain their
    /// original digest from a version-checked remote read before staging.
    pub fn from_sealed_session_for_existing(
        scope: Scope,
        folder: Node,
        original: Node,
        operation: Uuid,
        sign_in: ICloudSealedSignIn,
        state: &Path,
    ) -> UploadResult<Self> {
        Self::from_sealed_session_with_digest(
            scope, folder, original, None, operation, sign_in, state,
        )
    }

    /// Restore a mutation's captured source after it has left the active index.
    /// Only an authenticated, per-operation sealed checkpoint may be supplied.
    /// Legacy checkpoints return None and still require the original index entry.
    pub fn from_sealed_checkpoint(
        request: &UploadRequest,
        operation: Uuid,
        checkpoint: &SecretString,
        sign_in: ICloudSealedSignIn,
        state: &Path,
    ) -> UploadResult<Option<Self>> {
        if checkpoint.expose_secret().len() > MAX_CHECKPOINT {
            return Err(UploadError::CheckpointInvalid);
        }
        let saved: Checkpoint = serde_json::from_str(checkpoint.expose_secret())
            .map_err(|_| UploadError::CheckpointInvalid)?;
        request.validate()?;
        if saved.scope != request.scope
            || saved.operation != operation
            || saved.size != request.size
            || saved.sha256 != request.sha256
            || !matches!(&request.intent, UploadIntent::Replace { item, expected_etag }
                if item == &saved.original_id && expected_etag == &saved.original_etag)
        {
            return Err(UploadError::CheckpointInvalid);
        }
        if saved.version == 1 {
            return Ok(None);
        }
        if saved.version != 2 {
            return Err(UploadError::CheckpointInvalid);
        }
        let provider = Self::from_sealed_session_with_digest(
            request.scope.clone(),
            saved.folder.ok_or(UploadError::CheckpointInvalid)?,
            saved.original.ok_or(UploadError::CheckpointInvalid)?,
            Some(saved.original_sha256),
            operation,
            sign_in,
            state,
        )?;
        provider.check_checkpoint(request, checkpoint)?;
        Ok(Some(provider))
    }

    fn from_sealed_session_with_digest(
        scope: Scope,
        folder: Node,
        original: Node,
        original_sha256: Option<String>,
        operation: Uuid,
        sign_in: ICloudSealedSignIn,
        state: &Path,
    ) -> UploadResult<Self> {
        Self::check_identity(&scope, &folder, &original, original_sha256.as_deref())?;
        if sign_in.apple_id.trim().is_empty() || Uuid::parse_str(&sign_in.credential_id).is_err() {
            return Err(UploadError::Invalid);
        }
        let stage = ICloudFileCreate::from_sealed_session(
            scope.clone(),
            sign_in.apple_id.clone(),
            sign_in.credential_id.clone(),
            state,
            folder.clone(),
        )?;
        let vault =
            SealedSessionVault::new(state, &scope.account).map_err(|_| UploadError::Invalid)?;
        Ok(Self::with_stage(
            scope,
            folder,
            original,
            original_sha256,
            operation,
            SessionSource::Vault {
                apple_id: sign_in.apple_id,
                credential_id: sign_in.credential_id,
                vault: Arc::new(vault),
            },
            stage,
        ))
    }

    fn with_stage(
        scope: Scope,
        folder: Node,
        original: Node,
        original_sha256: Option<String>,
        operation: Uuid,
        session: SessionSource,
        stage: ICloudFileCreate,
    ) -> Self {
        Self {
            scope,
            folder,
            original,
            original_sha256,
            operation,
            stage_name: format!("staged-by-cirrove-{operation}.txt"),
            recovery_name: format!("recovery-by-cirrove-{operation}.txt"),
            session,
            stage,
        }
    }

    fn check_identity(
        scope: &Scope,
        folder: &Node,
        original: &Node,
        original_sha256: Option<&str>,
    ) -> UploadResult<()> {
        if scope.account.is_empty()
            || folder.kind != NodeKind::Folder
            || !folder.id.starts_with("FOLDER::com.apple.CloudDocs::")
            || folder.id.rsplit("::").next().is_none_or(str::is_empty)
            || if folder.id == ROOT_ID {
                folder.parent_id.is_some()
            } else {
                !folder.parent_id.as_deref().is_some_and(|parent| {
                    parent.starts_with("FOLDER::com.apple.CloudDocs::") && parent != folder.id
                })
            }
            || folder.name.is_empty()
            || folder.name.len() > 255
            || matches!(folder.name.as_str(), "." | "..")
            || folder.name.contains(['/', '\0', '\r', '\n'])
            || folder.target.is_some()
            || folder.package
            || original.kind != NodeKind::File
            || original.parent_id.as_deref() != Some(folder.id.as_str())
            || !original.id.starts_with("FILE::com.apple.CloudDocs::")
            || original.id.rsplit("::").next().is_none_or(str::is_empty)
            || original.etag.as_deref().is_none_or(str::is_empty)
            || original.size > crate::MAX_WRITE_FILE_SIZE
            || original.name.is_empty()
            || original.name.len() > 255
            || matches!(original.name.as_str(), "." | "..")
            || original.name.contains(['/', '\0', '\r', '\n'])
            || original.target.is_some()
            || original.package
            || original_sha256.is_some_and(|digest| !Self::valid_digest(digest))
        {
            return Err(UploadError::Invalid);
        }
        Ok(())
    }

    fn check_request(&self, request: &UploadRequest) -> UploadResult<()> {
        request.validate()?;
        if request.scope != self.scope
            || !matches!(&request.intent, UploadIntent::Replace { item, expected_etag }
                if item == &self.original.id && self.original.etag.as_deref() == Some(expected_etag))
            || request.size > crate::MAX_WRITE_FILE_SIZE
        {
            return Err(UploadError::Invalid);
        }
        Ok(())
    }

    fn stage_request(&self, request: &UploadRequest) -> UploadRequest {
        UploadRequest {
            scope: request.scope.clone(),
            intent: UploadIntent::Create {
                parent: self.folder.id.clone(),
                name: self.stage_name.clone(),
            },
            size: request.size,
            sha256: request.sha256.clone(),
        }
    }

    fn checkpoint(
        &self,
        request: &UploadRequest,
        original_sha256: &str,
        phase: Phase,
    ) -> UploadResult<SecretString> {
        self.check_request(request)?;
        if !Self::valid_digest(original_sha256)
            || self
                .original_sha256
                .as_deref()
                .is_some_and(|known| known != original_sha256)
        {
            return Err(UploadError::Invalid);
        }
        let value = Checkpoint {
            version: 2,
            scope: self.scope.clone(),
            operation: self.operation,
            original_id: self.original.id.clone(),
            original_etag: self.original.etag.clone().ok_or(UploadError::Invalid)?,
            folder: Some(self.folder.clone()),
            original: Some(self.original.clone()),
            original_sha256: original_sha256.into(),
            size: request.size,
            sha256: request.sha256.clone(),
            phase,
        };
        let encoded = serde_json::to_string(&value).map_err(|_| UploadError::CheckpointInvalid)?;
        if encoded.len() > MAX_CHECKPOINT {
            return Err(UploadError::CheckpointInvalid);
        }
        Ok(SecretString::from(encoded))
    }

    fn check_checkpoint(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
    ) -> UploadResult<(String, Phase)> {
        self.check_request(request)?;
        if checkpoint.expose_secret().len() > MAX_CHECKPOINT {
            return Err(UploadError::CheckpointInvalid);
        }
        let saved: Checkpoint = serde_json::from_str(checkpoint.expose_secret())
            .map_err(|_| UploadError::CheckpointInvalid)?;
        let original_sha256 = if saved.original_sha256.is_empty() {
            self.original_sha256
                .clone()
                .ok_or(UploadError::CheckpointInvalid)?
        } else {
            saved.original_sha256.clone()
        };
        if !Self::valid_digest(&original_sha256)
            || self
                .original_sha256
                .as_deref()
                .is_some_and(|known| known != original_sha256)
            || !matches!(saved.version, 1 | 2)
            || (saved.version == 2
                && (saved.folder.as_ref() != Some(&self.folder)
                    || saved.original.as_ref() != Some(&self.original)))
            || saved.scope != self.scope
            || saved.operation != self.operation
            || saved.original_id != self.original.id
            || Some(saved.original_etag.as_str()) != self.original.etag.as_deref()
            || saved.size != request.size
            || saved.sha256 != request.sha256
        {
            return Err(UploadError::CheckpointInvalid);
        }
        if let Phase::Handoff { plan, .. } = &saved.phase {
            plan.validate()
                .map_err(|_| UploadError::CheckpointInvalid)?;
            if plan.folder_parent() != self.folder.parent_id.as_deref().unwrap_or_default()
                || plan.folder_id != self.folder.id
                || plan.folder_name != self.folder.name
                || plan.original_id != self.original.id
                || plan.original_etag != saved.original_etag
                || plan.original_sha256 != original_sha256
                || plan.target_name != self.original.name
                || plan.staged_name != self.stage_name
                || plan.recovery_name != self.recovery_name
                || plan.staged_sha256 != request.sha256
            {
                return Err(UploadError::CheckpointInvalid);
            }
        }
        Ok((original_sha256, saved.phase))
    }

    fn valid_digest(hash: &str) -> bool {
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    }

    async fn load_session(&self) -> UploadResult<ICloudReadSession> {
        let (apple_id, saved) = match &self.session {
            SessionSource::Snapshot { apple_id, snapshot } => (apple_id, snapshot.clone()),
            SessionSource::Vault {
                apple_id,
                credential_id,
                vault,
            } => (
                apple_id,
                vault
                    .load(credential_id)
                    .await
                    .map_err(crate::file_create::map_session_error)?
                    .ok_or(UploadError::Uncertain)?,
            ),
        };
        let session = ICloudReadSession::from_session_snapshot(&saved, apple_id)
            .map_err(crate::file_create::map_session_error)?;
        if session.account_hash.is_none() {
            return Err(UploadError::Invalid);
        }
        Ok(session)
    }

    async fn handoff(&self, plan: HandoffPlan, size: u64) -> UploadResult<ICloudHandoff> {
        let session = self.load_session().await?;
        ICloudHandoff::new_conditional_trash(
            self.scope.clone(),
            self.operation,
            plan,
            size,
            session,
        )
    }

    /// A crashed or refused attempt without a durable upload checkpoint has
    /// no allocated staged identity. Before permitting a new attempt, prove
    /// that the exact old revision still occupies its confirmed folder and
    /// neither reserved name was created. A name alone is never a receipt.
    async fn observe_original_before_staging(&self) -> UploadResult<Option<String>> {
        let mut session = self.load_session().await?;
        if self.folder.id != ROOT_ID {
            let parent = self
                .folder
                .parent_id
                .as_deref()
                .ok_or(UploadError::Invalid)?;
            let parent_items = session
                .list_folder(parent)
                .await
                .map_err(crate::file_create::map_session_error)?;
            if parent_items
                .iter()
                .filter(|entry| entry.drivewsid == self.folder.id)
                .count()
                != 1
                || parent_items
                    .iter()
                    .filter(|entry| entry.display_name() == self.folder.name)
                    .count()
                    != 1
                || !parent_items.iter().any(|entry| {
                    entry.drivewsid == self.folder.id
                        && entry.parent_id == parent
                        && entry.display_name() == self.folder.name
                        && entry.is_folder()
                })
            {
                return Ok(None);
            }
        }
        let observe = |items: &[crate::DriveEntry]| -> Option<String> {
            let matching: Vec<_> = items
                .iter()
                .filter(|entry| entry.drivewsid == self.original.id)
                .collect();
            let original = matching.first()?;
            if matching.len() != 1
                || items.iter().any(|entry| {
                    entry.display_name() == self.stage_name
                        || entry.display_name() == self.recovery_name
                        || (entry.drivewsid != self.original.id
                            && entry.display_name() == self.original.name)
                })
                || original.is_folder()
                || original.parent_id != self.folder.id
                || original.docwsid != self.original.id.rsplit("::").next().unwrap_or_default()
                || original.display_name() != self.original.name
                || Some(original.etag.as_str()) != self.original.etag.as_deref()
                || original.size != self.original.size
            {
                return None;
            }
            Some(original.etag.clone())
        };
        let first = session
            .list_folder(&self.folder.id)
            .await
            .map_err(crate::file_create::map_session_error)?;
        let Some(etag) = observe(&first) else {
            return Ok(None);
        };
        let digest = session
            .hash_file_in_folder_for_revision(
                &self.folder.id,
                &self.original.id,
                &etag,
                self.original.size,
            )
            .await
            .map_err(crate::file_create::map_session_error)?;
        if self
            .original_sha256
            .as_deref()
            .is_some_and(|known| known != digest)
        {
            return Ok(None);
        }
        let second = session
            .list_folder(&self.folder.id)
            .await
            .map_err(crate::file_create::map_session_error)?;
        if observe(&second).as_deref() != Some(etag.as_str()) {
            return Ok(None);
        }
        Ok(Some(digest))
    }

    fn plan(
        &self,
        request: &UploadRequest,
        staged: &Node,
        original_sha256: &str,
    ) -> UploadResult<HandoffPlan> {
        if staged.kind != NodeKind::File
            || staged.parent_id.as_deref() != Some(self.folder.id.as_str())
            || staged.name != self.stage_name
            || staged.id == self.original.id
            || staged.size != request.size
            || staged.etag.as_deref().is_none_or(str::is_empty)
            || staged.target.is_some()
            || staged.package
        {
            return Err(UploadError::Conflict);
        }
        let plan = HandoffPlan {
            version: if self.folder.id == ROOT_ID { 4 } else { 5 },
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
            staged_id: staged.id.clone(),
            staged_doc_id: staged.id.rsplit("::").next().unwrap_or_default().into(),
            staged_etag: staged.etag.clone().ok_or(UploadError::Conflict)?,
            staged_name: self.stage_name.clone(),
            recovery_name: self.recovery_name.clone(),
            target_name: self.original.name.clone(),
            original_sha256: original_sha256.into(),
            staged_sha256: request.sha256.clone(),
        };
        plan.validate().map_err(|_| UploadError::Invalid)?;
        Ok(plan)
    }

    async fn wrap_stage(
        &self,
        request: &UploadRequest,
        original_sha256: &str,
        step: UploadStep,
        cancel: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        Ok(match step {
            UploadStep::Prepared(inner) => UploadStep::Prepared(self.checkpoint(
                request,
                original_sha256,
                Phase::Stage {
                    inner: inner.expose_secret().into(),
                },
            )?),
            UploadStep::Stream(inner) => UploadStep::Stream(self.checkpoint(
                request,
                original_sha256,
                Phase::Stage {
                    inner: inner.expose_secret().into(),
                },
            )?),
            UploadStep::Commit(inner) => UploadStep::Commit(self.checkpoint(
                request,
                original_sha256,
                Phase::Stage {
                    inner: inner.expose_secret().into(),
                },
            )?),
            UploadStep::Continue(progress) => UploadStep::Continue(UploadProgress {
                checkpoint: self.checkpoint(
                    request,
                    original_sha256,
                    Phase::Stage {
                        inner: progress.checkpoint.expose_secret().into(),
                    },
                )?,
                offset: progress.offset,
                length: progress.length,
            }),
            UploadStep::Complete(staged) => {
                let plan = self.plan(request, &staged, original_sha256)?;
                let handoff = self.handoff(plan.clone(), request.size).await?;
                let UploadStep::Commit(inner) = handoff.begin_upload(request, cancel).await? else {
                    return Err(UploadError::Uncertain);
                };
                UploadStep::Commit(self.checkpoint(
                    request,
                    original_sha256,
                    Phase::Handoff {
                        inner: inner.expose_secret().into(),
                        plan: Box::new(plan),
                    },
                )?)
            }
            UploadStep::HandoffComplete { .. } => return Err(UploadError::Invalid),
        })
    }

    fn wrap_handoff(
        &self,
        request: &UploadRequest,
        plan: HandoffPlan,
        step: UploadStep,
    ) -> UploadResult<UploadStep> {
        let original_sha256 = plan.original_sha256.clone();
        Ok(match step {
            UploadStep::Commit(inner) => UploadStep::Commit(self.checkpoint(
                request,
                &original_sha256,
                Phase::Handoff {
                    inner: inner.expose_secret().into(),
                    plan: Box::new(plan),
                },
            )?),
            UploadStep::HandoffComplete { current, backup } => {
                UploadStep::HandoffComplete { current, backup }
            }
            _ => return Err(UploadError::Invalid),
        })
    }
}

#[async_trait]
impl UploadProvider for ICloudFileReplace {
    fn staged_recovery_location(
        &self,
        operation: &str,
        request: &UploadRequest,
    ) -> Option<RecoveryLocation> {
        if operation != self.operation.to_string() || self.check_request(request).is_err() {
            return None;
        }
        Some(Self::recovery_location(self.operation))
    }

    async fn begin_upload(
        &self,
        r: &UploadRequest,
        c: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        self.check_request(r)?;
        let original_sha256 = match &self.original_sha256 {
            Some(digest) => digest.clone(),
            None => self
                .observe_original_before_staging()
                .await?
                .ok_or(UploadError::Conflict)?,
        };
        self.wrap_stage(
            r,
            &original_sha256,
            self.stage.begin_upload(&self.stage_request(r), c).await?,
            c,
        )
        .await
    }

    async fn inspect_upload(
        &self,
        r: &UploadRequest,
        checkpoint: &SecretString,
        c: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        match self.check_checkpoint(r, checkpoint)? {
            (original_sha256, Phase::Stage { inner }) => {
                self.wrap_stage(
                    r,
                    &original_sha256,
                    self.stage
                        .inspect_upload(&self.stage_request(r), &SecretString::from(inner), c)
                        .await?,
                    c,
                )
                .await
            }
            (_, Phase::Handoff { inner, plan }) => {
                let handoff = self.handoff(*plan.clone(), r.size).await?;
                self.wrap_handoff(
                    r,
                    *plan,
                    handoff
                        .inspect_upload(r, &SecretString::from(inner), c)
                        .await?,
                )
            }
        }
    }

    async fn upload_part(
        &self,
        r: &UploadRequest,
        checkpoint: &SecretString,
        offset: u64,
        bytes: Vec<u8>,
        c: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        let (original_sha256, Phase::Stage { inner }) = self.check_checkpoint(r, checkpoint)?
        else {
            return Err(UploadError::CheckpointInvalid);
        };
        self.wrap_stage(
            r,
            &original_sha256,
            self.stage
                .upload_part(
                    &self.stage_request(r),
                    &SecretString::from(inner),
                    offset,
                    bytes,
                    c,
                )
                .await?,
            c,
        )
        .await
    }

    async fn upload_stream(
        &self,
        r: &UploadRequest,
        checkpoint: &SecretString,
        file: File,
        c: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        let (original_sha256, Phase::Stage { inner }) = self.check_checkpoint(r, checkpoint)?
        else {
            return Err(UploadError::CheckpointInvalid);
        };
        self.wrap_stage(
            r,
            &original_sha256,
            self.stage
                .upload_stream(&self.stage_request(r), &SecretString::from(inner), file, c)
                .await?,
            c,
        )
        .await
    }

    async fn commit_upload(
        &self,
        r: &UploadRequest,
        checkpoint: &SecretString,
        c: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        match self.check_checkpoint(r, checkpoint)? {
            (original_sha256, Phase::Stage { inner }) => {
                self.wrap_stage(
                    r,
                    &original_sha256,
                    self.stage
                        .commit_upload(&self.stage_request(r), &SecretString::from(inner), c)
                        .await?,
                    c,
                )
                .await
            }
            (_, Phase::Handoff { inner, plan }) => {
                let handoff = self.handoff(*plan.clone(), r.size).await?;
                self.wrap_handoff(
                    r,
                    *plan,
                    handoff
                        .commit_upload(r, &SecretString::from(inner), c)
                        .await?,
                )
            }
        }
    }

    async fn reconcile_upload(
        &self,
        r: &UploadRequest,
        checkpoint: Option<&SecretString>,
        c: &CancellationToken,
    ) -> UploadResult<Reconciliation> {
        if checkpoint.is_none() {
            self.check_request(r)?;
            if c.is_cancelled() {
                return Err(UploadError::Uncertain);
            }
            return Ok(if self.observe_original_before_staging().await?.is_some() {
                Reconciliation::Uncommitted
            } else {
                Reconciliation::Conflict
            });
        }
        match self.check_checkpoint(r, checkpoint.ok_or(UploadError::CheckpointInvalid)?)? {
            (_, Phase::Stage { inner }) => match self
                .stage
                .reconcile_upload(&self.stage_request(r), Some(&SecretString::from(inner)), c)
                .await?
            {
                Reconciliation::Uncommitted => Ok(Reconciliation::Uncommitted),
                Reconciliation::Conflict => Ok(Reconciliation::Conflict),
                // A staged Create is not completion of the Replace. The next
                // verification must observe its exact ID and enter handoff.
                Reconciliation::Committed(_) => Err(UploadError::Uncertain),
                Reconciliation::HandoffCommitted { .. } => Err(UploadError::Invalid),
            },
            (_, Phase::Handoff { inner, plan }) => {
                self.handoff(*plan, r.size)
                    .await?
                    .reconcile_upload(r, Some(&SecretString::from(inner)), c)
                    .await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: String, parent: Option<String>, name: &str, kind: NodeKind) -> Node {
        Node {
            id,
            parent_id: parent,
            name: name.into(),
            kind,
            size: 1,
            modified_unix: 0,
            etag: Some("original-etag".into()),
            content_version: None,
            target: None,
            package: false,
        }
    }

    #[test]
    fn empty_replacements_restore_both_zero_sized_originals_and_targets() {
        use sha2::{Digest, Sha256};
        for (old_bytes, new_bytes) in [(b"a".as_slice(), b"".as_slice()), (b"", b"b"), (b"", b"")] {
            let state = tempfile::tempdir().expect("fixture");
            let scope = Scope {
                account: Uuid::new_v4().to_string(),
                provider: "icloud".into(),
                collection: "drive".into(),
            };
            let folder = node(ROOT_ID.into(), None, "iCloud Drive", NodeKind::Folder);
            let mut original = node(
                "FILE::com.apple.CloudDocs::original".into(),
                Some(ROOT_ID.into()),
                "Original.txt",
                NodeKind::File,
            );
            original.size = old_bytes.len() as u64;
            let digest = hex::encode(Sha256::digest(old_bytes));
            let operation = Uuid::new_v4();
            let sign_in = || ICloudSealedSignIn {
                apple_id: "fixture@example.invalid".into(),
                credential_id: Uuid::new_v4().to_string(),
            };
            let provider = ICloudFileReplace::from_sealed_session_in_folder(
                scope.clone(),
                folder,
                original.clone(),
                digest.clone(),
                operation,
                sign_in(),
                state.path(),
            )
            .expect("zero-sized source");
            let request = UploadRequest {
                scope,
                intent: UploadIntent::Replace {
                    item: original.id.clone(),
                    expected_etag: original.etag.clone().expect("revision"),
                },
                size: new_bytes.len() as u64,
                sha256: hex::encode(Sha256::digest(new_bytes)),
            };
            provider.check_request(&request).expect("zero-sized target");
            let mut staged = node(
                "FILE::com.apple.CloudDocs::staged".into(),
                Some(ROOT_ID.into()),
                &provider.stage_name,
                NodeKind::File,
            );
            staged.size = request.size;
            let plan = provider.plan(&request, &staged, &digest).expect("plan");
            let checkpoint = provider
                .checkpoint(
                    &request,
                    &digest,
                    Phase::Handoff {
                        inner: "fixture-inner".into(),
                        plan: Box::new(plan),
                    },
                )
                .expect("checkpoint");
            let restored = ICloudFileReplace::from_sealed_checkpoint(
                &request,
                operation,
                &checkpoint,
                sign_in(),
                state.path(),
            )
            .expect("restore")
            .expect("captured source");
            assert_eq!(restored.original, original);
            assert_eq!(restored.stage_request(&request).size, request.size);
            let mut changed = request.clone();
            changed.size += 1;
            assert!(restored.check_checkpoint(&changed, &checkpoint).is_err());
        }
    }

    #[test]
    fn root_replacement_plan_and_checkpoint_restore_without_an_invented_parent() {
        let state = tempfile::tempdir().expect("synthetic fixture");
        let scope = Scope {
            account: Uuid::new_v4().to_string(),
            provider: "icloud".into(),
            collection: "drive".into(),
        };
        let folder = node(ROOT_ID.into(), None, "iCloud Drive", NodeKind::Folder);
        let mut original = node(
            "FILE::com.apple.CloudDocs::original".into(),
            Some(ROOT_ID.into()),
            "Original.txt",
            NodeKind::File,
        );
        original.size = 65 * 1024 * 1024;
        let operation = Uuid::new_v4();
        let sign_in = || ICloudSealedSignIn {
            apple_id: "fixture@example.invalid".into(),
            credential_id: Uuid::new_v4().to_string(),
        };
        let provider = ICloudFileReplace::from_sealed_session_in_folder(
            scope.clone(),
            folder.clone(),
            original.clone(),
            "a".repeat(64),
            operation,
            sign_in(),
            state.path(),
        )
        .expect("root fixture");
        let request = UploadRequest {
            scope: scope.clone(),
            intent: UploadIntent::Replace {
                item: original.id.clone(),
                expected_etag: original.etag.clone().expect("fixture"),
            },
            size: 66 * 1024 * 1024,
            sha256: "b".repeat(64),
        };
        let mut staged = node(
            "FILE::com.apple.CloudDocs::staged".into(),
            Some(ROOT_ID.into()),
            &provider.stage_name,
            NodeKind::File,
        );
        staged.size = request.size;
        provider.check_request(&request).expect("large request");
        let mut unrepresentable = request.clone();
        unrepresentable.size = i64::MAX as u64 + 1;
        assert!(provider.check_request(&unrepresentable).is_err());
        let plan = provider
            .plan(&request, &staged, &"a".repeat(64))
            .expect("root plan");
        assert_eq!(plan.version, 4);
        assert!(plan.folder_parent_id.is_empty());
        let checkpoint = provider
            .checkpoint(
                &request,
                &"a".repeat(64),
                Phase::Handoff {
                    inner: "synthetic-inner".into(),
                    plan: Box::new(plan.clone()),
                },
            )
            .expect("fixture");
        let restored = ICloudFileReplace::from_sealed_checkpoint(
            &request,
            operation,
            &checkpoint,
            sign_in(),
            state.path(),
        )
        .expect("fixture")
        .expect("captured source");
        assert_eq!(restored.parent_id(), ROOT_ID);
        let mut wrong = plan.clone();
        wrong.version = 3;
        assert!(wrong.validate().is_err());
        wrong.folder_parent_id = "FOLDER::com.apple.CloudDocs::invented-parent".into();
        assert!(wrong.validate().is_err());
        wrong = plan;
        wrong.folder_parent_id = ROOT_ID.into();
        assert!(wrong.validate().is_err());
        let invalid_root = Node {
            parent_id: Some(ROOT_ID.into()),
            ..folder
        };
        assert!(
            ICloudFileReplace::from_sealed_session_for_existing(
                scope,
                invalid_root,
                original,
                operation,
                sign_in(),
                state.path()
            )
            .is_err()
        );
    }

    #[test]
    fn existing_file_checkpoint_keeps_original_digest_across_reconstruction() {
        let state = tempfile::tempdir().expect("synthetic fixture");
        let scope = Scope {
            account: Uuid::new_v4().to_string(),
            provider: "icloud".into(),
            collection: "drive".into(),
        };
        let folder = node(
            "FOLDER::com.apple.CloudDocs::parent".into(),
            Some(ROOT_ID.into()),
            "Owned",
            NodeKind::Folder,
        );
        let original = node(
            "FILE::com.apple.CloudDocs::original".into(),
            Some(folder.id.clone()),
            "Original.txt",
            NodeKind::File,
        );
        let operation = Uuid::new_v4();
        let sign_in = || ICloudSealedSignIn {
            apple_id: "fixture@example.invalid".into(),
            credential_id: Uuid::new_v4().to_string(),
        };
        let make = || {
            ICloudFileReplace::from_sealed_session_for_existing(
                scope.clone(),
                folder.clone(),
                original.clone(),
                operation,
                sign_in(),
                state.path(),
            )
            .expect("synthetic fixture")
        };
        let request = UploadRequest {
            scope: scope.clone(),
            intent: UploadIntent::Replace {
                item: original.id.clone(),
                expected_etag: original.etag.clone().expect("synthetic fixture"),
            },
            size: 2,
            sha256: "b".repeat(64),
        };
        let digest = "a".repeat(64);
        let checkpoint = make()
            .checkpoint(
                &request,
                &digest,
                Phase::Stage {
                    inner: "prepared".into(),
                },
            )
            .expect("synthetic fixture");
        let (saved_digest, Phase::Stage { inner }) = make()
            .check_checkpoint(&request, &checkpoint)
            .expect("synthetic fixture")
        else {
            panic!("synthetic checkpoint phase");
        };
        assert_eq!(saved_digest, digest);
        assert_eq!(inner, "prepared");

        let restored = ICloudFileReplace::from_sealed_checkpoint(
            &request,
            operation,
            &checkpoint,
            sign_in(),
            state.path(),
        )
        .expect("checkpoint-only reconstruction")
        .expect("captured source");
        assert_eq!(restored.folder, folder);
        assert_eq!(restored.original, original);
        assert_eq!(restored.original_sha256.as_deref(), Some(digest.as_str()));
        let mut staged = node(
            "FILE::com.apple.CloudDocs::staged".into(),
            Some(folder.id.clone()),
            &restored.stage_name,
            NodeKind::File,
        );
        staged.size = request.size;
        let current_plan = restored.plan(&request, &staged, &digest).expect("plan");
        assert_eq!(current_plan.version, 5);
        for version in [3, 5] {
            let mut plan = current_plan.clone();
            plan.version = version;
            let saved = restored
                .checkpoint(
                    &request,
                    &digest,
                    Phase::Handoff {
                        inner: "synthetic-handoff".into(),
                        plan: Box::new(plan),
                    },
                )
                .expect("handoff checkpoint");
            let resumed = ICloudFileReplace::from_sealed_checkpoint(
                &request,
                operation,
                &saved,
                sign_in(),
                state.path(),
            )
            .expect("restore handoff")
            .expect("captured source");
            let (_, Phase::Handoff { plan, .. }) = resumed
                .check_checkpoint(&request, &saved)
                .expect("retain saved contract")
            else {
                panic!("expected handoff");
            };
            assert_eq!(plan.version, version);
        }
        assert!(
            ICloudFileReplace::from_sealed_checkpoint(
                &request,
                Uuid::new_v4(),
                &checkpoint,
                sign_in(),
                state.path(),
            )
            .is_err()
        );
        let mut foreign = request.clone();
        foreign.scope.account = Uuid::new_v4().to_string();
        assert!(
            ICloudFileReplace::from_sealed_checkpoint(
                &foreign,
                operation,
                &checkpoint,
                sign_in(),
                state.path(),
            )
            .is_err()
        );
        let mut incomplete: serde_json::Value =
            serde_json::from_str(checkpoint.expose_secret()).expect("synthetic fixture");
        incomplete
            .as_object_mut()
            .expect("synthetic fixture")
            .remove("original");
        assert!(
            ICloudFileReplace::from_sealed_checkpoint(
                &request,
                operation,
                &SecretString::from(incomplete.to_string()),
                sign_in(),
                state.path(),
            )
            .is_err()
        );

        let mut value: serde_json::Value =
            serde_json::from_str(checkpoint.expose_secret()).expect("synthetic fixture");
        value["original_sha256"] = serde_json::Value::String("c".repeat(64));
        let altered = SecretString::from(value.to_string());
        let known = ICloudFileReplace::from_sealed_session_in_folder(
            request.scope.clone(),
            folder,
            original,
            digest,
            operation,
            sign_in(),
            state.path(),
        )
        .expect("synthetic fixture");
        assert!(known.check_checkpoint(&request, &altered).is_err());
        value["version"] = 1.into();
        value
            .as_object_mut()
            .expect("synthetic fixture")
            .remove("original_sha256");
        let legacy = SecretString::from(value.to_string());
        let (restored, _) = known
            .check_checkpoint(&request, &legacy)
            .expect("existing fixture checkpoint remains readable");
        assert_eq!(restored, "a".repeat(64));
    }
}
