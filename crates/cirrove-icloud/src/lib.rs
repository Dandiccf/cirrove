//! Experimental, native, read-only iCloud Drive transport.
//!
//! This module never invokes or reads another cloud client. Apple does not
//! publish a stable iCloud Drive API. The service has an internal read-only
//! account path and connection UI, but their live behavior and provider reliability
//! remain unvalidated.

mod provider;
pub use provider::ICloudDrive;

use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::{
    Client, Response, StatusCode,
    cookie::{CookieStore, Jar},
    header::{CONTENT_RANGE, HeaderMap, HeaderValue, RANGE},
};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use srp::{Client as SrpClient, groups::G2048};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use url::Url;
use uuid::Uuid;
use zeroize::Zeroizing;

const AUTH: &str = "https://idmsa.apple.com/appleauth/auth";
const SETUP: &str = "https://setup.icloud.com/setup/ws/1";
const ICLOUD_ORIGIN: &str = "https://www.icloud.com";
// Public widget identifier embedded in the iCloud web authentication flow;
// this is not an OAuth client secret or an rclone credential.
const WEB_WIDGET_ID: &str = "d39ba9916b7251055b22c7f910e2ea796ee65e98b2ddecea8f5dde8d9d1a815d";
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.3.1 Safari/605.1.15";
pub const ROOT_ID: &str = "FOLDER::com.apple.CloudDocs::root";
const MAX_JSON: usize = 8 * 1024 * 1024;
const MAX_FILE: usize = 16 * 1024 * 1024;
const MAX_RANGE: u32 = 4 * 1024 * 1024;
const MAX_SESSION_SNAPSHOT: usize = 128 * 1024;
const MAX_COOKIE_RECORDS: usize = 128;
const MAX_COOKIE_RECORD: usize = 4096;

#[derive(Debug)]
pub(crate) struct StaleRead;

#[derive(Debug)]
pub(crate) struct SessionRejected;

#[derive(Debug)]
pub(crate) struct IncompleteFolder;

impl std::fmt::Display for IncompleteFolder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("iCloud Drive returned an incomplete folder listing")
    }
}

impl std::error::Error for IncompleteFolder {}

impl std::fmt::Display for SessionRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Apple rejected the saved iCloud session; sign in again")
    }
}

impl std::error::Error for SessionRejected {}

fn drive_request_failure(status: StatusCode, stage: &'static str) -> anyhow::Error {
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        SessionRejected.into()
    } else {
        anyhow!("{stage} failed ({})", status.as_u16())
    }
}

impl std::fmt::Display for StaleRead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("iCloud file no longer matches the requested revision")
    }
}

impl std::error::Error for StaleRead {}

const LISTING_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignInStep {
    Ready,
    NeedsTrustedDeviceCode,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct DriveEntry {
    pub drivewsid: String,
    #[serde(default, deserialize_with = "null_to_default")]
    pub docwsid: String,
    #[serde(default, rename = "item_id", deserialize_with = "null_to_default")]
    pub item_id: String,
    #[serde(default, deserialize_with = "null_to_default")]
    pub zone: String,
    #[serde(default, deserialize_with = "null_to_default")]
    pub name: String,
    #[serde(default, deserialize_with = "null_to_default")]
    pub extension: String,
    #[serde(default, rename = "parentId", deserialize_with = "null_to_default")]
    pub parent_id: String,
    #[serde(default, deserialize_with = "null_to_default")]
    pub etag: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, deserialize_with = "null_to_default")]
    pub size: u64,
    #[serde(default, deserialize_with = "null_to_default")]
    pub items: Vec<DriveEntry>,
    #[serde(default, rename = "numberOfItems")]
    pub number_of_items: Option<usize>,
}

impl DriveEntry {
    pub fn display_name(&self) -> String {
        if self.extension.is_empty() {
            self.name.clone()
        } else {
            format!("{}.{}", self.name, self.extension)
        }
    }

    pub fn is_folder(&self) -> bool {
        matches!(
            self.kind.as_str(),
            "FOLDER" | "APP_CONTAINER" | "APP_LIBRARY"
        )
    }
}

fn check_expected_revision(entry: &DriveEntry, expected: Option<(&str, u64)>) -> Result<()> {
    if let Some((etag, size)) = expected
        && (etag.is_empty() || entry.is_folder() || entry.etag != etag || entry.size != size)
    {
        return Err(StaleRead.into());
    }
    Ok(())
}

#[derive(Debug, Default, Deserialize)]
struct AccountInfo {
    webservices: std::collections::HashMap<String, Option<WebService>>,
}

#[derive(Debug, Deserialize)]
struct WebService {
    url: Option<String>,
}

fn null_to_default<'de, D, T>(deserializer: D) -> std::result::Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Debug, Deserialize)]
struct SrpChallenge {
    iteration: u32,
    salt: String,
    protocol: String,
    b: String,
    c: String,
}

#[derive(Debug, Deserialize)]
struct DownloadLocation {
    data_token: Option<DownloadToken>,
    package_token: Option<DownloadToken>,
}

#[derive(Debug, Deserialize)]
struct DownloadToken {
    url: String,
}

#[derive(Default, Serialize, Deserialize)]
struct SessionHeaders {
    scnt: String,
    session_id: String,
    session_token: String,
    trust_token: String,
    account_country: String,
    auth_attributes: String,
}

#[derive(Serialize, Deserialize)]
struct CookieRecord {
    source: String,
    set_cookie: String,
}

#[derive(Serialize, Deserialize)]
struct SessionSnapshot {
    version: u8,
    account_hash: String,
    headers: SessionHeaders,
    drive_endpoint: String,
    docs_endpoint: String,
    cookies: Vec<CookieRecord>,
}

#[derive(Default)]
struct RecordingCookies {
    jar: Jar,
    records: Mutex<Vec<CookieRecord>>,
    overflow: AtomicBool,
}

impl RecordingCookies {
    fn records(&self) -> Result<Vec<CookieRecord>> {
        if self.overflow.load(Ordering::Relaxed) {
            bail!("iCloud session contains too many cookies to save safely");
        }
        let guard = self
            .records
            .lock()
            .map_err(|_| anyhow!("iCloud cookie store is unavailable"))?;
        Ok(guard
            .iter()
            .map(|record| CookieRecord {
                source: record.source.clone(),
                set_cookie: record.set_cookie.clone(),
            })
            .collect())
    }

