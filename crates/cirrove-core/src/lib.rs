//! Provider-neutral metadata contracts. Paths are presentation; IDs are identity.
pub mod auth;
pub use auth::{StaticToken, TokenSource};
pub mod mutation;
pub mod notifications;
pub mod reads;
pub mod upload;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
pub use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    // Keep the uncommon cross-drive target out of every ordinary node allocation.
    pub target: Option<Box<RemoteRef>>,
    /// A provider package: one thing to a person, a folder to the provider.
    ///
    /// A OneNote notebook is the case that forced this. Graph gives it a folder
    /// facet and a package facet, and beneath it are section files that only
    /// OneNote knows how to write. A filesystem that lets someone edit or
    /// rename one of those is offering to corrupt a notebook, so the mount
    /// shows the contents and refuses to change them.
    ///
    /// `#[serde(default)]` so a node written before this field reads back as
    /// false, which is what every node in an index written then was -- and
    /// skipped on the way out when it is false, so the stored form of every
    /// ordinary node is byte-for-byte what it was. An index holding 184,000
    /// nodes should not be rewritten, or grow, to record that almost none of
    /// them are notebooks.
    #[serde(default, skip_serializing_if = "not")]
    pub package: bool,
}
/// `skip_serializing_if` wants a predicate on a reference, which `bool` has no
/// inherent method for.
fn not(value: &bool) -> bool {
    !*value
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
    /// Maintain one collection's notification session until cancellation, renewal
    /// or failure. Implementations report connected only after subscribing, and
    /// turn remote hints into `changed`; metadata is still fetched via `changes`.
    async fn watch_changes(
        &self,
        _scope: &Scope,
        _hints: notifications::ChangeHintSender,
        _cancel: &CancellationToken,
    ) -> Result<notifications::WatchEnd, ProviderError> {
        Ok(notifications::WatchEnd::Unsupported)
    }
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

/// What an adapter's read path has actually done since the process started.
///
/// This exists because a validator cannot observe it. The bounded sequential
/// window path runs only behind the service's content cache, so a check that
/// talks to an adapter directly reports zero windows however well the mounted
/// daemon is doing -- which is exactly the wrong conclusion to draw from a zero.
/// Reporting the counters from a running daemon is the only way to see whether
/// the optimized read session is working on real data.
#[derive(Debug, Default, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ReadPathCounters {
    /// Transport sessions established from cold.
    pub setups: u64,
    /// Sessions re-bound after their lease expired rather than set up again.
    pub renewals: u64,
    /// Exact ranges served against a live session binding.
    pub conditional_ranges: u64,
    /// Bounded sequential windows served against a live session binding.
    pub conditional_windows: u64,
    /// Ranges served without a usable session, on the conservative path.
    pub fallback_ranges: u64,
    /// Windows served without a usable session, on the conservative path. A
    /// session that degrades to fallback never re-binds, so this rising while
    /// `renewals` stays at zero is the expected shape rather than a puzzle.
    pub fallback_windows: u64,
    /// Every Graph metadata GET this adapter attempted, whether or not a read
    /// session was involved. Without it a read served outside the session paths
    /// is indistinguishable from no read at all.
    pub graph_gets: u64,
    /// Every content GET this adapter attempted, on any path.
    pub content_gets: u64,
}

/// Why a provider would refuse a name, decided before anything is written.
///
/// `TooLong` is a limit and maps to the limit's errno; `Invalid` carries the
/// offending part in words, for a journal line or a test failure that says
/// what was wrong rather than only that something was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameProblem {
    TooLong,
    Invalid(&'static str),
}
impl std::fmt::Display for NameProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLong => write!(f, "the name is too long"),
            Self::Invalid(what) => write!(f, "the name contains {what}"),
        }
    }
}

