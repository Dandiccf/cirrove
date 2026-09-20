use super::*;
use reqwest::{Response, StatusCode};
use secrecy::ExposeSecret;
use serde::de::DeserializeOwned;

impl GoogleDrive {
    pub(super) async fn response(
        &self,
        url: Url,
        range: Option<(u64, u64)>,
    ) -> Result<Response, ProviderError> {
        if let Some(until) = *self.cooldown.lock().await
            && until > Instant::now()
        {
            return Err(ProviderError::Throttled(until - Instant::now()));
        }
        let mut response = None;
        for attempt in 0..2 {
            let token = self.tokens.access_token().await?;
            let mut request = self
                .client
                .get(url.clone())
                .bearer_auth(token.expose_secret())
                .header("Accept-Encoding", "identity");
            if let Some((start, end)) = range {
                request = request.header("Range", format!("bytes={start}-{end}"));
            }
            let reply = request
                .send()
                .await
                .map_err(|_| ProviderError::Unavailable)?;
            if reply.status() == StatusCode::UNAUTHORIZED && attempt == 0 {
                self.tokens.invalidate(&token).await;
                continue;
            }
            response = Some(reply);
            break;
        }
        let response = response.ok_or(ProviderError::Authentication)?;
        match response.status() {
            StatusCode::OK | StatusCode::PARTIAL_CONTENT => Ok(response),
            StatusCode::UNAUTHORIZED => Err(ProviderError::Authentication),
            StatusCode::NOT_FOUND => Err(ProviderError::NotFound),
            StatusCode::GONE => Err(ProviderError::CursorExpired),
            // Google tells clients to restart a listing when its page token is
            // rejected. The endpoint/query are ours, not a provider-supplied URL.
            StatusCode::BAD_REQUEST if url.query_pairs().any(|(key, _)| key == "pageToken") => {
                Err(ProviderError::CursorExpired)
            }
            StatusCode::PRECONDITION_FAILED | StatusCode::RANGE_NOT_SATISFIABLE => {
                Err(ProviderError::VersionChanged)
            }
            StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => {
                let delay = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| {
                        s.parse::<u64>().ok().map(Duration::from_secs).or_else(|| {
                            httpdate::parse_http_date(s).ok().map(|d| {
                                d.duration_since(std::time::SystemTime::now())
                                    .unwrap_or_default()
                            })
                        })
                    })
                    .unwrap_or(Duration::from_secs(30))
                    .clamp(Duration::from_secs(1), Duration::from_secs(86400));
                self.throttle(delay).await
            }
            // Drive also reports quota exhaustion as 403. Inspect only the bounded
            // structured reason, never return or log the body or its message.
            StatusCode::FORBIDDEN => {
                #[derive(serde::Deserialize)]
                struct Reason {
                    reason: String,
                }
                #[derive(serde::Deserialize)]
                struct Detail {
                    #[serde(default)]
                    errors: Vec<Reason>,
                }
                #[derive(serde::Deserialize)]
                struct ErrorBody {
                    error: Detail,
                }
                let limited = body(response, 64 * 1024).await?;
                let quota = serde_json::from_slice::<ErrorBody>(&limited)
                    .ok()
                    .is_some_and(|e| {
                        e.error.errors.iter().any(|e| {
                            matches!(
                                e.reason.as_str(),
                                "rateLimitExceeded"
                                    | "userRateLimitExceeded"
                                    | "sharingRateLimitExceeded"
                                    | "downloadQuotaExceeded"
                            )
                        })
                    });
                if quota {
                    self.throttle(Duration::from_secs(60)).await
                } else {
                    Err(ProviderError::Permission)
                }
            }
            status if status.is_server_error() => Err(ProviderError::Unavailable),
            _ => Err(ProviderError::Protocol("Google request refused")),
        }
    }
    async fn throttle(&self, delay: Duration) -> Result<Response, ProviderError> {
        let until = Instant::now() + delay;
        let mut cooldown = self.cooldown.lock().await;
        *cooldown = Some(cooldown.map_or(until, |old| old.max(until)));
        Err(ProviderError::Throttled(delay))
    }
    pub(super) async fn json<T: DeserializeOwned>(&self, url: Url) -> Result<T, ProviderError> {
        let response = self.response(url, None).await?;
        if response.status() != StatusCode::OK {
            return Err(ProviderError::Protocol("partial metadata response"));
        }
        serde_json::from_slice(&body(response, MAX_PAGE_BYTES).await?)
            .map_err(|_| ProviderError::Protocol("invalid Google metadata"))
    }
}
pub(super) async fn body(mut response: Response, limit: usize) -> Result<Vec<u8>, ProviderError> {
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err(ProviderError::Protocol("Google response exceeds limit"));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| ProviderError::Unavailable)?
    {
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            return Err(ProviderError::Protocol("Google response exceeds limit"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
