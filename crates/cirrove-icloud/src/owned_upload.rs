//! A worker-driven native upload into one freshly created Cirrove test folder.
//! This remains feature-gated: uncertain Apple content-slot requests are never
//! replayed merely because a later listing does not yet show the file.
use super::{
    DriveEntry, ICloudReadSession, ValidationFolder, checked_content_url, write_probe::UploadSlot,
};
use async_trait::async_trait;
use cirrove_core::upload::{
    Reconciliation, Result as UploadResult, UploadError, UploadIntent, UploadProgress,
    UploadProvider, UploadRequest, UploadStep,
};
use cirrove_core::{CancellationToken, Node, NodeKind, Scope};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Mutex;
use uuid::Uuid;

const NAME_PREFIX: &str = "staged-by-cirrove-";
const MAX_OWNED_FILE: u64 = 4096;
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
}

/// Only accepts a `ValidationFolder` returned by this authenticated session's
/// live fixture creation. It is not constructed by the ordinary daemon.
pub struct ICloudOwnedFixtureUpload {
    scope: Scope,
    folder: ValidationFolder,
    session: Mutex<ICloudReadSession>,
    discard_registration_receipt: AtomicBool,
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

    fn check_request(&self, request: &UploadRequest) -> UploadResult<()> {
        request.validate()?;
        let UploadIntent::Create { parent, name } = &request.intent else {
            return Err(UploadError::Unsupported("owned fixture replacement"));
        };
        if request.scope != self.scope
            || parent != &self.folder.id
            || request.size == 0
            || request.size > MAX_OWNED_FILE
            || name
                .strip_prefix(NAME_PREFIX)
                .and_then(|id| id.strip_suffix(".txt"))
                .is_none_or(|id| Uuid::parse_str(id).is_err())
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
        self.check_request(request)?;
        let UploadIntent::Create { parent, name } = &request.intent else {
            return Err(UploadError::Invalid);
        };
        let checkpoint = CreateCheckpoint {
            version: 2,
            scope: request.scope.clone(),
            parent: parent.clone(),
            name: name.clone(),
            size: request.size,
            sha256: request.sha256.clone(),
            slot,
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
        if saved.version != 2
            || saved.scope != request.scope
            || saved.parent != *parent
            || saved.name != *name
            || saved.size != request.size
            || saved.sha256 != request.sha256
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
        let bytes = session
            .read_small_file_in_folder(&self.folder.id, &node.id)
            .await
            .map_err(|_| UploadError::Uncertain)?;
        if bytes.len() as u64 != request.size
            || hex::encode(Sha256::digest(bytes)) != request.sha256
        {
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
            return Ok(UploadStep::Continue(UploadProgress {
                checkpoint: self.checkpoint(request, Some(slot))?,
                offset: 0,
                length: request.size as u32,
            }));
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

    async fn commit_upload(
        &self,
        _: &UploadRequest,
        _: &SecretString,
        _: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        Err(UploadError::Invalid)
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
        self.observed(request, &slot.document_id)
            .await?
            .map(Reconciliation::Committed)
            .ok_or(UploadError::Uncertain)
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
        let name = format!("{NAME_PREFIX}{}.txt", Uuid::new_v4());
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
