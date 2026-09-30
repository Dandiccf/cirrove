//! Exact-parent iCloud file creation through the durable upload worker.
//! Ordinary accounts remain read-only until replacement and folder writes are
//! validated together with this adapter.
use crate::{
    DriveEntry, ICloudReadSession, ROOT_ID, SealedSessionVault, checked_content_url,
    upload_transport::{UploadSlot, UploadedFile},
};
use async_trait::async_trait;
use cirrove_auth::CredentialVault;
use cirrove_core::upload::{
    Reconciliation, Result as UploadResult, UploadError, UploadIntent, UploadProvider,
    UploadRequest, UploadStep,
};
use cirrove_core::{CancellationToken, Node, NodeKind, Scope};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::Arc,
};
use tokio::sync::Mutex;
use uuid::Uuid;

const MAX_CHECKPOINT: usize = 8192;

fn verify_upload_payload(
    bytes: &mut impl Read,
    expected_size: u64,
    expected_hash: &str,
    cancel: &CancellationToken,
) -> UploadResult<()> {
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut received = 0u64;
    loop {
        if cancel.is_cancelled() {
            return Err(UploadError::Uncertain);
        }
        let read = bytes.read(&mut buffer).map_err(|_| UploadError::Invalid)?;
        if read == 0 {
            break;
        }
        received = received
            .checked_add(read as u64)
            .ok_or(UploadError::Invalid)?;
        if received > expected_size {
            return Err(UploadError::Invalid);
        }
        hash.update(&buffer[..read]);
    }
    if received != expected_size || hex::encode(hash.finalize()) != expected_hash {
        return Err(UploadError::Invalid);
    }
    Ok(())
}

enum SessionState {
    Ready(Box<ICloudReadSession>),
    Vault {
        apple_id: String,
        credential_id: String,
        vault: Arc<dyn CredentialVault>,
    },
}

#[derive(Serialize, Deserialize)]
struct CreateCheckpoint {
    version: u8,
    scope: Scope,
    parent: String,
    name: String,
    size: u64,
    sha256: String,
    slot: Option<UploadSlot>,
    receipt: Option<UploadedFile>,
}

pub struct ICloudFileCreate {
    scope: Scope,
    parent: Node,
    session: Mutex<SessionState>,
    #[cfg(feature = "write-probe")]
    reconciliation_only: bool,
    #[cfg(feature = "write-probe")]
    discard_registration_response: bool,
}

impl ICloudFileCreate {
    pub fn from_session_snapshot(
        scope: Scope,
        apple_id: &str,
        snapshot: &SecretString,
        parent: Node,
    ) -> UploadResult<Self> {
        Self::check_identity(&scope, &parent)?;
        let session = ICloudReadSession::from_session_snapshot(snapshot, apple_id)
            .map_err(|_| UploadError::Uncertain)?;
        if session.account_hash.is_none() {
            return Err(UploadError::Invalid);
        }
        Ok(Self {
            scope,
            parent,
            session: Mutex::new(SessionState::Ready(Box::new(session))),
            #[cfg(feature = "write-probe")]
            reconciliation_only: false,
            #[cfg(feature = "write-probe")]
            discard_registration_response: false,
        })
    }

    pub fn from_sealed_session(
        scope: Scope,
        apple_id: String,
        credential_id: String,
        state: &Path,
        parent: Node,
    ) -> UploadResult<Self> {
        Self::check_identity(&scope, &parent)?;
        if apple_id.trim().is_empty() || Uuid::parse_str(&credential_id).is_err() {
            return Err(UploadError::Invalid);
        }
        let vault =
            SealedSessionVault::new(state, &scope.account).map_err(|_| UploadError::Invalid)?;
        Ok(Self {
            scope,
            parent,
            session: Mutex::new(SessionState::Vault {
                apple_id,
                credential_id,
                vault: Arc::new(vault),
            }),
            #[cfg(feature = "write-probe")]
            reconciliation_only: false,
            #[cfg(feature = "write-probe")]
            discard_registration_response: false,
        })
    }

    #[cfg(feature = "write-probe")]
    pub fn reconciliation_only(mut self) -> Self {
        self.reconciliation_only = true;
        self
    }

    #[cfg(feature = "write-probe")]
    pub fn with_discarded_registration_response(mut self) -> Self {
        self.discard_registration_response = true;
        self
    }

    #[cfg(feature = "write-probe")]
    pub fn reserved_document_id(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
    ) -> UploadResult<Option<String>> {
        Ok(self
            .check_checkpoint(request, checkpoint)?
            .slot
            .map(|slot| slot.document_id))
    }

