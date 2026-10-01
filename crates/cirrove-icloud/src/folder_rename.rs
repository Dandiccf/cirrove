//! Exact-ID, ETag-bound iCloud folder rename for the shared mutation journal.
//! The normal account writer remains disabled until all operations are validated.
use crate::{ICloudReadSession, ROOT_ID, SealedSessionVault};
use async_trait::async_trait;
use cirrove_auth::CredentialVault;
use cirrove_core::mutation::{
    MutationError, MutationIntent, MutationProvider, MutationReceipt, MutationReconciliation,
    MutationRequest, Result as MutationResult,
};
use cirrove_core::{CancellationToken, Node, NodeKind, Scope};
use secrecy::SecretString;
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
    AtSource,
    AtDestination(Node),
    Conflict,
    Unknown,
}

pub struct ICloudFolderRename {
    scope: Scope,
    before: Node,
    target_name: String,
    session: Mutex<SessionState>,
    #[cfg(feature = "write-probe")]
    reconciliation_only: bool,
}

impl ICloudFolderRename {
    pub fn from_session_snapshot(
        scope: Scope,
        apple_id: &str,
        snapshot: &SecretString,
        before: Node,
        target_name: String,
    ) -> MutationResult<Self> {
        Self::check_identity(&scope, &before, &target_name)?;
        let session = ICloudReadSession::from_session_snapshot(snapshot, apple_id)
            .map_err(crate::mutation_error)?;
        if session.account_hash.is_none() {
            return Err(MutationError::Invalid);
        }
        Ok(Self {
            scope,
            before,
            target_name,
            session: Mutex::new(SessionState::Ready(Box::new(session))),
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
        target_name: String,
    ) -> MutationResult<Self> {
        Self::check_identity(&scope, &before, &target_name)?;
        if apple_id.trim().is_empty() || Uuid::parse_str(&credential_id).is_err() {
            return Err(MutationError::Invalid);
        }
        let vault =
            SealedSessionVault::new(state, &scope.account).map_err(|_| MutationError::Invalid)?;
        Ok(Self {
            scope,
            before,
            target_name,
            session: Mutex::new(SessionState::Vault {
                apple_id,
                credential_id,
                vault: Arc::new(vault),
            }),
            #[cfg(feature = "write-probe")]
            reconciliation_only: false,
        })
    }

    #[cfg(feature = "write-probe")]
    pub fn reconciliation_only(mut self) -> Self {
        self.reconciliation_only = true;
        self
    }

    fn valid_name(name: &str) -> bool {
        !name.is_empty()
            && name.len() <= 255
            && !matches!(name, "." | "..")
            && !name.contains(['/', '\0', '\r', '\n'])
    }

    fn check_identity(scope: &Scope, before: &Node, target_name: &str) -> MutationResult<()> {
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
            || !Self::valid_name(&before.name)
            || !Self::valid_name(target_name)
            || before.name == target_name
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
            || !matches!(&request.intent, MutationIntent::Relocate { before, parent, name }
                if before == &self.before
                    && Some(parent.as_str()) == self.before.parent_id.as_deref()
                    && name == &self.target_name)
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
        let Some(entry) = matches.next() else {
            return Ok(Observation::Unknown);
        };
        if matches.next().is_some()
            || !entry.is_folder()
            || entry.parent_id != parent
            || children.iter().any(|other| {
                other.drivewsid != self.before.id && other.display_name() == self.target_name
            })
        {
            return Ok(Observation::Conflict);
        }
        if entry.display_name() == self.before.name {
            if Some(entry.etag.as_str()) != self.before.etag.as_deref() {
                return Ok(Observation::Conflict);
            }
            return Ok(Observation::AtSource);
        }
        if entry.display_name() == self.target_name && !entry.etag.is_empty() {
            return Ok(Observation::AtDestination(Node {
                id: entry.drivewsid.clone(),
                parent_id: Some(parent.into()),
                name: self.target_name.clone(),
                kind: NodeKind::Folder,
                size: 0,
                modified_unix: 0,
                etag: Some(entry.etag.clone()),
                content_version: None,
                target: None,
                package: false,
            }));
        }
        Ok(Observation::Conflict)
    }
}

#[async_trait]
impl MutationProvider for ICloudFolderRename {
    async fn prepare_mutation(
        &self,
        request: &MutationRequest,
        cancel: &CancellationToken,
    ) -> MutationResult<Option<String>> {
        self.check_request(request, None)?;
        #[cfg(feature = "write-probe")]
        if self.reconciliation_only {
            return Err(MutationError::Unsupported("reconciliation-only validation"));
        }
        if cancel.is_cancelled() {
            return Err(MutationError::Uncertain);
        }
        match self.observe().await? {
            Observation::AtSource => Ok(Some(self.before.id.clone())),
            Observation::AtDestination(_) | Observation::Conflict => Err(MutationError::Conflict),
            Observation::Unknown => Err(MutationError::Uncertain),
        }
    }

    async fn mutate(
        &self,
        _: &MutationRequest,
        _: &CancellationToken,
    ) -> MutationResult<MutationReceipt> {
        Err(MutationError::Unsupported(
            "rename requires prepared identity",
        ))
    }

    async fn mutate_prepared(
        &self,
        request: &MutationRequest,
        prepared_item: Option<&str>,
        cancel: &CancellationToken,
    ) -> MutationResult<MutationReceipt> {
        self.check_request(request, prepared_item)?;
        #[cfg(feature = "write-probe")]
        if self.reconciliation_only {
            return Err(MutationError::Unsupported("reconciliation-only validation"));
        }
        if prepared_item != Some(self.before.id.as_str()) {
            return Err(MutationError::Invalid);
        }
        if cancel.is_cancelled() {
            return Err(MutationError::Uncertain);
        }
        if !matches!(self.observe().await?, Observation::AtSource) {
            return Err(MutationError::Conflict);
        }
        let accepted = {
            let mut state = self.session.lock().await;
            let session = Self::active_session(&mut state).await?;
            session
                .send_rename(
                    &self.before.id,
                    self.before.etag.as_deref().ok_or(MutationError::Invalid)?,
                    &self.target_name,
                )
                .await
                .map_err(crate::mutation_error)?
        };
        if !accepted {
            return Err(MutationError::Uncertain);
        }
        match self.observe().await? {
            Observation::AtDestination(node) => Ok(MutationReceipt::Upsert(node)),
            Observation::Conflict => Err(MutationError::Conflict),
            Observation::AtSource | Observation::Unknown => Err(MutationError::Uncertain),
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
        match self.observe().await? {
            Observation::AtDestination(node) => Ok(MutationReconciliation::Applied(
                MutationReceipt::Upsert(node),
            )),
            Observation::Conflict => Ok(MutationReconciliation::Conflict),
            Observation::AtSource | Observation::Unknown => {
                Ok(MutationReconciliation::Indeterminate)
            }
        }
    }
}