    fn restore(&self, records: Vec<CookieRecord>) -> Result<()> {
        if records.len() > MAX_COOKIE_RECORDS {
            bail!("saved iCloud session has too many cookies");
        }
        for record in &records {
            if record.set_cookie.len() > MAX_COOKIE_RECORD {
                bail!("saved iCloud cookie exceeds limit");
            }
            let url = Url::parse(&record.source)
                .map_err(|_| anyhow!("invalid saved iCloud cookie source"))?;
            if !allowed_cookie_source(&url) || url.query().is_some() || url.fragment().is_some() {
                bail!("saved iCloud cookie source is outside Apple authentication");
            }
            self.jar.add_cookie_str(&record.set_cookie, &url);
        }
        *self
            .records
            .lock()
            .map_err(|_| anyhow!("iCloud cookie store is unavailable"))? = records;
        Ok(())
    }
}

impl CookieStore for RecordingCookies {
    fn set_cookies(&self, cookie_headers: &mut dyn Iterator<Item = &HeaderValue>, url: &Url) {
        let headers: Vec<_> = cookie_headers.cloned().collect();
        self.jar.set_cookies(&mut headers.iter(), url);
        if !allowed_cookie_source(url) {
            return;
        }
        let mut source = url.clone();
        source.set_query(None);
        source.set_fragment(None);
        let Ok(mut records) = self.records.lock() else {
            self.overflow.store(true, Ordering::Relaxed);
            return;
        };
        for header in headers {
            let Ok(value) = header.to_str() else {
                self.overflow.store(true, Ordering::Relaxed);
                continue;
            };
            if value.len() > MAX_COOKIE_RECORD || records.len() >= MAX_COOKIE_RECORDS {
                self.overflow.store(true, Ordering::Relaxed);
                continue;
            }
            records.push(CookieRecord {
                source: source.as_str().into(),
                set_cookie: value.into(),
            });
        }
    }

    fn cookies(&self, url: &Url) -> Option<HeaderValue> {
        self.jar.cookies(url)
    }
}

fn allowed_cookie_source(url: &Url) -> bool {
    url.scheme() == "https"
        && url.port_or_known_default() == Some(443)
        && url
            .host_str()
            .is_some_and(|host| host == "idmsa.apple.com" || host.ends_with(".icloud.com"))
        && url.username().is_empty()
        && url.password().is_none()
}

fn account_hash(apple_id: &str) -> Result<String> {
    let normalized = apple_id.trim().to_lowercase();
    if normalized.is_empty() || normalized.len() > 320 {
        bail!("invalid Apple account identifier");
    }
    Ok(hex::encode(Sha256::digest(normalized.as_bytes())))
}

/// An account-specific Cirrove-owned keyring key without an address in its
/// Secret Service attributes.
pub fn probe_session_key(apple_id: &str) -> Result<String> {
    Ok(format!("icloud-probe-{}", account_hash(apple_id)?))
}

impl SessionHeaders {
    fn absorb(&mut self, headers: &HeaderMap) {
        for (name, field) in [
            ("scnt", &mut self.scnt),
            ("x-apple-id-session-id", &mut self.session_id),
            ("x-apple-session-token", &mut self.session_token),
            ("x-apple-twosv-trust-token", &mut self.trust_token),
            ("x-apple-id-account-country", &mut self.account_country),
            ("x-apple-auth-attributes", &mut self.auth_attributes),
        ] {
            if let Some(value) = headers.get(name).and_then(|value| value.to_str().ok()) {
                field.clear();
                field.push_str(value);
            }
        }
    }
}

/// An in-memory session. No account password, cookies or tokens are written to disk.
pub struct ICloudReadSession {
    http: Client,
    cookies: Arc<RecordingCookies>,
    account_hash: Option<String>,
    frame: String,
    headers: SessionHeaders,
    drive_endpoint: Option<Url>,
    docs_endpoint: Option<Url>,
}