    fn check_identity(scope: &Scope, parent: &Node) -> UploadResult<()> {
        if scope.account.is_empty()
            || scope.provider != "icloud"
            || scope.collection != "drive"
            || parent.kind != NodeKind::Folder
            || !parent.id.starts_with("FOLDER::com.apple.CloudDocs::")
            || parent.id.rsplit("::").next().is_none_or(str::is_empty)
            || (parent.id == ROOT_ID && parent.parent_id.is_some())
            || (parent.id != ROOT_ID
                && !parent.parent_id.as_deref().is_some_and(|id| {
                    id.starts_with("FOLDER::com.apple.CloudDocs::") && id != parent.id
                }))
            || parent.name.is_empty()
            || parent.target.is_some()
            || parent.package
        {
            return Err(UploadError::Invalid);
        }
        Ok(())
    }

    fn check_request(&self, request: &UploadRequest) -> UploadResult<()> {
        request.validate()?;
        let UploadIntent::Create { parent, name } = &request.intent else {
            return Err(UploadError::Unsupported("iCloud file replacement"));
        };
        if request.scope != self.scope
            || parent != &self.parent.id
            || request.size > crate::MAX_WRITE_FILE_SIZE
            || name.len() > 255
            || name.contains(['\\', '\r', '\n'])
        {
            return Err(UploadError::Invalid);
        }
        Ok(())
    }

    fn checkpoint(
        &self,
        request: &UploadRequest,
        slot: Option<UploadSlot>,
        receipt: Option<UploadedFile>,
    ) -> UploadResult<SecretString> {
        self.check_request(request)?;
        let UploadIntent::Create { parent, name } = &request.intent else {
            return Err(UploadError::Invalid);
        };
        let saved = CreateCheckpoint {
            version: 1,
            scope: request.scope.clone(),
            parent: parent.clone(),
            name: name.clone(),
            size: request.size,
            sha256: request.sha256.clone(),
            slot,
            receipt,
        };
        let text = serde_json::to_string(&saved).map_err(|_| UploadError::Invalid)?;
        if text.len() > MAX_CHECKPOINT {
            return Err(UploadError::Invalid);
        }
        Ok(SecretString::from(text))
    }

    fn check_checkpoint(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
    ) -> UploadResult<CreateCheckpoint> {
        self.check_request(request)?;
        if checkpoint.expose_secret().len() > MAX_CHECKPOINT {
            return Err(UploadError::CheckpointInvalid);
        }
        let saved: CreateCheckpoint = serde_json::from_str(checkpoint.expose_secret())
            .map_err(|_| UploadError::CheckpointInvalid)?;
        let UploadIntent::Create { parent, name } = &request.intent else {
            return Err(UploadError::Invalid);
        };
        if saved.version != 1
            || saved.scope != request.scope
            || saved.parent != *parent
            || saved.name != *name
            || saved.size != request.size
            || saved.sha256 != request.sha256
            || saved
                .receipt
                .as_ref()
                .is_some_and(|receipt| saved.slot.is_none() || !receipt.valid_for(request.size))
            || saved.slot.as_ref().is_some_and(|slot| {
                slot.document_id.is_empty()
                    || slot.document_id.len() > 256
                    || checked_content_url(&slot.url).is_err()
            })
        {
            return Err(UploadError::CheckpointInvalid);
        }
        Ok(saved)
    }

    async fn active_session(state: &mut SessionState) -> UploadResult<&mut ICloudReadSession> {
        if let SessionState::Vault {
            apple_id,
            credential_id,
            vault,
        } = state
        {
            let saved = vault
                .load(credential_id)
                .await
                .map_err(|_| UploadError::Uncertain)?
                .ok_or(UploadError::Uncertain)?;
            let restored = ICloudReadSession::from_session_snapshot(&saved, apple_id)
                .map_err(|_| UploadError::Uncertain)?;
            if restored.account_hash.is_none() {
                return Err(UploadError::Invalid);
            }
            *state = SessionState::Ready(Box::new(restored));
        }
        match state {
            SessionState::Ready(session) => Ok(session),
            SessionState::Vault { .. } => Err(UploadError::Uncertain),
        }
    }

