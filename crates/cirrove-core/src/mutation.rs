//! Conditional namespace changes. A missing response never authorizes replay.
use crate::{CancellationToken, Node, NodeKind, ProviderError, Scope};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, thiserror::Error)]
pub enum MutationError {
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error("remote item or destination changed; resolve the conflict")]
    Conflict,
    #[error("invalid namespace change")]
    Invalid,
    #[error("remote item is locked")]
    Locked,
    #[error("cloud storage quota exceeded")]
    Quota,
    #[error("namespace change has an uncertain outcome; verify before retrying")]
    Uncertain,
    #[error("namespace operation is not supported: {0}")]
    Unsupported(&'static str),
}
pub type Result<T> = std::result::Result<T, MutationError>;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MutationIntent {
    CreateFolder {
        parent: String,
        name: String,
    },
    /// Rename and/or move within the same collection, refusing a name collision.
    Relocate {
        before: Node,
        parent: String,
        name: String,
    },
    /// Delete a regular file using its original ETag. This is not recursive rmdir.
    RemoveFile {
        before: Node,
    },
    /// Delete an **empty** folder using its original ETag, the way POSIX `rmdir`
    /// does. Never recursive: a folder with children must be emptied first, one
    /// confirmed removal at a time, so that every deletion carries its own
    /// precondition.
    ///
    /// Adapters must verify emptiness immediately before deleting, because the
    /// provider call underneath may well be recursive -- Graph's is. A folder's
    /// eTag on OneDrive does not move when a child is added (measured: see
    /// `folder_etag_and_mtime_ignore_their_children`), so a precondition on the
    /// folder cannot close the window between that check and the delete. It can
    /// only be narrowed to one round trip. Implementations must not pretend
    /// otherwise, and callers must not present `rmdir` as atomic.
    RemoveFolder {
        before: Node,
    },
}
impl MutationIntent {
    pub fn before(&self) -> Option<&Node> {
        match self {
            Self::CreateFolder { .. } => None,
            Self::Relocate { before, .. }
            | Self::RemoveFile { before }
            | Self::RemoveFolder { before } => Some(before),
        }
    }
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutationRequest {
    pub scope: Scope,
    pub intent: MutationIntent,
}
fn text(value: &str) -> bool {
    !value.is_empty() && value.len() <= 4096 && !value.contains('\0')
}
fn name(value: &str) -> bool {
    text(value) && !matches!(value, "." | "..") && !value.contains('/')
}
impl MutationRequest {
    pub fn validate(&self) -> Result<()> {
        let scope = &self.scope;
        if !text(&scope.account) || !text(&scope.provider) || !text(&scope.collection) {
            return Err(MutationError::Invalid);
        }
        if let Some(before) = self.intent.before()
            && (!text(&before.id)
                || !name(&before.name)
                || !before.parent_id.as_deref().is_some_and(text)
                || !before
                    .etag
                    .as_deref()
                    .is_some_and(|s| text(s) && !s.contains(['\r', '\n', '*']))
                || before.target.is_some()
                || before.kind == NodeKind::Shortcut)
        {
            return Err(MutationError::Invalid);
        }
        let valid = match &self.intent {
            MutationIntent::CreateFolder {
                parent,
                name: value,
            }
            | MutationIntent::Relocate {
                parent,
                name: value,
                ..
            } => text(parent) && name(value),
            MutationIntent::RemoveFile { before } => before.kind == NodeKind::File,
            MutationIntent::RemoveFolder { before } => before.kind == NodeKind::Folder,
        };
        if !valid {
            return Err(MutationError::Invalid);
        }
        if let MutationIntent::Relocate { before, parent, .. } = &self.intent
            && &before.id == parent
        {
            return Err(MutationError::Invalid);
        }
        Ok(())
    }
    pub fn accepts(&self, receipt: &MutationReceipt) -> bool {
        match (&self.intent, receipt) {
            (MutationIntent::CreateFolder { parent, name }, MutationReceipt::Upsert(node)) => {
                !node.id.is_empty()
                    && node.kind == NodeKind::Folder
                    && node.target.is_none()
                    && &node.name == name
                    && node.parent_id.as_ref() == Some(parent)
                    && node.etag.as_ref().is_some_and(|s| !s.is_empty())
            }
            (
                MutationIntent::Relocate {
                    before,
                    parent,
                    name,
                },
                MutationReceipt::Upsert(node),
            ) => {
                node.id == before.id
                    && node.kind == before.kind
                    && node.target.is_none()
                    && &node.name == name
                    && node.parent_id.as_ref() == Some(parent)
                    && node.etag.as_ref().is_some_and(|s| !s.is_empty())
            }
            (
                MutationIntent::RemoveFile { before } | MutationIntent::RemoveFolder { before },
                MutationReceipt::Removed { item },
            ) => &before.id == item,
            _ => false,
        }
    }
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum MutationReceipt {
    Upsert(Node),
    /// Confirmed mutation response; a lookup returning 404 alone is insufficient.
    Removed {
        item: String,
    },
}
pub enum MutationReconciliation {
    /// The namespace result is observed. A later content edit may be included;
    /// consumers must verify content lineage before rebasing a subsequent save.
    Applied(MutationReceipt),
    Uncommitted,
    Conflict,
    /// Cannot distinguish a lost success from another actor/access change.
    Indeterminate,
}
#[async_trait]
pub trait MutationProvider: Send + Sync {
    /// Return the actual conditional mutation receipt. A later independent GET
    /// can include another actor's edit and is not an equivalent upload base.
    async fn mutate(
        &self,
        request: &MutationRequest,
        cancel: &CancellationToken,
    ) -> Result<MutationReceipt>;
    async fn reconcile_mutation(
        &self,
        request: &MutationRequest,
        cancel: &CancellationToken,
    ) -> Result<MutationReconciliation>;
}