impl ICloudReadSession {
    pub fn new() -> Result<Self> {
        let cookies = Arc::new(RecordingCookies::default());
        let http = Client::builder()
            .cookie_provider(cookies.clone())
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(30))
            .build()
            .context("cannot create iCloud HTTP client")?;
        Ok(Self {
            http,
            cookies,
            account_hash: None,
            frame: format!("auth-{}", Uuid::new_v4()),
            headers: SessionHeaders::default(),
            drive_endpoint: None,
            docs_endpoint: None,
        })
    }

    /// Serialize only Cirrove's Apple web session material for storage in the
    /// desktop Secret Service. Never write this value to a normal file or log.
    pub fn session_snapshot(&self) -> Result<SecretString> {
        let drive = self
            .drive_endpoint
            .as_ref()
            .context("iCloud sign-in is not complete")?;
        let docs = self
            .docs_endpoint
            .as_ref()
            .context("iCloud sign-in is not complete")?;
        let snapshot = SessionSnapshot {
            version: 1,
            account_hash: self
                .account_hash
                .clone()
                .context("iCloud account identity is not bound")?,
            headers: SessionHeaders {
                scnt: self.headers.scnt.clone(),
                session_id: self.headers.session_id.clone(),
                session_token: self.headers.session_token.clone(),
                trust_token: self.headers.trust_token.clone(),
                account_country: self.headers.account_country.clone(),
                auth_attributes: self.headers.auth_attributes.clone(),
            },
            drive_endpoint: drive.to_string(),
            docs_endpoint: docs.to_string(),
            cookies: self.cookies.records()?,
        };
        let value = serde_json::to_string(&snapshot)?;
        if value.len() > MAX_SESSION_SNAPSHOT {
            bail!("iCloud session exceeds the safe keyring limit");
        }
        Ok(SecretString::from(value))
    }

    /// Reconstitute a keyring-backed session. A subsequent Apple request still
    /// decides whether the session is valid; this does not claim renewal.
    pub fn from_session_snapshot(value: &SecretString, apple_id: &str) -> Result<Self> {
        if value.expose_secret().len() > MAX_SESSION_SNAPSHOT {
            bail!("saved iCloud session exceeds limit");
        }
        let snapshot: SessionSnapshot = serde_json::from_str(value.expose_secret())
            .map_err(|_| anyhow!("invalid saved iCloud session"))?;
        if snapshot.version != 1
            || snapshot.headers.session_token.is_empty()
            || snapshot.account_hash != account_hash(apple_id)?
        {
            bail!("unsupported or incomplete saved iCloud session");
        }
        let drive = checked_drive_endpoint(&snapshot.drive_endpoint)?;
        let docs = checked_drive_endpoint(&snapshot.docs_endpoint)?;
        let mut session = Self::new()?;
        session.cookies.restore(snapshot.cookies)?;
        session.account_hash = Some(snapshot.account_hash);
        session.headers = snapshot.headers;
        session.drive_endpoint = Some(drive);
        session.docs_endpoint = Some(docs);
        Ok(session)
    }

    /// Starts Apple's SRP sign-in. The password is only used for the proof and
    /// is never sent, saved or included in an error.
    pub async fn sign_in(&mut self, apple_id: &str, password: &SecretString) -> Result<SignInStep> {
        let apple_id = apple_id.trim().to_lowercase();
        if apple_id.is_empty() || apple_id.len() > 320 {
            bail!("invalid Apple account identifier");
        }
        self.account_hash = Some(account_hash(&apple_id)?);
        self.start_auth().await?;
        self.federate(&apple_id).await?;

        let mut secret = Zeroizing::new([0u8; 32]);
        getrandom::fill(&mut secret[..]).map_err(|_| anyhow!("cannot create SRP secret"))?;
        let srp = SrpClient::<G2048, Sha256>::new_with_options(false);
        let a_pub = pad_2048(&srp.compute_public_ephemeral(&secret[..]))?;
        let response = self
            .auth_request(self.http.post(format!("{AUTH}/signin/init")).json(&json!({
                "a": STANDARD.encode(a_pub),
                "accountName": apple_id,
                "protocols": ["s2k", "s2k_fo"]
            })))
            .await?;
        if response.status() != StatusCode::OK {
            bail!("Apple rejected the SRP initiation");
        }
        let challenge: SrpChallenge = read_json(response, "Apple SRP challenge").await?;
        let salt = STANDARD
            .decode(&challenge.salt)
            .context("invalid SRP salt")?;
        let b_pub = STANDARD
            .decode(&challenge.b)
            .context("invalid SRP challenge")?;
        let b_padded = pad_2048(&b_pub)?;
        let derived = Zeroizing::new(derive_password(
            password.expose_secret(),
            &salt,
            &challenge,
        )?);
        let verifier = srp
            .process_reply(
                &secret[..],
                apple_id.as_bytes(),
                &derived[..],
                &salt,
                &b_pub,
            )
            .map_err(|_| anyhow!("invalid SRP challenge"))?;
        let (m1, m2) = apple_proofs(
            apple_id.as_bytes(),
            &salt,
            &a_pub,
            &b_padded,
            verifier.key(),
        )?;

        let response = self
            .auth_request(
                self.http
                    .post(format!("{AUTH}/signin/complete"))
                    .query(&[("isRememberMeEnabled", "true")])
                    .json(&json!({
                        "accountName": apple_id,
                        "m1": STANDARD.encode(m1),
                        "m2": STANDARD.encode(m2),
                        "c": challenge.c,
                        "rememberMe": true,
                        "trustTokens": []
                    })),
            )
            .await?;
        match response.status() {
            StatusCode::OK => {
                self.account_login().await?;
                Ok(SignInStep::Ready)
            }
            StatusCode::CONFLICT => Ok(SignInStep::NeedsTrustedDeviceCode),
            StatusCode::FORBIDDEN => bail!("Apple rejected the account sign-in"),
            _ => bail!("Apple sign-in requires an unsupported account step"),
        }
    }

    pub async fn request_trusted_device_code(&mut self) -> Result<()> {
        let mut response = self
            .auth_request(
                self.http
                    .put(format!("{AUTH}/verify/trusteddevice/securitycode")),
            )
            .await?;
        if trusted_device_push_get_fallback(response.status()) {
            response = self
                .auth_request(self.http.get(format!("{AUTH}/verify/trusteddevice")))
                .await?;
        }
        if !response.status().is_success() {
            bail!("Apple did not send a trusted-device code");
        }
        Ok(())
    }

    pub async fn verify_trusted_device_code(&mut self, code: &SecretString) -> Result<()> {
        let value = code.expose_secret().trim();
        if value.len() != 6 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            bail!("enter the six-digit code shown on your trusted device");
        }
        let response = self
            .auth_request(
                self.http
                    .post(format!("{AUTH}/verify/trusteddevice/securitycode"))
                    .json(&json!({"securityCode": {"code": value}})),
            )
            .await?;
        let accepted = response.status().is_success()
            || (response.status() == StatusCode::CONFLICT
                && !self.headers.session_token.is_empty());
        if !accepted {
            bail!("Apple did not accept the trusted-device code");
        }
        let response = self
            .auth_request(self.http.get(format!("{AUTH}/2sv/trust")))
            .await?;
        if !response.status().is_success() {
            bail!("Apple did not trust this sign-in session");
        }
        self.account_login().await
    }

    pub async fn list_folder(&mut self, folder_id: &str) -> Result<Vec<DriveEntry>> {
        let endpoint = self
            .drive_endpoint
            .as_ref()
            .context("iCloud sign-in is not complete")?;
        if folder_id.is_empty() || folder_id.len() > 512 || folder_id.contains('/') {
            bail!("invalid iCloud folder ID");
        }
        let url = endpoint
            .join("retrieveItemDetailsInFolders")
            .context("invalid iCloud Drive endpoint")?;
        let response = self
            .http
            .post(url)
            .timeout(LISTING_TIMEOUT)
            .header("origin", ICLOUD_ORIGIN)
            .header("referer", format!("{ICLOUD_ORIGIN}/"))
            .json(&json!([{
                "drivewsid": folder_id,
                "partialData": false,
                "includeHierarchy": false
            }]))
            .send()
            .await
            .map_err(|error| {
                let reason = if error.is_timeout() {
                    "timed out"
                } else if error.is_connect() {
                    "could not connect"
                } else if error.is_request() {
                    "could not send request"
                } else {
                    "network error"
                };
                anyhow!("iCloud Drive listing request failed ({reason})")
            })?;
        self.headers.absorb(response.headers());
        if !response.status().is_success() {
            return Err(drive_request_failure(
                response.status(),
                "iCloud Drive listing",
            ));
        }
        let mut folders: Vec<DriveEntry> = read_json(response, "iCloud folder listing").await?;
        if folders.len() != 1 || folders[0].drivewsid != folder_id {
            bail!("iCloud Drive returned an unexpected folder identity");
        }
        let folder = folders.remove(0);
        if folder.number_of_items != Some(folder.items.len()) {
            return Err(IncompleteFolder.into());
        }
        for item in &folder.items {
            if item.drivewsid.is_empty() || item.name.is_empty() {
                bail!("iCloud Drive returned an item without identity or name");
            }
        }
        Ok(folder.items)
    }

    pub async fn list_root(&mut self) -> Result<Vec<DriveEntry>> {
        self.list_folder(ROOT_ID).await
    }

    /// Fetch a small ordinary file by provider ID. This is an experimental
    /// validation read, not yet the mounted version-bound `ReadProvider` path.
    pub async fn read_small_file(&mut self, drive_id: &str) -> Result<Vec<u8>> {
        self.read_small_file_with_parent(drive_id, None).await
    }

    /// Validate a file through its known parent listing. This also covers
    /// accounts where Apple's individual-item lookup rejects a valid Drive ID.
    pub async fn read_small_file_in_folder(
        &mut self,
        folder_id: &str,
        drive_id: &str,
    ) -> Result<Vec<u8>> {
        self.read_small_file_with_parent(drive_id, Some(folder_id))
            .await
    }

    async fn read_small_file_with_parent(
        &mut self,
        drive_id: &str,
        folder_id: Option<&str>,
    ) -> Result<Vec<u8>> {
        let before = self.item_for_read(drive_id, folder_id).await?;
        if before.is_folder() || before.size > MAX_FILE as u64 || before.etag.is_empty() {
            bail!("file is not eligible for the bounded read-only probe");
        }
        let signed_url = self.signed_download_url(drive_id).await?;
        let mut response = self
            .http
            .get(signed_url)
            .send()
            .await
            .map_err(|_| anyhow!("iCloud content request failed"))?;
        if response.status() != StatusCode::OK {
            bail!(
                "iCloud content request failed ({})",
                response.status().as_u16()
            );
        }
        let mut bytes = Vec::with_capacity(before.size as usize);
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow!("iCloud content response was interrupted"))?
        {
            if bytes.len().saturating_add(chunk.len()) > MAX_FILE {
                bail!("iCloud file exceeds the bounded probe limit");
            }
            bytes.extend_from_slice(&chunk);
        }
        let after = self.item_for_read(drive_id, folder_id).await?;
        if before.etag != after.etag
            || before.size != after.size
            || bytes.len() as u64 != before.size
        {
            bail!("iCloud file changed during the read or returned an unexpected size");
        }
        Ok(bytes)
    }

    /// Read one exact, bounded byte range while checking the enclosing file's
    /// identity, ETag and size in its known parent folder on both sides.
    pub async fn read_range_in_folder(
        &mut self,
        folder_id: &str,
        drive_id: &str,
        offset: u64,
        length: u32,
    ) -> Result<Vec<u8>> {
        self.read_range_in_folder_for_revision(folder_id, drive_id, offset, length, None)
            .await
    }

    /// The mounted cache may only receive bytes for the exact node revision it
    /// requested. The caller supplies the metadata it already published.
    pub(crate) async fn read_range_in_folder_for_revision(
        &mut self,
        folder_id: &str,
        drive_id: &str,
        offset: u64,
        length: u32,
        expected_revision: Option<(&str, u64)>,
    ) -> Result<Vec<u8>> {
        if length == 0 || length > MAX_RANGE {
            bail!("iCloud range length is outside the probe limit");
        }
        let before = self
            .item_for_read(drive_id, Some(folder_id))
            .await
            .context("iCloud read: initial parent lookup")?;
        check_expected_revision(&before, expected_revision)
            .context("iCloud read: initial revision check")?;
        if before.is_folder() || before.etag.is_empty() {
            bail!("file is not eligible for the bounded read-only probe");
        }
        let expected = before.size.saturating_sub(offset).min(u64::from(length));
        if expected == 0 {
            return Ok(Vec::new());
        }
        let end = offset
            .checked_add(expected - 1)
            .context("iCloud range overflow")?;
        let signed_url = self
            .signed_download_url(drive_id)
            .await
            .context("iCloud read: download lookup")?;
        let response = self
            .http
            .get(signed_url)
            .header(RANGE, format!("bytes={offset}-{end}"))
            .send()
            .await
            .map_err(|_| anyhow!("iCloud range request failed"))?;
        let bytes = read_exact_range_response(response, offset, end, before.size)
            .await
            .context("iCloud read: content response")?;
        let after = self
            .item_for_read(drive_id, Some(folder_id))
            .await
            .context("iCloud read: final parent lookup")?;
        check_expected_revision(&after, expected_revision)
            .context("iCloud read: final revision check")?;
        if before.etag != after.etag || before.size != after.size {
            if expected_revision.is_some() {
                return Err(StaleRead.into());
            }
            bail!("iCloud file changed during the range read");
        }
        Ok(bytes)
    }

    async fn signed_download_url(&mut self, drive_id: &str) -> Result<Url> {
        let (zone, doc_id) = split_file_id(drive_id)?;
        let endpoint = self
            .docs_endpoint
            .as_ref()
            .context("iCloud sign-in is not complete")?;
        let url = endpoint
            .join(&format!("ws/{zone}/download/by_id"))
            .context("invalid iCloud document endpoint")?;
        let response = self
            .http
            .get(url)
            .query(&[("document_id", doc_id)])
            .header("origin", ICLOUD_ORIGIN)
            .header("referer", format!("{ICLOUD_ORIGIN}/"))
            .send()
            .await
            .map_err(|_| anyhow!("iCloud download lookup failed"))?;
        if !response.status().is_success() {
            return Err(drive_request_failure(
                response.status(),
                "iCloud download lookup",
            ));
        }
        let location: DownloadLocation = read_json(response, "iCloud download lookup").await?;
        let signed = location
            .data_token
            .or(location.package_token)
            .context("iCloud did not provide a download location")?;
        checked_content_url(&signed.url)
    }

    async fn item_for_read(
        &mut self,
        drive_id: &str,
        folder_id: Option<&str>,
    ) -> Result<DriveEntry> {
        if let Some(folder_id) = folder_id {
            let matches: Vec<_> = self
                .list_folder(folder_id)
                .await?
                .into_iter()
                .filter(|item| item.drivewsid == drive_id)
                .collect();
            match matches.as_slice() {
                [item] => Ok(item.clone()),
                _ => bail!("iCloud folder did not contain exactly one matching file"),
            }
        } else {
            self.item_by_id(drive_id).await
        }
    }

    async fn item_by_id(&mut self, drive_id: &str) -> Result<DriveEntry> {
        let _ = split_file_id(drive_id)?;
        let endpoint = self
            .drive_endpoint
            .as_ref()
            .context("iCloud sign-in is not complete")?;
        let url = endpoint
            .join("retrieveItemDetails")
            .context("invalid iCloud Drive endpoint")?;
        let response = self
            .http
            .post(url)
            .header("origin", ICLOUD_ORIGIN)
            .header("referer", format!("{ICLOUD_ORIGIN}/"))
            .json(&json!([{"items": [{
                "drivewsid": drive_id,
                "partialData": false,
                "includeHierarchy": false
            }]}]))
            .send()
            .await
            .map_err(|_| anyhow!("iCloud item lookup failed"))?;
        if !response.status().is_success() {
            return Err(drive_request_failure(
                response.status(),
                "iCloud item lookup",
            ));
        }
        let items: Vec<DriveEntry> = read_json(response, "iCloud item lookup").await?;
        match items.as_slice() {
            [item] if item.drivewsid == drive_id => Ok(item.clone()),
            _ => bail!("iCloud returned an unexpected item identity"),
        }
    }

    async fn start_auth(&mut self) -> Result<()> {
        let response = self
            .http
            .get(format!("{AUTH}/authorize/signin"))
            .query(&[
                ("frame_id", self.frame.as_str()),
                ("language", "en_US"),
                ("skVersion", "7"),
                ("iframeId", self.frame.as_str()),
                ("client_id", WEB_WIDGET_ID),
                ("redirect_uri", ICLOUD_ORIGIN),
                ("response_type", "code"),
                ("response_mode", "web_message"),
                ("state", self.frame.as_str()),
                ("authVersion", "latest"),
            ])
            .send()
            .await
            .map_err(|_| anyhow!("cannot start Apple sign-in"))?;
        self.headers.absorb(response.headers());
        if response.status() != StatusCode::OK {
            bail!("Apple sign-in could not start");
        }
        Ok(())
    }

    async fn federate(&mut self, apple_id: &str) -> Result<()> {
        let response = self
            .auth_request(
                self.http
                    .post(format!("{AUTH}/federate"))
                    .query(&[("isRememberMeEnabled", "true")])
                    .json(&json!({"accountName": apple_id, "rememberMe": true})),
            )
            .await?;
        if response.status() != StatusCode::OK {
            bail!("Apple account discovery failed");
        }
        Ok(())
    }

    async fn account_login(&mut self) -> Result<()> {
        if self.headers.session_token.is_empty() {
            bail!("Apple did not issue an account session");
        }
        let response = self
            .http
            .post(format!("{SETUP}/accountLogin"))
            .header("origin", ICLOUD_ORIGIN)
            .header("referer", format!("{ICLOUD_ORIGIN}/"))
            .json(&json!({
                "accountCountryCode": self.headers.account_country,
                "dsWebAuthToken": self.headers.session_token,
                "extended_login": true,
                "trustToken": self.headers.trust_token
            }))
            .send()
            .await
            .map_err(|_| anyhow!("iCloud account session failed"))?;
        self.headers.absorb(response.headers());
        if !response.status().is_success() {
            bail!(
                "iCloud account session was rejected ({})",
                response.status().as_u16()
            );
        }
        let account: AccountInfo = read_json(response, "iCloud account login").await?;
        let drive = account
            .webservices
            .get("drivews")
            .and_then(Option::as_ref)
            .and_then(|service| service.url.as_deref())
            .context("this Apple account did not expose iCloud Drive")?;
        let docs = account
            .webservices
            .get("docws")
            .and_then(Option::as_ref)
            .and_then(|service| service.url.as_deref())
            .context("this Apple account did not expose iCloud documents")?;
        self.drive_endpoint = Some(checked_drive_endpoint(drive)?);
        self.docs_endpoint = Some(checked_drive_endpoint(docs)?);
        Ok(())
    }

    async fn auth_request(&mut self, builder: reqwest::RequestBuilder) -> Result<Response> {
        let builder = builder
            .header("accept", "application/json")
            .header("origin", "https://idmsa.apple.com")
            .header("referer", "https://idmsa.apple.com/")
            .header("x-apple-widget-key", WEB_WIDGET_ID)
            .header("x-apple-oauth-client-id", WEB_WIDGET_ID)
            .header("x-apple-oauth-client-type", "firstPartyAuth")
            .header("x-apple-oauth-redirect-uri", ICLOUD_ORIGIN)
            .header("x-apple-oauth-require-grant-code", "true")
            .header("x-apple-oauth-response-mode", "web_message")
            .header("x-apple-oauth-response-type", "code")
            .header("x-apple-oauth-state", self.frame.as_str())
            .header("x-apple-frame-id", self.frame.as_str())
            .header("x-requested-with", "XMLHttpRequest")
            .header("x-apple-mandate-security-upgrade", "0")
            .header("x-apple-i-require-ue", "true")
            .header(
                "x-apple-i-fd-client-info",
                format!("{{\"U\":\"{USER_AGENT}\",\"L\":\"en-US\",\"Z\":\"GMT-05:00\",\"V\":\"1.1\",\"F\":\"\"}}"),
            );
        let builder = if self.headers.scnt.is_empty() {
            builder
        } else {
            builder.header("scnt", self.headers.scnt.as_str())
        };
        let builder = if self.headers.session_id.is_empty() {
            builder
        } else {
            builder.header("x-apple-id-session-id", self.headers.session_id.as_str())
        };
        let builder = if self.headers.auth_attributes.is_empty() {
            builder
        } else {
            builder.header(
                "x-apple-auth-attributes",
                self.headers.auth_attributes.as_str(),
            )
        };
        let response = builder
            .send()
            .await
            .map_err(|_| anyhow!("Apple sign-in network request failed"))?;
        self.headers.absorb(response.headers());
        Ok(response)
    }
}

