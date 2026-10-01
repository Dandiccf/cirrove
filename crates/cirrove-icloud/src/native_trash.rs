//! Journal-bound recoverable removal of an exact Pages PACKAGE revision.
//! This adapter is not a filesystem/public admission API. Possibly dispatched
//! operations are inspect-only forever; absence never proves removal.
use crate::{ICloudReadSession, PackageSemanticIdentity, SealedSessionVault};
use async_trait::async_trait;
use cirrove_auth::CredentialVault;
use cirrove_core::mutation::{
    DeletionSupport, MutationError, MutationIntent, MutationProvider, MutationReceipt,
    MutationReconciliation, MutationRequest, Result,
};
use cirrove_core::{
    CancellationToken, Node, NodeKind, ProviderError, Scope, reads::ReadWindowSink,
};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::{io::AsyncWriteExt, sync::Mutex};
use uuid::Uuid;
const LIMIT: usize = 96 * 1024;
const ARCHIVE_LIMIT: u64 = 64 * 1024 * 1024;
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Prepared,
    MayHaveSent,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint {
    version: u8,
    operation: Uuid,
    request: MutationRequest,
    account_hash: String,
    semantic: PackageSemanticIdentity,
    phase: Phase,
}
enum Session {
    Ready(Box<ICloudReadSession>),
    Vault {
        apple_id: String,
        credential_id: String,
        vault: Arc<dyn CredentialVault>,
    },
}
/// An internal, exact-operation adapter. The service must admit the explicit
/// native-document intent; ordinary unlink/rmdir do not acquire this capability.
pub struct ICloudNativeTrash {
    scope: Scope,
    before: Node,
    apple_id: String,
    staging: PathBuf,
    session: Mutex<Session>,
    checkpoint: Arc<dyn CredentialVault>,
    operation_lock: Mutex<()>,
}
impl ICloudNativeTrash {
    pub fn from_sealed_session(
        scope: Scope,
        apple_id: String,
        credential_id: String,
        state: &Path,
        before: Node,
        staging: PathBuf,
    ) -> Result<Self> {
        Self::identity(&scope, &before)?;
        if apple_id.trim().is_empty() || Uuid::parse_str(&credential_id).is_err() {
            return Err(MutationError::Invalid);
        }
        let session =
            SealedSessionVault::new(state, &scope.account).map_err(|_| MutationError::Invalid)?;
        let checkpoint = crate::SealedNativeTrashCheckpointVault::new(state, &scope.account)
            .map_err(|_| MutationError::Invalid)?;
        Ok(Self {
            scope,
            before,
            apple_id: apple_id.clone(),
            staging,
            session: Mutex::new(Session::Vault {
                apple_id,
                credential_id,
                vault: Arc::new(session),
            }),
            checkpoint: Arc::new(checkpoint),
            operation_lock: Mutex::new(()),
        })
    }
    fn identity(scope: &Scope, before: &Node) -> Result<()> {
        let id = before.id.strip_prefix("FILE::com.apple.CloudDocs::");
        let parent = before
            .parent_id
            .as_deref()
            .and_then(|v| v.strip_prefix("FOLDER::com.apple.CloudDocs::"));
        if Uuid::parse_str(&scope.account).is_err()
            || scope.provider != "icloud"
            || scope.collection != "drive"
            || before.kind != NodeKind::Folder
            || !before.package
            || before.target.is_some()
            || id.is_none_or(|v| v.is_empty() || v.len() > 4096 || v.contains(['/', '\0', ':']))
            || parent.is_none_or(|v| {
                v.is_empty() || v == "TRASH_ROOT" || v.len() > 4096 || v.contains(['/', '\0', ':'])
            })
            || before.name.len() > 255
            || !before.name.ends_with(".pages")
            || before.name.contains(['/', '\0'])
            || before.etag.as_deref().is_none_or(|v| {
                v.is_empty() || v.len() > 4096 || v.contains(['\0', '\r', '\n', '*'])
            })
            || before
                .content_version
                .as_ref()
                .is_some_and(|v| Some(v) != before.etag.as_ref())
        {
            return Err(MutationError::Invalid);
        }
        Ok(())
    }
    fn binding(
        &self,
        operation: &str,
        request: &MutationRequest,
        prepared: Option<&str>,
    ) -> Result<Uuid> {
        request.validate()?;
        Self::identity(&request.scope, &self.before)?;
        if request.scope != self.scope
            || !matches!(&request.intent, MutationIntent::TrashNativeDocument{before} if before==&self.before)
            || prepared.is_some_and(|v| v != self.before.id)
        {
            return Err(MutationError::Invalid);
        }
        Uuid::parse_str(operation).map_err(|_| MutationError::Invalid)
    }
    async fn load(&self, operation: Uuid, request: &MutationRequest) -> Result<Option<Checkpoint>> {
        let key = crate::SealedNativeTrashCheckpointVault::key(&self.scope.account, operation);
        let Some(value) = self
            .checkpoint
            .load(&key)
            .await
            .map_err(|_| MutationError::Uncertain)?
        else {
            return Ok(None);
        };
        if value.expose_secret().len() > LIMIT {
            return Err(MutationError::Invalid);
        }
        let saved: Checkpoint =
            serde_json::from_str(value.expose_secret()).map_err(|_| MutationError::Invalid)?;
        if saved.version != 1
            || saved.operation != operation
            || &saved.request != request
            || saved.account_hash
                != crate::account_hash(&self.apple_id).map_err(crate::mutation_error)?
            || saved.semantic.validate().is_err()
        {
            return Err(MutationError::Invalid);
        }
        Ok(Some(saved))
    }
    async fn save(&self, saved: &Checkpoint) -> Result<()> {
        let value = serde_json::to_string(saved).map_err(|_| MutationError::Invalid)?;
        if value.len() > LIMIT {
            return Err(MutationError::Invalid);
        }
        self.checkpoint
            .save(
                &crate::SealedNativeTrashCheckpointVault::key(&self.scope.account, saved.operation),
                SecretString::from(value),
            )
            .await
            .map_err(|_| MutationError::Uncertain)
    }
    async fn active(state: &mut Session) -> Result<&mut ICloudReadSession> {
        if let Session::Vault {
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
            *state = Session::Ready(Box::new(restored));
        }
        match state {
            Session::Ready(session) => Ok(session),
            _ => Err(MutationError::Uncertain),
        }
    }
    fn staging(&self) -> Result<File> {
        let meta =
            std::fs::symlink_metadata(&self.staging).map_err(|_| MutationError::Uncertain)?;
        let uid = std::fs::metadata("/proc/self")
            .map_err(|_| MutationError::Uncertain)?
            .uid();
        if !self.staging.is_absolute()
            || !meta.is_dir()
            || meta.uid() != uid
            || meta.permissions().mode() & 0o077 != 0
        {
            return Err(MutationError::Invalid);
        }
        let file = tempfile::tempfile_in(&self.staging).map_err(|_| MutationError::Uncertain)?;
        // tempfile's anonymous file follows the process umask. Establish the
        // private-file contract before any cloud content is written into it.
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|_| MutationError::Uncertain)?;
        Ok(file)
    }
    fn observed(&self, value: &serde_json::Value) -> Result<()> {
        let entry: crate::DriveEntry =
            serde_json::from_value(value.clone()).map_err(|_| MutationError::Conflict)?;
        if entry.drivewsid != self.before.id
            || entry.docwsid
                != self
                    .before
                    .id
                    .rsplit("::")
                    .next()
                    .ok_or(MutationError::Invalid)?
            || entry.kind != "FILE"
            || entry.zone != "com.apple.CloudDocs"
            || Some(&entry.parent_id) != self.before.parent_id.as_ref()
            || entry.display_name() != self.before.name
            || Some(&entry.etag) != self.before.etag.as_ref()
            || entry.size != self.before.size
            || value.get("restorePath").is_some_and(|v| !v.is_null())
        {
            return Err(MutationError::Conflict);
        }
        Ok(())
    }
    async fn capture(
        &self,
        session: &mut ICloudReadSession,
        cancel: &CancellationToken,
    ) -> Result<PackageSemanticIdentity> {
        if session.account_hash.as_deref()
            != Some(
                crate::account_hash(&self.apple_id)
                    .map_err(crate::mutation_error)?
                    .as_str(),
            )
        {
            return Err(MutationError::Invalid);
        }
        self.observed(
            &session
                .item_details(&self.before.id)
                .await
                .map_err(crate::mutation_error)?,
        )?;
        let crate::ContentRepresentation::Package(url) = session
            .download_representation(&self.before.id)
            .await
            .map_err(crate::mutation_error)?
        else {
            return Err(MutationError::Conflict);
        };
        check(cancel)?;
        let file = self.staging()?;
        let mut sink = Sink(tokio::fs::File::from_std(
            file.try_clone().map_err(|_| MutationError::Uncertain)?,
        ));
        let response = session
            .http
            .get(url)
            .header(reqwest::header::ACCEPT_ENCODING, "identity")
            .timeout(crate::VERIFICATION_TRANSFER_TIMEOUT)
            .send()
            .await
            .map_err(|_| MutationError::Uncertain)?;
        let archive =
            crate::package_download::stage_response(response, &mut sink, ARCHIVE_LIMIT, cancel)
                .await
                .map_err(crate::mutation_error)?;
        sink.0.flush().await.map_err(|_| MutationError::Uncertain)?;
        drop(sink);
        self.observed(
            &session
                .item_details(&self.before.id)
                .await
                .map_err(crate::mutation_error)?,
        )?;
        let root = self.before.name.clone();
        let token = cancel.clone();
        let semantic = tokio::task::spawn_blocking(move || {
            crate::package_archive_semantic_identity(&file, &archive, &root, &token)
        })
        .await
        .map_err(|_| MutationError::Uncertain)??;
        check(cancel)?;
        Ok(semantic)
    }
    async fn recover(
        &self,
        session: &mut ICloudReadSession,
        saved: &Checkpoint,
        cancel: &CancellationToken,
    ) -> Result<MutationReceipt> {
        check(cancel)?;
        session
            .verify_owned_package_in_trash(
                crate::package_trash::OwnedPackageTrashRequest {
                    apple_account: self.apple_id.clone(),
                    drive_id: self.before.id.clone(),
                    document_id: self
                        .before
                        .id
                        .rsplit("::")
                        .next()
                        .ok_or(MutationError::Invalid)?
                        .into(),
                    expected_root: self.before.name.clone(),
                    semantic: saved.semantic.clone(),
                },
                self.staging()?,
                cancel,
            )
            .await
            .map_err(crate::mutation_error)?;
        check(cancel)?;
        Ok(MutationReceipt::Removed {
            item: self.before.id.clone(),
        })
    }
}
fn check(cancel: &CancellationToken) -> Result<()> {
    if cancel.is_cancelled() {
        Err(ProviderError::Cancelled.into())
    } else {
        Ok(())
    }
}
struct Sink(tokio::fs::File);
#[async_trait]
impl ReadWindowSink for Sink {
    async fn write_chunk(&mut self, bytes: &[u8]) -> std::result::Result<(), ProviderError> {
        self.0
            .write_all(bytes)
            .await
            .map_err(|_| ProviderError::Protocol("native package staging unavailable"))
    }
}
#[async_trait]
impl MutationProvider for ICloudNativeTrash {
    fn deletion(&self) -> DeletionSupport {
        DeletionSupport {
            recycle_bin: true,
            permanent: false,
        }
    }
    async fn prepare_mutation_for_operation(
        &self,
        operation: &str,
        request: &MutationRequest,
        cancel: &CancellationToken,
    ) -> Result<Option<String>> {
        let operation = self.binding(operation, request, None)?;
        let _guard = self.operation_lock.lock().await;
        tokio::select! { biased; _=cancel.cancelled()=>Err(ProviderError::Cancelled.into()), result=async {
            if let Some(saved)=self.load(operation,request).await? {
                return if saved.phase==Phase::Prepared {Ok(Some(self.before.id.clone()))} else {Err(MutationError::Uncertain)};
            }
            let mut state=self.session.lock().await; let session=Self::active(&mut state).await?;
            check(cancel)?;
            let semantic=self.capture(session,cancel).await?;
            self.save(&Checkpoint {version:1,operation,request:request.clone(),account_hash:crate::account_hash(&self.apple_id).map_err(crate::mutation_error)?,semantic,phase:Phase::Prepared}).await?;
            check(cancel)?;
            Ok(Some(self.before.id.clone()))
        }=>result }
    }
    async fn mutate(&self, _: &MutationRequest, _: &CancellationToken) -> Result<MutationReceipt> {
        Err(MutationError::Unsupported(
            "durable native Trash operation required",
        ))
    }
    async fn mutate_operation(
        &self,
        operation: &str,
        request: &MutationRequest,
        prepared: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<MutationReceipt> {
        let operation = self.binding(operation, request, prepared)?;
        if prepared != Some(self.before.id.as_str()) {
            return Err(MutationError::Invalid);
        }
        let _guard = self.operation_lock.lock().await;
        tokio::select! { biased; _=cancel.cancelled()=>Err(ProviderError::Cancelled.into()), result=async {
            let mut saved=self.load(operation,request).await?.ok_or(MutationError::Uncertain)?;
            if saved.phase!=Phase::Prepared {return Err(MutationError::Uncertain);}
            let mut state=self.session.lock().await; let session=Self::active(&mut state).await?;
            check(cancel)?;
            if self.capture(session,cancel).await?!=saved.semantic {return Err(MutationError::Conflict);}
            saved.phase=Phase::MayHaveSent;
            self.save(&saved).await?;
            check(cancel)?;
            if !session.send_trash(&self.before.id,self.before.etag.as_deref().ok_or(MutationError::Invalid)?)
                .await.map_err(crate::mutation_error)? { return Err(MutationError::Conflict); }
            self.recover(session,&saved,cancel).await
        }=>result }
    }
    async fn reconcile_mutation(
        &self,
        _: &MutationRequest,
        _: &CancellationToken,
    ) -> Result<MutationReconciliation> {
        Ok(MutationReconciliation::Indeterminate)
    }
    async fn reconcile_operation(
        &self,
        operation: &str,
        request: &MutationRequest,
        prepared: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<MutationReconciliation> {
        let operation = self.binding(operation, request, prepared)?;
        let _guard = self.operation_lock.lock().await;
        tokio::select! { biased; _=cancel.cancelled()=>Err(ProviderError::Cancelled.into()), result=async {
            let Some(saved)=self.load(operation,request).await? else {return Ok(MutationReconciliation::Indeterminate);};
            if saved.phase==Phase::Prepared {return Ok(MutationReconciliation::Uncommitted);}
            let mut state=self.session.lock().await; let session=Self::active(&mut state).await?;
            check(cancel)?;
            match self.recover(session,&saved,cancel).await {
                Ok(receipt)=>Ok(MutationReconciliation::Applied(receipt)),
                Err(error @ MutationError::Provider(_)) | Err(error @ MutationError::InsufficientStorage)=>Err(error),
                Err(_)=>Ok(MutationReconciliation::Indeterminate),
            }
        }=>result }
    }
}
#[cfg(test)]
mod tests;
