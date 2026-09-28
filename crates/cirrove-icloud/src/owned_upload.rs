//! A worker-driven native upload into one freshly created Cirrove test folder.
//! This remains feature-gated: uncertain Apple content-slot requests are never
//! replayed merely because a later listing does not yet show the file.
use super::{
    DriveEntry, ICloudReadSession, ValidationFolder, checked_content_url,
    write_probe::{MAX_OWNED_UPLOAD, OwnedRegistration, UploadSlot, UploadedFile},
};
use async_trait::async_trait;
use cirrove_core::upload::{
    Reconciliation, Result as UploadResult, UploadError, UploadIntent, UploadProvider,
    UploadRequest, UploadStep,
};
use cirrove_core::{CancellationToken, Node, NodeKind, Scope};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    sync::atomic::{AtomicBool, Ordering},
};
use tokio::sync::Mutex;
use uuid::Uuid;

const MAX_OWNED_FILE: u64 = 32 * 1024 * 1024;
const VERIFY_RANGE: u32 = 4 * 1024 * 1024;
const MAX_CHECKPOINT: usize = 8192;

#[derive(Serialize, Deserialize)]
struct CreateCheckpoint {
    version: u8,
    scope: Scope,
    parent: String,
    name: String,
    size: u64,
    sha256: String,
    /// Assigned before any visible registration and held only in the vault.
    slot: Option<UploadSlot>,
    /// Saved after content transfer and before registration is ever sent.
    #[serde(default)]
    receipt: Option<UploadedFile>,
}

/// Only accepts a `ValidationFolder` returned by this authenticated session's
/// live fixture creation. It is not constructed by the ordinary daemon.
pub struct ICloudOwnedFixtureUpload {
    scope: Scope,
    folder: ValidationFolder,
    session: Mutex<ICloudReadSession>,
    discard_registration_receipt: AtomicBool,
    discard_content_receipt: AtomicBool,
    reconciliation_only: bool,
}

impl ICloudOwnedFixtureUpload {
    pub fn new(
        scope: Scope,
        session: ICloudReadSession,
        folder: ValidationFolder,
    ) -> UploadResult<Self> {
        if scope.account.is_empty()
            || scope.provider != "icloud"
            || scope.collection != "drive"
            || session.account_hash.is_none()
            || !folder.id.starts_with("FOLDER::com.apple.CloudDocs::")
            || folder
                .name
                .strip_prefix("Cirrove Write Validation-")
                .is_none_or(|id| Uuid::parse_str(id).is_err())
        {
            return Err(UploadError::Invalid);
        }
        Ok(Self {
            scope,
            folder,
            session: Mutex::new(session),
            discard_registration_receipt: AtomicBool::new(false),
            discard_content_receipt: AtomicBool::new(false),
            reconciliation_only: false,
        })
    }

    /// Deliberately lose one already-received registration response in the
    /// foreground validation process. No normal service constructs this mode.
    pub fn with_discarded_registration_receipt(self) -> Self {
        self.discard_registration_receipt
            .store(true, Ordering::Release);
        self
    }

    /// Drop an accepted content response before its receipt is checkpointed.
    /// Only the isolated validator constructs this mode; add_file is not sent.
    pub fn with_discarded_content_receipt(self) -> Self {
        self.discard_content_receipt.store(true, Ordering::Release);
        self
    }

    /// A restarted validation process must reconcile; it must never upload.
    pub fn reconciliation_only(mut self) -> Self {
        self.reconciliation_only = true;
        self
    }

    /// Exposes only the opaque document identity for the isolated validator;
    /// the slot URL stays in the vault and is never printed or logged.
    pub fn reserved_document_id(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
    ) -> UploadResult<Option<String>> {
        Ok(self
            .check_checkpoint(request, checkpoint)?
            .slot
            .map(|slot| slot.document_id))
    }

    pub fn content_receipt_saved(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
    ) -> UploadResult<bool> {
        Ok(self
            .check_checkpoint(request, checkpoint)?
            .receipt
            .is_some())
    }

