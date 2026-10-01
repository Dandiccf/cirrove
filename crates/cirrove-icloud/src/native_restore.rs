//! Native restore coordinator, not yet enabled by the service. Destination
//! vacancy is checked but Apple offers no proven atomic no-overwrite condition.
//! Callers must also bind a durable Applied Trash receipt before admission.
use crate::native_trash::Session;
use crate::sealed_session::SealedNativeRestoreCheckpointVault;
use crate::{ICloudNativeTrash, ICloudReadSession, PackageSemanticIdentity};
use cirrove_auth::CredentialVault;
use cirrove_core::mutation::{MutationError, MutationReceipt, MutationReconciliation, Result};
use cirrove_core::{CancellationToken, Node, NodeKind, Scope};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::Mutex;
use uuid::Uuid;
const LIMIT: usize = 96 * 1024;
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeRestoreRequest {
    pub scope: Scope,
    pub removal_operation: Uuid,
    pub before: Node,
    /// Original active route including CloudDocs root and exact destination.
    /// Maximum 33 nodes. No shortcuts, packages, or app containers.
    pub parent_route: Vec<Node>,
}
impl NativeRestoreRequest {
    fn validate(&self) -> Result<()> {
        ICloudNativeTrash::identity(&self.scope, &self.before)?;
        if self.parent_route.is_empty() || self.parent_route.len() > 33 {
            return Err(MutationError::Invalid);
        }
        for (index, node) in self.parent_route.iter().enumerate() {
            let suffix = node
                .id
                .strip_prefix("FOLDER::com.apple.CloudDocs::")
                .ok_or(MutationError::Invalid)?;
            if suffix.is_empty()
                || node.id.len() > 512
                || suffix == "TRASH_ROOT"
                || suffix
                    .chars()
                    .any(|c| c.is_control() || matches!(c, '/' | ':' | '\\'))
            {
                return Err(MutationError::Invalid);
            }
            if node.kind != NodeKind::Folder
                || node.package
                || node.target.is_some()
                || !node.id.starts_with("FOLDER::com.apple.CloudDocs::")
            {
                return Err(MutationError::Invalid);
            }
            if index == 0 {
                if node.id != crate::ROOT_ID || node.parent_id.is_some() {
                    return Err(MutationError::Invalid);
                }
            } else if node.parent_id.as_deref() != Some(&self.parent_route[index - 1].id)
                || node.name.is_empty()
                || node.name.len() > 255
                || node.name.chars().any(|c| c.is_control() || c == '/')
                || matches!(node.name.as_str(), "." | "..")
                || !node.etag.as_deref().is_some_and(revision)
            {
                return Err(MutationError::Invalid);
            }
            if self.parent_route[..index]
                .iter()
                .any(|old| old.id == node.id)
            {
                return Err(MutationError::Invalid);
            }
        }
        if self.before.parent_id.as_ref() != self.parent_route.last().map(|n| &n.id) {
            return Err(MutationError::Invalid);
        }
        Ok(())
    }
    fn restore_path(&self) -> String {
        self.parent_route
            .iter()
            .skip(1)
            .map(|n| n.name.as_str())
            .chain(std::iter::once(self.before.name.as_str()))
            .collect::<Vec<_>>()
            .join("/")
    }
}
fn revision(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4096
        && !value.chars().any(char::is_control)
        && !value.contains('*')
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Prepared,
    MayHaveSent,
    ReceiptObserved,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint {
    version: u8,
    operation: Uuid,
    request: NativeRestoreRequest,
    account_hash: String,
    semantic: PackageSemanticIdentity,
    trash_etag: String,
    inventory_sha256: String,
    phase: Phase,
    receipt_etag: Option<String>,
}
pub struct ICloudNativeRestore {
    request: NativeRestoreRequest,
    apple_id: String,
    source: ICloudNativeTrash,
    session: Mutex<Session>,
    checkpoint: Arc<dyn CredentialVault>,
    operation_lock: Mutex<()>,
}
impl ICloudNativeRestore {
    pub fn from_sealed_session(
        apple_id: String,
        credential_id: String,
        state: &Path,
        request: NativeRestoreRequest,
        staging: PathBuf,
    ) -> Result<Self> {
        request.validate()?;
        let source = ICloudNativeTrash::from_sealed_session(
            request.scope.clone(),
            apple_id.clone(),
            credential_id.clone(),
            state,
            request.before.clone(),
            staging.clone(),
        )?;
        let vault = crate::SealedSessionVault::new(state, &request.scope.account)
            .map_err(|_| MutationError::Invalid)?;
        let checkpoint = SealedNativeRestoreCheckpointVault::new(state, &request.scope.account)
            .map_err(|_| MutationError::Invalid)?;
        Ok(Self {
            request,
            apple_id: apple_id.clone(),
            source,
            session: Mutex::new(Session::Vault {
                apple_id,
                credential_id,
                vault: Arc::new(vault),
            }),
            checkpoint: Arc::new(checkpoint),
            operation_lock: Mutex::new(()),
        })
    }
    fn key(&self, id: Uuid) -> String {
        SealedNativeRestoreCheckpointVault::key(&self.request.scope.account, id)
    }
    async fn load(&self, id: Uuid) -> Result<Option<Checkpoint>> {
        let Some(secret) = self
            .checkpoint
            .load(&self.key(id))
            .await
            .map_err(|_| MutationError::Uncertain)?
        else {
            return Ok(None);
        };
        if secret.expose_secret().len() > LIMIT {
            return Err(MutationError::Invalid);
        }
        let saved: Checkpoint =
            serde_json::from_str(secret.expose_secret()).map_err(|_| MutationError::Invalid)?;
        if saved.version != 1
            || saved.operation != id
            || saved.request != self.request
            || saved.account_hash
                != crate::account_hash(&self.apple_id).map_err(crate::mutation_error)?
            || saved.semantic.validate().is_err()
            || !revision(&saved.trash_etag)
            || saved.inventory_sha256.len() != 64
            || !saved
                .inventory_sha256
                .bytes()
                .all(|b| b.is_ascii_hexdigit())
            || !(if saved.phase == Phase::ReceiptObserved {
                saved.receipt_etag.as_deref().is_some_and(revision)
            } else {
                saved.receipt_etag.is_none()
            })
        {
            return Err(MutationError::Invalid);
        }
        Ok(Some(saved))
    }
    async fn save(&self, saved: &Checkpoint) -> Result<()> {
        let encoded = serde_json::to_string(saved).map_err(|_| MutationError::Invalid)?;
        if encoded.len() > LIMIT {
            return Err(MutationError::Invalid);
        }
        self.checkpoint
            .save(&self.key(saved.operation), SecretString::from(encoded))
            .await
            .map_err(|_| MutationError::Uncertain)
    }
    async fn session<'a>(&self, state: &'a mut Session) -> Result<&'a mut ICloudReadSession> {
        let session = ICloudNativeTrash::active(state).await?;
        if session.account_hash.as_deref()
            != Some(
                crate::account_hash(&self.apple_id)
                    .map_err(crate::mutation_error)?
                    .as_str(),
            )
        {
            return Err(MutationError::Invalid);
        }
        Ok(session)
    }
    /// Local persistence and read-only verification only; never sends restore.
    pub async fn prepare(&self, operation: Uuid, cancel: &CancellationToken) -> Result<()> {
        self.request.validate()?;
        tokio::select! {biased;_=cancel.cancelled()=>Err(MutationError::Provider(cirrove_core::ProviderError::Cancelled)), result=tokio::time::timeout(crate::VERIFICATION_TRANSFER_TIMEOUT,async{
            let _lock=self.operation_lock.lock().await;
            if self.load(operation).await?.is_some(){return Err(MutationError::Uncertain)}
            let semantic=self.source.restore_semantic(self.request.removal_operation).await?;
            let mut state=self.session.lock().await;let session=self.session(&mut state).await?;
            let (trash_etag,inventory)=self.preflight(session,&semantic,cancel).await?;
            self.save(&Checkpoint{version:1,operation,request:self.request.clone(),account_hash:crate::account_hash(&self.apple_id).map_err(crate::mutation_error)?,semantic,trash_etag,inventory_sha256:inventory,phase:Phase::Prepared,receipt_etag:None}).await
        })=>result.map_err(|_|MutationError::Uncertain)?}
    }
    /// Only a Prepared checkpoint can send. Lost response/cancellation after the
    /// durable arm leaves this operation inspect-only, never a second dispatch.
    pub async fn execute(&self, operation: Uuid, cancel: &CancellationToken) -> Result<Node> {
        tokio::select! {biased;_=cancel.cancelled()=>Err(MutationError::Provider(cirrove_core::ProviderError::Cancelled)),result=tokio::time::timeout(crate::VERIFICATION_TRANSFER_TIMEOUT,async{
            let _lock=self.operation_lock.lock().await;
            let mut saved=self.load(operation).await?.ok_or(MutationError::Uncertain)?;
            if saved.phase!=Phase::Prepared{return Err(MutationError::Uncertain)}
            if self.source.restore_semantic(self.request.removal_operation).await?!=saved.semantic{return Err(MutationError::Invalid)}
            let mut state=self.session.lock().await;let session=self.session(&mut state).await?;
            let (etag,inventory)=self.preflight(session,&saved.semantic,cancel).await?;
            if etag!=saved.trash_etag||inventory!=saved.inventory_sha256{return Err(MutationError::Conflict)}
            saved.phase=Phase::MayHaveSent;self.save(&saved).await?;
            if cancel.is_cancelled(){return Err(MutationError::Uncertain)}
            let receipt=session.send_probe_restore(&self.request.before.id,self.document_id()?,&saved.trash_etag,self.parent_id()?).await.map_err(crate::mutation_error)?;
            if !(200..300).contains(&receipt.http_status){return Err(MutationError::Uncertain)}
            saved.receipt_etag=Some(receipt.etag);saved.phase=Phase::ReceiptObserved;self.save(&saved).await?;
            self.verify_active(session,&saved,cancel).await
        })=>result.map_err(|_|MutationError::Uncertain)?}
    }
    /// Observation only, including a missing reply. Never prepares or dispatches.
    pub async fn reconcile(
        &self,
        operation: Uuid,
        cancel: &CancellationToken,
    ) -> Result<MutationReconciliation> {
        tokio::select! {biased;_=cancel.cancelled()=>Err(MutationError::Provider(cirrove_core::ProviderError::Cancelled)), result=tokio::time::timeout(crate::VERIFICATION_TRANSFER_TIMEOUT,async{
            let _lock=self.operation_lock.lock().await;
            let Some(saved)=self.load(operation).await?else{return Ok(MutationReconciliation::Indeterminate)};
            if self.source.restore_semantic(self.request.removal_operation).await?!=saved.semantic{return Err(MutationError::Invalid)}
            if saved.phase==Phase::Prepared{return Ok(MutationReconciliation::Uncommitted)}
            let mut state=self.session.lock().await;let session=self.session(&mut state).await?;
            match self.verify_active(session,&saved,cancel).await {
                Ok(node)=>Ok(MutationReconciliation::Applied(MutationReceipt::Upsert(node))),
                Err(error @ MutationError::Provider(_)) | Err(error @ MutationError::InsufficientStorage)=>Err(error),
                Err(_)=>Ok(MutationReconciliation::Indeterminate),
            }
        })=>result.map_err(|_|MutationError::Uncertain)?}
    }
    fn parent_id(&self) -> Result<&str> {
        self.request
            .before
            .parent_id
            .as_deref()
            .ok_or(MutationError::Invalid)
    }
    fn document_id(&self) -> Result<&str> {
        self.request
            .before
            .id
            .strip_prefix("FILE::com.apple.CloudDocs::")
            .ok_or(MutationError::Invalid)
    }
    async fn parent_inventory(
        &self,
        session: &mut ICloudReadSession,
        restored: Option<&crate::DriveEntry>,
    ) -> Result<String> {
        let mut entries = Vec::new();
        for (index, node) in self.request.parent_route.iter().enumerate() {
            let observed = session
                .active_folder_metadata(&node.id)
                .await
                .map_err(crate::mutation_error)?;
            if observed.drivewsid != node.id
                || observed.kind != "FOLDER"
                || (index > 0
                    && (observed.zone != "com.apple.CloudDocs"
                        || observed.parent_id != self.request.parent_route[index - 1].id
                        || observed.display_name() != node.name
                        || (restored.is_none() && Some(&observed.etag) != node.etag.as_ref())))
            {
                return Err(MutationError::Conflict);
            }
            entries = observed.items;
        }
        if entries.len() > 10000 {
            return Err(MutationError::Invalid);
        }
        if let Some(target) = restored {
            if entries
                .iter()
                .filter(|e| e.drivewsid == target.drivewsid)
                .count()
                != 1
            {
                return Err(MutationError::Uncertain);
            }
            let listed = entries
                .iter()
                .find(|e| e.drivewsid == target.drivewsid)
                .ok_or(MutationError::Uncertain)?;
            if identity_fields(listed) != identity_fields(target) {
                return Err(MutationError::Conflict);
            }
            entries.retain(|e| e.drivewsid != target.drivewsid);
        } else if entries.iter().any(|e| {
            e.drivewsid == self.request.before.id
                || e.display_name()
                    .eq_ignore_ascii_case(&self.request.before.name)
        }) {
            return Err(MutationError::Conflict);
        }
        let mut inventory: Vec<_> = entries.iter().map(identity_fields).collect();
        inventory.sort();
        Ok(hex::encode(Sha256::digest(
            serde_json::to_vec(&inventory).map_err(|_| MutationError::Invalid)?,
        )))
    }
    fn trash_request(
        &self,
        semantic: &PackageSemanticIdentity,
    ) -> Result<crate::package_trash::OwnedPackageTrashRequest> {
        Ok(crate::package_trash::OwnedPackageTrashRequest {
            apple_account: self.apple_id.clone(),
            drive_id: self.request.before.id.clone(),
            document_id: self.document_id()?.into(),
            expected_root: self.request.before.name.clone(),
            semantic: semantic.clone(),
        })
    }
    async fn preflight(
        &self,
        session: &mut ICloudReadSession,
        semantic: &PackageSemanticIdentity,
        cancel: &CancellationToken,
    ) -> Result<(String, String)> {
        let item = session
            .item_details(&self.request.before.id)
            .await
            .map_err(crate::mutation_error)?;
        if item.get("restorePath").and_then(serde_json::Value::as_str)
            != Some(self.request.restore_path().as_str())
            || item.get("zone").and_then(serde_json::Value::as_str) != Some("com.apple.CloudDocs")
        {
            return Err(MutationError::Conflict);
        }
        let binding = crate::package_trash::observation(&item, &self.trash_request(semantic)?)
            .map_err(crate::mutation_error)?;
        let inventory = self.parent_inventory(session, None).await?;
        let proof = session
            .verify_owned_package_in_trash(
                self.trash_request(semantic)?,
                self.source.staging()?,
                cancel,
            )
            .await
            .map_err(crate::mutation_error)?;
        let after = session
            .item_details(&self.request.before.id)
            .await
            .map_err(crate::mutation_error)?;
        if after.get("zone") != item.get("zone")
            || crate::package_trash::observation(&after, &self.trash_request(semantic)?)
                .map_err(crate::mutation_error)?
                != binding
            || after.get("etag").and_then(serde_json::Value::as_str) != Some(&proof.trash_etag)
            || self.parent_inventory(session, None).await? != inventory
        {
            return Err(MutationError::Conflict);
        }
        Ok((proof.trash_etag, inventory))
    }
    async fn verify_active(
        &self,
        session: &mut ICloudReadSession,
        saved: &Checkpoint,
        cancel: &CancellationToken,
    ) -> Result<Node> {
        let item = session
            .item_details(&self.request.before.id)
            .await
            .map_err(crate::mutation_error)?;
        if item.get("restorePath").is_some_and(|v| !v.is_null()) {
            return Err(MutationError::Uncertain);
        }
        let before: crate::DriveEntry =
            serde_json::from_value(item).map_err(|_| MutationError::Uncertain)?;
        if before.drivewsid != self.request.before.id
            || before.docwsid != self.document_id()?
            || before.zone != "com.apple.CloudDocs"
            || before.kind != "FILE"
            || before.parent_id != self.parent_id()?
            || !revision(&before.etag)
            || saved
                .receipt_etag
                .as_ref()
                .is_some_and(|v| v != &before.etag)
            || !before.display_name().ends_with(".pages")
        {
            return Err(MutationError::Conflict);
        }
        if self.parent_inventory(session, Some(&before)).await? != saved.inventory_sha256 {
            return Err(MutationError::Conflict);
        }
        let file = self.source.staging()?;
        let mut sink = Sink(tokio::fs::File::from_std(
            file.try_clone().map_err(|_| MutationError::Uncertain)?,
        ));
        let receipt = session
            .download_package(
                self.parent_id()?,
                &before,
                64 * 1024 * 1024,
                &mut sink,
                cancel,
            )
            .await
            .map_err(crate::mutation_error)?;
        tokio::io::AsyncWriteExt::flush(&mut sink.0)
            .await
            .map_err(|_| MutationError::Uncertain)?;
        drop(sink);
        let version = saved.semantic.version;
        let root = before.display_name();
        let token = cancel.clone();
        let semantic = tokio::task::spawn_blocking(move || {
            crate::package_archive_semantic_identity_versioned(
                &file, &receipt, &root, version, &token,
            )
        })
        .await
        .map_err(|_| MutationError::Uncertain)??;
        if semantic != saved.semantic {
            return Err(MutationError::Conflict);
        }
        let value = session
            .item_details(&before.drivewsid)
            .await
            .map_err(crate::mutation_error)?;
        if value.get("restorePath").is_some_and(|v| !v.is_null()) {
            return Err(MutationError::Conflict);
        }
        let after: crate::DriveEntry =
            serde_json::from_value(value).map_err(|_| MutationError::Uncertain)?;
        if after != before
            || self.parent_inventory(session, Some(&before)).await? != saved.inventory_sha256
        {
            return Err(MutationError::Conflict);
        }
        if cancel.is_cancelled() {
            return Err(cirrove_core::ProviderError::Cancelled.into());
        }
        let name = before.display_name();
        Ok(Node {
            id: before.drivewsid,
            parent_id: Some(before.parent_id),
            name,
            kind: NodeKind::Folder,
            size: before.size,
            modified_unix: 0,
            etag: Some(before.etag),
            content_version: None,
            target: None,
            package: true,
        })
    }
}
fn identity_fields(entry: &crate::DriveEntry) -> String {
    serde_json::json!([
        entry.drivewsid,
        entry.docwsid,
        entry.zone,
        entry.kind,
        entry.parent_id,
        entry.name,
        entry.extension,
        entry.etag,
        entry.size
    ])
    .to_string()
}
struct Sink(tokio::fs::File);
#[async_trait::async_trait]
impl cirrove_core::reads::ReadWindowSink for Sink {
    async fn write_chunk(
        &mut self,
        bytes: &[u8],
    ) -> std::result::Result<(), cirrove_core::ProviderError> {
        tokio::io::AsyncWriteExt::write_all(&mut self.0, bytes)
            .await
            .map_err(|_| {
                cirrove_core::ProviderError::Protocol("native restore staging unavailable")
            })
    }
}
#[cfg(test)]
mod tests;
