//! Public-web-client PACKAGE wire shape; not yet live-validated.
use super::*;
use crate::{
    ICLOUD_ORIGIN, checked_content_url, drive_request_failure, parse_package_upload_receipt,
};
use serde_json::{Value, json};
use tokio_util::io::ReaderStream;
const MAX_REPLY: usize = 64 * 1024;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Slot {
    pub url: String,
    pub document_id: String,
    pub owner_id: String,
    #[serde(default)]
    owner: Option<String>,
}
/// Fixed diagnostic categories only: no slot values, URLs, IDs or provider text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageAllocationRefusal {
    Schema,
    ResultCount,
    DocumentIdentity,
    OwnerIdentifierTooLong,
    OwnerTooLong,
    UrlTooLong,
    ContentLocation,
}
impl std::fmt::Display for PackageAllocationRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Schema => "invalid package allocation response shape",
            Self::ResultCount => "package allocation count mismatch",
            Self::DocumentIdentity => "invalid package allocation document identity",
            Self::OwnerIdentifierTooLong => "package allocation owner identifier exceeds bound",
            Self::OwnerTooLong => "package allocation owner exceeds bound",
            Self::UrlTooLong => "package allocation location exceeds bound",
            Self::ContentLocation => "invalid package allocation content location",
        })
    }
}
impl std::error::Error for PackageAllocationRefusal {}
impl Slot {
    pub fn validate(&self) -> Result<()> {
        // Local ID policy is intentionally unchanged until separately evidenced.
        if self.document_id.is_empty()
            || self.document_id.len() > 256
            || self.document_id.contains([':', '/', '\\', '\0'])
        {
            return Err(PackageAllocationRefusal::DocumentIdentity.into());
        }
        if self.owner_id.len() > 256 {
            return Err(PackageAllocationRefusal::OwnerIdentifierTooLong.into());
        }
        if self.owner.as_ref().is_some_and(|value| value.len() > 256) {
            return Err(PackageAllocationRefusal::OwnerTooLong.into());
        }
        if self.url.len() > 8192 {
            return Err(PackageAllocationRefusal::UrlTooLong.into());
        }
        // Optional owner is opaque metadata, not evidence of a shared target.
        // The exact owned CloudDocs parent determines authority. Neither owner
        // field is forwarded into the ordinary owned registration request.
        checked_content_url(&self.url).map_err(|_| PackageAllocationRefusal::ContentLocation)?;
        Ok(())
    }
}
async fn bounded(mut response: reqwest::Response) -> Result<Vec<u8>> {
    if !response.status().is_success() {
        return Err(drive_request_failure(response.status(), "package transfer"));
    }
    ensure!(
        response
            .content_length()
            .is_none_or(|n| n <= MAX_REPLY as u64),
        "package response exceeds bound"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow::anyhow!("package response interrupted"))?
    {
        ensure!(
            chunk.len() <= MAX_REPLY.saturating_sub(bytes.len()),
            "package response exceeds bound"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
async fn post(session: &ICloudReadSession, suffix: &str, body: Value) -> Result<Vec<u8>> {
    let url = session
        .docs_endpoint
        .as_ref()
        .context("package session incomplete")?
        .join(suffix)?;
    let response = session
        .http
        .post(url)
        .timeout(crate::UPLOAD_TRANSFER_TIMEOUT)
        .header("origin", ICLOUD_ORIGIN)
        .header("referer", format!("{ICLOUD_ORIGIN}/"))
        .header("content-type", "text/plain")
        .body(serde_json::to_vec(&body)?)
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("package operation outcome uncertain"))?;
    bounded(response).await
}
pub(super) async fn allocate(
    session: &mut ICloudReadSession,
    plan: &OwnedPackagePlan,
) -> Result<Slot> {
    // The fixed validator destination is ASCII; encodeURI changes its spaces.
    let filename = plan.destination.replace(' ', "%20");
    let bytes = post(session, "ws/com.apple.CloudDocs/upload/web", json!({"filename": filename, "type":"PACKAGE", "size":plan.archive_size, "content_type":"application/zip"})).await?;
    let mut slots: Vec<Slot> =
        serde_json::from_slice(&bytes).map_err(|_| PackageAllocationRefusal::Schema)?;
    if slots.len() != 1 {
        return Err(PackageAllocationRefusal::ResultCount.into());
    }
    let slot = slots.remove(0);
    slot.validate()?;
    Ok(slot)
}
pub(super) async fn upload(
    session: &mut ICloudReadSession,
    slot: &Slot,
    file: File,
    size: u64,
) -> Result<SecretString> {
    slot.validate()?;
    let url = checked_content_url(&slot.url)?;
    let body = reqwest::Body::wrap_stream(ReaderStream::with_capacity(
        tokio::fs::File::from_std(file),
        64 * 1024,
    ));
    let response = session
        .http
        .post(url)
        .timeout(crate::UPLOAD_TRANSFER_TIMEOUT)
        .header("content-type", "application/zip")
        .header(reqwest::header::CONTENT_LENGTH, size)
        .body(body)
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("package body outcome uncertain"))?;
    let bytes = bounded(response).await?;
    Ok(parse_package_upload_receipt(&bytes)?.registration_data()?)
}
fn registration(saved: &Checkpoint) -> Result<Value> {
    ensure!(
        saved.phase == Phase::RegistrationStarted,
        "package registration not durably armed"
    );
    saved.plan.validate()?;
    let slot = saved.slot.as_ref().context("package slot absent")?;
    slot.validate()?;
    ensure!(
        slot.document_id != saved.plan.source.docwsid,
        "package allocation reused source"
    );
    let fragment = saved
        .registration
        .as_ref()
        .context("package receipt absent")?;
    ensure!(fragment.len() <= MAX_REPLY, "package receipt exceeds bound");
    let data: Value = serde_json::from_str(fragment)
        .map_err(|_| anyhow::anyhow!("invalid saved package receipt"))?;
    let object = data.as_object().context("invalid saved package receipt")?;
    ensure!(
        object.len() == 3
            && object.contains_key("pkgSignature")
            && object.contains_key("manifest")
            && object.contains_key("package"),
        "package receipt authority mismatch"
    );
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64;
    Ok(
        json!({"command":"add_package", "document_id":slot.document_id,
        "pkgSignature":data["pkgSignature"], "manifest":data["manifest"], "package":data["package"],
        "path":{"starting_document_id":saved.plan.parent.rsplit("::").next(), "path":saved.plan.destination},
        "allow_conflict":false, "file_flags":{"is_executable":false,"is_hidden":false,"is_writable":true}, "mtime":now,"btime":now}),
    )
}
#[derive(Deserialize)]
struct Reply {
    status: Status,
    results: Vec<Entry>,
}
#[derive(Deserialize)]
struct Status {
    status_code: i64,
}
#[derive(Deserialize)]
struct Entry {
    status: Status,
    document: Option<Document>,
}
#[derive(Deserialize)]
struct Document {
    document_id: String,
    item_id: String,
    etag: String,
    size: u64,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    deleted: Option<bool>,
}
pub(super) async fn register(session: &mut ICloudReadSession, saved: &Checkpoint) -> Result<()> {
    let body = registration(saved)?;
    let bytes = post(
        session,
        "ws/com.apple.CloudDocs/update/documents?errorBreakdown=true",
        body,
    )
    .await?;
    let reply: Reply = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("invalid package registration response"))?;
    ensure!(
        reply.status.status_code == 0 && reply.results.len() == 1,
        "package registration status mismatch"
    );
    let result = &reply.results[0];
    let item = result
        .document
        .as_ref()
        .context("package registration identity absent")?;
    let slot = saved.slot.as_ref().context("package slot absent")?;
    ensure!(
        result.status.status_code == 0
            && item.document_id == slot.document_id
            && !item.item_id.is_empty()
            && item.item_id.len() <= 512
            && !item.etag.is_empty()
            && item.etag.len() <= 1024
            && item.size <= MAX_BODY
            && item.deleted != Some(true)
            && item
                .name
                .as_ref()
                .is_none_or(|name| name == &saved.plan.destination),
        "package registration identity mismatch"
    );
    Ok(())
}
