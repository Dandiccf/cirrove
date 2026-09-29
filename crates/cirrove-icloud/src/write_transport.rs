//! Conditional iCloud Drive requests shared by the isolated validator and a
//! future account writer. No normal account constructs a write provider yet.
use crate::{ICLOUD_ORIGIN, ICloudReadSession, LISTING_TIMEOUT, drive_request_failure, read_json};
use anyhow::{Context, Result, anyhow, bail};
use reqwest::{Response, StatusCode};
use serde::Deserialize;
use serde_json::json;

pub(crate) const TRASH_ROOT: &str = "FOLDER::com.apple.CloudDocs::TRASH_ROOT";

#[derive(Deserialize)]
struct TrashReply {
    items: Vec<TrashResult>,
}

#[derive(Deserialize)]
struct TrashResult {
    status: String,
}

fn classify_trash(status: StatusCode, result: Option<&str>) -> Result<bool> {
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

impl ICloudReadSession {
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
    fn conditional_trash_distinguishes_refusal_from_uncertain_transport() {
        assert!(classify_trash(StatusCode::OK, Some("OK")).unwrap());
        assert!(!classify_trash(StatusCode::OK, Some("CONFLICT")).unwrap());
        assert!(!classify_trash(StatusCode::PRECONDITION_FAILED, None).unwrap());
        assert!(classify_trash(StatusCode::TOO_MANY_REQUESTS, None).is_err());
        assert!(classify_trash(StatusCode::INTERNAL_SERVER_ERROR, None).is_err());
        assert!(classify_trash(StatusCode::OK, None).is_err());
    }
}
