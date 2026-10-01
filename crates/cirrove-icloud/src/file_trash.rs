//! Exact-ID, recoverable iCloud file removal for the shared mutation journal.
//! Ordinary iCloud connections remain read-only until the complete writer is
//! validated and explicitly enabled.
use crate::{ICloudReadSession, SealedSessionVault};
use async_trait::async_trait;
use cirrove_auth::CredentialVault;
use cirrove_core::mutation::{
    DeletionSupport, MutationError, MutationIntent, MutationProvider, MutationReceipt,
    MutationReconciliation, MutationRequest, Result as MutationResult,
};
use cirrove_core::{CancellationToken, Node, NodeKind, Scope};
use secrecy::SecretString;
use sha2::{Digest, Sha256};
#[cfg(feature = "write-probe")]
use std::sync::atomic::{AtomicBool, Ordering};
use std::{path::Path, sync::Arc};
use tokio::sync::Mutex;
use uuid::Uuid;

enum SessionState {
    Ready(Box<ICloudReadSession>),
    Vault {
        apple_id: String,
        credential_id: String,
        vault: Arc<dyn CredentialVault>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Observation {
    Present,
    InTrash,
    Conflict,
    Unknown,
}

/// One deletion operation bound to its account, collection, parent, item and
/// original ETag. It never changes a file that the caller has not named.
pub struct ICloudFileTrash {
    scope: Scope,
    before: Node,
    expected_sha256: Option<String>,
    session: Mutex<SessionState>,
    #[cfg(feature = "write-probe")]
    discard_response: AtomicBool,
    #[cfg(feature = "write-probe")]
    reconciliation_only: bool,
}

impl ICloudFileTrash {
    pub fn from_session_snapshot(
        scope: Scope,
        apple_id: &str,
        snapshot: &SecretString,
        before: Node,
    ) -> MutationResult<Self> {
        Self::check_identity(&scope, &before)?;
        let session = ICloudReadSession::from_session_snapshot(snapshot, apple_id)
            .map_err(crate::mutation_error)?;
        if session.account_hash.is_none() {
            return Err(MutationError::Invalid);
        }
        Ok(Self {
            scope,
            before,
            expected_sha256: None,
            session: Mutex::new(SessionState::Ready(Box::new(session))),
            #[cfg(feature = "write-probe")]
            discard_response: AtomicBool::new(false),
            #[cfg(feature = "write-probe")]
            reconciliation_only: false,
        })
    }

    pub fn from_sealed_session(
        scope: Scope,
        apple_id: String,
        credential_id: String,
        state: &Path,
        before: Node,
    ) -> MutationResult<Self> {
        Self::check_identity(&scope, &before)?;
        if apple_id.trim().is_empty() || Uuid::parse_str(&credential_id).is_err() {
            return Err(MutationError::Invalid);
        }
        let vault =
            SealedSessionVault::new(state, &scope.account).map_err(|_| MutationError::Invalid)?;
        Ok(Self {
            scope,
            before,
            expected_sha256: None,
            session: Mutex::new(SessionState::Vault {
                apple_id,
                credential_id,
                vault: Arc::new(vault),
            }),
            #[cfg(feature = "write-probe")]
            discard_response: AtomicBool::new(false),
            #[cfg(feature = "write-probe")]
            reconciliation_only: false,
        })
    }

    /// A caller with a sealed, journal-confirmed payload can require its
    /// complete digest in addition to Apple's conditional revision token.
    pub fn with_expected_sha256(mut self, digest: String) -> MutationResult<Self> {
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            || self.before.size > crate::MAX_WRITE_FILE_SIZE
        {
            return Err(MutationError::Invalid);
        }
        self.expected_sha256 = Some(digest);
        Ok(self)
    }

    /// Isolated validator fault: send once but drop Apple's response before
    /// recording a receipt. The restarted worker must only reconcile.
    #[cfg(feature = "write-probe")]
    pub fn with_discarded_response(self) -> Self {
        self.discard_response.store(true, Ordering::Release);
        self
    }

    /// Recovery validator must fail if a worker attempts another delete.
    #[cfg(feature = "write-probe")]
    pub fn with_reconciliation_only(mut self) -> Self {
        self.reconciliation_only = true;
        self
    }

    fn check_identity(scope: &Scope, before: &Node) -> MutationResult<()> {
        let etag = before.etag.as_deref().ok_or(MutationError::Invalid)?;
        if scope.account.is_empty()
            || scope.provider != "icloud"
            || scope.collection != "drive"
            || before.kind != NodeKind::File
            || !before.id.starts_with("FILE::com.apple.CloudDocs::")
            || before.id.rsplit("::").next().is_none_or(str::is_empty)
            || !before
                .parent_id
                .as_deref()
                .is_some_and(|parent| parent.starts_with("FOLDER::com.apple.CloudDocs::"))
            || before.name.is_empty()
            || before.name.len() > 255
            || matches!(before.name.as_str(), "." | "..")
            || before.name.contains(['/', '\0'])
            || etag.is_empty()
            || etag.len() > 4096
            || etag.contains(['\r', '\n', '*'])
            || before.target.is_some()
            || before.package
        {
            return Err(MutationError::Invalid);
        }
        Ok(())
    }

    fn check_request(
        &self,
        request: &MutationRequest,
        prepared: Option<&str>,
    ) -> MutationResult<()> {
        request.validate()?;
        if request.scope != self.scope
            || !matches!(&request.intent, MutationIntent::RemoveFile { before } if before == &self.before)
            || prepared.is_some_and(|id| id != self.before.id)
        {
            return Err(MutationError::Invalid);
        }
        Ok(())
    }

    async fn active_session(state: &mut SessionState) -> MutationResult<&mut ICloudReadSession> {
        if let SessionState::Vault {
            apple_id,
            credential_id,
            vault,
        } = state
        {
            let saved = vault
                .load(credential_id)
                .await
                .map_err(crate::mutation_error)?
                .ok_or(MutationError::Uncertain)?;
            let restored = ICloudReadSession::from_session_snapshot(&saved, apple_id)
                .map_err(crate::mutation_error)?;
            if restored.account_hash.is_none() {
                return Err(MutationError::Invalid);
            }
            *state = SessionState::Ready(Box::new(restored));
        }
        match state {
            SessionState::Ready(session) => Ok(session),
            SessionState::Vault { .. } => Err(MutationError::Uncertain),
        }
    }

    async fn observe(&self) -> MutationResult<Observation> {
        let mut state = self.session.lock().await;
        let session = Self::active_session(&mut state).await?;
        let parent = self
            .before
            .parent_id
            .as_deref()
            .ok_or(MutationError::Invalid)?;
        let children = session
            .list_folder(parent)
            .await
            .map_err(crate::mutation_error)?;
        let mut matches = children
            .iter()
            .filter(|entry| entry.drivewsid == self.before.id);
        if let Some(entry) = matches.next() {
            let document = self.before.id.rsplit("::").next().unwrap_or_default();
            if matches.next().is_some()
                || entry.is_folder()
                || entry.docwsid != document
                || entry.parent_id != parent
                || entry.display_name() != self.before.name
                || entry.etag != self.before.etag.as_deref().unwrap_or_default()
                || entry.size != self.before.size
            {
                return Ok(Observation::Conflict);
            }
            if let Some(expected) = &self.expected_sha256
                && session
                    .hash_file_in_folder_for_revision(
                        parent,
                        &self.before.id,
                        self.before.etag.as_deref().ok_or(MutationError::Invalid)?,
                        self.before.size,
                    )
                    .await
                    .map_err(crate::mutation_error)?
                    != *expected
            {
                return Ok(Observation::Conflict);
            }
            return Ok(Observation::Present);
        }
        // Presence of this exact recoverable item is enough. A global Trash
        // inventory can be incomplete or slow and is not evidence about this ID.
        let item = session
            .item_details(&self.before.id)
            .await
            .map_err(crate::mutation_error)?;
        if !trash_binding(&item, &self.before) {
            return Ok(
                if children
                    .iter()
                    .any(|entry| entry.display_name() == self.before.name)
                {
                    Observation::Conflict
                } else {
                    Observation::Unknown
                },
            );
        }
        if let Some(expected) = &self.expected_sha256 {
            let etag = item
                .get("etag")
                .and_then(|v| v.as_str())
                .filter(|v| !v.is_empty() && v.len() <= 4096 && !v.contains(['\r', '\n', '*']));
            if item.get("size").and_then(|v| v.as_u64()) != Some(self.before.size) || etag.is_none()
            {
                return Ok(Observation::Conflict);
            }
            let etag = etag.ok_or(MutationError::Uncertain)?;
            // Even an empty file must have an ordinary (not package) representation.
            let signed = session
                .ordinary_download_url(&self.before.id)
                .await
                .map_err(crate::mutation_error)?;
            let mut received = 0u64;
            let mut hash = Sha256::new();
            if self.before.size > 0 {
                let mut response = session
                    .http
                    .get(signed)
                    .send()
                    .await
                    .map_err(|_| MutationError::Uncertain)?;
                verification_download_status(response.status())?;
                while let Some(chunk) = response
                    .chunk()
                    .await
                    .map_err(|_| MutationError::Uncertain)?
                {
                    received = received.saturating_add(chunk.len() as u64);
                    if received > self.before.size {
                        return Ok(Observation::Conflict);
                    }
                    hash.update(&chunk);
                }
            }
            if received != self.before.size || hex::encode(hash.finalize()) != *expected {
                return Ok(Observation::Conflict);
            }
            let again = session
                .item_details(&self.before.id)
                .await
                .map_err(crate::mutation_error)?;
            if !trash_binding(&again, &self.before)
                || again.get("etag").and_then(|v| v.as_str()) != Some(etag)
                || again.get("size").and_then(|v| v.as_u64()) != Some(self.before.size)
                || again.get("restorePath") != item.get("restorePath")
                || again.get("name") != item.get("name")
                || again.get("extension") != item.get("extension")
            {
                return Ok(Observation::Unknown);
            }
        }
        Ok(Observation::InTrash)
    }
}

fn trash_binding(item: &serde_json::Value, before: &Node) -> bool {
    item.get("drivewsid").and_then(|v| v.as_str()) == Some(before.id.as_str())
        && item.get("docwsid").and_then(|v| v.as_str()) == before.id.rsplit("::").next()
        && item.get("type").and_then(|v| v.as_str()) == Some("FILE")
        && matches!(
            item.get("parentId").and_then(|v| v.as_str()),
            Some("TRASH_ROOT" | crate::write_transport::TRASH_ROOT)
        )
        && item.get("restorePath").is_some_and(|v| !v.is_null())
}

#[async_trait]
impl MutationProvider for ICloudFileTrash {
    fn deletion(&self) -> DeletionSupport {
        DeletionSupport {
            recycle_bin: true,
            permanent: false,
        }
    }

    async fn prepare_mutation(
        &self,
        request: &MutationRequest,
        cancel: &CancellationToken,
    ) -> MutationResult<Option<String>> {
        self.check_request(request, None)?;
        if self.expected_sha256.is_none() {
            return Err(MutationError::Invalid);
        }
        if cancel.is_cancelled() {
            return Err(MutationError::Uncertain);
        }
        match self.observe().await? {
            Observation::Present => Ok(Some(self.before.id.clone())),
            Observation::InTrash | Observation::Conflict => Err(MutationError::Conflict),
            Observation::Unknown => Err(MutationError::Uncertain),
        }
    }

    async fn mutate(
        &self,
        _: &MutationRequest,
        _: &CancellationToken,
    ) -> MutationResult<MutationReceipt> {
        Err(MutationError::Unsupported(
            "iCloud Trash requires a prepared file ID",
        ))
    }

    async fn mutate_prepared(
        &self,
        request: &MutationRequest,
        prepared: Option<&str>,
        cancel: &CancellationToken,
    ) -> MutationResult<MutationReceipt> {
        #[cfg(feature = "write-probe")]
        if self.reconciliation_only {
            return Err(MutationError::Unsupported("reconciliation-only validation"));
        }
        self.check_request(request, prepared)?;
        if self.expected_sha256.is_none() {
            return Err(MutationError::Invalid);
        }
        if prepared != Some(self.before.id.as_str()) {
            return Err(MutationError::Invalid);
        }
        if cancel.is_cancelled() {
            return Err(MutationError::Uncertain);
        }
        if self.observe().await? != Observation::Present {
            return Err(MutationError::Conflict);
        }
        let accepted = {
            let mut state = self.session.lock().await;
            let session = Self::active_session(&mut state).await?;
            #[cfg(feature = "write-probe")]
            if self.discard_response.swap(false, Ordering::AcqRel) {
                session
                    .send_trash_without_receipt(
                        &self.before.id,
                        self.before.etag.as_deref().ok_or(MutationError::Invalid)?,
                    )
                    .await
                    .map_err(crate::mutation_error)?;
                return Err(MutationError::Uncertain);
            }
            session
                .send_trash(
                    &self.before.id,
                    self.before.etag.as_deref().ok_or(MutationError::Invalid)?,
                )
                .await
                .map_err(crate::mutation_error)?
        };
        if !accepted {
            return match self.observe().await? {
                Observation::Conflict => Err(MutationError::Conflict),
                _ => Err(MutationError::Uncertain),
            };
        }
        match self.observe().await? {
            Observation::InTrash => Ok(MutationReceipt::Removed {
                item: self.before.id.clone(),
            }),
            _ => Err(MutationError::Uncertain),
        }
    }

    async fn reconcile_mutation(
        &self,
        request: &MutationRequest,
        cancel: &CancellationToken,
    ) -> MutationResult<MutationReconciliation> {
        self.check_request(request, None)?;
        if cancel.is_cancelled() {
            return Err(MutationError::Uncertain);
        }
        // This adapter never sends Trash before the journal stores the exact ID.
        match self.observe().await? {
            Observation::Present => Ok(MutationReconciliation::Uncommitted),
            Observation::Conflict | Observation::InTrash => Ok(MutationReconciliation::Conflict),
            Observation::Unknown => Ok(MutationReconciliation::Indeterminate),
        }
    }

    async fn reconcile_prepared_mutation(
        &self,
        request: &MutationRequest,
        prepared: Option<&str>,
        cancel: &CancellationToken,
    ) -> MutationResult<MutationReconciliation> {
        self.check_request(request, prepared)?;
        if prepared != Some(self.before.id.as_str()) {
            return Err(MutationError::Invalid);
        }
        if cancel.is_cancelled() {
            return Err(MutationError::Uncertain);
        }
        match self.observe().await? {
            Observation::InTrash if self.expected_sha256.is_some() => {
                Ok(MutationReconciliation::Applied(MutationReceipt::Removed {
                    item: self.before.id.clone(),
                }))
            }
            Observation::Conflict => Ok(MutationReconciliation::Conflict),
            Observation::Present | Observation::Unknown | Observation::InTrash => {
                Ok(MutationReconciliation::Indeterminate)
            }
        }
    }
}

fn verification_download_status(status: reqwest::StatusCode) -> MutationResult<()> {
    if status == reqwest::StatusCode::OK {
        Ok(())
    } else {
        Err(crate::mutation_error(crate::content_request_failure(
            status,
            "iCloud Trash verification download",
        )))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[test]
    fn signed_verification_download_distinguishes_storage_from_url_expiry() {
        for status in [200, 507, 401, 403, 503, 509] {
            let result =
                verification_download_status(reqwest::StatusCode::from_u16(status).unwrap());
            match status {
                200 => assert!(result.is_ok()),
                507 => assert!(matches!(result, Err(MutationError::InsufficientStorage))),
                _ => assert!(matches!(result, Err(MutationError::Uncertain))),
            }
        }
    }

    fn before() -> Node {
        Node {
            id: format!("FILE::com.apple.CloudDocs::{}", Uuid::new_v4()),
            parent_id: Some(format!("FOLDER::com.apple.CloudDocs::{}", Uuid::new_v4())),
            name: "Owned.txt".into(),
            kind: NodeKind::File,
            size: 3,
            modified_unix: 0,
            etag: Some("etag".into()),
            content_version: None,
            target: None,
            package: false,
        }
    }

    #[tokio::test]
    async fn lost_empty_file_delete_recovers_by_exact_id_without_a_trash_inventory() {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        for change in [
            "none",
            "doc",
            "type",
            "parent",
            "restore",
            "size",
            "etag_after",
            "restore_after",
            "name_after",
            "foreign_id",
            "package",
            "digest",
        ] {
            let scope = Scope {
                account: Uuid::new_v4().to_string(),
                provider: "icloud".into(),
                collection: "drive".into(),
            };
            let mut original = before();
            original.size = 0;
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}/", listener.local_addr().unwrap());
            let stop = CancellationToken::new();
            let done = stop.clone();
            let source = original.clone();
            let server = tokio::spawn(async move {
                let mut details = 0;
                loop {
                    let accepted = tokio::select! { _=done.cancelled()=>break, r=listener.accept()=>r.unwrap() };
                    let (mut peer, _) = accepted;
                    let mut request = Vec::new();
                    let (header, payload) = loop {
                        let mut buf = [0u8; 4096];
                        let n = peer.read(&mut buf).await.unwrap();
                        assert!(n > 0 && request.len() + n < 16384);
                        request.extend_from_slice(&buf[..n]);
                        let Some(end) = request.windows(4).position(|b| b == b"\r\n\r\n") else {
                            continue;
                        };
                        let end = end + 4;
                        let header = std::str::from_utf8(&request[..end]).unwrap().to_string();
                        let len: usize = header
                            .lines()
                            .find_map(|s| {
                                s.to_ascii_lowercase()
                                    .strip_prefix("content-length: ")
                                    .map(str::to_owned)
                            })
                            .map(|s| s.parse().unwrap())
                            .unwrap_or(0);
                        if request.len() < end + len {
                            continue;
                        }
                        break (header, request[end..end + len].to_vec());
                    };
                    let body = if header.starts_with("POST /retrieveItemDetailsInFolders ") {
                        let p: serde_json::Value = serde_json::from_slice(&payload).unwrap();
                        if p[0]["drivewsid"].as_str() != source.parent_id.as_deref() {
                            serde_json::json!([])
                        } else {
                            serde_json::json!([{"drivewsid":source.parent_id,"type":"FOLDER","numberOfItems":0,"items":[]}])
                        }
                    } else if header.starts_with("POST /retrieveItemDetails ") {
                        let p: serde_json::Value = serde_json::from_slice(&payload).unwrap();
                        assert_eq!(p["items"][0]["drivewsid"], source.id);
                        details += 1;
                        let mut item = serde_json::json!({"drivewsid":source.id,"docwsid":source.id.rsplit("::").next(),"type":"FILE","parentId":"TRASH_ROOT","name":"Owned","extension":"txt","etag":"trashed","size":0,"restorePath":"owned-path"});
                        match change {
                            "doc" => item["docwsid"] = "foreign".into(),
                            "type" => item["type"] = "FOLDER".into(),
                            "parent" => item["parentId"] = "active-folder".into(),
                            "restore" => item["restorePath"] = serde_json::Value::Null,
                            "size" => item["size"] = 1.into(),
                            "foreign_id" => {
                                item["drivewsid"] = "FILE::com.apple.CloudDocs::foreign".into()
                            }
                            "etag_after" if details == 2 => item["etag"] = "changed".into(),
                            "restore_after" if details == 2 => {
                                item["restorePath"] = "changed".into()
                            }
                            "name_after" if details == 2 => item["name"] = "changed".into(),
                            _ => (),
                        }
                        serde_json::json!({"items":[item]})
                    } else {
                        assert!(header.starts_with("GET /ws/com.apple.CloudDocs/download/by_id?"));
                        if change == "package" {
                            serde_json::json!({"package_token":{"url":"https://fixture.icloud-content.com/empty"}})
                        } else {
                            serde_json::json!({"data_token":{"url":"https://fixture.icloud-content.com/empty"}})
                        }
                    };
                    let body = body.to_string();
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    peer.write_all(response.as_bytes()).await.unwrap();
                }
                details
            });
            let mut session = ICloudReadSession::new().unwrap();
            session.drive_endpoint = Some(endpoint.parse().unwrap());
            session.docs_endpoint = Some(endpoint.parse().unwrap());
            let provider = ICloudFileTrash {
                scope: scope.clone(),
                before: original.clone(),
                expected_sha256: Some(if change == "digest" {
                    "0".repeat(64)
                } else {
                    hex::encode(Sha256::digest([]))
                }),
                session: Mutex::new(SessionState::Ready(Box::new(session))),
                #[cfg(feature = "write-probe")]
                discard_response: AtomicBool::new(false),
                #[cfg(feature = "write-probe")]
                reconciliation_only: false,
            };
            let request = MutationRequest {
                scope,
                intent: MutationIntent::RemoveFile {
                    before: original.clone(),
                },
            };
            #[cfg(feature = "write-probe")]
            let provider = {
                let provider = provider.with_reconciliation_only();
                assert!(matches!(
                    provider
                        .mutate_prepared(&request, Some(&original.id), &CancellationToken::new())
                        .await,
                    Err(MutationError::Unsupported("reconciliation-only validation"))
                ));
                provider
            };
            let result = provider
                .reconcile_prepared_mutation(
                    &request,
                    Some(&original.id),
                    &CancellationToken::new(),
                )
                .await;
            stop.cancel();
            let details = server.await.unwrap();
            assert_eq!(
                matches!(result,Ok(MutationReconciliation::Applied(MutationReceipt::Removed{item})) if item==original.id),
                change == "none",
                "{change}"
            );
            if change == "none" {
                assert_eq!(details, 2);
            }
        }
    }

    #[test]
    fn exact_scope_file_and_revision_are_required_before_vault_access() {
        let scope = Scope {
            account: Uuid::new_v4().to_string(),
            provider: "icloud".into(),
            collection: "drive".into(),
        };
        let file = before();
        assert!(ICloudFileTrash::check_identity(&scope, &file).is_ok());
        let mut foreign = file.clone();
        foreign.parent_id = Some("foreign-folder".into());
        assert!(ICloudFileTrash::check_identity(&scope, &foreign).is_err());
        let mut stale = file.clone();
        stale.etag = Some("*".into());
        assert!(ICloudFileTrash::check_identity(&scope, &stale).is_err());
        let mut other_scope = scope;
        other_scope.collection = "another-drive".into();
        assert!(ICloudFileTrash::check_identity(&other_scope, &file).is_err());
    }

    #[tokio::test]
    async fn a_missing_saved_digest_cannot_start_or_send_a_delete() {
        let scope = Scope {
            account: Uuid::new_v4().to_string(),
            provider: "icloud".into(),
            collection: "drive".into(),
        };
        let before = before();
        let request = MutationRequest {
            scope: scope.clone(),
            intent: MutationIntent::RemoveFile {
                before: before.clone(),
            },
        };
        let provider = ICloudFileTrash {
            scope,
            before: before.clone(),
            expected_sha256: None,
            session: Mutex::new(SessionState::Vault {
                apple_id: "owned@example.test".into(),
                credential_id: Uuid::new_v4().to_string(),
                vault: Arc::new(cirrove_auth::DesktopVault),
            }),
            #[cfg(feature = "write-probe")]
            discard_response: AtomicBool::new(false),
            #[cfg(feature = "write-probe")]
            reconciliation_only: false,
        };
        let cancel = CancellationToken::new();
        assert!(matches!(
            provider.prepare_mutation(&request, &cancel).await,
            Err(MutationError::Invalid)
        ));
        assert!(matches!(
            provider
                .mutate_prepared(&request, Some(&before.id), &cancel)
                .await,
            Err(MutationError::Invalid)
        ));
    }
}
