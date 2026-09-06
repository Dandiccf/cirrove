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
    #[serde(default)]
    pub kind: Option<NodeKind>,
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
    #[serde(default)]
    pub modified_unix: u64,
    pub etag: Option<String>,
    /// Provider content revision, unaffected by metadata-only changes when available.
    #[serde(default)]
    pub content_version: Option<String>,
    /// A link across drives must retain the target identity, never just a path.
    pub target: Option<RemoteRef>,
}
impl Node {
    /// Retain the tag namespace: a content tag must not collide with an ETag.
    pub fn content_revision(&self) -> Option<(&'static str, &str)> {
        self.content_version
            .as_deref()
            .filter(|tag| !tag.is_empty())
            .map(|tag| ("content", tag))
            .or_else(|| {
                self.etag
                    .as_deref()
                    .filter(|tag| !tag.is_empty())
                    .map(|tag| ("etag", tag))
            })
    }
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

#[derive(Clone, Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("remote file changed; reopen it to read the new version")]
    VersionChanged,
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
/// Read operations extend this contract separately. Mutation contracts require
/// journal and conflict semantics before introduction (see architecture.md).
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

#[derive(Clone, Debug)]
pub struct DirectoryPage {
    pub nodes: Vec<Node>,
    pub next: Option<Cursor>,
}

/// Read-only filesystem operations, deliberately separate from change feeds.
#[async_trait]
pub trait ReadProvider: MetadataProvider {
    async fn node(
        &self,
        scope: &Scope,
        id: &str,
        cancel: &CancellationToken,
    ) -> Result<Node, ProviderError>;
    async fn children(
        &self,
        scope: &Scope,
        parent: &str,
        cursor: Option<&Cursor>,
        cancel: &CancellationToken,
    ) -> Result<DirectoryPage, ProviderError>;
    /// Return exactly the requested range, bounded by EOF. Never return bytes of
    /// another version of `node`; a changed object must yield VersionChanged.
    async fn read_range(
        &self,
        scope: &Scope,
        node: &Node,
        offset: u64,
        length: u32,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>, ProviderError>;
}

#[derive(Clone, Copy)]
pub enum Priority {
    Interactive,
    Background,
    Content,
}

/// Separate bounded pools reserve capacity for interactive work. These are per
/// provider account; provider cooldowns still apply to both pools.
pub struct RequestBudget {
    interactive: Arc<Semaphore>,
    background: Arc<Semaphore>,
    content: Arc<Semaphore>,
}
impl Default for RequestBudget {
    fn default() -> Self {
        Self {
            interactive: Arc::new(Semaphore::new(4)),
            background: Arc::new(Semaphore::new(1)),
            content: Arc::new(Semaphore::new(4)),
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
            Priority::Content => &self.content,
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
    #[tokio::test]
    async fn downloads_cannot_consume_folder_request_capacity() {
        let budget = RequestBudget::default();
        let cancel = CancellationToken::new();
        let mut downloads = vec![];
        for _ in 0..4 {
            downloads.push(budget.acquire(Priority::Content, &cancel).await.unwrap());
        }
        let _directory = tokio::time::timeout(
            Duration::from_millis(100),
            budget.acquire(Priority::Interactive, &cancel),
        )
        .await
        .unwrap()
        .unwrap();
        cancel.cancel();
        assert!(matches!(
            budget.acquire(Priority::Content, &cancel).await,
            Err(ProviderError::Cancelled)
        ));
    }
}
