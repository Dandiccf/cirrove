//! Conditional iCloud Drive requests shared by the isolated validator and a
//! future account writer. No normal account constructs a write provider yet.
use crate::{ICLOUD_ORIGIN, ICloudReadSession, LISTING_TIMEOUT, drive_request_failure, read_json};
use anyhow::{Context, Result, anyhow, bail};
use reqwest::{Response, StatusCode};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

pub(crate) const TRASH_ROOT: &str = "FOLDER::com.apple.CloudDocs::TRASH_ROOT";

/// Probe-only sanitized classification. Generic conflict/rejection does not
/// establish that a stale revision was the reason for refusal. Numeric HTTP
/// status is retained for diagnostics; provider body text is never included.
#[cfg(feature = "write-probe")]
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ProbeTrashResult {
    Accepted,
    PreconditionFailed,
    // Diagnostic only: the strict probe acceptance remains HTTP 412.
    EtagConflict { http_status: u16 },
    Conflict { http_status: u16 },
    Rejected { http_status: u16 },
}

#[cfg(feature = "write-probe")]
#[derive(Deserialize)]
struct ProbeTrashReply {
    items: Vec<ProbeTrashItem>,
}

#[cfg(feature = "write-probe")]
#[derive(Deserialize)]
struct ProbeTrashItem {
    status: String,
    drivewsid: Option<String>,
    #[serde(rename = "parentId")]
    parent_id: Option<String>,
    etag: Option<String>,
}

#[cfg(feature = "write-probe")]
pub(crate) struct ProbeRestoreReceipt {
    pub http_status: u16,
    pub etag: String,
}

#[cfg(feature = "write-probe")]
#[derive(Deserialize)]
struct ProbeRestoreReply {
    items: Vec<ProbeRestoreItem>,
}
#[cfg(feature = "write-probe")]
#[derive(Deserialize)]
struct ProbeRestoreItem {
    status: String,
    drivewsid: Option<String>,
    docwsid: Option<String>,
    #[serde(rename = "parentId")]
    parent_id: Option<String>,
    etag: Option<String>,
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
struct RenameReply {
    items: Vec<TrashResult>,
}

#[derive(Deserialize)]
struct FolderReply {
    folders: Vec<FolderCreated>,
}

#[derive(Deserialize)]
struct FolderCreated {
    drivewsid: String,
    name: String,
    #[serde(default, deserialize_with = "crate::null_to_default")]
    extension: String,
    status: String,
}

impl FolderCreated {
    fn confirmed_identity(&self, name: &str) -> Result<String> {
        let display_name = if self.extension.is_empty() {
            self.name.clone()
        } else {
            format!("{}.{}", self.name, self.extension)
        };
        if self.status != "OK"
            || display_name != name
            || !self.drivewsid.starts_with("FOLDER::com.apple.CloudDocs::")
            || self.drivewsid.rsplit("::").next().is_none_or(str::is_empty)
        {
            bail!("iCloud folder creation lacks a confirmed identity");
        }
        Ok(self.drivewsid.clone())
    }
}

fn classify_trash(status: StatusCode, result: Option<&str>) -> Result<bool> {
    if status == StatusCode::INSUFFICIENT_STORAGE {
        return Err(crate::StorageRefused.into());
    }
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        return Err(crate::SessionRejected.into());
    }
    if status.is_success() {
        return match result {
            Some("OK") => Ok(true),
            Some(_) => Ok(false),
            None => Err(anyhow!("iCloud conditional Trash receipt is incomplete")),
        };
    }
    if matches!(status.as_u16(), 400 | 404 | 409 | 412) {
        return Ok(false);
    }
    Err(anyhow!("iCloud conditional Trash result is uncertain"))
}

fn classify_rename(status: StatusCode, result: Option<&str>) -> Result<bool> {
    if status == StatusCode::INSUFFICIENT_STORAGE {
        return Err(crate::StorageRefused.into());
    }
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        return Err(crate::SessionRejected.into());
    }
    if status.is_success() {
        return match result {
            Some("OK") => Ok(true),
            Some(_) => Ok(false),
            None => Err(anyhow!("iCloud conditional rename receipt is incomplete")),
        };
    }
    if matches!(status.as_u16(), 400 | 404 | 409 | 412) {
        return Ok(false);
    }
    Err(anyhow!("iCloud conditional rename result is uncertain"))
}

fn classify_move(status: StatusCode, result: Option<&str>) -> Result<bool> {
    if status == StatusCode::INSUFFICIENT_STORAGE {
        return Err(crate::StorageRefused.into());
    }
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        return Err(crate::SessionRejected.into());
    }
    if status.is_success() {
        return match result {
            Some("OK") => Ok(true),
            Some(_) => Ok(false),
            None => Err(anyhow!("iCloud conditional move receipt is incomplete")),
        };
    }
    if matches!(status.as_u16(), 400 | 404 | 409 | 412) {
        return Ok(false);
    }
    Err(anyhow!("iCloud conditional move result is uncertain"))
}

