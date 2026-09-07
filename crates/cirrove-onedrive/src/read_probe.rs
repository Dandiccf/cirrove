//! Explicit, bounded observation of content-origin validators. This does not
//! enable a read fast path or prove behavior across later edits and URL renewal.
use super::read_sessions::strong_etag;
use super::*;
use reqwest::header::{ETAG, HeaderValue, IF_MATCH, IF_RANGE};
use sha2::{Digest, Sha256};

#[derive(Debug, Serialize)]
pub struct ReadValidationReport {
    pub schema_version: u32,
    pub sample_bytes: u64,
    /// Logical operations, excluding redirects and authentication retries.
    pub metadata_operations: u32,
    pub content_operations: usize,
    pub metadata_unchanged: bool,
    pub observations: Vec<Observation>,
}

#[derive(Debug, Serialize)]
pub struct Observation {
    pub condition: &'static str,
    pub status: u16,
    pub redirected: bool,
    pub strong_etag: bool,
    pub etag_matches_initial: Option<bool>,
    pub exact_range: bool,
    pub identity_encoding: bool,
    pub body_complete: bool,
    pub body_matches_initial: Option<bool>,
    pub retained_body_bytes: usize,
    pub headers_ms: u64,
    pub complete_ms: u64,
}

// Transport credentials and byte fingerprints never enter the report or Debug.
struct Sample {
    observation: Observation,
    etag: Option<HeaderValue>,
    digest: Option<[u8; 32]>,
}

impl OneDrive {
    /// Observe GET/If-Match/If-Range on at most 4 KiB of one explicitly selected
    /// file. All requests are reads; bytes, tags and URLs stay private in memory.
    /// Positive observations do not constitute permission to enable a fast path.
    pub async fn inspect_read_validation(
        &self,
        scope: &Scope,
        node: &Node,
        cancel: &CancellationToken,
    ) -> Result<ReadValidationReport, ProviderError> {
        if scope.account != self.account || scope.provider != "onedrive" {
            return Err(ProviderError::Protocol("provider/account mismatch"));
        }
        if node.kind != NodeKind::File || node.size < 2 || node.content_revision().is_none() {
            return Err(ProviderError::Protocol(
                "read probe requires a versioned file of at least two bytes",
            ));
        }
        let _permit = self.budget.acquire(Priority::Content, cancel).await?;
        tokio::select! { biased;
            _ = cancel.cancelled() => Err(ProviderError::Cancelled),
            result = tokio::time::timeout(Duration::from_secs(60), self.probe_validation(scope, node)) =>
                result.map_err(|_| ProviderError::Unavailable)?,
        }
    }

    async fn probe_validation(
        &self,
        scope: &Scope,
        node: &Node,
    ) -> Result<ReadValidationReport, ProviderError> {
        let metadata_url = self.resource_url(&["drives", &scope.collection, "items", &node.id])?;
        let before: DriveItem =
            serde_json::from_slice(&self.request_bytes(metadata_url.clone()).await?)
                .map_err(|_| ProviderError::Protocol("invalid file metadata"))?;
        if !before.matches_read(scope, node) {
            return Err(ProviderError::VersionChanged);
        }
        let url = self.download_url(&before)?;
        // Always request a partial representation, even for a small file.
        let count = (node.size - 1).min(4096);
        let first = self
            .probe_sample(&url, count, node.size, "range", None)
            .await?;
        let wrong = if first
            .etag
            .as_ref()
            .is_some_and(|v| v == "\"cirrove-probe-mismatch\"")
        {
            HeaderValue::from_static("\"cirrove-probe-mismatch-other\"")
        } else {
            HeaderValue::from_static("\"cirrove-probe-mismatch\"")
        };
        let mut samples = vec![];
        for (name, header) in [
            ("if_match_mismatch", IF_MATCH),
            ("if_range_mismatch", IF_RANGE),
        ] {
            samples.push(
                self.probe_sample(&url, count, node.size, name, Some((header, wrong.clone())))
                    .await?,
            );
        }
        if let Some(tag) = first.etag.as_ref().filter(|tag| strong_etag(tag)) {
            for (name, header) in [("if_match_same", IF_MATCH), ("if_range_same", IF_RANGE)] {
                samples.push(
                    self.probe_sample(&url, count, node.size, name, Some((header, tag.clone())))
                        .await?,
                );
            }
        }
        for sample in &mut samples {
            sample.observation.etag_matches_initial = first
                .etag
                .as_ref()
                .map(|tag| sample.etag.as_ref() == Some(tag));
            sample.observation.body_matches_initial = first
                .digest
                .zip(sample.digest)
                .map(|(initial, current)| initial == current);
        }
        let after: DriveItem = serde_json::from_slice(&self.request_bytes(metadata_url).await?)
            .map_err(|_| ProviderError::Protocol("invalid file metadata"))?;
        let mut observations = vec![first.observation];
        observations.extend(samples.into_iter().map(|sample| sample.observation));
        Ok(ReadValidationReport {
            schema_version: 1,
            sample_bytes: count,
            metadata_operations: 2,
            content_operations: observations.len(),
            metadata_unchanged: after.matches_read(scope, node),
            observations,
        })
    }

