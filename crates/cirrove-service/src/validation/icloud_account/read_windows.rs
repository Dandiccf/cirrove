//! Controlled cache-path comparison, confined to one newly created owned file.
use super::*;
use crate::content::{BLOCK_SIZE, ContentCache};
use cirrove_core::reads::{ReadIdentity, ReadSession, ReadWindowSink};
use cirrove_core::{Cursor, DirectoryPage, ProviderError, ReadProvider};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
const SIZE: usize = 64 * 1024 * 1024 + 17;
const BUDGET: u64 = 256 * 1024 * 1024;

fn payload() -> Vec<u8> {
    let mut bytes = vec![0; SIZE];
    for (i, chunk) in bytes.chunks_mut(8).enumerate() {
        let word = (i as u64).wrapping_mul(0x9e3779b97f4a7c15).rotate_left(23) ^ 0x18a344768976ccab;
        chunk.copy_from_slice(&word.to_le_bytes()[..chunk.len()]);
    }
    bytes
}
#[derive(Default)]
struct Counts {
    ranges: AtomicU64,
    windows: AtomicU64,
}
struct CountedSession {
    inner: Arc<dyn ReadSession>,
    counts: Arc<Counts>,
}
#[async_trait::async_trait]
impl ReadSession for CountedSession {
    fn identity(&self) -> &ReadIdentity {
        self.inner.identity()
    }
    fn window_limit(&self) -> u32 {
        self.inner.window_limit()
    }
    async fn read_range(
        &self,
        offset: u64,
        length: u32,
        cancel: &CancellationToken,
    ) -> std::result::Result<Vec<u8>, ProviderError> {
        self.counts.ranges.fetch_add(1, Ordering::Relaxed);
        self.inner.read_range(offset, length, cancel).await
    }
    async fn read_window(
        &self,
        offset: u64,
        length: u32,
        sink: &mut dyn ReadWindowSink,
        cancel: &CancellationToken,
    ) -> std::result::Result<(), ProviderError> {
        self.counts.windows.fetch_add(1, Ordering::Relaxed);
        self.inner.read_window(offset, length, sink, cancel).await
    }
}
struct View {
    inner: ICloudDrive,
    scope: Scope,
    node: Node,
    windows: bool,
    counts: Arc<Counts>,
}
impl View {
    fn check(&self, scope: &Scope, node: &Node) -> std::result::Result<(), ProviderError> {
        if scope != &self.scope || node != &self.node {
            return Err(ProviderError::Permission);
        }
        Ok(())
    }
}
#[async_trait::async_trait]
impl cirrove_core::MetadataProvider for View {
    fn provider_id(&self) -> &'static str {
        "icloud"
    }
    async fn changes(
        &self,
        _: &Scope,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<cirrove_core::ChangePage, ProviderError> {
        Err(ProviderError::Permission)
    }
}
#[async_trait::async_trait]
impl ReadProvider for View {
    async fn node(
        &self,
        _: &Scope,
        _: &str,
        _: &CancellationToken,
    ) -> std::result::Result<Node, ProviderError> {
        Err(ProviderError::Permission)
    }
    async fn children(
        &self,
        _: &Scope,
        _: &str,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<DirectoryPage, ProviderError> {
        Err(ProviderError::Permission)
    }
    async fn read_range(
        &self,
        scope: &Scope,
        node: &Node,
        offset: u64,
        length: u32,
        cancel: &CancellationToken,
    ) -> std::result::Result<Vec<u8>, ProviderError> {
        self.check(scope, node)?;
        self.counts.ranges.fetch_add(1, Ordering::Relaxed);
        self.inner
            .read_range(scope, node, offset, length, cancel)
            .await
    }
    async fn open_read_session(
        &self,
        scope: &Scope,
        node: &Node,
        cancel: &CancellationToken,
    ) -> std::result::Result<Option<Arc<dyn ReadSession>>, ProviderError> {
        self.check(scope, node)?;
        if !self.windows {
            return Ok(None);
        }
        let inner = self
            .inner
            .open_read_session(scope, node, cancel)
            .await?
            .ok_or(ProviderError::Unavailable)?;
        Ok(Some(Arc::new(CountedSession {
            inner,
            counts: self.counts.clone(),
        })))
    }
}
async fn arm(
    f: &Fixture,
    node: &Node,
    bytes: &[u8],
    number: usize,
    windows: bool,
) -> Result<serde_json::Value> {
    let dir = f.run_dir.join(format!("arm-{number}"));
    std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
    let cache = ContentCache::new(dir.join("cache"), dir.join("blocks.db"), BUDGET)?;
    let counts = Arc::new(Counts::default());
    let view = View {
        inner: ICloudDrive::on_demand_from_session_snapshot(
            f.scope.clone(),
            &f.account.identity.username,
            &f.snapshot,
        )?,
        scope: f.scope.clone(),
        node: node.clone(),
        windows,
        counts: counts.clone(),
    };
    let started = Instant::now();
    let mut first = None;
    let mut digest = Sha256::new();
    let cancel = CancellationToken::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let read = cache
            .read(&view, &f.scope, node, offset as u64, 65536, &cancel)
            .await?;
        if first.is_none() {
            first = Some(started.elapsed().as_secs_f64());
        }
        ensure!(
            !read.is_empty() && bytes.get(offset..offset + read.len()) == Some(read.as_slice()),
            "owned read differs from positions in the fixture"
        );
        offset += read.len();
        digest.update(&read);
    }
    let elapsed = started.elapsed().as_secs_f64();
    let ranges = counts.ranges.load(Ordering::Relaxed);
    let streamed = counts.windows.load(Ordering::Relaxed);
    let stats = cache.window_stats();
    ensure!(
        offset == bytes.len()
            && hex::encode(digest.finalize()) == hex::encode(Sha256::digest(bytes)),
        "owned read digest mismatch"
    );
    if windows {
        ensure!(
            streamed > 0 && stats.validated_windows > 0,
            "window arm did not exercise windows"
        );
    } else {
        ensure!(
            streamed == 0 && ranges == (SIZE as u64).div_ceil(BLOCK_SIZE as u64),
            "baseline did not exercise exact ranges"
        );
    }
    // Sparse cached rereads must not cause additional provider calls.
    for offset in [0, 17 * 1024 * 1024, SIZE - 17] {
        let actual = cache
            .read(&view, &f.scope, node, offset as u64, 17, &cancel)
            .await?;
        ensure!(
            actual == bytes[offset..offset + 17],
            "cached reread differs"
        );
    }
    ensure!(
        counts.ranges.load(Ordering::Relaxed) == ranges
            && counts.windows.load(Ordering::Relaxed) == streamed,
        "cached reread fetched cloud bytes"
    );
    Ok(
        serde_json::json!({"arm":number,"windows":windows,"bytes":offset,"seconds":elapsed,"first_64k_seconds":first,"exact_range_calls":ranges,"window_calls":streamed,"validated_windows":stats.validated_windows,"staged_bytes":stats.staged_bytes,"staging_peak_bytes":stats.staging_peak_bytes,"digest_verified":true,"cached_sparse_rereads":true}),
    )
}
pub async fn icloud_account_read_windows(run: Uuid) -> Result<()> {
    let f = prepare_with_budget(run, "read-windows", BUDGET).await?;
    let bytes = payload();
    let node = transfer(
        &f.state,
        &f.account,
        &f.scope,
        &f.snapshot,
        &f.parent,
        None,
        &bytes,
    )
    .await?;
    record(&f.run_dir.join("source.json"), &node)?;
    verify(&f.snapshot, &f.account, &f.parent, &node, &bytes).await?;
    println!("Owned varied-content source independently verified; starting fresh-cache arms");
    let mut results = Vec::new();
    for (number, windows) in [false, true, true, false, false, true]
        .into_iter()
        .enumerate()
    {
        let result = tokio::time::timeout(
            Duration::from_secs(600),
            arm(&f, &node, &bytes, number, windows),
        )
        .await??;
        record(&f.run_dir.join(format!("arm-{number}.json")), &result)?;
        println!("{}", serde_json::to_string(&result)?);
        results.push(result);
    }
    record(
        &f.run_dir.join("passed.json"),
        &serde_json::json!({"run":run,"size":SIZE,"arms":results,"mounted":false,"source_independent_digest":true}),
    )?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixture_changes_across_reorderable_blocks_and_has_a_partial_tail() {
        let bytes = payload();
        assert_eq!(bytes.len(), SIZE);
        assert_eq!(SIZE % BLOCK_SIZE as usize, 17);
        let hashes: std::collections::HashSet<_> =
            bytes.chunks(65536).map(Sha256::digest).collect();
        assert_eq!(hashes.len(), bytes.len().div_ceil(65536));
    }
}
