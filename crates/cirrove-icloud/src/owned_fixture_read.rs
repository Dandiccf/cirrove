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
fn exact(expected: &DriveEntry, observed: &DriveEntry) -> Result<()> {
    if expected != observed {
        bail!("owned fixture metadata changed");
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
        let result = tokio::time::timeout(std::time::Duration::from_secs(240), async {
            ensure_fixture(parent, source)?;
            exact_parent(
                parent,
                self.active_folder_metadata(&parent.drivewsid).await?,
            )?;
            exact(
                source,
                &self
                    .item_for_read(&source.drivewsid, Some(&parent.drivewsid))
                    .await?,
            )?;
            let location = self.download_representation(&source.drivewsid).await?;
            representation(expected, kind(&location))?;
            if cancel.is_cancelled() {
                bail!("owned fixture read cancelled");
            }
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
            representation(
                expected,
                kind(&self.download_representation(&source.drivewsid).await?),
            )?;
            exact(
                source,
                &self
                    .item_for_read(&source.drivewsid, Some(&parent.drivewsid))
                    .await?,
            )?;
            exact_parent(
                parent,
                self.active_folder_metadata(&parent.drivewsid).await?,
            )?;
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
        result.map_err(|_| anyhow!("owned fixture read refused"))
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
