//! Journal-checkpointed staging and two-ID replacement for one owned mount fixture.
//! Ordinary iCloud accounts never construct this feature-gated provider.
use super::{
    HandoffPlan, ICloudFileCreate, ICloudOwnedFixtureHandoff, ICloudReadSession, ROOT_ID,
    SealedSessionVault, ValidationFolder, write_probe::TRASH_ROOT,
};
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
const MAX_FIXTURE_FILE: u64 = 32 * 1024 * 1024;

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
    size: u64,
    sha256: String,
    phase: Phase,
}

pub struct ICloudOwnedMountedReplace {
    scope: Scope,
    folder: Node,
    original: Node,
    original_sha256: String,
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

impl ICloudOwnedMountedReplace {
    pub fn parent_id(&self) -> &str {
        &self.folder.id
    }

    pub fn recovery_location(operation: Uuid) -> RecoveryLocation {
        RecoveryLocation::Trash {
            local_name: format!("recovery-by-cirrove-{operation}.txt"),
            parent: TRASH_ROOT.into(),
        }
    }

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
        Self::check_identity(&scope, &folder, &original, &original_sha256)?;
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
            original_sha256,
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
        Self::check_identity(&scope, &folder, &original, &original_sha256)?;
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
        original_sha256: String,
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
        original_sha256: &str,
    ) -> UploadResult<()> {
        let valid_digest = |hash: &str| {
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        };
        if scope.account.is_empty()
            || folder.kind != NodeKind::Folder
            || !folder.id.starts_with("FOLDER::com.apple.CloudDocs::")
            || folder.id.rsplit("::").next().is_none_or(str::is_empty)
            || !folder.parent_id.as_deref().is_some_and(|parent| {
                parent.starts_with("FOLDER::com.apple.CloudDocs::") && parent != folder.id
            })
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
            || original.size == 0
            || original.size > MAX_FIXTURE_FILE
            || original.name.is_empty()
            || original.name.len() > 255
            || matches!(original.name.as_str(), "." | "..")
            || original.name.contains(['/', '\0', '\r', '\n'])
            || original.target.is_some()
            || original.package
            || !valid_digest(original_sha256)
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
            || request.size == 0
            || request.size > MAX_FIXTURE_FILE
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

    fn checkpoint(&self, request: &UploadRequest, phase: Phase) -> UploadResult<SecretString> {
        self.check_request(request)?;
        let value = Checkpoint {
            version: 1,
            scope: self.scope.clone(),
            operation: self.operation,
            original_id: self.original.id.clone(),
            original_etag: self.original.etag.clone().ok_or(UploadError::Invalid)?,
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
            if Some(plan.folder_parent()) != self.folder.parent_id.as_deref()
                || plan.folder_id != self.folder.id
                || plan.folder_name != self.folder.name
                || plan.original_id != self.original.id
                || plan.original_etag != saved.original_etag
                || plan.original_sha256 != self.original_sha256
                || plan.target_name != self.original.name
                || plan.staged_name != self.stage_name
                || plan.recovery_name != self.recovery_name
                || plan.staged_sha256 != request.sha256
            {
                return Err(UploadError::CheckpointInvalid);
            }
        }
        Ok(saved.phase)
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
                    .map_err(|_| UploadError::Uncertain)?
                    .ok_or(UploadError::Uncertain)?,
            ),
        };
        let session = ICloudReadSession::from_session_snapshot(&saved, apple_id)
            .map_err(|_| UploadError::Uncertain)?;
        if session.account_hash.is_none() {
            return Err(UploadError::Invalid);
        }
        Ok(session)
    }

    async fn handoff(
        &self,
        plan: HandoffPlan,
        size: u64,
    ) -> UploadResult<ICloudOwnedFixtureHandoff> {
        let session = self.load_session().await?;
        ICloudOwnedFixtureHandoff::new_conditional_trash(
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
    async fn inspect_without_checkpoint(&self) -> UploadResult<Reconciliation> {
        let mut session = self.load_session().await?;
        let parent = self
            .folder
            .parent_id
            .as_deref()
            .ok_or(UploadError::Invalid)?;
        let parent_items = session
            .list_folder(parent)
            .await
            .map_err(|_| UploadError::Uncertain)?;
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
            return Ok(Reconciliation::Conflict);
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
            .map_err(|_| UploadError::Uncertain)?;
        let Some(etag) = observe(&first) else {
            return Ok(Reconciliation::Conflict);
        };
        if session
            .hash_file_in_folder_for_revision(
                &self.folder.id,
                &self.original.id,
                &etag,
                self.original.size,
            )
            .await
            .map_err(|_| UploadError::Uncertain)?
            != self.original_sha256
        {
            return Ok(Reconciliation::Conflict);
        }
        let second = session
            .list_folder(&self.folder.id)
            .await
            .map_err(|_| UploadError::Uncertain)?;
        if observe(&second).as_deref() != Some(etag.as_str()) {
            return Ok(Reconciliation::Conflict);
        }
        Ok(Reconciliation::Uncommitted)
    }

    fn plan(&self, request: &UploadRequest, staged: &Node) -> UploadResult<HandoffPlan> {
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
            version: 3,
            folder_parent_id: self.folder.parent_id.clone().ok_or(UploadError::Invalid)?,
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
            original_sha256: self.original_sha256.clone(),
            staged_sha256: request.sha256.clone(),
        };
        plan.validate().map_err(|_| UploadError::Invalid)?;
        Ok(plan)
    }

    async fn wrap_stage(
        &self,
        request: &UploadRequest,
        step: UploadStep,
        cancel: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        Ok(match step {
            UploadStep::Prepared(inner) => UploadStep::Prepared(self.checkpoint(
                request,
                Phase::Stage {
                    inner: inner.expose_secret().into(),
                },
            )?),
            UploadStep::Stream(inner) => UploadStep::Stream(self.checkpoint(
                request,
                Phase::Stage {
                    inner: inner.expose_secret().into(),
                },
            )?),
            UploadStep::Commit(inner) => UploadStep::Commit(self.checkpoint(
                request,
                Phase::Stage {
                    inner: inner.expose_secret().into(),
                },
            )?),
            UploadStep::Continue(progress) => UploadStep::Continue(UploadProgress {
                checkpoint: self.checkpoint(
                    request,
                    Phase::Stage {
                        inner: progress.checkpoint.expose_secret().into(),
                    },
                )?,
                offset: progress.offset,
                length: progress.length,
            }),
            UploadStep::Complete(staged) => {
                let plan = self.plan(request, &staged)?;
                let handoff = self.handoff(plan.clone(), request.size).await?;
                let UploadStep::Commit(inner) = handoff.begin_upload(request, cancel).await? else {
                    return Err(UploadError::Uncertain);
                };
                UploadStep::Commit(self.checkpoint(
                    request,
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
        Ok(match step {
            UploadStep::Commit(inner) => UploadStep::Commit(self.checkpoint(
                request,
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
impl UploadProvider for ICloudOwnedMountedReplace {
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
        self.wrap_stage(
            r,
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
            Phase::Stage { inner } => {
                self.wrap_stage(
                    r,
                    self.stage
                        .inspect_upload(&self.stage_request(r), &SecretString::from(inner), c)
                        .await?,
                    c,
                )
                .await
            }
            Phase::Handoff { inner, plan } => {
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
        let Phase::Stage { inner } = self.check_checkpoint(r, checkpoint)? else {
            return Err(UploadError::CheckpointInvalid);
        };
        self.wrap_stage(
            r,
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
        let Phase::Stage { inner } = self.check_checkpoint(r, checkpoint)? else {
            return Err(UploadError::CheckpointInvalid);
        };
        self.wrap_stage(
            r,
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
            Phase::Stage { inner } => {
                self.wrap_stage(
                    r,
                    self.stage
                        .commit_upload(&self.stage_request(r), &SecretString::from(inner), c)
                        .await?,
                    c,
                )
                .await
            }
            Phase::Handoff { inner, plan } => {
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
            return self.inspect_without_checkpoint().await;
        }
        match self.check_checkpoint(r, checkpoint.ok_or(UploadError::CheckpointInvalid)?)? {
            Phase::Stage { inner } => match self
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
            Phase::Handoff { inner, plan } => {
                self.handoff(*plan, r.size)
                    .await?
                    .reconcile_upload(r, Some(&SecretString::from(inner)), c)
                    .await
            }
        }
    }
}
