//! Complete package representations, staged by the caller before publication.
use super::*;
use cirrove_core::{CancellationToken, ProviderError, reads::ReadWindowSink};

/// Describes the downloaded representation, not the source listing's logical size.
/// The caller may publish its private staging only after this receipt is returned.
pub struct PackageDownload {
    pub size: u64,
    pub sha256: String,
}

fn check_source(expected: &DriveEntry, observed: &DriveEntry) -> Result<()> {
    if expected.kind != "FILE"
        || expected.etag.is_empty()
        || observed.kind != "FILE"
        || observed.drivewsid != expected.drivewsid
        || observed.docwsid != expected.docwsid
        || observed.parent_id != expected.parent_id
        || observed.etag != expected.etag
        || observed.size != expected.size
    {
        return Err(StaleRead.into());
    }
    Ok(())
}

pub(super) async fn stage_response(
    response: Response,
    sink: &mut dyn ReadWindowSink,
    max_bytes: u64,
    cancel: &CancellationToken,
) -> Result<PackageDownload> {
    tokio::select! { biased;
        _ = cancel.cancelled() => Err(ProviderError::Cancelled.into()),
        result = async {
            if response.status() != StatusCode::OK
                || response.headers().contains_key(CONTENT_RANGE)
                || response.headers().get(reqwest::header::CONTENT_ENCODING).is_some_and(|v| v != "identity")
            {
                bail!("iCloud package response is not a complete identity representation");
            }
            let declared = response.content_length();
            if declared.is_some_and(|size| size > max_bytes) {
                bail!("iCloud package exceeds the staging budget");
            }
            let mut response = response;
            let mut size = 0u64;
            let mut hash = Sha256::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| anyhow!("iCloud package transfer interrupted"))? {
                let next = size.checked_add(chunk.len() as u64).context("iCloud package length overflow")?;
                if next > max_bytes { bail!("iCloud package exceeds the staging budget"); }
                for part in chunk.chunks(64 * 1024) {
                    if cancel.is_cancelled() { return Err(ProviderError::Cancelled.into()); }
                    sink.write_chunk(part).await?;
                    hash.update(part);
                }
                size = next;
            }
            if cancel.is_cancelled() { return Err(ProviderError::Cancelled.into()); }
            if size == 0 || declared.is_some_and(|declared| declared != size) {
                bail!("iCloud package transfer is incomplete");
            }
            Ok(PackageDownload { size, sha256: hex::encode(hash.finalize()) })
        } => result,
    }
}

