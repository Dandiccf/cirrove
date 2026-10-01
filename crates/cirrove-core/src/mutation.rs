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
    /// The provider reported insufficient server storage, not necessarily a
    /// user quota. Retain the operation and wait for an explicit retry.
    #[error(
        "the cloud service reported insufficient storage; local changes are kept; retry explicitly when storage is available"
    )]
    InsufficientStorage,
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
    /// Trash one original native-document container, never its generated children.
    /// This explicit action is not POSIX rmdir and does not require an empty
    /// projected directory. Only an adapter with native recovery proof may apply it.
    TrashNativeDocument {
        before: Node,
    },
}
impl MutationIntent {
    pub fn before(&self) -> Option<&Node> {
        match self {
            Self::CreateFolder { .. } => None,
            Self::Relocate { before, .. }
            | Self::RemoveFile { before }
            | Self::RemoveFolder { before }
            | Self::TrashNativeDocument { before } => Some(before),
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
            MutationIntent::TrashNativeDocument { before } => {
                before.kind == NodeKind::Folder && before.package
            }
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
                // Creating an exact prepared identity can be acknowledged even
                // when a provider exposes no token for a later conditional edit.
                !node.id.is_empty()
                    && node.kind == NodeKind::Folder
                    && node.target.is_none()
                    && &node.name == name
                    && node.parent_id.as_ref() == Some(parent)
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
                MutationIntent::RemoveFile { before }
                | MutationIntent::RemoveFolder { before }
                | MutationIntent::TrashNativeDocument { before },
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
/// Provider attestation that the exact source and result revisions have the
/// same full SHA-256 content. The source digest must have been captured before
/// dispatch under the source ETag; a hash of only the current file is not proof.
/// This is separate from a provider content-version token and never becomes a
/// cache identity. Scope, item, both ETags and size bind its use to one receipt.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedMutationContent {
    pub scope: Scope,
    pub item: String,
    pub source_etag: String,
    pub result_etag: String,
    pub size: u64,
    pub sha256: String,
}
impl VerifiedMutationContent {
    /// Call only after verifying both full revisions, or recovering a durable
    /// conditional receipt whose original and result content were so verified.
    pub fn for_relocation(
        request: &MutationRequest,
        receipt: &MutationReceipt,
        sha256: String,
    ) -> Result<Self> {
        let (MutationIntent::Relocate { before, .. }, MutationReceipt::Upsert(after)) =
            (&request.intent, receipt)
        else {
            return Err(MutationError::Invalid);
        };
        let proof = Self {
            scope: request.scope.clone(),
            item: before.id.clone(),
            source_etag: before.etag.clone().ok_or(MutationError::Invalid)?,
            result_etag: after.etag.clone().ok_or(MutationError::Invalid)?,
            size: before.size,
            sha256,
        };
        if !proof.valid_for(request, receipt) {
            return Err(MutationError::Invalid);
        }
        Ok(proof)
    }
    pub fn valid_for(&self, request: &MutationRequest, receipt: &MutationReceipt) -> bool {
        let (MutationIntent::Relocate { before, .. }, MutationReceipt::Upsert(after)) =
            (&request.intent, receipt)
        else {
            return false;
        };
        request.accepts(receipt)
            && self.scope == request.scope
            && self.item == before.id
            && before.kind == NodeKind::File
            && !before.package
            && !after.package
            && before.target.is_none()
            && before.etag.as_deref() == Some(self.source_etag.as_str())
            && after.etag.as_deref() == Some(self.result_etag.as_str())
            && [&self.source_etag, &self.result_etag]
                .into_iter()
                .all(|etag| {
                    !etag.is_empty() && etag.len() <= 4096 && !etag.contains(['\r', '\n', '*'])
                })
            && self.size == before.size
            && self.size == after.size
            && self.sha256.len() == 64
            && self
                .sha256
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    }
}

pub enum MutationReconciliation {
    /// The namespace result is observed. A later content edit may be included;
    /// consumers must verify content lineage before rebasing a subsequent save.
    Applied(MutationReceipt),
    /// Unlike a namespace-only observation, the adapter verified unchanged
    /// content under the original and result ETags. Journal code validates the
    /// attestation's binding; the provider is responsible for its byte evidence.
    AppliedWithVerifiedContent {
        receipt: MutationReceipt,
        proof: VerifiedMutationContent,
    },
    Uncommitted,
    Conflict,
    /// Cannot distinguish a lost success from another actor/access change.
    Indeterminate,
}
/// What a provider can actually do about deletion.
///
/// Both default to false, and that is the whole point of the type. A provider
/// that has not said it has a recycle bin must not have its ordinary delete
/// presented to a person as recoverable, and one that has not said it can
/// delete permanently must not show a menu entry that quietly falls back to the
/// ordinary delete. Those two are the failures ADR 0008 exists to prevent, and
/// a default of "yes" would reintroduce both by silence.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DeletionSupport {
    /// An ordinary delete is recoverable by the person, without our help.
    pub recycle_bin: bool,
    /// Something can be removed without passing through that recovery.
    pub permanent: bool,
}

#[async_trait]
pub trait MutationProvider: Send + Sync {
    /// Reconcile a fresh read-side observation with the exact node returned by
    /// a completed write. Providers whose mounted namespace encodes identity in
    /// the visible name may restore the acknowledged user-facing name here.
    ///
    /// Callers accept only a name change; identity, parent, kind, versions and
    /// all other metadata continue to come from `observed`.
    fn present_observation(&self, _acknowledged: &Node, observed: Node) -> Node {
        observed
    }

