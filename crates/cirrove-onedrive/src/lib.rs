//! Read-only Microsoft Graph adapter. Authentication is injected; tokens, cursor
//! URLs and remote error bodies must never appear in logs.
use async_trait::async_trait;
use cirrove_core::{
    CancellationToken, Change, ChangePage, Checkpoint, Cursor, MetadataProvider, Node, NodeKind,
    Priority, ProviderError, RemoteRef, RequestBudget, Scope,
};
use reqwest::{Client, StatusCode, Url, redirect::Policy};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
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
}

/// Developer bootstrap only. A production sign-in/refresh broker is not yet built.
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
            endpoint,
            client,
            tokens,
            budget: RequestBudget::default(),
            cooldown: Mutex::new(None),
        })
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
    async fn fetch(
        &self,
        scope: &Scope,
        cursor: Option<&Cursor>,
    ) -> Result<ChangePage, ProviderError> {
        if scope.provider != "onedrive" || scope.account != self.account {
            return Err(ProviderError::Protocol("provider/account mismatch"));
        }
        if let Some(deadline) = *self.cooldown.lock().await {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if !remaining.is_zero() {
                return Err(ProviderError::Throttled(remaining));
            }
        }
        let url = match cursor {
            Some(c) => self.validate_cursor(&scope.collection, &c.0)?,
            None => self.initial_url(&scope.collection)?,
        };
        let token = self.tokens.access_token().await?;
        let mut response = self
            .client
            .get(url)
            .bearer_auth(token.expose_secret())
            .header("Prefer", "Include-Feature=AddToOneDrive")
            .send()
            .await
            .map_err(|_| ProviderError::Unavailable)?;
        let status = response.status();
        if status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::SERVICE_UNAVAILABLE {
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
            *cooldown = Some(cooldown.map_or(deadline, |existing| existing.max(deadline)));
            return Err(ProviderError::Throttled(delay));
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
    parent_reference: Option<Parent>,
    file: Option<serde_json::Value>,
    folder: Option<serde_json::Value>,
    deleted: Option<serde_json::Value>,
    remote_item: Option<RemoteItem>,
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
            })
        })
        .transpose()?;
    let kind = if target.is_some() {
        NodeKind::Shortcut
    } else if item.folder.is_some() {
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
        etag: item.e_tag,
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
}