    fn check_request(&self, request: &UploadRequest) -> UploadResult<()> {
        request.validate()?;
        let UploadIntent::Create { parent, name } = &request.intent else {
            return Err(UploadError::Unsupported("owned fixture replacement"));
        };
        if request.scope != self.scope
            || parent != &self.folder.id
            || request.size == 0
            || request.size > MAX_OWNED_FILE
            || name.len() > 255
        {
            return Err(UploadError::Invalid);
        }
        Ok(())
    }

    fn checkpoint(
        &self,
        request: &UploadRequest,
        slot: Option<UploadSlot>,
    ) -> UploadResult<SecretString> {
        self.checkpoint_with_receipt(request, slot, None)
    }

    fn checkpoint_with_receipt(
        &self,
        request: &UploadRequest,
        slot: Option<UploadSlot>,
        receipt: Option<UploadedFile>,
    ) -> UploadResult<SecretString> {
        self.check_request(request)?;
        let UploadIntent::Create { parent, name } = &request.intent else {
            return Err(UploadError::Invalid);
        };
        let checkpoint = CreateCheckpoint {
            // Legacy v2 can register in the same call as content upload.
            // Every newly started worker Create uses v3's split stream.
            version: 3,
            scope: request.scope.clone(),
            parent: parent.clone(),
            name: name.clone(),
            size: request.size,
            sha256: request.sha256.clone(),
            slot,
            receipt,
        };
        let text = serde_json::to_string(&checkpoint).map_err(|_| UploadError::Invalid)?;
        if text.len() > MAX_CHECKPOINT {
            return Err(UploadError::Invalid);
        }
        Ok(SecretString::from(text))
    }

    fn check_checkpoint(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
    ) -> UploadResult<CreateCheckpoint> {
        self.check_request(request)?;
        if checkpoint.expose_secret().len() > MAX_CHECKPOINT {
            return Err(UploadError::CheckpointInvalid);
        }
        let saved: CreateCheckpoint = serde_json::from_str(checkpoint.expose_secret())
            .map_err(|_| UploadError::CheckpointInvalid)?;
        let UploadIntent::Create { parent, name } = &request.intent else {
            return Err(UploadError::Invalid);
        };
        if !matches!(saved.version, 2 | 3)
            || saved.scope != request.scope
            || saved.parent != *parent
            || saved.name != *name
            || saved.size != request.size
            || saved.sha256 != request.sha256
            || (saved.version == 2 && saved.receipt.is_some())
            || saved
                .receipt
                .as_ref()
                .is_some_and(|receipt| saved.slot.is_none() || !receipt.valid_for(request.size))
            || saved.slot.as_ref().is_some_and(|slot| {
                slot.document_id.is_empty()
                    || slot.document_id.len() > 256
                    || checked_content_url(&slot.url).is_err()
            })
        {
            return Err(UploadError::CheckpointInvalid);
        }
        Ok(saved)
    }

    fn node(
        &self,
        entry: &DriveEntry,
        request: &UploadRequest,
        expected_doc_id: &str,
    ) -> UploadResult<Node> {
        let UploadIntent::Create { name, .. } = &request.intent else {
            return Err(UploadError::Invalid);
        };
        if entry.is_folder()
            || entry.drivewsid.is_empty()
            || !entry.drivewsid.starts_with("FILE::com.apple.CloudDocs::")
            || entry.docwsid.is_empty()
            || entry.drivewsid.rsplit("::").next() != Some(entry.docwsid.as_str())
            || entry.docwsid != expected_doc_id
            || entry.display_name() != *name
            || entry.size != request.size
            || entry.etag.is_empty()
        {
            return Err(UploadError::Conflict);
        }
        Ok(Node {
            id: entry.drivewsid.clone(),
            parent_id: Some(self.folder.id.clone()),
            name: name.clone(),
            kind: NodeKind::File,
            size: entry.size,
            modified_unix: 0,
            etag: Some(entry.etag.clone()),
            content_version: Some(entry.etag.clone()),
            target: None,
            package: false,
        })
    }

