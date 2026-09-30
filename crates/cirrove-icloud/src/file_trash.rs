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
            .map_err(|_| MutationError::Uncertain)?;
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
                .map_err(|_| MutationError::Uncertain)?
                .ok_or(MutationError::Uncertain)?;
            let restored = ICloudReadSession::from_session_snapshot(&saved, apple_id)
                .map_err(|_| MutationError::Uncertain)?;
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
            .map_err(|_| MutationError::Uncertain)?;
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
                    .map_err(|_| MutationError::Uncertain)?
                    != *expected
            {
                return Ok(Observation::Conflict);
            }
            return Ok(Observation::Present);
        }
        let (trash, complete) = session
            .read_trash_items()
            .await
            .map_err(|_| MutationError::Uncertain)?;
        if !complete {
            return Ok(Observation::Unknown);
        }
        let mut matches = trash.iter().filter(|item| {
            item.get("drivewsid").and_then(|value| value.as_str()) == Some(self.before.id.as_str())
        });
        if let Some(item) = matches.next() {
            if matches.next().is_some()
                || item.get("restorePath").is_none_or(|path| path.is_null())
                || item
                    .get("docwsid")
                    .and_then(|id| id.as_str())
                    .is_some_and(|id| {
                        !id.is_empty() && self.before.id.rsplit("::").next() != Some(id)
                    })
            {
                return Ok(Observation::Conflict);
            }
            if let Some(expected) = &self.expected_sha256 {
                let etag = item.get("etag").and_then(|value| value.as_str());
                if item.get("size").and_then(|value| value.as_u64()) != Some(self.before.size)
                    || etag.is_none_or(str::is_empty)
                {
                    return Ok(Observation::Conflict);
                }
                let etag = etag.ok_or(MutationError::Uncertain)?.to_owned();
                let signed = session
                    .signed_download_url(&self.before.id)
                    .await
                    .map_err(|_| MutationError::Uncertain)?;
                let mut response = session
                    .http
                    .get(signed)
                    .send()
                    .await
                    .map_err(|_| MutationError::Uncertain)?;
                if response.status() != reqwest::StatusCode::OK {
                    return Err(MutationError::Uncertain);
                }
                let mut received = 0u64;
                let mut hash = Sha256::new();
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
                if received != self.before.size || hex::encode(hash.finalize()) != *expected {
                    return Ok(Observation::Conflict);
                }
                let (again, complete) = session
                    .read_trash_items()
                    .await
                    .map_err(|_| MutationError::Uncertain)?;
                if !complete {
                    return Ok(Observation::Unknown);
                }
                let matches: Vec<_> = again
                    .iter()
                    .filter(|candidate| {
                        candidate.get("drivewsid").and_then(|value| value.as_str())
                            == Some(self.before.id.as_str())
                    })
                    .collect();
                if matches.len() != 1
                    || matches[0].get("etag").and_then(|value| value.as_str())
                        != Some(etag.as_str())
                    || matches[0].get("size").and_then(|value| value.as_u64())
                        != Some(self.before.size)
                    || matches[0]
                        .get("restorePath")
                        .is_none_or(|path| path.is_null())
                {
                    return Ok(Observation::Unknown);
                }
            }
            return Ok(Observation::InTrash);
        }
        if children
            .iter()
            .any(|entry| entry.display_name() == self.before.name)
        {
            return Ok(Observation::Conflict);
        }
        Ok(Observation::Unknown)
    }
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
                    .map_err(|_| MutationError::Uncertain)?;
                return Err(MutationError::Uncertain);
            }
            session
                .send_trash(
                    &self.before.id,
                    self.before.etag.as_deref().ok_or(MutationError::Invalid)?,
                )
                .await
                .map_err(|_| MutationError::Uncertain)?
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

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
