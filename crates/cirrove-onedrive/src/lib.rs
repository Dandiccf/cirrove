//! Read-only Microsoft Graph adapter. Authentication is injected; tokens, cursor
//! URLs and remote error bodies must never appear in logs.
use async_trait::async_trait;
use cirrove_core::{
    CancellationToken, Change, ChangePage, Checkpoint, Cursor, DirectoryPage, MetadataProvider,
    Node, NodeKind, Priority, ProviderError, ReadProvider, RemoteRef, RequestBudget, Scope,
};
use reqwest::{Client, StatusCode, Url, redirect::Policy};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::{sync::Mutex, time::Instant};

const MAX_PAGE_BYTES: usize = 8 * 1024 * 1024;

#[async_trait]
pub trait TokenSource: Send + Sync {
    /// Implementations should serialize refresh and return a usable access token.
    async fn access_token(&self) -> Result<SecretString, ProviderError>;
    async fn invalidate(&self, _rejected: &SecretString) {}
}

/// Developer bootstrap only. The browser broker implements TokenSource separately.
pub struct StaticToken(pub SecretString);
#[async_trait]
impl TokenSource for StaticToken {
    async fn access_token(&self) -> Result<SecretString, ProviderError> {
        Ok(self.0.clone())
    }
}

pub struct OneDrive {
    account: String,
    endpoint: Url,
    client: Client,
    downloads: Client,
    tokens: Arc<dyn TokenSource>,
    budget: RequestBudget,
    cooldown: Mutex<Option<Instant>>,
}
impl OneDrive {
    pub fn new(account: String, tokens: Arc<dyn TokenSource>) -> Result<Self, ProviderError> {
        let endpoint = Url::parse("https://graph.microsoft.com/v1.0/")
            .map_err(|_| ProviderError::Protocol("Graph endpoint"))?;
        Self::build(account, tokens, endpoint)
    }
    fn build(
        account: String,
        tokens: Arc<dyn TokenSource>,
        endpoint: Url,
    ) -> Result<Self, ProviderError> {
        let client = Client::builder()
            .redirect(Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("Cirrove/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| ProviderError::Unavailable)?;
        Ok(Self {
            account,
            endpoint: endpoint.clone(),
            downloads: Client::builder()
                .https_only(endpoint.scheme() == "https")
                .redirect(Policy::limited(5))
                .retry(reqwest::retry::never())
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(40))
                .build()
                .map_err(|_| ProviderError::Unavailable)?,
            client,
            tokens,
            budget: RequestBudget::default(),
            cooldown: Mutex::new(None),
        })
    }
    async fn fetch_range(
        &self,
        scope: &Scope,
        node: &Node,
        offset: u64,
        length: u32,
    ) -> Result<Vec<u8>, ProviderError> {
        let expected = node
            .content_revision()
            .ok_or(ProviderError::Protocol("file has no version tag"))?;
        let count = (node.size - offset).min(length as u64);
        let url = self.resource_url(&["drives", &scope.collection, "items", &node.id])?;
        let before: DriveItem = serde_json::from_slice(&self.request_bytes(url.clone()).await?)
            .map_err(|_| ProviderError::Protocol("invalid file metadata"))?;
        if !before.matches_content(expected, node.size) {
            return Err(ProviderError::VersionChanged);
        }
        let download = Url::parse(
            before
                .download_url
                .as_deref()
                .ok_or(ProviderError::Protocol("file is not downloadable"))?,
        )
        .map_err(|_| ProviderError::Protocol("invalid download URL"))?;
        if (download.scheme() != "https" && self.endpoint.scheme() == "https")
            || !download.username().is_empty()
            || download.password().is_some()
        {
            return Err(ProviderError::Protocol("unsafe download URL"));
        }
        // This client has no Graph bearer token or default authorization headers.
        let mut response = self
            .downloads
            .get(download)
            .header("Range", format!("bytes={offset}-{}", offset + count - 1))
            .header("Accept-Encoding", "identity")
            .send()
            .await
            .map_err(|_| ProviderError::Unavailable)?;
        if matches!(
            response.status(),
            StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
        ) {
            return Err(self.throttle(&response).await?);
        }
        if response.status() == StatusCode::PARTIAL_CONTENT {
            let expected_range = format!("bytes {offset}-{}/{}", offset + count - 1, node.size);
            if response
                .headers()
                .get("content-range")
                .and_then(|v| v.to_str().ok())
                != Some(expected_range.as_str())
            {
                return Err(ProviderError::Protocol("download range mismatch"));
            }
        } else if response.status() != StatusCode::OK || offset != 0 || count != node.size {
            return Err(ProviderError::Unavailable);
        }
        let mut bytes = Vec::with_capacity(count as usize);
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| ProviderError::Unavailable)?
        {
            if chunk.len() > count as usize - bytes.len() {
                return Err(ProviderError::Protocol("download exceeded requested range"));
            }
            bytes.extend_from_slice(&chunk);
        }
        if bytes.len() != count as usize {
            return Err(ProviderError::Unavailable);
        }
        // A signed URL may identify the current content, not an immutable version.
        // Verify again before publishing a block; never combine changed versions.
        let after: DriveItem = serde_json::from_slice(&self.request_bytes(url).await?)
            .map_err(|_| ProviderError::Protocol("invalid file metadata"))?;
        if !after.matches_content(expected, node.size) {
            return Err(ProviderError::VersionChanged);
        }
        Ok(bytes)
    }
    fn initial_url(&self, drive: &str) -> Result<Url, ProviderError> {
        if drive.is_empty() || drive == "." || drive == ".." {
            return Err(ProviderError::Protocol("invalid drive ID"));
        }
        let mut url = self.endpoint.clone();
        url.path_segments_mut()
            .map_err(|_| ProviderError::Protocol("Graph endpoint"))?
            .pop_if_empty()
            .extend(["drives", drive, "root", "delta"]);
        Ok(url)
    }
    /// Continuation URLs are untrusted provider input. Bearer tokens never follow
    /// redirects, cross origins, or switch to a different drive's feed.
    fn validate_cursor(&self, drive: &str, cursor: &str) -> Result<Url, ProviderError> {
        let url =
            Url::parse(cursor).map_err(|_| ProviderError::Protocol("invalid continuation URL"))?;
        let initial = self.initial_url(drive)?;
        let prefix = initial
            .path()
            .strip_suffix("root/delta")
            .ok_or(ProviderError::Protocol("invalid feed path"))?;
        if url.origin() != self.endpoint.origin()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || !url.path().starts_with(prefix)
        {
            return Err(ProviderError::Protocol(
                "continuation outside configured drive",
            ));
        }
        Ok(url)
    }
    async fn authorized_request(
        &self,
        url: Url,
    ) -> Result<(reqwest::Response, SecretString), ProviderError> {
        let token = self.tokens.access_token().await?;
        let response = self
            .client
            .get(url)
            .bearer_auth(token.expose_secret())
            .header("Prefer", "Include-Feature=AddToOneDrive")
            .send()
            .await
            .map_err(|_| ProviderError::Unavailable)?;
        Ok((response, token))
    }
    async fn request_bytes(&self, url: Url) -> Result<Vec<u8>, ProviderError> {
        if let Some(deadline) = *self.cooldown.lock().await {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if !remaining.is_zero() {
                return Err(ProviderError::Throttled(remaining));
            }
        }
        let (mut response, rejected) = self.authorized_request(url.clone()).await?;
        if response.status() == StatusCode::UNAUTHORIZED {
            self.tokens.invalidate(&rejected).await;
            response = self.authorized_request(url).await?.0;
        }
        let status = response.status();
        if status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::SERVICE_UNAVAILABLE {
            return Err(self.throttle(&response).await?);
        }
        match status {
            StatusCode::OK => (),
            StatusCode::UNAUTHORIZED => return Err(ProviderError::Authentication),
            StatusCode::FORBIDDEN => return Err(ProviderError::Permission),
            StatusCode::NOT_FOUND => return Err(ProviderError::NotFound),
            StatusCode::GONE => return Err(ProviderError::CursorExpired),
            s if s.is_server_error() => return Err(ProviderError::Unavailable),
            _ => return Err(ProviderError::Protocol("unexpected HTTP status")),
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| ProviderError::Unavailable)?
        {
            if chunk.len() > MAX_PAGE_BYTES.saturating_sub(body.len()) {
                return Err(ProviderError::Protocol("metadata page exceeds limit"));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }
    async fn throttle(&self, response: &reqwest::Response) -> Result<ProviderError, ProviderError> {
        let delay = retry_after(
            response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok()),
            SystemTime::now(),
        );
        let deadline = Instant::now()
            .checked_add(delay)
            .ok_or(ProviderError::Protocol("retry delay overflow"))?;
        let mut cooldown = self.cooldown.lock().await;
        *cooldown = Some(cooldown.map_or(deadline, |old| old.max(deadline)));
        Ok(ProviderError::Throttled(delay))
    }
    fn resource_url(&self, segments: &[&str]) -> Result<Url, ProviderError> {
        if segments
            .iter()
            .any(|s| s.is_empty() || *s == "." || *s == "..")
        {
            return Err(ProviderError::Protocol("invalid resource ID"));
        }
        let mut url = self.endpoint.clone();
        url.path_segments_mut()
            .map_err(|_| ProviderError::Protocol("Graph endpoint"))?
            .pop_if_empty()
            .extend(segments);
        Ok(url)
    }
    pub async fn resource<T: serde::de::DeserializeOwned>(
        &self,
        segments: &[&str],
        cancel: &CancellationToken,
    ) -> Result<T, ProviderError> {
        let url = self.resource_url(segments)?;
        let _permit = self.budget.acquire(Priority::Interactive, cancel).await?;
        tokio::select! {biased;
            _=cancel.cancelled()=>Err(ProviderError::Cancelled),
            result=tokio::time::timeout(Duration::from_secs(45),self.request_bytes(url))=>{
                let body=result.map_err(|_|ProviderError::Unavailable)??;
                serde_json::from_slice(&body).map_err(|_|ProviderError::Protocol("invalid Graph resource"))
            }
        }
    }
    /// Present the default Documents drive first; never silently pick a cache library.
    pub async fn drives(
        &self,
        cancel: &CancellationToken,
    ) -> Result<Vec<DriveInfo>, ProviderError> {
        let home: DriveInfo = self.resource(&["me", "drive"], cancel).await?;
        let mut result = vec![home];
        let mut url = self.resource_url(&["me", "drives"])?;
        let mut seen = std::collections::HashSet::new();
        loop {
            if !seen.insert(url.as_str().to_owned()) || seen.len() > 100 {
                return Err(ProviderError::Protocol(
                    "drive pagination exceeded limit or repeated a cursor",
                ));
            }
            let _permit = self.budget.acquire(Priority::Interactive, cancel).await?;
            let bytes = tokio::select! {biased; _=cancel.cancelled()=>return Err(ProviderError::Cancelled),r=tokio::time::timeout(Duration::from_secs(45),self.request_bytes(url))=>r.map_err(|_|ProviderError::Unavailable)??};
            let page: DriveList = serde_json::from_slice(&bytes)
                .map_err(|_| ProviderError::Protocol("invalid drive list"))?;
            for drive in page.value {
                if !result.iter().any(|v| v.id == drive.id) {
                    result.push(drive);
                }
            }
            let Some(next) = page.next else { break };
            url = Url::parse(&next)
                .map_err(|_| ProviderError::Protocol("invalid drive list continuation"))?;
            if url.origin() != self.endpoint.origin()
                || url.path() != self.resource_url(&["me", "drives"])?.path()
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return Err(ProviderError::Protocol("invalid drive list continuation"));
            }
            if result.len() > 1000 {
                return Err(ProviderError::Protocol("drive list exceeds limit"));
            }
        }
        Ok(result)
    }
    pub async fn drive(
        &self,
        id: &str,
        cancel: &CancellationToken,
    ) -> Result<DriveInfo, ProviderError> {
        self.resource(&["drives", id], cancel).await
    }
    pub async fn root(
        &self,
        drive: &str,
        cancel: &CancellationToken,
    ) -> Result<Node, ProviderError> {
        let item: DriveItem = self.resource(&["drives", drive, "root"], cancel).await?;
        match map_item(item)? {
            Change::Upsert(node) => Ok(node),
            _ => Err(ProviderError::NotFound),
        }
    }
    async fn fetch(
        &self,
        scope: &Scope,
        cursor: Option<&Cursor>,
    ) -> Result<ChangePage, ProviderError> {
        if scope.provider != "onedrive" || scope.account != self.account {
            return Err(ProviderError::Protocol("provider/account mismatch"));
        }
        let url = match cursor {
            Some(c) => self.validate_cursor(&scope.collection, &c.0)?,
            None => self.initial_url(&scope.collection)?,
        };
        let body = self.request_bytes(url).await?;
        let wire: DeltaResponse = serde_json::from_slice(&body)
            .map_err(|_| ProviderError::Protocol("invalid Graph metadata"))?;
        let checkpoint = match (wire.next, wire.delta) {
            (Some(next), None) => Checkpoint::Continue(Cursor(next)),
            (None, Some(delta)) => Checkpoint::Complete(Cursor(delta)),
            _ => {
                return Err(ProviderError::Protocol(
                    "expected exactly one continuation or delta link",
                ));
            }
        };
        self.validate_cursor(&scope.collection, &checkpoint.cursor().0)?;
        let changes = wire
            .value
            .into_iter()
            .map(map_item)
            .collect::<Result<_, _>>()?;
        Ok(ChangePage {
            changes,
            checkpoint,
        })
    }
}
#[async_trait]
impl MetadataProvider for OneDrive {
    fn provider_id(&self) -> &'static str {
        "onedrive"
    }
    async fn changes(
        &self,
        scope: &Scope,
        cursor: Option<&Cursor>,
        cancel: &CancellationToken,
    ) -> Result<ChangePage, ProviderError> {
        let _permit = self.budget.acquire(Priority::Background, cancel).await?;
        tokio::select! { biased;
            _=cancel.cancelled()=>Err(ProviderError::Cancelled),
            result=tokio::time::timeout(Duration::from_secs(45),self.fetch(scope,cursor))=>result.unwrap_or(Err(ProviderError::Unavailable)),
        }
    }
}