    async fn probe_sample(
        &self,
        url: &Url,
        count: u64,
        size: u64,
        condition: &'static str,
        header: Option<(reqwest::header::HeaderName, HeaderValue)>,
    ) -> Result<Sample, ProviderError> {
        let start = Instant::now();
        let mut request = self
            .downloads
            .get(url.clone())
            .header("Range", format!("bytes=0-{}", count - 1))
            .header("Accept-Encoding", "identity");
        if let Some((name, value)) = header {
            request = request.header(name, value);
        }
        self.counters
            .content_get_attempts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut response = request
            .send()
            .await
            .map_err(|_| ProviderError::Unavailable)?;
        if matches!(
            response.status(),
            StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
        ) {
            return Err(self.throttle(&response).await?);
        }
        let etag = response.headers().get(ETAG).cloned();
        let expected_range = format!("bytes 0-{}/{size}", count - 1);
        let identity_encoding = response
            .headers()
            .get("content-encoding")
            .is_none_or(|value| value == "identity");
        let exact_range = response.status() == StatusCode::PARTIAL_CONTENT
            && response
                .headers()
                .get("content-range")
                .and_then(|v| v.to_str().ok())
                == Some(expected_range.as_str());
        let mut observation = Observation {
            condition,
            status: response.status().as_u16(),
            redirected: response.url() != url,
            strong_etag: etag.as_ref().is_some_and(strong_etag),
            etag_matches_initial: None,
            exact_range,
            identity_encoding,
            body_complete: false,
            body_matches_initial: None,
            retained_body_bytes: 0,
            headers_ms: start.elapsed().as_millis() as u64,
            complete_ms: 0,
        };
        let mut bytes = Vec::new();
        let mut oversized = false;
        // Never consume a full-file 200 or an error body. An oversized chunk is
        // dropped immediately, rather than copied into the sample buffer.
        if exact_range && identity_encoding {
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| ProviderError::Unavailable)?
            {
                self.counters
                    .content_body_bytes
                    .fetch_add(chunk.len() as u64, std::sync::atomic::Ordering::Relaxed);
                if chunk.len() > count as usize - bytes.len() {
                    oversized = true;
                    break;
                }
                bytes.extend_from_slice(&chunk);
                observation.retained_body_bytes = bytes.len();
            }
            observation.body_complete = !oversized && bytes.len() == count as usize;
        }
        observation.complete_ms = start.elapsed().as_millis() as u64;
        Ok(Sample {
            digest: observation
                .body_complete
                .then(|| Sha256::digest(&bytes).into()),
            observation,
            etag,
        })
    }
}

#[cfg(test)]
mod tests;
