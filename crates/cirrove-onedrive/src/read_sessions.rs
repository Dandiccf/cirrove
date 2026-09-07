//! Conditional read-session prototype. Ordinary account construction stays
//! conservative until the complete provider acceptance matrix has passed.
mod probe;
use super::*;
use cirrove_core::reads::{ReadIdentity, ReadSession};
pub use probe::ReadSessionValidationReport;
use reqwest::header::{ETAG, HeaderValue};
use std::sync::{
    Mutex as StdMutex,
    atomic::{AtomicU64, Ordering},
};

pub(super) fn strong_etag(value: &HeaderValue) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 2
        && bytes.first() == Some(&b'"')
        && bytes.last() == Some(&b'"')
        && bytes[1..bytes.len() - 1]
            .iter()
            .all(|b| *b == 0x21 || (0x23..=0x7e).contains(b) || *b >= 0x80)
}

const SESSION_LEASE: Duration = Duration::from_secs(60);

#[derive(Default)]
pub(super) struct ReadCounters {
    pub graph_get_attempts: AtomicU64,
    pub content_get_attempts: AtomicU64,
    pub content_body_bytes: AtomicU64,
    setups: AtomicU64,
    renewals: AtomicU64,
    conditional_ranges: AtomicU64,
    fallback_ranges: AtomicU64,
}
#[derive(Debug, Serialize)]
pub struct ReadCountersSnapshot {
    pub graph_get_attempts: u64,
    pub content_get_attempts: u64,
    pub content_body_bytes: u64,
    pub setups: u64,
    pub renewals: u64,
    pub conditional_ranges: u64,
    pub fallback_ranges: u64,
}

#[derive(Clone)]
struct Bound {
    url: Url,
    effective_url: Url,
    tag: HeaderValue,
    expires: Instant,
}
#[derive(Clone, Default)]
enum State {
    #[default]
    Cold,
    Bound(Arc<Bound>),
    Fallback,
}
pub(super) struct GraphReadSession {
    graph: OneDrive,
    identity: ReadIdentity,
    node: Node,
    state: StdMutex<State>,
    setup: Mutex<()>,
    failure: StdMutex<Option<(ProviderError, Instant)>>,
}
impl GraphReadSession {
    pub(super) fn new(graph: OneDrive, scope: &Scope, node: &Node) -> Result<Self, ProviderError> {
        Ok(Self {
            graph,
            identity: ReadIdentity::new(scope, node)?,
            node: node.clone(),
            state: StdMutex::new(State::Cold),
            setup: Mutex::new(()),
            failure: StdMutex::new(None),
        })
    }
    fn state(&self) -> Result<State, ProviderError> {
        self.state
            .lock()
            .map(|state| state.clone())
            .map_err(|_| ProviderError::Unavailable)
    }
    fn check_failure(&self) -> Result<(), ProviderError> {
        let mut failure = self
            .failure
            .lock()
            .map_err(|_| ProviderError::Unavailable)?;
        if let Some((error, until)) = failure.as_ref() {
            if *until > Instant::now() {
                return Err(error.clone());
            }
            *failure = None;
        }
        Ok(())
    }
    async fn range(&self, offset: u64, length: u32) -> Result<Vec<u8>, ProviderError> {
        self.graph.check_cooldown().await?;
        self.check_failure()?;
        let mut rejected: Option<Arc<Bound>> = None;
        // At most one renewal/rebind after a transport rejects the old context.
        for _ in 0..2 {
            let state = self.state()?;
            match &state {
                State::Bound(bound)
                    if bound.expires > Instant::now()
                        && !rejected.as_ref().is_some_and(|old| Arc::ptr_eq(old, bound)) =>
                {
                    self.graph
                        .counters
                        .conditional_ranges
                        .fetch_add(1, Ordering::Relaxed);
                    match self
                        .graph
                        .download_range(&bound.url, &self.node, offset, length, Some(bound))
                        .await
                    {
                        Ok(range) if bound.expires > Instant::now() => return Ok(range.bytes),
                        Ok(_) | Err(DownloadError::Renew) => {
                            rejected = Some(bound.clone());
                        }
                        Err(DownloadError::Provider(error)) => return Err(error),
                    }
                }
                State::Fallback => {
                    self.graph
                        .counters
                        .fallback_ranges
                        .fetch_add(1, Ordering::Relaxed);
                    return self
                        .graph
                        .fetch_range(&self.identity.scope, &self.node, offset, length)
                        .await;
                }
                _ => (),
            }
            // This is a per-version setup gate, never a filesystem/map/DB lock.
            let _setup = self.setup.lock().await;
            self.check_failure()?;
            let current = self.state()?;
            if let State::Bound(bound) = &current
                && bound.expires > Instant::now()
                && !rejected.as_ref().is_some_and(|old| Arc::ptr_eq(old, bound))
            {
                // Another range has already established/renewed this session.
                continue;
            }
            if matches!(current, State::Fallback) {
                continue;
            }
            let start = Instant::now();
            let range = match self
                .graph
                .checked_read(&self.identity.scope, &self.node, offset, length)
                .await
            {
                Ok(range) => range,
                Err(error) => {
                    let delay = match &error {
                        ProviderError::Throttled(delay) => *delay,
                        _ => Duration::from_secs(2),
                    };
                    *self
                        .failure
                        .lock()
                        .map_err(|_| ProviderError::Unavailable)? =
                        Some((error.clone(), Instant::now() + delay));
                    return Err(error);
                }
            };
            if matches!(current, State::Cold) {
                self.graph.counters.setups.fetch_add(1, Ordering::Relaxed);
            } else {
                self.graph.counters.renewals.fetch_add(1, Ordering::Relaxed);
            }
            let next = match range.tag.filter(strong_etag) {
                Some(tag) => State::Bound(Arc::new(Bound {
                    url: range.url,
                    effective_url: range.effective_url,
                    tag,
                    expires: start + SESSION_LEASE,
                })),
                None => State::Fallback,
            };
            *self.state.lock().map_err(|_| ProviderError::Unavailable)? = next;
            return Ok(range.bytes);
        }
        Err(ProviderError::Unavailable)
    }
}

