//! Exact-ID iCloud folder relocation through the shared mutation journal.
//! Normal account writes remain disabled while the folder ancestry protocol
//! and wider provider behavior are validated.
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

/// A folder move is bound to its original account, ID, parent, name and ETag.
/// Its descendants retain provider IDs; the FUSE ancestry route still needs
/// separate validation before ordinary mounts can construct this adapter.
pub struct ICloudFolderMove {
    scope: Scope,
    before: Node,
    destination: Node,
    session: Mutex<SessionState>,
    #[cfg(feature = "write-probe")]
    reconciliation_only: bool,
}

impl ICloudFolderMove {
    pub fn from_session_snapshot(
        scope: Scope,
        apple_id: &str,
        snapshot: &SecretString,
        before: Node,
        destination: Node,
    ) -> MutationResult<Self> {
        Self::check_identity(&scope, &before, &destination)?;
        let session = ICloudReadSession::from_session_snapshot(snapshot, apple_id)
            .map_err(crate::mutation_error)?;
        if session.account_hash.is_none() {
            return Err(MutationError::Invalid);
        }
        Ok(Self {
            scope,
            before,
            destination,
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
        destination: Node,
    ) -> MutationResult<Self> {
        Self::check_identity(&scope, &before, &destination)?;
        if apple_id.trim().is_empty() || Uuid::parse_str(&credential_id).is_err() {
            return Err(MutationError::Invalid);
        }
        let vault =
            SealedSessionVault::new(state, &scope.account).map_err(|_| MutationError::Invalid)?;
        Ok(Self {
            scope,
            before,
            destination,
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

    fn check_identity(scope: &Scope, before: &Node, destination: &Node) -> MutationResult<()> {
        let source = before.parent_id.as_deref().ok_or(MutationError::Invalid)?;
        let etag = before.etag.as_deref().ok_or(MutationError::Invalid)?;
        if scope.account.is_empty()
            || scope.provider != "icloud"
            || scope.collection != "drive"
            || before.kind != NodeKind::Folder
            || !before.id.starts_with("FOLDER::com.apple.CloudDocs::")
            || before.id == ROOT_ID
            || before.id.rsplit("::").next().is_none_or(str::is_empty)
            || !source.starts_with("FOLDER::com.apple.CloudDocs::")
            || before.id == source
            || before.name.is_empty()
            || before.name.len() > 255
            || matches!(before.name.as_str(), "." | "..")
            || before.name.contains(['/', '\0', '\r', '\n'])
            || before.size != 0
            || etag.is_empty()
            || etag.len() > 4096
            || etag.contains(['\r', '\n', '*'])
            || before.target.is_some()
            || before.package
            || destination.kind != NodeKind::Folder
            || !destination.id.starts_with("FOLDER::com.apple.CloudDocs::")
            || destination.id.rsplit("::").next().is_none_or(str::is_empty)
            || destination.id == source
            || destination.id == before.id
            || (destination.id == ROOT_ID && destination.parent_id.is_some())
            || (destination.id != ROOT_ID
                && !destination.parent_id.as_deref().is_some_and(|id| {
                    id.starts_with("FOLDER::com.apple.CloudDocs::")
                        && id != destination.id
                        && id != before.id
                }))
            || destination.name.is_empty()
            || destination.target.is_some()
            || destination.package
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
                if before == &self.before && parent == &self.destination.id && name == &self.before.name)
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
        let source_id = self
            .before
            .parent_id
            .as_deref()
            .ok_or(MutationError::Invalid)?;
        if self.destination.id != ROOT_ID {
            let parent = self
                .destination
                .parent_id
                .as_deref()
                .ok_or(MutationError::Invalid)?;
            let entries = session
                .list_folder(parent)
                .await
                .map_err(crate::mutation_error)?;
            let matches: Vec<_> = entries
                .iter()
                .filter(|entry| entry.drivewsid == self.destination.id)
                .collect();
            if matches.len() != 1
                || matches[0].kind != "FOLDER"
                || matches[0].parent_id != parent
                || matches[0].display_name() != self.destination.name
            {
                return Ok(Observation::Conflict);
            }
        }
        let source = session
            .list_folder(source_id)
            .await
            .map_err(crate::mutation_error)?;
        let target = session
            .list_folder(&self.destination.id)
            .await
            .map_err(crate::mutation_error)?;
        let at_source: Vec<_> = source
            .iter()
            .filter(|entry| entry.drivewsid == self.before.id)
            .collect();
        let at_target: Vec<_> = target
            .iter()
            .filter(|entry| entry.drivewsid == self.before.id)
            .collect();
        if at_source.len() > 1
            || at_target.len() > 1
            || (at_source.len() == 1 && at_target.len() == 1)
            || target.iter().any(|entry| {
                entry.drivewsid != self.before.id && entry.display_name() == self.before.name
            })
        {
            return Ok(Observation::Conflict);
        }
        let (entry, parent, at_source) = match (at_source.as_slice(), at_target.as_slice()) {
            ([entry], []) => (entry, source_id, true),
            ([], [entry]) => (entry, self.destination.id.as_str(), false),
            _ => return Ok(Observation::Unknown),
        };
        let etag = entry.etag.clone();
        if entry.kind != "FOLDER"
            || entry.parent_id != parent
            || entry.display_name() != self.before.name
            || etag.is_empty()
            || (at_source && self.before.etag.as_deref() != Some(etag.as_str()))
        {
            return Ok(Observation::Conflict);
        }
        let source_again = session
            .list_folder(source_id)
            .await
            .map_err(crate::mutation_error)?;
        let target_again = session
            .list_folder(&self.destination.id)
            .await
            .map_err(crate::mutation_error)?;
        let here = if at_source {
            &source_again
        } else {
            &target_again
        };
        let elsewhere = if at_source {
            &target_again
        } else {
            &source_again
        };
        let matches: Vec<_> = here
            .iter()
            .filter(|candidate| candidate.drivewsid == self.before.id)
            .collect();
        if matches.len() != 1
            || matches[0].kind != "FOLDER"
            || matches[0].etag != etag
            || matches[0].parent_id != parent
            || matches[0].display_name() != self.before.name
            || elsewhere
                .iter()
                .any(|candidate| candidate.drivewsid == self.before.id)
            || target_again.iter().any(|candidate| {
                candidate.drivewsid != self.before.id
                    && candidate.display_name() == self.before.name
            })
        {
            return Ok(Observation::Unknown);
        }
        if at_source {
            return Ok(Observation::AtSource);
        }
        Ok(Observation::AtDestination(Node {
            id: self.before.id.clone(),
            parent_id: Some(self.destination.id.clone()),
            name: self.before.name.clone(),
            kind: NodeKind::Folder,
            size: 0,
            modified_unix: 0,
            etag: Some(etag),
            content_version: self.before.content_version.clone(),
            target: None,
            package: false,
        }))
    }
}

#[async_trait]
impl MutationProvider for ICloudFolderMove {
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
            "iCloud folder move requires a prepared ID",
        ))
    }

    async fn mutate_prepared(
        &self,
        request: &MutationRequest,
        prepared: Option<&str>,
        cancel: &CancellationToken,
    ) -> MutationResult<MutationReceipt> {
        self.check_request(request, prepared)?;
        #[cfg(feature = "write-probe")]
        if self.reconciliation_only {
            return Err(MutationError::Unsupported("reconciliation-only validation"));
        }
        if prepared != Some(self.before.id.as_str()) {
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
            Self::active_session(&mut state)
                .await?
                .send_move(
                    &self.before.id,
                    self.before.etag.as_deref().ok_or(MutationError::Invalid)?,
                    &self.destination.id,
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
        request: &MutationRequest,
        cancel: &CancellationToken,
    ) -> MutationResult<MutationReconciliation> {
        self.check_request(request, None)?;
        if cancel.is_cancelled() {
            return Err(MutationError::Uncertain);
        }
        match self.observe().await? {
            Observation::AtSource => Ok(MutationReconciliation::Uncommitted),
            Observation::AtDestination(_) | Observation::Conflict => {
                Ok(MutationReconciliation::Conflict)
            }
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

#[cfg(test)]
pub(crate) mod container_tests;