    fn select_candidate<'a>(
        items: &'a [DriveEntry],
        name: &str,
        expected_doc_id: &str,
    ) -> UploadResult<Option<&'a DriveEntry>> {
        let mut matches = items
            .iter()
            .filter(|entry| entry.docwsid == expected_doc_id);
        let candidate = matches.next();
        if matches.next().is_some()
            || items
                .iter()
                .any(|entry| entry.display_name() == name && entry.docwsid != expected_doc_id)
        {
            return Err(UploadError::Conflict);
        }
        Ok(candidate)
    }

    /// `None` is uncertain, not proof that an in-flight registration failed.
    async fn observed(
        &self,
        request: &UploadRequest,
        expected_doc_id: &str,
    ) -> UploadResult<Option<Node>> {
        let UploadIntent::Create { name, .. } = &request.intent else {
            return Err(UploadError::Invalid);
        };
        let mut session = self.session.lock().await;
        let items = session
            .list_folder(&self.folder.id)
            .await
            .map_err(|_| UploadError::Uncertain)?;
        let Some(entry) = Self::select_candidate(&items, name, expected_doc_id)? else {
            return Ok(None);
        };
        let node = self.node(entry, request, expected_doc_id)?;
        let mut hash = Sha256::new();
        let mut offset = 0;
        while offset < request.size {
            let length = (request.size - offset).min(u64::from(VERIFY_RANGE)) as u32;
            let bytes = session
                .read_range_in_folder_for_revision(
                    &self.folder.id,
                    &node.id,
                    offset,
                    length,
                    Some((
                        node.etag.as_deref().ok_or(UploadError::Conflict)?,
                        request.size,
                    )),
                )
                .await
                .map_err(|_| UploadError::Uncertain)?;
            if bytes.len() != length as usize {
                return Err(UploadError::Conflict);
            }
            hash.update(bytes);
            offset += u64::from(length);
        }
        if hex::encode(hash.finalize()) != request.sha256 {
            return Err(UploadError::Conflict);
        }
        Ok(Some(node))
    }
}

#[async_trait]
impl UploadProvider for ICloudOwnedFixtureUpload {
    async fn begin_upload(
        &self,
        request: &UploadRequest,
        cancel: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        if cancel.is_cancelled() {
            return Err(UploadError::Uncertain);
        }
        // Save the operation identity before asking Apple for a content slot.
        // Slot allocation alone cannot make a file visible in Drive.
        Ok(UploadStep::Prepared(self.checkpoint(request, None)?))
    }

    async fn inspect_upload(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
        _: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        let saved = self.check_checkpoint(request, checkpoint)?;
        let Some(slot) = saved.slot else {
            if self.reconciliation_only {
                return Err(UploadError::Uncertain);
            }
            let UploadIntent::Create { name, .. } = &request.intent else {
                return Err(UploadError::Invalid);
            };
            let slot = self
                .session
                .lock()
                .await
                .allocate_upload_slot(name, request.size)
                .await
                .map_err(|_| UploadError::Uncertain)?;
            return Ok(UploadStep::Stream(self.checkpoint(request, Some(slot))?));
        };
        self.observed(request, &slot.document_id)
            .await?
            .map(UploadStep::Complete)
            .ok_or(UploadError::Uncertain)
    }

