//! Stream larger windows into untrusted staging, retaining both revision checks.
use super::*;
use cirrove_core::reads::{ReadIdentity, ReadSession, ReadWindowSink};
const WINDOW_LIMIT: u32 = 64 * 1024 * 1024;

pub(super) fn validate_node(node: &Node) -> Result<(), ProviderError> {
    if node.kind != NodeKind::File
        || node.package
        || node.target.is_some()
        || node
            .content_version
            .as_deref()
            .is_some_and(|version| Some(version) != node.etag.as_deref())
        || !node.id.starts_with("FILE::")
        || !node
            .parent_id
            .as_deref()
            .is_some_and(|p| p.starts_with("FOLDER::"))
        || !node.etag.as_deref().is_some_and(|tag| !tag.is_empty())
    {
        return Err(ProviderError::Protocol(
            "invalid iCloud ordinary read identity",
        ));
    }
    Ok(())
}
pub(super) struct OrdinarySession {
    identity: ReadIdentity,
    node: Node,
    transport: ICloudReadSession,
    reads: Arc<Semaphore>,
}
impl OrdinarySession {
    pub(super) fn new(
        scope: &Scope,
        node: &Node,
        transport: ICloudReadSession,
        reads: Arc<Semaphore>,
    ) -> Result<Self, ProviderError> {
        validate_node(node)?;
        Ok(Self {
            identity: ReadIdentity::new(scope, node)?,
            node: node.clone(),
            transport,
            reads,
        })
    }
    fn parent(&self) -> &str {
        self.node.parent_id.as_deref().unwrap_or_default()
    }
    fn expected(&self) -> Option<(&str, u64)> {
        self.node.etag.as_deref().map(|tag| (tag, self.node.size))
    }

    async fn finish_window(
        &self,
        transport: &mut ICloudReadSession,
        mut response: reqwest::Response,
        offset: u64,
        end: u64,
        sink: &mut dyn ReadWindowSink,
    ) -> Result<(), ProviderError> {
        let expected = crate::checked_range_response_length(&response, offset, end, self.node.size)
            .map_err(|error| map_read_error(&error))? as u64;
        let mut received = 0u64;
        while let Some(bytes) = response
            .chunk()
            .await
            .map_err(|_| ProviderError::Unavailable)?
        {
            if bytes.len() as u64 > expected.saturating_sub(received) {
                return Err(ProviderError::Protocol("long iCloud read window"));
            }
            for chunk in bytes.chunks(64 * 1024) {
                sink.write_chunk(chunk).await?;
            }
            received += bytes.len() as u64;
        }
        if received != expected {
            return Err(ProviderError::Protocol("short iCloud read window"));
        }
        let after = transport
            .item_for_read(&self.node.id, Some(self.parent()))
            .await
            .map_err(|error| map_read_error(&error))?;
        crate::check_expected_revision(&after, self.expected())
            .map_err(|error| map_read_error(&error))?;
        // Only this success allows the caller to publish its staged bytes.
        Ok(())
    }
}
#[async_trait]
impl ReadSession for OrdinarySession {
    fn identity(&self) -> &ReadIdentity {
        &self.identity
    }
    fn window_limit(&self) -> u32 {
        WINDOW_LIMIT
    }
    async fn read_window(
        &self,
        offset: u64,
        length: u32,
        sink: &mut dyn ReadWindowSink,
        cancel: &CancellationToken,
    ) -> Result<(), ProviderError> {
        if length > WINDOW_LIMIT {
            return Err(ProviderError::Protocol("iCloud read window exceeds limit"));
        }
        tokio::select! { biased;
            _ = cancel.cancelled() => Err(ProviderError::Cancelled),
            result = async {
                let _permit = self.reads.acquire().await.map_err(|_| ProviderError::Unavailable)?;
                let mut transport = self.transport.read_only_fork();
                let before = transport.item_for_read(&self.node.id, Some(self.parent())).await.map_err(|error| map_read_error(&error))?;
                crate::check_expected_revision(&before,self.expected()).map_err(|error| map_read_error(&error))?;
                if length == 0 || offset >= self.node.size { return Ok(()); }
                let count = u64::from(length).min(self.node.size-offset);
                let end = offset+count-1;
                // Resolve afresh for every validated window. A signed URL is
                // transport, never a substitute for the metadata revision.
                let url = transport.ordinary_download_url(&self.node.id).await.map_err(|error| map_read_error(&error))?;
                let response = transport.http.get(url)
                    .timeout(Duration::from_secs(30))
                    .header(reqwest::header::ACCEPT_ENCODING,"identity")
                    .header(reqwest::header::RANGE,format!("bytes={offset}-{end}"))
                    .send().await.map_err(|_| ProviderError::Unavailable)?;
                self.finish_window(&mut transport,response,offset,end,sink).await
            } => result,
        }
    }
    async fn read_range(
        &self,
        offset: u64,
        length: u32,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>, ProviderError> {
        if length == 0 {
            return Ok(Vec::new());
        }
        if length > MAX_RANGE {
            return Err(ProviderError::Protocol("iCloud range exceeds limit"));
        }
        tokio::select! { biased;
            _ = cancel.cancelled() => Err(ProviderError::Cancelled),
            result = async {
                let _permit = self.reads.acquire().await.map_err(|_| ProviderError::Unavailable)?;
                let mut transport = self.transport.read_only_fork();
                transport.read_range_in_folder_for_revision(self.parent(), &self.node.id, offset, length, self.expected()).await.map_err(|error| map_read_error(&error))
            } => result,
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod deadline_tests;