/// Read-only filesystem operations, deliberately separate from change feeds.
#[async_trait]
pub trait ReadProvider: MetadataProvider {
    /// Package contents generated by this adapter may change across app
    /// versions without a remote change. Recheck cached package listings once
    /// when a new service process first opens them.
    fn refresh_cached_packages_on_first_open(&self) -> bool {
        false
    }
    /// Why this provider would refuse a file or folder name, or `None`. The
    /// mount asks before creating or renaming, so a name the cloud will not
    /// take fails at the application that chose it and not an hour later as a
    /// change the daemon gave up on. The default accepts everything; a
    /// provider with rules states them.
    fn name_problem(&self, _name: &str) -> Option<NameProblem> {
        None
    }
    /// Read-path counters, for adapters that keep them. `None` means the adapter
    /// does not count, which is not the same as counting zero.
    fn read_path_counters(&self) -> Option<ReadPathCounters> {
        None
    }
    /// Optional version-bound transport. The shared service coalesces creation
    /// and bounds residency; adapters keep credentials and validators private.
    /// None uses the existing exact-range contract without a transport session.
    async fn open_read_session(
        &self,
        _scope: &Scope,
        _node: &Node,
        _cancel: &CancellationToken,
    ) -> Result<Option<Arc<dyn reads::ReadSession>>, ProviderError> {
        Ok(None)
    }
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
    /// List a parent for which the service already has provider metadata.
    ///
    /// The default preserves the ordinary remote-directory contract. Providers
    /// may use the parent metadata to expand an opaque provider package into
    /// read-only representations whose identities are derived from the remote
    /// item rather than from a presentation path.
    async fn children_for_node(
        &self,
        scope: &Scope,
        parent: &Node,
        cursor: Option<&Cursor>,
        cancel: &CancellationToken,
    ) -> Result<DirectoryPage, ProviderError> {
        self.children(scope, &parent.id, cursor, cancel).await
    }
    /// Bytes already materialized while constructing a derived directory entry.
    ///
    /// Generated provider representations sometimes have no trustworthy size
    /// until the provider has produced the complete artifact. The service asks
    /// for those exact bytes before publishing the entry so its ordinary disk
    /// cache, rather than an adapter's bounded scratch memory, owns subsequent
    /// reads. Ordinary remote files return `None`.
    async fn staged_content(
        &self,
        _scope: &Scope,
        _node: &Node,
        _cancel: &CancellationToken,
    ) -> Result<Option<Arc<[u8]>>, ProviderError> {
        Ok(None)
    }
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
    Upload,
}

/// Separate bounded pools reserve capacity for interactive work. These are per
/// provider account; provider cooldowns still apply to both pools.
#[derive(Clone)]
pub struct RequestBudget {
    interactive: Arc<Semaphore>,
    background: Arc<Semaphore>,
    content: Arc<Semaphore>,
    uploads: Arc<Semaphore>,
}
impl Default for RequestBudget {
    fn default() -> Self {
        Self {
            interactive: Arc::new(Semaphore::new(4)),
            background: Arc::new(Semaphore::new(1)),
            content: Arc::new(Semaphore::new(4)),
            uploads: Arc::new(Semaphore::new(2)),
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
            Priority::Upload => &self.uploads,
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
    #[tokio::test]
    async fn uploads_leave_directory_and_download_capacity_available() {
        let budget = RequestBudget::default();
        let cancel = CancellationToken::new();
        let _first = budget.acquire(Priority::Upload, &cancel).await.unwrap();
        let _second = budget.acquire(Priority::Upload, &cancel).await.unwrap();
        for priority in [
            Priority::Interactive,
            Priority::Content,
            Priority::Background,
        ] {
            let _permit = tokio::time::timeout(
                Duration::from_millis(100),
                budget.acquire(priority, &cancel),
            )
            .await
            .unwrap()
            .unwrap();
        }
        assert!(
            tokio::time::timeout(
                Duration::from_millis(10),
                budget.acquire(Priority::Upload, &cancel)
            )
            .await
            .is_err()
        );
        cancel.cancel();
        assert!(matches!(
            budget.acquire(Priority::Upload, &cancel).await,
            Err(ProviderError::Cancelled)
        ));
    }
}

/// A saved provider collection. The legacy JSON spelling remains readable by
/// existing OneDrive settings; the type itself belongs to no adapter.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionInfo {
    pub id: String,
    pub name: String,
    pub drive_type: String,
    #[serde(default)]
    pub web_url: String,
}