fn trusted_device_push_get_fallback(status: StatusCode) -> bool {
    status == StatusCode::METHOD_NOT_ALLOWED
}

fn checked_drive_endpoint(raw: &str) -> Result<Url> {
    let url = Url::parse(raw).map_err(|_| anyhow!("invalid iCloud Drive endpoint"))?;
    let host = url
        .host_str()
        .context("iCloud Drive endpoint has no host")?;
    if url.scheme() != "https"
        || url.port_or_known_default() != Some(443)
        || !(host == "icloud.com" || host.ends_with(".icloud.com"))
        || url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("iCloud Drive endpoint is outside Apple's expected domain");
    }
    Ok(url)
}

fn checked_content_url(raw: &str) -> Result<Url> {
    let url = Url::parse(raw).map_err(|_| anyhow!("invalid iCloud content location"))?;
    let host = url
        .host_str()
        .context("iCloud content location has no host")?;
    if url.scheme() != "https"
        || url.port_or_known_default() != Some(443)
        || !(host == "icloud-content.com" || host.ends_with(".icloud-content.com"))
        || url.username() != ""
        || url.password().is_some()
        || url.fragment().is_some()
    {
        bail!("iCloud content location is outside Apple's expected domain");
    }
    Ok(url)
}

fn split_file_id(id: &str) -> Result<(&str, &str)> {
    let mut parts = id.split("::");
    let (Some("FILE"), Some(zone), Some(doc_id), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        bail!("invalid iCloud file identity");
    };
    if zone.is_empty()
        || zone.len() > 160
        || !zone
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        || doc_id.is_empty()
        || doc_id.len() > 256
    {
        bail!("invalid iCloud file identity");
    }
    Ok((zone, doc_id))
}

