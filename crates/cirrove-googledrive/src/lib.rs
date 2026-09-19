//! Google Drive v3 adapter. Ordinary mounts select only its read path; upload
//! support is exercised against synthetic HTTP until the write policy is complete.
mod feed;
mod files;
#[cfg(test)]
mod tests;
mod transport;
mod upload;

use cirrove_core::{ProviderError, RequestBudget, Scope, TokenSource};
use reqwest::{Client, Url, redirect::Policy};
use std::{sync::Arc, time::Duration};
use tokio::{sync::Mutex, time::Instant};

pub const PROVIDER_ID: &str = "googledrive";
const MAX_PAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_TOKEN_BYTES: usize = 16 * 1024;

#[derive(Clone)]
pub struct GoogleDrive {
    account: String,
    collection: String,
    endpoint: Url,
    client: Client,
    tokens: Arc<dyn TokenSource>,
    budget: RequestBudget,
    cooldown: Arc<Mutex<Option<Instant>>>,
}
impl GoogleDrive {
    /// `collection` is the real My Drive root ID, resolved during connection.
    pub fn new(
        account: String,
        collection: String,
        tokens: Arc<dyn TokenSource>,
    ) -> Result<Self, ProviderError> {
        Self::build(
            account,
            collection,
            tokens,
            Url::parse("https://www.googleapis.com/drive/v3/")
                .map_err(|_| ProviderError::Protocol("Google endpoint"))?,
        )
    }
    fn build(
        account: String,
        collection: String,
        tokens: Arc<dyn TokenSource>,
        endpoint: Url,
    ) -> Result<Self, ProviderError> {
        valid_id(&collection)?;
        if account.is_empty() {
            return Err(ProviderError::Protocol("missing account identity"));
        }
        Ok(Self {
            account,
            collection,
            endpoint,
            tokens,
            budget: RequestBudget::default(),
            cooldown: Arc::new(Mutex::new(None)),
            client: Client::builder()
                .redirect(Policy::none())
                .retry(reqwest::retry::never())
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(30))
                .user_agent(concat!("Cirrove/", env!("CARGO_PKG_VERSION")))
                .build()
                .map_err(|_| ProviderError::Unavailable)?,
        })
    }
    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn synthetic_loopback(
        account: String,
        collection: String,
        endpoint: &str,
    ) -> Result<Self, ProviderError> {
        let endpoint = Url::parse(endpoint)
            .map_err(|_| ProviderError::Protocol("invalid synthetic endpoint"))?;
        if endpoint.scheme() != "http"
            || !endpoint
                .host_str()
                .and_then(|h| h.parse::<std::net::Ipv4Addr>().ok())
                .is_some_and(|ip| ip.is_loopback())
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(ProviderError::Protocol(
                "synthetic endpoint must be HTTP IPv4 loopback",
            ));
        }
        Self::build(
            account,
            collection,
            Arc::new(cirrove_core::StaticToken(secrecy::SecretString::from(
                "synthetic-google-token",
            ))),
            endpoint,
        )
    }
    fn check_scope(&self, scope: &Scope) -> Result<(), ProviderError> {
        if scope.account != self.account
            || scope.provider != PROVIDER_ID
            || scope.collection != self.collection
        {
            return Err(ProviderError::Permission);
        }
        Ok(())
    }
    fn url(&self, segments: &[&str]) -> Result<Url, ProviderError> {
        let mut url = self.endpoint.clone();
        url.path_segments_mut()
            .map_err(|_| ProviderError::Protocol("Google endpoint"))?
            .pop_if_empty()
            .extend(segments);
        Ok(url)
    }
}
fn valid_id(id: &str) -> Result<(), ProviderError> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(ProviderError::Protocol("invalid Google item identity"));
    }
    Ok(())
}
fn valid_token(token: &str) -> Result<(), ProviderError> {
    if token.is_empty() || token.len() > MAX_TOKEN_BYTES {
        return Err(ProviderError::Protocol("invalid Google continuation"));
    }
    Ok(())
}
