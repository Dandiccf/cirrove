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
    ) -> Result<String> {
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
        let starting_document_id = folder_id
            .rsplit("::")
            .next()
            .context("invalid folder identity")?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis() as u64;
        let update_url = endpoint.join("ws/com.apple.CloudDocs/update/documents")?;
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
        Ok(entry.drivewsid)
    }
}