fn content_range_matches(value: &str, start: u64, end: u64, size: u64) -> bool {
    let Some(range) = value.strip_prefix("bytes ") else {
        return false;
    };
    let Some((bounds, total)) = range.split_once('/') else {
        return false;
    };
    let Some((first, last)) = bounds.split_once('-') else {
        return false;
    };
    matches!(
        (first.parse::<u64>(), last.parse::<u64>(), total.parse::<u64>()),
        (Ok(actual_start), Ok(actual_end), Ok(actual_size))
            if actual_start == start && actual_end == end && actual_size == size
    )
}

async fn read_exact_range_response(
    mut response: Response,
    offset: u64,
    end: u64,
    size: u64,
) -> Result<Vec<u8>> {
    let diagnose = |reason: &'static str| {
        if std::env::var_os("CIRROVE_ICLOUD_PROBE_DIAGNOSTICS").is_some() {
            eprintln!("iCloud content response rejected: {reason}");
        }
    };
    if response.status() == StatusCode::OK && offset == 0 && end.checked_add(1) == Some(size) {
        if let Some(content_range) = response.headers().get(CONTENT_RANGE)
            && !content_range
                .to_str()
                .is_ok_and(|value| content_range_matches(value, offset, end, size))
        {
            diagnose("complete response has mismatched Content-Range");
            bail!("iCloud complete response identified different bytes");
        }
    } else if response.status() == StatusCode::PARTIAL_CONTENT {
        let content_range = response
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| {
                diagnose("partial response lacks Content-Range");
                anyhow!("iCloud range response omitted Content-Range")
            })?;
        if !content_range_matches(content_range, offset, end, size) {
            diagnose("partial response has mismatched Content-Range");
            bail!("iCloud range response identified different bytes");
        }
    } else {
        diagnose("unexpected HTTP status or full response for partial range");
        bail!("iCloud range response did not identify the requested bytes");
    }
    let expected = end
        .checked_sub(offset)
        .and_then(|length| length.checked_add(1))
        .and_then(|length| usize::try_from(length).ok())
        .context("iCloud range exceeds memory bound")?;
    let mut bytes = Vec::with_capacity(expected);
    while let Some(chunk) = response.chunk().await.map_err(|_| {
        diagnose("response stream interrupted");
        anyhow!("iCloud range response was interrupted")
    })? {
        if bytes.len().saturating_add(chunk.len()) > expected {
            diagnose("response exceeds requested length");
            bail!("iCloud range response exceeded the requested length");
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.len() != expected {
        diagnose("response shorter than requested");
        bail!("iCloud range response was shorter than requested");
    }
    Ok(bytes)
}

async fn read_json<T: serde::de::DeserializeOwned>(
    mut response: Response,
    stage: &'static str,
) -> Result<T> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow!("iCloud response was interrupted"))?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_JSON {
            bail!("iCloud response exceeds the safe metadata limit");
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|error| {
        let kind = match error.classify() {
            serde_json::error::Category::Data => "unexpected JSON shape",
            serde_json::error::Category::Eof => "incomplete JSON",
            serde_json::error::Category::Syntax if bytes.starts_with(b"<") => {
                "HTML instead of JSON"
            }
            serde_json::error::Category::Syntax => "invalid JSON",
            serde_json::error::Category::Io => "interrupted JSON",
        };
        anyhow!("invalid {stage} response ({kind})")
    })
}

