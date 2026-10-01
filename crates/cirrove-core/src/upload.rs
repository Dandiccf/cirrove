//! Provider upload contract consumed by the durable local journal and its worker.
mod representation;
use crate::{CancellationToken, Node, ProviderError, Scope};
use async_trait::async_trait;
pub use representation::{
    PACKAGE_SEMANTIC_IDENTITY_VERSION, PackageHandoffReceipt, PackageSemanticIdentity,
    PackageUploadReceipt, UploadRepresentation,
};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::time::Duration;

#[derive(Clone, Debug, thiserror::Error)]
pub enum UploadError {
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error("remote content or destination conflicts with this edit")]
    Conflict,
    #[error("cloud storage quota exceeded")]
    Quota,
    /// The provider reported insufficient server storage, not necessarily a
    /// user quota. Retain the operation and wait for an explicit retry.
    #[error(
        "the cloud service reported insufficient storage; local changes are kept; retry explicitly when storage is available"
    )]
    InsufficientStorage,
    #[error("remote file is locked")]
    Locked,
    #[error("upload session is no longer available; verify remote content")]
    SessionGone,
    #[error("saved upload checkpoint is invalid; verify remote content")]
    CheckpointInvalid,
    #[error("upload result is uncertain; verify before retrying")]
    Uncertain,
    #[error("provider returned HTTP {0}; verify the upload result before retrying")]
    UnexpectedStatus(u16),
    #[error("invalid upload request")]
    Invalid,
    #[error("provider upload behavior is not supported: {0}")]
    Unsupported(&'static str),
}
pub type Result<T> = std::result::Result<T, UploadError>;

/// New names must fail on collision. Replacements carry the original metadata
/// ETag, which also covers concurrent metadata changes such as rename/move.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UploadIntent {
    Create { parent: String, name: String },
    Replace { item: String, expected_etag: String },
}
impl UploadIntent {
    pub fn validate(&self) -> Result<()> {
        let valid = |s: &str| !s.is_empty() && s.len() <= 4096 && !s.contains('\0');
        let okay = match self {
            Self::Create { parent, name } => {
                valid(parent)
                    && valid(name)
                    && !matches!(name.as_str(), "." | "..")
                    && !name.contains('/')
            }
            Self::Replace {
                item,
                expected_etag,
            } => valid(item) && valid(expected_etag) && !expected_etag.contains(['\r', '\n', '*']),
        };
        if okay {
            Ok(())
        } else {
            Err(UploadError::Invalid)
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadRequest {
    #[serde(default, skip_serializing_if = "UploadRepresentation::is_file_bytes")]
    pub representation: UploadRepresentation,
    pub scope: Scope,
    pub intent: UploadIntent,
    pub size: u64,
    pub sha256: String,
}
impl UploadRequest {
    /// Ordinary adapters must call this before every remote operation.
    pub fn require_file_bytes(&self) -> Result<()> {
        self.validate()?;
        if self.representation.is_file_bytes() {
            Ok(())
        } else {
            Err(UploadError::Unsupported("package archive upload"))
        }
    }
    pub fn validate(&self) -> Result<()> {
        self.intent.validate()?;
        self.representation.validate()?;
        if let UploadRepresentation::PackageReplacementArchive { original, .. } =
            &self.representation
            && !matches!(&self.intent, UploadIntent::Replace { item, expected_etag }
                if item == &original.id && original.etag.as_ref() == Some(expected_etag))
        {
            return Err(UploadError::Invalid);
        }
        if !self.representation.is_file_bytes() && self.size > 64 * 1024 * 1024 {
            return Err(UploadError::Invalid);
        }
        if self.scope.account.is_empty()
            || self.scope.provider.is_empty()
            || self.scope.collection.is_empty()
            || self.sha256.len() != 64
            || !self
                .sha256
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(UploadError::Invalid);
        }
        Ok(())
    }
}

/// The checkpoint may contain a preauthenticated URL. Keep it in credential
/// storage; never serialize it to status, the metadata database or diagnostics.
pub struct UploadProgress {
    pub checkpoint: SecretString,
    /// Next exact range requested by the provider, bounded by its part-size limit.
    pub offset: u64,
    pub length: u32,
}
pub enum UploadStep {
    /// Arm a non-idempotent remote allocation. Only a fresh `begin_upload` may
    /// return this step. The worker persists it before invoking the allocation
    /// callback once; after interruption it is inspected/reconciled, never
    /// automatically allocated again. The checkpoint must describe uncertainty
    /// even if the allocation succeeds but its returned identity is lost.
    Allocate(SecretString),
    /// Provider-specific preparation that must be persisted before the caller
    /// asks the provider to start or recover a mutating upload session.
    Prepared(SecretString),
    Continue(UploadProgress),
    /// Send the sealed journal payload through a bounded-memory provider stream.
    /// Persist this checkpoint before handing the file to the provider. A lost
    /// result is uncertain and must be reconciled before any further mutation.
    Stream(SecretString),
    /// Bytes are staged remotely; persist this checkpoint before the conditional
    /// final commit makes them visible as the replacement file.
    Commit(SecretString),
    Complete(Node),
    PackageComplete(PackageUploadReceipt),
    PackageHandoffComplete(Box<PackageHandoffReceipt>),
    /// A staged replacement has installed a new item ID and retained the old
    /// exact ID at its reserved recovery name. The provider must verify both
    /// identities and exact content before returning this receipt.
    HandoffComplete {
        current: Node,
        backup: Node,
    },
}
impl std::fmt::Debug for UploadStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Allocate(_) => "UploadStep::Allocate([redacted])",
            Self::Prepared(_) => "UploadStep::Prepared([redacted])",
            Self::Continue(_) => "UploadStep::Continue([redacted])",
            Self::Stream(_) => "UploadStep::Stream([redacted])",
            Self::Commit(_) => "UploadStep::Commit([redacted])",
            Self::PackageHandoffComplete(_) => "UploadStep::PackageHandoffComplete([redacted])",
            Self::PackageComplete(_) => "UploadStep::PackageComplete([redacted])",
            Self::Complete(_) => "UploadStep::Complete([redacted])",
            Self::HandoffComplete { .. } => "UploadStep::HandoffComplete([redacted])",
        })
    }
}
pub enum Reconciliation {
    Committed(Node),
    PackageCommitted(PackageUploadReceipt),
    PackageHandoffCommitted(Box<PackageHandoffReceipt>),
    HandoffCommitted { current: Node, backup: Node },
    Uncommitted,
    Conflict,
}