#[async_trait]
impl ReadSession for GraphReadSession {
    fn identity(&self) -> &ReadIdentity {
        &self.identity
    }
    async fn read_range(
        &self,
        offset: u64,
        length: u32,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>, ProviderError> {
        if offset >= self.node.size || length == 0 {
            return Ok(vec![]);
        }
        if length > 8 * 1024 * 1024 {
            return Err(ProviderError::Protocol("range exceeds memory limit"));
        }
        let _permit = self.graph.budget.acquire(Priority::Content, cancel).await?;
        tokio::select! { biased;
            _ = cancel.cancelled() => Err(ProviderError::Cancelled),
            result = tokio::time::timeout(Duration::from_secs(60), self.range(offset, length)) =>
                result.map_err(|_| ProviderError::Unavailable)?,
        }
    }
}

pub(super) struct DownloadedRange {
    pub bytes: Vec<u8>,
    url: Url,
    effective_url: Url,
    tag: Option<HeaderValue>,
}
enum DownloadError {
    Provider(ProviderError),
    Renew,
}
impl From<ProviderError> for DownloadError {
    fn from(error: ProviderError) -> Self {
        Self::Provider(error)
    }
}

impl OneDrive {
    /// Developer acceptance path, not selected by ordinary account construction.
    pub fn with_experimental_read_sessions(mut self) -> Self {
        self.conditional_reads = true;
        self
    }
    /// Adapter-level attempts; token-broker calls and automatic content redirects
    /// are not counted. Counters include other Graph GETs on this same adapter.
    pub fn read_counters(&self) -> ReadCountersSnapshot {
        let c = &self.counters;
        ReadCountersSnapshot {
            graph_get_attempts: c.graph_get_attempts.load(Ordering::Relaxed),
            content_get_attempts: c.content_get_attempts.load(Ordering::Relaxed),
            content_body_bytes: c.content_body_bytes.load(Ordering::Relaxed),
            setups: c.setups.load(Ordering::Relaxed),
            renewals: c.renewals.load(Ordering::Relaxed),
            conditional_ranges: c.conditional_ranges.load(Ordering::Relaxed),
            fallback_ranges: c.fallback_ranges.load(Ordering::Relaxed),
        }
    }
    pub(super) async fn checked_read(
        &self,
        scope: &Scope,
        node: &Node,
        offset: u64,
        length: u32,
    ) -> Result<DownloadedRange, ProviderError> {
        let url = self.resource_url(&["drives", &scope.collection, "items", &node.id])?;
        let before: DriveItem = serde_json::from_slice(&self.request_bytes(url.clone()).await?)
            .map_err(|_| ProviderError::Protocol("invalid file metadata"))?;
        if !before.matches_read(scope, node) {
            return Err(ProviderError::VersionChanged);
        }
        let download = self.download_url(&before)?;
        let range = self
            .download_range(&download, node, offset, length, None)
            .await
            .map_err(|error| match error {
                DownloadError::Provider(error) => error,
                DownloadError::Renew => ProviderError::Unavailable,
            })?;
        let after: DriveItem = serde_json::from_slice(&self.request_bytes(url).await?)
            .map_err(|_| ProviderError::Protocol("invalid file metadata"))?;
        if !after.matches_read(scope, node) {
            return Err(ProviderError::VersionChanged);
        }
        Ok(range)
    }
    async fn download_range(
        &self,
        url: &Url,
        node: &Node,
        offset: u64,
        length: u32,
        bound: Option<&Bound>,
    ) -> Result<DownloadedRange, DownloadError> {
        self.check_cooldown().await?;
        let count = (node.size - offset).min(length as u64);
        let mut request = self
            .downloads
            .get(url.clone())
            .header("Range", format!("bytes={offset}-{}", offset + count - 1))
            .header("Accept-Encoding", "identity");
        if let Some(bound) = bound {
            request = request.header("If-Match", bound.tag.clone());
        }
        self.counters
            .content_get_attempts
            .fetch_add(1, Ordering::Relaxed);
        let mut response = request
            .send()
            .await
            .map_err(|_| ProviderError::Unavailable)?;
        if matches!(
            response.status(),
            StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
        ) {
            return Err(self.throttle(&response).await?.into());
        }
        if bound.is_some()
            && matches!(
                response.status(),
                StatusCode::UNAUTHORIZED
                    | StatusCode::FORBIDDEN
                    | StatusCode::NOT_FOUND
                    | StatusCode::GONE
            )
        {
            return Err(DownloadError::Renew);
        }
        if response.status() == StatusCode::PRECONDITION_FAILED {
            return Err(if bound.is_some() {
                DownloadError::Renew
            } else {
                ProviderError::VersionChanged.into()
            });
        }
        let tag = response.headers().get(ETAG).cloned();
        if let Some(bound) = bound {
            // Entity tags are scoped to the effective resource, not globally.
            if response.url() != &bound.effective_url
                || tag.as_ref().is_none_or(|tag| !strong_etag(tag))
            {
                return Err(DownloadError::Renew);
            }
            if tag.as_ref() != Some(&bound.tag) {
                // The origin's validator may also change on a metadata-only
                // rename. Recheck Graph's original content identity before
                // accepting anything under a replacement transport context.
                return Err(DownloadError::Renew);
            }
        }
        if response
            .headers()
            .get("content-encoding")
            .is_some_and(|value| value != "identity")
        {
            return Err(ProviderError::Protocol("encoded download representation").into());
        }
        if response.status() == StatusCode::PARTIAL_CONTENT {
            let expected_range = format!("bytes {offset}-{}/{}", offset + count - 1, node.size);
            if response
                .headers()
                .get("content-range")
                .and_then(|v| v.to_str().ok())
                != Some(expected_range.as_str())
            {
                return Err(ProviderError::Protocol("download range mismatch").into());
            }
        } else if response.status() != StatusCode::OK || offset != 0 || count != node.size {
            return Err(ProviderError::Unavailable.into());
        }
        let effective_url = response.url().clone();
        let mut bytes = Vec::with_capacity(count as usize);
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| ProviderError::Unavailable)?
        {
            self.counters
                .content_body_bytes
                .fetch_add(chunk.len() as u64, Ordering::Relaxed);
            if chunk.len() > count as usize - bytes.len() {
                return Err(ProviderError::Protocol("download exceeded requested range").into());
            }
            bytes.extend_from_slice(&chunk);
        }
        if bytes.len() != count as usize {
            return Err(ProviderError::Unavailable.into());
        }
        Ok(DownloadedRange {
            bytes,
            url: url.clone(),
            effective_url,
            tag,
        })
    }
}

#[cfg(test)]
mod tests;