#[async_trait]
impl ReadProvider for OneDrive {
    async fn node(
        &self,
        scope: &Scope,
        id: &str,
        cancel: &CancellationToken,
    ) -> Result<Node, ProviderError> {
        if scope.account != self.account || scope.provider != "onedrive" {
            return Err(ProviderError::Protocol("provider/account mismatch"));
        }
        let item: DriveItem = self
            .resource(&["drives", &scope.collection, "items", id], cancel)
            .await?;
        match map_item(item)? {
            Change::Upsert(node) => Ok(node),
            _ => Err(ProviderError::NotFound),
        }
    }
    async fn children(
        &self,
        scope: &Scope,
        parent: &str,
        cursor: Option<&Cursor>,
        cancel: &CancellationToken,
    ) -> Result<DirectoryPage, ProviderError> {
        if scope.account != self.account || scope.provider != "onedrive" {
            return Err(ProviderError::Protocol("provider/account mismatch"));
        }
        let base =
            self.resource_url(&["drives", &scope.collection, "items", parent, "children"])?;
        let url = match cursor {
            Some(c) => self.validate_cursor(&scope.collection, &c.0)?,
            None => base.clone(),
        };
        if url.path() != base.path() {
            return Err(ProviderError::Protocol(
                "directory continuation outside parent",
            ));
        }
        let _permit = self.budget.acquire(Priority::Interactive, cancel).await?;
        let body = tokio::select! {biased;_=cancel.cancelled()=>return Err(ProviderError::Cancelled),r=tokio::time::timeout(Duration::from_secs(45),self.request_bytes(url))=>r.map_err(|_|ProviderError::Unavailable)??};
        let page: ChildrenResponse = serde_json::from_slice(&body)
            .map_err(|_| ProviderError::Protocol("invalid directory response"))?;
        if let Some(next) = &page.next {
            let parsed = self.validate_cursor(&scope.collection, next)?;
            if parsed.path() != base.path() {
                return Err(ProviderError::Protocol(
                    "directory continuation outside parent",
                ));
            }
        }
        let mut nodes = vec![];
        for item in page.value {
            if let Change::Upsert(mut node) = map_item(item)? {
                node.parent_id = Some(parent.into());
                nodes.push(node);
            }
        }
        Ok(DirectoryPage {
            nodes,
            next: page.next.map(Cursor),
        })
    }
    async fn read_range(
        &self,
        scope: &Scope,
        node: &Node,
        offset: u64,
        length: u32,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>, ProviderError> {
        if scope.account != self.account || scope.provider != "onedrive" {
            return Err(ProviderError::Protocol("provider/account mismatch"));
        }
        if offset >= node.size || length == 0 {
            return Ok(vec![]);
        }
        if length > 8 * 1024 * 1024 {
            return Err(ProviderError::Protocol("range exceeds memory limit"));
        }
        let _permit = self.budget.acquire(Priority::Content, cancel).await?;
        tokio::select! {biased;
            _=cancel.cancelled()=>Err(ProviderError::Cancelled),
            result=tokio::time::timeout(Duration::from_secs(60),self.fetch_range(scope,node,offset,length))=>result.map_err(|_|ProviderError::Unavailable)?,
        }
    }
}
#[derive(Deserialize)]
struct ChildrenResponse {
    value: Vec<DriveItem>,
    #[serde(rename = "@odata.nextLink")]
    next: Option<String>,
}

fn retry_after(header: Option<&str>, now: SystemTime) -> Duration {
    header
        .and_then(|v| {
            v.parse::<u64>().ok().map(Duration::from_secs).or_else(|| {
                httpdate::parse_http_date(v)
                    .ok()
                    .map(|date| date.duration_since(now).unwrap_or_default())
            })
        })
        .unwrap_or(Duration::from_secs(30))
        .max(Duration::from_secs(1))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveInfo {
    pub id: String,
    pub name: String,
    pub drive_type: String,
    #[serde(default)]
    pub web_url: String,
}
#[derive(Deserialize)]
struct DriveList {
    value: Vec<DriveInfo>,
    #[serde(rename = "@odata.nextLink")]
    next: Option<String>,
}

#[derive(Deserialize)]
struct DeltaResponse {
    value: Vec<DriveItem>,
    #[serde(rename = "@odata.nextLink")]
    next: Option<String>,
    #[serde(rename = "@odata.deltaLink")]
    delta: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DriveItem {
    id: String,
    name: Option<String>,
    size: Option<u64>,
    e_tag: Option<String>,
    c_tag: Option<String>,
    last_modified_date_time: Option<String>,
    parent_reference: Option<Parent>,
    file: Option<serde_json::Value>,
    folder: Option<serde_json::Value>,
    package: Option<serde_json::Value>,
    deleted: Option<serde_json::Value>,
    remote_item: Option<RemoteItem>,
    #[serde(rename = "@microsoft.graph.downloadUrl")]
    download_url: Option<String>,
}
impl DriveItem {
    fn matches_content(&self, expected: (&str, &str), size: u64) -> bool {
        let revision = match expected.0 {
            "content" => self.c_tag.as_deref(),
            "etag" => self.e_tag.as_deref(),
            _ => None,
        };
        revision == Some(expected.1) && self.size == Some(size)
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Parent {
    id: Option<String>,
    drive_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteItem {
    id: String,
    file: Option<serde_json::Value>,
    folder: Option<serde_json::Value>,
    package: Option<serde_json::Value>,
    parent_reference: Parent,
}
fn map_item(item: DriveItem) -> Result<Change, ProviderError> {
    if item.id.is_empty() {
        return Err(ProviderError::Protocol("empty item ID"));
    }
    if item.deleted.is_some() {
        return Ok(Change::Delete { id: item.id });
    }
    let target = item
        .remote_item
        .map(|r| {
            if r.id.is_empty() {
                return Err(ProviderError::Protocol("empty shortcut target"));
            }
            let collection = r
                .parent_reference
                .drive_id
                .filter(|v| !v.is_empty())
                .ok_or(ProviderError::Protocol("shortcut missing target drive"))?;
            Ok(RemoteRef {
                collection,
                item: r.id,
                kind: if r.folder.is_some() || r.package.is_some() {
                    Some(NodeKind::Folder)
                } else if r.file.is_some() {
                    Some(NodeKind::File)
                } else {
                    None
                },
            })
        })
        .transpose()?;
    let kind = if target.is_some() {
        NodeKind::Shortcut
    } else if item.folder.is_some() || item.package.is_some() {
        NodeKind::Folder
    } else if item.file.is_some() {
        NodeKind::File
    } else {
        return Err(ProviderError::Protocol("unsupported item facet"));
    };
    Ok(Change::Upsert(Node {
        id: item.id,
        parent_id: item.parent_reference.and_then(|p| p.id),
        name: item
            .name
            .ok_or(ProviderError::Protocol("item missing name"))?,
        kind,
        size: item.size.unwrap_or(0),
        modified_unix: item
            .last_modified_date_time
            .as_deref()
            .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
            .map(|t| t.timestamp().max(0) as u64)
            .unwrap_or(0),
        etag: item.e_tag,
        content_version: item.c_tag,
        target,
    }))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    fn scope() -> Scope {
        Scope {
            account: "fixture".into(),
            provider: "onedrive".into(),
            collection: "drive".into(),
        }
    }
    fn provider(endpoint: Url) -> OneDrive {
        OneDrive::build(
            "fixture".into(),
            Arc::new(StaticToken(SecretString::from("fake-token"))),
            endpoint,
        )
        .unwrap()
    }
    async fn server(
        status: u16,
        body: &str,
        headers: &str,
    ) -> (Url, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}/v1.0/");
        let body = body.replace("BASE", &base);
        let response = format!(
            "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
            body.len()
        );
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0u8; 8192];
            let mut request = String::new();
            while !request.contains("\r\n\r\n") {
                let n = socket.read(&mut buffer).await.unwrap();
                if n == 0 {
                    break;
                }
                request.push_str(&String::from_utf8_lossy(&buffer[..n]));
            }
            socket.write_all(response.as_bytes()).await.unwrap();
            request
        });
        (Url::parse(&base).unwrap(), task)
    }
    #[tokio::test]
    async fn actual_http_maps_shortcuts_tombstones_and_delta() {
        let body = r#"{"value":[{"id":"link","name":"Team","remoteItem":{"id":"target","parentReference":{"driveId":"team-drive"}}},{"id":"gone","deleted":{}}],"@odata.deltaLink":"BASEdrives/drive/root/delta?token=next"}"#;
        let (url, task) = server(200, body, "").await;
        let p = provider(url);
        let result = p
            .changes(&scope(), None, &CancellationToken::new())
            .await
            .unwrap();
        assert!(result.checkpoint.complete());
        let Change::Upsert(node) = &result.changes[0] else {
            panic!("expected node")
        };
        assert_eq!(node.target.as_ref().unwrap().collection, "team-drive");
        assert!(matches!(result.changes[1], Change::Delete { .. }));
        let request = task.await.unwrap();
        assert!(request.contains("GET /v1.0/drives/drive/root/delta"));
        assert!(
            request
                .to_lowercase()
                .contains("authorization: bearer fake-token")
        );
    }
    #[tokio::test]
    async fn throttling_sets_cooldown_without_a_second_network_call() {
        let (url, task) = server(429, "", "Retry-After: 60\r\n").await;
        let p = provider(url);
        let cancel = CancellationToken::new();
        assert!(matches!(
            p.changes(&scope(), None, &cancel).await,
            Err(ProviderError::Throttled(_))
        ));
        task.await.unwrap();
        assert!(matches!(
            p.changes(&scope(), None, &cancel).await,
            Err(ProviderError::Throttled(_))
        ));
    }
    #[tokio::test]
    async fn expired_cursor_is_explicit() {
        let (url, task) = server(410, "", "").await;
        let p = provider(url);
        assert!(matches!(
            p.changes(&scope(), None, &CancellationToken::new()).await,
            Err(ProviderError::CursorExpired)
        ));
        task.await.unwrap();
    }
    #[test]
    fn cursor_cannot_send_token_to_another_origin_or_drive() {
        let p = provider(Url::parse("https://graph.microsoft.com/v1.0/").unwrap());
        for url in [
            "https://evil.example/v1.0/drives/drive/root/delta",
            "https://graph.microsoft.com/v1.0/drives/other/root/delta",
            "http://graph.microsoft.com/v1.0/drives/drive/root/delta",
            "https://user@graph.microsoft.com/v1.0/drives/drive/root/delta",
        ] {
            assert!(p.validate_cursor("drive", url).is_err());
        }
    }
    #[test]
    fn retry_after_supports_dates_and_seconds() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        assert_eq!(retry_after(Some("17"), now), Duration::from_secs(17));
        assert_eq!(
            retry_after(
                Some(&httpdate::fmt_http_date(now + Duration::from_secs(23))),
                now
            ),
            Duration::from_secs(23)
        );
        assert_eq!(retry_after(Some("garbage"), now), Duration::from_secs(30));
    }
    #[tokio::test]
    async fn cancellation_interrupts_a_stalled_http_request() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let p = provider(
            Url::parse(&format!("http://{}/v1.0/", listener.local_addr().unwrap())).unwrap(),
        );
        let cancel = CancellationToken::new();
        let request_cancel = cancel.clone();
        let request = tokio::spawn(async move { p.changes(&scope(), None, &request_cancel).await });
        let (_socket, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
            .await
            .unwrap()
            .unwrap();
        cancel.cancel();
        assert!(matches!(
            tokio::time::timeout(Duration::from_millis(500), request)
                .await
                .unwrap()
                .unwrap(),
            Err(ProviderError::Cancelled)
        ));
    }
    #[tokio::test]
    async fn redirects_are_not_followed_and_error_bodies_are_not_exposed() {
        let (url, task) = server(
            302,
            "sensitive-provider-body",
            "Location: https://example.invalid/token-leak\r\n",
        )
        .await;
        let p = provider(url);
        let error = p
            .changes(&scope(), None, &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(error, ProviderError::Protocol(_)));
        assert!(!format!("{error:?}").contains("sensitive-provider-body"));
        task.await.unwrap();
    }
    #[tokio::test]
    async fn response_cannot_store_a_foreign_continuation() {
        let (url, task) = server(
            200,
            r#"{"value":[],"@odata.nextLink":"https://example.invalid/leak"}"#,
            "",
        )
        .await;
        let p = provider(url);
        assert!(matches!(
            p.changes(&scope(), None, &CancellationToken::new()).await,
            Err(ProviderError::Protocol(_))
        ));
        task.await.unwrap();
    }
    async fn download_fixture(
        mode: &'static str,
    ) -> (
        OneDrive,
        Arc<Mutex<Vec<String>>>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let endpoint = Url::parse(&format!("{origin}/v1.0/")).unwrap();
        let requests = Arc::new(Mutex::new(vec![]));
        let seen = requests.clone();
        let task = tokio::spawn(async move {
            let mut metadata = 0;
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = String::new();
                let mut chunk = [0; 1024];
                while !request.contains("\r\n\r\n") {
                    let n = socket.read(&mut chunk).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    request.push_str(&String::from_utf8_lossy(&chunk[..n]));
                }
                let download = request.starts_with("GET /download");
                seen.lock().await.push(request);
                let (status, headers, body) = if download {
                    match mode {
                        "throttle" => (
                            429,
                            "Retry-After: 20\r\n".into(),
                            "provider-private-error".into(),
                        ),
                        "wrong_range" => {
                            (206, "Content-Range: bytes 0-3/10\r\n".into(), "cdef".into())
                        }
                        "oversize" => (
                            206,
                            "Content-Range: bytes 2-5/10\r\n".into(),
                            "cdefg".into(),
                        ),
                        "ignored_range" => (200, String::new(), "abcdefghij".into()),
                        _ => (206, "Content-Range: bytes 2-5/10\r\n".into(), "cdef".into()),
                    }
                } else {
                    metadata += 1;
                    let version = if matches!(mode, "changed" | "metadata_changed") && metadata > 1
                    {
                        "v2"
                    } else {
                        "v1"
                    };
                    let content = if mode == "content_changed" && metadata > 1 {
                        "content-v2"
                    } else {
                        "content-v1"
                    };
                    (
                        200,
                        String::new(),
                        format!(
                            r#"{{"id":"file","name":"file","size":10,"eTag":"{version}","cTag":"{content}","file":{{}},"@microsoft.graph.downloadUrl":"{origin}/download?synthetic-signed-url"}}"#
                        ),
                    )
                };
                let response = format!(
                    "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });
        (provider(endpoint), requests, task)
    }
    fn download_node() -> Node {
        Node {
            id: "file".into(),
            name: "file".into(),
            parent_id: None,
            kind: NodeKind::File,
            size: 10,
            modified_unix: 0,
            etag: Some("v1".into()),
            content_version: None,
            target: None,
        }
    }
    #[tokio::test]
    async fn content_is_exactly_ranged_and_bearer_tokens_never_reach_download_requests() {
        let (provider, requests, server) = download_fixture("ok").await;
        let result = provider
            .read_range(&scope(), &download_node(), 2, 4, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result, b"cdef");
        let requests = requests.lock().await;
        assert_eq!(requests.len(), 3);
        assert!(
            requests[0]
                .to_lowercase()
                .contains("authorization: bearer fake-token")
        );
        assert!(requests[1].to_lowercase().contains("range: bytes=2-5"));
        assert!(!requests[1].to_lowercase().contains("authorization:"));
        server.abort();
        let _ = server.await;
    }
    #[tokio::test]
    async fn content_rejects_changed_versions_bad_ranges_and_oversized_bodies() {
        for mode in [
            "changed",
            "wrong_range",
            "oversize",
            "ignored_range",
            "throttle",
        ] {
            let (provider, requests, server) = download_fixture(mode).await;
            let error = provider
                .read_range(&scope(), &download_node(), 2, 4, &CancellationToken::new())
                .await
                .unwrap_err();
            if mode == "changed" {
                assert!(matches!(error, ProviderError::VersionChanged));
            }
            if mode == "throttle" {
                assert!(matches!(error, ProviderError::Throttled(_)));
                assert!(matches!(
                    provider
                        .node(&scope(), "file", &CancellationToken::new())
                        .await,
                    Err(ProviderError::Throttled(_))
                ));
                assert_eq!(requests.lock().await.len(), 2);
            }
            assert!(!error.to_string().contains("provider-private-error"));
            server.abort();
            let _ = server.await;
        }
    }
    #[tokio::test]
    async fn packages_do_not_abort_delta_or_directory_enumeration() {
        let body = r#"{"value":[{"id":"notebook","name":"Notes","package":{"type":"oneNote"}},{"id":"ordinary","name":"report.txt","file":{}},{"id":"linked-notebook","name":"Team notes","remoteItem":{"id":"target","parentReference":{"driveId":"team"},"package":{"type":"futurePackage"}}}],"@odata.deltaLink":"BASEdrives/drive/root/delta?token=next"}"#;
        let (url, task) = server(200, body, "").await;
        let graph = provider(url);
        let result = graph
            .changes(&scope(), None, &CancellationToken::new())
            .await;
        task.await.unwrap();
        let page = result.unwrap();
        assert_eq!(page.changes.len(), 3);
        let Change::Upsert(notebook) = &page.changes[0] else {
            panic!("missing notebook")
        };
        assert_eq!(notebook.kind, NodeKind::Folder);
        let Change::Upsert(link) = &page.changes[2] else {
            panic!("missing linked notebook")
        };
        assert_eq!(link.target.as_ref().unwrap().kind, Some(NodeKind::Folder));

        let mut children: serde_json::Value = serde_json::from_str(body).unwrap();
        children.as_object_mut().unwrap().remove("@odata.deltaLink");
        let (url, task) = server(200, &children.to_string(), "").await;
        let graph = provider(url);
        let result = graph
            .children(&scope(), "root", None, &CancellationToken::new())
            .await;
        task.await.unwrap();
        let listing = result.unwrap();
        assert_eq!(listing.nodes.len(), 3);
        assert_eq!(listing.nodes[0].kind, NodeKind::Folder);
        assert!(
            listing
                .nodes
                .iter()
                .all(|node| node.parent_id.as_deref() == Some("root"))
        );
    }
    #[tokio::test]
    async fn content_tags_allow_metadata_only_changes_but_reject_new_bytes() {
        for mode in ["metadata_changed", "content_changed"] {
            let (provider, _, server) = download_fixture(mode).await;
            let mut node = download_node();
            node.content_version = Some("content-v1".into());
            let result = provider
                .read_range(&scope(), &node, 2, 4, &CancellationToken::new())
                .await;
            server.abort();
            let _ = server.await;
            if mode == "metadata_changed" {
                assert_eq!(result.unwrap(), b"cdef");
            } else {
                assert!(matches!(result, Err(ProviderError::VersionChanged)));
            }
        }
    }
}
