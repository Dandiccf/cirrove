//! Google Drive adapter for selected My Drive or shared-drive collections.
mod feed;
mod files;
#[cfg(test)]
mod tests;
mod transport;
mod upload;

use cirrove_core::{CollectionInfo, ProviderError, RequestBudget, Scope, TokenSource};
use reqwest::{Client, Url, redirect::Policy};
use std::{collections::VecDeque, sync::Arc, time::Duration};
use tokio::{sync::Mutex, time::Instant};

#[cfg(feature = "test-support")]
#[derive(Clone, Debug, serde::Serialize)]
pub struct NativeExportObservation {
    pub kind: &'static str,
    pub metadata_size: Option<u64>,
    pub export_bytes: u64,
    pub export_sha256: String,
    pub metadata_matches_export: bool,
    pub version_stable: bool,
    pub head_revision_available: bool,
    pub listed_revision_available: bool,
}

#[cfg(feature = "test-support")]
#[derive(Clone, Debug, serde::Serialize)]
pub struct NativeExportPreflight {
    pub scanned_files: usize,
    pub observations: Vec<NativeExportObservation>,
}

pub const PROVIDER_ID: &str = "googledrive";
const MAX_PAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_TOKEN_BYTES: usize = 16 * 1024;

#[derive(Clone)]
pub struct GoogleDrive {
    account: String,
    collection: String,
    shared_drive: bool,
    endpoint: Url,
    client: Client,
    download_client: Client,
    tokens: Arc<dyn TokenSource>,
    budget: RequestBudget,
    cooldown: Arc<Mutex<Option<Instant>>>,
    native_exports: Arc<Mutex<VecDeque<NativeExportCacheEntry>>>,
    settle_write_receipts: bool,
}

#[derive(Clone)]
struct NativeExportCacheEntry {
    identity: String,
    bytes: Arc<[u8]>,
}
impl GoogleDrive {
    /// `collection` is the real My Drive root ID, resolved during connection.
    pub fn new(
        account: String,
        collection: String,
        tokens: Arc<dyn TokenSource>,
    ) -> Result<Self, ProviderError> {
        let mut drive = Self::build(
            account,
            collection,
            tokens,
            Url::parse("https://www.googleapis.com/drive/v3/")
                .map_err(|_| ProviderError::Protocol("Google endpoint"))?,
        )?;
        drive.settle_write_receipts = true;
        Ok(drive)
    }
    /// Bind an authenticated provider to one drive selected at connection time.
    /// The collection remains part of every cache, cursor and namespace identity.
    pub fn for_collection(mut self, drive: &CollectionInfo) -> Result<Self, ProviderError> {
        valid_id(&drive.id)?;
        self.shared_drive = match drive.drive_type.as_str() {
            "my_drive" => false,
            "shared_drive" => true,
            _ => return Err(ProviderError::Protocol("unsupported Google drive type")),
        };
        self.collection = drive.id.clone();
        Ok(self)
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
            shared_drive: false,
            endpoint,
            tokens,
            budget: RequestBudget::default(),
            cooldown: Arc::new(Mutex::new(None)),
            native_exports: Arc::new(Mutex::new(VecDeque::new())),
            settle_write_receipts: false,
            client: Client::builder()
                .redirect(Policy::none())
                .retry(reqwest::retry::never())
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(30))
                .user_agent(concat!("Cirrove/", env!("CARGO_PKG_VERSION")))
                .build()
                .map_err(|_| ProviderError::Unavailable)?,
            download_client: Client::builder()
                .redirect(Policy::none())
                .retry(reqwest::retry::never())
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(240))
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
        let mut drive = Self::build(
            account,
            collection,
            Arc::new(cirrove_core::StaticToken(secrecy::SecretString::from(
                "synthetic-google-token",
            ))),
            endpoint,
        )?;
        drive.settle_write_receipts = false;
        Ok(drive)
    }
    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn synthetic_loopback_with_settled_writes(
        account: String,
        collection: String,
        endpoint: &str,
    ) -> Result<Self, ProviderError> {
        let mut drive = Self::synthetic_loopback(account, collection, endpoint)?;
        drive.settle_write_receipts = true;
        Ok(drive)
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
    fn accepts_file(&self, file: &files::File) -> bool {
        if self.shared_drive {
            file.drive_id.as_deref() == Some(self.collection.as_str())
        } else {
            file.drive_id.is_none()
        }
    }
    fn shared_drive_query(&self, url: &mut Url) {
        if self.shared_drive {
            url.query_pairs_mut()
                .append_pair("supportsAllDrives", "true");
        }
    }
    fn url(&self, segments: &[&str]) -> Result<Url, ProviderError> {
        let mut url = self.endpoint.clone();
        url.path_segments_mut()
            .map_err(|_| ProviderError::Protocol("Google endpoint"))?
            .pop_if_empty()
            .extend(segments);
        if self.shared_drive
            && (segments == ["files"]
                || matches!(segments, ["files", item] if *item != "generateIds"))
        {
            self.shared_drive_query(&mut url);
        }
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

fn version_precondition(version: &str) -> String {
    format!("google-version:{version}")
}

fn precondition_version(token: &str) -> Option<&str> {
    token
        .strip_prefix("google-version:")
        .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
}
