//! Journal-driven folder creation inside a fresh Cirrove validation folder.
//! Apple's allocated ID is saved before the worker sees a success response.
//! Without that ID, a matching name is never treated as proof of creation.
use super::{ICloudReadSession, ValidationFolder};
use async_trait::async_trait;
use cirrove_auth::CredentialVault;
use cirrove_core::mutation::{
    MutationError, MutationIntent, MutationProvider, MutationReceipt, MutationReconciliation,
    MutationRequest, Result as MutationResult,
};
use cirrove_core::{CancellationToken, Node, NodeKind, Scope};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
struct FolderCheckpoint {
    version: u8,
    operation: Uuid,
    scope: Scope,
    parent: String,
    name: String,
    id: String,
}

pub struct ICloudOwnedFixtureFolderCreate {
    scope: Scope,
    parent: ValidationFolder,
    operation: Uuid,
    vault: Arc<dyn CredentialVault>,
    session: Mutex<ICloudReadSession>,
    discard_receipt: AtomicBool,
    reconciliation_only: bool,
}

impl ICloudOwnedFixtureFolderCreate {
    pub fn new(
        scope: Scope,
        session: ICloudReadSession,
        parent: ValidationFolder,
        operation: Uuid,
        vault: Arc<dyn CredentialVault>,
    ) -> MutationResult<Self> {
        if scope.account.is_empty()
            || scope.provider != "icloud"
            || scope.collection != "drive"
            || session.account_hash.is_none()
            || !parent.id.starts_with("FOLDER::com.apple.CloudDocs::")
            || parent
                .name
                .strip_prefix("Cirrove Write Validation-")
                .is_none_or(|suffix| Uuid::parse_str(suffix).is_err())
        {
            return Err(MutationError::Invalid);
        }
        Ok(Self {
            scope,
            parent,
            operation,
            vault,
            session: Mutex::new(session),
            discard_receipt: AtomicBool::new(false),
            reconciliation_only: false,
        })
    }

    pub fn with_discarded_receipt(self) -> Self {
        self.discard_receipt.store(true, Ordering::Release);
        self
    }

    pub fn reconciliation_only(mut self) -> Self {
        self.reconciliation_only = true;
        self
    }

    fn key(&self) -> String {
        format!(
            "icloud-folder-create/{}/{}",
            self.scope.account, self.operation
        )
    }

