//! Deliberately narrow live-write experiment. Only a new, probe-named folder
//! and a new file inside that folder can be created. This is not WriteProvider.
use super::*;

const PROBE_PREFIX: &str = "Cirrove Write Validation-";
const PROBE_FILE: &str = "created-by-cirrove.txt";

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

#[derive(Deserialize)]
struct RenameReply {
    items: Vec<RenameResult>,
}

#[derive(Deserialize)]
struct RenameResult {
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
    async fn upload_probe_bytes(&mut self, bytes: &[u8]) -> Result<(UploadSlot, UploadedFile)> {
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
                "filename": PROBE_FILE,
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
        if !self.list_folder(folder_id).await?.is_empty() {
            bail!("iCloud validation folder must be empty");
        }
        let (slot, data) = self.upload_probe_bytes(bytes).await?;
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
                "path": {"path": PROBE_FILE, "starting_document_id": starting_document_id}
            }))
            .send()
            .await
            .map_err(|_| anyhow!("iCloud validation file registration failed"))?;
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
        if document.deleted
            || document.name != PROBE_FILE
            || document.document_id != slot.document_id
        {
            bail!("iCloud returned a different validation file");
        }
        let matches: Vec<_> = self
            .list_folder(folder_id)
            .await?
            .into_iter()
            .filter(|entry| entry.display_name() == PROBE_FILE)
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
        Ok(ValidationFile {
            id: entry.drivewsid,
            document_id: entry.docwsid,
            etag: entry.etag,
        })
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
            .send_same_id_update(&folder.id, &file.document_id, &file.etag, new_bytes)
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
    ) -> Result<bool> {
        let (_slot, data) = self.upload_probe_bytes(bytes).await?;
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
        let response = self
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
            }))
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
            .send_same_id_update(&folder.id, &file.document_id, &file.etag, candidate_bytes)
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
}
