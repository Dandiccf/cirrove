//! Provider-independent identity and lifetime for version-bound ranged reads.
use crate::{CancellationToken, Node, NodeKind, ProviderError, Scope};
use async_trait::async_trait;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ReadIdentity {
    pub scope: Scope,
    pub item: String,
    pub revision_namespace: String,
    pub revision: String,
    pub size: u64,
}
impl ReadIdentity {
    pub fn new(scope: &Scope, node: &Node) -> Result<Self, ProviderError> {
        let (namespace, revision) = node
            .content_revision()
            .ok_or(ProviderError::Protocol("file has no version tag"))?;
        if node.kind != NodeKind::File {
            return Err(ProviderError::Protocol("read session requires a file"));
        }
        Ok(Self {
            scope: scope.clone(),
            item: node.id.clone(),
            revision_namespace: namespace.into(),
            revision: revision.into(),
            size: node.size,
        })
    }
}

/// An adapter-private transport context for exactly one content identity.
/// Implementations bound setup/renewal and validate every returned representation;
/// a retained transport URL is never itself a content-version guarantee.
#[async_trait]
pub trait ReadSession: Send + Sync {
    fn identity(&self) -> &ReadIdentity;
    /// Maximum useful streamed window for the current validation strategy.
    /// Zero keeps exact-range reads. This is a capability, not a prefetch request.
    fn window_limit(&self) -> u32 {
        0
    }
    /// Stream untrusted bytes into private staging. Only successful completion
    /// validates the entire range against this session's identity. On any error
    /// or cancellation the caller must discard every staged byte.
    async fn read_window(
        &self,
        _offset: u64,
        _length: u32,
        _sink: &mut dyn ReadWindowSink,
        _cancel: &CancellationToken,
    ) -> Result<(), ProviderError> {
        Err(ProviderError::Protocol("streamed read windows unsupported"))
    }
    async fn read_range(
        &self,
        offset: u64,
        length: u32,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>, ProviderError>;
}

/// Caller-owned staging sink. Adapters send chunks of at most 64 KiB, never expose storage
/// paths, and cannot publish incomplete windows. Sink errors abort the transfer.
#[async_trait]
pub trait ReadWindowSink: Send {
    async fn write_chunk(&mut self, bytes: &[u8]) -> Result<(), ProviderError>;
}
