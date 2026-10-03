//! Read-only comparison of exact-ID metadata against authoritative Trash listings.
//! Feature-gated diagnostics return booleans/timings only, never provider bodies.
use super::*;
use serde_json::Value;
use std::time::Instant;

fn comparison(listed: &Value, direct: &Value) -> Value {
    let fields = [
        "drivewsid",
        "docwsid",
        "etag",
        "size",
        "name",
        "extension",
        "type",
        "parentId",
        "restorePath",
    ];
    let mut result = serde_json::Map::new();
    for field in fields {
        let expected = listed.get(field).filter(|value| !value.is_null());
        let actual = direct.get(field).filter(|value| !value.is_null());
        result.insert(
            field.into(),
            json!({
                "listed_present": expected.is_some(),
                "direct_present": actual.is_some(),
                "equal": expected.is_some() && expected == actual,
            }),
        );
    }
    Value::Object(result)
}

impl ICloudReadSession {
    async fn lookup_shape(&mut self, id: &str, listed: &Value, shape: &str) -> Result<Value> {
        if shape == "production" {
            let started = Instant::now();
            let item = self.item_details(id).await?;
            return Ok(
                json!({"shape":shape,"elapsed_ms":started.elapsed().as_millis(),
                "status":200,"array_entries":1,"fields":comparison(listed,&item),
                "parent_is_trash":matches!(item.get("parentId").and_then(Value::as_str), Some("TRASH_ROOT" | "FOLDER::com.apple.CloudDocs::TRASH_ROOT")),
                "type_is_file":item.get("type").and_then(Value::as_str) == Some("FILE")}),
            );
        }
        let endpoint = self
            .drive_endpoint
            .as_ref()
            .context("iCloud sign-in is not complete")?
            .join("retrieveItemDetails")?;
        let item = json!({"drivewsid":id,"partialData":false,"includeHierarchy":false});
        let request = match shape {
            "nested_array" => json!([{"items":[item]}]),
            "object_items" => json!({"items":[item]}),
            "flat_array" => json!([item]),
            _ => bail!("unknown read-only lookup envelope"),
        };
        let started = Instant::now();
        let response = self
            .http
            .post(endpoint)
            .timeout(LISTING_TIMEOUT)
            .header("origin", ICLOUD_ORIGIN)
            .header("referer", format!("{ICLOUD_ORIGIN}/"))
            .json(&request)
            .send()
            .await
            .map_err(|_| anyhow!("direct Trash comparison request failed"))?;
        let status = response.status().as_u16();
        let body: Option<Value> = if response.status().is_success() {
            Some(read_json(response, "direct Trash comparison").await?)
        } else {
            None
        };
        let elapsed_ms = started.elapsed().as_millis();
        let entries = body.as_ref().and_then(|body| {
            body.as_array()
                .or_else(|| body.get("items").and_then(Value::as_array))
        });
        let item = entries
            .filter(|items| items.len() == 1)
            .and_then(|items| items.first());
        Ok(
            json!({"shape":shape,"status":status,"elapsed_ms":elapsed_ms,
            "response_object":body.as_ref().is_some_and(Value::is_object),
            "array_entries":entries.map(Vec::len),"fields":item.map(|item| comparison(listed,item))}),
        )
    }