    fn check_request<'a>(&self, request: &'a MutationRequest) -> MutationResult<&'a str> {
        request.validate()?;
        let MutationIntent::CreateFolder { parent, name } = &request.intent else {
            return Err(MutationError::Unsupported(
                "owned fixture folder creation only",
            ));
        };
        if request.scope != self.scope
            || parent != &self.parent.id
            || name.len() > 255
            || name.starts_with("Cirrove Write Validation-")
        {
            return Err(MutationError::Invalid);
        }
        Ok(name)
    }

    fn check_checkpoint(
        &self,
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
            || saved.operation != self.operation
            || saved.scope != self.scope
            || saved.parent != self.parent.id
            || saved.name != name
            || !saved.id.starts_with("FOLDER::com.apple.CloudDocs::")
            || saved.id == self.parent.id
        {
            return Err(MutationError::Invalid);
        }
        Ok(saved.id)
    }

    async fn observe_parent(&self) -> MutationResult<()> {
        let mut session = self.session.lock().await;
        let root = session
            .list_root()
            .await
            .map_err(|_| MutationError::Uncertain)?;
        if root
            .iter()
            .filter(|entry| {
                entry.drivewsid == self.parent.id
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
        let mut session = self.session.lock().await;
        let children = session
            .list_folder(&self.parent.id)
            .await
            .map_err(|_| MutationError::Uncertain)?;
        if let Some(id) = expected_id {
            let matching: Vec<_> = children
                .iter()
                .filter(|entry| entry.drivewsid == id)
                .collect();
            if matching.len() != 1 {
                return Ok(None);
            }
            let entry = matching[0];
            if !entry.is_folder()
                || entry.display_name() != name
                || entry.parent_id != self.parent.id
                || children
                    .iter()
                    .any(|other| other.drivewsid != id && other.display_name() == name)
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

    async fn save_identity(&self, request: &MutationRequest, id: &str) -> MutationResult<()> {
        let name = self.check_request(request)?;
        let saved = FolderCheckpoint {
            version: 1,
            operation: self.operation,
            scope: self.scope.clone(),
            parent: self.parent.id.clone(),
            name: name.into(),
            id: id.into(),
        };
        let value = serde_json::to_string(&saved).map_err(|_| MutationError::Invalid)?;
        self.vault
            .save(&self.key(), SecretString::from(value))
            .await
            .map_err(|_| MutationError::Uncertain)
    }

    async fn saved_identity(&self, request: &MutationRequest) -> MutationResult<Option<String>> {
        let Some(value) = self
            .vault
            .load(&self.key())
            .await
            .map_err(|_| MutationError::Uncertain)?
        else {
            return Ok(None);
        };
        self.check_checkpoint(request, &value).map(Some)
    }
}

#[async_trait]
impl MutationProvider for ICloudOwnedFixtureFolderCreate {
    async fn mutate(
        &self,
        request: &MutationRequest,
        cancel: &CancellationToken,
    ) -> MutationResult<MutationReceipt> {
        let name = self.check_request(request)?;
        if self.reconciliation_only {
            return Err(MutationError::Unsupported("reconciliation-only validation"));
        }
        if cancel.is_cancelled() {
            return Err(MutationError::Uncertain);
        }
        // A prior ID means the remote call may already have succeeded. Never
        // issue another create in that case, even if a listing is delayed.
        if self.saved_identity(request).await?.is_some() {
            return Err(MutationError::Uncertain);
        }
        self.observe_parent().await?;
        self.observe_child(name, None).await?;
        if cancel.is_cancelled() {
            return Err(MutationError::Uncertain);
        }
        let id = {
            let mut session = self.session.lock().await;
            session
                .create_folder_request(&self.parent.id, name)
                .await
                .map_err(|_| MutationError::Uncertain)?
        };
        self.save_identity(request, &id).await?;
        if self.discard_receipt.swap(false, Ordering::AcqRel) {
            return Err(MutationError::Uncertain);
        }
        let node = self
            .observe_child(name, Some(&id))
            .await?
            .ok_or(MutationError::Uncertain)?;
        Ok(MutationReceipt::Upsert(node))
    }

    async fn reconcile_mutation(
        &self,
        request: &MutationRequest,
        _cancel: &CancellationToken,
    ) -> MutationResult<MutationReconciliation> {
        let name = self.check_request(request)?;
        let Some(id) = self.saved_identity(request).await? else {
            // A name match is not an identity proof. The request might have
            // reached Apple before its response was lost.
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
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use anyhow::Result;
    use std::collections::HashMap;

    #[derive(Default)]
    struct MemoryVault(Mutex<HashMap<String, String>>);

    #[async_trait]
    impl CredentialVault for MemoryVault {
        async fn load(&self, key: &str) -> Result<Option<SecretString>> {
            Ok(self
                .0
                .lock()
                .await
                .get(key)
                .cloned()
                .map(SecretString::from))
        }
        async fn save(&self, key: &str, value: SecretString) -> Result<()> {
            self.0
                .lock()
                .await
                .insert(key.into(), value.expose_secret().to_owned());
            Ok(())
        }
        async fn remove(&self, key: &str) -> Result<()> {
            self.0.lock().await.remove(key);
            Ok(())
        }
    }

    fn fixture() -> (ICloudOwnedFixtureFolderCreate, MutationRequest) {
        let mut session = ICloudReadSession::new().unwrap();
        session.account_hash = Some("synthetic-account".into());
        let scope = Scope {
            account: Uuid::new_v4().to_string(),
            provider: "icloud".into(),
            collection: "drive".into(),
        };
        let parent = ValidationFolder {
            id: format!("FOLDER::com.apple.CloudDocs::{}", Uuid::new_v4()),
            name: format!("Cirrove Write Validation-{}", Uuid::new_v4()),
        };
        let request = MutationRequest {
            scope: scope.clone(),
            intent: MutationIntent::CreateFolder {
                parent: parent.id.clone(),
                name: "Nested test folder".into(),
            },
        };
        let provider = ICloudOwnedFixtureFolderCreate::new(
            scope,
            session,
            parent,
            Uuid::new_v4(),
            Arc::new(MemoryVault::default()),
        )
        .unwrap();
        (provider, request)
    }

    #[tokio::test]
    async fn no_allocated_id_never_treats_a_name_as_a_receipt() {
        let (provider, request) = fixture();
        assert!(matches!(
            provider
                .reconcile_mutation(&request, &CancellationToken::new())
                .await
                .unwrap(),
            MutationReconciliation::Indeterminate
        ));
    }

    #[tokio::test]
    async fn saved_identity_is_bound_to_operation_account_and_parent() {
        let (provider, request) = fixture();
        let id = format!("FOLDER::com.apple.CloudDocs::{}", Uuid::new_v4());
        provider.save_identity(&request, &id).await.unwrap();
        assert_eq!(provider.saved_identity(&request).await.unwrap(), Some(id));

        let mut other = request.clone();
        other.scope.account = Uuid::new_v4().to_string();
        assert!(provider.saved_identity(&other).await.is_err());
        let MutationIntent::CreateFolder { parent, .. } = &mut other.intent else {
            unreachable!()
        };
        *parent = "FOLDER::com.apple.CloudDocs::other".into();
        assert!(provider.saved_identity(&other).await.is_err());

        let saved = provider.vault.load(&provider.key()).await.unwrap().unwrap();
        let mut checkpoint: FolderCheckpoint = serde_json::from_str(saved.expose_secret()).unwrap();
        checkpoint.operation = Uuid::new_v4();
        let forged = SecretString::from(serde_json::to_string(&checkpoint).unwrap());
        assert!(provider.check_checkpoint(&request, &forged).is_err());
    }
}
