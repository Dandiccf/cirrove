//! Deliberately narrow live-write experiment. Only a new, probe-named folder
//! and a new file inside that folder can be created. This is not WriteProvider.
use super::*;

const PROBE_PREFIX: &str = "Cirrove Write Validation-";
const PROBE_FILE: &str = "created-by-cirrove.txt";
const TRASH_ROOT: &str = "FOLDER::com.apple.CloudDocs::TRASH_ROOT";

pub struct TrashListingProbe {
    pub entries: usize,
    pub complete: bool,
    pub entries_with_restore_path: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub enum TrashRestoreOutcome {
    RestoredSameIdAndBytes,
    RestoredNewIdAndBytes,
    RestoredNewDocumentIdAndBytes,
    RestoredDifferentNameAndBytes,
    RestoredDifferentDocumentIdAndNameWithBytes,
    Indeterminate(&'static str),
}

/// Unforgeable by callers outside this module; obtained only after creating
/// and listing a fresh fixture through this session.
pub struct ValidationFolder {
    id: String,
    name: String,
}

pub struct ValidationFile {
    id: String,
    document_id: String,
    etag: String,
    name: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SameIdUpdateOutcome {
    Updated,
    RejectedUnchanged,
    Indeterminate,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RenameProbeOutcome {
    StaleAccepted,
    StaleRejectedFreshAccepted,
    StaleRejectedFreshRejected,
    Indeterminate,
}

#[derive(Debug, PartialEq, Eq)]
pub enum TrashProbeOutcome {
    StaleAcceptedAbsentFromParent,
    StaleRejectedCurrentIntact,
    StaleRejectedFreshAcceptedAbsentFromParent,
    StaleRejectedFreshRejectedCurrentIntact,
    Indeterminate,
}

#[derive(Debug, PartialEq, Eq)]
pub enum OccupiedNameOutcome {
    RejectedBothIntact,
    AcceptedDuplicateName,
    ConflictRenamedBothIntact,
    OriginalNoLongerListed,
    Indeterminate,
}

#[derive(Debug, PartialEq, Eq)]
pub enum HandoffOutcome {
    BothPreservedAtTargetAndRecovery,
    RecoveryMovedStagingUnchanged,
    Indeterminate,
}

/// Non-secret exact identities and content hashes needed to reconcile a
/// staged handoff after process death. The owning account is bound separately
/// by the private validation journal.
#[derive(Serialize, Deserialize)]
pub struct HandoffPlan {
    version: u8,
    folder_id: String,
    folder_name: String,
    original_id: String,
    original_doc_id: String,
    original_etag: String,
    staged_id: String,
    staged_doc_id: String,
    staged_etag: String,
    staged_name: String,
    recovery_name: String,
    original_sha256: String,
    staged_sha256: String,
}

/// A pre-request identity reservation for one unique staged upload. The
/// remote file ID is discovered only after Apple's registration request.
#[derive(Serialize, Deserialize)]
pub struct StagedRegistrationPlan {
    version: u8,
    folder_id: String,
    folder_name: String,
    original_id: String,
    original_doc_id: String,
    original_etag: String,
    original_sha256: String,
    staged_name: String,
    staged_sha256: String,
    staged_id: Option<String>,
    staged_doc_id: Option<String>,
    staged_etag: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoffObserved {
    Prepared,
    OldAtRecovery,
    Complete,
    Diverged,
}

#[derive(Deserialize)]
struct RenameReply {
    items: Vec<RenameResult>,
}

#[derive(Deserialize)]
struct RenameResult {
    status: String,
}

#[derive(Deserialize)]
struct TrashReply {
    items: Vec<TrashResult>,
}

#[derive(Deserialize)]
struct TrashResult {
    status: String,
}

#[derive(Deserialize)]
struct FolderReply {
    folders: Vec<FolderCreated>,
}

#[derive(Deserialize)]
struct FolderCreated {
    drivewsid: String,
    name: String,
    status: String,
}

#[derive(Deserialize)]
struct UploadSlot {
    url: String,
    document_id: String,
}

#[derive(Deserialize)]
struct Uploaded {
    #[serde(rename = "singleFile")]
    file: UploadedFile,
}

#[derive(Deserialize)]
struct UploadedFile {
    receipt: String,
    #[serde(rename = "fileChecksum")]
    signature: String,
    #[serde(rename = "referenceChecksum")]
    reference_signature: String,
    #[serde(rename = "wrappingKey")]
    wrapping_key: String,
    size: u64,
}

#[derive(Deserialize)]
struct UpdateReply {
    status: UpdateStatus,
    results: Vec<UpdateResult>,
}

#[derive(Deserialize)]
struct UpdateStatus {
    status_code: i64,
}

#[derive(Deserialize)]
struct UpdateResult {
    status: UpdateStatus,
    document: Option<UpdatedDocument>,
}

#[derive(Deserialize)]
struct UpdatedDocument {
    document_id: String,
    name: String,
    deleted: bool,
}

fn exactly_one<T>(mut values: Vec<T>, stage: &'static str) -> Result<T> {
    if values.len() != 1 {
        bail!("iCloud {stage} returned an unexpected result count");
    }
    Ok(values.remove(0))
}

impl ICloudReadSession {
    /// Bounded read-only inspection of Apple's special Trash root. It may
    /// lack the ordinary folder `type` field, so this deliberately avoids
    /// treating it as a normal mounted directory.
    pub async fn inspect_trash_listing(&mut self) -> Result<TrashListingProbe> {
        let (items, complete) = self.read_trash_items().await?;
        Ok(TrashListingProbe {
            entries: items.len(),
            complete,
            entries_with_restore_path: items
                .iter()
                .filter(|item| item.get("restorePath").is_some_and(|path| !path.is_null()))
                .count(),
        })
    }

    async fn read_trash_items(&mut self) -> Result<(Vec<serde_json::Value>, bool)> {
        let endpoint = self
            .drive_endpoint
            .as_ref()
            .context("iCloud sign-in is not complete")?
            .join("retrieveItemDetailsInFolders")?;
        let response = self
            .http
            .post(endpoint)
            .timeout(LISTING_TIMEOUT)
            .header("origin", ICLOUD_ORIGIN)
            .header("referer", format!("{ICLOUD_ORIGIN}/"))
            .json(&json!([{"drivewsid": TRASH_ROOT, "partialData": false}]))
            .send()
            .await
            .map_err(|_| anyhow!("iCloud validation Trash listing request failed"))?;
        if !response.status().is_success() {
            return Err(drive_request_failure(
                response.status(),
                "iCloud validation Trash listing",
            ));
        }
        let folders: serde_json::Value =
            read_json(response, "iCloud validation Trash listing").await?;
        let folder = folders
            .as_array()
            .filter(|folders| folders.len() == 1)
            .and_then(|folders| folders.first())
            .context("iCloud Trash listing returned an unexpected envelope")?;
        let returned_root = folder.get("drivewsid").and_then(|value| value.as_str());
        if !matches!(returned_root, Some(TRASH_ROOT | "TRASH_ROOT")) {
            bail!("iCloud Trash listing returned a different root ID");
        }
        let items = folder
            .get("items")
            .and_then(|value| value.as_array())
            .context("iCloud Trash listing has no item array")?;
        if items.iter().any(|item| {
            item.get("drivewsid")
                .and_then(|value| value.as_str())
                .is_none_or(str::is_empty)
        }) {
            bail!("iCloud Trash listing contains an item without ID");
        }
        let complete = folder
            .get("numberOfItems")
            .and_then(|value| value.as_u64())
            .is_some_and(|count| count == items.len() as u64);
        Ok((items.clone(), complete))
    }

    async fn upload_probe_bytes(
        &mut self,
        name: &str,
        bytes: &[u8],
    ) -> Result<(UploadSlot, UploadedFile)> {
        let endpoint = self
            .docs_endpoint
            .as_ref()
            .context("iCloud sign-in is not complete")?;
        let slot_url = endpoint.join("ws/com.apple.CloudDocs/upload/web")?;
        let response = self
            .http
            .post(slot_url)
            .header("origin", ICLOUD_ORIGIN)
            .header("referer", format!("{ICLOUD_ORIGIN}/"))
            .json(&json!({
                "filename": name,
                "type": "FILE",
                "size": bytes.len().to_string(),
                "content_type": "text/plain"
            }))
            .send()
            .await
            .map_err(|_| anyhow!("iCloud validation upload allocation failed"))?;
        if !response.status().is_success() {
            return Err(drive_request_failure(
                response.status(),
                "iCloud validation upload allocation",
            ));
        }
        let slot: UploadSlot = exactly_one(
            read_json::<Vec<UploadSlot>>(response, "iCloud upload allocation").await?,
            "upload allocation",
        )?;
        if slot.document_id.is_empty() || slot.document_id.len() > 256 {
            bail!("iCloud upload allocation lacks a document identity");
        }
        let upload_url = checked_content_url(&slot.url)?;
        let response = self
            .http
            .post(upload_url)
            .header("content-type", "text/plain")
            .body(bytes.to_vec())
            .send()
            .await
            .map_err(|_| anyhow!("iCloud validation content upload failed"))?;
        if !response.status().is_success() {
            return Err(drive_request_failure(
                response.status(),
                "iCloud validation content upload",
            ));
        }
        let uploaded: Uploaded = read_json(response, "iCloud validation content upload").await?;
        let data = uploaded.file;
        if data.size != bytes.len() as u64 || data.receipt.is_empty() || data.signature.is_empty() {
            bail!("iCloud upload receipt is incomplete");
        }
        Ok((slot, data))
    }

    /// Create a unique fixture at the root. The caller must keep its returned ID;
    /// no retry is safe if the response is lost after Apple commits the request.
    pub async fn create_validation_folder(&mut self, name: &str) -> Result<ValidationFolder> {
        if !name.starts_with(PROBE_PREFIX) || Uuid::parse_str(&name[PROBE_PREFIX.len()..]).is_err()
        {
            bail!("write probe requires a fresh Cirrove validation folder name");
        }
        let root = self.list_root().await?;
        if root.iter().any(|entry| entry.display_name() == name) {
            bail!("iCloud validation folder already exists");
        }
        let endpoint = self
            .drive_endpoint
            .as_ref()
            .context("iCloud sign-in is not complete")?;
        let url = endpoint.join("createFolders")?;
        let response = self
            .http
            .post(url)
            .header("origin", ICLOUD_ORIGIN)
            .header("referer", format!("{ICLOUD_ORIGIN}/"))
            .json(&json!({
                "destinationDrivewsId": ROOT_ID,
                "folders": [{
                    "clientId": format!("FOLDER::UNKNOWN_ZONE::TempId-{}", Uuid::new_v4()),
                    "name": name
                }]
            }))
            .send()
            .await
            .map_err(|_| anyhow!("iCloud validation folder request failed"))?;
        if !response.status().is_success() {
            return Err(drive_request_failure(
                response.status(),
                "iCloud validation folder creation",
            ));
        }
        let reply: FolderReply = read_json(response, "iCloud validation folder creation").await?;
        let created = exactly_one(reply.folders, "folder creation")?;
        if created.status != "OK"
            || created.name != name
            || !created.drivewsid.starts_with("FOLDER::")
        {
            bail!("iCloud did not confirm the validation folder identity");
        }
        let listed = self.list_root().await?;
        if !listed.iter().any(|entry| {
            entry.drivewsid == created.drivewsid
                && entry.display_name() == name
                && entry.is_folder()
        }) {
            bail!("iCloud did not list the created validation folder");
        }
        Ok(ValidationFolder {
            id: created.drivewsid,
            name: name.into(),
        })
    }

    /// Upload at most 4 KiB into a folder just created by this process. No
    /// overwrite, rename, trash, or general path-based write is exposed.
    pub async fn create_validation_file(
        &mut self,
        folder: &ValidationFolder,
        bytes: &[u8],
    ) -> Result<ValidationFile> {
        self.create_owned_file(folder, PROBE_FILE, bytes, true, false)
            .await
            .and_then(|file| file.context("iCloud validation file receipt was discarded"))
    }

    /// A second, uniquely named upload, only beside this process's original
    /// fixture. It cannot target an existing folder or overwrite a name.
    pub async fn create_staged_file(
        &mut self,
        folder: &ValidationFolder,
        original: &ValidationFile,
        bytes: &[u8],
    ) -> Result<ValidationFile> {
        let children = self.list_folder(&folder.id).await?;
        if children.len() != 1
            || children[0].drivewsid != original.id
            || children[0].display_name() != PROBE_FILE
            || original.name != PROBE_FILE
        {
            bail!("iCloud validation folder does not contain only the original fixture");
        }
        let name = format!("staged-by-cirrove-{}.txt", Uuid::new_v4());
        self.create_owned_file(folder, &name, bytes, false, false)
            .await
            .and_then(|file| file.context("iCloud staged file receipt was discarded"))
    }

    pub async fn prepare_staged_registration(
        &mut self,
        folder: &ValidationFolder,
        original: &ValidationFile,
        original_bytes: &[u8],
        staged_bytes: &[u8],
    ) -> Result<StagedRegistrationPlan> {
        if original.name != PROBE_FILE
            || original_bytes.is_empty()
            || staged_bytes.is_empty()
            || original_bytes.len() > 4096
            || staged_bytes.len() > 4096
        {
            bail!("invalid staged iCloud registration fixture");
        }
        let mut plan = StagedRegistrationPlan {
            version: 2,
            folder_id: folder.id.clone(),
            folder_name: folder.name.clone(),
            original_id: original.id.clone(),
            original_doc_id: original.document_id.clone(),
            original_etag: original.etag.clone(),
            original_sha256: hex::encode(Sha256::digest(original_bytes)),
            staged_name: format!("staged-by-cirrove-{}.txt", Uuid::new_v4()),
            staged_sha256: hex::encode(Sha256::digest(staged_bytes)),
            staged_id: None,
            staged_doc_id: None,
            staged_etag: None,
        };
        if self.inspect_staged_registration(&mut plan).await? {
            bail!("staged iCloud registration name was already present");
        }
        Ok(plan)
    }

    /// Deliberately discard the registration response after the request was
    /// sent. The plan must have been durably saved before this call.
    pub async fn send_staged_registration_discard_receipt(
        &mut self,
        plan: &mut StagedRegistrationPlan,
        bytes: &[u8],
    ) -> Result<()> {
        plan.validate()?;
        if plan.staged_id.is_some()
            || hex::encode(Sha256::digest(bytes)) != plan.staged_sha256
            || self.inspect_staged_registration(plan).await?
        {
            bail!("staged iCloud registration is no longer prepared");
        }
        let folder = ValidationFolder {
            id: plan.folder_id.clone(),
            name: plan.folder_name.clone(),
        };
        let receipt = self
            .create_owned_file(&folder, &plan.staged_name, bytes, false, true)
            .await?;
        if receipt.is_some() {
            bail!("staged registration receipt was unexpectedly retained");
        }
        Ok(())
    }

    /// Reconcile by an exact parent ID and a UUID-reserved name, then bind
    /// the discovered remote ID only after full-byte verification. Absence is
    /// uncertain after a pending request and must not trigger a replay.
    pub async fn inspect_staged_registration(
        &mut self,
        plan: &mut StagedRegistrationPlan,
    ) -> Result<bool> {
        plan.validate()?;
        if !self.list_root().await?.iter().any(|entry| {
            entry.drivewsid == plan.folder_id
                && entry.display_name() == plan.folder_name
                && entry.is_folder()
        }) {
            bail!("staged iCloud parent identity changed");
        }
        let children = self.list_folder(&plan.folder_id).await?;
        if children.is_empty() || children.len() > 2 {
            bail!("staged iCloud parent has an unexpected item count");
        }
        let old = exactly_one(
            children
                .iter()
                .filter(|entry| entry.drivewsid == plan.original_id)
                .collect(),
            "staged registration original",
        )?;
        if old.is_folder()
            || old.docwsid != plan.original_doc_id
            || old.etag != plan.original_etag
            || old.display_name() != PROBE_FILE
            || old.size > 4096
            || hex::encode(Sha256::digest(
                self.read_small_file_in_folder(&plan.folder_id, &plan.original_id)
                    .await?,
            )) != plan.original_sha256
        {
            bail!("staged iCloud original changed");
        }
        if children.len() == 1 {
            if plan.staged_id.is_some() {
                bail!("previously bound staged iCloud ID disappeared");
            }
            return Ok(false);
        }
        let stage = exactly_one(
            children
                .iter()
                .filter(|entry| entry.display_name() == plan.staged_name)
                .collect(),
            "staged registration candidate",
        )?;
        if stage.is_folder()
            || stage.drivewsid == plan.original_id
            || stage.size > 4096
            || plan
                .staged_id
                .as_ref()
                .is_some_and(|id| id != &stage.drivewsid)
            || plan
                .staged_doc_id
                .as_ref()
                .is_some_and(|id| id != &stage.docwsid)
            || plan
                .staged_etag
                .as_ref()
                .is_some_and(|etag| etag != &stage.etag)
            || hex::encode(Sha256::digest(
                self.read_small_file_in_folder(&plan.folder_id, &stage.drivewsid)
                    .await?,
            )) != plan.staged_sha256
        {
            bail!("staged iCloud candidate identity or bytes differ");
        }
        plan.staged_id = Some(stage.drivewsid.clone());
        plan.staged_doc_id = Some(stage.docwsid.clone());
        plan.staged_etag = Some(stage.etag.clone());
        Ok(true)
    }

    /// Convert a verified, bound registration checkpoint into the existing
    /// two-ID handoff plan. No mutation occurs until the handoff plan is
    /// separately fsynced by its journal.
    pub async fn prepare_handoff_from_registration(
        &mut self,
        registration: &mut StagedRegistrationPlan,
    ) -> Result<HandoffPlan> {
        if !self.inspect_staged_registration(registration).await? {
            bail!("staged registration is not visible for handoff");
        }
        let plan = HandoffPlan {
            version: 2,
            folder_id: registration.folder_id.clone(),
            folder_name: registration.folder_name.clone(),
            original_id: registration.original_id.clone(),
            original_doc_id: registration.original_doc_id.clone(),
            original_etag: registration.original_etag.clone(),
            staged_id: registration
                .staged_id
                .clone()
                .context("staged registration has no item identity")?,
            staged_doc_id: registration
                .staged_doc_id
                .clone()
                .context("staged registration has no document identity")?,
            staged_etag: registration
                .staged_etag
                .clone()
                .context("staged registration has no item revision")?,
            staged_name: registration.staged_name.clone(),
            recovery_name: format!("recovery-by-cirrove-{}.txt", Uuid::new_v4()),
            original_sha256: registration.original_sha256.clone(),
            staged_sha256: registration.staged_sha256.clone(),
        };
        if self.inspect_durable_handoff(&plan).await? != HandoffObserved::Prepared {
            bail!("registered staged file changed before handoff");
        }
        Ok(plan)
    }

    async fn create_owned_file(
        &mut self,
        folder: &ValidationFolder,
        name: &str,
        bytes: &[u8],
        require_empty: bool,
        discard_registration_receipt: bool,
    ) -> Result<Option<ValidationFile>> {
        let folder_id = folder.id.as_str();
        if bytes.is_empty()
            || bytes.len() > 4096
            || !folder_id.starts_with("FOLDER::com.apple.CloudDocs::")
        {
            bail!("invalid iCloud validation upload");
        }
        if !self.list_root().await?.iter().any(|entry| {
            entry.drivewsid == folder.id && entry.display_name() == folder.name && entry.is_folder()
        }) {
            bail!("iCloud validation folder is no longer at the root");
        }
        let before = self.list_folder(folder_id).await?;
        if (require_empty && !before.is_empty())
            || before.iter().any(|entry| entry.display_name() == name)
        {
            bail!("iCloud validation destination is occupied");
        }
        let (slot, data) = self.upload_probe_bytes(name, bytes).await?;
        let starting_document_id = folder_id
            .rsplit("::")
            .next()
            .context("invalid folder identity")?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis() as u64;
        let update_url = self
            .docs_endpoint
            .as_ref()
            .context("iCloud sign-in is not complete")?
            .join("ws/com.apple.CloudDocs/update/documents")?;
        let response = self
            .http
            .post(update_url)
            .header("origin", ICLOUD_ORIGIN)
            .header("referer", format!("{ICLOUD_ORIGIN}/"))
            .json(&json!({
                "allow_conflict": false,
                "btime": now,
                "command": "add_file",
                "create_short_guid": true,
                "data": {
                    "receipt": data.receipt,
                    "reference_signature": data.reference_signature,
                    "signature": data.signature,
                    "size": data.size,
                    "wrapping_key": data.wrapping_key
                },
                "document_id": slot.document_id,
                "file_flags": {"is_executable": false, "is_hidden": false, "is_writable": true},
                "mtime": now,
                "path": {"path": name, "starting_document_id": starting_document_id}
            }))
            .send()
            .await
            .map_err(|_| anyhow!("iCloud validation file registration failed"))?;
        if discard_registration_receipt {
            // The response exists, but this process intentionally never parses
            // it or learns the resulting remote item ID.
            drop(response);
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(drive_request_failure(
                response.status(),
                "iCloud validation file registration",
            ));
        }
        let reply: UpdateReply = read_json(response, "iCloud validation file registration").await?;
        let result = exactly_one(reply.results, "file registration")?;
        if reply.status.status_code != 0 || result.status.status_code != 0 {
            bail!("iCloud rejected validation file registration");
        }
        let document = result
            .document
            .context("iCloud did not return the registered file")?;
        if document.deleted || document.name != name || document.document_id != slot.document_id {
            bail!("iCloud returned a different validation file");
        }
        let matches: Vec<_> = self
            .list_folder(folder_id)
            .await?
            .into_iter()
            .filter(|entry| entry.display_name() == name)
            .collect();
        let entry = exactly_one(matches, "validation file listing")?;
        if entry.is_folder()
            || entry.docwsid != document.document_id
            || entry.size != bytes.len() as u64
        {
            bail!("iCloud validation file listing differs from the upload");
        }
        let readback = self
            .read_small_file_in_folder(folder_id, &entry.drivewsid)
            .await?;
        if readback != bytes {
            bail!("iCloud validation file readback differs from the upload");
        }
        Ok(Some(ValidationFile {
            id: entry.drivewsid,
            document_id: entry.docwsid,
            etag: entry.etag,
            name: name.into(),
        }))
    }

    /// One intentionally narrow experiment against the file created above.
    /// The caller supplies its original bytes so a rejected request can be
    /// distinguished from a response whose effect is unknown. No retry occurs.
    pub async fn probe_same_id_update(
        &mut self,
        folder: &ValidationFolder,
        file: &ValidationFile,
        old_bytes: &[u8],
        new_bytes: &[u8],
    ) -> Result<SameIdUpdateOutcome> {
        if old_bytes.is_empty()
            || new_bytes.is_empty()
            || new_bytes.len() > 4096
            || old_bytes == new_bytes
            || file.etag.is_empty()
        {
            bail!("invalid iCloud validation update");
        }
        let items = self.list_folder(&folder.id).await?;
        let entry = exactly_one(
            items
                .iter()
                .filter(|entry| entry.drivewsid == file.id)
                .collect(),
            "validation base lookup",
        )?;
        if entry.docwsid != file.document_id
            || entry.etag != file.etag
            || entry.display_name() != PROBE_FILE
            || self.read_small_file_in_folder(&folder.id, &file.id).await? != old_bytes
        {
            bail!("iCloud validation base has changed");
        }
        let accepted = self
            .send_same_id_update(&folder.id, &file.document_id, &file.etag, new_bytes, false)
            .await?;
        let after = self.list_folder(&folder.id).await?;
        let original: Vec<_> = after
            .iter()
            .filter(|entry| entry.drivewsid == file.id)
            .collect();
        if original.len() != 1
            || after
                .iter()
                .filter(|entry| entry.display_name() == PROBE_FILE)
                .count()
                != 1
        {
            return Ok(SameIdUpdateOutcome::Indeterminate);
        }
        let original = original[0];
        if original.docwsid != file.document_id || original.is_folder() {
            return Ok(SameIdUpdateOutcome::Indeterminate);
        }
        let bytes = self.read_small_file_in_folder(&folder.id, &file.id).await?;
        if accepted && bytes == new_bytes {
            Ok(SameIdUpdateOutcome::Updated)
        } else if !accepted && original.etag == file.etag && bytes == old_bytes {
            Ok(SameIdUpdateOutcome::RejectedUnchanged)
        } else {
            Ok(SameIdUpdateOutcome::Indeterminate)
        }
    }

    async fn send_same_id_update(
        &mut self,
        folder_id: &str,
        document_id: &str,
        etag: &str,
        bytes: &[u8],
        http_if_match: bool,
    ) -> Result<bool> {
        let (_slot, data) = self.upload_probe_bytes(PROBE_FILE, bytes).await?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis() as u64;
        let starting_document_id = folder_id
            .rsplit("::")
            .next()
            .context("invalid folder identity")?;
        let endpoint = self
            .docs_endpoint
            .as_ref()
            .context("iCloud sign-in is not complete")?
            .join("ws/com.apple.CloudDocs/update/documents")?;
        let mut request = self
            .http
            .post(endpoint)
            .header("origin", ICLOUD_ORIGIN)
            .header("referer", format!("{ICLOUD_ORIGIN}/"))
            .json(&json!({
                "allow_conflict": false,
                "btime": now,
                "command": "add_file",
                "create_short_guid": false,
                "data": {
                    "receipt": data.receipt,
                    "reference_signature": data.reference_signature,
                    "signature": data.signature,
                    "size": data.size,
                    "wrapping_key": data.wrapping_key
                },
                "document_id": document_id,
                "etag": etag,
                "file_flags": {"is_executable": false, "is_hidden": false, "is_writable": true},
                "mtime": now,
                "path": {"path": PROBE_FILE, "starting_document_id": starting_document_id}
            }));
        if http_if_match {
            let tag = etag.trim_matches('"');
            if tag.is_empty() || tag.contains('"') || tag.contains('\n') || tag.contains('\r') {
                bail!("iCloud ETag cannot be sent as an HTTP precondition");
            }
            request = request.header(reqwest::header::IF_MATCH, format!("\"{tag}\""));
        }
        let response = request
            .send()
            .await
            .map_err(|_| anyhow!("iCloud same-ID update request failed"))?;
        if response.status().is_success() {
            let reply: UpdateReply = read_json(response, "iCloud same-ID update").await?;
            Ok(reply.status.status_code == 0
                && reply.results.len() == 1
                && reply.results[0].status.status_code == 0
                && reply.results[0].document.as_ref().is_some_and(|document| {
                    document.document_id == document_id
                        && document.name == PROBE_FILE
                        && !document.deleted
                }))
        } else {
            Ok(false)
        }
    }

    /// Reuse the *original* ETag after one confirmed same-ID update. All bytes
    /// and identities belong to the newly created validation fixture.
    pub async fn probe_stale_etag_update(
        &mut self,
        folder: &ValidationFolder,
        file: &ValidationFile,
        current_bytes: &[u8],
        candidate_bytes: &[u8],
        http_if_match: bool,
    ) -> Result<SameIdUpdateOutcome> {
        if current_bytes.is_empty()
            || current_bytes.len() != candidate_bytes.len()
            || current_bytes == candidate_bytes
        {
            bail!("stale-revision probe requires different, same-size bytes");
        }
        let before = self.list_folder(&folder.id).await?;
        let entry = exactly_one(
            before
                .iter()
                .filter(|entry| entry.drivewsid == file.id)
                .collect(),
            "stale-revision base lookup",
        )?;
        if entry.docwsid != file.document_id
            || entry.display_name() != PROBE_FILE
            || entry.etag.is_empty()
            || entry.etag == file.etag
            || self.read_small_file_in_folder(&folder.id, &file.id).await? != current_bytes
        {
            bail!("iCloud did not expose the expected newer validation revision");
        }
        let current_etag = entry.etag.clone();
        let accepted = self
            .send_same_id_update(
                &folder.id,
                &file.document_id,
                &file.etag,
                candidate_bytes,
                http_if_match,
            )
            .await?;
        let after = self.list_folder(&folder.id).await?;
        let same_name: Vec<_> = after
            .iter()
            .filter(|entry| entry.display_name() == PROBE_FILE)
            .collect();
        if same_name.len() != 1
            || same_name[0].drivewsid != file.id
            || same_name[0].docwsid != file.document_id
        {
            return Ok(SameIdUpdateOutcome::Indeterminate);
        }
        let bytes = self.read_small_file_in_folder(&folder.id, &file.id).await?;
        if accepted && bytes == candidate_bytes {
            Ok(SameIdUpdateOutcome::Updated)
        } else if !accepted && same_name[0].etag == current_etag && bytes == current_bytes {
            Ok(SameIdUpdateOutcome::RejectedUnchanged)
        } else {
            Ok(SameIdUpdateOutcome::Indeterminate)
        }
    }

    /// On a fresh fixture only, ask whether a stale ETag prevents moving an
    /// exact item ID out of its parent. The request is sent once and never
    /// retried; absence from the parent does not by itself prove Trash state.
    pub async fn probe_stale_etag_trash(
        &mut self,
        folder: &ValidationFolder,
        file: &ValidationFile,
        current_bytes: &[u8],
        fresh_followup: bool,
    ) -> Result<TrashProbeOutcome> {
        if current_bytes.is_empty() || current_bytes.len() > 4096 || file.name != PROBE_FILE {
            bail!("invalid iCloud trash validation fixture");
        }
        let before = self.list_folder(&folder.id).await?;
        let entry = exactly_one(
            before
                .iter()
                .filter(|entry| entry.drivewsid == file.id)
                .collect(),
            "trash base lookup",
        )?;
        if before.len() != 1
            || entry.is_folder()
            || entry.docwsid != file.document_id
            || entry.display_name() != PROBE_FILE
            || entry.etag.is_empty()
            || entry.etag == file.etag
            || self.read_small_file_in_folder(&folder.id, &file.id).await? != current_bytes
        {
            bail!("iCloud trash base is not the expected newer fixture");
        }
        let current_etag = entry.etag.clone();
        let accepted = self.send_trash(&file.id, &file.etag).await?;
        let after = self.list_folder(&folder.id).await?;
        if accepted && after.iter().all(|entry| entry.drivewsid != file.id) {
            return Ok(TrashProbeOutcome::StaleAcceptedAbsentFromParent);
        }
        if !accepted
            && after.len() == 1
            && after[0].drivewsid == file.id
            && after[0].docwsid == file.document_id
            && after[0].etag == current_etag
            && self.read_small_file_in_folder(&folder.id, &file.id).await? == current_bytes
        {
            if !fresh_followup {
                return Ok(TrashProbeOutcome::StaleRejectedCurrentIntact);
            }
            let fresh_accepted = self.send_trash(&file.id, &current_etag).await?;
            let after_fresh = self.list_folder(&folder.id).await?;
            if fresh_accepted && after_fresh.iter().all(|entry| entry.drivewsid != file.id) {
                return Ok(TrashProbeOutcome::StaleRejectedFreshAcceptedAbsentFromParent);
            }
            if !fresh_accepted
                && after_fresh.len() == 1
                && after_fresh[0].drivewsid == file.id
                && after_fresh[0].docwsid == file.document_id
                && after_fresh[0].etag == current_etag
                && self.read_small_file_in_folder(&folder.id, &file.id).await? == current_bytes
            {
                return Ok(TrashProbeOutcome::StaleRejectedFreshRejectedCurrentIntact);
            }
        }
        Ok(TrashProbeOutcome::Indeterminate)
    }

    async fn send_trash(&mut self, item_id: &str, etag: &str) -> Result<bool> {
        let endpoint = self
            .drive_endpoint
            .as_ref()
            .context("iCloud sign-in is not complete")?
            .join("moveItemsToTrash")?;
        let response = self
            .http
            .post(endpoint)
            .header("origin", ICLOUD_ORIGIN)
            .header("referer", format!("{ICLOUD_ORIGIN}/"))
            .json(&json!({"items": [{
                "drivewsid": item_id,
                "etag": etag,
                "clientId": item_id
            }]}))
            .send()
            .await
            .map_err(|_| anyhow!("iCloud validation trash request failed"))?;
        if response.status().is_success() {
            let reply: TrashReply = read_json(response, "iCloud validation trash").await?;
            Ok(exactly_one(reply.items, "trash")?.status == "OK")
        } else {
            Ok(false)
        }
    }

    /// Exercise recoverable deletion on one newly created fixture. Every
    /// remote request is sent once; an uncertain result stops for inspection.
    pub async fn probe_trash_restore_cycle(
        &mut self,
        folder: &ValidationFolder,
        file: &ValidationFile,
        bytes: &[u8],
    ) -> Result<TrashRestoreOutcome> {
        if bytes.is_empty() || bytes.len() > 4096 || file.name != PROBE_FILE {
            bail!("invalid iCloud Trash restore validation fixture");
        }
        let before = self.list_folder(&folder.id).await?;
        if before.len() != 1
            || before[0].drivewsid != file.id
            || before[0].docwsid != file.document_id
            || before[0].display_name() != PROBE_FILE
            || before[0].etag != file.etag
            || self.read_small_file_in_folder(&folder.id, &file.id).await? != bytes
        {
            bail!("iCloud Trash restore base changed");
        }
        let accepted = self.send_trash(&file.id, &file.etag).await?;
        if !accepted
            || self
                .list_folder(&folder.id)
                .await?
                .iter()
                .any(|entry| entry.drivewsid == file.id)
        {
            return Ok(TrashRestoreOutcome::Indeterminate("trash_not_confirmed"));
        }
        let (trash, complete) = self.read_trash_items().await?;
        if !complete {
            return Ok(TrashRestoreOutcome::Indeterminate(
                "trash_listing_incomplete",
            ));
        }
        let candidate = exactly_one(
            trash
                .iter()
                .filter(|item| {
                    item.get("drivewsid").and_then(|id| id.as_str()) == Some(file.id.as_str())
                })
                .collect(),
            "validation item in Trash",
        )?;
        let etag = candidate
            .get("etag")
            .and_then(|etag| etag.as_str())
            .filter(|etag| !etag.is_empty())
            .context("iCloud Trash item has no ETag")?;
        if candidate
            .get("restorePath")
            .is_none_or(|path| path.is_null())
            || candidate
                .get("docwsid")
                .and_then(|doc| doc.as_str())
                .is_some_and(|doc| !doc.is_empty() && doc != file.document_id)
        {
            bail!("iCloud Trash item lacks expected recovery identity");
        }
        let endpoint = self
            .drive_endpoint
            .as_ref()
            .context("iCloud sign-in is not complete")?
            .join("putBackItemsFromTrash")?;
        let response = self
            .http
            .post(endpoint)
            .header("origin", ICLOUD_ORIGIN)
            .header("referer", format!("{ICLOUD_ORIGIN}/"))
            .json(&json!({"items": [{"drivewsid": file.id, "etag": etag}]}))
            .send()
            .await
            .map_err(|_| anyhow!("iCloud validation Trash restore request failed"))?;
        let restored = if response.status().is_success() {
            let reply: TrashReply = read_json(response, "iCloud validation Trash restore").await?;
            exactly_one(reply.items, "Trash restore")?.status == "OK"
        } else {
            false
        };
        let after = self.list_folder(&folder.id).await?;
        if !restored {
            return Ok(TrashRestoreOutcome::Indeterminate("restore_not_accepted"));
        }
        if after.len() != 1 {
            return Ok(TrashRestoreOutcome::Indeterminate("restore_parent_count"));
        }
        let item = &after[0];
        if item.is_folder() {
            return Ok(TrashRestoreOutcome::Indeterminate("restore_kind"));
        }
        if self
            .read_small_file_in_folder(&folder.id, &item.drivewsid)
            .await?
            != bytes
        {
            return Ok(TrashRestoreOutcome::Indeterminate("restore_bytes"));
        }
        let same_document = item.docwsid == file.document_id;
        let same_name = item.display_name() == PROBE_FILE;
        if !same_document && !same_name {
            return Ok(TrashRestoreOutcome::RestoredDifferentDocumentIdAndNameWithBytes);
        }
        if !same_document {
            return Ok(TrashRestoreOutcome::RestoredNewDocumentIdAndBytes);
        }
        if !same_name {
            return Ok(TrashRestoreOutcome::RestoredDifferentNameAndBytes);
        }
        Ok(if item.drivewsid == file.id {
            TrashRestoreOutcome::RestoredSameIdAndBytes
        } else {
            TrashRestoreOutcome::RestoredNewIdAndBytes
        })
    }

    async fn send_rename(&mut self, item_id: &str, etag: &str, name: &str) -> Result<bool> {
        let endpoint = self
            .drive_endpoint
            .as_ref()
            .context("iCloud sign-in is not complete")?
            .join("renameItems")?;
        let response = self
            .http
            .post(endpoint)
            .header("origin", ICLOUD_ORIGIN)
            .header("referer", format!("{ICLOUD_ORIGIN}/"))
            .json(&json!({"items": [{"drivewsid": item_id, "name": name, "etag": etag}]}))
            .send()
            .await
            .map_err(|_| anyhow!("iCloud validation rename request failed"))?;
        if !response.status().is_success() {
            return Ok(false);
        }
        let reply: RenameReply = read_json(response, "iCloud validation rename").await?;
        let item = exactly_one(reply.items, "rename")?;
        Ok(item.status == "OK")
    }

    /// First submit the original stale ETag for a rename, then the newly
    /// observed ETag if and only if the stale request left the fixture intact.
    pub async fn probe_conditional_rename(
        &mut self,
        folder: &ValidationFolder,
        file: &ValidationFile,
        current_bytes: &[u8],
    ) -> Result<RenameProbeOutcome> {
        const NEW_NAME: &str = "renamed-by-cirrove.txt";
        let before = self.list_folder(&folder.id).await?;
        let entry = exactly_one(
            before
                .iter()
                .filter(|entry| entry.drivewsid == file.id)
                .collect(),
            "rename base lookup",
        )?;
        if entry.docwsid != file.document_id
            || entry.display_name() != PROBE_FILE
            || entry.etag.is_empty()
            || entry.etag == file.etag
            || self.read_small_file_in_folder(&folder.id, &file.id).await? != current_bytes
        {
            bail!("iCloud rename base is not the expected newer fixture");
        }
        let current_etag = entry.etag.clone();
        let stale_accepted = self.send_rename(&file.id, &file.etag, NEW_NAME).await?;
        let after_stale = self.list_folder(&folder.id).await?;
        let listed = exactly_one(
            after_stale
                .iter()
                .filter(|entry| entry.drivewsid == file.id)
                .collect(),
            "stale rename observation",
        )?;
        if self.read_small_file_in_folder(&folder.id, &file.id).await? != current_bytes {
            return Ok(RenameProbeOutcome::Indeterminate);
        }
        if stale_accepted {
            return Ok(
                if listed.display_name() == NEW_NAME && listed.docwsid == file.document_id {
                    RenameProbeOutcome::StaleAccepted
                } else {
                    RenameProbeOutcome::Indeterminate
                },
            );
        }
        if listed.display_name() != PROBE_FILE
            || listed.docwsid != file.document_id
            || listed.etag != current_etag
        {
            return Ok(RenameProbeOutcome::Indeterminate);
        }
        let fresh_accepted = self.send_rename(&file.id, &current_etag, NEW_NAME).await?;
        let after_fresh = self.list_folder(&folder.id).await?;
        let listed = exactly_one(
            after_fresh
                .iter()
                .filter(|entry| entry.drivewsid == file.id)
                .collect(),
            "fresh rename observation",
        )?;
        if self.read_small_file_in_folder(&folder.id, &file.id).await? != current_bytes {
            return Ok(RenameProbeOutcome::Indeterminate);
        }
        if fresh_accepted && listed.display_name() == NEW_NAME && listed.docwsid == file.document_id
        {
            Ok(RenameProbeOutcome::StaleRejectedFreshAccepted)
        } else if !fresh_accepted
            && listed.display_name() == PROBE_FILE
            && listed.etag == current_etag
        {
            Ok(RenameProbeOutcome::StaleRejectedFreshRejected)
        } else {
            Ok(RenameProbeOutcome::Indeterminate)
        }
    }

    /// A metadata-only stale ETag test: rename once, then reuse the ETag from
    /// before that rename. This distinguishes metadata from content revisions.
    pub async fn probe_metadata_rename_revision(
        &mut self,
        folder: &ValidationFolder,
        file: &ValidationFile,
        content: &[u8],
    ) -> Result<RenameProbeOutcome> {
        const FIRST: &str = "renamed-once.txt";
        const SECOND: &str = "renamed-twice.txt";
        let before = self.list_folder(&folder.id).await?;
        let entry = exactly_one(
            before
                .iter()
                .filter(|entry| entry.drivewsid == file.id)
                .collect(),
            "metadata rename base lookup",
        )?;
        if entry.docwsid != file.document_id
            || entry.display_name() != PROBE_FILE
            || entry.etag != file.etag
            || self.read_small_file_in_folder(&folder.id, &file.id).await? != content
        {
            bail!("iCloud metadata rename base changed");
        }
        if !self.send_rename(&file.id, &file.etag, FIRST).await? {
            bail!("first metadata rename was rejected");
        }
        let first = self.list_folder(&folder.id).await?;
        let entry = exactly_one(
            first
                .iter()
                .filter(|entry| entry.drivewsid == file.id)
                .collect(),
            "first metadata rename observation",
        )?;
        if entry.display_name() != FIRST
            || entry.docwsid != file.document_id
            || self.read_small_file_in_folder(&folder.id, &file.id).await? != content
        {
            return Ok(RenameProbeOutcome::Indeterminate);
        }
        if entry.etag.is_empty() || entry.etag == file.etag {
            bail!("first metadata rename did not expose a new ETag");
        }
        let current_etag = entry.etag.clone();
        let stale_accepted = self.send_rename(&file.id, &file.etag, SECOND).await?;
        let after_stale = self.list_folder(&folder.id).await?;
        let entry = exactly_one(
            after_stale
                .iter()
                .filter(|entry| entry.drivewsid == file.id)
                .collect(),
            "stale metadata rename observation",
        )?;
        if self.read_small_file_in_folder(&folder.id, &file.id).await? != content {
            return Ok(RenameProbeOutcome::Indeterminate);
        }
        if stale_accepted {
            return Ok(
                if entry.display_name() == SECOND && entry.docwsid == file.document_id {
                    RenameProbeOutcome::StaleAccepted
                } else {
                    RenameProbeOutcome::Indeterminate
                },
            );
        }
        if entry.display_name() != FIRST || entry.etag != current_etag {
            return Ok(RenameProbeOutcome::Indeterminate);
        }
        let fresh_accepted = self.send_rename(&file.id, &current_etag, SECOND).await?;
        let after_fresh = self.list_folder(&folder.id).await?;
        let entry = exactly_one(
            after_fresh
                .iter()
                .filter(|entry| entry.drivewsid == file.id)
                .collect(),
            "fresh metadata rename observation",
        )?;
        if self.read_small_file_in_folder(&folder.id, &file.id).await? != content {
            return Ok(RenameProbeOutcome::Indeterminate);
        }
        if fresh_accepted && entry.display_name() == SECOND && entry.docwsid == file.document_id {
            Ok(RenameProbeOutcome::StaleRejectedFreshAccepted)
        } else if !fresh_accepted && entry.display_name() == FIRST && entry.etag == current_etag {
            Ok(RenameProbeOutcome::StaleRejectedFreshRejected)
        } else {
            Ok(RenameProbeOutcome::Indeterminate)
        }
    }

    /// Test the exact occupied-name handoff that a staged replacement would
    /// need. Both items were created in this process and are independently
    /// verified before the single rename request.
    pub async fn probe_occupied_name_rename(
        &mut self,
        folder: &ValidationFolder,
        original: &ValidationFile,
        staged: &ValidationFile,
        original_bytes: &[u8],
        staged_bytes: &[u8],
    ) -> Result<OccupiedNameOutcome> {
        if original.id == staged.id
            || original.name != PROBE_FILE
            || !staged.name.starts_with("staged-by-cirrove-")
            || original_bytes == staged_bytes
        {
            bail!("invalid staged validation pair");
        }
        let before = self.list_folder(&folder.id).await?;
        if before.len() != 2
            || !before.iter().any(|entry| {
                entry.drivewsid == original.id && entry.display_name() == original.name
            })
            || !before
                .iter()
                .any(|entry| entry.drivewsid == staged.id && entry.display_name() == staged.name)
            || self
                .read_small_file_in_folder(&folder.id, &original.id)
                .await?
                != original_bytes
            || self
                .read_small_file_in_folder(&folder.id, &staged.id)
                .await?
                != staged_bytes
        {
            bail!("iCloud staged validation pair changed before the rename");
        }
        let accepted = self
            .send_rename(&staged.id, &staged.etag, PROBE_FILE)
            .await?;
        let after = self.list_folder(&folder.id).await?;
        let old = after.iter().find(|entry| entry.drivewsid == original.id);
        let new = after.iter().find(|entry| entry.drivewsid == staged.id);
        let Some(old) = old else {
            return Ok(OccupiedNameOutcome::OriginalNoLongerListed);
        };
        let Some(new) = new else {
            return Ok(OccupiedNameOutcome::Indeterminate);
        };
        if old.docwsid != original.document_id
            || new.docwsid != staged.document_id
            || self
                .read_small_file_in_folder(&folder.id, &original.id)
                .await?
                != original_bytes
            || self
                .read_small_file_in_folder(&folder.id, &staged.id)
                .await?
                != staged_bytes
        {
            return Ok(OccupiedNameOutcome::Indeterminate);
        }
        if !accepted && old.display_name() == original.name && new.display_name() == staged.name {
            Ok(OccupiedNameOutcome::RejectedBothIntact)
        } else if accepted && old.display_name() == PROBE_FILE && new.display_name() == PROBE_FILE {
            Ok(OccupiedNameOutcome::AcceptedDuplicateName)
        } else if old.display_name() == PROBE_FILE
            && new.display_name().starts_with("created-by-cirrove ")
            && new.display_name().ends_with(".txt")
        {
            Ok(OccupiedNameOutcome::ConflictRenamedBothIntact)
        } else {
            Ok(OccupiedNameOutcome::Indeterminate)
        }
    }

    /// Stage -> recovery -> target, but only for two new fixtures. No remote
    /// item is deleted. This tests protocol observability, not atomicity.
    pub async fn probe_staged_handoff(
        &mut self,
        folder: &ValidationFolder,
        original: &ValidationFile,
        staged: &ValidationFile,
        original_bytes: &[u8],
        staged_bytes: &[u8],
    ) -> Result<HandoffOutcome> {
        if original.id == staged.id
            || original.name != PROBE_FILE
            || !staged.name.starts_with("staged-by-cirrove-")
            || original_bytes == staged_bytes
        {
            bail!("invalid staged handoff pair");
        }
        let before = self.list_folder(&folder.id).await?;
        if before.len() != 2
            || !before.iter().any(|entry| {
                entry.drivewsid == original.id && entry.display_name() == original.name
            })
            || !before
                .iter()
                .any(|entry| entry.drivewsid == staged.id && entry.display_name() == staged.name)
            || self
                .read_small_file_in_folder(&folder.id, &original.id)
                .await?
                != original_bytes
            || self
                .read_small_file_in_folder(&folder.id, &staged.id)
                .await?
                != staged_bytes
        {
            bail!("iCloud staged handoff pair changed before the first rename");
        }
        let recovery_name = format!("recovery-by-cirrove-{}.txt", Uuid::new_v4());
        let first_accepted = self
            .send_rename(&original.id, &original.etag, &recovery_name)
            .await?;
        let after_first = self.list_folder(&folder.id).await?;
        let old = after_first
            .iter()
            .find(|entry| entry.drivewsid == original.id);
        let new = after_first
            .iter()
            .find(|entry| entry.drivewsid == staged.id);
        let (Some(old), Some(new)) = (old, new) else {
            return Ok(HandoffOutcome::Indeterminate);
        };
        if !first_accepted
            || old.display_name() != recovery_name
            || new.display_name() != staged.name
            || old.docwsid != original.document_id
            || new.docwsid != staged.document_id
            || self
                .read_small_file_in_folder(&folder.id, &original.id)
                .await?
                != original_bytes
            || self
                .read_small_file_in_folder(&folder.id, &staged.id)
                .await?
                != staged_bytes
        {
            return Ok(HandoffOutcome::Indeterminate);
        }
        let second_accepted = self
            .send_rename(&staged.id, &staged.etag, PROBE_FILE)
            .await?;
        let after_second = self.list_folder(&folder.id).await?;
        let old = after_second
            .iter()
            .find(|entry| entry.drivewsid == original.id);
        let new = after_second
            .iter()
            .find(|entry| entry.drivewsid == staged.id);
        let (Some(old), Some(new)) = (old, new) else {
            return Ok(HandoffOutcome::Indeterminate);
        };
        if old.display_name() != recovery_name
            || old.docwsid != original.document_id
            || new.docwsid != staged.document_id
            || self
                .read_small_file_in_folder(&folder.id, &original.id)
                .await?
                != original_bytes
            || self
                .read_small_file_in_folder(&folder.id, &staged.id)
                .await?
                != staged_bytes
        {
            return Ok(HandoffOutcome::Indeterminate);
        }
        if second_accepted && new.display_name() == PROBE_FILE {
            Ok(HandoffOutcome::BothPreservedAtTargetAndRecovery)
        } else if !second_accepted && new.display_name() == staged.name {
            Ok(HandoffOutcome::RecoveryMovedStagingUnchanged)
        } else {
            Ok(HandoffOutcome::Indeterminate)
        }
    }

    pub async fn prepare_durable_handoff(
        &mut self,
        folder: &ValidationFolder,
        original: &ValidationFile,
        staged: &ValidationFile,
        original_bytes: &[u8],
        staged_bytes: &[u8],
    ) -> Result<HandoffPlan> {
        if original.name != PROBE_FILE
            || !staged.name.starts_with("staged-by-cirrove-")
            || original.id == staged.id
            || original_bytes.is_empty()
            || staged_bytes.is_empty()
            || original_bytes.len() > 4096
            || staged_bytes.len() > 4096
        {
            bail!("invalid durable iCloud validation pair");
        }
        let plan = HandoffPlan {
            version: 2,
            folder_id: folder.id.clone(),
            folder_name: folder.name.clone(),
            original_id: original.id.clone(),
            original_doc_id: original.document_id.clone(),
            original_etag: original.etag.clone(),
            staged_id: staged.id.clone(),
            staged_doc_id: staged.document_id.clone(),
            staged_etag: staged.etag.clone(),
            staged_name: staged.name.clone(),
            recovery_name: format!("recovery-by-cirrove-{}.txt", Uuid::new_v4()),
            original_sha256: hex::encode(Sha256::digest(original_bytes)),
            staged_sha256: hex::encode(Sha256::digest(staged_bytes)),
        };
        if self.inspect_durable_handoff(&plan).await? != HandoffObserved::Prepared {
            bail!("durable iCloud validation pair changed during preparation");
        }
        Ok(plan)
    }

    /// Read-only reconciliation by account-bound item IDs and full content
    /// hashes. Names describe phase but never establish identity alone.
    pub async fn inspect_durable_handoff(&mut self, plan: &HandoffPlan) -> Result<HandoffObserved> {
        plan.validate()?;
        if !self.list_root().await?.iter().any(|entry| {
            entry.drivewsid == plan.folder_id
                && entry.display_name() == plan.folder_name
                && entry.is_folder()
        }) {
            return Ok(HandoffObserved::Diverged);
        }
        let items = self.list_folder(&plan.folder_id).await?;
        if items.len() != 2 {
            return Ok(HandoffObserved::Diverged);
        }
        let old = items
            .iter()
            .find(|entry| entry.drivewsid == plan.original_id);
        let new = items.iter().find(|entry| entry.drivewsid == plan.staged_id);
        let (Some(old), Some(new)) = (old, new) else {
            return Ok(HandoffObserved::Diverged);
        };
        if old.is_folder()
            || new.is_folder()
            || old.docwsid != plan.original_doc_id
            || new.docwsid != plan.staged_doc_id
            || old.size > 4096
            || new.size > 4096
        {
            return Ok(HandoffObserved::Diverged);
        }
        let old_bytes = self
            .read_small_file_in_folder(&plan.folder_id, &plan.original_id)
            .await?;
        let new_bytes = self
            .read_small_file_in_folder(&plan.folder_id, &plan.staged_id)
            .await?;
        if hex::encode(Sha256::digest(&old_bytes)) != plan.original_sha256
            || hex::encode(Sha256::digest(&new_bytes)) != plan.staged_sha256
        {
            return Ok(HandoffObserved::Diverged);
        }
        let (old_name, new_name) = (old.display_name(), new.display_name());
        Ok(if old_name == PROBE_FILE && new_name == plan.staged_name {
            if plan.revisions_match_prepared(&old.etag, &new.etag) {
                HandoffObserved::Prepared
            } else {
                HandoffObserved::Diverged
            }
        } else if old_name == plan.recovery_name && new_name == plan.staged_name {
            if plan.staged_etag == new.etag {
                HandoffObserved::OldAtRecovery
            } else {
                HandoffObserved::Diverged
            }
        } else if old_name == plan.recovery_name && new_name == PROBE_FILE {
            HandoffObserved::Complete
        } else {
            HandoffObserved::Diverged
        })
    }

    pub async fn move_old_to_recovery(&mut self, plan: &HandoffPlan) -> Result<bool> {
        if self.inspect_durable_handoff(plan).await? != HandoffObserved::Prepared {
            bail!("old iCloud validation item is not in the prepared state");
        }
        // Never refresh the precondition to a newer remote revision. Apple's
        // web rename may ignore a stale ETag, but a known intervening edit must
        // stop this research operation before its first mutating request.
        self.send_rename(&plan.original_id, &plan.original_etag, &plan.recovery_name)
            .await
    }

    pub async fn move_staged_to_target(&mut self, plan: &HandoffPlan) -> Result<bool> {
        if self.inspect_durable_handoff(plan).await? != HandoffObserved::OldAtRecovery {
            bail!("staged iCloud validation item is not ready for handoff");
        }
        self.send_rename(&plan.staged_id, &plan.staged_etag, PROBE_FILE)
            .await
    }
}

fn valid_etag(etag: &str) -> bool {
    !etag.is_empty() && etag.len() <= 4096 && !etag.contains(['\0', '\r', '\n'])
}

impl HandoffPlan {
    fn revisions_match_prepared(&self, old: &str, staged: &str) -> bool {
        old == self.original_etag && staged == self.staged_etag
    }

    fn validate(&self) -> Result<()> {
        let uuid_name = |name: &str, prefix: &str, suffix: &str| {
            name.strip_prefix(prefix)
                .and_then(|middle| middle.strip_suffix(suffix))
                .is_some_and(|middle| Uuid::parse_str(middle).is_ok())
        };
        let digest = |value: &str| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        };
        let old = split_file_id(&self.original_id)?;
        let new = split_file_id(&self.staged_id)?;
        if self.version != 2
            || !uuid_name(&self.folder_name, PROBE_PREFIX, "")
            || !self.folder_id.starts_with("FOLDER::com.apple.CloudDocs::")
            || !uuid_name(&self.staged_name, "staged-by-cirrove-", ".txt")
            || !uuid_name(&self.recovery_name, "recovery-by-cirrove-", ".txt")
            || self.original_id == self.staged_id
            || old.0 != "com.apple.CloudDocs"
            || new.0 != "com.apple.CloudDocs"
            || old.1 != self.original_doc_id
            || new.1 != self.staged_doc_id
            || !digest(&self.original_sha256)
            || !digest(&self.staged_sha256)
            || !valid_etag(&self.original_etag)
            || !valid_etag(&self.staged_etag)
        {
            bail!("invalid durable iCloud handoff plan");
        }
        Ok(())
    }
}

impl StagedRegistrationPlan {
    pub fn validate(&self) -> Result<()> {
        let uuid_name = |name: &str, prefix: &str, suffix: &str| {
            name.strip_prefix(prefix)
                .and_then(|middle| middle.strip_suffix(suffix))
                .is_some_and(|middle| Uuid::parse_str(middle).is_ok())
        };
        let digest = |value: &str| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        };
        let old = split_file_id(&self.original_id)?;
        let bound = match (&self.staged_id, &self.staged_doc_id, &self.staged_etag) {
            (None, None, None) => true,
            (Some(id), Some(document), Some(etag)) => {
                let candidate = split_file_id(id)?;
                candidate.0 == "com.apple.CloudDocs"
                    && candidate.1 == document
                    && id != &self.original_id
                    && valid_etag(etag)
            }
            _ => false,
        };
        if self.version != 2
            || !uuid_name(&self.folder_name, PROBE_PREFIX, "")
            || !self.folder_id.starts_with("FOLDER::com.apple.CloudDocs::")
            || !uuid_name(&self.staged_name, "staged-by-cirrove-", ".txt")
            || old.0 != "com.apple.CloudDocs"
            || old.1 != self.original_doc_id
            || !digest(&self.original_sha256)
            || !digest(&self.staged_sha256)
            || !valid_etag(&self.original_etag)
            || !bound
        {
            bail!("invalid staged iCloud registration plan");
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod handoff_tests {
    use super::*;

    fn plan() -> HandoffPlan {
        HandoffPlan {
            version: 2,
            folder_id: "FOLDER::com.apple.CloudDocs::folder-1".into(),
            folder_name: format!("{PROBE_PREFIX}{}", Uuid::new_v4()),
            original_id: "FILE::com.apple.CloudDocs::old-1".into(),
            original_doc_id: "old-1".into(),
            original_etag: "old-revision".into(),
            staged_id: "FILE::com.apple.CloudDocs::new-1".into(),
            staged_doc_id: "new-1".into(),
            staged_etag: "staged-revision".into(),
            staged_name: format!("staged-by-cirrove-{}.txt", Uuid::new_v4()),
            recovery_name: format!("recovery-by-cirrove-{}.txt", Uuid::new_v4()),
            original_sha256: "a".repeat(64),
            staged_sha256: "b".repeat(64),
        }
    }

    #[test]
    fn persisted_handoff_cannot_retarget_another_item_or_unowned_name() {
        let value = plan();
        value.validate().unwrap();
        let bytes = serde_json::to_vec(&value).unwrap();
        let restored: HandoffPlan = serde_json::from_slice(&bytes).unwrap();
        restored.validate().unwrap();

        let mut foreign = plan();
        foreign.original_id = "FILE::other.zone::old-1".into();
        assert!(foreign.validate().is_err());
        let mut swapped = plan();
        swapped.staged_doc_id = "unrelated".into();
        assert!(swapped.validate().is_err());
        let mut unsafe_name = plan();
        unsafe_name.recovery_name = "important.txt".into();
        assert!(unsafe_name.validate().is_err());
        let mut legacy = plan();
        legacy.version = 1;
        assert!(legacy.validate().is_err());
        let mut invalid_revision = plan();
        invalid_revision.original_etag = "new\nline".into();
        assert!(invalid_revision.validate().is_err());
    }

    #[test]
    fn durable_handoff_refuses_changed_revisions_even_when_bytes_match() {
        let plan = plan();
        assert!(plan.revisions_match_prepared("old-revision", "staged-revision"));
        assert!(!plan.revisions_match_prepared("foreign-revision", "staged-revision"));
        assert!(!plan.revisions_match_prepared("old-revision", "foreign-revision"));
    }

    #[test]
    fn staged_registration_reservation_requires_a_matching_bound_identity() {
        let mut plan = StagedRegistrationPlan {
            version: 2,
            folder_id: "FOLDER::com.apple.CloudDocs::folder-1".into(),
            folder_name: format!("{PROBE_PREFIX}{}", Uuid::new_v4()),
            original_id: "FILE::com.apple.CloudDocs::old-1".into(),
            original_doc_id: "old-1".into(),
            original_etag: "old-revision".into(),
            original_sha256: "a".repeat(64),
            staged_name: format!("staged-by-cirrove-{}.txt", Uuid::new_v4()),
            staged_sha256: "b".repeat(64),
            staged_id: None,
            staged_doc_id: None,
            staged_etag: None,
        };
        plan.validate().unwrap();
        plan.staged_id = Some("FILE::com.apple.CloudDocs::new-1".into());
        assert!(plan.validate().is_err());
        plan.staged_doc_id = Some("new-1".into());
        assert!(plan.validate().is_err());
        plan.staged_etag = Some("staged-revision".into());
        plan.validate().unwrap();
        plan.staged_doc_id = Some("unrelated".into());
        assert!(plan.validate().is_err());
        plan.staged_id = Some("FILE::other.zone::unrelated".into());
        assert!(plan.validate().is_err());
    }
}
