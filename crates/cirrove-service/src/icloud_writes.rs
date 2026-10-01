//! Account-wide iCloud write routing. Settings keep this experimental path gated.
//! Every checkpoint captures its parent before any network mutation; recovery
//! must not depend on an item still being present in today's metadata index.
use crate::{
    accounts::Account,
    journal::{UploadJournal, UploadState},
    manager::WriteContext,
};
use async_trait::async_trait;
use cirrove_auth::{AccessMode, AppRegistration, CredentialVault};
use cirrove_core::{
    CancellationToken, Node, NodeKind, Scope,
    mutation::{
        MutationError, MutationProvider, MutationReceipt, MutationReconciliation, MutationRequest,
    },
    upload::{
        Reconciliation, Result, UploadError, UploadIntent, UploadProgress, UploadProvider,
        UploadRequest, UploadStep,
    },
};
use cirrove_icloud::{ICloudFileCreate, ROOT_ID, SealedFolderCheckpointVault};
use cirrove_store::Store;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

pub struct ICloudWriteProvider {
    scope: Scope,
    apple_id: String,
    credential_id: String,
    state: PathBuf,
    metadata: PathBuf,
    journal: Arc<Mutex<UploadJournal>>,
    folder_vault: Arc<dyn CredentialVault>,
    #[cfg(test)]
    folder_create_test_adapter: Option<Arc<dyn MutationProvider>>,
    #[cfg(test)]
    package_test_adapter: Option<Arc<dyn UploadProvider>>,
    #[cfg(feature = "icloud-write-probe")]
    discard_registration: Option<Uuid>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateEnvelope {
    version: u8,
    operation: Uuid,
    request: UploadRequest,
    parent: Node,
    inner: String,
}

impl ICloudWriteProvider {
    pub fn new(account: &Account, context: &WriteContext) -> anyhow::Result<Self> {
        anyhow::ensure!(
            matches!(account.registration, AppRegistration::ICloud)
                && account.access == AccessMode::ReadWrite
                && account.root_id == ROOT_ID
                && account.drive.id == "drive"
                && account.drive.drive_type == "icloud_drive",
            "invalid iCloud write account"
        );
        Uuid::parse_str(&account.id)?;
        Uuid::parse_str(&account.credential_id)?;
        anyhow::ensure!(
            !account.identity.username.trim().is_empty(),
            "missing Apple account"
        );
        Ok(Self {
            #[cfg(test)]
            folder_create_test_adapter: None,
            #[cfg(test)]
            package_test_adapter: None,
            #[cfg(feature = "icloud-write-probe")]
            discard_registration: None,
            scope: Scope {
                account: account.id.clone(),
                provider: "icloud".into(),
                collection: "drive".into(),
            },
            apple_id: account.identity.username.clone(),
            credential_id: account.credential_id.clone(),
            state: context.state().to_owned(),
            metadata: context.metadata_db().to_owned(),
            journal: context.journal(),
            folder_vault: Arc::new(SealedFolderCheckpointVault::new(
                context.state(),
                &account.id,
            )?),
        })
    }

    #[cfg(feature = "icloud-write-probe")]
    pub(crate) fn validation_folder_vault(mut self, vault: Arc<dyn CredentialVault>) -> Self {
        self.folder_vault = vault;
        self
    }

    #[cfg(feature = "icloud-write-probe")]
    pub(crate) fn validation_discard_registration(mut self, operation: Uuid) -> Self {
        self.discard_registration = Some(operation);
        self
    }

    fn validate_operation(&self, operation: &str, request: &UploadRequest) -> Result<Uuid> {
        request.validate()?;
        if request.scope != self.scope {
            return Err(UploadError::Invalid);
        }
        let id = Uuid::parse_str(operation).map_err(|_| UploadError::Invalid)?;
        let journal = self.journal.lock().map_err(|_| UploadError::Uncertain)?;
        let row = journal.get(id).map_err(|_| UploadError::Invalid)?;
        if row.scope != request.scope
            || row.intent != request.intent
            || row.representation != request.representation
            || row.size != request.size
            || row.sha256 != request.sha256
            || !matches!(
                row.state,
                UploadState::Pending
                    | UploadState::Uploading
                    | UploadState::VerifyRequired
                    | UploadState::Verifying
                    | UploadState::Failed
            )
        {
            return Err(UploadError::Invalid);
        }
        Ok(id)
    }

    async fn parent(&self, id: &str) -> Result<Node> {
        self.parent_excluding(id, None).await
    }