    async fn upload_part(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
        offset: u64,
        bytes: Vec<u8>,
        cancel: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        let saved = self.check_checkpoint(request, checkpoint)?;
        let slot = saved.slot.ok_or(UploadError::CheckpointInvalid)?;
        if self.reconciliation_only {
            return Err(UploadError::Unsupported("reconciliation-only validation"));
        }
        if offset != 0
            || request.size > MAX_OWNED_UPLOAD as u64
            || bytes.len() as u64 != request.size
            || hex::encode(Sha256::digest(&bytes)) != request.sha256
        {
            return Err(UploadError::Invalid);
        }
        if cancel.is_cancelled() {
            return Err(UploadError::Uncertain);
        }
        if let Some(node) = self.observed(request, &slot.document_id).await? {
            return Ok(UploadStep::Complete(node));
        }
        let UploadIntent::Create { name, .. } = &request.intent else {
            return Err(UploadError::Invalid);
        };
        let created = {
            let mut session = self.session.lock().await;
            // This method independently checks the exact parent and full bytes.
            // An error after the request may mean Apple committed it; no replay
            // follows from a missing or delayed listing.
            session
                .create_owned_file_with_slot(
                    &self.folder,
                    name,
                    &bytes,
                    false,
                    self.discard_registration_receipt
                        .swap(false, Ordering::AcqRel),
                    Some(&slot),
                )
                .await
                .map_err(|_| UploadError::Uncertain)?
                .ok_or(UploadError::Uncertain)?
        };
        if created.document_id != slot.document_id {
            return Err(UploadError::Conflict);
        }
        let node = self
            .observed(request, &slot.document_id)
            .await?
            .ok_or(UploadError::Uncertain)?;
        if node.id != created.id || node.etag.as_deref() != Some(created.etag.as_str()) {
            return Err(UploadError::Conflict);
        }
        Ok(UploadStep::Complete(node))
    }

    async fn upload_stream(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
        file: File,
        cancel: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        let saved = self.check_checkpoint(request, checkpoint)?;
        let slot = saved.slot.ok_or(UploadError::CheckpointInvalid)?;
        if self.reconciliation_only || saved.version != 3 || saved.receipt.is_some() {
            return Err(UploadError::Invalid);
        }
        let expected_size = request.size;
        let expected_hash = request.sha256.clone();
        let file = tokio::task::spawn_blocking(move || {
            let mut file = file;
            if file.metadata().map_err(|_| UploadError::Invalid)?.len() != expected_size {
                return Err(UploadError::Invalid);
            }
            let mut hash = Sha256::new();
            let mut buffer = [0u8; 64 * 1024];
            loop {
                let read = file.read(&mut buffer).map_err(|_| UploadError::Invalid)?;
                if read == 0 {
                    break;
                }
                hash.update(&buffer[..read]);
            }
            if hex::encode(hash.finalize()) != expected_hash {
                return Err(UploadError::Invalid);
            }
            file.seek(SeekFrom::Start(0))
                .map_err(|_| UploadError::Invalid)?;
            Ok(file)
        })
        .await
        .map_err(|_| UploadError::Invalid)??;
        if cancel.is_cancelled() {
            return Err(UploadError::Uncertain);
        }
        if let Some(node) = self.observed(request, &slot.document_id).await? {
            return Ok(UploadStep::Complete(node));
        }
        let UploadIntent::Create { name, .. } = &request.intent else {
            return Err(UploadError::Invalid);
        };
        let receipt = {
            let mut session = self.session.lock().await;
            if !session
                .list_root()
                .await
                .map_err(|_| UploadError::Uncertain)?
                .iter()
                .any(|entry| {
                    entry.drivewsid == self.folder.id
                        && entry.display_name() == self.folder.name
                        && entry.is_folder()
                })
            {
                return Err(UploadError::Conflict);
            }
            if session
                .list_folder(&self.folder.id)
                .await
                .map_err(|_| UploadError::Uncertain)?
                .iter()
                .any(|entry| entry.display_name() == *name)
            {
                return Err(UploadError::Conflict);
            }
            session
                .upload_stream_to_slot(&slot, name, file, request.size)
                .await
                .map_err(|_| UploadError::Uncertain)?
        };
        if self.discard_content_receipt.swap(false, Ordering::AcqRel) {
            return Err(UploadError::Uncertain);
        }
        // The shared worker saves this receipt in the vault before it invokes
        // commit_upload. A lost content response cannot have registered a file.
        Ok(UploadStep::Commit(self.checkpoint_with_receipt(
            request,
            Some(slot),
            Some(receipt),
        )?))
    }