/// Recovery identity reserved before a two-ID replacement touches the provider.
/// A Trash parent is an opaque provider collection ID, never a local path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecoveryLocation {
    Sibling { name: String },
    Trash { local_name: String, parent: String },
}

/// A caller persists every returned checkpoint before advancing the upload.
/// `Prepared` may be returned by `begin_upload`, or once during recovery when a
/// session is gone but its durable provider identity can start a replacement.
/// After persisting it, the caller passes the checkpoint to `inspect_upload`.
/// This lets a provider bind and reuse an idempotent destination identity before
/// its next mutating request.
/// Each later checkpoint must retain any identity needed to reconcile an
/// uncertain result; `reconcile_upload` receives the last persisted checkpoint
/// when one is available.
/// After an uncertain result, inspect the same session; a missing session alone
/// does not prove failure. Reconcile the remote target before restarting it.
#[async_trait]
pub trait UploadProvider: Send + Sync {
    /// True only if `begin_upload` cannot contact or mutate the provider before
    /// returning a checkpoint. When the journal has never recorded a checkpoint
    /// and the vault has none, the worker can then retry that preflight safely.
    /// A lost vault entry after a recorded checkpoint never qualifies.
    fn begin_is_mutation_free_until_checkpoint(&self, _request: &UploadRequest) -> bool {
        false
    }

    fn staged_recovery_location(
        &self,
        operation: &str,
        request: &UploadRequest,
    ) -> Option<RecoveryLocation> {
        self.staged_recovery_name(operation, request)
            .map(|name| RecoveryLocation::Sibling { name })
    }
    /// Pure, stable choice of an old-version recovery name for this operation.
    /// The worker reserves it in the local journal before any provider call.
    /// A provider returning `Some` must never mutate a replacement before that
    /// reservation, and must return the same name after a process restart.
    fn staged_recovery_name(&self, _operation: &str, _request: &UploadRequest) -> Option<String> {
        None
    }

