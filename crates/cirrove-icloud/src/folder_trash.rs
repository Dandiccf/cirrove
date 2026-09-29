//! Exact-ID, recoverable iCloud removal of an empty folder.
//! Only an independently observed Trash identity confirms deletion.
use crate::{ICloudReadSession, ROOT_ID, SealedSessionVault};
use async_trait::async_trait;
use cirrove_auth::CredentialVault;
use cirrove_core::mutation::{
    DeletionSupport, MutationError, MutationIntent, MutationProvider, MutationReceipt,
    MutationReconciliation, MutationRequest, Result as MutationResult,
};
use cirrove_core::{CancellationToken, Node, NodeKind, Scope};
use secrecy::SecretString;
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

enum Observation {
    AtParentEmpty,
    InTrash,
    Conflict,
    Unknown,
}

pub struct ICloudFolderTrash {
    scope: Scope,
    before: Node,
    session: Mutex<SessionState>,
    #[cfg(feature = "write-probe")]
    discard_response: AtomicBool,
}

impl ICloudFolderTrash {
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
            session: Mutex::new(SessionState::Vault {
                apple_id,
                credential_id,
                vault: Arc::new(vault),
            }),
            #[cfg(feature = "write-probe")]
            discard_response: AtomicBool::new(false),
        })
    }

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
            || before.kind != NodeKind::Folder
            || before.id == ROOT_ID
            || !before.id.starts_with("FOLDER::com.apple.CloudDocs::")
            || before.id.rsplit("::").next().is_none_or(str::is_empty)
            || !before.parent_id.as_deref().is_some_and(|parent| {
                parent.starts_with("FOLDER::com.apple.CloudDocs::") && parent != before.id
            })
            || before.name.is_empty()
            || before.name.len() > 255
            || matches!(before.name.as_str(), "." | "..")
            || before.name.contains(['/', '\0', '\r', '\n'])
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
            || !matches!(&request.intent, MutationIntent::RemoveFolder { before } if before == &self.before)
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
        let siblings = session
            .list_folder(parent)
            .await
            .map_err(|_| MutationError::Uncertain)?;
        let mut matches = siblings
            .iter()
            .filter(|entry| entry.drivewsid == self.before.id);
        if let Some(entry) = matches.next() {
            if matches.next().is_some()
                || !entry.is_folder()
                || entry.kind != "FOLDER"
                || entry.parent_id != parent
                || entry.display_name() != self.before.name
                || entry.etag != self.before.etag.as_deref().unwrap_or_default()
                || siblings.iter().any(|other| {
                    other.drivewsid != self.before.id && other.display_name() == self.before.name
                })
            {
                return Ok(Observation::Conflict);
            }
            let children = session
                .list_folder(&self.before.id)
                .await
                .map_err(|_| MutationError::Uncertain)?;
            return Ok(if children.is_empty() {
                Observation::AtParentEmpty
            } else {
                Observation::Conflict
            });
        }
        if siblings
            .iter()
            .any(|entry| entry.display_name() == self.before.name)
        {
            return Ok(Observation::Conflict);
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
        let Some(item) = matches.next() else {
            return Ok(Observation::Unknown);
        };
        if matches.next().is_some()
            || item
                .get("type")
                .and_then(|value| value.as_str())
                .is_some_and(|kind| kind != "FOLDER")
            || item.get("restorePath").is_none_or(|path| path.is_null())
        {
            return Ok(Observation::Conflict);
        }
        Ok(Observation::InTrash)
    }
}

#[async_trait]
impl MutationProvider for ICloudFolderTrash {
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
        if cancel.is_cancelled() {
            return Err(MutationError::Uncertain);
        }
        match self.observe().await? {
            Observation::AtParentEmpty => Ok(Some(self.before.id.clone())),
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
            "folder removal requires prepared identity",
        ))
    }

    async fn mutate_prepared(
        &self,
        request: &MutationRequest,
        prepared_item: Option<&str>,
        cancel: &CancellationToken,
    ) -> MutationResult<MutationReceipt> {
        self.check_request(request, prepared_item)?;
        if prepared_item != Some(self.before.id.as_str()) {
            return Err(MutationError::Invalid);
        }
        if cancel.is_cancelled() {
            return Err(MutationError::Uncertain);
        }
        if !matches!(self.observe().await?, Observation::AtParentEmpty) {
            return Err(MutationError::Conflict);
        }
        let accepted = {
            let mut state = self.session.lock().await;
            let session = Self::active_session(&mut state).await?;
            let etag = self.before.etag.as_deref().ok_or(MutationError::Invalid)?;
            #[cfg(feature = "write-probe")]
            if self.discard_response.swap(false, Ordering::AcqRel) {
                session
                    .send_trash_without_receipt(&self.before.id, etag)
                    .await
                    .map_err(|_| MutationError::Uncertain)?;
                return Err(MutationError::Uncertain);
            }
            session
                .send_trash(&self.before.id, etag)
                .await
                .map_err(|_| MutationError::Uncertain)?
        };
        if !accepted {
            return Err(MutationError::Uncertain);
        }
        if matches!(self.observe().await?, Observation::InTrash) {
            Ok(MutationReceipt::Removed {
                item: self.before.id.clone(),
            })
        } else {
            Err(MutationError::Uncertain)
        }
    }

    async fn reconcile_mutation(
        &self,
        _: &MutationRequest,
        _: &CancellationToken,
    ) -> MutationResult<MutationReconciliation> {
        Ok(MutationReconciliation::Indeterminate)
    }

    async fn reconcile_prepared_mutation(
        &self,
        request: &MutationRequest,
        prepared_item: Option<&str>,
        _: &CancellationToken,
    ) -> MutationResult<MutationReconciliation> {
        self.check_request(request, prepared_item)?;
        if prepared_item != Some(self.before.id.as_str()) {
            return Err(MutationError::Invalid);
        }
        match self.observe().await {
            Ok(Observation::InTrash) => {
                Ok(MutationReconciliation::Applied(MutationReceipt::Removed {
                    item: self.before.id.clone(),
                }))
            }
            Ok(Observation::AtParentEmpty | Observation::Unknown)
            | Err(MutationError::Uncertain) => Ok(MutationReconciliation::Indeterminate),
            Ok(Observation::Conflict) | Err(MutationError::Conflict) => {
                Ok(MutationReconciliation::Conflict)
            }
            Err(error) => Err(error),
        }
    }
}
