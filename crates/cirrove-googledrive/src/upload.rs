//! Google create uploads. This adapter is deliberately not selected by the
//! service yet: the mounted namespace and replacement policy remain read-only.
//! Prepared IDs and resumable session URLs stay inside opaque vault checkpoints.
use super::{files::File, transport::body, *};
use async_trait::async_trait;
use cirrove_core::upload::{
    Reconciliation, Result, UploadError, UploadIntent, UploadProgress, UploadProvider,
    UploadRequest, UploadStep,
};
use cirrove_core::{CancellationToken, Node, NodeKind, Priority, ProviderError, ReadProvider};
use reqwest::{Method, Response, StatusCode, Url};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{future::Future, time::Duration};

const ALIGNMENT: u64 = 256 * 1024;
const PART_SIZE: u32 = 8 * 1024 * 1024;
const MAX_UPLOAD_RESPONSE: usize = 1024 * 1024;
const MAX_CHECKPOINT: usize = 64 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
enum SavedUpload {
    Prepared {
        version: u8,
        request: UploadRequest,
        id: String,
    },
    Session {
        version: u8,
        request: UploadRequest,
        id: String,
        url: String,
        offset: u64,
        length: u32,
    },
}
impl SavedUpload {
    fn request(&self) -> &UploadRequest {
        match self {
            Self::Prepared { request, .. } | Self::Session { request, .. } => request,
        }
    }
    fn id(&self) -> &str {
        match self {
            Self::Prepared { id, .. } | Self::Session { id, .. } => id,
        }
    }
}

#[derive(Deserialize)]
struct GeneratedIds {
    #[serde(default)]
    ids: Vec<String>,
    space: String,
    kind: String,
}

fn protocol(message: &'static str) -> UploadError {
    ProviderError::Protocol(message).into()
}

impl GoogleDrive {
    fn check_upload(&self, request: &UploadRequest) -> Result<()> {
        request.validate()?;
        self.check_scope(&request.scope)
            .map_err(|_| UploadError::Invalid)?;
        let UploadIntent::Create { parent, .. } = &request.intent else {
            return Err(UploadError::Unsupported(
                "Google replacement safety is not implemented",
            ));
        };
        valid_id(parent).map_err(|_| UploadError::Invalid)
    }

    fn prepared_checkpoint(&self, request: &UploadRequest, id: String) -> Result<SecretString> {
        valid_id(&id).map_err(|_| UploadError::Invalid)?;
        serde_json::to_string(&SavedUpload::Prepared {
            version: 1,
            request: request.clone(),
            id,
        })
        .map(SecretString::from)
        .map_err(|_| UploadError::Invalid)
    }

    fn session_checkpoint(
        &self,
        request: &UploadRequest,
        id: String,
        url: String,
        offset: u64,
    ) -> Result<UploadStep> {
        self.session_url(&url)?;
        if offset >= request.size {
            return Err(protocol("Google session offset exceeds snapshot"));
        }
        let remaining = request.size - offset;
        let length = remaining.min(u64::from(PART_SIZE)) as u32;
        if u64::from(length) < remaining && !u64::from(length).is_multiple_of(ALIGNMENT) {
            return Err(protocol("unaligned Google upload range"));
        }
        let saved = SavedUpload::Session {
            version: 1,
            request: request.clone(),
            id,
            url,
            offset,
            length,
        };
        let checkpoint = serde_json::to_string(&saved)
            .map(SecretString::from)
            .map_err(|_| UploadError::Invalid)?;
        Ok(UploadStep::Continue(UploadProgress {
            checkpoint,
            offset,
            length,
        }))
    }

    fn load_upload(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
    ) -> Result<SavedUpload> {
        if checkpoint.expose_secret().len() > MAX_CHECKPOINT {
            return Err(UploadError::CheckpointInvalid);
        }
        let saved: SavedUpload = serde_json::from_str(checkpoint.expose_secret())
            .map_err(|_| UploadError::CheckpointInvalid)?;
        let version = match &saved {
            SavedUpload::Prepared { version, .. } | SavedUpload::Session { version, .. } => {
                *version
            }
        };
        if version != 1 || saved.request() != request || valid_id(saved.id()).is_err() {
            return Err(UploadError::CheckpointInvalid);
        }
        if let SavedUpload::Session {
            url,
            offset,
            length,
            ..
        } = &saved
        {
            self.session_url(url)
                .map_err(|_| UploadError::CheckpointInvalid)?;
            if *length == 0
                || *length > PART_SIZE
                || *offset >= request.size
                || u64::from(*length) > request.size - *offset
                || (*offset + u64::from(*length) < request.size
                    && !u64::from(*length).is_multiple_of(ALIGNMENT))
            {
                return Err(UploadError::CheckpointInvalid);
            }
        }
        Ok(saved)
    }

