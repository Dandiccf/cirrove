use super::*;
use reqwest::{Method, Response, StatusCode};
use secrecy::ExposeSecret;
use serde::de::DeserializeOwned;

impl GoogleDrive {
    pub(super) async fn response(
        &self,
        url: Url,
        range: Option<(u64, u64)>,
    ) -> Result<Response, ProviderError> {
        self.response_inner(url, Method::GET, range, &self.client, true, false)
            .await
    }

    pub(super) async fn post(&self, url: Url) -> Result<Response, ProviderError> {
        self.response_inner(url, Method::POST, None, &self.client, true, false)
            .await
    }

    fn valid_download_origin(&self, url: &Url) -> bool {
        if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
            return false;
        }
        // A synthetic provider may download only from its own loopback origin.
        if self.endpoint.scheme() == "http" {
            return url.origin() == self.endpoint.origin();
        }
        let Some(host) = url.host_str() else {
            return false;
        };
        url.scheme() == "https"
            && url.port().is_none_or(|port| port == 443)
            && ["google.com", "googleapis.com", "googleusercontent.com"]
                .iter()
                .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
    }

    pub(super) async fn download_response(&self, mut url: Url) -> Result<Response, ProviderError> {
        let mut send_auth = true;
        for _ in 0..5 {
            if !self.valid_download_origin(&url) {
                return Err(ProviderError::Permission);
            }
            let response = self
                .response_inner(
                    url,
                    Method::GET,
                    None,
                    &self.download_client,
                    send_auth,
                    true,
                )
                .await?;
            if !response.status().is_redirection() {
                return Ok(response);
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or(ProviderError::Protocol(
                    "Google download redirect lacks location",
                ))?;
            url = Url::parse(location)
                .map_err(|_| ProviderError::Protocol("invalid Google download redirect"))?;
            // A redirect target may be a signed URL, never send the bearer on it.
            send_auth = false;
        }
        Err(ProviderError::Protocol(
            "too many Google download redirects",
        ))
    }

    async fn response_inner(
        &self,
        url: Url,
        method: Method,
        range: Option<(u64, u64)>,
        client: &Client,
        send_auth: bool,
        allow_redirect: bool,
    ) -> Result<Response, ProviderError> {
        if let Some(until) = *self.cooldown.lock().await
            && until > Instant::now()
        {
            return Err(ProviderError::Throttled(until - Instant::now()));
        }
        let mut response = None;
        for attempt in 0..if send_auth { 2 } else { 1 } {
            let token = if send_auth {
                Some(self.tokens.access_token().await?)
            } else {
                None
            };
            let mut request = client
                .request(method.clone(), url.clone())
                .header("Accept-Encoding", "identity");
            if method == Method::POST {
                request = request.header("Content-Type", "application/json").body("");
            }
            if let Some(token) = &token {
                request = request.bearer_auth(token.expose_secret());
            }
            if let Some((start, end)) = range {
                request = request.header("Range", format!("bytes={start}-{end}"));
            }
            let reply = request
                .send()
                .await
                .map_err(|_| ProviderError::Unavailable)?;
            if reply.status() == StatusCode::UNAUTHORIZED
                && attempt == 0
                && let Some(token) = &token
            {
                self.tokens.invalidate(token).await;
                continue;
            }
            response = Some(reply);
            break;
        }
        let response = response.ok_or(ProviderError::Authentication)?;
        match response.status() {
            StatusCode::OK | StatusCode::PARTIAL_CONTENT => Ok(response),
            status if allow_redirect && status.is_redirection() => Ok(response),
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
