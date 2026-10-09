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
    package_staging_budget: cirrove_icloud::ICloudWriteStagingBudget,
    #[cfg(test)]
    folder_create_test_adapter: Option<Arc<dyn MutationProvider>>,
    #[cfg(test)]
    package_test_adapter: Option<Arc<dyn UploadProvider>>,
    #[cfg(test)]
    package_test_transport: Option<reqwest::Client>,
    #[cfg(test)]
    native_trash_test_vault: Option<Arc<dyn CredentialVault>>,
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
            #[cfg(test)]
            package_test_transport: None,
            #[cfg(test)]
            native_trash_test_vault: None,
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
            package_staging_budget: context
                .icloud_staging_budget()
                .ok_or_else(|| anyhow::anyhow!("missing iCloud staging budget"))?,
            folder_vault: Arc::new(SealedFolderCheckpointVault::new(
                context.state(),
                &account.id,
            )?),
        })
    }

    // Bind loopback only after the normal native factory has validated the
    // account, journal, parent, private staging and any captured checkpoint.
    #[cfg(all(test, feature = "icloud-write-probe"))]
    pub(crate) fn synthetic_native_trash_checkpoint(
        mut self,
        vault: Arc<dyn CredentialVault>,
    ) -> Self {
        self.native_trash_test_vault = Some(vault);
        self
    }
    #[cfg(test)]
    pub(crate) fn synthetic_native_transport(mut self, client: reqwest::Client) -> Self {
        self.package_test_transport = Some(client);
        self
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
        let journal = self.journal.clone();
        tokio::task::spawn_blocking(move || {
            let store = Store::open(&metadata).map_err(|_| UploadError::Uncertain)?;
            let chain = match store
                .node_chain_to_root(&scope, &id, ROOT_ID)
                .map_err(|_| UploadError::Uncertain)?
            {
                Some(chain) => chain,
                None => {
                    // Mkdir's confirmed receipt can unblock its child before
                    // the read feed indexes the new parent. Only that exact
                    // current scoped creation may supply the missing leaf;
                    // indexed changes/absence and all ancestors still win.
                    let confirmed = {
                        let journal = journal.lock().map_err(|_| UploadError::Uncertain)?;
                        let object = journal
                            .namespace_by_remote(&scope, &id)
                            .map_err(|_| UploadError::Uncertain)?
                            .filter(|o| !o.unlinked && !o.follows_remote)
                            .ok_or(UploadError::Conflict)?;
                        let latest = object.latest.ok_or(UploadError::Conflict)?;
                        let record = journal
                            .mutation(latest)
                            .map_err(|_| UploadError::Conflict)?;
                        let Some(MutationReceipt::Upsert(node)) = record.receipt.as_ref() else {
                            return Err(UploadError::Conflict);
                        };
                        if record.state != crate::journal::MutationState::Applied
                            || record.request.scope != scope
                            || !matches!(
                                record.request.intent,
                                cirrove_core::mutation::MutationIntent::CreateFolder { .. }
                            )
                            || !record
                                .request
                                .accepts(record.receipt.as_ref().ok_or(UploadError::Conflict)?)
                            || node.id != id
                            || object.remote.as_ref() != Some(node)
                            || object.remote_sequence != record.sequence
                        {
                            return Err(UploadError::Conflict);
                        }
                        node.clone()
                    };
                    store
                        .node_chain_to_root_with_unindexed_leaf(&scope, &confirmed, ROOT_ID)
                        .map_err(|_| UploadError::Uncertain)?
                        .ok_or(UploadError::Conflict)?
                }
            };
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
    fn requires_begin_payload(&self, request: &UploadRequest) -> bool {
        matches!(
            request.representation,
            cirrove_core::upload::UploadRepresentation::FlatNumbersArchive { .. }
                | cirrove_core::upload::UploadRepresentation::FlatPagesArchive { .. }
                | cirrove_core::upload::UploadRepresentation::FlatNumbersReplacementArchive { .. }
                | cirrove_core::upload::UploadRepresentation::FlatPagesReplacementArchive { .. }
        )
    }
    fn staged_recovery_location(
        &self,
        operation: &str,
        request: &UploadRequest,
    ) -> Option<cirrove_core::upload::RecoveryLocation> {
        if !matches!(request.intent, UploadIntent::Replace { .. }) {
            return None;
        }
        let operation = self.validate_operation(operation, request).ok()?;
        if let cirrove_core::upload::UploadRepresentation::PackageReplacementArchive {
            original,
            ..
        }
        | cirrove_core::upload::UploadRepresentation::FlatNumbersReplacementArchive {
            original,
            ..
        }
        | cirrove_core::upload::UploadRepresentation::FlatPagesReplacementArchive {
            original,
            ..
        } = &request.representation
        {
            let suffix = cirrove_core::upload::native_package_suffix(&original.name)?;
            return Some(cirrove_core::upload::RecoveryLocation::Trash {
                local_name: format!("recovery-by-cirrove-{operation}{suffix}"),
                parent: "FOLDER::com.apple.CloudDocs::TRASH_ROOT".into(),
            });
        }
        request.require_file_bytes().ok()?;
        self.ordinary_recovery_location(operation, request).ok()
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
            && (matches!(request.intent, UploadIntent::Create { .. })
                || matches!(
                    (&request.intent, &request.representation),
                    (
                        UploadIntent::Replace { .. },
                        cirrove_core::upload::UploadRepresentation::PackageReplacementArchive { .. }
                            | cirrove_core::upload::UploadRepresentation::FlatNumbersReplacementArchive { .. }
        | cirrove_core::upload::UploadRepresentation::FlatPagesReplacementArchive { .. }
                    )
                ))
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
    async fn begin_upload_from_payload_for_operation(
        &self,
        operation: &str,
        request: &UploadRequest,
        payload: File,
        cancel: &CancellationToken,
    ) -> Result<UploadStep> {
        if !request.representation.is_file_bytes() {
            return self
                .package_adapter(operation, request, None)
                .await?
                .begin_upload_from_payload_for_operation(operation, request, payload, cancel)
                .await;
        }
        self.begin_upload_for_operation(operation, request, cancel)
            .await
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
mod native_trash;
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
            #[cfg(test)]
            package_test_transport: None,
            #[cfg(test)]
            native_trash_test_vault: None,
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
            package_staging_budget: cirrove_icloud::ICloudWriteStagingBudget::default(),
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

    fn confirmed_unindexed_directory(p: &ICloudWriteProvider) -> (Uuid, Node) {
        confirmed_unindexed_directory_at(p, ROOT_ID)
    }

    fn confirmed_unindexed_directory_at(p: &ICloudWriteProvider, ancestor: &str) -> (Uuid, Node) {
        let mut journal = p.journal.lock().expect("journal lock");
        let object = journal
            .create_namespace_directory(p.scope.clone(), ancestor.into(), "Fresh parent".into())
            .expect("queue mkdir");
        let mkdir = journal
            .claim_mutation()
            .expect("claim mkdir")
            .expect("mkdir ready");
        let parent = Node {
            id: format!("FOLDER::com.apple.CloudDocs::{}", Uuid::new_v4()),
            name: object.node.name.clone(),
            ..folder(ancestor)
        };
        journal
            .acknowledge_mutation(
                mkdir.id,
                mkdir.attempt.expect("mkdir attempt"),
                MutationReceipt::Upsert(parent.clone()),
            )
            .expect("acknowledge mkdir");
        assert_eq!(
            journal.mutation(mkdir.id).expect("mkdir row").state,
            crate::journal::MutationState::Applied
        );
        (object.id, parent)
    }

    #[tokio::test]
    async fn create_after_confirmed_mkdir_prepares_before_parent_feed_publication() {
        let (_temp, p) = fixture();
        let (owner, parent) = confirmed_unindexed_directory(&p);
        let (operation, request) = {
            let mut journal = p.journal.lock().expect("journal lock");
            let directory = journal.namespace_object(owner).expect("local directory");
            let local = Node {
                id: format!("local-file-{}", Uuid::new_v4()),
                parent_id: Some(directory.node.id),
                name: "New.txt".into(),
                kind: NodeKind::File,
                size: 0,
                modified_unix: 0,
                etag: None,
                content_version: None,
                target: None,
                package: false,
            };
            let working = journal
                .create_working(p.scope.clone(), local, true, &b""[..])
                .expect("new file in fresh directory");
            journal
                .write_working(working.id, 0, b"synthetic payload")
                .expect("write file");
            journal
                .seal_working(working.id)
                .expect("seal file")
                .expect("queued file");
            let row = journal
                .claim_next()
                .expect("claim upload")
                .expect("mkdir unblocks upload");
            assert!(
                matches!(&row.intent, UploadIntent::Create { parent: id, .. } if id == &parent.id)
            );
            (
                row.id.to_string(),
                UploadRequest {
                    representation: row.representation,
                    scope: row.scope,
                    intent: row.intent,
                    size: row.size,
                    sha256: row.sha256,
                },
            )
        };
        let store = Store::open(&p.metadata).expect("metadata store");
        assert!(
            store
                .node(&p.scope, &parent.id)
                .expect("parent lookup")
                .is_none()
        );
        let UploadStep::Prepared(saved) = p
            .begin_upload_for_operation(&operation, &request, &CancellationToken::new())
            .await
            .expect("receipt-bound parent")
        else {
            panic!("create must checkpoint before provider contact");
        };
        let (envelope, _) = p
            .restore(&operation, &request, &saved)
            .expect("restore checkpoint");
        assert_eq!(envelope.parent, parent);
        // Planning supplies an ancestry hint, never writes the read index or cursor.
        assert!(
            store
                .node(&p.scope, &parent.id)
                .expect("parent remains unindexed")
                .is_none()
        );
    }

    #[tokio::test]
    async fn confirmed_mkdir_parent_never_overrides_indexed_absence_or_changed_shape() {
        let (_temp, p) = fixture();
        let (_, parent) = confirmed_unindexed_directory(&p);
        let (operation, request) = enqueue(&p, &parent.id);
        let mut store = Store::open(&p.metadata).expect("metadata store");
        let ticket = store
            .node_observation(&p.scope, &parent.id)
            .expect("absence ticket");
        store.publish_absence(&ticket).expect("publish absence");
        assert!(
            p.begin_upload_for_operation(&operation, &request, &CancellationToken::new())
                .await
                .is_err()
        );
        let changed = Node {
            package: true,
            ..parent.clone()
        };
        store
            .observe_node(&p.scope, &changed)
            .expect("changed metadata");
        assert!(
            p.begin_upload_for_operation(&operation, &request, &CancellationToken::new())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn confirmed_mkdir_parent_rejects_unlinked_owner_and_wrong_scope() {
        let (_temp, mut p) = fixture();
        let (owner, parent) = confirmed_unindexed_directory(&p);
        let original_scope = p.scope.clone();
        p.scope.collection = "other-collection".into();
        assert!(p.parent(&parent.id).await.is_err());
        p.scope = original_scope;
        // Folder removals cannot chain to their own creation receipt. Model
        // the unlinked boundary directly without bypassing that admission rule.
        let db = rusqlite::Connection::open(p.state.join("journal/uploads.db"))
            .expect("synthetic journal database");
        assert_eq!(db.execute(
            "UPDATE namespace_objects SET body=json_set(body,'$.unlinked',json('true')) WHERE id=?1",
            [owner.to_string()],
        ).expect("mark synthetic owner unlinked"), 1);
        assert!(p.parent(&parent.id).await.is_err());
    }

    #[tokio::test]
    async fn confirmed_mkdir_parent_rejects_receipt_identity_or_sequence_drift() {
        for sequence_drift in [false, true] {
            let (_temp, p) = fixture();
            let (owner, parent) = confirmed_unindexed_directory(&p);
            assert_eq!(
                p.parent(&parent.id)
                    .await
                    .expect("valid receipt before drift"),
                parent
            );
            let db = rusqlite::Connection::open(p.state.join("journal/uploads.db"))
                .expect("synthetic journal database");
            let changed = if sequence_drift {
                db.execute(
                    "UPDATE namespace_objects SET body=json_set(body,'$.remote_sequence',json_extract(body,'$.remote_sequence')+1) WHERE id=?1",
                    [owner.to_string()],
                ).expect("drift synthetic owner sequence")
            } else {
                let latest = p
                    .journal
                    .lock()
                    .expect("journal lock")
                    .namespace_object(owner)
                    .expect("owner")
                    .latest
                    .expect("mkdir operation");
                db.execute(
                    "UPDATE mutations SET body=json_set(body,'$.receipt.value.id',?2) WHERE id=?1",
                    rusqlite::params![
                        latest.to_string(),
                        "FOLDER::com.apple.CloudDocs::different-receipt"
                    ],
                )
                .expect("drift synthetic receipt identity")
            };
            assert_eq!(changed, 1);
            assert!(p.parent(&parent.id).await.is_err());
        }
    }

    #[tokio::test]
    async fn confirmed_mkdir_parent_rejects_later_queued_rename() {
        let (_temp, p) = fixture();
        let (owner, parent) = confirmed_unindexed_directory(&p);
        assert_eq!(
            p.parent(&parent.id)
                .await
                .expect("mkdir authority before rename"),
            parent
        );
        {
            let mut journal = p.journal.lock().expect("journal lock");
            let object = journal.namespace_object(owner).expect("directory owner");
            let rename = journal
                .relocate_namespace_item(
                    owner,
                    object.revision,
                    ROOT_ID.into(),
                    "Renamed parent".into(),
                )
                .expect("queue later rename");
            assert_eq!(rename.state, crate::journal::MutationState::Pending);
            assert_eq!(
                journal
                    .namespace_object(owner)
                    .expect("renamed owner")
                    .latest,
                Some(rename.id)
            );
        }
        assert!(p.parent(&parent.id).await.is_err());
    }

    #[tokio::test]
    async fn confirmed_mkdir_parent_requires_known_non_tombstoned_grandparent() {
        let (_temp, p) = fixture();
        let grandparent = folder(ROOT_ID);
        let (_, parent) = confirmed_unindexed_directory_at(&p, &grandparent.id);
        assert!(p.parent(&parent.id).await.is_err());
        let mut store = Store::open(&p.metadata).expect("metadata store");
        store
            .observe_node(&p.scope, &grandparent)
            .expect("publish grandparent");
        assert_eq!(
            p.parent(&parent.id)
                .await
                .expect("indexed ancestor authorizes leaf"),
            parent
        );
        let ticket = store
            .node_observation(&p.scope, &grandparent.id)
            .expect("grandparent absence ticket");
        store
            .publish_absence(&ticket)
            .expect("publish absent grandparent");
        assert!(p.parent(&parent.id).await.is_err());
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