    /// What this provider can do about deletion. See [`DeletionSupport`] for
    /// why the default is "neither".
    fn deletion(&self) -> DeletionSupport {
        DeletionSupport::default()
    }

    /// Remove an item without passing through the provider's recovery.
    ///
    /// Never the answer to an ordinary `unlink`: POSIX has one delete and no
    /// flag in which "and skip the recycle bin" could live, so this is only
    /// ever reached by a person asking for it a second time, in words. The
    /// eTag precondition is carried for the same reason it is carried
    /// everywhere else -- the thing being destroyed must be the thing that was
    /// looked at.
    ///
    /// The default refuses, so a provider that has not implemented it cannot
    /// have an ordinary delete quietly substituted for what was asked.
    async fn delete_permanently(
        &self,
        _scope: &Scope,
        _item: &str,
        _etag: Option<&str>,
        _cancel: &CancellationToken,
    ) -> Result<()> {
        Err(MutationError::Invalid)
    }

    /// Reserve an exact provider item identity before the first mutating call.
    ///
    /// This call must not mutate provider state. The worker persists the returned item ID together with the account and
    /// collection already carried by `request`. It must therefore be an opaque
    /// item identity only, never a token, cursor or signed URL. Providers whose
    /// namespace operations do not need preparation use the default `None`.
    async fn prepare_mutation(
        &self,
        _request: &MutationRequest,
        _cancel: &CancellationToken,
    ) -> Result<Option<String>> {
        Ok(None)
    }

    /// Prepare against the durable journal identity before any remote mutation.
    /// Providers may persist read-only preflight evidence under this identity;
    /// returned values still obey the opaque-item-only contract above.
    async fn prepare_mutation_for_operation(
        &self,
        _operation: &str,
        request: &MutationRequest,
        cancel: &CancellationToken,
    ) -> Result<Option<String>> {
        self.prepare_mutation(request, cancel).await
    }

    /// Return the actual conditional mutation receipt. A later independent GET
    /// can include another actor's edit and is not an equivalent upload base.
    async fn mutate(
        &self,
        request: &MutationRequest,
        cancel: &CancellationToken,
    ) -> Result<MutationReceipt>;

    /// Apply a mutation using the exact identity saved by
    /// [`Self::prepare_mutation`]. The default rejects a prepared identity and
    /// preserves the existing unprepared provider contract.
    async fn mutate_prepared(
        &self,
        request: &MutationRequest,
        prepared_item: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<MutationReceipt> {
        if prepared_item.is_some() {
            return Err(MutationError::Invalid);
        }
        self.mutate(request, cancel).await
    }
    /// Execute with the durable journal operation ID. A provider may use this
    /// ID to save a response-only item identity before it returns a receipt.
    /// Existing providers that do not need it retain their prepared contract.
    async fn mutate_operation(
        &self,
        _operation: &str,
        request: &MutationRequest,
        prepared_item: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<MutationReceipt> {
        self.mutate_prepared(request, prepared_item, cancel).await
    }
    async fn reconcile_mutation(
        &self,
        request: &MutationRequest,
        cancel: &CancellationToken,
    ) -> Result<MutationReconciliation>;

    /// Reconcile using the same durable provider identity. A provider that
    /// prepares an identity must override this together with
    /// [`Self::mutate_prepared`].
    async fn reconcile_prepared_mutation(
        &self,
        request: &MutationRequest,
        prepared_item: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<MutationReconciliation> {
        if prepared_item.is_some() {
            return Err(MutationError::Invalid);
        }
        self.reconcile_mutation(request, cancel).await
    }
    /// Reconcile the same durable operation after an uncertain response or a
    /// process restart. The default preserves existing provider behavior.
    async fn reconcile_operation(
        &self,
        _operation: &str,
        request: &MutationRequest,
        prepared_item: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<MutationReconciliation> {
        self.reconcile_prepared_mutation(request, prepared_item, cancel)
            .await
    }
}

#[cfg(test)]
mod native_trash_tests {
    use super::*;
    fn request() -> MutationRequest {
        MutationRequest {
            scope: Scope {
                account: "owned".into(),
                provider: "icloud".into(),
                collection: "drive".into(),
            },
            intent: MutationIntent::TrashNativeDocument {
                before: Node {
                    id: "FILE::com.apple.CloudDocs::owned".into(),
                    parent_id: Some("parent".into()),
                    name: "Owned.pages".into(),
                    kind: NodeKind::Folder,
                    package: true,
                    etag: Some("E1".into()),
                    content_version: None,
                    target: None,
                    size: 17,
                    modified_unix: 0,
                },
            },
        }
    }
    #[test]
    fn native_trash_intent_requires_original_container_and_exact_removed_receipt() {
        let valid = request();
        assert!(valid.validate().is_ok());
        let id = valid.intent.before().expect("source").id.clone();
        assert!(valid.accepts(&MutationReceipt::Removed { item: id }));
        assert!(!valid.accepts(&MutationReceipt::Removed {
            item: "other".into()
        }));
        assert!(!valid.accepts(&MutationReceipt::Upsert(
            valid.intent.before().expect("source").clone()
        )));
        for arm in 0..5 {
            let mut changed = request();
            let MutationIntent::TrashNativeDocument { before } = &mut changed.intent else {
                unreachable!()
            };
            match arm {
                0 => before.package = false,
                1 => before.kind = NodeKind::File,
                2 => before.etag = None,
                3 => before.parent_id = None,
                _ => before.kind = NodeKind::Shortcut,
            }
            assert!(changed.validate().is_err(), "arm {arm}");
        }
    }
}
