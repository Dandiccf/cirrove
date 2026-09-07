//! Microsoft resumable transfers. Preauthenticated session URLs never receive
//! Graph bearer headers and never appear in errors or Debug output.
#[cfg(test)]
mod tests;
use super::{DriveItem, OneDrive, map_item};
use async_trait::async_trait;
use cirrove_core::upload::{
    Reconciliation, Result, UploadError, UploadIntent, UploadProgress, UploadProvider,
    UploadRequest, UploadStep,
};
use cirrove_core::{
    CancellationToken, Change, Node, NodeKind, Priority, ProviderError, ReadProvider, Scope,
};
use reqwest::{Method, Response, StatusCode, Url};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    future::Future,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::time::Instant;

// Sixteen 320 KiB units: below Graph's 60 MiB maximum and bounded in memory.
const ALIGNMENT: u64 = 320 * 1024;
const PART_SIZE: u32 = 5 * 1024 * 1024;
const MAX_RESPONSE: usize = 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct Session {
    version: u8,
    request: UploadRequest,
    url: String,
    expires_at: u64,
    offset: u64,
    length: u32,
    commit_target: Option<(String, String)>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionReply {
    upload_url: Option<String>,
    expiration_date_time: String,
    next_expected_ranges: Option<Vec<String>>,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn protocol(message: &'static str) -> UploadError {
    ProviderError::Protocol(message).into()
}
fn expiry(value: &str) -> Result<u64> {
    let result = chrono::DateTime::parse_from_rfc3339(value)
        .map_err(|_| protocol("invalid upload session expiry"))?
        .timestamp();
    let result = u64::try_from(result).map_err(|_| UploadError::SessionGone)?;
    if result <= now() {
        return Err(UploadError::SessionGone);
    }
    Ok(result)
}
/// Treat ranges as missing data, not as recommended chunk sizes. Validate every
/// range and use the earliest missing span, respecting Graph's fragment alignment.
fn next_range(ranges: &[String], size: u64) -> Result<(u64, u32)> {
    if ranges.is_empty() || ranges.len() > 1024 || size == 0 {
        return Err(protocol("invalid upload ranges"));
    }
    let mut spans = Vec::with_capacity(ranges.len());
    for range in ranges {
        let (start, end) = range
            .split_once('-')
            .ok_or_else(|| protocol("invalid upload range"))?;
        let start = start
            .parse::<u64>()
            .map_err(|_| protocol("invalid upload range start"))?;
        let end = if end.is_empty() {
            size - 1
        } else {
            end.parse::<u64>()
                .map_err(|_| protocol("invalid upload range end"))?
        };
        if start > end || end >= size {
            return Err(protocol("upload range exceeds snapshot"));
        }
        spans.push((start, end));
    }
    spans.sort_unstable();
    if spans.windows(2).any(|s| s[0].1 >= s[1].0) {
        return Err(protocol("overlapping upload ranges"));
    }
    let (offset, end) = spans[0];
    let mut length = (end - offset + 1).min(PART_SIZE as u64);
    if offset + length < size {
        length -= length % ALIGNMENT;
    }
    if length == 0 || offset % ALIGNMENT != 0 {
        return Err(protocol("unaligned upload range"));
    }
    Ok((offset, length as u32))
}
impl OneDrive {
    /// Fail on name collision. An uncertain response must be reconciled by the
    /// caller; this method never retries a folder creation after a lost response.
    pub async fn create_folder(
        &self,
        scope: &Scope,
        parent: &str,
        name: &str,
        cancel: &CancellationToken,
    ) -> Result<Node> {
        UploadIntent::Create {
            parent: parent.into(),
            name: name.into(),
        }
        .validate()?;
        if scope.account != self.account
            || scope.provider != "onedrive"
            || scope.collection.is_empty()
        {
            return Err(UploadError::Invalid);
        }
        self.upload_call(cancel, async {
            let url =
                self.resource_url(&["drives", &scope.collection, "items", parent, "children"])?;
            let response = self
                .authorized_upload(
                    Method::POST,
                    url,
                    Some(json!({
                        "name": name, "folder": {}, "@microsoft.graph.conflictBehavior": "fail"
                    })),
                    None,
                )
                .await?;
            let (status, bytes) = self.upload_body(response, false).await?;
            if status != StatusCode::CREATED {
                return Err(UploadError::Uncertain);
            }
            let item: DriveItem =
                serde_json::from_slice(&bytes).map_err(|_| UploadError::Uncertain)?;
            if item
                .parent_reference
                .as_ref()
                .and_then(|p| p.drive_id.as_deref())
                != Some(scope.collection.as_str())
            {
                return Err(UploadError::Uncertain);
            }
            match map_item(item)? {
                Change::Upsert(node)
                    if node.kind == NodeKind::Folder
                        && node.name == name
                        && node.parent_id.as_deref() == Some(parent)
                        && !node.id.is_empty() =>
                {
                    Ok(node)
                }
                _ => Err(UploadError::Uncertain),
            }
        })
        .await
    }
    fn check_upload(&self, request: &UploadRequest) -> Result<()> {
        request.validate()?;
        if request.scope.account != self.account || request.scope.provider != "onedrive" {
            return Err(UploadError::Invalid);
        }
        Ok(())
    }
    fn session_url(&self, value: &str) -> Result<Url> {
        let url = Url::parse(value).map_err(|_| protocol("invalid upload session URL"))?;
        if !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || url.host_str().is_none()
            || (url.scheme() != "https"
                && !(self.endpoint.scheme() == "http" && url.origin() == self.endpoint.origin()))
        {
            return Err(protocol("unsafe upload session URL"));
        }
        Ok(url)
    }
    fn load_session(&self, request: &UploadRequest, checkpoint: &SecretString) -> Result<Session> {
        if checkpoint.expose_secret().len() > MAX_RESPONSE {
            return Err(UploadError::CheckpointInvalid);
        }
        let session: Session = serde_json::from_str(checkpoint.expose_secret())
            .map_err(|_| UploadError::CheckpointInvalid)?;
        if session.version != 1 || &session.request != request {
            return Err(UploadError::CheckpointInvalid);
        }
        self.session_url(&session.url)
            .map_err(|_| UploadError::CheckpointInvalid)?;
        if session.expires_at <= now() {
            return Err(UploadError::SessionGone);
        }
        let ready = session.commit_target.is_some()
            && session.offset == request.size
            && session.length == 0;
        if !ready
            && (session.length == 0
                || session.length > PART_SIZE
                || session.offset >= request.size
                || session.length as u64 > request.size - session.offset
                || !session.offset.is_multiple_of(ALIGNMENT)
                || (session.offset + (session.length as u64) < request.size
                    && !(session.length as u64).is_multiple_of(ALIGNMENT)))
        {
            return Err(UploadError::CheckpointInvalid);
        }
        Ok(session)
    }
    pub(super) async fn upload_call<T>(
        &self,
        cancel: &CancellationToken,
        call: impl Future<Output = Result<T>>,
    ) -> Result<T> {
        let _permit = self.budget.acquire(Priority::Upload, cancel).await?;
        if let Some(deadline) = *self.cooldown.lock().await {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if !remaining.is_zero() {
                return Err(ProviderError::Throttled(remaining).into());
            }
        }
        tokio::select! {biased;
            _=cancel.cancelled()=>Err(UploadError::Uncertain),
            result=tokio::time::timeout(Duration::from_secs(125),call)=>result.unwrap_or(Err(UploadError::Uncertain)),
        }
    }
    pub(super) async fn authorized_upload(
        &self,
        method: Method,
        url: Url,
        body: Option<serde_json::Value>,
        etag: Option<&str>,
    ) -> Result<Response> {
        // A definite 401 can refresh once; transport failures are never retried here.
        for retry in 0..2 {
            let token = self.tokens.access_token().await?;
            let mut builder = self
                .client
                .request(method.clone(), url.clone())
                .bearer_auth(token.expose_secret());
            if let Some(etag) = etag {
                builder = builder.header("If-Match", etag);
            }
            if let Some(body) = &body {
                builder = builder.json(body);
            } else {
                builder = builder
                    .header("Content-Type", "application/octet-stream")
                    .header("Content-Length", "0")
                    .body(Vec::<u8>::new());
            }
            let response = builder.send().await.map_err(|_| UploadError::Uncertain)?;
            if response.status() != StatusCode::UNAUTHORIZED || retry == 1 {
                return Ok(response);
            }
            self.tokens.invalidate(&token).await;
        }
        Err(ProviderError::Authentication.into())
    }
    pub(super) async fn upload_body(
        &self,
        mut response: Response,
        session: bool,
    ) -> Result<(StatusCode, Vec<u8>)> {
        let status = response.status();
        match status {
            StatusCode::OK | StatusCode::CREATED | StatusCode::ACCEPTED => (),
            StatusCode::CONFLICT | StatusCode::PRECONDITION_FAILED => {
                return Err(UploadError::Conflict);
            }
            StatusCode::INSUFFICIENT_STORAGE => return Err(UploadError::Quota),
            StatusCode::LOCKED => return Err(UploadError::Locked),
            StatusCode::NOT_FOUND | StatusCode::GONE if session => {
                return Err(UploadError::SessionGone);
            }
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN if session => {
                return Err(UploadError::SessionGone);
            }
            StatusCode::UNAUTHORIZED => return Err(ProviderError::Authentication.into()),
            StatusCode::FORBIDDEN => return Err(ProviderError::Permission.into()),
            StatusCode::NOT_FOUND => return Err(ProviderError::NotFound.into()),
            StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => {
                return Err(self.throttle(&response).await?.into());
            }
            StatusCode::BAD_REQUEST => return Err(UploadError::Invalid),
            // An interrupted final response, 416, redirect or unexpected status
            // cannot establish that a mutation did not commit.
            _ => return Err(UploadError::UnexpectedStatus(status.as_u16())),
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| UploadError::Uncertain)? {
            if chunk.len() > MAX_RESPONSE.saturating_sub(bytes.len()) {
                return Err(UploadError::Uncertain);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok((status, bytes))
    }
    fn progress(
        &self,
        request: &UploadRequest,
        reply: SessionReply,
        existing: Option<&Session>,
        initial: bool,
        commit_target: Option<(String, String)>,
        accepted_final: bool,
    ) -> Result<UploadStep> {
        let url = match (reply.upload_url, existing) {
            (Some(url), None) => url,
            (Some(url), Some(old)) if url == old.url => url,
            (None, Some(old)) => old.url.clone(),
            _ => return Err(protocol("upload session URL changed or missing")),
        };
        self.session_url(&url)?;
        let ranges = match reply.next_expected_ranges {
            Some(ranges) => ranges,
            None if initial => vec!["0-".into()],
            None if accepted_final && commit_target.is_some() => vec![],
            None => return Err(protocol("upload status has no ranges")),
        };
        let ready = commit_target.is_some()
            && !initial
            && (ranges.is_empty() || ranges == [format!("{}-", request.size)]);
        let (offset, length) = if ready {
            (request.size, 0)
        } else {
            next_range(&ranges, request.size)?
        };
        if initial && offset != 0 {
            return Err(protocol("new upload session did not start at zero"));
        }
        let checkpoint = Session {
            version: 1,
            request: request.clone(),
            url,
            expires_at: expiry(&reply.expiration_date_time)?,
            offset,
            length,
            commit_target,
        };
        let checkpoint = SecretString::from(
            serde_json::to_string(&checkpoint).map_err(|_| UploadError::Invalid)?,
        );
        if ready {
            Ok(UploadStep::Commit(checkpoint))
        } else {
            Ok(UploadStep::Continue(UploadProgress {
                checkpoint,
                offset,
                length,
            }))
        }
    }
    fn receipt(&self, request: &UploadRequest, bytes: &[u8]) -> Result<Node> {
        let item: DriveItem = serde_json::from_slice(bytes).map_err(|_| UploadError::Uncertain)?;
        if item.size != Some(request.size)
            || item
                .parent_reference
                .as_ref()
                .and_then(|p| p.drive_id.as_deref())
                != Some(request.scope.collection.as_str())
        {
            return Err(UploadError::Uncertain);
        }
        let Change::Upsert(node) = map_item(item).map_err(|_| UploadError::Uncertain)? else {
            return Err(UploadError::Uncertain);
        };
        let matches = match &request.intent {
            UploadIntent::Create { parent, name } => {
                node.parent_id.as_deref() == Some(parent.as_str()) && &node.name == name
            }
            UploadIntent::Replace { item, .. } => &node.id == item,
        };
        if !matches || node.kind != NodeKind::File || node.content_revision().is_none() {
            return Err(UploadError::Uncertain);
        }
        Ok(node)
    }
    fn upload_target(&self, request: &UploadRequest, action: &str) -> Result<Url> {
        Ok(match &request.intent {
            UploadIntent::Create { parent, name } => self.resource_url(&[
                "drives",
                &request.scope.collection,
                "items",
                &format!("{parent}:"),
                &format!("{name}:"),
                action,
            ])?,
            UploadIntent::Replace { item, .. } => {
                self.resource_url(&["drives", &request.scope.collection, "items", item, action])?
            }
        })
    }
}

#[async_trait]
impl UploadProvider for OneDrive {
    async fn begin_upload(
        &self,
        request: &UploadRequest,
        cancel: &CancellationToken,
    ) -> Result<UploadStep> {
        self.check_upload(request)?;
        self.upload_call(cancel, async {
            let etag = match &request.intent {
                UploadIntent::Replace { expected_etag, .. } => Some(expected_etag.as_str()),
                _ => None,
            };
            if request.size == 0 {
                let mut url = self.upload_target(request, "content")?;
                if matches!(request.intent, UploadIntent::Create { .. }) {
                    url.query_pairs_mut().append_pair("@microsoft.graph.conflictBehavior", "fail");
                }
                let response = self.authorized_upload(Method::PUT, url, None, etag).await?;
                let (status, bytes) = self.upload_body(response, false).await?;
                if status == StatusCode::ACCEPTED {
                    return Err(UploadError::Uncertain);
                }
                return Ok(UploadStep::Complete(self.receipt(request, &bytes)?));
            }
            let commit_target = if let UploadIntent::Replace { item, expected_etag } = &request.intent {
                let bytes = self.request_bytes(self.resource_url(&["drives", &request.scope.collection, "items", item])?).await?;
                let current: DriveItem = serde_json::from_slice(&bytes)
                    .map_err(|_| protocol("invalid replacement metadata"))?;
                if current.id != *item || current.e_tag.as_ref() != Some(expected_etag) || current.file.is_none() {
                    return Err(UploadError::Conflict);
                }
                let parent = current.parent_reference.and_then(|p| p.id)
                    .ok_or_else(|| protocol("replacement has no parent"))?;
                let name = current.name.ok_or_else(|| protocol("replacement has no name"))?;
                Some((parent, name))
            } else {
                None
            };
            let body = match &request.intent {
                UploadIntent::Create { name, .. } => json!({"item":{"name":name,"@microsoft.graph.conflictBehavior":"fail"}}),
                UploadIntent::Replace { .. } => json!({"deferCommit":true,"item":{"@microsoft.graph.conflictBehavior":"replace"}}),
            };
            let response = self.authorized_upload(Method::POST, self.upload_target(request, "createUploadSession")?, Some(body), etag).await?;
            let (status, bytes) = self.upload_body(response, false).await?;
            if status != StatusCode::OK {
                return Err(UploadError::Uncertain);
            }
            let reply = serde_json::from_slice(&bytes).map_err(|_| UploadError::Uncertain)?;
            self.progress(request, reply, None, true, commit_target, false)
        }).await
    }
    async fn inspect_upload(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
        cancel: &CancellationToken,
    ) -> Result<UploadStep> {
        self.check_upload(request)?;
        let session = self.load_session(request, checkpoint)?;
        self.upload_call(cancel, async {
            let response = self
                .uploads
                .get(self.session_url(&session.url)?)
                .send()
                .await
                .map_err(|_| UploadError::Uncertain)?;
            let (status, bytes) = self.upload_body(response, true).await?;
            if status != StatusCode::OK {
                return Err(UploadError::Uncertain);
            }
            let reply = serde_json::from_slice(&bytes).map_err(|_| UploadError::Uncertain)?;
            self.progress(
                request,
                reply,
                Some(&session),
                false,
                session.commit_target.clone(),
                false,
            )
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
        let session = self.load_session(request, checkpoint)?;
        if session.length == 0 || offset != session.offset || bytes.len() != session.length as usize
        {
            return Err(UploadError::Invalid);
        }
        let end = offset + bytes.len() as u64;
        self.upload_call(cancel, async {
            let response = self
                .uploads
                .put(self.session_url(&session.url)?)
                .header(
                    "Content-Range",
                    format!("bytes {offset}-{}/{}", end - 1, request.size),
                )
                .header("Content-Type", "application/octet-stream")
                .body(bytes)
                .send()
                .await
                .map_err(|_| UploadError::Uncertain)?;
            let (status, body) = self.upload_body(response, true).await?;
            if status == StatusCode::ACCEPTED {
                let reply = serde_json::from_slice(&body).map_err(|_| UploadError::Uncertain)?;
                let next = self.progress(
                    request,
                    reply,
                    Some(&session),
                    false,
                    session.commit_target.clone(),
                    end == request.size,
                )?;
                if let UploadStep::Continue(progress) = &next
                    && progress.offset < end
                {
                    return Err(UploadError::Uncertain);
                }
                Ok(next)
            } else if session.commit_target.is_some() {
                // Deferral was part of the request contract. Do not label an
                // unexpected automatic replacement as a verified success.
                Err(UploadError::Unsupported(
                    "deferred replacement was committed automatically",
                ))
            } else {
                Ok(UploadStep::Complete(self.receipt(request, &body)?))
            }
        })
        .await
    }
    async fn commit_upload(
        &self,
        request: &UploadRequest,
        checkpoint: &SecretString,
        cancel: &CancellationToken,
    ) -> Result<UploadStep> {
        self.check_upload(request)?;
        let session = self.load_session(request, checkpoint)?;
        let UploadIntent::Replace { expected_etag, .. } = &request.intent else {
            return Err(UploadError::Invalid);
        };
        let (parent, name) = session.commit_target.as_ref().ok_or(UploadError::Invalid)?;
        if session.offset != request.size || session.length != 0 {
            return Err(UploadError::Invalid);
        }
        self.upload_call(cancel, async {
            // The drive type, not the account's primary drive, determines the
            // commit endpoint (a linked library may have different semantics).
            let drive = self.drive(&request.scope.collection, cancel).await?;
            let (response, session_response) = match drive.drive_type.as_str() {
                "business" | "documentLibrary" => {
                    let response = self.uploads.post(self.session_url(&session.url)?)
                        .header("Content-Length", "0")
                        .header("If-Match", expected_etag)
                        .body(Vec::<u8>::new()).send().await.map_err(|_| UploadError::Uncertain)?;
                    (response, true)
                }
                "personal" => {
                    let url = self.resource_url(&["drives", &request.scope.collection, "items", &format!("{parent}:"), name])?;
                    let body = json!({"name":name,"@microsoft.graph.conflictBehavior":"replace","@microsoft.graph.sourceUrl":session.url});
                    let response = self.authorized_upload(Method::PUT, url, Some(body), Some(expected_etag)).await?;
                    (response, false)
                }
                _ => return Err(UploadError::Unsupported("unknown drive commit protocol")),
            };
            // Original ETag is sent at both session creation and final commit.
            // A real competing-edit check remains essential: do not infer that
            // an accepted header necessarily provides atomic conflict protection.
            let (status, bytes) = self.upload_body(response, session_response).await?;
            if !matches!(status, StatusCode::OK | StatusCode::CREATED) {
                return Err(UploadError::UnexpectedStatus(status.as_u16()));
            }
            Ok(UploadStep::Complete(self.receipt(request, &bytes)?))
        }).await
    }
    async fn reconcile_upload(
        &self,
        request: &UploadRequest,
        cancel: &CancellationToken,
    ) -> Result<Reconciliation> {
        self.check_upload(request)?;
        let node_result = match &request.intent {
            UploadIntent::Create { parent, name } => {
                let wire: std::result::Result<DriveItem, ProviderError> = self
                    .resource(
                        &[
                            "drives",
                            &request.scope.collection,
                            "items",
                            &format!("{parent}:"),
                            name,
                        ],
                        cancel,
                    )
                    .await;
                wire.and_then(|wire| match map_item(wire)? {
                    Change::Upsert(node) => Ok(node),
                    _ => Err(ProviderError::NotFound),
                })
            }
            UploadIntent::Replace { item, .. } => self.node(&request.scope, item, cancel).await,
        };
        let node = match node_result {
            Ok(node) => node,
            Err(ProviderError::NotFound) => {
                return Ok(if matches!(request.intent, UploadIntent::Create { .. }) {
                    Reconciliation::Uncommitted
                } else {
                    Reconciliation::Conflict
                });
            }
            Err(error) => return Err(error.into()),
        };
        if let UploadIntent::Replace { expected_etag, .. } = &request.intent
            && node.etag.as_ref() == Some(expected_etag)
        {
            return Ok(Reconciliation::Uncommitted);
        }
        if node.kind != NodeKind::File || node.size != request.size {
            return Ok(Reconciliation::Conflict);
        }
        // Only uncertain completions require a streamed read-back. Matching size
        // alone could acknowledge somebody else's concurrent edit.
        let mut hash = Sha256::new();
        let mut offset = 0;
        let deadline = Instant::now() + Duration::from_secs(15 * 60);
        while offset < node.size {
            let bytes = tokio::time::timeout_at(
                deadline,
                self.read_range(&request.scope, &node, offset, 4 * 1024 * 1024, cancel),
            )
            .await
            .map_err(|_| UploadError::Uncertain)??;
            if bytes.is_empty() {
                return Err(UploadError::Uncertain);
            }
            offset += bytes.len() as u64;
            hash.update(&bytes);
        }
        // Recheck metadata even for empty files, and retain the matching content
        // version in the receipt instead of acknowledging an intervening edit.
        let after = self.node(&request.scope, &node.id, cancel).await?;
        if after.content_revision() != node.content_revision() || after.size != node.size {
            return Err(UploadError::Uncertain);
        }
        if hex::encode(hash.finalize()) == request.sha256 {
            Ok(Reconciliation::Committed(after))
        } else {
            Ok(Reconciliation::Conflict)
        }
    }
}