    async fn commit_upload(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
        cancel: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        let saved = self.check_checkpoint(request, checkpoint)?;
        let slot = saved.slot.ok_or(UploadError::CheckpointInvalid)?;
        let receipt = saved.receipt.ok_or(UploadError::CheckpointInvalid)?;
        if saved.version != 3 || self.reconciliation_only {
            return Err(UploadError::Invalid);
        }
        if cancel.is_cancelled() {
            return Err(UploadError::Uncertain);
        }
        if let Some(node) = self.observed(request, &slot.document_id).await? {
            return Ok(UploadStep::Complete(node));
        }
        let UploadIntent::Create { name, .. } = &request.intent else {
            return Err(UploadError::Invalid);
        };
        let created = {
            let mut session = self.session.lock().await;
            if !session
                .list_root()
                .await
                .map_err(|_| UploadError::Uncertain)?
                .iter()
                .any(|entry| {
                    entry.drivewsid == self.folder.id
                        && entry.display_name() == self.folder.name
                        && entry.is_folder()
                })
            {
                return Err(UploadError::Conflict);
            }
            if session
                .list_folder(&self.folder.id)
                .await
                .map_err(|_| UploadError::Uncertain)?
                .iter()
                .any(|entry| entry.display_name() == *name)
            {
                return Err(UploadError::Conflict);
            }
            session
                .register_owned_file(OwnedRegistration {
                    folder: &self.folder,
                    name,
                    slot: &slot,
                    data: receipt,
                    size: request.size,
                    expected_bytes: None,
                    discard_receipt: self
                        .discard_registration_receipt
                        .swap(false, Ordering::AcqRel),
                })
                .await
                .map_err(|_| UploadError::Uncertain)?
                .ok_or(UploadError::Uncertain)?
        };
        if created.document_id != slot.document_id {
            return Err(UploadError::Conflict);
        }
        let node = self
            .observed(request, &slot.document_id)
            .await?
            .ok_or(UploadError::Uncertain)?;
        if node.id != created.id || node.etag.as_deref() != Some(created.etag.as_str()) {
            return Err(UploadError::Conflict);
        }
        Ok(UploadStep::Complete(node))
    }