    async fn verify_parent(&self, session: &mut ICloudReadSession) -> UploadResult<()> {
        if self.parent.id == ROOT_ID {
            return Ok(());
        }
        let grandparent = self
            .parent
            .parent_id
            .as_deref()
            .ok_or(UploadError::Invalid)?;
        let entries = session
            .list_folder(grandparent)
            .await
            .map_err(|_| UploadError::Uncertain)?;
        if entries
            .iter()
            .filter(|entry| {
                entry.drivewsid == self.parent.id
                    && entry.display_name() == self.parent.name
                    && entry.is_folder()
                    && entry.parent_id == grandparent
            })
            .count()
            != 1
        {
            return Err(UploadError::Conflict);
        }
        Ok(())
    }

    fn node(
        &self,
        entry: &DriveEntry,
        request: &UploadRequest,
        document_id: &str,
    ) -> UploadResult<Node> {
        let UploadIntent::Create { name, .. } = &request.intent else {
            return Err(UploadError::Invalid);
        };
        if entry.is_folder()
            || !entry.drivewsid.starts_with("FILE::com.apple.CloudDocs::")
            || entry.docwsid != document_id
            || entry.drivewsid.rsplit("::").next() != Some(document_id)
            || entry.display_name() != *name
            || entry.size != request.size
            || entry.etag.is_empty()
            || entry.parent_id != self.parent.id
        {
            return Err(UploadError::Conflict);
        }
        Ok(Node {
            id: entry.drivewsid.clone(),
            parent_id: Some(self.parent.id.clone()),
            name: name.clone(),
            kind: NodeKind::File,
            size: entry.size,
            modified_unix: 0,
            etag: Some(entry.etag.clone()),
            content_version: Some(entry.etag.clone()),
            target: None,
            package: false,
        })
    }

    async fn observed(
        &self,
        request: &UploadRequest,
        document_id: &str,
    ) -> UploadResult<Option<Node>> {
        let UploadIntent::Create { name, .. } = &request.intent else {
            return Err(UploadError::Invalid);
        };
        let mut state = self.session.lock().await;
        let session = Self::active_session(&mut state).await?;
        self.verify_parent(session).await?;
        let entries = session
            .list_folder(&self.parent.id)
            .await
            .map_err(|_| UploadError::Uncertain)?;
        let mut matches = entries.iter().filter(|entry| entry.docwsid == document_id);
        let candidate = matches.next();
        if matches.next().is_some()
            || entries
                .iter()
                .any(|entry| entry.display_name() == *name && entry.docwsid != document_id)
        {
            return Err(UploadError::Conflict);
        }
        let Some(entry) = candidate else {
            return Ok(None);
        };
        let node = self.node(entry, request, document_id)?;
        if session
            .hash_file_in_folder_for_revision(
                &self.parent.id,
                &node.id,
                node.etag.as_deref().ok_or(UploadError::Conflict)?,
                request.size,
            )
            .await
            .map_err(|_| UploadError::Uncertain)?
            != request.sha256
        {
            return Err(UploadError::Conflict);
        }
        Ok(Some(node))
    }
}

#[async_trait]
impl UploadProvider for ICloudFileCreate {
    fn begin_is_mutation_free_until_checkpoint(&self, request: &UploadRequest) -> bool {
        self.check_request(request).is_ok()
    }

    async fn begin_upload(
        &self,
        request: &UploadRequest,
        cancel: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        if cancel.is_cancelled() {
            return Err(UploadError::Uncertain);
        }
        Ok(UploadStep::Prepared(self.checkpoint(request, None, None)?))
    }

    async fn inspect_upload(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
        _: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        let saved = self.check_checkpoint(request, checkpoint)?;
        let Some(slot) = saved.slot else {
            #[cfg(feature = "write-probe")]
            if self.reconciliation_only {
                return Err(UploadError::Uncertain);
            }
            let UploadIntent::Create { name, .. } = &request.intent else {
                return Err(UploadError::Invalid);
            };
            let mut state = self.session.lock().await;
            let session = Self::active_session(&mut state).await?;
            self.verify_parent(session).await?;
            let entries = session
                .list_folder(&self.parent.id)
                .await
                .map_err(|_| UploadError::Uncertain)?;
            if entries.iter().any(|entry| entry.display_name() == *name) {
                return Err(UploadError::Conflict);
            }
            let slot = session
                .allocate_upload_slot(name, request.size)
                .await
                .map_err(|_| UploadError::Uncertain)?;
            return Ok(UploadStep::Stream(self.checkpoint(
                request,
                Some(slot),
                None,
            )?));
        };
        self.observed(request, &slot.document_id)
            .await?
            .map(UploadStep::Complete)
            .ok_or(UploadError::Uncertain)
    }