    fn upload_endpoint(&self) -> Url {
        let mut url = self.endpoint.clone();
        url.set_path("/upload/drive/v3/files");
        url.set_query(None);
        url.set_fragment(None);
        url
    }

    fn session_url(&self, value: &str) -> Result<Url> {
        let url = Url::parse(value).map_err(|_| protocol("invalid Google upload session URL"))?;
        let endpoint = self.upload_endpoint();
        if !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || url.origin() != endpoint.origin()
            || url.path() != endpoint.path()
        {
            return Err(protocol("unsafe Google upload session URL"));
        }
        Ok(url)
    }

    async fn upload_call<T>(
        &self,
        cancel: &CancellationToken,
        timeout: Duration,
        call: impl Future<Output = Result<T>>,
    ) -> Result<T> {
        let _permit = self.budget.acquire(Priority::Upload, cancel).await?;
        if let Some(until) = *self.cooldown.lock().await
            && until > Instant::now()
        {
            return Err(ProviderError::Throttled(until - Instant::now()).into());
        }
        tokio::select! { biased;
            _ = cancel.cancelled() => Err(UploadError::Uncertain),
            result = tokio::time::timeout(timeout, call) => {
                result.unwrap_or(Err(UploadError::Uncertain))
            }
        }
    }

    async fn authorized_json_upload(
        &self,
        method: Method,
        url: Url,
        value: &serde_json::Value,
        size: Option<u64>,
    ) -> Result<Response> {
        for attempt in 0..2 {
            let token = self.tokens.access_token().await?;
            let mut request = self
                .client
                .request(method.clone(), url.clone())
                .bearer_auth(token.expose_secret())
                .header("Accept-Encoding", "identity")
                .json(value);
            if let Some(size) = size {
                request = request
                    .header("X-Upload-Content-Type", "application/octet-stream")
                    .header("X-Upload-Content-Length", size);
            }
            let response = request.send().await.map_err(|_| UploadError::Uncertain)?;
            if response.status() == StatusCode::UNAUTHORIZED && attempt == 0 {
                self.tokens.invalidate(&token).await;
                continue;
            }
            return self.upload_response(response, false).await;
        }
        Err(ProviderError::Authentication.into())
    }