    async fn reconcile_upload(
        &self,
        request: &UploadRequest,
        checkpoint: Option<&SecretString>,
        _: &CancellationToken,
    ) -> UploadResult<Reconciliation> {
        let saved =
            self.check_checkpoint(request, checkpoint.ok_or(UploadError::CheckpointInvalid)?)?;
        let Some(slot) = saved.slot else {
            return Ok(Reconciliation::Uncommitted);
        };
        let observed = self.observed(request, &slot.document_id).await?;
        if let Some(node) = observed {
            return Ok(Reconciliation::Committed(node));
        }
        if saved.version == 3 && saved.receipt.is_none() {
            // In v3 the content-only phase never sends add_file. Its signed
            // slot may hold orphaned bytes, but no visible item was created.
            // A later attempt allocates a fresh slot and document identity.
            return Ok(Reconciliation::Uncommitted);
        }
        Err(UploadError::Uncertain)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn fixture() -> (ICloudOwnedFixtureUpload, UploadRequest) {
        let mut session = ICloudReadSession::new().unwrap();
        session.account_hash = Some("synthetic-account".into());
        let scope = Scope {
            account: "fixture".into(),
            provider: "icloud".into(),
            collection: "drive".into(),
        };
        let folder = ValidationFolder {
            id: "FOLDER::com.apple.CloudDocs::fixture".into(),
            name: format!("Cirrove Write Validation-{}", Uuid::new_v4()),
        };
        let name = format!("staged-by-cirrove-{}.txt", Uuid::new_v4());
        let request = UploadRequest {
            scope: scope.clone(),
            intent: UploadIntent::Create {
                parent: folder.id.clone(),
                name,
            },
            size: 3,
            sha256: hex::encode(Sha256::digest(b"new")),
        };
        (
            ICloudOwnedFixtureUpload::new(scope, session, folder).unwrap(),
            request,
        )
    }

    fn slot() -> UploadSlot {
        UploadSlot {
            url: "https://example.icloud-content.com/upload".into(),
            document_id: "expected-document".into(),
        }
    }

    #[test]
    fn same_name_foreign_document_cannot_be_a_create_receipt() {
        let (provider, request) = fixture();
        let UploadIntent::Create { name, .. } = &request.intent else {
            panic!("expected create fixture");
        };
        let entry: DriveEntry = serde_json::from_value(serde_json::json!({
            "drivewsid": "FILE::com.apple.CloudDocs::foreign",
            "docwsid": "foreign",
            "name": name.strip_suffix(".txt").unwrap(),
            "extension": "txt",
            "type": "FILE",
            "size": request.size,
            "etag": "\"foreign\""
        }))
        .unwrap();
        assert!(
            provider
                .node(&entry, &request, "expected-document")
                .is_err()
        );
        assert!(
            ICloudOwnedFixtureUpload::select_candidate(&[entry.clone()], name, "expected-document")
                .is_err()
        );
        let mut expected = entry.clone();
        expected.drivewsid = "FILE::com.apple.CloudDocs::expected-document".into();
        expected.docwsid = "expected-document".into();
        assert!(
            provider
                .node(&expected, &request, "expected-document")
                .is_ok()
        );
        assert!(
            ICloudOwnedFixtureUpload::select_candidate(
                &[expected.clone()],
                name,
                "expected-document"
            )
            .unwrap()
            .is_some()
        );
        assert!(
            ICloudOwnedFixtureUpload::select_candidate(
                &[expected, entry],
                name,
                "expected-document"
            )
            .is_err()
        );
    }

    #[test]
    fn ordinary_unicode_name_is_allowed_only_in_the_owned_folder() {
        let (provider, mut request) = fixture();
        let UploadIntent::Create { name, .. } = &mut request.intent else {
            panic!("expected create fixture");
        };
        *name = "Résumé 2026 final.txt".into();
        assert!(provider.check_request(&request).is_ok());
        let UploadIntent::Create { parent, .. } = &mut request.intent else {
            panic!("expected create fixture");
        };
        *parent = "FOLDER::com.apple.CloudDocs::other".into();
        assert!(provider.check_request(&request).is_err());
    }

    #[test]
    fn a_streamed_binary_create_is_bounded_before_network() {
        let (provider, mut request) = fixture();
        request.size = 1024 * 1024;
        request.intent = UploadIntent::Create {
            parent: provider.folder.id.clone(),
            name: "Cirrove payload 1MiB.bin".into(),
        };
        assert!(provider.check_request(&request).is_ok());
        request.size = 20 * 1024 * 1024;
        assert!(provider.check_request(&request).is_ok());
        request.size = MAX_OWNED_FILE + 1;
        assert!(provider.check_request(&request).is_err());
    }

    #[test]
    fn streamed_create_checkpoint_preserves_exact_allocated_identity() {
        let (provider, mut request) = fixture();
        request.size = 20 * 1024 * 1024;
        let allocated = slot();
        let saved = provider
            .checkpoint(&request, Some(allocated.clone()))
            .unwrap();
        assert_eq!(
            provider.reserved_document_id(&request, &saved).unwrap(),
            Some(allocated.document_id)
        );
        let mut changed = request.clone();
        changed.size += 1;
        assert!(provider.check_checkpoint(&changed, &saved).is_err());
    }

    #[test]
    fn streamed_content_receipt_is_private_and_bound_to_the_request() {
        let (provider, mut request) = fixture();
        request.size = 20 * 1024 * 1024;
        let allocated = slot();
        let receipt: UploadedFile = serde_json::from_value(serde_json::json!({
            "receipt": "private-content-receipt",
            "fileChecksum": "private-signature",
            "referenceChecksum": "reference",
            "wrappingKey": "wrapping",
            "size": request.size,
        }))
        .unwrap();
        let saved = provider
            .checkpoint_with_receipt(&request, Some(allocated.clone()), Some(receipt))
            .unwrap();
        let checked = provider.check_checkpoint(&request, &saved).unwrap();
        assert_eq!(checked.version, 3);
        assert!(checked.receipt.is_some());
        assert!(
            !format!("{:?}", UploadStep::Commit(saved.clone())).contains("private-content-receipt")
        );
        let mut changed = request.clone();
        changed.sha256 = hex::encode(Sha256::digest(b"other"));
        assert!(provider.check_checkpoint(&changed, &saved).is_err());
        let mut forged: serde_json::Value = serde_json::from_str(saved.expose_secret()).unwrap();
        forged["receipt"]["size"] = serde_json::json!(1);
        let forged = SecretString::from(forged.to_string());
        assert!(provider.check_checkpoint(&request, &forged).is_err());
        let mut legacy: serde_json::Value = serde_json::from_str(saved.expose_secret()).unwrap();
        legacy["version"] = serde_json::json!(2);
        legacy.as_object_mut().unwrap().remove("receipt");
        let legacy = SecretString::from(legacy.to_string());
        assert!(provider.check_checkpoint(&request, &legacy).is_ok());
    }

    #[tokio::test]
    async fn create_checkpoint_is_bound_to_scope_name_and_bytes_before_network() {
        let (provider, request) = fixture();
        let UploadStep::Prepared(checkpoint) = provider
            .begin_upload(&request, &CancellationToken::new())
            .await
            .unwrap()
        else {
            panic!("expected a durable preparation before slot allocation");
        };
        assert!(
            provider
                .check_checkpoint(&request, &checkpoint)
                .unwrap()
                .slot
                .is_none()
        );
        assert_eq!(
            provider
                .check_checkpoint(&request, &checkpoint)
                .unwrap()
                .version,
            3
        );
        let mut legacy: serde_json::Value =
            serde_json::from_str(checkpoint.expose_secret()).unwrap();
        legacy["version"] = serde_json::json!(2);
        legacy.as_object_mut().unwrap().remove("receipt");
        let legacy = SecretString::from(legacy.to_string());
        assert!(provider.check_checkpoint(&request, &legacy).is_ok());
        let with_slot = provider.checkpoint(&request, Some(slot())).unwrap();
        assert!(provider.check_checkpoint(&request, &with_slot).is_ok());
        let mut other = request.clone();
        other.sha256 = hex::encode(Sha256::digest(b"old"));
        assert!(provider.check_checkpoint(&other, &with_slot).is_err());
        let mut other = request.clone();
        other.scope.account = "different".into();
        assert!(provider.check_checkpoint(&other, &with_slot).is_err());
        let mut other = request.clone();
        other.intent = UploadIntent::Replace {
            item: "existing".into(),
            expected_etag: "etag".into(),
        };
        assert!(
            provider
                .begin_upload(&other, &CancellationToken::new())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn malformed_or_mismatched_upload_never_contacts_apple() {
        let (provider, request) = fixture();
        let checkpoint = provider.checkpoint(&request, Some(slot())).unwrap();
        assert!(
            provider
                .upload_part(
                    &request,
                    &checkpoint,
                    1,
                    b"new".to_vec(),
                    &CancellationToken::new(),
                )
                .await
                .is_err()
        );
        assert!(
            provider
                .upload_part(
                    &request,
                    &checkpoint,
                    0,
                    b"bad".to_vec(),
                    &CancellationToken::new(),
                )
                .await
                .is_err()
        );
        assert!(
            provider
                .reconcile_upload(&request, None, &CancellationToken::new())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn restarted_validation_cannot_send_an_upload_part() {
        let (provider, request) = fixture();
        let provider = provider.reconciliation_only();
        let checkpoint = provider.checkpoint(&request, Some(slot())).unwrap();
        let error = provider
            .upload_part(
                &request,
                &checkpoint,
                0,
                b"new".to_vec(),
                &CancellationToken::new(),
            )
            .await
            .err()
            .unwrap();
        assert!(matches!(error, UploadError::Unsupported(_)));
    }
}
