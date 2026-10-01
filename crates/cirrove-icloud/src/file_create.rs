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

// Preserve typed session rejection without exposing any provider response or URL.
// Other failures stay uncertain: authentication is not proof of non-commitment.
pub(crate) fn map_session_error(error: anyhow::Error) -> UploadError {
    if error.downcast_ref::<crate::SessionRejected>().is_some() {
        cirrove_core::ProviderError::Authentication.into()
    } else {
        UploadError::Uncertain
    }
}

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
            .map_err(map_session_error)?;
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
                .map_err(map_session_error)?
                .ok_or(UploadError::Uncertain)?;
            let restored = ICloudReadSession::from_session_snapshot(&saved, apple_id)
                .map_err(map_session_error)?;
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
        let folder = session
            .folder_metadata(&self.parent.id)
            .await
            .map_err(map_session_error)?;
        if folder.drivewsid != self.parent.id
            || folder.display_name() != self.parent.name
            || folder.kind != "FOLDER"
            || folder.parent_id != grandparent
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
            .map_err(map_session_error)?;
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
            .map_err(map_session_error)?
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
                .map_err(map_session_error)?;
            if entries.iter().any(|entry| entry.display_name() == *name) {
                return Err(UploadError::Conflict);
            }
            let slot = session
                .allocate_upload_slot(name, request.size)
                .await
                .map_err(map_session_error)?;
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
                .map_err(map_session_error)?;
            if entries.iter().any(|entry| entry.display_name() == *name) {
                return Err(UploadError::Conflict);
            }
            session
                .upload_stream_to_slot(&slot, name, file, request.size)
                .await
                // A signed content URL can expire independently of the account
                // session. Retain uncertainty rather than requesting sign-in.
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
                .map_err(map_session_error)?;
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
                    .map_err(map_session_error)?;
            } else {
                session
                    .register_uploaded_file(&self.parent.id, name, &slot, &receipt, request.size)
                    .await
                    .map_err(map_session_error)?;
            }
            #[cfg(not(feature = "write-probe"))]
            session
                .register_uploaded_file(&self.parent.id, name, &slot, &receipt, request.size)
                .await
                .map_err(map_session_error)?;
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
    fn session_error_mapping_uses_typed_causes_and_never_provider_text() {
        let rejected = anyhow::Error::new(crate::SessionRejected).context("PRIVATE RESPONSE");
        assert!(matches!(
            map_session_error(rejected),
            UploadError::Provider(cirrove_core::ProviderError::Authentication)
        ));
        for error in [
            anyhow::anyhow!("401 PRIVATE SIGNED URL"),
            anyhow::anyhow!("Apple rejected the saved iCloud session; sign in again"),
            anyhow::Error::new(crate::IncompleteFolder),
        ] {
            let mapped = map_session_error(error);
            assert!(matches!(mapped, UploadError::Uncertain));
            assert!(!mapped.to_string().contains("PRIVATE"));
        }
    }

    #[tokio::test]
    async fn parent_validation_queries_the_exact_folder_and_rejects_changed_identity() {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        for change in [
            "none",
            "id",
            "parent",
            "name",
            "kind",
            "app_container",
            "app_library",
            "incomplete",
            "duplicate",
            "unauthorized",
            "forbidden",
            "server_error",
        ] {
            let parent = Node {
                id: "FOLDER::com.apple.CloudDocs::owned".into(),
                parent_id: Some(ROOT_ID.into()),
                name: "Owned".into(),
                kind: NodeKind::Folder,
                size: 0,
                etag: Some("etag".into()),
                content_version: None,
                modified_unix: 0,
                target: None,
                package: false,
            };
            let mut item = serde_json::json!({"drivewsid":parent.id,"parentId":ROOT_ID,"name":"Owned","type":"FOLDER","numberOfItems":0,"items":[]});
            match change {
                "id" => item["drivewsid"] = "foreign".into(),
                "parent" => item["parentId"] = "foreign".into(),
                "name" => item["name"] = "Changed".into(),
                "kind" => item["type"] = "FILE".into(),
                "app_container" => item["type"] = "APP_CONTAINER".into(),
                "app_library" => item["type"] = "APP_LIBRARY".into(),
                "incomplete" => item["numberOfItems"] = 1.into(),
                _ => (),
            }
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}/", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let payload = loop {
                    let mut buf = [0u8; 4096];
                    let n = stream.read(&mut buf).await.unwrap();
                    assert!(n > 0 && bytes.len() + n < 16384);
                    bytes.extend_from_slice(&buf[..n]);
                    let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") else {
                        continue;
                    };
                    let end = end + 4;
                    let headers = std::str::from_utf8(&bytes[..end]).unwrap();
                    let size: usize = headers
                        .lines()
                        .find_map(|s| {
                            s.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .map(str::to_owned)
                        })
                        .unwrap()
                        .parse()
                        .unwrap();
                    if bytes.len() < end + size {
                        continue;
                    }
                    break serde_json::from_slice::<serde_json::Value>(&bytes[end..end + size])
                        .unwrap();
                };
                let body = if change == "duplicate" {
                    serde_json::json!([item.clone(), item])
                } else {
                    serde_json::json!([item])
                }
                .to_string();
                let status = match change {
                    "unauthorized" => "401 Unauthorized",
                    "forbidden" => "403 Forbidden",
                    "server_error" => "503 Unavailable",
                    _ => "200 OK",
                };
                let reply = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(reply.as_bytes()).await.unwrap();
                payload
            });
            let provider = ICloudFileCreate {
                scope: Scope {
                    account: Uuid::new_v4().to_string(),
                    provider: "icloud".into(),
                    collection: "drive".into(),
                },
                parent: parent.clone(),
                session: Mutex::new(SessionState::Ready(Box::new(
                    ICloudReadSession::new().unwrap(),
                ))),
                #[cfg(feature = "write-probe")]
                reconciliation_only: false,
                #[cfg(feature = "write-probe")]
                discard_registration_response: false,
            };
            let mut session = ICloudReadSession::new().unwrap();
            session.drive_endpoint = Some(url::Url::parse(&endpoint).unwrap());
            let result = provider.verify_parent(&mut session).await;
            let payload = server.await.unwrap();
            assert_eq!(
                payload[0]["drivewsid"], parent.id,
                "must not enumerate ancestors"
            );
            assert_eq!(payload[0]["partialData"], false);
            assert_eq!(result.is_ok(), change == "none", "{change}");
            if matches!(change, "unauthorized" | "forbidden") {
                assert!(
                    matches!(
                        result,
                        Err(UploadError::Provider(
                            cirrove_core::ProviderError::Authentication
                        ))
                    ),
                    "{change}: {result:?}"
                );
            } else if change == "server_error" {
                assert!(matches!(result, Err(UploadError::Uncertain)));
            }
        }
    }

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
