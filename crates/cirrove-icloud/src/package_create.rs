//! Native package creation through the shared durable upload worker.
//! No mount routing and no package replacement. Recovery only observes exact IDs.
use crate::file_create::{map_content_error, map_session_error};
use crate::{
    ICloudReadSession, ICloudWriteStagingBudget, ROOT_ID, SealedSessionVault,
    package_transport as wire, write_staging::WriteStagingFile,
};
use async_trait::async_trait;
use cirrove_auth::CredentialVault;
use cirrove_core::upload::{
    PackageUploadReceipt, Reconciliation, Result, UploadError, UploadIntent, UploadProvider,
    UploadRepresentation, UploadRequest, UploadStep,
};
use cirrove_core::{
    CancellationToken, Node, NodeKind, ProviderError, Scope, reads::ReadWindowSink,
};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::Mutex;
use uuid::Uuid;
const MAX_CHECKPOINT: usize = 96 * 1024;
const MAX_ARCHIVE: u64 = 64 * 1024 * 1024;
// These persisted names emphasize uncertainty: none proves remote completion.
#[allow(clippy::enum_variant_names)]
#[derive(Serialize, Deserialize, PartialEq, Eq)]
enum Phase {
    AllocationArmed,
    BodyArmed,
    RegistrationArmed,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint {
    version: u8,
    operation: String,
    request: UploadRequest,
    account_hash: String,
    parent: Node,
    phase: Phase,
    slot: Option<wire::Slot>,
    registration: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wire: Option<crate::package_wire::WireReceipt>,
}
#[derive(Default)]
struct VerificationProgress {
    fence: &'static str,
    #[cfg(feature = "write-probe")]
    retain_download: bool,
    #[cfg(feature = "write-probe")]
    download: Option<(Arc<WriteStagingFile>, crate::PackageDownload)>,
}
enum Session {
    Ready(Box<ICloudReadSession>),
    Sealed {
        apple_id: String,
        credential_id: String,
        vault: Arc<dyn CredentialVault>,
    },
}
pub struct ICloudPackageCreate {
    scope: Scope,
    parent: Node,
    session: Mutex<Session>,
    staging: PathBuf,
    pub(crate) staging_budget: ICloudWriteStagingBudget,
    #[cfg(test)]
    body_dispatch_probe: Option<Arc<std::sync::atomic::AtomicUsize>>,
}
impl ICloudPackageCreate {
    /// Share native staging admission across fresh and restored account adapters.
    /// Standalone constructors otherwise own an isolated four-file budget.
    pub fn with_write_staging_budget(mut self, budget: ICloudWriteStagingBudget) -> Self {
        self.staging_budget = budget;
        self
    }
    /// Bind an already constructed provider to credential-free synthetic HTTPS.
    /// Identity and retained checkpoint validation remain on their normal paths.
    #[cfg(feature = "test-support")]
    pub(crate) fn bind_synthetic_package_transport(
        &mut self,
        client: reqwest::Client,
    ) -> Result<()> {
        let account_hash = match self.session.get_mut() {
            Session::Sealed { apple_id, .. } => {
                crate::account_hash(apple_id).map_err(|_| UploadError::Invalid)?
            }
            Session::Ready(session) => session.account_hash.clone().ok_or(UploadError::Invalid)?,
        };
        let endpoint: url::Url = "https://fixture.icloud-content.com/"
            .parse()
            .map_err(|_| UploadError::Invalid)?;
        let mut session = ICloudReadSession::new().map_err(map_session_error)?;
        // A fresh session never carries cookies or authentication headers from
        // the original provider, and no credential vault is consulted.
        session.account_hash = Some(account_hash);
        session.http = client;
        session.drive_endpoint = Some(endpoint.clone());
        session.docs_endpoint = Some(endpoint);
        *self.session.get_mut() = Session::Ready(Box::new(session));
        Ok(())
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn native_handoff_test_provider(
        scope: Scope,
        parent: Node,
        staging: &Path,
        session: ICloudReadSession,
    ) -> Self {
        Self {
            scope,
            parent,
            staging: staging.into(),
            staging_budget: ICloudWriteStagingBudget::default(),
            session: Mutex::new(Session::Ready(Box::new(session))),
            #[cfg(test)]
            body_dispatch_probe: None,
        }
    }

    /// Caller-owned private persistent staging; no user archive path is retained.
    pub fn from_sealed_session(
        scope: Scope,
        apple_id: String,
        credential_id: String,
        state: &Path,
        parent: Node,
        staging: &Path,
    ) -> Result<Self> {
        Self::identity(&scope, &parent, staging)?;
        if apple_id.trim().is_empty() || Uuid::parse_str(&credential_id).is_err() {
            return Err(UploadError::Invalid);
        }
        let vault =
            SealedSessionVault::new(state, &scope.account).map_err(|_| UploadError::Invalid)?;
        Ok(Self {
            scope,
            parent,
            staging: staging.into(),
            staging_budget: ICloudWriteStagingBudget::default(),
            #[cfg(test)]
            body_dispatch_probe: None,
            session: Mutex::new(Session::Sealed {
                apple_id,
                credential_id,
                vault: Arc::new(vault),
            }),
        })
    }
    pub fn from_session_snapshot(
        scope: Scope,
        apple_id: &str,
        snapshot: &SecretString,
        parent: Node,
        staging: &Path,
    ) -> Result<Self> {
        Self::identity(&scope, &parent, staging)?;
        let session = ICloudReadSession::from_session_snapshot(snapshot, apple_id)
            .map_err(map_session_error)?;
        if session.account_hash.is_none() {
            return Err(UploadError::Invalid);
        }
        Ok(Self {
            scope,
            parent,
            staging: staging.into(),
            staging_budget: ICloudWriteStagingBudget::default(),
            #[cfg(test)]
            body_dispatch_probe: None,
            session: Mutex::new(Session::Ready(Box::new(session))),
        })
    }
    /// Restore only this provider's typed checkpoint. Does not consult current
    /// metadata or load credentials; every continuation retains its exact parent.
    #[allow(clippy::too_many_arguments)]
    pub fn restore_from_sealed_checkpoint(
        scope: Scope,
        apple_id: String,
        credential_id: String,
        state: &Path,
        staging: &Path,
        operation: &str,
        request: &UploadRequest,
        checkpoint: &SecretString,
    ) -> Result<Self> {
        if checkpoint.expose_secret().len() > MAX_CHECKPOINT {
            return Err(UploadError::CheckpointInvalid);
        }
        let saved: Checkpoint = serde_json::from_str(checkpoint.expose_secret())
            .map_err(|_| UploadError::CheckpointInvalid)?;
        let UploadIntent::Create { parent, .. } = &request.intent else {
            return Err(UploadError::Unsupported("native package replacement"));
        };
        if &saved.parent.id != parent {
            return Err(UploadError::CheckpointInvalid);
        }
        let adapter = Self::from_sealed_session(
            scope,
            apple_id,
            credential_id,
            state,
            saved.parent,
            staging,
        )?;
        adapter.decode(operation, request, checkpoint)?;
        Ok(adapter)
    }
    fn identity(scope: &Scope, parent: &Node, staging: &Path) -> Result<()> {
        let meta = std::fs::symlink_metadata(staging).map_err(|_| UploadError::Invalid)?;
        if Uuid::parse_str(&scope.account).is_err()
            || scope.provider != "icloud"
            || scope.collection != "drive"
            || parent.kind != NodeKind::Folder
            || parent.package
            || parent.target.is_some()
            || !parent.id.starts_with("FOLDER::com.apple.CloudDocs::")
            || parent.id.rsplit("::").next().is_none_or(str::is_empty)
            || !staging.is_absolute()
            || !meta.is_dir()
            || meta.permissions().mode() & 0o077 != 0
            || meta.uid()
                != std::fs::metadata("/proc/self")
                    .map_err(|_| UploadError::Invalid)?
                    .uid()
        {
            return Err(UploadError::Invalid);
        }
        Ok(())
    }
    fn request<'a>(&self, request: &'a UploadRequest) -> Result<&'a str> {
        request.validate()?;
        let UploadIntent::Create { parent, name } = &request.intent else {
            return Err(UploadError::Unsupported("native package replacement"));
        };
        if request.scope != self.scope
            || parent != &self.parent.id
            || request.size == 0
            || request.size > MAX_ARCHIVE
            || name.len() > 255
            || name
                .chars()
                .any(|c| c.is_control() || matches!(c, '\\' | ':'))
            || !matches!(
                request.representation,
                UploadRepresentation::PackageArchive { .. }
                    | UploadRepresentation::FlatNumbersArchive { .. }
                    | UploadRepresentation::FlatPagesArchive { .. }
            )
        {
            return Err(UploadError::Invalid);
        }
        if matches!(
            request.representation,
            UploadRepresentation::FlatNumbersArchive { .. }
        ) && cirrove_core::upload::native_package_suffix(name) != Some(".numbers")
        {
            return Err(UploadError::Invalid);
        }
        Ok(name)
    }
    pub(crate) fn validate_native_checkpoint(
        &self,
        operation: &str,
        request: &UploadRequest,
        checkpoint: &SecretString,
        account_hash: &str,
    ) -> Result<()> {
        if self.decode(operation, request, checkpoint)?.account_hash != account_hash {
            return Err(UploadError::CheckpointInvalid);
        }
        Ok(())
    }
    fn encode(saved: &Checkpoint) -> Result<SecretString> {
        let text = serde_json::to_string(saved).map_err(|_| UploadError::Invalid)?;
        if text.len() > MAX_CHECKPOINT {
            return Err(UploadError::Invalid);
        }
        Ok(SecretString::from(text))
    }
    fn decode(
        &self,
        operation: &str,
        request: &UploadRequest,
        checkpoint: &SecretString,
    ) -> Result<Checkpoint> {
        self.request(request)?;
        if checkpoint.expose_secret().len() > MAX_CHECKPOINT {
            return Err(UploadError::CheckpointInvalid);
        }
        let saved: Checkpoint = serde_json::from_str(checkpoint.expose_secret())
            .map_err(|_| UploadError::CheckpointInvalid)?;
        if !matches!(saved.version, 1 | 2)
            || Uuid::parse_str(operation).is_err()
            || saved.operation != operation
            || saved.request != *request
            || saved.parent != self.parent
            || saved.account_hash.is_empty()
            || saved.account_hash.len() > 256
        {
            return Err(UploadError::CheckpointInvalid);
        }
        let flat = matches!(
            request.representation,
            UploadRepresentation::FlatNumbersArchive { .. }
                | UploadRepresentation::FlatPagesArchive { .. }
        );
        if (saved.version == 1
            && matches!(
                request.representation,
                UploadRepresentation::FlatPagesArchive { .. }
            ))
            || (saved.version == 2 && (!flat || saved.wire.is_none()))
            || (saved.version == 1 && saved.wire.is_some())
        {
            return Err(UploadError::CheckpointInvalid);
        }
        if let Some(wire) = &saved.wire
            && (!matches!(
                request.representation,
                UploadRepresentation::FlatNumbersArchive { .. }
                    | UploadRepresentation::FlatPagesArchive { .. }
            ) || wire.validate(self.request(request)?).is_err())
        {
            return Err(UploadError::CheckpointInvalid);
        }
        let valid = match saved.phase {
            Phase::AllocationArmed => saved.slot.is_none() && saved.registration.is_none(),
            Phase::BodyArmed => saved.slot.is_some() && saved.registration.is_none(),
            Phase::RegistrationArmed => {
                saved.slot.is_some()
                    && saved
                        .registration
                        .as_ref()
                        .is_some_and(|s| s.len() <= 64 * 1024)
            }
        };
        if !valid
            || saved
                .slot
                .as_ref()
                .is_some_and(|slot| slot.validate().is_err())
        {
            return Err(UploadError::CheckpointInvalid);
        }
        if let Some(fragment) = &saved.registration {
            wire::registration(
                &self.parent.id,
                self.request(request)?,
                saved.slot.as_ref().ok_or(UploadError::CheckpointInvalid)?,
                fragment,
            )
            .map_err(|_| UploadError::CheckpointInvalid)?;
        }
        Ok(saved)
    }
    async fn active(state: &mut Session) -> Result<&mut ICloudReadSession> {
        if let Session::Sealed {
            apple_id,
            credential_id,
            vault,
        } = state
        {
            let saved = vault
                .load(credential_id)
                .await
                .map_err(map_session_error)?
                .ok_or(UploadError::Uncertain)?;
            let session = ICloudReadSession::from_session_snapshot(&saved, apple_id)
                .map_err(map_session_error)?;
            if session.account_hash.is_none() {
                return Err(UploadError::Invalid);
            }
            *state = Session::Ready(Box::new(session));
        }
        match state {
            Session::Ready(s) => Ok(s),
            _ => Err(UploadError::Uncertain),
        }
    }
    fn binding(session: &ICloudReadSession, saved: &Checkpoint) -> Result<()> {
        if session.account_hash.as_deref() != Some(saved.account_hash.as_str()) {
            return Err(UploadError::CheckpointInvalid);
        }
        Ok(())
    }
    async fn parent(&self, session: &mut ICloudReadSession) -> Result<()> {
        let mut next = self.parent.id.clone();
        for depth in 0..128 {
            if next == ROOT_ID {
                return Ok(());
            }
            let entry = session
                .folder_metadata(&next)
                .await
                .map_err(map_session_error)?;
            if entry.kind != "FOLDER"
                || entry.zone != "com.apple.CloudDocs"
                || entry.drivewsid != next
                || (depth == 0
                    && (Some(&entry.parent_id) != self.parent.parent_id.as_ref()
                        || entry.display_name() != self.parent.name))
                || !entry.parent_id.starts_with("FOLDER::com.apple.CloudDocs::")
                || entry.parent_id == next
            {
                return Err(UploadError::Conflict);
            }
            next = entry.parent_id;
        }
        Err(UploadError::Conflict)
    }
    async fn vacant(&self, session: &mut ICloudReadSession, name: &str) -> Result<()> {
        self.parent(session).await?;
        let entries = session
            .list_folder(&self.parent.id)
            .await
            .map_err(map_session_error)?;
        if entries.iter().any(|entry| entry.display_name() == name) {
            return Err(UploadError::Conflict);
        }
        Ok(())
    }
    async fn payload(
        &self,
        mut source: File,
        request: &UploadRequest,
        cancel: &CancellationToken,
    ) -> Result<Arc<WriteStagingFile>> {
        let request = request.clone();
        let cancel = cancel.clone();
        let target = self
            .staging_budget
            .create(&self.staging)
            .await
            .map_err(|_| UploadError::Uncertain)?;
        tokio::task::spawn_blocking(move || {
            let semantic = match &request.representation {
                UploadRepresentation::PackageArchive { semantic, .. }
                | UploadRepresentation::FlatNumbersArchive { semantic }
                | UploadRepresentation::FlatPagesArchive { semantic } => semantic,
                _ => return Err(UploadError::Invalid),
            };
            if !source
                .metadata()
                .map_err(|_| UploadError::Invalid)?
                .is_file()
            {
                return Err(UploadError::Invalid);
            }
            source
                .seek(SeekFrom::Start(0))
                .map_err(|_| UploadError::Invalid)?;
            let mut hash = Sha256::new();
            let mut size = 0u64;
            let mut buffer = [0; 64 * 1024];
            loop {
                if cancel.is_cancelled() {
                    return Err(UploadError::Uncertain);
                }
                let n = source.read(&mut buffer).map_err(|_| UploadError::Invalid)?;
                if n == 0 {
                    break;
                }
                let offset = size;
                size += n as u64;
                if size > request.size {
                    return Err(UploadError::Invalid);
                }
                target
                    .write_chunk_at(offset, &buffer[..n])
                    .map_err(|_| UploadError::Invalid)?;
                hash.update(&buffer[..n]);
            }
            let digest = hex::encode(hash.finalize());
            if size != request.size || digest != request.sha256 {
                return Err(UploadError::Invalid);
            }
            let receipt = crate::PackageDownload {
                size,
                sha256: digest,
            };
            let actual = match &request.representation {
                UploadRepresentation::PackageArchive { expected_root, .. } => {
                    crate::package_archive_semantic_identity_versioned(
                        target.borrowed_file(),
                        &receipt,
                        expected_root,
                        semantic.version,
                        &cancel,
                    )?
                }
                UploadRepresentation::FlatNumbersArchive { .. }
                | UploadRepresentation::FlatPagesArchive { .. } => {
                    crate::package_flat_archive_semantic_identity_v2(
                        target.borrowed_file(),
                        &receipt,
                        &cancel,
                    )?
                }
                _ => return Err(UploadError::Invalid),
            };
            if &actual != semantic {
                return Err(UploadError::Invalid);
            }
            Ok(target)
        })
        .await
        .map_err(|_| UploadError::Uncertain)?
    }
    async fn flat_wire(
        &self,
        source: File,
        request: &UploadRequest,
        cancel: &CancellationToken,
    ) -> Result<(Arc<WriteStagingFile>, crate::package_wire::WireReceipt)> {
        let name = self.request(request)?.to_owned();
        let (UploadRepresentation::FlatNumbersArchive { semantic }
        | UploadRepresentation::FlatPagesArchive { semantic }) = &request.representation
        else {
            return Err(UploadError::Invalid);
        };
        let pages = matches!(
            request.representation,
            UploadRepresentation::FlatPagesArchive { .. }
        );
        let semantic = semantic.clone();
        let original = crate::PackageDownload {
            size: request.size,
            sha256: request.sha256.clone(),
        };
        let source = self.payload(source, request, cancel).await?;
        let wire = self
            .staging_budget
            .create(&self.staging)
            .await
            .map_err(|_| UploadError::Uncertain)?;
        let cancel = cancel.clone();
        tokio::task::spawn_blocking(move || {
            let receipt = if pages {
                crate::package_wire::flat_pages_wire(
                    &source, &original, &name, &semantic, &wire, &cancel,
                )?
            } else {
                crate::package_wire::flat_numbers_wire(
                    &source, &original, &name, &semantic, &wire, &cancel,
                )?
            };
            Ok((wire, receipt))
        })
        .await
        .map_err(|_| UploadError::Uncertain)?
    }
    fn wire_size(&self, saved: &Checkpoint) -> Result<u64> {
        if matches!(
            saved.request.representation,
            UploadRepresentation::FlatNumbersArchive { .. }
                | UploadRepresentation::FlatPagesArchive { .. }
        ) {
            let wire = saved.wire.as_ref().ok_or(UploadError::CheckpointInvalid)?;
            wire.validate(self.request(&saved.request)?)
                .map_err(|_| UploadError::CheckpointInvalid)?;
            Ok(wire.size)
        } else {
            Ok(saved.request.size)
        }
    }
    async fn verify(
        &self,
        session: &mut ICloudReadSession,
        saved: &Checkpoint,
        cancel: &CancellationToken,
    ) -> Result<PackageUploadReceipt> {
        self.verify_with_progress(session, saved, cancel, &mut VerificationProgress::default())
            .await
    }
    async fn verify_with_progress(
        &self,
        session: &mut ICloudReadSession,
        saved: &Checkpoint,
        cancel: &CancellationToken,
        progress: &mut VerificationProgress,
    ) -> Result<PackageUploadReceipt> {
        progress.fence = "account-and-parent";
        Self::binding(session, saved)?;
        let slot = saved.slot.as_ref().ok_or(UploadError::Uncertain)?;
        self.parent(session).await?;
        let name = self.request(&saved.request)?;
        progress.fence = "stage-metadata";
        let entries = session
            .list_folder(&self.parent.id)
            .await
            .map_err(map_session_error)?;
        let exact: Vec<_> = entries
            .iter()
            .filter(|e| e.docwsid == slot.document_id)
            .collect();
        if exact.len() != 1 {
            return Err(UploadError::Uncertain);
        }
        let entry = exact[0];
        if entry.kind != "FILE"
            || entry.zone != "com.apple.CloudDocs"
            || entry.drivewsid != format!("FILE::com.apple.CloudDocs::{}", slot.document_id)
            || entry.parent_id != self.parent.id
            || entry.display_name() != name
            || entry.etag.is_empty()
            || entries.iter().filter(|e| e.display_name() == name).count() != 1
        {
            return Err(UploadError::Conflict);
        }
        progress.fence = "package-download";
        let file = self
            .staging_budget
            .create(&self.staging)
            .await
            .map_err(|_| UploadError::Uncertain)?;
        let mut sink = DiskSink {
            file: file.clone(),
            offset: 0,
        };
        let receipt = session
            .download_package(&self.parent.id, entry, MAX_ARCHIVE, &mut sink, cancel)
            .await
            .map_err(map_session_error)?;
        drop(sink);
        #[cfg(feature = "write-probe")]
        if progress.retain_download {
            progress.download = Some((
                file.clone(),
                crate::PackageDownload {
                    size: receipt.size,
                    sha256: receipt.sha256.clone(),
                },
            ));
        }
        progress.fence = "archive-semantic-parse";
        let token = cancel.clone();
        let root = name.to_owned();
        let expected = match &saved.request.representation {
            UploadRepresentation::PackageArchive { semantic, .. }
            | UploadRepresentation::FlatNumbersArchive { semantic }
            | UploadRepresentation::FlatPagesArchive { semantic } => semantic,
            _ => return Err(UploadError::Invalid),
        };
        let version = expected.version;
        let semantic = tokio::task::spawn_blocking(move || {
            crate::package_archive_semantic_identity_versioned(
                file.borrowed_file(),
                &receipt,
                &root,
                version,
                &token,
            )
        })
        .await
        .map_err(|_| UploadError::Uncertain)??;
        let expected = match &saved.request.representation {
            UploadRepresentation::PackageArchive { semantic, .. }
            | UploadRepresentation::FlatNumbersArchive { semantic }
            | UploadRepresentation::FlatPagesArchive { semantic } => semantic,
            _ => return Err(UploadError::Invalid),
        };
        progress.fence = "archive-semantic-equality";
        if &semantic != expected {
            return Err(UploadError::Conflict);
        }
        progress.fence = "final-metadata-fence";
        // A final independent listing also binds name, uniqueness and parent.
        let after = session
            .list_folder(&self.parent.id)
            .await
            .map_err(map_session_error)?;
        if after
            .iter()
            .filter(|e| e.docwsid == slot.document_id)
            .count()
            != 1
            || after.iter().filter(|e| e.display_name() == name).count() != 1
            || !after.iter().any(|e| e == entry)
        {
            return Err(UploadError::Conflict);
        }
        progress.fence = "verified";
        Ok(PackageUploadReceipt {
            remote: Node {
                id: entry.drivewsid.clone(),
                parent_id: Some(entry.parent_id.clone()),
                name: entry.display_name(),
                kind: NodeKind::Folder,
                size: entry.size,
                modified_unix: 0,
                etag: Some(entry.etag.clone()),
                // The source package folder is versioned by Apple's ETag,
                // exactly like provider::directory_nodes. Only a generated
                // archive child carries its own synthetic content version.
                content_version: None,
                target: None,
                package: true,
            },
            semantic,
        })
    }
}
struct DiskSink {
    file: Arc<WriteStagingFile>,
    offset: u64,
}
#[async_trait]
impl ReadWindowSink for DiskSink {
    async fn write_chunk(&mut self, bytes: &[u8]) -> std::result::Result<(), ProviderError> {
        if bytes.len() > 64 * 1024 {
            return Err(ProviderError::Protocol(
                "package verification chunk exceeds bound",
            ));
        }
        self.file
            .write_at(self.offset, bytes.to_vec())
            .await
            .map_err(|_| ProviderError::Protocol("package verification staging unavailable"))?;
        self.offset += bytes.len() as u64;
        Ok(())
    }
}
#[async_trait]
impl UploadProvider for ICloudPackageCreate {
    fn requires_begin_payload(&self, request: &UploadRequest) -> bool {
        matches!(
            request.representation,
            UploadRepresentation::FlatNumbersArchive { .. }
                | UploadRepresentation::FlatPagesArchive { .. }
        )
    }
    fn begin_is_mutation_free_until_checkpoint(&self, request: &UploadRequest) -> bool {
        self.request(request).is_ok()
    }
    async fn begin_upload(&self, _: &UploadRequest, _: &CancellationToken) -> Result<UploadStep> {
        Err(UploadError::Unsupported(
            "package upload requires operation binding",
        ))
    }
    async fn begin_upload_for_operation(
        &self,
        operation: &str,
        request: &UploadRequest,
        cancel: &CancellationToken,
    ) -> Result<UploadStep> {
        if self.requires_begin_payload(request) {
            return Err(UploadError::Unsupported(
                "flat Numbers preparation requires sealed payload",
            ));
        }
        self.request(request)?;
        if Uuid::parse_str(operation).is_err() {
            return Err(UploadError::Invalid);
        }
        tokio::select! {biased; _ = cancel.cancelled() => Err(UploadError::Uncertain), result = async {
            let mut state = self.session.lock().await; let session = Self::active(&mut state).await?;
            self.vacant(session, self.request(request)?).await?;
            let saved = Checkpoint {version:1, operation:operation.into(), request:request.clone(), parent:self.parent.clone(), account_hash:session.account_hash.clone().ok_or(UploadError::Invalid)?, phase:Phase::AllocationArmed, slot:None, registration:None, wire:None};
            Ok(UploadStep::Allocate(Self::encode(&saved)?))
        } => result}
    }
    async fn begin_upload_from_payload_for_operation(
        &self,
        operation: &str,
        request: &UploadRequest,
        payload: File,
        cancel: &CancellationToken,
    ) -> Result<UploadStep> {
        if !self.requires_begin_payload(request) {
            return self
                .begin_upload_for_operation(operation, request, cancel)
                .await;
        }
        self.request(request)?;
        if Uuid::parse_str(operation).is_err() {
            return Err(UploadError::Invalid);
        }
        let (_, wire) = self.flat_wire(payload, request, cancel).await?;
        tokio::select! {biased; _ = cancel.cancelled() => Err(UploadError::Uncertain), result = async {
            let mut state = self.session.lock().await; let session = Self::active(&mut state).await?;
            self.vacant(session, self.request(request)?).await?;
            if cancel.is_cancelled() { return Err(UploadError::Uncertain); }
            let saved = Checkpoint { version:2, operation:operation.into(), request:request.clone(), parent:self.parent.clone(), account_hash:session.account_hash.clone().ok_or(UploadError::Invalid)?, phase:Phase::AllocationArmed, slot:None, registration:None, wire:Some(wire) };
            Ok(UploadStep::Allocate(Self::encode(&saved)?))
        } => result}
    }
    async fn allocate_upload_for_operation(
        &self,
        operation: &str,
        request: &UploadRequest,
        checkpoint: &SecretString,
        cancel: &CancellationToken,
    ) -> Result<UploadStep> {
        let mut saved = self.decode(operation, request, checkpoint)?;
        if saved.phase != Phase::AllocationArmed {
            return Err(UploadError::CheckpointInvalid);
        }
        let wire_size = self.wire_size(&saved)?;
        tokio::select! {biased; _ = cancel.cancelled() => Err(UploadError::Uncertain), result = async {
            let _reservation = self.staging_budget.create(&self.staging).await.map_err(|_| UploadError::Uncertain)?;
            let mut state = self.session.lock().await; let session = Self::active(&mut state).await?; Self::binding(session, &saved)?;
            self.vacant(session, self.request(request)?).await?;
            if cancel.is_cancelled() {return Err(UploadError::Uncertain);}
            saved.slot = Some(wire::allocate(session, self.request(request)?, wire_size).await.map_err(map_session_error)?);
            saved.phase = Phase::BodyArmed;
            Ok(UploadStep::Stream(Self::encode(&saved)?))
        } => result}
    }
    async fn inspect_upload(
        &self,
        _: &UploadRequest,
        _: &SecretString,
        _: &CancellationToken,
    ) -> Result<UploadStep> {
        Err(UploadError::Unsupported(
            "package upload requires operation binding",
        ))
    }
    async fn inspect_upload_for_operation(
        &self,
        operation: &str,
        request: &UploadRequest,
        checkpoint: &SecretString,
        cancel: &CancellationToken,
    ) -> Result<UploadStep> {
        let saved = self.decode(operation, request, checkpoint)?;
        if saved.slot.is_none() {
            return Err(UploadError::Uncertain);
        }
        tokio::select! {biased; _ = cancel.cancelled() => Err(UploadError::Uncertain), result = async {
            let mut state = self.session.lock().await; let session = Self::active(&mut state).await?;
            Ok(UploadStep::PackageComplete(self.verify(session, &saved, cancel).await?))
        } => result}
    }
    fn inspection_timeout(&self, _: &UploadRequest) -> Duration {
        Duration::from_secs(240)
    }
    fn commit_timeout(&self, _: &UploadRequest) -> Duration {
        Duration::from_secs(240)
    }
    async fn upload_part(
        &self,
        _: &UploadRequest,
        _: &SecretString,
        _: u64,
        _: Vec<u8>,
        _: &CancellationToken,
    ) -> Result<UploadStep> {
        Err(UploadError::Unsupported(
            "package requires complete archive stream",
        ))
    }
    async fn upload_stream_for_operation(
        &self,
        operation: &str,
        request: &UploadRequest,
        checkpoint: &SecretString,
        payload: File,
        cancel: &CancellationToken,
    ) -> Result<UploadStep> {
        let mut saved = self.decode(operation, request, checkpoint)?;
        if saved.phase != Phase::BodyArmed {
            return Err(UploadError::CheckpointInvalid);
        }
        let wire_size = self.wire_size(&saved)?;
        let file = if self.requires_begin_payload(request) {
            let (file, receipt) = self.flat_wire(payload, request, cancel).await?;
            if saved.wire.as_ref() != Some(&receipt) {
                return Err(UploadError::CheckpointInvalid);
            }
            file
        } else {
            self.payload(payload, request, cancel).await?
        };
        tokio::select! {biased; _ = cancel.cancelled() => Err(UploadError::Uncertain), result = async {
            let mut state = self.session.lock().await; let session = Self::active(&mut state).await?; Self::binding(session, &saved)?;
            // Vault loading can cancel and return Ready in the same poll. The
            // enclosing select must not be the only pre-dispatch boundary.
            if cancel.is_cancelled() { return Err(UploadError::Uncertain); }
            #[cfg(test)]
            if let Some(probe) = &self.body_dispatch_probe {
                // Immediate test transport: observes dispatch without DNS or I/O.
                probe.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                return Err(UploadError::Uncertain);
            }
            let receipt = wire::upload(session, saved.slot.as_ref().ok_or(UploadError::CheckpointInvalid)?, file, wire_size).await.map_err(map_content_error)?;
            saved.registration = Some(receipt.expose_secret().to_owned()); saved.phase = Phase::RegistrationArmed;
            Ok(UploadStep::Commit(Self::encode(&saved)?))
        } => result}
    }
    async fn commit_upload(
        &self,
        _: &UploadRequest,
        _: &SecretString,
        _: &CancellationToken,
    ) -> Result<UploadStep> {
        Err(UploadError::Unsupported(
            "package upload requires operation binding",
        ))
    }
    async fn commit_upload_for_operation(
        &self,
        operation: &str,
        request: &UploadRequest,
        checkpoint: &SecretString,
        cancel: &CancellationToken,
    ) -> Result<UploadStep> {
        let saved = self.decode(operation, request, checkpoint)?;
        if saved.phase != Phase::RegistrationArmed {
            return Err(UploadError::CheckpointInvalid);
        }
        self.wire_size(&saved)?;
        tokio::select! {biased; _ = cancel.cancelled() => Err(UploadError::Uncertain), result = async {
            let mut state = self.session.lock().await; let session = Self::active(&mut state).await?; Self::binding(session, &saved)?;
            self.vacant(session, self.request(request)?).await?;
            if cancel.is_cancelled() {return Err(UploadError::Uncertain);}
            wire::register(session, &self.parent.id, self.request(request)?, saved.slot.as_ref().ok_or(UploadError::CheckpointInvalid)?, saved.registration.as_deref().ok_or(UploadError::CheckpointInvalid)?).await.map_err(map_session_error)?;
            Ok(UploadStep::PackageComplete(self.verify(session, &saved, cancel).await?))
        } => result}
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
        let checkpoint = checkpoint.ok_or(UploadError::Uncertain)?;
        match self
            .inspect_upload_for_operation(operation, request, checkpoint, cancel)
            .await?
        {
            UploadStep::PackageComplete(receipt) => Ok(Reconciliation::PackageCommitted(receipt)),
            _ => Err(UploadError::Uncertain),
        }
    }
}
#[cfg(test)]
mod tests;