impl ICloudReadSession {
    /// Stage a complete package into caller-owned private storage. On any error
    /// or cancellation, none of the staged bytes may be published. No range or
    /// listing-size assumption applies to the generated representation.
    pub async fn download_package(
        &mut self,
        folder_id: &str,
        source: &DriveEntry,
        max_bytes: u64,
        sink: &mut dyn ReadWindowSink,
        cancel: &CancellationToken,
    ) -> Result<PackageDownload> {
        tokio::select! { biased;
            _ = cancel.cancelled() => Err(ProviderError::Cancelled.into()),
            result = async {
                if max_bytes == 0 || max_bytes > MAX_WRITE_FILE_SIZE {
                    bail!("invalid iCloud package staging budget");
                }
                let before = self.item_for_read(&source.drivewsid, Some(folder_id)).await?;
                check_source(source, &before)?;
                let ContentRepresentation::Package(url) = self.download_representation(&source.drivewsid).await? else {
                    bail!("iCloud item does not expose a package representation");
                };
                let response = self.http.get(url)
                    .header(reqwest::header::ACCEPT_ENCODING, "identity")
                    .timeout(VERIFICATION_TRANSFER_TIMEOUT)
                    .send().await.map_err(|_| anyhow!("iCloud package request failed"))?;
                let artifact = stage_response(response, sink, max_bytes, cancel).await?;
                let after = self.item_for_read(&source.drivewsid, Some(folder_id)).await?;
                check_source(source, &after)?;
                if cancel.is_cancelled() { return Err(ProviderError::Cancelled.into()); }
                Ok(artifact)
            } => result,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[derive(Default)]
    struct Sink {
        bytes: Vec<u8>,
        chunks: usize,
    }
    #[async_trait]
    impl ReadWindowSink for Sink {
        async fn write_chunk(&mut self, bytes: &[u8]) -> std::result::Result<(), ProviderError> {
            assert!(bytes.len() <= 64 * 1024);
            self.bytes.extend_from_slice(bytes);
            self.chunks += 1;
            Ok(())
        }
    }
    async fn response(
        status: &str,
        extra: &str,
        declared: usize,
        bytes: &[u8],
    ) -> Result<Response> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let length = if extra.starts_with("Transfer-Encoding: chunked") {
            String::new()
        } else {
            format!("Content-Length: {declared}\r\n")
        };
        let reply = format!("HTTP/1.1 {status}\r\n{length}{extra}Connection: close\r\n\r\n");
        let bytes = bytes.to_vec();
        tokio::spawn(async move {
            let (mut peer, _) = listener.accept().await.expect("accept");
            let mut headers = [0; 4096];
            let _ = peer.read(&mut headers).await;
            peer.write_all(reply.as_bytes()).await.expect("headers");
            let _ = peer.write_all(&bytes).await;
        });
        Ok(Client::new()
            .get(format!("http://{address}/"))
            .send()
            .await?)
    }
    #[tokio::test]
    async fn complete_package_stages_actual_size_and_bounded_chunks() -> Result<()> {
        let bytes = vec![0x35; 131_091];
        let response = response("200 OK", "", bytes.len(), &bytes).await?;
        let mut sink = Sink::default();
        let artifact =
            stage_response(response, &mut sink, 200_000, &CancellationToken::new()).await?;
        assert_eq!(artifact.size, bytes.len() as u64);
        assert_eq!(artifact.sha256, hex::encode(Sha256::digest(&bytes)));
        assert_eq!(sink.bytes, bytes);
        assert!(sink.chunks >= 3);
        Ok(())
    }
    #[tokio::test]
    async fn package_stage_rejects_partial_encoded_truncated_and_over_budget_bodies() -> Result<()>
    {
        for (status, header, declared, bytes, limit) in [
            (
                "206 Partial Content",
                "Content-Range: bytes 0-2/3\r\n",
                3,
                b"abc".as_slice(),
                10,
            ),
            ("200 OK", "Content-Encoding: gzip\r\n", 3, b"abc", 10),
            ("200 OK", "", 5, b"abc", 10),
            ("200 OK", "", 3, b"abc", 2),
            ("200 OK", "", 0, b"", 10),
        ] {
            let response = response(status, header, declared, bytes).await?;
            assert!(
                stage_response(
                    response,
                    &mut Sink::default(),
                    limit,
                    &CancellationToken::new()
                )
                .await
                .is_err()
            );
        }
        Ok(())
    }
    #[tokio::test]
    async fn chunked_package_uses_received_size_and_enforces_budget_without_a_length() -> Result<()>
    {
        let mut sink = Sink::default();
        let reply = response(
            "200 OK",
            "Transfer-Encoding: chunked\r\n",
            0,
            b"3\r\nabc\r\n0\r\n\r\n",
        )
        .await?;
        let result = stage_response(reply, &mut sink, 3, &CancellationToken::new()).await?;
        assert_eq!(result.size, 3);
        assert_eq!(sink.bytes, b"abc");
        let reply = response(
            "200 OK",
            "Transfer-Encoding: chunked\r\n",
            0,
            b"3\r\nabc\r\n0\r\n\r\n",
        )
        .await?;
        assert!(
            stage_response(reply, &mut Sink::default(), 2, &CancellationToken::new())
                .await
                .is_err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn cancelled_package_staging_never_yields_a_receipt() -> Result<()> {
        let response = response("200 OK", "", 3, b"abc").await?;
        let cancel = CancellationToken::new();
        cancel.cancel();
        let mut sink = Sink::default();
        assert!(
            stage_response(response, &mut sink, 10, &cancel)
                .await
                .is_err()
        );
        assert!(sink.bytes.is_empty());
        Ok(())
    }
    #[tokio::test]
    async fn cancellation_during_the_final_sink_write_refuses_publication() -> Result<()> {
        struct CancelSink(CancellationToken);
        #[async_trait]
        impl ReadWindowSink for CancelSink {
            async fn write_chunk(&mut self, _: &[u8]) -> std::result::Result<(), ProviderError> {
                self.0.cancel();
                Ok(())
            }
        }
        let response = response("200 OK", "", 3, b"abc").await?;
        let cancel = CancellationToken::new();
        let result = stage_response(response, &mut CancelSink(cancel.clone()), 10, &cancel).await;
        assert!(
            result.is_err(),
            "cancelled final write must not publish a package receipt"
        );
        Ok(())
    }

    #[test]
    fn package_source_must_keep_identity_revision_parent_and_logical_size() -> Result<()> {
        let source: DriveEntry =
            serde_json::from_value(json!({"drivewsid":"FILE::zone::id", "docwsid":"id",
            "parentId":"FOLDER::zone::parent","etag":"v1","size":100,"type":"FILE"}))?;
        check_source(&source, &source)?;
        let mut changed = source.clone();
        changed.etag = "v2".into();
        assert!(check_source(&source, &changed).is_err());
        changed = source.clone();
        changed.size = 101;
        assert!(check_source(&source, &changed).is_err());
        changed = source.clone();
        changed.parent_id = "different".into();
        assert!(check_source(&source, &changed).is_err());
        changed = source.clone();
        changed.drivewsid = "different".into();
        assert!(check_source(&source, &changed).is_err());
        Ok(())
    }
}