    async fn upload_response(&self, response: Response, session: bool) -> Result<Response> {
        let status = response.status();
        if matches!(status, StatusCode::OK | StatusCode::CREATED)
            || (session && status == StatusCode::PERMANENT_REDIRECT)
        {
            return Ok(response);
        }
        match status {
            StatusCode::NOT_FOUND | StatusCode::GONE if session => Err(UploadError::SessionGone),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN if session => {
                Err(UploadError::SessionGone)
            }
            StatusCode::UNAUTHORIZED => Err(ProviderError::Authentication.into()),
            StatusCode::NOT_FOUND => Err(ProviderError::NotFound.into()),
            StatusCode::CONFLICT => Err(UploadError::Uncertain),
            StatusCode::BAD_REQUEST => Err(UploadError::Invalid),
            StatusCode::FORBIDDEN => {
                let bytes = body(response, 64 * 1024)
                    .await
                    .map_err(UploadError::Provider)?;
                #[derive(Deserialize)]
                struct Reason {
                    reason: String,
                }
                #[derive(Deserialize)]
                struct Detail {
                    #[serde(default)]
                    errors: Vec<Reason>,
                }
                #[derive(Deserialize)]
                struct ErrorBody {
                    error: Detail,
                }
                let quota = serde_json::from_slice::<ErrorBody>(&bytes)
                    .ok()
                    .is_some_and(|error| {
                        error.error.errors.iter().any(|error| {
                            matches!(
                                error.reason.as_str(),
                                "rateLimitExceeded"
                                    | "userRateLimitExceeded"
                                    | "storageQuotaExceeded"
                            )
                        })
                    });
                Err(if quota {
                    UploadError::Quota
                } else {
                    ProviderError::Permission.into()
                })
            }
            StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => {
                let delay = response
                    .headers()
                    .get("retry-after")
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok())
                    .map(Duration::from_secs)
                    .unwrap_or(Duration::from_secs(30))
                    .clamp(Duration::from_secs(1), Duration::from_secs(86400));
                let until = Instant::now() + delay;
                let mut cooldown = self.cooldown.lock().await;
                *cooldown = Some(cooldown.map_or(until, |old| old.max(until)));
                Err(ProviderError::Throttled(delay).into())
            }
            status if status.is_server_error() => Err(UploadError::Uncertain),
            _ => Err(UploadError::UnexpectedStatus(status.as_u16())),
        }
    }

    async fn generated_id(&self) -> Result<String> {
        let mut url = self.url(&["files", "generateIds"])?;
        url.query_pairs_mut()
            .extend_pairs([("count", "1"), ("space", "drive"), ("type", "files")]);
        let generated: GeneratedIds = self.json(url).await?;
        if generated.kind != "drive#generatedIds"
            || generated.space != "drive"
            || generated.ids.len() != 1
        {
            return Err(protocol("invalid Google generated IDs response"));
        }
        let id = generated
            .ids
            .into_iter()
            .next()
            .ok_or_else(|| protocol("invalid Google generated IDs response"))?;
        valid_id(&id).map_err(UploadError::Provider)?;
        Ok(id)
    }

    fn create_metadata(request: &UploadRequest, id: &str) -> serde_json::Value {
        let UploadIntent::Create { parent, name } = &request.intent else {
            unreachable!("checked before metadata construction")
        };
        json!({
            "id": id,
            "name": name,
            "parents": [parent],
            "mimeType": "application/octet-stream"
        })
    }

    async fn begin_session(&self, request: &UploadRequest, id: &str) -> Result<UploadStep> {
        let mut url = self.upload_endpoint();
        url.query_pairs_mut()
            .append_pair("uploadType", "resumable")
            .append_pair("fields", files::FIELDS);
        let response = self
            .authorized_json_upload(
                Method::POST,
                url,
                &Self::create_metadata(request, id),
                Some(request.size),
            )
            .await?;
        if response.status() != StatusCode::OK {
            return Err(UploadError::Uncertain);
        }
        let location = response
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| protocol("Google upload session has no location"))?;
        self.session_checkpoint(request, id.to_owned(), location.to_owned(), 0)
    }

    async fn create_empty(&self, request: &UploadRequest, id: &str) -> Result<UploadStep> {
        let mut url = self.url(&["files"])?;
        url.query_pairs_mut().append_pair("fields", files::FIELDS);
        let response = self
            .authorized_json_upload(Method::POST, url, &Self::create_metadata(request, id), None)
            .await?;
        Ok(UploadStep::Complete(
            self.receipt(request, id, response).await?,
        ))
    }

    async fn receipt(&self, request: &UploadRequest, id: &str, response: Response) -> Result<Node> {
        let bytes = body(response, MAX_UPLOAD_RESPONSE)
            .await
            .map_err(|_| UploadError::Uncertain)?;
        let file: File = serde_json::from_slice(&bytes).map_err(|_| UploadError::Uncertain)?;
        self.receipt_from_file(request, id, file)
    }

    fn receipt_from_file(&self, request: &UploadRequest, id: &str, file: File) -> Result<Node> {
        let UploadIntent::Create { parent, name } = &request.intent else {
            return Err(UploadError::Invalid);
        };
        if file.id != id
            || &file.name != name
            || file.parents.len() != 1
            || file.parents.first() != Some(parent)
            || file.mime_type != "application/octet-stream"
            || file
                .size
                .as_deref()
                .and_then(|size| size.parse::<u64>().ok())
                != Some(request.size)
        {
            return Err(UploadError::Uncertain);
        }
        let mut node = file
            .node(&request.scope.collection)
            .map_err(|_| UploadError::Uncertain)?;
        // The upload journal confirms the requested provider name. The current
        // read-only Google projection adds an ID suffix later at the namespace
        // boundary; writable publication is intentionally not connected yet.
        node.name = name.clone();
        if node.kind != NodeKind::File || node.content_revision().is_none() {
            return Err(UploadError::Uncertain);
        }
        Ok(node)
    }

    fn received_offset(response: &Response, size: u64) -> Result<u64> {
        let Some(range) = response
            .headers()
            .get("range")
            .and_then(|value| value.to_str().ok())
        else {
            return Ok(0);
        };
        let end = range
            .strip_prefix("bytes=0-")
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| protocol("invalid Google upload status range"))?;
        let offset = end
            .checked_add(1)
            .ok_or_else(|| protocol("invalid Google upload status range"))?;
        if offset >= size {
            return Err(protocol("invalid Google upload status offset"));
        }
        Ok(offset)
    }

    async fn session_request(
        &self,
        method: Method,
        url: Url,
        range: String,
        bytes: Vec<u8>,
    ) -> Result<Response> {
        let response = self
            .client
            .request(method, url)
            .header("Accept-Encoding", "identity")
            .header("Content-Length", bytes.len())
            .header("Content-Range", range)
            .body(bytes)
            .send()
            .await
            .map_err(|_| UploadError::Uncertain)?;
        self.upload_response(response, true).await
    }

    async fn inspect_prepared(&self, request: &UploadRequest, id: &str) -> Result<UploadStep> {
        match self.file(id).await {
            Ok(file) => Ok(UploadStep::Complete(
                self.receipt_from_file(request, id, file)?,
            )),
            Err(ProviderError::NotFound) if request.size == 0 => {
                self.create_empty(request, id).await
            }
            Err(ProviderError::NotFound) => self.begin_session(request, id).await,
            Err(error) => Err(error.into()),
        }
    }
}

