//! Exact-parent iCloud folder creation through the shared mutation journal.
//! A lost create response cannot be retried from a name match alone.
use crate::{ICloudReadSession, ROOT_ID, SealedSessionVault};
use async_trait::async_trait;
use cirrove_auth::CredentialVault;
use cirrove_core::mutation::{
    MutationError, MutationIntent, MutationProvider, MutationReceipt, MutationReconciliation,
    MutationRequest, Result as MutationResult,
};
use cirrove_core::{CancellationToken, Node, NodeKind, Scope};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
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

#[derive(Serialize, Deserialize)]
struct FolderCheckpoint {
    version: u8,
    operation: Uuid,
    scope: Scope,
    parent: String,
    name: String,
    id: String,
}

pub struct ICloudFolderCreate {
    scope: Scope,
    parent: Node,
    checkpoint_vault: Arc<dyn CredentialVault>,
    session: Mutex<SessionState>,
    #[cfg(feature = "write-probe")]
    reconciliation_only: bool,
}

impl ICloudFolderCreate {
    pub fn from_session_snapshot(
        scope: Scope,
        apple_id: &str,
        snapshot: &SecretString,
        parent: Node,
        checkpoint_vault: Arc<dyn CredentialVault>,
    ) -> MutationResult<Self> {
        Self::check_identity(&scope, &parent)?;
        let session = ICloudReadSession::from_session_snapshot(snapshot, apple_id)
            .map_err(crate::mutation_error)?;
        if session.account_hash.is_none() {
            return Err(MutationError::Invalid);
        }
        Ok(Self {
            scope,
            parent,
            checkpoint_vault,
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
        parent: Node,
        checkpoint_vault: Arc<dyn CredentialVault>,
    ) -> MutationResult<Self> {
        Self::check_identity(&scope, &parent)?;
        if apple_id.trim().is_empty() || Uuid::parse_str(&credential_id).is_err() {
            return Err(MutationError::Invalid);
        }
        let vault =
            SealedSessionVault::new(state, &scope.account).map_err(|_| MutationError::Invalid)?;
        Ok(Self {
            scope,
            parent,
            checkpoint_vault,
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

    fn check_identity(scope: &Scope, parent: &Node) -> MutationResult<()> {
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
            return Err(MutationError::Invalid);
        }
        Ok(())
    }

    fn key(&self, operation: Uuid) -> String {
        format!("icloud-folder-create/{}/{operation}", self.scope.account)
    }

    fn check_request<'a>(&self, request: &'a MutationRequest) -> MutationResult<&'a str> {
        request.validate()?;
        let MutationIntent::CreateFolder { parent, name } = &request.intent else {
            return Err(MutationError::Unsupported("iCloud folder creation"));
        };
        if request.scope != self.scope
            || parent != &self.parent.id
            || name.len() > 255
            || name.contains(['\\', '\r', '\n'])
        {
            return Err(MutationError::Invalid);
        }
        Ok(name)
    }

    fn check_checkpoint(
        &self,
        operation: Uuid,
        request: &MutationRequest,
        value: &SecretString,
    ) -> MutationResult<String> {
        let name = self.check_request(request)?;
        if value.expose_secret().len() > 4096 {
            return Err(MutationError::Invalid);
        }
        let saved: FolderCheckpoint =
            serde_json::from_str(value.expose_secret()).map_err(|_| MutationError::Invalid)?;
        if saved.version != 1
            || saved.operation != operation
            || saved.scope != self.scope
            || saved.parent != self.parent.id
            || saved.name != name
            || !saved.id.starts_with("FOLDER::com.apple.CloudDocs::")
            || saved.id.rsplit("::").next().is_none_or(str::is_empty)
            || saved.id == self.parent.id
        {
            return Err(MutationError::Invalid);
        }
        Ok(saved.id)
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

    async fn observe_parent(&self) -> MutationResult<()> {
        if self.parent.id == ROOT_ID {
            return Ok(());
        }
        let ancestor = self
            .parent
            .parent_id
            .as_deref()
            .ok_or(MutationError::Invalid)?;
        let mut state = self.session.lock().await;
        let session = Self::active_session(&mut state).await?;
        let entries = session
            .list_folder(ancestor)
            .await
            .map_err(crate::mutation_error)?;
        if entries
            .iter()
            .filter(|entry| {
                entry.drivewsid == self.parent.id
                    && entry.parent_id == ancestor
                    && entry.display_name() == self.parent.name
                    && entry.is_folder()
            })
            .count()
            != 1
        {
            return Err(MutationError::Conflict);
        }
        Ok(())
    }

    async fn observe_child(
        &self,
        name: &str,
        expected_id: Option<&str>,
    ) -> MutationResult<Option<Node>> {
        let mut state = self.session.lock().await;
        let session = Self::active_session(&mut state).await?;
        let children = session
            .list_folder(&self.parent.id)
            .await
            .map_err(crate::mutation_error)?;
        if let Some(id) = expected_id {
            if children
                .iter()
                .any(|entry| entry.drivewsid != id && entry.display_name() == name)
            {
                return Err(MutationError::Conflict);
            }
            let mut matches = children.iter().filter(|entry| entry.drivewsid == id);
            let Some(entry) = matches.next() else {
                return Ok(None);
            };
            if matches.next().is_some()
                || !entry.is_folder()
                || entry.display_name() != name
                || entry.parent_id != self.parent.id
            {
                return Err(MutationError::Conflict);
            }
            return Ok(Some(Node {
                id: id.into(),
                parent_id: Some(self.parent.id.clone()),
                name: name.into(),
                kind: NodeKind::Folder,
                size: 0,
                modified_unix: 0,
                etag: (!entry.etag.is_empty()).then(|| entry.etag.clone()),
                content_version: None,
                target: None,
                package: false,
            }));
        }
        if children.iter().any(|entry| entry.display_name() == name) {
            return Err(MutationError::Conflict);
        }
        Ok(None)
    }

    async fn save_identity(
        &self,
        operation: Uuid,
        request: &MutationRequest,
        id: &str,
    ) -> MutationResult<()> {
        let name = self.check_request(request)?;
        let checkpoint = FolderCheckpoint {
            version: 1,
            operation,
            scope: self.scope.clone(),
            parent: self.parent.id.clone(),
            name: name.into(),
            id: id.into(),
        };
        let value = serde_json::to_string(&checkpoint).map_err(|_| MutationError::Invalid)?;
        self.checkpoint_vault
            .save(&self.key(operation), SecretString::from(value))
            .await
            .map_err(crate::mutation_error)
    }

    async fn saved_identity(
        &self,
        operation: Uuid,
        request: &MutationRequest,
    ) -> MutationResult<Option<String>> {
        let Some(value) = self
            .checkpoint_vault
            .load(&self.key(operation))
            .await
            .map_err(crate::mutation_error)?
        else {
            return Ok(None);
        };
        self.check_checkpoint(operation, request, &value).map(Some)
    }
}

#[async_trait]
impl MutationProvider for ICloudFolderCreate {
    async fn mutate_operation(
        &self,
        operation: &str,
        request: &MutationRequest,
        prepared_item: Option<&str>,
        cancel: &CancellationToken,
    ) -> MutationResult<MutationReceipt> {
        let operation = Uuid::parse_str(operation).map_err(|_| MutationError::Invalid)?;
        if prepared_item.is_some() {
            return Err(MutationError::Invalid);
        }
        let name = self.check_request(request)?;
        #[cfg(feature = "write-probe")]
        if self.reconciliation_only {
            return Err(MutationError::Unsupported("reconciliation-only validation"));
        }
        if cancel.is_cancelled() {
            return Err(MutationError::Uncertain);
        }
        if self.saved_identity(operation, request).await?.is_some() {
            return Err(MutationError::Uncertain);
        }
        self.observe_parent().await?;
        self.observe_child(name, None).await?;
        if cancel.is_cancelled() {
            return Err(MutationError::Uncertain);
        }
        let id = {
            let mut state = self.session.lock().await;
            let session = Self::active_session(&mut state).await?;
            session
                .create_folder_request(&self.parent.id, name)
                .await
                .map_err(crate::mutation_error)?
        };
        self.save_identity(operation, request, &id).await?;
        let node = self
            .observe_child(name, Some(&id))
            .await?
            .ok_or(MutationError::Uncertain)?;
        Ok(MutationReceipt::Upsert(node))
    }

    async fn reconcile_operation(
        &self,
        operation: &str,
        request: &MutationRequest,
        prepared_item: Option<&str>,
        _: &CancellationToken,
    ) -> MutationResult<MutationReconciliation> {
        let operation = Uuid::parse_str(operation).map_err(|_| MutationError::Invalid)?;
        if prepared_item.is_some() {
            return Err(MutationError::Invalid);
        }
        let name = self.check_request(request)?;
        let Some(id) = self.saved_identity(operation, request).await? else {
            return Ok(MutationReconciliation::Indeterminate);
        };
        match self.observe_child(name, Some(&id)).await {
            Ok(Some(node)) => Ok(MutationReconciliation::Applied(MutationReceipt::Upsert(
                node,
            ))),
            Ok(None) | Err(MutationError::Uncertain) => Ok(MutationReconciliation::Indeterminate),
            Err(MutationError::Conflict) => Ok(MutationReconciliation::Conflict),
            Err(error) => Err(error),
        }
    }

    async fn mutate(
        &self,
        _: &MutationRequest,
        _: &CancellationToken,
    ) -> MutationResult<MutationReceipt> {
        Err(MutationError::Unsupported("durable operation ID required"))
    }

    async fn reconcile_mutation(
        &self,
        _: &MutationRequest,
        _: &CancellationToken,
    ) -> MutationResult<MutationReconciliation> {
        Ok(MutationReconciliation::Indeterminate)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod session_tests {
    use super::*;
    struct EmptyVault;
    #[async_trait]
    impl CredentialVault for EmptyVault {
        async fn load(&self, _: &str) -> anyhow::Result<Option<SecretString>> {
            Ok(None)
        }
        async fn save(&self, _: &str, _: SecretString) -> anyhow::Result<()> {
            Ok(())
        }
        async fn remove(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn mutation_session_mapping_requires_a_typed_cause() {
        let rejected = anyhow::Error::new(crate::SessionRejected).context("PRIVATE RESPONSE");
        assert!(matches!(
            crate::mutation_error(rejected),
            MutationError::Provider(cirrove_core::ProviderError::Authentication)
        ));
        for error in [
            anyhow::anyhow!("401 PRIVATE RESPONSE"),
            anyhow::Error::new(crate::IncompleteFolder),
        ] {
            let mapped = crate::mutation_error(error);
            assert!(matches!(mapped, MutationError::Uncertain));
            assert!(!mapped.to_string().contains("PRIVATE"));
        }
    }

    #[tokio::test]
    async fn folder_preflight_preserves_session_rejection_without_claiming_a_conflict() {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        for status in [401, 403, 503] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}/", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = [0u8; 8192];
                assert!(stream.read(&mut bytes).await.unwrap() > 0);
                stream.write_all(format!("HTTP/1.1 {status} Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
            });
            let mut session = ICloudReadSession::new().unwrap();
            session.drive_endpoint = Some(url::Url::parse(&endpoint).unwrap());
            let provider = ICloudFolderCreate {
                scope: Scope {
                    account: Uuid::new_v4().to_string(),
                    provider: "icloud".into(),
                    collection: "drive".into(),
                },
                parent: Node {
                    id: "FOLDER::com.apple.CloudDocs::owned".into(),
                    parent_id: Some(ROOT_ID.into()),
                    name: "Owned".into(),
                    kind: NodeKind::Folder,
                    size: 0,
                    modified_unix: 0,
                    etag: None,
                    content_version: None,
                    target: None,
                    package: false,
                },
                checkpoint_vault: Arc::new(EmptyVault),
                session: Mutex::new(SessionState::Ready(Box::new(session))),
                #[cfg(feature = "write-probe")]
                reconciliation_only: false,
            };
            let result = provider.observe_parent().await;
            server.await.unwrap();
            if status == 503 {
                assert!(matches!(result, Err(MutationError::Uncertain)));
            } else {
                assert!(
                    matches!(
                        result,
                        Err(MutationError::Provider(
                            cirrove_core::ProviderError::Authentication
                        ))
                    ),
                    "{status}: {result:?}"
                );
            }
        }
    }
}
