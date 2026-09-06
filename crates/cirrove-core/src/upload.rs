//! Provider upload contract consumed by the durable local journal and its worker.
use crate::{CancellationToken, Node, ProviderError, Scope};
use async_trait::async_trait;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, thiserror::Error)]
pub enum UploadError {
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error("remote content or destination conflicts with this edit")]
    Conflict,
    #[error("cloud storage quota exceeded")]
    Quota,
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
            } => valid(item) && valid(expected_etag) && !expected_etag.contains(['\r', '\n']),
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
    pub scope: Scope,
    pub intent: UploadIntent,
    pub size: u64,
    pub sha256: String,
}
impl UploadRequest {
    pub fn validate(&self) -> Result<()> {
        self.intent.validate()?;
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
    Continue(UploadProgress),
    /// Bytes are staged remotely; persist this checkpoint before the conditional
    /// final commit makes them visible as the replacement file.
    Commit(SecretString),
    Complete(Node),
}
impl std::fmt::Debug for UploadStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Continue(_) => "UploadStep::Continue([redacted])",
            Self::Commit(_) => "UploadStep::Commit([redacted])",
            Self::Complete(_) => "UploadStep::Complete([redacted])",
        })
    }
}
pub enum Reconciliation {
    Committed(Node),
    Uncommitted,
    Conflict,
}

/// A caller persists every returned checkpoint before sending its next range.
/// After an uncertain result, inspect the same session; a missing session alone
/// does not prove failure. Reconcile the remote target before restarting it.
#[async_trait]
pub trait UploadProvider: Send + Sync {
    async fn begin_upload(
        &self,
        request: &UploadRequest,
        cancel: &CancellationToken,
    ) -> Result<UploadStep>;
    async fn inspect_upload(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
        cancel: &CancellationToken,
    ) -> Result<UploadStep>;
    async fn upload_part(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
        offset: u64,
        bytes: Vec<u8>,
        cancel: &CancellationToken,
    ) -> Result<UploadStep>;
    async fn commit_upload(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
        cancel: &CancellationToken,
    ) -> Result<UploadStep>;
    /// Committed requires actual content/identity evidence, not just matching size.
    async fn reconcile_upload(
        &self,
        request: &UploadRequest,
        cancel: &CancellationToken,
    ) -> Result<Reconciliation>;
}