#[cfg(feature = "write-probe")]
impl ICloudPackageCreate {
    /// Decode/validate locally; do not interpret an armed checkpoint as success.
    pub(crate) fn diagnostic_checkpoint(
        &self,
        operation: &str,
        request: &UploadRequest,
        checkpoint: &SecretString,
        account_hash: &str,
    ) -> Result<(&'static str, Option<String>)> {
        let saved = self.decode(operation, request, checkpoint)?;
        if saved.account_hash != account_hash {
            return Err(UploadError::CheckpointInvalid);
        }
        let phase = match saved.phase {
            Phase::AllocationArmed => "stage-allocation-armed",
            Phase::BodyArmed => "stage-body-armed",
            Phase::RegistrationArmed => "stage-registration-armed",
        };
        Ok((
            phase,
            saved
                .slot
                .map(|slot| format!("FILE::com.apple.CloudDocs::{}", slot.document_id)),
        ))
    }
}

#[cfg(feature = "write-probe")]
impl ICloudPackageCreate {
    /// The same verification-only path as inspect_upload_for_operation. No
    /// allocation, upload, registration, checkpoint save or returned-step execution.
    pub(crate) async fn diagnostic_inspect(
        &self,
        operation: &str,
        request: &UploadRequest,
        checkpoint: &SecretString,
        source: File,
        source_root: Option<String>,
        cancel: &CancellationToken,
    ) -> Result<(serde_json::Value, Option<PackageUploadReceipt>)> {
        let saved = self.decode(operation, request, checkpoint)?;
        match (&request.representation, source_root.as_deref()) {
            (UploadRepresentation::PackageArchive { expected_root, .. }, Some(root))
                if root == expected_root => {}
            (
                UploadRepresentation::FlatNumbersArchive { .. }
                | UploadRepresentation::FlatPagesArchive { .. },
                None,
            ) => {}
            _ => return Err(UploadError::CheckpointInvalid),
        }
        if saved.slot.is_none() {
            return Err(UploadError::Uncertain);
        }
        let mut progress = VerificationProgress {
            retain_download: true,
            ..Default::default()
        };
        let outcome = tokio::select! {biased; _ = cancel.cancelled() => Err(UploadError::Uncertain), result = async {
            let mut state = self.session.lock().await;
            let session = Self::active(&mut state).await?;
            self.verify_with_progress(session, &saved, cancel, &mut progress).await
        } => result};
        let category = match &outcome {
            Ok(_) => "verified",
            Err(UploadError::Conflict) => "conflict",
            Err(_) => "unavailable",
        };
        let comparison = if let Some((download, receipt)) = progress.download {
            let token = cancel.clone();
            let source_receipt = crate::PackageDownload {
                size: request.size,
                sha256: request.sha256.clone(),
            };
            let stage_root = self.request(request)?.to_owned();
            match tokio::task::spawn_blocking(move || {
                crate::package_semantic::diagnostic_archive_comparison(
                    &source,
                    &source_receipt,
                    source_root.as_deref(),
                    download.borrowed_file(),
                    &receipt,
                    &stage_root,
                    &token,
                )
            })
            .await
            {
                Ok(Ok(value)) => value,
                _ => serde_json::json!({"comparison_available":false}),
            }
        } else {
            serde_json::json!({"comparison_available":false})
        };
        Ok((
            serde_json::json!({"fence":progress.fence,"category":category,"archive_comparison":comparison}),
            outcome.ok(),
        ))
    }
}

impl ICloudPackageCreate {
    pub(crate) fn registered_stage_for_abandonment(
        &self,
        operation: &str,
        request: &UploadRequest,
        checkpoint: &SecretString,
        account_hash: &str,
    ) -> Result<String> {
        let saved = self.decode(operation, request, checkpoint)?;
        if saved.phase != Phase::RegistrationArmed || saved.account_hash != account_hash {
            return Err(UploadError::CheckpointInvalid);
        }
        let slot = saved.slot.ok_or(UploadError::CheckpointInvalid)?;
        Ok(format!("FILE::com.apple.CloudDocs::{}", slot.document_id))
    }
}
