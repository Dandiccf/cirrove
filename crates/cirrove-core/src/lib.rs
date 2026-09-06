//! Provider-neutral metadata contracts. Paths are presentation; IDs are identity.
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
pub use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    pub account: String,
    pub provider: String,
    /// One independently tracked change feed (a drive for Microsoft Graph).
    pub collection: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteRef {
    pub collection: String,
    pub item: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    File,
    Folder,
    Shortcut,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub parent_id: Option<String>,
    pub name: String,
    pub kind: NodeKind,
    pub size: u64,
    pub etag: Option<String>,
    /// A link across drives must retain the target identity, never just a path.
    pub target: Option<RemoteRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Change {
    Upsert(Node),
    Delete { id: String },
}

/// Opaque pagination/checkpoint material; never log the contents.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor(pub String);
impl std::fmt::Debug for Cursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cursor([redacted])")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Checkpoint {
    Continue(Cursor),
    Complete(Cursor),
}
impl Checkpoint {
    pub fn cursor(&self) -> &Cursor {
        match self {
            Self::Continue(c) | Self::Complete(c) => c,
        }
    }
    pub fn complete(&self) -> bool {
        matches!(self, Self::Complete(_))
    }
}

#[derive(Clone, Debug)]
pub struct ChangePage {
    pub changes: Vec<Change>,
    pub checkpoint: Checkpoint,
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("operation cancelled")]
    Cancelled,
    #[error("sign-in required")]
    Authentication,
    #[error("access denied")]
    Permission,
    #[error("remote item not found")]
    NotFound,
    #[error("provider requested a new metadata baseline")]
    CursorExpired,
    #[error("provider throttled requests; retry after {0:?}")]
    Throttled(Duration),
    #[error("temporary provider failure")]
    Unavailable,
    #[error("invalid or unsupported provider response: {0}")]
    Protocol(&'static str),
}

/// Every provider owns its authentication and translates its API into this contract.
/// The first milestone supports metadata only. Content and mutation contracts follow
/// after version/ETag and crash-recovery semantics are agreed (see architecture.md).
#[async_trait]
pub trait MetadataProvider: Send + Sync {
    fn provider_id(&self) -> &'static str;
    async fn changes(
        &self,
        scope: &Scope,
        cursor: Option<&Cursor>,
        cancel: &CancellationToken,
    ) -> Result<ChangePage, ProviderError>;
}

#[derive(Clone, Copy)]
pub enum Priority {
    Interactive,
    Background,
}

/// Separate bounded pools reserve capacity for interactive work. These are per
/// provider account; provider cooldowns still apply to both pools.
pub struct RequestBudget {
    interactive: Arc<Semaphore>,
    background: Arc<Semaphore>,
}
impl Default for RequestBudget {
    fn default() -> Self {
        Self {
            interactive: Arc::new(Semaphore::new(4)),
            background: Arc::new(Semaphore::new(1)),
        }
    }
}
impl RequestBudget {
    pub async fn acquire(
        &self,
        priority: Priority,
        cancel: &CancellationToken,
    ) -> Result<OwnedSemaphorePermit, ProviderError> {
        let pool = match priority {
            Priority::Interactive => &self.interactive,
            Priority::Background => &self.background,
        };
        tokio::select! { biased;
            _ = cancel.cancelled() => Err(ProviderError::Cancelled),
            permit = pool.clone().acquire_owned() => permit.map_err(|_| ProviderError::Unavailable),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn background_saturation_does_not_block_interactive_work() {
        let budget = RequestBudget::default();
        let cancel = CancellationToken::new();
        let _background = budget.acquire(Priority::Background, &cancel).await.unwrap();
        let _interactive = tokio::time::timeout(
            Duration::from_millis(100),
            budget.acquire(Priority::Interactive, &cancel),
        )
        .await
        .unwrap()
        .unwrap();
        cancel.cancel();
        assert!(matches!(
            budget.acquire(Priority::Background, &cancel).await,
            Err(ProviderError::Cancelled)
        ));
    }
    #[test]
    fn cursor_debug_does_not_leak_material() {
        assert!(!format!("{:?}", Cursor("secret-query".into())).contains("secret-query"));
    }
}