    async fn parent_excluding(&self, id: &str, excluded: Option<String>) -> Result<Node> {
        if id == ROOT_ID {
            return Ok(Node {
                id: ROOT_ID.into(),
                parent_id: None,
                name: "iCloud Drive".into(),
                kind: NodeKind::Folder,
                size: 0,
                modified_unix: 0,
                etag: None,
                content_version: None,
                target: None,
                package: false,
            });
        }
        let (metadata, scope, id) = (self.metadata.clone(), self.scope.clone(), id.to_owned());
        tokio::task::spawn_blocking(move || {
            let store = Store::open(&metadata).map_err(|_| UploadError::Uncertain)?;
            let chain = store
                .node_chain_to_root(&scope, &id, ROOT_ID)
                .map_err(|_| UploadError::Uncertain)?
                .ok_or(UploadError::Conflict)?;
            if chain.iter().any(|node| {
                node.kind != NodeKind::Folder
                    || node.package
                    || node.target.is_some()
                    || excluded.as_deref() == Some(node.id.as_str())
            }) {
                return Err(UploadError::Invalid);
            }
            chain.into_iter().next().ok_or(UploadError::Conflict)
        })
        .await
        .map_err(|_| UploadError::Uncertain)?
    }

    fn create(&self, parent: &Node) -> Result<ICloudFileCreate> {
        ICloudFileCreate::from_sealed_session(
            self.scope.clone(),
            self.apple_id.clone(),
            self.credential_id.clone(),
            &self.state,
            parent.clone(),
        )
    }

    fn restore(
        &self,
        operation: &str,
        request: &UploadRequest,
        checkpoint: &SecretString,
    ) -> Result<(CreateEnvelope, ICloudFileCreate)> {
        request.require_file_bytes()?;
        let id = self.validate_operation(operation, request)?;
        if checkpoint.expose_secret().len() > 32 * 1024 {
            return Err(UploadError::CheckpointInvalid);
        }
        let saved: CreateEnvelope = serde_json::from_str(checkpoint.expose_secret())
            .map_err(|_| UploadError::CheckpointInvalid)?;
        let UploadIntent::Create { parent, .. } = &request.intent else {
            return Err(UploadError::Invalid);
        };
        if saved.version != 1
            || saved.operation != id
            || saved.request != *request
            || saved.parent.id != *parent
        {
            return Err(UploadError::CheckpointInvalid);
        }
        let adapter = self.create(&saved.parent)?;
        Ok((saved, adapter))
    }
}

impl CreateEnvelope {
    fn wrap(mut self, step: UploadStep) -> Result<UploadStep> {
        let (inner, kind, range) = match step {
            UploadStep::Prepared(inner) => (inner, 0, None),
            UploadStep::Stream(inner) => (inner, 1, None),
            UploadStep::Commit(inner) => (inner, 2, None),
            UploadStep::Continue(p) => (p.checkpoint, 3, Some((p.offset, p.length))),
            receipt => return Ok(receipt),
        };
        self.inner = inner.expose_secret().to_string();
        let value = serde_json::to_string(&self).map_err(|_| UploadError::CheckpointInvalid)?;
        if value.len() > 32 * 1024 {
            return Err(UploadError::CheckpointInvalid);
        }
        let checkpoint = SecretString::from(value);
        Ok(match (kind, range) {
            (0, _) => UploadStep::Prepared(checkpoint),
            (1, _) => UploadStep::Stream(checkpoint),
            (2, _) => UploadStep::Commit(checkpoint),
            (_, Some((offset, length))) => UploadStep::Continue(UploadProgress {
                checkpoint,
                offset,
                length,
            }),
            _ => return Err(UploadError::CheckpointInvalid),
        })
    }
}

#[async_trait]
impl UploadProvider for ICloudWriteProvider {
    fn staged_recovery_location(
        &self,
        operation: &str,
        request: &UploadRequest,
    ) -> Option<cirrove_core::upload::RecoveryLocation> {
        if request.require_file_bytes().is_err()
            || !matches!(request.intent, UploadIntent::Replace { .. })
        {
            return None;
        }
        let operation = self.validate_operation(operation, request).ok()?;
        Some(cirrove_icloud::ICloudFileReplace::recovery_location(
            operation,
        ))
    }

    fn inspection_timeout(&self, request: &UploadRequest) -> std::time::Duration {
        self.commit_timeout(request)
    }

