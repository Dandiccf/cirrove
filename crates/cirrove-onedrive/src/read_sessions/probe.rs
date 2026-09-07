//! Explicit GET-only exercise of the experimental session against one chosen file.
use super::*;

#[derive(Debug, Serialize)]
pub struct ReadSessionValidationReport {
    pub sample_bytes: u32,
    pub samples: usize,
    pub all_samples_match: bool,
    pub first_range_ms: u64,
    pub concurrent_ranges_ms: u64,
    pub conservative_comparison_ms: u64,
    /// Cumulative snapshots; subtract adjacent phases for that phase's cost.
    pub before: ReadCountersSnapshot,
    pub after_first: ReadCountersSnapshot,
    pub after_concurrent: ReadCountersSnapshot,
    pub after_comparison: ReadCountersSnapshot,
}
impl OneDrive {
    /// Reads five bounded samples via one experimental session, then compares
    /// each to an independent conservative read. No file content is serialized.
    /// This does not enable sessions on this adapter's ordinary read path.
    pub async fn inspect_read_session(
        &self,
        scope: &Scope,
        node: &Node,
        cancel: &CancellationToken,
    ) -> Result<ReadSessionValidationReport, ProviderError> {
        if scope.account != self.account || scope.provider != "onedrive" {
            return Err(ProviderError::Protocol("provider/account mismatch"));
        }
        if node.size == 0 {
            return Err(ProviderError::Protocol(
                "read validation requires a nonempty file",
            ));
        }
        tokio::select! { biased;
            _ = cancel.cancelled() => Err(ProviderError::Cancelled),
            result = tokio::time::timeout(Duration::from_secs(60), self.session_samples(scope,node,cancel)) =>
                result.map_err(|_|ProviderError::Unavailable)?,
        }
    }
    async fn session_samples(
        &self,
        scope: &Scope,
        node: &Node,
        cancel: &CancellationToken,
    ) -> Result<ReadSessionValidationReport, ProviderError> {
        let session = GraphReadSession::new(self.clone(), scope, node)?;
        let length = node.size.min(256 * 1024) as u32;
        let before = self.read_counters();
        let start = Instant::now();
        let first = session.read_range(0, length, cancel).await?;
        let first_range_ms = start.elapsed().as_millis() as u64;
        let after_first = self.read_counters();
        let offsets = [
            0,
            (node.size - length as u64) / 2,
            node.size - length as u64,
            0,
        ];
        let start = Instant::now();
        let (a, b, c, d) = tokio::try_join!(
            session.read_range(offsets[0], length, cancel),
            session.read_range(offsets[1], length, cancel),
            session.read_range(offsets[2], length, cancel),
            session.read_range(offsets[3], length, cancel),
        )?;
        let concurrent_ranges_ms = start.elapsed().as_millis() as u64;
        let after_concurrent = self.read_counters();
        let start = Instant::now();
        let mut all_samples_match = true;
        for (offset, bytes) in [
            (0, first),
            (offsets[0], a),
            (offsets[1], b),
            (offsets[2], c),
            (offsets[3], d),
        ] {
            all_samples_match &=
                self.read_range(scope, node, offset, length, cancel).await? == bytes;
        }
        Ok(ReadSessionValidationReport {
            sample_bytes: length,
            samples: 5,
            all_samples_match,
            first_range_ms,
            concurrent_ranges_ms,
            conservative_comparison_ms: start.elapsed().as_millis() as u64,
            before,
            after_first,
            after_concurrent,
            after_comparison: self.read_counters(),
        })
    }
}