    /// One read-only envelope control, including an active exact-ID reference.
    pub async fn compare_trash_lookup_envelopes(
        &mut self,
        ids: &[String],
        active: &cirrove_core::Node,
    ) -> Result<Value> {
        if ids.len() != 2
            || ids[0] == ids[1]
            || ids.contains(&active.id)
            || ids
                .iter()
                .chain(std::iter::once(&active.id))
                .any(|id| split_file_id(id).is_err())
        {
            bail!("invalid envelope comparison identities");
        }
        let parent = active
            .parent_id
            .as_deref()
            .context("active comparison lacks parent")?;
        let active_entries = self.list_folder(parent).await?;
        let observed = active_entries
            .iter()
            .find(|item| item.drivewsid == active.id)
            .context("active comparison file missing")?;
        if active.etag.as_deref() != Some(&observed.etag)
            || active.size != observed.size
            || active.name != observed.display_name()
            || observed.is_folder()
        {
            bail!("active comparison revision changed");
        }
        let (before, complete) = self.read_trash_items().await?;
        if !complete {
            bail!("envelope comparison listing incomplete");
        }
        let mut expected = vec![serde_json::to_value(observed)?];
        for id in ids {
            let matches: Vec<_> = before
                .iter()
                .filter(|item| item.get("drivewsid").and_then(Value::as_str) == Some(id))
                .collect();
            match matches.as_slice() {
                [item]
                    if item
                        .get("restorePath")
                        .is_some_and(|value| !value.is_null()) =>
                {
                    expected.push((*item).clone())
                }
                _ => bail!("envelope comparison lacks exact recoverable identity"),
            }
        }
        let mut reports = Vec::new();
        for shape in ["nested_array", "object_items", "flat_array"] {
            let mut rows = Vec::new();
            for (id, listed) in std::iter::once(&active.id).chain(ids).zip(&expected) {
                rows.push(self.lookup_shape(id, listed, shape).await?);
            }
            reports.push(json!({"shape":shape,"active_then_predecessors":rows}));
        }
        let (after, complete) = self.read_trash_items().await?;
        let after_active = self.list_folder(parent).await?;
        let stable = complete
            && ids
                .iter()
                .zip(expected.iter().skip(1))
                .all(|(id, expected)| {
                    let matches: Vec<_> = after
                        .iter()
                        .filter(|item| item.get("drivewsid").and_then(Value::as_str) == Some(id))
                        .collect();
                    matches.len() == 1 && matches[0] == expected
                })
            && after_active
                .iter()
                .filter(|item| item.drivewsid == active.id)
                .collect::<Vec<_>>()
                == vec![observed];
        Ok(json!({"selected_metadata_stable":stable,"envelopes":reports}))
    }

    /// Diagnostic only: does not replace the production Trash verification.
    pub async fn compare_trash_lookup(&mut self, ids: &[String]) -> Result<Value> {
        if ids.len() != 2
            || ids[0] == ids[1]
            || ids.iter().any(|id| {
                !id.starts_with("FILE::com.apple.CloudDocs::") || split_file_id(id).is_err()
            })
        {
            bail!("invalid owned Trash comparison identities");
        }
        let before_started = Instant::now();
        let (before, complete) = self.read_trash_items().await?;
        let before_ms = before_started.elapsed().as_millis();
        if !complete {
            bail!("comparison Trash listing is incomplete");
        }
        let selected = |items: &[Value], id: &str| -> Result<Value> {
            let matches: Vec<_> = items
                .iter()
                .filter(|item| item.get("drivewsid").and_then(Value::as_str) == Some(id))
                .collect();
            match matches.as_slice() {
                [item]
                    if item
                        .get("restorePath")
                        .is_some_and(|value| !value.is_null()) =>
                {
                    Ok((*item).clone())
                }
                _ => bail!("comparison requires exactly one recoverable owned identity"),
            }
        };
        let mut reports = Vec::new();
        for id in ids {
            let listed = selected(&before, id)?;
            reports.push(self.lookup_shape(id, &listed, "production").await?);
        }
        let after_started = Instant::now();
        let (after, complete) = self.read_trash_items().await?;
        let after_ms = after_started.elapsed().as_millis();
        if !complete {
            bail!("comparison final Trash listing is incomplete");
        }
        let mut stable = true;
        for id in ids {
            let first = selected(&before, id)?;
            let last = selected(&after, id)?;
            // Compare all fields used for the existing recovery receipt, including
            // the actual restore path. Dynamic unrelated fields cannot authorize it.
            for key in [
                "drivewsid",
                "docwsid",
                "etag",
                "size",
                "name",
                "extension",
                "type",
                "parentId",
                "restorePath",
            ] {
                stable &= first.get(key) == last.get(key);
            }
        }
        Ok(json!({"full_before_ms":before_ms,"full_after_ms":after_ms,
            "full_before_count":before.len(),"full_after_count":after.len(),
            "full_listings_complete":true,"selected_metadata_stable":stable,"direct":reports}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_fields_are_not_reported_as_matching() {
        let result = comparison(&json!({"size":0}), &json!({"size":0}));
        assert_eq!(result["size"]["equal"], true);
        assert_eq!(result["restorePath"]["equal"], false);
        assert_eq!(result["drivewsid"]["equal"], false);
    }
    #[test]
    fn mismatched_identity_and_recovery_path_remain_visible() {
        let result = comparison(
            &json!({"drivewsid":"a","restorePath":["parent"]}),
            &json!({"drivewsid":"b","restorePath":["other"]}),
        );
        assert_eq!(result["drivewsid"]["equal"], false);
        assert_eq!(result["restorePath"]["equal"], false);
    }
}
