//! Feature-only read observation; no URL or session material crosses this API.
use super::*;
use cirrove_core::{CancellationToken, reads::ReadWindowSink};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureRepresentation {
    Data,
    Package,
}
fn kind(value: &ContentRepresentation) -> FixtureRepresentation {
    match value {
        ContentRepresentation::Data(_) => FixtureRepresentation::Data,
        ContentRepresentation::Package(_) => FixtureRepresentation::Package,
    }
}
// Only a compile-time field name can cross the diagnostic boundary.
#[derive(Debug)]
struct MetadataDifference(&'static str);
impl std::fmt::Display for MetadataDifference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} differs", self.0)
    }
}
impl std::error::Error for MetadataDifference {}
fn exact(expected: &DriveEntry, observed: &DriveEntry) -> Result<()> {
    if expected != observed {
        let field = [
            (expected.drivewsid != observed.drivewsid, "drivewsid"),
            (expected.docwsid != observed.docwsid, "docwsid"),
            (expected.item_id != observed.item_id, "item_id"),
            (expected.zone != observed.zone, "zone"),
            (expected.name != observed.name, "name"),
            (expected.extension != observed.extension, "extension"),
            (expected.parent_id != observed.parent_id, "parent_id"),
            (expected.etag != observed.etag, "etag"),
            (expected.kind != observed.kind, "kind"),
            (expected.size != observed.size, "size"),
            (expected.items != observed.items, "items"),
            (
                expected.number_of_items != observed.number_of_items,
                "number_of_items",
            ),
        ]
        .into_iter()
        .find_map(|(differs, field)| differs.then_some(field))
        .unwrap_or("metadata");
        return Err(MetadataDifference(field).into());
    }
    Ok(())
}
fn exact_parent(expected: &DriveEntry, mut observed: DriveEntry) -> Result<()> {
    observed.items.clear();
    observed.number_of_items = None;
    exact(expected, &observed)
}
fn representation(expected: FixtureRepresentation, actual: FixtureRepresentation) -> Result<()> {
    if expected != actual {
        bail!("owned fixture representation changed");
    }
    Ok(())
}
impl ICloudReadSession {
    /// Metadata and representation fences bracket a bounded full download.
    /// An exact active ordinary parent is required; no accountLogin is performed.
    pub async fn read_owned_fixture(
        &mut self,
        parent: &DriveEntry,
        source: &DriveEntry,
        expected: FixtureRepresentation,
        sink: &mut dyn ReadWindowSink,
        cancel: &CancellationToken,
    ) -> Result<PackageDownload> {
        let mut phase = "fixture identity";
        let result = tokio::time::timeout(std::time::Duration::from_secs(240), async {
            ensure_fixture(parent, source)?;
            phase = "pre-transfer parent metadata";
            let mut active_parent = self.active_folder_metadata(&parent.drivewsid).await?;
            active_parent.items.clear();
            active_parent.number_of_items = None;
            // Root-list children and direct folder envelopes expose different
            // optional item_id values. The canonical drivewsid remains exact.
            // Bind the envelope value now, then fence it again after transfer.
            let mut registered_parent = parent.clone();
            registered_parent.item_id.clone_from(&active_parent.item_id);
            exact(&registered_parent, &active_parent)?;
            phase = "pre-transfer item metadata";
            exact(
                source,
                &self
                    .item_for_read(&source.drivewsid, Some(&parent.drivewsid))
                    .await?,
            )?;
            phase = "pre-transfer representation";
            let location = self.download_representation(&source.drivewsid).await?;
            representation(expected, kind(&location))?;
            if cancel.is_cancelled() {
                bail!("owned fixture read cancelled");
            }
            phase = "content transfer";
            let response = self
                .http
                .get(location.for_exact_read())
                .header(reqwest::header::ACCEPT_ENCODING, "identity")
                .timeout(VERIFICATION_TRANSFER_TIMEOUT)
                .send()
                .await
                .map_err(|_| anyhow!("owned fixture content unavailable"))?;
            let receipt =
                crate::package_download::stage_response(response, sink, 64 * 1024 * 1024, cancel)
                    .await?;
            phase = "post-transfer representation";
            representation(
                expected,
                kind(&self.download_representation(&source.drivewsid).await?),
            )?;
            phase = "post-transfer item metadata";
            exact(
                source,
                &self
                    .item_for_read(&source.drivewsid, Some(&parent.drivewsid))
                    .await?,
            )?;
            phase = "post-transfer parent metadata";
            exact_parent(
                &active_parent,
                self.active_folder_metadata(&parent.drivewsid).await?,
            )?;
            phase = "content receipt";
            if expected == FixtureRepresentation::Data && receipt.size != source.size {
                bail!("owned fixture DATA size differs");
            }
            if cancel.is_cancelled() {
                bail!("owned fixture read cancelled");
            }
            Ok(receipt)
        })
        .await
        .map_err(|_| anyhow!("owned fixture read deadline"))?;
        result.map_err(|error: anyhow::Error| {
            if let Some(field) = error.downcast_ref::<MetadataDifference>() {
                anyhow!("owned fixture read refused during {phase}: {field}")
            } else {
                anyhow!("owned fixture read refused during {phase}")
            }
        })
    }
}
fn ensure_fixture(parent: &DriveEntry, source: &DriveEntry) -> Result<()> {
    if parent.kind != "FOLDER"
        || parent.zone != "com.apple.CloudDocs"
        || !parent
            .drivewsid
            .starts_with("FOLDER::com.apple.CloudDocs::")
        || parent.drivewsid == ROOT_ID
        || parent.drivewsid.ends_with("::TRASH_ROOT")
        || source.kind != "FILE"
        || source.zone != "com.apple.CloudDocs"
        || source.drivewsid != format!("FILE::com.apple.CloudDocs::{}", source.docwsid)
        || source.docwsid.is_empty()
        || source.parent_id != parent.drivewsid
        || source.etag.is_empty()
        || source.etag.contains('*')
        || source.size > 64 * 1024 * 1024
        || !source.items.is_empty()
        || !parent.items.is_empty()
        || parent.number_of_items.is_some()
    {
        bail!("owned fixture identity refused");
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn owned_fixture_read_fences_revision_parent_and_representation() -> Result<()> {
        let parent: DriveEntry = serde_json::from_value(
            serde_json::json!({"drivewsid":"FOLDER::com.apple.CloudDocs::owned","zone":"com.apple.CloudDocs","type":"FOLDER"}),
        )?;
        let source: DriveEntry = serde_json::from_value(
            serde_json::json!({"drivewsid":"FILE::com.apple.CloudDocs::doc","docwsid":"doc","zone":"com.apple.CloudDocs","parentId":parent.drivewsid,"type":"FILE","etag":"v1","size":1}),
        )?;
        ensure_fixture(&parent, &source)?;
        exact(&source, &source)?;
        let mut changed = source.clone();
        changed.etag = "v2".into();
        assert!(exact(&source, &changed).is_err());
        changed = source.clone();
        changed.parent_id = "foreign".into();
        assert!(ensure_fixture(&parent, &changed).is_err());
        assert!(
            representation(FixtureRepresentation::Package, FixtureRepresentation::Data).is_err()
        );
        assert!(
            representation(FixtureRepresentation::Data, FixtureRepresentation::Package).is_err()
        );
        Ok(())
    }
}
#[cfg(test)]
mod http_tests;