impl ICloudReadSession {
    /// Return only Apple's allocated identity. Losing this response leaves
    /// creation indeterminate; a matching name is not an identity receipt.
    pub(crate) async fn create_folder_request(
        &mut self,
        parent: &str,
        name: &str,
    ) -> Result<String> {
        if !parent.starts_with("FOLDER::com.apple.CloudDocs::")
            || parent.rsplit("::").next().is_none_or(str::is_empty)
            || name.is_empty()
            || name.len() > 255
            || matches!(name, "." | "..")
            || name.contains(['/', '\0', '\r', '\n'])
        {
            bail!("invalid iCloud folder creation request");
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
                "destinationDrivewsId": parent,
                "folders": [{
                    "clientId": format!("FOLDER::UNKNOWN_ZONE::TempId-{}", Uuid::new_v4()),
                    "name": name
                }]
            }))
            .send()
            .await
            .map_err(|_| anyhow!("iCloud folder creation outcome is uncertain"))?;
        if !response.status().is_success() {
            return Err(drive_request_failure(
                response.status(),
                "iCloud folder creation",
            ));
        }
        let reply: FolderReply = read_json(response, "iCloud folder creation").await?;
        if reply.folders.len() != 1 {
            bail!("iCloud folder creation returned an unexpected result count");
        }
        let created = &reply.folders[0];
        created.confirmed_identity(name)
    }

    pub(crate) async fn send_move(
        &mut self,
        item_id: &str,
        etag: &str,
        destination: &str,
    ) -> Result<bool> {
        if !(item_id.starts_with("FILE::com.apple.CloudDocs::")
            || item_id.starts_with("FOLDER::com.apple.CloudDocs::"))
            || item_id.rsplit("::").next().is_none_or(str::is_empty)
            || !destination.starts_with("FOLDER::com.apple.CloudDocs::")
            || destination.rsplit("::").next().is_none_or(str::is_empty)
            || etag.is_empty()
            || etag.len() > 4096
            || etag.contains(['\r', '\n', '*'])
        {
            bail!("invalid iCloud conditional move identity");
        }
        let endpoint = self
            .drive_endpoint
            .as_ref()
            .context("iCloud sign-in is not complete")?
            .join("moveItems")?;
        let response = self
            .http
            .post(endpoint)
            .header("origin", ICLOUD_ORIGIN)
            .header("referer", format!("{ICLOUD_ORIGIN}/"))
            .json(&json!({
                "destinationDrivewsId": destination,
                "items": [{"drivewsid": item_id, "etag": etag, "clientId": item_id}]
            }))
            .send()
            .await
            .map_err(|_| anyhow!("iCloud conditional move outcome is uncertain"))?;
        let status = response.status();
        if !status.is_success() {
            return classify_move(status, None);
        }
        let reply: RenameReply = read_json(response, "iCloud conditional move").await?;
        if reply.items.len() != 1 {
            return Err(anyhow!("iCloud conditional move receipt is incomplete"));
        }
        classify_move(status, Some(&reply.items[0].status))
    }

    pub(crate) async fn send_rename(
        &mut self,
        item_id: &str,
        etag: &str,
        name: &str,
    ) -> Result<bool> {
        if !(item_id.starts_with("FILE::com.apple.CloudDocs::")
            || item_id.starts_with("FOLDER::com.apple.CloudDocs::"))
            || item_id.rsplit("::").next().is_none_or(str::is_empty)
            || etag.is_empty()
            || etag.len() > 4096
            || etag.contains(['\r', '\n', '*'])
            || name.is_empty()
            || name.len() > 255
            || matches!(name, "." | "..")
            || name.contains(['/', '\0', '\r', '\n'])
        {
            bail!("invalid iCloud conditional rename identity");
        }
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
            .map_err(|_| anyhow!("iCloud conditional rename outcome is uncertain"))?;
        let status = response.status();
        if !status.is_success() {
            return classify_rename(status, None);
        }
        let reply: RenameReply = read_json(response, "iCloud conditional rename").await?;
        if reply.items.len() != 1 {
            return Err(anyhow!("iCloud conditional rename receipt is incomplete"));
        }
        classify_rename(status, Some(&reply.items[0].status))
    }

    /// Bounded read-only inventory of Apple's special Trash root. A missing
    /// item is evidence only when the returned item count proves completeness.
    pub(crate) async fn read_trash_items(&mut self) -> Result<(Vec<serde_json::Value>, bool)> {
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
            .map_err(|_| anyhow!("iCloud Trash listing request failed"))?;
        if !response.status().is_success() {
            return Err(drive_request_failure(
                response.status(),
                "iCloud Trash listing",
            ));
        }
        let folders: serde_json::Value = read_json(response, "iCloud Trash listing").await?;
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

    async fn trash_response(&mut self, item_id: &str, etag: &str) -> Result<Response> {
        if !(item_id.starts_with("FILE::com.apple.CloudDocs::")
            || item_id.starts_with("FOLDER::com.apple.CloudDocs::"))
            || etag.is_empty()
            || etag.len() > 4096
            || etag.contains(['\r', '\n', '*'])
        {
            bail!("invalid iCloud conditional Trash identity");
        }
        let endpoint = self
            .drive_endpoint
            .as_ref()
            .context("iCloud sign-in is not complete")?
            .join("moveItemsToTrash")?;
        self.http
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
            .map_err(|_| anyhow!("iCloud conditional Trash request outcome is uncertain"))
    }

    #[cfg(feature = "write-probe")]
    pub(crate) async fn send_trash_without_receipt(
        &mut self,
        item_id: &str,
        etag: &str,
    ) -> Result<()> {
        // A deliberate fault: the request may have committed, but no response
        // status or item body is consumed by this process.
        drop(self.trash_response(item_id, etag).await?);
        Ok(())
    }

    #[cfg(feature = "write-probe")]
    pub(crate) async fn send_probe_trash(
        &mut self,
        item_id: &str,
        etag: &str,
        expected_parent: &str,
        expected_current_etag: &str,
    ) -> Result<ProbeTrashResult> {
        let response = self.trash_response(item_id, etag).await?;
        let status = response.status();
        if !status.is_success() {
            // Preserve production authentication/storage/uncertainty mapping.
            classify_trash(status, None)?;
            return Ok(match status {
                StatusCode::PRECONDITION_FAILED => ProbeTrashResult::PreconditionFailed,
                StatusCode::CONFLICT => ProbeTrashResult::Conflict {
                    http_status: status.as_u16(),
                },
                _ => ProbeTrashResult::Rejected {
                    http_status: status.as_u16(),
                },
            });
        }
        let reply: ProbeTrashReply = read_json(response, "iCloud conditional Trash").await?;
        if reply.items.len() != 1 {
            bail!("iCloud conditional Trash receipt is incomplete");
        }
        Ok(if classify_trash(status, Some(&reply.items[0].status))? {
            ProbeTrashResult::Accepted
        } else if reply.items[0].status == "ETAG_CONFLICT"
            && reply.items[0].drivewsid.as_deref() == Some(item_id)
            && reply.items[0].parent_id.as_deref() == Some(expected_parent)
            && !expected_parent.is_empty()
            && reply.items[0].etag.as_deref() == Some(expected_current_etag)
            && !expected_current_etag.is_empty()
            && expected_current_etag.len() <= 4096
            && !expected_current_etag.contains(['\r', '\n', '*'])
            && expected_current_etag != etag
        {
            // Public Drive module 2262 names this status and binds its target
            // and parent before retrying. We additionally bind the independently
            // observed current revision, and never adopt or replay its receipt.
            ProbeTrashResult::EtagConflict {
                http_status: status.as_u16(),
            }
        } else if reply.items[0].status == "CONFLICT" {
            ProbeTrashResult::Conflict {
                http_status: status.as_u16(),
            }
        } else {
            ProbeTrashResult::Rejected {
                http_status: status.as_u16(),
            }
        })
    }

    /// One request only. No generic Drive retry helper and no revision adoption.
    #[cfg(feature = "write-probe")]
    pub(crate) async fn send_probe_restore(
        &mut self,
        item_id: &str,
        document_id: &str,
        trash_etag: &str,
        parent: &str,
    ) -> Result<ProbeRestoreReceipt> {
        if document_id.is_empty()
            || item_id != format!("FILE::com.apple.CloudDocs::{document_id}")
            || trash_etag.is_empty()
            || trash_etag.len() > 4096
            || trash_etag.contains(['\0', '\r', '\n', '*'])
            || !parent.starts_with("FOLDER::com.apple.CloudDocs::")
        {
            bail!("invalid owned package restore identity");
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
            .json(&json!({"items": [{"drivewsid": item_id, "etag": trash_etag}]}))
            .send()
            .await
            .map_err(|_| anyhow!("owned package restore outcome is uncertain"))?;
        let status = response.status();
        if !status.is_success() {
            if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
                return Err(crate::SessionRejected.into());
            }
            if status == StatusCode::INSUFFICIENT_STORAGE {
                return Err(crate::StorageRefused.into());
            }
            bail!(
                "owned package restore did not return success (HTTP {})",
                status.as_u16()
            );
        }
        let reply: ProbeRestoreReply = read_json(response, "owned package restore").await?;
        let [item] = reply.items.as_slice() else {
            bail!("owned package restore receipt is incomplete");
        };
        if item.status != "OK"
            || item.drivewsid.as_deref() != Some(item_id)
            || item.docwsid.as_deref() != Some(document_id)
            || item.parent_id.as_deref() != Some(parent)
        {
            bail!("owned package restore receipt is not bound to the expected item");
        }
        let etag = item
            .etag
            .as_deref()
            .filter(|etag| {
                !etag.is_empty() && etag.len() <= 4096 && !etag.contains(['\0', '\r', '\n', '*'])
            })
            .context("owned package restore receipt has no valid revision")?;
        Ok(ProbeRestoreReceipt {
            http_status: status.as_u16(),
            etag: etag.into(),
        })
    }

    pub(crate) async fn send_trash(&mut self, item_id: &str, etag: &str) -> Result<bool> {
        let response = self.trash_response(item_id, etag).await?;
        let status = response.status();
        if !status.is_success() {
            return classify_trash(status, None);
        }
        let reply: TrashReply = read_json(response, "iCloud conditional Trash").await?;
        if reply.items.len() != 1 {
            return Err(anyhow!("iCloud conditional Trash receipt is incomplete"));
        }
        classify_trash(status, Some(&reply.items[0].status))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn folder_creation_receipt_reconstructs_split_extensions() {
        for (base, extension, expected) in [
            ("Combined", "txt", "Combined.txt"),
            ("Archive.tar", "gz", "Archive.tar.gz"),
            ("Plain", "", "Plain"),
        ] {
            let receipt: FolderCreated = serde_json::from_value(json!({
                "drivewsid":"FOLDER::com.apple.CloudDocs::fixture",
                "name":base,"extension":extension,"status":"OK"
            }))
            .unwrap();
            assert_eq!(
                receipt.confirmed_identity(expected).unwrap(),
                "FOLDER::com.apple.CloudDocs::fixture"
            );
            assert!(receipt.confirmed_identity("different").is_err());
        }
    }

    #[test]
    fn folder_creation_receipt_keeps_identity_and_status_guards() {
        let base = json!({"drivewsid":"FOLDER::com.apple.CloudDocs::fixture",
            "name":"Plain","status":"OK"});
        for extension in [None, Some(serde_json::Value::Null)] {
            let mut value = base.clone();
            if let Some(extension) = extension {
                value["extension"] = extension;
            }
            let receipt: FolderCreated = serde_json::from_value(value).unwrap();
            assert!(receipt.confirmed_identity("Plain").is_ok());
        }
        for (field, invalid) in [
            ("status", "CONFLICT"),
            ("drivewsid", "FILE::com.apple.CloudDocs::fixture"),
            ("drivewsid", "FOLDER::com.apple.CloudDocs::"),
            ("extension", "txt"),
        ] {
            let mut value = base.clone();
            value[field] = json!(invalid);
            let receipt: FolderCreated = serde_json::from_value(value).unwrap();
            assert!(receipt.confirmed_identity("Plain").is_err());
        }
    }

    #[test]
    fn conditional_trash_distinguishes_refusal_from_uncertain_transport() {
        assert!(classify_trash(StatusCode::OK, Some("OK")).unwrap());
        assert!(!classify_trash(StatusCode::OK, Some("CONFLICT")).unwrap());
        assert!(!classify_trash(StatusCode::PRECONDITION_FAILED, None).unwrap());
        assert!(classify_trash(StatusCode::TOO_MANY_REQUESTS, None).is_err());
        assert!(classify_trash(StatusCode::INTERNAL_SERVER_ERROR, None).is_err());
        assert!(classify_trash(StatusCode::OK, None).is_err());
    }

    #[test]
    fn conditional_rename_does_not_treat_throttling_as_a_clean_refusal() {
        assert!(classify_rename(StatusCode::OK, Some("OK")).unwrap());
        assert!(!classify_rename(StatusCode::CONFLICT, None).unwrap());
        assert!(classify_rename(StatusCode::TOO_MANY_REQUESTS, None).is_err());
        assert!(classify_rename(StatusCode::BAD_GATEWAY, None).is_err());
        assert!(classify_rename(StatusCode::OK, None).is_err());
    }

    #[test]
    fn conditional_move_does_not_treat_server_errors_as_refusal() {
        assert!(classify_move(StatusCode::OK, Some("OK")).unwrap());
        assert!(!classify_move(StatusCode::PRECONDITION_FAILED, None).unwrap());
        assert!(classify_move(StatusCode::TOO_MANY_REQUESTS, None).is_err());
        assert!(classify_move(StatusCode::SERVICE_UNAVAILABLE, None).is_err());
        assert!(classify_move(StatusCode::OK, None).is_err());
    }
}