    async fn begin_upload(
        &self,
        request: &UploadRequest,
        cancel: &CancellationToken,
    ) -> Result<UploadStep>;
    /// The worker's durable operation ID, for providers whose remote
    /// reservation and recovery names must be bound to one journal row.
    /// Existing providers keep their request-only implementation.
    async fn begin_upload_for_operation(
        &self,
        _operation: &str,
        request: &UploadRequest,
        cancel: &CancellationToken,
    ) -> Result<UploadStep> {
        self.begin_upload(request, cancel).await
    }
    /// Execute a fresh, durably armed allocation exactly once in this attempt.
    /// This is distinct from `inspect_upload`: inspecting a saved allocation
    /// checkpoint must be read-only. Lost responses remain uncertain unless
    /// provider-specific readback establishes the exact allocated identity or
    /// proves no allocation occurred. Never infer absence from a missing receipt.
    /// Return a content/commit continuation or an independently verified receipt,
    /// not another Allocate or Prepared step. Existing providers need no change.
    async fn allocate_upload_for_operation(
        &self,
        _operation: &str,
        _request: &UploadRequest,
        _checkpoint: &SecretString,
        _cancel: &CancellationToken,
    ) -> Result<UploadStep> {
        Err(UploadError::Unsupported("remote upload allocation"))
    }
    async fn inspect_upload(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
        cancel: &CancellationToken,
    ) -> Result<UploadStep>;
    /// Deadline for inspecting a persisted checkpoint, including any required
    /// integrity readback. Expiry never authorizes replaying a remote mutation.
    fn inspection_timeout(&self, _request: &UploadRequest) -> Duration {
        Duration::from_secs(125)
    }
    async fn inspect_upload_for_operation(
        &self,
        _operation: &str,
        request: &UploadRequest,
        checkpoint: &SecretString,
        cancel: &CancellationToken,
    ) -> Result<UploadStep> {
        self.inspect_upload(request, checkpoint, cancel).await
    }
    async fn upload_part(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
        offset: u64,
        bytes: Vec<u8>,
        cancel: &CancellationToken,
    ) -> Result<UploadStep>;
    async fn upload_part_for_operation(
        &self,
        _operation: &str,
        request: &UploadRequest,
        checkpoint: &SecretString,
        offset: u64,
        bytes: Vec<u8>,
        cancel: &CancellationToken,
    ) -> Result<UploadStep> {
        self.upload_part(request, checkpoint, offset, bytes, cancel)
            .await
    }
    async fn upload_stream(
        &self,
        _request: &UploadRequest,
        _checkpoint: &SecretString,
        _payload: File,
        _cancel: &CancellationToken,
    ) -> Result<UploadStep> {
        Err(UploadError::Unsupported("streaming upload"))
    }
    async fn upload_stream_for_operation(
        &self,
        _operation: &str,
        request: &UploadRequest,
        checkpoint: &SecretString,
        payload: File,
        cancel: &CancellationToken,
    ) -> Result<UploadStep> {
        self.upload_stream(request, checkpoint, payload, cancel)
            .await
    }
    async fn commit_upload(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
        cancel: &CancellationToken,
    ) -> Result<UploadStep>;
    /// A bounded provider-specific deadline for commit and its required
    /// readback. The default preserves the existing worker behavior.
    fn commit_timeout(&self, _request: &UploadRequest) -> Duration {
        Duration::from_secs(125)
    }
    async fn commit_upload_for_operation(
        &self,
        _operation: &str,
        request: &UploadRequest,
        checkpoint: &SecretString,
        cancel: &CancellationToken,
    ) -> Result<UploadStep> {
        self.commit_upload(request, checkpoint, cancel).await
    }
    /// Committed requires actual content/identity evidence, not just matching size.
    async fn reconcile_upload(
        &self,
        request: &UploadRequest,
        checkpoint: Option<&SecretString>,
        cancel: &CancellationToken,
    ) -> Result<Reconciliation>;
    async fn reconcile_upload_for_operation(
        &self,
        _operation: &str,
        request: &UploadRequest,
        checkpoint: Option<&SecretString>,
        cancel: &CancellationToken,
    ) -> Result<Reconciliation> {
        self.reconcile_upload(request, checkpoint, cancel).await
    }
}