#[async_trait]
impl UploadProvider for GoogleDrive {
    async fn begin_upload(
        &self,
        request: &UploadRequest,
        cancel: &CancellationToken,
    ) -> Result<UploadStep> {
        self.check_upload(request)?;
        self.upload_call(cancel, Duration::from_secs(125), async {
            let id = self.generated_id().await?;
            Ok(UploadStep::Prepared(self.prepared_checkpoint(request, id)?))
        })
        .await
    }

    async fn inspect_upload(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
        cancel: &CancellationToken,
    ) -> Result<UploadStep> {
        self.check_upload(request)?;
        let saved = self.load_upload(request, checkpoint)?;
        self.upload_call(cancel, Duration::from_secs(125), async {
            match saved {
                SavedUpload::Prepared { id, .. } => self.inspect_prepared(request, &id).await,
                SavedUpload::Session { id, url, .. } => {
                    let result = self
                        .session_request(
                            Method::PUT,
                            self.session_url(&url)?,
                            format!("bytes */{}", request.size),
                            Vec::new(),
                        )
                        .await;
                    match result {
                        Ok(response) if response.status() == StatusCode::PERMANENT_REDIRECT => {
                            let offset = Self::received_offset(&response, request.size)?;
                            self.session_checkpoint(request, id, url, offset)
                        }
                        Ok(response) => Ok(UploadStep::Complete(
                            self.receipt(request, &id, response).await?,
                        )),
                        Err(UploadError::SessionGone) => {
                            Ok(UploadStep::Prepared(self.prepared_checkpoint(request, id)?))
                        }
                        Err(error) => Err(error),
                    }
                }
            }
        })
        .await
    }

    async fn upload_part(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
        offset: u64,
        bytes: Vec<u8>,
        cancel: &CancellationToken,
    ) -> Result<UploadStep> {
        self.check_upload(request)?;
        let saved = self.load_upload(request, checkpoint)?;
        let SavedUpload::Session {
            id,
            url,
            offset: expected,
            length,
            ..
        } = saved
        else {
            return Err(UploadError::CheckpointInvalid);
        };
        if offset != expected || bytes.len() != length as usize {
            return Err(UploadError::Invalid);
        }
        let end = offset + bytes.len() as u64;
        self.upload_call(cancel, Duration::from_secs(125), async {
            let result = self
                .session_request(
                    Method::PUT,
                    self.session_url(&url)?,
                    format!("bytes {offset}-{}/{}", end - 1, request.size),
                    bytes,
                )
                .await;
            match result {
                Ok(response) if response.status() == StatusCode::PERMANENT_REDIRECT => {
                    let next = Self::received_offset(&response, request.size)?;
                    if next != end {
                        return Err(UploadError::Uncertain);
                    }
                    self.session_checkpoint(request, id, url, next)
                }
                Ok(response) => Ok(UploadStep::Complete(
                    self.receipt(request, &id, response).await?,
                )),
                Err(UploadError::SessionGone) => {
                    Ok(UploadStep::Prepared(self.prepared_checkpoint(request, id)?))
                }
                Err(error) => Err(error),
            }
        })
        .await
    }

    async fn commit_upload(
        &self,
        request: &UploadRequest,
        _checkpoint: &SecretString,
        _cancel: &CancellationToken,
    ) -> Result<UploadStep> {
        self.check_upload(request)?;
        Err(UploadError::Unsupported(
            "Google create uploads commit with their final byte range",
        ))
    }