    async fn upload_part(
        &self,
        _: &UploadRequest,
        _: &SecretString,
        _: u64,
        _: Vec<u8>,
        _: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        Err(UploadError::Unsupported("iCloud multipart upload"))
    }

    async fn upload_stream(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
        file: File,
        cancel: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        let saved = self.check_checkpoint(request, checkpoint)?;
        let slot = saved.slot.ok_or(UploadError::CheckpointInvalid)?;
        if saved.receipt.is_some() {
            return Err(UploadError::CheckpointInvalid);
        }
        #[cfg(feature = "write-probe")]
        if self.reconciliation_only {
            return Err(UploadError::Uncertain);
        }
        let expected_size = request.size;
        let expected_hash = request.sha256.clone();
        let hash_cancel = cancel.clone();
        let file = tokio::task::spawn_blocking(move || {
            let mut file = file;
            if file.metadata().map_err(|_| UploadError::Invalid)?.len() != expected_size {
                return Err(UploadError::Invalid);
            }
            verify_upload_payload(&mut file, expected_size, &expected_hash, &hash_cancel)?;
            file.seek(SeekFrom::Start(0))
                .map_err(|_| UploadError::Invalid)?;
            Ok(file)
        })
        .await
        .map_err(|_| UploadError::Invalid)??;
        if cancel.is_cancelled() {
            return Err(UploadError::Uncertain);
        }
        if let Some(node) = self.observed(request, &slot.document_id).await? {
            return Ok(UploadStep::Complete(node));
        }
        let UploadIntent::Create { name, .. } = &request.intent else {
            return Err(UploadError::Invalid);
        };
        let receipt = {
            let mut state = self.session.lock().await;
            let session = Self::active_session(&mut state).await?;
            self.verify_parent(session).await?;
            let entries = session
                .list_folder(&self.parent.id)
                .await
                .map_err(|_| UploadError::Uncertain)?;
            if entries.iter().any(|entry| entry.display_name() == *name) {
                return Err(UploadError::Conflict);
            }
            session
                .upload_stream_to_slot(&slot, name, file, request.size)
                .await
                .map_err(|_| UploadError::Uncertain)?
        };
        Ok(UploadStep::Commit(self.checkpoint(
            request,
            Some(slot),
            Some(receipt),
        )?))
    }

    async fn commit_upload(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
        cancel: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        let saved = self.check_checkpoint(request, checkpoint)?;
        let slot = saved.slot.ok_or(UploadError::CheckpointInvalid)?;
        let receipt = saved.receipt.ok_or(UploadError::CheckpointInvalid)?;
        #[cfg(feature = "write-probe")]
        if self.reconciliation_only {
            return Err(UploadError::Uncertain);
        }
        if cancel.is_cancelled() {
            return Err(UploadError::Uncertain);
        }
        if let Some(node) = self.observed(request, &slot.document_id).await? {
            return Ok(UploadStep::Complete(node));
        }
        let UploadIntent::Create { name, .. } = &request.intent else {
            return Err(UploadError::Invalid);
        };
        {
            let mut state = self.session.lock().await;
            let session = Self::active_session(&mut state).await?;
            self.verify_parent(session).await?;
            let entries = session
                .list_folder(&self.parent.id)
                .await
                .map_err(|_| UploadError::Uncertain)?;
            if entries.iter().any(|entry| entry.display_name() == *name) {
                return Err(UploadError::Conflict);
            }
            #[cfg(feature = "write-probe")]
            if self.discard_registration_response {
                session
                    .register_uploaded_file_discard_response(
                        &self.parent.id,
                        name,
                        &slot,
                        &receipt,
                        request.size,
                    )
                    .await
                    .map_err(|_| UploadError::Uncertain)?;
            } else {
                session
                    .register_uploaded_file(&self.parent.id, name, &slot, &receipt, request.size)
                    .await
                    .map_err(|_| UploadError::Uncertain)?;
            }
            #[cfg(not(feature = "write-probe"))]
            session
                .register_uploaded_file(&self.parent.id, name, &slot, &receipt, request.size)
                .await
                .map_err(|_| UploadError::Uncertain)?;
        }
        self.observed(request, &slot.document_id)
            .await?
            .map(UploadStep::Complete)
            .ok_or(UploadError::Uncertain)
    }

