//! Read-only, fixed-scope metadata shape diagnostic. Never returns provider values.
use super::*;
use serde_json::Value;
use std::collections::BTreeMap;

fn shape(items: &[Value]) -> Value {
    let mut fields: BTreeMap<String, BTreeMap<&str, usize>> = BTreeMap::new();
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    for item in items {
        if let Some(object) = item.as_object() {
            for (key, value) in object {
                // Metadata keys only. Never descend into maps containing user data.
                if key.len() > 64 || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                    continue;
                }
                let ty = match value {
                    Value::Null => "null",
                    Value::Bool(true) => "true",
                    Value::Bool(false) => "false",
                    Value::Number(_) => "number",
                    Value::String(_) => "string",
                    Value::Array(_) => "array",
                    Value::Object(_) => "object",
                };
                *fields
                    .entry(key.clone())
                    .or_default()
                    .entry(ty)
                    .or_default() += 1;
            }
        }
        let kind = match item.get("type").and_then(Value::as_str) {
            Some("FILE") => "FILE",
            Some("FOLDER") => "FOLDER",
            Some("APP_CONTAINER") => "APP_CONTAINER",
            Some("APP_LIBRARY") => "APP_LIBRARY",
            _ => "other",
        };
        *kinds.entry(kind).or_default() += 1;
    }
    json!({"count":items.len(),"fields":fields,"kinds":kinds})
}

fn native_extension(app: &str) -> &str {
    match app {
        "keynote" => "key",
        other => other,
    }
}

impl ICloudReadSession {
    /// Root and at most three Apple document containers, selected by opaque ID.
    /// At most one download-location lookup per native app; never fetch its content.
    pub async fn document_metadata_shapes(&mut self) -> Result<Value> {
        let endpoint = self
            .drive_endpoint
            .as_ref()
            .context("iCloud sign-in is not complete")?
            .join("retrieveItemDetailsInFolders")
            .context("invalid iCloud endpoint")?;
        let mut folders = vec![("root", ROOT_ID.to_owned())];
        let mut reports = Vec::new();
        let mut index = 0;
        while index < folders.len() {
            let (label, id) = folders[index].clone();
            let response = self
                .http
                .post(endpoint.clone())
                .timeout(LISTING_TIMEOUT)
                .header("origin", ICLOUD_ORIGIN)
                .header("referer", format!("{ICLOUD_ORIGIN}/"))
                .json(&json!([{"drivewsid":id,"partialData":false,"includeHierarchy":false}]))
                .send()
                .await
                .map_err(|_| anyhow!("metadata shape request failed"))?;
            if !response.status().is_success() {
                return Err(drive_request_failure(
                    response.status(),
                    "metadata shape listing",
                ));
            }
            let result: Vec<Value> = read_json(response, "metadata shape listing").await?;
            if result.len() != 1 || result[0]["drivewsid"].as_str() != Some(&id) {
                bail!("metadata shape identity mismatch");
            }
            let items = result[0]["items"]
                .as_array()
                .context("metadata shape lacks items")?;
            if result[0]["numberOfItems"].as_u64() != Some(items.len() as u64) {
                return Err(IncompleteFolder.into());
            }
            if index == 0 {
                for (app, expected) in [
                    ("pages", "FOLDER::com.apple.Pages::documents"),
                    ("numbers", "FOLDER::com.apple.Numbers::documents"),
                    ("keynote", "FOLDER::com.apple.Keynote::documents"),
                ] {
                    if items
                        .iter()
                        .any(|v| v["drivewsid"].as_str() == Some(expected))
                    {
                        folders.push((app, expected.to_owned()));
                    }
                }
            }
            let sample = if label == "root" {
                None
            } else {
                items
                    .iter()
                    .find(|v| v["type"] == "FILE" && v["extension"] == native_extension(label))
            };
            let representation = if let Some(sample) = sample {
                let id = sample["drivewsid"]
                    .as_str()
                    .context("sample identity missing")?;
                let (zone, doc_id) = split_file_id(id)?;
                let url = self
                    .docs_endpoint
                    .as_ref()
                    .context("document endpoint missing")?
                    .join(&format!("ws/{zone}/download/by_id"))
                    .context("invalid document endpoint")?;
                let response = self
                    .http
                    .get(url)
                    .query(&[("document_id", doc_id)])
                    .header("origin", ICLOUD_ORIGIN)
                    .header("referer", format!("{ICLOUD_ORIGIN}/"))
                    .send()
                    .await
                    .map_err(|_| anyhow!("representation lookup failed"))?;
                if !response.status().is_success() {
                    return Err(drive_request_failure(
                        response.status(),
                        "representation lookup",
                    ));
                }
                let location: DownloadLocation =
                    read_json(response, "representation lookup").await?;
                json!({"sample_available":true,"data_token":location.data_token.is_some(),
                    "package_token":location.package_token.is_some(),"content_downloaded":false})
            } else {
                json!({"sample_available":false})
            };
            reports.push(
                json!({"container":label,"shape":shape(items),"representation":representation}),
            );
            index += 1;
        }
        Ok(json!({"read_only":true,"reports":reports}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_app_selection_uses_the_document_extension() {
        assert_eq!(native_extension("pages"), "pages");
        assert_eq!(native_extension("numbers"), "numbers");
        assert_eq!(native_extension("keynote"), "key");
    }
    #[test]
    fn metadata_shape_keeps_types_and_counts_without_provider_values() {
        let result = shape(&[
            json!({"name":"PRIVATE_NAME","drivewsid":"PRIVATE_ID",
            "url":"PRIVATE_URL","type":"PRIVATE_TYPE","nested":{"private":"PRIVATE_NESTED"},
            "isPackage":true}),
            json!({"type":"FILE","isPackage":false}),
        ]);
        let text = result.to_string();
        assert!(!text.contains("PRIVATE"));
        assert_eq!(result["fields"]["isPackage"]["true"], 1);
        assert_eq!(result["fields"]["isPackage"]["false"], 1);
        assert_eq!(result["kinds"]["other"], 1);
        assert_eq!(result["kinds"]["FILE"], 1);
    }
}
