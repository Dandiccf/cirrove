//! iCloud content-slot transport. A slot upload alone does not create a visible
//! Drive item; registration and reconciliation belong to an upload provider.
use crate::{
    ICLOUD_ORIGIN, ICloudReadSession, checked_content_url, drive_request_failure, read_json,
};
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs::File;
use tokio_util::io::ReaderStream;

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct UploadSlot {
    pub(crate) url: String,
    pub(crate) document_id: String,
}

#[derive(Deserialize)]
pub(crate) struct Uploaded {
    #[serde(rename = "singleFile")]
    pub(crate) file: UploadedFile,
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct UploadedFile {
    pub(crate) receipt: String,
    #[serde(rename = "fileChecksum")]
    pub(crate) signature: String,
    #[serde(rename = "referenceChecksum")]
    pub(crate) reference_signature: String,
    #[serde(rename = "wrappingKey")]
    pub(crate) wrapping_key: String,
    pub(crate) size: u64,
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

impl UploadedFile {
    pub(crate) fn valid_for(&self, size: u64) -> bool {
        self.size == size && !self.receipt.is_empty() && !self.signature.is_empty()
    }
}

fn content_type_for_name(name: &str) -> &'static str {
    if name.ends_with(".txt") {
        "text/plain"
    } else {
        "application/octet-stream"
    }
}

impl ICloudReadSession {
    /// Register previously uploaded bytes under one exact parent. The caller
    /// must persist the slot and receipt first; an uncertain response must be
    /// reconciled by document ID rather than replayed.
    pub(crate) async fn register_uploaded_file(
        &mut self,
        parent: &str,
        name: &str,
        slot: &UploadSlot,
        data: &UploadedFile,
        size: u64,
    ) -> Result<()> {
        if !parent.starts_with("FOLDER::com.apple.CloudDocs::")
            || parent.rsplit("::").next().is_none_or(str::is_empty)
            || name.is_empty()
            || name.len() > 255
            || matches!(name, "." | "..")
            || name.contains(['/', '\0', '\r', '\n'])
            || slot.document_id.is_empty()
            || !data.valid_for(size)
        {
            bail!("invalid iCloud file registration");
        }
        let starting_document_id = parent
            .rsplit("::")
            .next()
            .context("invalid iCloud parent")?;
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
            .map_err(|_| anyhow!("iCloud file registration outcome is uncertain"))?;
        if !response.status().is_success() {
            return Err(drive_request_failure(
                response.status(),
                "iCloud file registration",
            ));
        }
        let reply: UpdateReply = read_json(response, "iCloud file registration").await?;
        if reply.status.status_code != 0 || reply.results.len() != 1 {
            bail!("iCloud file registration receipt is incomplete");
        }
        let result = &reply.results[0];
        let document = result
            .document
            .as_ref()
            .context("iCloud file registration lacks identity")?;
        if result.status.status_code != 0
            || document.deleted
            || document.name != name
            || document.document_id != slot.document_id
        {
            bail!("iCloud file registration returned a different identity");
        }
        Ok(())
    }

    pub(crate) async fn allocate_upload_slot(
        &mut self,
        name: &str,
        size: u64,
    ) -> Result<UploadSlot> {
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
                "size": size.to_string(),
                "content_type": content_type_for_name(name)
            }))
            .send()
            .await
            .map_err(|_| anyhow!("iCloud upload allocation failed"))?;
        if !response.status().is_success() {
            return Err(drive_request_failure(
                response.status(),
                "iCloud upload allocation",
            ));
        }
        let mut slots: Vec<UploadSlot> = read_json(response, "iCloud upload allocation").await?;
        if slots.len() != 1 {
            bail!("iCloud upload allocation returned an unexpected result count");
        }
        let slot = slots.remove(0);
        if slot.document_id.is_empty() || slot.document_id.len() > 256 {
            bail!("iCloud upload allocation lacks a document identity");
        }
        checked_content_url(&slot.url)?;
        Ok(slot)
    }

    pub(crate) async fn upload_stream_to_slot(
        &mut self,
        slot: &UploadSlot,
        name: &str,
        file: File,
        size: u64,
    ) -> Result<UploadedFile> {
        let upload_url = checked_content_url(&slot.url)?;
        let body = reqwest::Body::wrap_stream(ReaderStream::with_capacity(
            tokio::fs::File::from_std(file),
            64 * 1024,
        ));
        let response = self
            .http
            .post(upload_url)
            .header("content-type", content_type_for_name(name))
            .header(reqwest::header::CONTENT_LENGTH, size)
            .body(body)
            .send()
            .await
            .map_err(|_| anyhow!("iCloud streamed content upload failed"))?;
        if !response.status().is_success() {
            return Err(drive_request_failure(
                response.status(),
                "iCloud streamed content upload",
            ));
        }
        let uploaded: Uploaded = read_json(response, "iCloud streamed content upload").await?;
        let data = uploaded.file;
        if !data.valid_for(size) {
            bail!("iCloud streamed upload receipt is incomplete");
        }
        Ok(data)
    }
}