    async fn reconcile_upload(
        &self,
        request: &UploadRequest,
        checkpoint: Option<&SecretString>,
        _: &CancellationToken,
    ) -> UploadResult<Reconciliation> {
        let saved =
            self.check_checkpoint(request, checkpoint.ok_or(UploadError::CheckpointInvalid)?)?;
        let Some(slot) = saved.slot else {
            return Ok(Reconciliation::Uncommitted);
        };
        if let Some(node) = self.observed(request, &slot.document_id).await? {
            return Ok(Reconciliation::Committed(node));
        }
        if saved.receipt.is_none() {
            return Ok(Reconciliation::Uncommitted);
        }
        Err(UploadError::Uncertain)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn cancelled_payload_hash_stops_before_reading_another_block() {
        struct CancellingReader {
            cancel: CancellationToken,
            reads: usize,
        }
        impl Read for CancellingReader {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                self.reads += 1;
                if self.reads == 1 {
                    buf[0] = b'x';
                    self.cancel.cancel();
                    Ok(1)
                } else {
                    Ok(0)
                }
            }
        }
        let cancel = CancellationToken::new();
        let mut reader = CancellingReader {
            cancel: cancel.clone(),
            reads: 0,
        };
        let result =
            verify_upload_payload(&mut reader, 1, &hex::encode(Sha256::digest(b"x")), &cancel);
        assert!(matches!(result, Err(UploadError::Uncertain)));
        assert_eq!(reader.reads, 1);
    }

    #[test]
    fn streamed_payload_hash_requires_exact_size_and_digest() {
        let cancel = CancellationToken::new();
        let digest = hex::encode(Sha256::digest(b"xyz"));
        assert!(verify_upload_payload(&mut b"xyz".as_slice(), 3, &digest, &cancel).is_ok());
        for (mut bytes, size, hash) in [
            (b"xyz".as_slice(), 2, digest.clone()),
            (b"xyz", 4, digest),
            (b"xyz", 3, "0".repeat(64)),
        ] {
            assert!(matches!(
                verify_upload_payload(&mut bytes, size, &hash, &cancel),
                Err(UploadError::Invalid)
            ));
        }
    }

    #[tokio::test]
    async fn create_preflight_is_mutation_free_but_lost_checkpoint_remains_uncertain() {
        let state = tempfile::tempdir().unwrap();
        let scope = Scope {
            account: Uuid::new_v4().to_string(),
            provider: "icloud".into(),
            collection: "drive".into(),
        };
        let parent = Node {
            id: "FOLDER::com.apple.CloudDocs::owned-parent".into(),
            parent_id: Some(ROOT_ID.into()),
            name: "Owned".into(),
            kind: NodeKind::Folder,
            size: 0,
            modified_unix: 0,
            etag: None,
            content_version: None,
            target: None,
            package: false,
        };
        let provider = ICloudFileCreate::from_sealed_session(
            scope.clone(),
            "test@example.invalid".into(),
            Uuid::new_v4().to_string(),
            state.path(),
            parent.clone(),
        )
        .unwrap();
        let request = UploadRequest {
            scope,
            intent: UploadIntent::Create {
                parent: parent.id,
                name: "New.txt".into(),
            },
            size: 1,
            sha256: hex::encode(Sha256::digest(b"x")),
        };
        assert!(matches!(
            provider
                .begin_upload(&request, &CancellationToken::new())
                .await,
            Ok(UploadStep::Prepared(_))
        ));
        assert!(provider.begin_is_mutation_free_until_checkpoint(&request));
        for size in [65 * 1024 * 1024, i64::MAX as u64] {
            let mut large = request.clone();
            large.size = size;
            assert!(
                matches!(
                    provider
                        .begin_upload(&large, &CancellationToken::new())
                        .await,
                    Ok(UploadStep::Prepared(_))
                ),
                "large preflight rejected before streaming"
            );
        }
        let mut unrepresentable = request.clone();
        unrepresentable.size = i64::MAX as u64 + 1;
        assert!(
            provider
                .begin_upload(&unrepresentable, &CancellationToken::new())
                .await
                .is_err()
        );

        let mut empty = request.clone();
        empty.size = 0;
        empty.sha256 = hex::encode(Sha256::digest(b""));
        assert!(matches!(
            provider
                .begin_upload(&empty, &CancellationToken::new())
                .await,
            Ok(UploadStep::Prepared(_))
        ));
        assert!(matches!(
            provider
                .reconcile_upload(&request, None, &CancellationToken::new())
                .await,
            Err(UploadError::CheckpointInvalid)
        ));
    }
}