fn derive_password(password: &str, salt: &[u8], challenge: &SrpChallenge) -> Result<[u8; 32]> {
    if challenge.iteration == 0 || challenge.iteration > 1_000_000 || salt.is_empty() {
        bail!("invalid Apple SRP parameters");
    }
    let digest = Sha256::digest(password.as_bytes());
    let input = match challenge.protocol.as_str() {
        "s2k" => digest.to_vec(),
        "s2k_fo" => hex::encode(digest).into_bytes(),
        _ => bail!("unsupported Apple SRP protocol"),
    };
    let mut output = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha256>(&input, salt, challenge.iteration, &mut output);
    Ok(output)
}

fn pad_2048(value: &[u8]) -> Result<[u8; 256]> {
    if value.is_empty() || value.len() > 256 {
        bail!("invalid SRP group value");
    }
    let mut padded = [0u8; 256];
    padded[256 - value.len()..].copy_from_slice(value);
    Ok(padded)
}

fn apple_proofs(
    username: &[u8],
    salt: &[u8],
    a: &[u8; 256],
    b: &[u8; 256],
    premaster: &[u8],
) -> Result<([u8; 32], [u8; 32])> {
    // RFC 5054's 2048-bit N. Apple's variant hashes a fixed-width shared secret.
    const N: &str = concat!(
        "ac6bdb41324a9a9bf166de5e1389582faf72b6651987ee07fc3192943db56050",
        "a37329cbb4a099ed8193e0757767a13dd52312ab4b03310dcd7f48a9da04fd50",
        "e8083969edb767b0cf6095179a163ab3661a05fbd5faaaE82918a9962f0b93b8",
        "55f97993ec975eeaa80d740adbf4ff747359d041d5c33ea71d281e446b14773b",
        "ca97b43a23fb801676bd207a436c6481f1d2b9078717461a5b9d32e688f87748",
        "544523b524b0d57d5ea77a2775d2ecfa032cfbdbf52fb3786160279004e57ae6",
        "af874e7303ce53299ccc041c7bc308d82a5698f3a8d0c38271ae35f8e9dbfbb",
        "694b5c803d89f7ae435de236d525f54759b65e372fcd68ef20fa7111f9e4aff73"
    );
    let n = hex::decode(N).context("invalid built-in SRP group")?;
    if n.len() != 256 {
        bail!("invalid built-in SRP group width");
    }
    let mut g = [0u8; 256];
    g[255] = 2;
    let hg = Sha256::digest(g);
    let hn = Sha256::digest(n);
    let mut xor = [0u8; 32];
    for index in 0..32 {
        xor[index] = hg[index] ^ hn[index];
    }
    let key = Sha256::digest(pad_2048(premaster)?);
    let username_hash = Sha256::digest(username);
    let m1 = Sha256::new()
        .chain_update(xor)
        .chain_update(username_hash)
        .chain_update(salt)
        .chain_update(a)
        .chain_update(b)
        .chain_update(key)
        .finalize();
    let m2 = Sha256::new()
        .chain_update(a)
        .chain_update(m1)
        .chain_update(key)
        .finalize();
    Ok((m1.into(), m2.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    async fn synthetic_range_response(
        status: &'static str,
        content_range: Option<&'static str>,
        body: &'static [u8],
    ) -> Result<Response> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("fixture accepts a request");
            let range = content_range
                .map(|range| format!("Content-Range: {range}\r\n"))
                .unwrap_or_default();
            let headers = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\n{range}Connection: close\r\n\r\n",
                body.len()
            );
            stream
                .write_all(headers.as_bytes())
                .await
                .expect("fixture writes headers");
            stream.write_all(body).await.expect("fixture writes body");
        });
        Ok(Client::new()
            .get(format!("http://{address}/"))
            .send()
            .await?)
    }

    #[test]
    fn trusted_device_push_retries_get_only_when_put_is_unsupported() {
        assert!(trusted_device_push_get_fallback(
            StatusCode::METHOD_NOT_ALLOWED
        ));
        assert!(!trusted_device_push_get_fallback(StatusCode::OK));
        assert!(!trusted_device_push_get_fallback(StatusCode::UNAUTHORIZED));
        assert!(!trusted_device_push_get_fallback(
            StatusCode::TOO_MANY_REQUESTS
        ));
    }

    #[test]
    fn rejects_untrusted_drive_endpoint() {
        assert!(checked_drive_endpoint("http://drivews.icloud.com/").is_err());
        assert!(checked_drive_endpoint("https://drivews.icloud.com.evil.test/").is_err());
        assert!(checked_drive_endpoint("https://example.com/").is_err());
        assert!(checked_drive_endpoint("https://drivews.icloud.com/").is_ok());
        assert!(checked_content_url("https://cvws.icloud-content.com/B/file?token=secret").is_ok());
        assert!(checked_content_url("https://cvws.icloud-content.com.evil.test/B/file").is_err());
    }

    #[test]
    fn password_derivation_rejects_unbounded_work_and_unknown_protocol() {
        let mut challenge = SrpChallenge {
            iteration: 1_000_001,
            salt: String::new(),
            protocol: "s2k".into(),
            b: String::new(),
            c: String::new(),
        };
        assert!(derive_password("secret", b"salt", &challenge).is_err());
        challenge.iteration = 1;
        challenge.protocol = "unknown".into();
        assert!(derive_password("secret", b"salt", &challenge).is_err());
    }

    #[test]
    fn folder_entry_retains_opaque_identity() -> Result<()> {
        let entry: DriveEntry = serde_json::from_value(json!({
            "drivewsid": "FOLDER::zone::opaque",
            "name": "Example",
            "type": "FOLDER",
            "items": []
        }))?;
        assert_eq!(entry.drivewsid, "FOLDER::zone::opaque");
        assert!(entry.is_folder());
        Ok(())
    }

    #[test]
    fn mounted_range_guard_refuses_a_different_revision_or_size() -> Result<()> {
        let mut entry: DriveEntry = serde_json::from_value(json!({
            "drivewsid": "FILE::zone::opaque",
            "name": "Example",
            "type": "FILE",
            "size": 12,
            "etag": "revision-a"
        }))?;
        check_expected_revision(&entry, Some(("revision-a", 12)))?;
        assert!(
            check_expected_revision(&entry, Some(("revision-b", 12)))
                .is_err_and(|error| error.downcast_ref::<StaleRead>().is_some())
        );
        entry.size = 13;
        assert!(
            check_expected_revision(&entry, Some(("revision-a", 12)))
                .is_err_and(|error| error.downcast_ref::<StaleRead>().is_some())
        );
        Ok(())
    }

    #[test]
    fn optional_service_and_item_fields_may_be_null() -> Result<()> {
        let account: AccountInfo = serde_json::from_value(json!({
            "webservices": {
                "drivews": {"url": "https://p01-drivews.icloud.com/"},
                "unused": null
            }
        }))?;
        assert!(
            account
                .webservices
                .get("unused")
                .is_some_and(Option::is_none)
        );
        let entry: DriveEntry = serde_json::from_value(json!({
            "drivewsid": "FOLDER::com.apple.CloudDocs::root",
            "type": "FOLDER",
            "name": null,
            "size": null,
            "items": null
        }))?;
        assert!(entry.items.is_empty());
        assert_eq!(entry.size, 0);
        Ok(())
    }

    #[test]
    fn content_range_requires_exact_bounds_and_file_size() {
        assert!(content_range_matches("bytes 17-31/100", 17, 31, 100));
        for value in [
            "bytes 17-30/100",
            "bytes 17-31/101",
            "bytes */100",
            "bytes 17-31/*",
            "bytes 17-31/100, bytes 40-42/100",
            "17-31/100",
        ] {
            assert!(!content_range_matches(value, 17, 31, 100));
        }
    }

    #[tokio::test]
    async fn complete_file_response_may_be_ok_but_a_subrange_still_requires_partial_content()
    -> Result<()> {
        let full = synthetic_range_response("200 OK", None, b"ninebytes").await?;
        assert_eq!(
            read_exact_range_response(full, 0, 8, 9).await?,
            b"ninebytes"
        );

        let full_with_range =
            synthetic_range_response("200 OK", Some("bytes 0-8/9"), b"ninebytes").await?;
        assert_eq!(
            read_exact_range_response(full_with_range, 0, 8, 9).await?,
            b"ninebytes"
        );

        let wrong_full_range =
            synthetic_range_response("200 OK", Some("bytes 0-7/9"), b"ninebytes").await?;
        assert!(
            read_exact_range_response(wrong_full_range, 0, 8, 9)
                .await
                .is_err()
        );

        let ignored_range = synthetic_range_response("200 OK", None, b"ninebytes").await?;
        assert!(
            read_exact_range_response(ignored_range, 1, 8, 9)
                .await
                .is_err()
        );

        let unexpected_bytes = synthetic_range_response("200 OK", None, b"ninebytes!").await?;
        assert!(
            read_exact_range_response(unexpected_bytes, 0, 8, 9)
                .await
                .is_err()
        );

        let exact =
            synthetic_range_response("206 Partial Content", Some("bytes 1-8/9"), b"inebytes")
                .await?;
        assert_eq!(
            read_exact_range_response(exact, 1, 8, 9).await?,
            b"inebytes"
        );
        let unproven = synthetic_range_response("206 Partial Content", None, b"inebytes").await?;
        assert!(read_exact_range_response(unproven, 1, 8, 9).await.is_err());
        Ok(())
    }

    #[test]
    fn saved_session_restores_only_apple_cookies_and_rejects_other_hosts() -> Result<()> {
        let mut session = ICloudReadSession::new()?;
        session.account_hash = Some(account_hash("person@example.com")?);
        session.headers.session_token = "synthetic-session-token".into();
        session.drive_endpoint = Some(checked_drive_endpoint("https://p01-drivews.icloud.com/")?);
        session.docs_endpoint = Some(checked_drive_endpoint("https://p01-docws.icloud.com/")?);
        let source = Url::parse("https://setup.icloud.com/setup/ws/1/accountLogin")?;
        let cookie = HeaderValue::from_static("synthetic=private-value; Path=/; Secure; HttpOnly");
        session
            .cookies
            .set_cookies(&mut [&cookie].into_iter(), &source);
        let snapshot = session.session_snapshot()?;
        let restored = ICloudReadSession::from_session_snapshot(&snapshot, "person@example.com")?;
        assert_eq!(
            session.cookies.cookies(&source),
            restored.cookies.cookies(&source)
        );
        let mut invalid: serde_json::Value = serde_json::from_str(snapshot.expose_secret())?;
        invalid["cookies"][0]["source"] = "https://other.example/".into();
        let invalid = SecretString::from(serde_json::to_string(&invalid)?);
        assert!(ICloudReadSession::from_session_snapshot(&invalid, "person@example.com").is_err());
        let mut invalid: serde_json::Value = serde_json::from_str(snapshot.expose_secret())?;
        invalid["cookies"][0]["source"] = "https://setup.icloud.com:8443/".into();
        let invalid = SecretString::from(serde_json::to_string(&invalid)?);
        assert!(ICloudReadSession::from_session_snapshot(&invalid, "person@example.com").is_err());
        assert!(
            ICloudReadSession::from_session_snapshot(&snapshot, "someone-else@example.com")
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn apple_proof_matches_independent_sha256_vector() -> Result<()> {
        let a = pad_2048(&[2])?;
        let b = pad_2048(&[3])?;
        let (m1, m2) = apple_proofs(b"user@example.com", b"salt", &a, &b, &[1])?;
        assert_eq!(
            hex::encode(m1),
            "a740d1d9c747fe9821d1a4f2b7d399b43af822b513482cc85682e41f4f4f551f"
        );
        assert_eq!(
            hex::encode(m2),
            "236d05a9a5c2ffd140a11d41bdc04aa08c197c4feab51d2a9c6b290e1486013f"
        );
        Ok(())
    }

    #[test]
    fn file_id_is_opaque_but_cannot_choose_an_endpoint() -> Result<()> {
        assert_eq!(
            split_file_id("FILE::com.apple.CloudDocs::abc-123")?,
            ("com.apple.CloudDocs", "abc-123")
        );
        assert!(split_file_id("FILE::evil/path::abc").is_err());
        assert!(split_file_id("FOLDER::com.apple.CloudDocs::abc").is_err());
        Ok(())
    }
}