    fn commit_timeout(&self, request: &UploadRequest) -> std::time::Duration {
        if !request.representation.is_file_bytes() {
            return std::time::Duration::from_secs(240);
        }
        // Full integrity verification exceeded the default in the registered
        // owned-replacement runs. Keep those checks and their tested deadline.
        std::time::Duration::from_secs(if matches!(request.intent, UploadIntent::Replace { .. }) {
            300
        } else {
            125
        })
    }

    fn begin_is_mutation_free_until_checkpoint(&self, request: &UploadRequest) -> bool {
        request.validate().is_ok()
            && request.scope == self.scope
            && matches!(request.intent, UploadIntent::Create { .. })
    }
    async fn begin_upload(&self, _: &UploadRequest, _: &CancellationToken) -> Result<UploadStep> {
        Err(UploadError::Unsupported(
            "durable iCloud operation required",
        ))
    }
    async fn begin_upload_for_operation(
        &self,
        operation: &str,
        request: &UploadRequest,
        cancel: &CancellationToken,
    ) -> Result<UploadStep> {
        if !request.representation.is_file_bytes() {
            return self
                .package_adapter(operation, request, None)
                .await?
                .begin_upload_for_operation(operation, request, cancel)
                .await;
        }

        if matches!(request.intent, UploadIntent::Replace { .. }) {
            return self
                .replacement(operation, request, None)
                .await?
                .begin_upload(request, cancel)
                .await;
        }
        let operation = self.validate_operation(operation, request)?;
        let UploadIntent::Create { parent, .. } = &request.intent else {
            return Err(UploadError::Unsupported("account-wide iCloud replacement"));
        };
        let parent = self.parent(parent).await?;
        let step = self.create(&parent)?.begin_upload(request, cancel).await?;
        CreateEnvelope {
            version: 1,
            operation,
            request: request.clone(),
            parent,
            inner: String::new(),
        }
        .wrap(step)
    }
    async fn allocate_upload_for_operation(
        &self,
        operation: &str,
        request: &UploadRequest,
        checkpoint: &SecretString,
        cancel: &CancellationToken,
    ) -> Result<UploadStep> {
        if request.representation.is_file_bytes() {
            return Err(UploadError::Unsupported(
                "ordinary iCloud allocation callback",
            ));
        }
        self.package_adapter(operation, request, Some(checkpoint))
            .await?
            .allocate_upload_for_operation(operation, request, checkpoint, cancel)
            .await
    }
    async fn inspect_upload(
        &self,
        _: &UploadRequest,
        _: &SecretString,
        _: &CancellationToken,
    ) -> Result<UploadStep> {
        Err(UploadError::Unsupported(
            "durable iCloud operation required",
        ))
    }
    async fn inspect_upload_for_operation(
        &self,
        operation: &str,
        request: &UploadRequest,
        checkpoint: &SecretString,
        cancel: &CancellationToken,
    ) -> Result<UploadStep> {
        if !request.representation.is_file_bytes() {
            return self
                .package_adapter(operation, request, Some(checkpoint))
                .await?
                .inspect_upload_for_operation(operation, request, checkpoint, cancel)
                .await;
        }

        if matches!(request.intent, UploadIntent::Replace { .. }) {
            return self
                .replacement(operation, request, Some(checkpoint))
                .await?
                .inspect_upload(request, checkpoint, cancel)
                .await;
        }
        let (saved, adapter) = self.restore(operation, request, checkpoint)?;
        let step = adapter
            .inspect_upload(request, &SecretString::from(saved.inner.clone()), cancel)
            .await?;
        saved.wrap(step)
    }
    async fn upload_part(
        &self,
        _: &UploadRequest,
        _: &SecretString,
        _: u64,
        _: Vec<u8>,
        _: &CancellationToken,
    ) -> Result<UploadStep> {
        Err(UploadError::Unsupported("iCloud multipart upload"))
    }
    async fn upload_stream_for_operation(
        &self,
        operation: &str,
        request: &UploadRequest,
        checkpoint: &SecretString,
        payload: File,
        cancel: &CancellationToken,
    ) -> Result<UploadStep> {
        if !request.representation.is_file_bytes() {
            return self
                .package_adapter(operation, request, Some(checkpoint))
                .await?
                .upload_stream_for_operation(operation, request, checkpoint, payload, cancel)
                .await;
        }

        if matches!(request.intent, UploadIntent::Replace { .. }) {
            return self
                .replacement(operation, request, Some(checkpoint))
                .await?
                .upload_stream(request, checkpoint, payload, cancel)
                .await;
        }
        let (saved, adapter) = self.restore(operation, request, checkpoint)?;
        let step = adapter
            .upload_stream(
                request,
                &SecretString::from(saved.inner.clone()),
                payload,
                cancel,
            )
            .await?;
        saved.wrap(step)
    }
    async fn commit_upload(
        &self,
        _: &UploadRequest,
        _: &SecretString,
        _: &CancellationToken,
    ) -> Result<UploadStep> {
        Err(UploadError::Unsupported(
            "durable iCloud operation required",
        ))
    }
    async fn commit_upload_for_operation(
        &self,
        operation: &str,
        request: &UploadRequest,
        checkpoint: &SecretString,
        cancel: &CancellationToken,
    ) -> Result<UploadStep> {
        if !request.representation.is_file_bytes() {
            return self
                .package_adapter(operation, request, Some(checkpoint))
                .await?
                .commit_upload_for_operation(operation, request, checkpoint, cancel)
                .await;
        }

        if matches!(request.intent, UploadIntent::Replace { .. }) {
            let adapter = self
                .replacement(operation, request, Some(checkpoint))
                .await?;
            #[cfg(feature = "icloud-write-probe")]
            let adapter = if self
                .discard_registration
                .is_some_and(|id| operation == id.to_string())
            {
                adapter.with_discarded_stage_registration_response()
            } else {
                adapter
            };
            return adapter.commit_upload(request, checkpoint, cancel).await;
        }
        let (saved, adapter) = self.restore(operation, request, checkpoint)?;
        #[cfg(feature = "icloud-write-probe")]
        let adapter = if self.discard_registration == Some(saved.operation) {
            adapter.with_discarded_registration_response()
        } else {
            adapter
        };
        let step = adapter
            .commit_upload(request, &SecretString::from(saved.inner.clone()), cancel)
            .await?;
        saved.wrap(step)
    }
    async fn reconcile_upload(
        &self,
        _: &UploadRequest,
        _: Option<&SecretString>,
        _: &CancellationToken,
    ) -> Result<Reconciliation> {
        Err(UploadError::Uncertain)
    }
    async fn reconcile_upload_for_operation(
        &self,
        operation: &str,
        request: &UploadRequest,
        checkpoint: Option<&SecretString>,
        cancel: &CancellationToken,
    ) -> Result<Reconciliation> {
        if !request.representation.is_file_bytes() {
            let saved = checkpoint.ok_or(UploadError::Uncertain)?;
            return self
                .package_adapter(operation, request, Some(saved))
                .await?
                .reconcile_upload_for_operation(operation, request, Some(saved), cancel)
                .await;
        }

        if matches!(request.intent, UploadIntent::Replace { .. }) {
            return self
                .replacement(operation, request, checkpoint)
                .await?
                .reconcile_upload(request, checkpoint, cancel)
                .await;
        }
        let checkpoint = checkpoint.ok_or(UploadError::Uncertain)?;
        let (saved, adapter) = self.restore(operation, request, checkpoint)?;
        adapter
            .reconcile_upload(request, Some(&SecretString::from(saved.inner)), cancel)
            .await
    }
}

mod folders;
mod packages;
mod replacements;

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    pub(super) fn fixture() -> (tempfile::TempDir, ICloudWriteProvider) {
        let temp = tempfile::tempdir().unwrap();
        let account = Uuid::new_v4().to_string();
        let journal =
            UploadJournal::open(&temp.path().join("journal"), &account, 1024 * 1024).unwrap();
        let metadata = temp.path().join("metadata.db");
        Store::open(&metadata).unwrap();
        let provider = ICloudWriteProvider {
            folder_create_test_adapter: None,
            #[cfg(test)]
            package_test_adapter: None,
            #[cfg(feature = "icloud-write-probe")]
            discard_registration: None,
            scope: Scope {
                account,
                provider: "icloud".into(),
                collection: "drive".into(),
            },
            apple_id: "synthetic@example.invalid".into(),
            credential_id: Uuid::new_v4().to_string(),
            state: temp.path().to_owned(),
            metadata,
            journal: Arc::new(Mutex::new(journal)),
            folder_vault: Arc::new(folders::tests::MemoryVault::default()),
        };
        (temp, provider)
    }