    async fn reconcile_upload(
        &self,
        request: &UploadRequest,
        checkpoint: Option<&SecretString>,
        cancel: &CancellationToken,
    ) -> Result<Reconciliation> {
        self.check_upload(request)?;
        let Some(checkpoint) = checkpoint else {
            // No mutation is permitted before a Prepared checkpoint is durable.
            return Ok(Reconciliation::Uncommitted);
        };
        let saved = self.load_upload(request, checkpoint)?;
        let id = saved.id().to_owned();
        self.upload_call(cancel, Duration::from_secs(15 * 60), async {
            let file = match self.file(&id).await {
                Ok(file) => file,
                Err(ProviderError::NotFound) if matches!(saved, SavedUpload::Prepared { .. }) => {
                    return Ok(Reconciliation::Uncommitted);
                }
                Err(ProviderError::NotFound) => return Err(UploadError::Uncertain),
                Err(error) => return Err(error.into()),
            };
            let node = match self.receipt_from_file(request, &id, file) {
                Ok(node) => node,
                Err(_) => return Ok(Reconciliation::Conflict),
            };
            let mut digest = Sha256::new();
            let mut offset = 0;
            while offset < node.size {
                let bytes = self
                    .read_range(&request.scope, &node, offset, 4 * 1024 * 1024, cancel)
                    .await?;
                if bytes.is_empty() {
                    return Err(UploadError::Uncertain);
                }
                offset += bytes.len() as u64;
                digest.update(bytes);
            }
            if hex::encode(digest.finalize()) == request.sha256 {
                Ok(Reconciliation::Committed(node))
            } else {
                Ok(Reconciliation::Conflict)
            }
        })
        .await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use cirrove_core::upload::UploadProvider;
    use serde_json::Value;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    enum ExpectedBody {
        Json(Value),
        Bytes(Vec<u8>),
    }
    struct Exchange {
        method: &'static str,
        path: &'static str,
        query: Vec<(&'static str, &'static str)>,
        headers: Vec<String>,
        authorized: bool,
        body: Option<ExpectedBody>,
        status: u16,
        response_headers: String,
        response_body: Vec<u8>,
    }
    impl Exchange {
        fn json(method: &'static str, path: &'static str, status: u16, body: Value) -> Self {
            Self {
                method,
                path,
                query: vec![],
                headers: vec![],
                authorized: true,
                body: None,
                status,
                response_headers: String::new(),
                response_body: serde_json::to_vec(&body).unwrap(),
            }
        }
    }

    fn scope() -> Scope {
        Scope {
            account: "account".into(),
            provider: PROVIDER_ID.into(),
            collection: "root-id".into(),
        }
    }
    fn request(bytes: &[u8]) -> UploadRequest {
        UploadRequest {
            scope: scope(),
            intent: UploadIntent::Create {
                parent: "root-id".into(),
                name: "report.txt".into(),
            },
            size: bytes.len() as u64,
            sha256: hex::encode(Sha256::digest(bytes)),
        }
    }
    fn file(id: &str, size: usize) -> Value {
        json!({
            "id": id,
            "name": "report.txt",
            "mimeType": "application/octet-stream",
            "parents": ["root-id"],
            "size": size.to_string(),
            "version": "7",
            "capabilities": {"canDownload": true}
        })
    }
    fn metadata(id: &str) -> Value {
        json!({
            "id": id,
            "name": "report.txt",
            "parents": ["root-id"],
            "mimeType": "application/octet-stream"
        })
    }

    async fn fixture(
        build: impl FnOnce(std::net::SocketAddr) -> Vec<Exchange>,
    ) -> (GoogleDrive, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let exchanges = build(address);
        let task = tokio::spawn(async move {
            for exchange in exchanges {
                let (mut socket, _) =
                    tokio::time::timeout(Duration::from_secs(5), listener.accept())
                        .await
                        .unwrap()
                        .unwrap();
                let mut request = Vec::new();
                let header_end = loop {
                    let mut bytes = [0; 4096];
                    let read = socket.read(&mut bytes).await.unwrap();
                    assert!(read > 0);
                    request.extend_from_slice(&bytes[..read]);
                    assert!(request.len() < 2 * 1024 * 1024);
                    if let Some(position) =
                        request.windows(4).position(|window| window == b"\r\n\r\n")
                    {
                        break position + 4;
                    }
                };
                let head = String::from_utf8(request[..header_end].to_vec()).unwrap();
                let content_length = head
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                while request.len() - header_end < content_length {
                    let mut bytes = [0; 4096];
                    let read = socket.read(&mut bytes).await.unwrap();
                    assert!(read > 0);
                    request.extend_from_slice(&bytes[..read]);
                }
                let mut words = head.lines().next().unwrap().split_ascii_whitespace();
                assert_eq!(words.next(), Some(exchange.method));
                let target = words.next().unwrap();
                let url = Url::parse(&format!("http://fixture{target}")).unwrap();
                assert_eq!(url.path(), exchange.path);
                let query: std::collections::HashMap<_, _> = url.query_pairs().collect();
                for (name, value) in exchange.query {
                    assert_eq!(query.get(name).map(|value| value.as_ref()), Some(value));
                }
                let lower = head.to_ascii_lowercase();
                assert_eq!(
                    lower.contains("authorization: bearer synthetic-token"),
                    exchange.authorized
                );
                for header in exchange.headers {
                    assert!(
                        lower.contains(&header.to_ascii_lowercase()),
                        "missing {header}"
                    );
                }
                let request_body = &request[header_end..header_end + content_length];
                match exchange.body {
                    Some(ExpectedBody::Json(expected)) => {
                        assert_eq!(
                            serde_json::from_slice::<Value>(request_body).unwrap(),
                            expected
                        )
                    }
                    Some(ExpectedBody::Bytes(expected)) => assert_eq!(request_body, expected),
                    None => assert!(request_body.is_empty()),
                }
                let reply = format!(
                    "HTTP/1.1 {} Synthetic\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n",
                    exchange.status,
                    exchange.response_body.len(),
                    exchange.response_headers
                );
                socket.write_all(reply.as_bytes()).await.unwrap();
                socket.write_all(&exchange.response_body).await.unwrap();
            }
        });
        (
            GoogleDrive::build(
                "account".into(),
                "root-id".into(),
                Arc::new(cirrove_core::StaticToken(SecretString::from(
                    "synthetic-token",
                ))),
                Url::parse(&format!("http://{address}/drive/v3/")).unwrap(),
            )
            .unwrap(),
            task,
        )
    }

    #[tokio::test]
    async fn generated_identity_is_persisted_before_session_and_used_for_exact_create() {
        let bytes = b"abcdef";
        let generated = "generated-id";
        let (provider, server) = fixture(|address| {
            let mut generate = Exchange::json(
                "GET",
                "/drive/v3/files/generateIds",
                200,
                json!({"ids":[generated],"space":"drive","kind":"drive#generatedIds"}),
            );
            generate.query = vec![("count", "1"), ("space", "drive"), ("type", "files")];
            let missing = Exchange::json("GET", "/drive/v3/files/generated-id", 404, json!({}));
            let mut start = Exchange::json("POST", "/upload/drive/v3/files", 200, json!({}));
            start.query = vec![("uploadType", "resumable"), ("fields", files::FIELDS)];
            start.headers = vec![
                "x-upload-content-type: application/octet-stream".into(),
                "x-upload-content-length: 6".into(),
            ];
            start.body = Some(ExpectedBody::Json(metadata(generated)));
            start.response_body.clear();
            start.response_headers = format!(
                "Location: http://{address}/upload/drive/v3/files?upload_id=PRIVATE-SESSION\r\n"
            );
            let mut upload = Exchange::json(
                "PUT",
                "/upload/drive/v3/files",
                201,
                file(generated, bytes.len()),
            );
            upload.query = vec![("upload_id", "PRIVATE-SESSION")];
            upload.authorized = false;
            upload.headers = vec!["content-range: bytes 0-5/6".into()];
            upload.body = Some(ExpectedBody::Bytes(bytes.to_vec()));
            vec![generate, missing, start, upload]
        })
        .await;
        let request = request(bytes);
        let cancel = CancellationToken::new();
        let UploadStep::Prepared(prepared) =
            provider.begin_upload(&request, &cancel).await.unwrap()
        else {
            panic!("generated ID was not prepared")
        };
        assert!(prepared.expose_secret().contains(generated));
        assert!(!format!("{:?}", UploadStep::Prepared(prepared.clone())).contains(generated));
        let UploadStep::Continue(progress) = provider
            .inspect_upload(&request, &prepared, &cancel)
            .await
            .unwrap()
        else {
            panic!("session did not request content")
        };
        assert_eq!((progress.offset, progress.length), (0, 6));
        let UploadStep::Complete(node) = provider
            .upload_part(&request, &progress.checkpoint, 0, bytes.to_vec(), &cancel)
            .await
            .unwrap()
        else {
            panic!("final range was not acknowledged")
        };
        assert_eq!(node.id, generated);
        assert_eq!(node.name, "report.txt");
        assert_eq!(node.parent_id.as_deref(), Some("root-id"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn an_empty_file_uses_the_prepared_identity_without_a_session() {
        let generated = "generated-id";
        let (provider, server) = fixture(|_| {
            let generate = Exchange::json(
                "GET",
                "/drive/v3/files/generateIds",
                200,
                json!({"ids":[generated],"space":"drive","kind":"drive#generatedIds"}),
            );
            let missing = Exchange::json("GET", "/drive/v3/files/generated-id", 404, json!({}));
            let mut create = Exchange::json("POST", "/drive/v3/files", 200, file(generated, 0));
            create.query = vec![("fields", files::FIELDS)];
            create.body = Some(ExpectedBody::Json(metadata(generated)));
            vec![generate, missing, create]
        })
        .await;
        let request = request(b"");
        let cancel = CancellationToken::new();
        let UploadStep::Prepared(prepared) =
            provider.begin_upload(&request, &cancel).await.unwrap()
        else {
            panic!("missing prepared ID")
        };
        let UploadStep::Complete(node) = provider
            .inspect_upload(&request, &prepared, &cancel)
            .await
            .unwrap()
        else {
            panic!("empty file did not complete")
        };
        assert_eq!((node.id.as_str(), node.size), (generated, 0));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn replacement_is_rejected_before_any_provider_request() {
        let (provider, server) = fixture(|_| vec![]).await;
        let request = UploadRequest {
            scope: scope(),
            intent: UploadIntent::Replace {
                item: "generated-id".into(),
                expected_etag: "etag".into(),
            },
            size: 6,
            sha256: hex::encode(Sha256::digest(b"abcdef")),
        };
        assert!(matches!(
            provider
                .begin_upload(&request, &CancellationToken::new())
                .await,
            Err(UploadError::Unsupported(_))
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_gone_session_returns_to_the_same_prepared_identity_without_its_old_url() {
        let generated = "generated-id";
        let bytes = b"abcdef";
        let (provider, server) = fixture(|address| {
            let mut generate = Exchange::json(
                "GET",
                "/drive/v3/files/generateIds",
                200,
                json!({"ids":[generated],"space":"drive","kind":"drive#generatedIds"}),
            );
            generate.query = vec![("count", "1")];
            let missing1 = Exchange::json("GET", "/drive/v3/files/generated-id", 404, json!({}));
            let mut start1 = Exchange::json("POST", "/upload/drive/v3/files", 200, json!({}));
            start1.body = Some(ExpectedBody::Json(metadata(generated)));
            start1.response_body.clear();
            start1.response_headers = format!(
                "Location: http://{address}/upload/drive/v3/files?upload_id=PRIVATE-ONE\r\n"
            );
            let mut gone = Exchange::json("PUT", "/upload/drive/v3/files", 404, json!({}));
            gone.query = vec![("upload_id", "PRIVATE-ONE")];
            gone.authorized = false;
            gone.headers = vec!["content-range: bytes */6".into()];
            gone.body = Some(ExpectedBody::Bytes(vec![]));
            let missing2 = Exchange::json("GET", "/drive/v3/files/generated-id", 404, json!({}));
            let mut start2 = Exchange::json("POST", "/upload/drive/v3/files", 200, json!({}));
            start2.body = Some(ExpectedBody::Json(metadata(generated)));
            start2.response_body.clear();
            start2.response_headers = format!(
                "Location: http://{address}/upload/drive/v3/files?upload_id=PRIVATE-TWO\r\n"
            );
            vec![generate, missing1, start1, gone, missing2, start2]
        })
        .await;
        let request = request(bytes);
        let cancel = CancellationToken::new();
        let UploadStep::Prepared(prepared) =
            provider.begin_upload(&request, &cancel).await.unwrap()
        else {
            panic!("missing prepared ID")
        };
        let UploadStep::Continue(session) = provider
            .inspect_upload(&request, &prepared, &cancel)
            .await
            .unwrap()
        else {
            panic!("missing first session")
        };
        let UploadStep::Prepared(restarted) = provider
            .inspect_upload(&request, &session.checkpoint, &cancel)
            .await
            .unwrap()
        else {
            panic!("gone session did not retain its identity")
        };
        assert!(restarted.expose_secret().contains(generated));
        assert!(!restarted.expose_secret().contains("PRIVATE-ONE"));
        let UploadStep::Continue(second) = provider
            .inspect_upload(&request, &restarted, &cancel)
            .await
            .unwrap()
        else {
            panic!("same identity did not start a replacement session")
        };
        assert!(second.checkpoint.expose_secret().contains("PRIVATE-TWO"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn status_uses_the_exact_server_offset_even_when_an_interrupted_chunk_is_partial() {
        let size = 20_000_000_u64;
        let (provider, server) = fixture(|_| {
            let mut status = Exchange::json("PUT", "/upload/drive/v3/files", 308, json!({}));
            status.query = vec![("upload_id", "PRIVATE-SESSION")];
            status.authorized = false;
            status.headers = vec![format!("content-range: bytes */{size}")];
            status.body = Some(ExpectedBody::Bytes(vec![]));
            status.response_headers = "Range: bytes=0-42\r\n".into();
            vec![status]
        })
        .await;
        let request = UploadRequest {
            scope: scope(),
            intent: UploadIntent::Create {
                parent: "root-id".into(),
                name: "report.txt".into(),
            },
            size,
            sha256: "0".repeat(64),
        };
        let session = SavedUpload::Session {
            version: 1,
            request: request.clone(),
            id: "generated-id".into(),
            url: format!(
                "{}/upload/drive/v3/files?upload_id=PRIVATE-SESSION",
                provider.endpoint.origin().ascii_serialization()
            ),
            offset: 0,
            length: PART_SIZE,
        };
        let checkpoint = SecretString::from(serde_json::to_string(&session).unwrap());
        let UploadStep::Continue(progress) = provider
            .inspect_upload(&request, &checkpoint, &CancellationToken::new())
            .await
            .unwrap()
        else {
            panic!("status did not return a range")
        };
        assert_eq!(progress.offset, 43);
        assert_eq!(progress.length, PART_SIZE);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_chunk_reply_cannot_advance_past_the_bytes_that_were_sent() {
        let size = ALIGNMENT + 10;
        let bytes = vec![b'x'; ALIGNMENT as usize];
        let (provider, server) = fixture(|_| {
            let mut reply = Exchange::json("PUT", "/upload/drive/v3/files", 308, json!({}));
            reply.query = vec![("upload_id", "PRIVATE-SESSION")];
            reply.authorized = false;
            reply.headers = vec![format!("content-range: bytes 0-{}/{size}", ALIGNMENT - 1)];
            reply.body = Some(ExpectedBody::Bytes(bytes.clone()));
            reply.response_headers = format!("Range: bytes=0-{ALIGNMENT}\r\n");
            vec![reply]
        })
        .await;
        let request = UploadRequest {
            scope: scope(),
            intent: UploadIntent::Create {
                parent: "root-id".into(),
                name: "report.txt".into(),
            },
            size,
            sha256: "0".repeat(64),
        };
        let session = SavedUpload::Session {
            version: 1,
            request: request.clone(),
            id: "generated-id".into(),
            url: format!(
                "{}/upload/drive/v3/files?upload_id=PRIVATE-SESSION",
                provider.endpoint.origin().ascii_serialization()
            ),
            offset: 0,
            length: ALIGNMENT as u32,
        };
        let checkpoint = SecretString::from(serde_json::to_string(&session).unwrap());
        assert!(matches!(
            provider
                .upload_part(&request, &checkpoint, 0, bytes, &CancellationToken::new())
                .await,
            Err(UploadError::Uncertain)
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn reconciliation_reads_back_the_exact_generated_identity_and_hash() {
        let generated = "generated-id";
        let bytes = b"abcdef";
        let (provider, server) = fixture(|_| {
            let mut generate = Exchange::json(
                "GET",
                "/drive/v3/files/generateIds",
                200,
                json!({"ids":[generated],"space":"drive","kind":"drive#generatedIds"}),
            );
            generate.query = vec![("count", "1")];
            let receipt = Exchange::json(
                "GET",
                "/drive/v3/files/generated-id",
                200,
                file(generated, bytes.len()),
            );
            let before = Exchange::json(
                "GET",
                "/drive/v3/files/generated-id",
                200,
                file(generated, bytes.len()),
            );
            let mut content = Exchange::json("GET", "/drive/v3/files/generated-id", 206, json!({}));
            content.query = vec![("alt", "media")];
            content.headers = vec!["range: bytes=0-5".into()];
            content.response_headers = "Content-Range: bytes 0-5/6\r\n".into();
            content.response_body = bytes.to_vec();
            let after = Exchange::json(
                "GET",
                "/drive/v3/files/generated-id",
                200,
                file(generated, bytes.len()),
            );
            vec![generate, receipt, before, content, after]
        })
        .await;
        let request = request(bytes);
        let cancel = CancellationToken::new();
        let UploadStep::Prepared(prepared) =
            provider.begin_upload(&request, &cancel).await.unwrap()
        else {
            panic!("missing prepared ID")
        };
        let Reconciliation::Committed(node) = provider
            .reconcile_upload(&request, Some(&prepared), &cancel)
            .await
            .unwrap()
        else {
            panic!("exact content was not reconciled")
        };
        assert_eq!(node.id, generated);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_foreign_session_location_is_rejected_without_following_it() {
        let generated = "generated-id";
        let (provider, server) = fixture(|_| {
            let generate = Exchange::json(
                "GET",
                "/drive/v3/files/generateIds",
                200,
                json!({"ids":[generated],"space":"drive","kind":"drive#generatedIds"}),
            );
            let missing = Exchange::json("GET", "/drive/v3/files/generated-id", 404, json!({}));
            let mut start = Exchange::json("POST", "/upload/drive/v3/files", 200, json!({}));
            start.body = Some(ExpectedBody::Json(metadata(generated)));
            start.response_body.clear();
            start.response_headers =
                "Location: https://evil.invalid/upload/drive/v3/files?private=session\r\n".into();
            vec![generate, missing, start]
        })
        .await;
        let request = request(b"abcdef");
        let cancel = CancellationToken::new();
        let UploadStep::Prepared(prepared) =
            provider.begin_upload(&request, &cancel).await.unwrap()
        else {
            panic!("missing prepared ID")
        };
        assert!(matches!(
            provider.inspect_upload(&request, &prepared, &cancel).await,
            Err(UploadError::Provider(ProviderError::Protocol(_)))
        ));
        server.await.unwrap();
    }
}
