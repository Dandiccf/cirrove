//! Read-only, explicitly owned native-package recovery experiment.
use super::*;
use cirrove_core::{CancellationToken, ProviderError, reads::ReadWindowSink};
use std::{
    fs::File,
    os::unix::fs::{MetadataExt, PermissionsExt},
};
use tokio::io::AsyncWriteExt;

/// Bind these fields from the retained owned-import plan, never a Trash search.
/// The Apple account identifier is checked against the signed-in session locally.
pub struct OwnedPackageTrashRequest {
    pub apple_account: String,
    pub drive_id: String,
    pub document_id: String,
    pub expected_root: String,
    pub semantic: PackageSemanticIdentity,
}

/// Success proves a matching package remained recoverable during this read.
/// It does not prove conditional mutation semantics or authorize a replacement.
pub struct VerifiedPackageTrash {
    pub archive: PackageDownload,
    pub semantic: PackageSemanticIdentity,
    pub trash_etag: String,
}

fn observation(
    item: &serde_json::Value,
    request: &OwnedPackageTrashRequest,
) -> Result<serde_json::Value> {
    if item.get("drivewsid").and_then(serde_json::Value::as_str) != Some(request.drive_id.as_str())
        || item.get("docwsid").and_then(serde_json::Value::as_str)
            != Some(request.document_id.as_str())
        || item.get("type").and_then(serde_json::Value::as_str) != Some("FILE")
        || !matches!(
            item.get("parentId").and_then(serde_json::Value::as_str),
            Some("TRASH_ROOT" | write_transport::TRASH_ROOT)
        )
        || item
            .get("restorePath")
            .is_none_or(serde_json::Value::is_null)
        || item
            .get("etag")
            .and_then(serde_json::Value::as_str)
            .is_none_or(|s| s.is_empty() || s.len() > 4096 || s.contains(['\0', '\r', '\n']))
        || item
            .get("name")
            .and_then(serde_json::Value::as_str)
            .is_none_or(str::is_empty)
        || item
            .get("size")
            .and_then(serde_json::Value::as_u64)
            .is_none()
    {
        bail!("iCloud package lacks its exact Trash recovery identity");
    }
    // Compare only the recovery binding, not unrelated volatile metadata.
    Ok(
        json!({"drivewsid":item["drivewsid"],"docwsid":item["docwsid"],
        "type":item["type"],"parentId":item["parentId"],"restorePath":item["restorePath"],
        "etag":item["etag"],"name":item["name"],"extension":item["extension"],"size":item["size"]}),
    )
}
fn unchanged(
    before: &serde_json::Value,
    after: &serde_json::Value,
    request: &OwnedPackageTrashRequest,
) -> Result<()> {
    if &observation(after, request)? != before {
        return Err(StaleRead.into());
    }
    Ok(())
}
fn verify_archive(
    file: &File,
    archive: &PackageDownload,
    request: &OwnedPackageTrashRequest,
    cancel: &CancellationToken,
) -> std::result::Result<PackageSemanticIdentity, ProviderError> {
    let semantic =
        package_archive_semantic_identity(file, archive, &request.expected_root, cancel)?;
    if semantic != request.semantic {
        return Err(ProviderError::Protocol("package recovery content mismatch"));
    }
    Ok(semantic)
}
struct Sink(tokio::fs::File);
#[async_trait::async_trait]
impl ReadWindowSink for Sink {
    async fn write_chunk(&mut self, bytes: &[u8]) -> std::result::Result<(), ProviderError> {
        self.0
            .write_all(bytes)
            .await
            .map_err(|_| ProviderError::Protocol("package recovery staging unavailable"))
    }
}
impl ICloudReadSession {
    /// Consume an empty anonymous private regular file created in the validator's
    /// disk staging directory. No archive or signed URL is published. The caller
    /// must bound concurrency and must not retain a writable clone of this file.
    pub async fn verify_owned_package_in_trash(
        &mut self,
        request: OwnedPackageTrashRequest,
        staging: File,
        cancel: &CancellationToken,
    ) -> Result<VerifiedPackageTrash> {
        tokio::select! { biased;
            _ = cancel.cancelled() => Err(ProviderError::Cancelled.into()),
            result = async {
                if self.account_hash.as_deref() != Some(account_hash(&request.apple_account)?.as_str())
                    || request.document_id.is_empty()
                    || request.drive_id != format!("FILE::com.apple.CloudDocs::{}", request.document_id)
                { bail!("iCloud package recovery account or identity mismatch"); }
                let meta = staging.metadata().map_err(|_| anyhow!("package recovery staging unavailable"))?;
                if !meta.is_file() || meta.len() != 0 || meta.nlink() != 0 || meta.permissions().mode() & 0o077 != 0 {
                    bail!("package recovery requires empty anonymous private staging");
                }
                let before = observation(&self.item_details(&request.drive_id).await?, &request)?;
                let ContentRepresentation::Package(url) = self.download_representation(&request.drive_id).await? else {
                    bail!("iCloud Trash item does not expose a package representation");
                };
                let response = self.http.get(url).header(reqwest::header::ACCEPT_ENCODING, "identity")
                    .timeout(VERIFICATION_TRANSFER_TIMEOUT).send().await
                    .map_err(|_| anyhow!("iCloud package recovery download failed"))?;
                let clone = staging.try_clone().map_err(|_| anyhow!("package recovery staging unavailable"))?;
                let mut sink = Sink(tokio::fs::File::from_std(clone));
                let archive = package_download::stage_response(response, &mut sink, 64 * 1024 * 1024, cancel).await?;
                sink.0.flush().await.map_err(|_| anyhow!("package recovery staging unavailable"))?;
                drop(sink);
                unchanged(&before, &self.item_details(&request.drive_id).await?, &request)?;
                let token = cancel.clone();
                let semantic = tokio::task::spawn_blocking(move || {
                    let semantic = verify_archive(&staging, &archive, &request, &token)?;
                    Ok::<_, ProviderError>((archive, semantic))
                }).await.map_err(|_| anyhow!("package recovery verification interrupted"))??;
                if cancel.is_cancelled() { return Err(ProviderError::Cancelled.into()); }
                Ok(VerifiedPackageTrash { archive: semantic.0, semantic: semantic.1,
                    trash_etag: before["etag"].as_str().expect("validated revision").to_owned() })
            } => result,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> OwnedPackageTrashRequest {
        OwnedPackageTrashRequest {
            apple_account: "fixture@example.com".into(),
            drive_id: "FILE::com.apple.CloudDocs::owned".into(),
            document_id: "owned".into(),
            expected_root: "Owned.pages".into(),
            semantic: PackageSemanticIdentity {
                version: 1,
                sha256: "a".repeat(64),
                entries: 1,
                files: 1,
                expanded_bytes: 1,
            },
        }
    }
    fn metadata() -> serde_json::Value {
        json!({"drivewsid":"FILE::com.apple.CloudDocs::owned","docwsid":"owned","type":"FILE","parentId":"TRASH_ROOT","restorePath":["owned-parent"],"etag":"trash-version","name":"Owned","extension":"pages","size":123})
    }
    #[test]
    fn package_trash_binding_refuses_foreign_identity_and_changed_recovery_revision() -> Result<()>
    {
        let request = request();
        let original = metadata();
        let before = observation(&original, &request)?;
        unchanged(&before, &original, &request)?;
        for key in [
            "drivewsid",
            "docwsid",
            "type",
            "parentId",
            "restorePath",
            "etag",
            "name",
            "size",
        ] {
            let mut bad = original.clone();
            bad.as_object_mut().expect("object").remove(key);
            assert!(
                observation(&bad, &request).is_err(),
                "missing {key} admitted"
            );
        }
        for key in [
            "drivewsid",
            "docwsid",
            "type",
            "parentId",
            "restorePath",
            "etag",
            "name",
            "extension",
            "size",
        ] {
            let mut changed = original.clone();
            changed[key] = if key == "size" {
                json!(124)
            } else {
                json!("changed")
            };
            assert!(
                unchanged(&before, &changed, &request).is_err(),
                "changed {key} admitted"
            );
        }
        Ok(())
    }
    #[tokio::test]
    async fn package_trash_wrong_account_and_cancellation_need_no_provider_request() -> Result<()> {
        let mut session = ICloudReadSession::new()?;
        session.account_hash = Some(account_hash("another@example.com")?);
        let result = session
            .verify_owned_package_in_trash(
                request(),
                tempfile::tempfile()?,
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(
            result.err().expect("account refused").to_string(),
            "iCloud package recovery account or identity mismatch"
        );
        let cancel = CancellationToken::new();
        cancel.cancel();
        let error = session
            .verify_owned_package_in_trash(request(), tempfile::tempfile()?, &cancel)
            .await
            .err()
            .expect("cancelled");
        assert!(matches!(
            error.downcast_ref::<ProviderError>(),
            Some(ProviderError::Cancelled)
        ));
        Ok(())
    }
    #[tokio::test]
    async fn package_trash_http_metadata_refusal_precedes_any_download_lookup() -> Result<()> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut bytes = Vec::new();
            let end = loop {
                let mut chunk = [0; 1024];
                let count = socket.read(&mut chunk).await.expect("read");
                assert!(count > 0 && bytes.len() < 8192);
                bytes.extend_from_slice(&chunk[..count]);
                if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                    break end + 4;
                }
            };
            let headers = std::str::from_utf8(&bytes[..end])
                .expect("headers")
                .to_lowercase();
            assert!(headers.starts_with("post /retrieveitemdetails "));
            let length: usize = headers
                .lines()
                .find_map(|s| s.strip_prefix("content-length: "))
                .expect("length")
                .parse()
                .expect("number");
            while bytes.len() - end < length {
                let mut chunk = [0; 1024];
                let count = socket.read(&mut chunk).await.expect("body");
                assert!(count > 0);
                bytes.extend_from_slice(&chunk[..count]);
            }
            let body: serde_json::Value =
                serde_json::from_slice(&bytes[end..end + length]).expect("json");
            assert_eq!(
                body,
                json!({"items":[{"drivewsid":"FILE::com.apple.CloudDocs::owned","partialData":false,"includeHierarchy":false}]})
            );
            let mut item = metadata();
            item["parentId"] = json!("active-parent");
            let body = serde_json::to_vec(&json!({"items":[item]})).expect("json");
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .expect("header");
            socket.write_all(&body).await.expect("response");
        });
        let mut session = ICloudReadSession::new()?;
        session.account_hash = Some(account_hash("fixture@example.com")?);
        session.drive_endpoint = Some(format!("http://{address}/").parse()?);
        // tempfile's Linux O_TMPFILE path uses OpenOptions' default creation
        // mode, so anonymous does not imply 0600. Match the verifier contract
        // explicitly without weakening its permission check.
        let staging = tempfile::tempfile()?;
        staging.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        let metadata = staging.metadata()?;
        assert!(metadata.is_file() && metadata.len() == 0 && metadata.nlink() == 0);
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        // No docs endpoint: reaching representation lookup gives a different error.
        let error = tokio::time::timeout(
            Duration::from_secs(3),
            session.verify_owned_package_in_trash(request(), staging, &CancellationToken::new()),
        )
        .await?
        .err()
        .expect("must refuse");
        assert_eq!(
            error.to_string(),
            "iCloud package lacks its exact Trash recovery identity"
        );
        tokio::time::timeout(Duration::from_secs(3), server).await??;
        Ok(())
    }
    #[test]
    fn package_trash_semantics_require_exact_root_content_and_uncancelled_validation() -> Result<()>
    {
        use std::io::{Seek, Write};
        let mut file = tempfile::tempfile()?;
        {
            let mut zip = zip::ZipWriter::new(&mut file);
            zip.start_file(
                "Owned.pages/Index/document.iwa",
                zip::write::SimpleFileOptions::default(),
            )?;
            zip.write_all(b"synthetic owned content")?;
            zip.finish()?;
        }
        file.rewind()?;
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut file, &mut bytes)?;
        let receipt = PackageDownload {
            size: bytes.len() as u64,
            sha256: hex::encode(Sha256::digest(&bytes)),
        };
        let cancel = CancellationToken::new();
        let mut expected = request();
        expected.semantic =
            package_archive_semantic_identity(&file, &receipt, &expected.expected_root, &cancel)?;
        assert_eq!(
            verify_archive(&file, &receipt, &expected, &cancel)?,
            expected.semantic
        );
        expected.expected_root = "Other.pages".into();
        assert!(verify_archive(&file, &receipt, &expected, &cancel).is_err());
        expected.expected_root = "Owned.pages".into();
        expected.semantic.sha256 = "b".repeat(64);
        assert!(verify_archive(&file, &receipt, &expected, &cancel).is_err());
        cancel.cancel();
        assert!(matches!(
            verify_archive(&file, &receipt, &expected, &cancel),
            Err(ProviderError::Cancelled)
        ));
        Ok(())
    }
}