    pub(super) fn folder(parent: &str) -> Node {
        Node {
            id: "FOLDER::com.apple.CloudDocs::synthetic-parent".into(),
            parent_id: Some(parent.into()),
            name: "Parent".into(),
            kind: NodeKind::Folder,
            size: 0,
            modified_unix: 0,
            etag: Some("parent-version".into()),
            content_version: None,
            target: None,
            package: false,
        }
    }

    fn enqueue(p: &ICloudWriteProvider, parent: &str) -> (String, UploadRequest) {
        let row = p
            .journal
            .lock()
            .unwrap()
            .enqueue(
                p.scope.clone(),
                UploadIntent::Create {
                    parent: parent.into(),
                    name: "New.txt".into(),
                },
                &b"synthetic payload"[..],
            )
            .unwrap();
        (
            row.id.to_string(),
            UploadRequest {
                representation: Default::default(),
                scope: row.scope,
                intent: row.intent,
                size: row.size,
                sha256: row.sha256,
            },
        )
    }

    #[tokio::test]
    async fn create_reopens_saved_parent_after_metadata_changes_and_binds_operation() {
        let (_temp, p) = fixture();
        let parent = folder(ROOT_ID);
        let mut store = Store::open(&p.metadata).unwrap();
        store.observe_node(&p.scope, &parent).unwrap();
        let (operation, request) = enqueue(&p, &parent.id);
        let cancel = CancellationToken::new();
        let UploadStep::Prepared(saved) = p
            .begin_upload_for_operation(&operation, &request, &cancel)
            .await
            .unwrap()
        else {
            panic!("create must persist before contacting Apple")
        };
        let mut changed = parent.clone();
        changed.package = true;
        store.observe_node(&p.scope, &changed).unwrap();
        assert!(p.parent(&parent.id).await.is_err());
        let (restored, _) = p.restore(&operation, &request, &saved).unwrap();
        assert_eq!(restored.parent, parent);
        // An identical request still belongs to a different durable operation.
        let (other_operation, same_request) = enqueue(&p, &parent.id);
        assert!(p.restore(&other_operation, &same_request, &saved).is_err());
        let mut wrong = request.clone();
        wrong.size += 1;
        assert!(p.restore(&operation, &wrong, &saved).is_err());
        wrong = request.clone();
        wrong.scope.account = Uuid::new_v4().to_string();
        assert!(p.restore(&operation, &wrong, &saved).is_err());
        let invalid = SecretString::from("{}".to_string());
        assert!(p.restore(&operation, &request, &invalid).is_err());
        assert!(
            p.reconcile_upload_for_operation(&operation, &request, None, &cancel)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn create_requires_scoped_plain_folder_ancestry_before_checkpoint() {
        let (_temp, p) = fixture();
        let mut store = Store::open(&p.metadata).unwrap();
        let parent = folder("FOLDER::com.apple.CloudDocs::missing");
        store.observe_node(&p.scope, &parent).unwrap();
        let (operation, request) = enqueue(&p, &parent.id);
        let cancel = CancellationToken::new();
        assert!(
            p.begin_upload_for_operation(&operation, &request, &cancel)
                .await
                .is_err()
        );
        let ancestor = Node {
            id: parent.parent_id.clone().unwrap(),
            package: true,
            ..folder(ROOT_ID)
        };
        store.observe_node(&p.scope, &ancestor).unwrap();
        assert!(
            p.begin_upload_for_operation(&operation, &request, &cancel)
                .await
                .is_err()
        );
        store
            .observe_node(
                &p.scope,
                &Node {
                    package: false,
                    ..ancestor
                },
            )
            .unwrap();
        assert!(matches!(
            p.begin_upload_for_operation(&operation, &request, &cancel)
                .await
                .unwrap(),
            UploadStep::Prepared(_)
        ));
        let (root_operation, root_request) = enqueue(&p, ROOT_ID);
        assert!(matches!(
            p.begin_upload_for_operation(&root_operation, &root_request, &cancel)
                .await
                .unwrap(),
            UploadStep::Prepared(_)
        ));
        assert!(p.begin_upload(&root_request, &cancel).await.is_err());
    }
    #[tokio::test]
    async fn package_archive_cannot_relabel_an_ordinary_journal_row() {
        let (_temp, provider) = fixture();
        let (operation, mut request) = enqueue(&provider, ROOT_ID);
        request.representation = cirrove_core::upload::UploadRepresentation::PackageArchive {
            expected_root: "Source.pages".into(),
            semantic: cirrove_core::upload::PackageSemanticIdentity {
                version: 1,
                sha256: "a".repeat(64),
                entries: 1,
                files: 1,
                expanded_bytes: 1,
            },
        };
        assert!(matches!(
            provider
                .begin_upload_for_operation(&operation, &request, &CancellationToken::new())
                .await,
            Err(UploadError::Invalid)
        ));
    }
}
