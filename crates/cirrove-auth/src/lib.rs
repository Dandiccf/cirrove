//! Microsoft browser OAuth/PKCE, validated OIDC identity and keyring-backed refresh.
//! No raw token, callback URL or provider response is used in errors or logs.
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use cirrove_core::ProviderError;
use cirrove_onedrive::TokenSource;
use openidconnect::{
    AdditionalClaims, ClientId, CsrfToken, IdToken, IssuerUrl, Nonce, PkceCodeChallenge,
    PkceCodeVerifier,
    core::{
        CoreGenderClaim, CoreIdTokenVerifier, CoreJsonWebKeySet, CoreJweContentEncryptionAlgorithm,
        CoreJwsSigningAlgorithm,
    },
};
use reqwest::{Client, Url, redirect::Policy};
use secrecy::{ExposeSecret, SecretString};
use secret_service::{EncryptionType, SecretService};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Mutex,
};

pub const SCOPES: &str = "openid profile offline_access https://graph.microsoft.com/User.Read https://graph.microsoft.com/Files.Read.All";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppRegistration {
    pub client_id: String,
    pub authority: String,
}
impl AppRegistration {
    pub fn validate(&self) -> Result<()> {
        uuid::Uuid::parse_str(&self.client_id).context("application/client ID must be a UUID")?;
        if !matches!(
            self.authority.as_str(),
            "common" | "organizations" | "consumers"
        ) {
            uuid::Uuid::parse_str(&self.authority)
                .context("tenant must be a UUID, common, organizations or consumers")?;
        }
        Ok(())
    }
    fn endpoint(&self, path: &str) -> String {
        format!(
            "https://login.microsoftonline.com/{}/oauth2/v2.0/{path}",
            self.authority
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Identity {
    pub tenant_id: String,
    pub subject: String,
    pub username: String,
    pub graph_user_id: String,
    pub display_name: String,
}

/// Owned secret state. Intentionally no Debug implementation.
#[derive(Serialize, Deserialize)]
pub struct Credentials {
    access_token: String,
    refresh_token: String,
    expires_at: u64,
}
impl Credentials {
    pub fn access_token(&self) -> SecretString {
        SecretString::from(self.access_token.clone())
    }
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn http_client() -> Result<Client> {
    Ok(Client::builder()
        .redirect(Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()?)
}
async fn bounded_json<T: serde::de::DeserializeOwned>(
    mut response: reqwest::Response,
) -> Result<T> {
    if !response.status().is_success() {
        let status = response.status();
        if status.as_u16() == 429 || status.as_u16() == 503 {
            let delay = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(30);
            return Err(ProviderError::Throttled(Duration::from_secs(delay.max(1))).into());
        }
        if status.is_server_error() {
            return Err(ProviderError::Unavailable.into());
        }
        return Err(ProviderError::Authentication.into());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow::Error::from(ProviderError::Unavailable))?
    {
        if chunk.len() > 1024usize.saturating_mul(1024).saturating_sub(body.len()) {
            bail!("Microsoft response exceeds limit");
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| anyhow::anyhow!("invalid Microsoft response"))
}

#[async_trait]
pub trait CredentialVault: Send + Sync {
    async fn load(&self, key: &str) -> Result<Option<SecretString>>;
    async fn save(&self, key: &str, value: SecretString) -> Result<()>;
    async fn remove(&self, key: &str) -> Result<()>;
}
/// Only Cirrove-owned entries are ever searched, updated or removed.
pub struct DesktopVault;
fn attributes(key: &str) -> HashMap<&str, &str> {
    HashMap::from([("application", "cirrove"), ("credential-id", key)])
}
#[async_trait]
impl CredentialVault for DesktopVault {
    async fn load(&self, key: &str) -> Result<Option<SecretString>> {
        let ss = SecretService::connect(EncryptionType::Dh)
            .await
            .context("desktop Secret Service unavailable")?;
        let result = ss
            .search_items(attributes(key))
            .await
            .context("cannot search Cirrove credentials")?;
        if let Some(item) = result.unlocked.first() {
            return Ok(Some(SecretString::from(
                String::from_utf8(
                    item.get_secret()
                        .await
                        .context("cannot read Cirrove credential")?,
                )
                .context("invalid credential encoding")?,
            )));
        }
        if !result.locked.is_empty() {
            bail!("desktop keyring is locked; unlock it and reconnect");
        }
        Ok(None)
    }
    async fn save(&self, key: &str, value: SecretString) -> Result<()> {
        let ss = SecretService::connect(EncryptionType::Dh)
            .await
            .context("desktop Secret Service unavailable")?;
        let collection = ss
            .get_default_collection()
            .await
            .context("no default desktop keyring")?;
        if collection.is_locked().await? {
            collection
                .unlock()
                .await
                .context("unlock the desktop keyring to save Cirrove credentials")?;
        }
        collection
            .create_item(
                "Cirrove Microsoft account",
                attributes(key),
                value.expose_secret().as_bytes(),
                true,
                "application/json",
            )
            .await
            .context("cannot save Cirrove credential")?;
        Ok(())
    }
    async fn remove(&self, key: &str) -> Result<()> {
        let ss = SecretService::connect(EncryptionType::Dh)
            .await
            .context("desktop Secret Service unavailable")?;
        let result = ss.search_items(attributes(key)).await?;
        if !result.locked.is_empty() {
            bail!("unlock the desktop keyring before removing this account");
        }
        for item in result.unlocked {
            item.delete().await?;
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct TokenReply {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
    id_token: Option<String>,
    token_type: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct MicrosoftClaims {
    tid: String,
    #[serde(default)]
    preferred_username: String,
}
impl AdditionalClaims for MicrosoftClaims {}
type MicrosoftIdToken = IdToken<
    MicrosoftClaims,
    CoreGenderClaim,
    CoreJweContentEncryptionAlgorithm,
    CoreJwsSigningAlgorithm,
>;

pub struct PendingLogin {
    app: AppRegistration,
    listener: TcpListener,
    url: Url,
    redirect: Url,
    state: CsrfToken,
    nonce: Nonce,
    verifier: PkceCodeVerifier,
    client: Client,
}
impl PendingLogin {
    pub async fn new(app: AppRegistration) -> Result<Self> {
        app.validate()?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        // Microsoft ignores localhost redirect ports; register http://localhost.
        let redirect = Url::parse(&format!(
            "http://localhost:{}/",
            listener.local_addr()?.port()
        ))?;
        let state = CsrfToken::new_random();
        let nonce = Nonce::new_random();
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let mut url = Url::parse(&app.endpoint("authorize"))?;
        url.query_pairs_mut().extend_pairs([
            ("client_id", app.client_id.as_str()),
            ("response_type", "code"),
            ("redirect_uri", redirect.as_str()),
            ("response_mode", "query"),
            ("scope", SCOPES),
            ("state", state.secret()),
            ("nonce", nonce.secret()),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("prompt", "select_account"),
        ]);
        Ok(Self {
            app,
            listener,
            url,
            redirect,
            state,
            nonce,
            verifier,
            client: http_client()?,
        })
    }
    /// Pass directly to the browser; never log or persist this URL.
    pub fn authorization_url(&self) -> &Url {
        &self.url
    }
    pub async fn finish(self) -> Result<(Identity, Credentials)> {
        tokio::time::timeout(Duration::from_secs(600), self.finish_inner())
            .await
            .context("sign-in timed out; try connecting again")?
    }
    async fn finish_inner(self) -> Result<(Identity, Credentials)> {
        let code = loop {
            let (mut socket, _) = self.listener.accept().await?;
            let request = tokio::time::timeout(Duration::from_secs(3), async {
                let mut request = Vec::new();
                let mut buf = [0u8; 1024];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = socket.read(&mut buf).await?;
                    if n == 0 || request.len() + n > 32768 {
                        bail!("invalid callback");
                    }
                    request.extend_from_slice(&buf[..n]);
                }
                Ok::<_, anyhow::Error>(String::from_utf8(request)?)
            })
            .await;
            let outcome = request
                .ok()
                .and_then(|v| v.ok())
                .and_then(|r| parse_callback(&r, &self.redirect, self.state.secret()));
            let message = if outcome.is_some() {
                "Sign-in received. Return to Cirrove to select your drive. You can close this tab."
            } else {
                "This request is not a valid Cirrove sign-in callback."
            };
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{message}",
                message.len()
            );
            let _ =
                tokio::time::timeout(Duration::from_secs(2), socket.write_all(reply.as_bytes()))
                    .await;
            if let Some(outcome) = outcome {
                break outcome?;
            }
        };
        let reply: TokenReply = bounded_json(
            self.client
                .post(self.app.endpoint("token"))
                .form(&[
                    ("client_id", self.app.client_id.as_str()),
                    ("grant_type", "authorization_code"),
                    ("code", code.as_str()),
                    ("redirect_uri", self.redirect.as_str()),
                    ("code_verifier", self.verifier.secret()),
                    ("scope", SCOPES),
                ])
                .send()
                .await
                .map_err(|_| anyhow::anyhow!("Microsoft token endpoint unavailable"))?,
        )
        .await?;
        let raw = reply
            .id_token
            .as_deref()
            .context("Microsoft did not provide an identity token")?;
        // Unverified tenant is only a routing hint. Accept it only after signature,
        // issuer, audience, expiry and nonce validation below.
        let payload = raw.split('.').nth(1).context("invalid identity token")?;
        let hint: MicrosoftClaims = serde_json::from_slice(
            &URL_SAFE_NO_PAD
                .decode(payload)
                .map_err(|_| anyhow::anyhow!("invalid identity token"))?,
        )
        .map_err(|_| anyhow::anyhow!("identity token missing tenant"))?;
        let tenant = uuid::Uuid::parse_str(&hint.tid)
            .context("invalid identity tenant")?
            .to_string();
        if uuid::Uuid::parse_str(&self.app.authority).is_ok()
            && !tenant.eq_ignore_ascii_case(&self.app.authority)
        {
            bail!("signed-in tenant does not match the selected tenant");
        }
        let keys: CoreJsonWebKeySet = bounded_json(
            self.client
                .get("https://login.microsoftonline.com/common/discovery/v2.0/keys")
                .send()
                .await
                .map_err(|_| anyhow::anyhow!("identity keys unavailable"))?,
        )
        .await?;
        let (subject, username) =
            verify_id_token(raw, &tenant, &self.app.client_id, &self.nonce, keys)?;
        let user = graph_identity(&self.client, &reply.access_token).await?;
        let identity = Identity {
            tenant_id: tenant,
            subject,
            username: if user.user_principal_name.is_empty() {
                username
            } else {
                user.user_principal_name
            },
            graph_user_id: user.id,
            display_name: user.display_name,
        };
        let credentials = credentials(reply, None)?;
        Ok((identity, credentials))
    }
}
fn verify_id_token(
    raw: &str,
    tenant: &str,
    client_id: &str,
    nonce: &Nonce,
    keys: CoreJsonWebKeySet,
) -> Result<(String, String)> {
    let verifier = CoreIdTokenVerifier::new_public_client(
        ClientId::new(client_id.to_owned()),
        IssuerUrl::new(format!("https://login.microsoftonline.com/{tenant}/v2.0"))?,
        keys,
    )
    .set_allowed_algs([CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256]);
    let token: MicrosoftIdToken = raw
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid identity token"))?;
    let claims = token
        .claims(&verifier, nonce)
        .map_err(|_| anyhow::anyhow!("Microsoft identity verification failed"))?;
    if claims.additional_claims().tid != tenant {
        bail!("Microsoft identity tenant mismatch");
    }
    Ok((
        claims.subject().as_str().to_owned(),
        claims.additional_claims().preferred_username.clone(),
    ))
}
fn parse_callback(request: &str, redirect: &Url, state: &str) -> Option<Result<String>> {
    let mut lines = request.lines();
    let line = lines.next()?;
    let mut fields = line.split_whitespace();
    if fields.next() != Some("GET") {
        return None;
    }
    let target = fields.next()?;
    if !target.starts_with("/?") {
        return None;
    }
    let host = lines.find_map(|l| {
        l.split_once(':')
            .filter(|(k, _)| k.eq_ignore_ascii_case("host"))
            .map(|(_, v)| v.trim())
    })?;
    if host != format!("localhost:{}", redirect.port()?) {
        return None;
    }
    let parsed = redirect.join(target).ok()?;
    let pairs: Vec<_> = parsed.query_pairs().collect();
    let values = |key: &str| {
        pairs
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.as_ref())
            .collect::<Vec<_>>()
    };
    let states = values("state");
    if states.len() != 1 || !constant_time_eq(states[0].as_bytes(), state.as_bytes()) {
        return None;
    }
    if !values("error").is_empty() {
        return Some(Err(anyhow::anyhow!(
            "Microsoft sign-in was cancelled or denied"
        )));
    }
    let codes = values("code");
    if codes.len() != 1 || codes[0].is_empty() {
        return None;
    }
    Some(Ok(codes[0].to_owned()))
}
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    a.ct_eq(b).into()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphUser {
    id: String,
    #[serde(default)]
    user_principal_name: String,
    #[serde(default)]
    display_name: String,
}
async fn graph_identity(client: &Client, token: &str) -> Result<GraphUser> {
    graph_identity_at(
        client,
        token,
        "https://graph.microsoft.com/v1.0/me?$select=id,displayName,userPrincipalName",
    )
    .await
}
async fn graph_identity_at(client: &Client, token: &str, endpoint: &str) -> Result<GraphUser> {
    bounded_json(
        client
            .get(endpoint)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| anyhow::Error::from(ProviderError::Unavailable))?,
    )
    .await
}
fn credentials(reply: TokenReply, previous: Option<&Credentials>) -> Result<Credentials> {
    if !reply.token_type.eq_ignore_ascii_case("bearer") || reply.access_token.is_empty() {
        bail!("invalid Microsoft token type");
    }
    Ok(Credentials {
        access_token: reply.access_token,
        refresh_token: reply
            .refresh_token
            .or_else(|| previous.map(|p| p.refresh_token.clone()))
            .filter(|t| !t.is_empty())
            .context("Microsoft did not grant offline access")?,
        expires_at: now().saturating_add(reply.expires_in),
    })
}
pub async fn save_credentials(
    vault: &dyn CredentialVault,
    key: &str,
    credentials: &Credentials,
) -> Result<()> {
    let encoded = SecretString::from(serde_json::to_string(credentials)?);
    tokio::time::timeout(Duration::from_secs(30), vault.save(key, encoded))
        .await
        .context("desktop keyring operation timed out")?
}

/// A single instance is shared by every drive and content operation for an account.
/// A process-wide account-owner lock is held by the daemon (see service accounts).
pub struct TokenBroker {
    app: AppRegistration,
    token_url: String,
    identity_url: String,
    identity: Identity,
    key: String,
    vault: Arc<dyn CredentialVault>,
    client: Client,
    gate: Mutex<()>,
    memory: Mutex<Option<Credentials>>,
}
impl TokenBroker {
    pub fn new(
        app: AppRegistration,
        identity: Identity,
        key: String,
        vault: Arc<dyn CredentialVault>,
    ) -> Result<Self> {
        app.validate()?;
        Ok(Self {
            token_url: app.endpoint("token"),
            identity_url:
                "https://graph.microsoft.com/v1.0/me?$select=id,displayName,userPrincipalName"
                    .into(),
            app,
            identity,
            key,
            vault,
            client: http_client()?,
            gate: Mutex::new(()),
            memory: Mutex::new(None),
        })
    }
    pub async fn invalidate(&self, rejected: &SecretString) {
        if let Some(c) = self.memory.lock().await.as_mut() {
            // A delayed 401 for the old token must not invalidate a newer grant.
            if constant_time_eq(
                c.access_token.as_bytes(),
                rejected.expose_secret().as_bytes(),
            ) {
                c.expires_at = 0;
            }
        }
    }
    async fn token(&self) -> Result<SecretString> {
        let _guard = self.gate.lock().await;
        let mut state = self.memory.lock().await;
        if state.is_none() {
            let encoded = self
                .vault
                .load(&self.key)
                .await?
                .context("Cirrove credential missing; sign in again")?;
            *state = Some(
                serde_json::from_str(encoded.expose_secret())
                    .map_err(|_| anyhow::anyhow!("invalid Cirrove credential"))?,
            );
        }
        let previous = state.as_ref().context("credential unavailable")?;
        if previous.expires_at > now().saturating_add(90) {
            return Ok(previous.access_token());
        }
        let response = self
            .client
            .post(&self.token_url)
            .form(&[
                ("client_id", self.app.client_id.as_str()),
                ("grant_type", "refresh_token"),
                ("refresh_token", previous.refresh_token.as_str()),
                ("scope", SCOPES),
            ])
            .send()
            .await
            .map_err(|_| anyhow::Error::from(ProviderError::Unavailable))?;
        let reply: TokenReply = bounded_json(response).await?;
        let user = graph_identity_at(&self.client, &reply.access_token, &self.identity_url).await?;
        if user.id != self.identity.graph_user_id {
            bail!("refreshed token belongs to a different Microsoft account");
        }
        let next = credentials(reply, Some(previous))?;
        // Persist rotation before using the new token; a keyring failure is visible.
        save_credentials(self.vault.as_ref(), &self.key, &next).await?;
        let token = next.access_token();
        *state = Some(next);
        Ok(token)
    }
}
#[async_trait]
impl TokenSource for TokenBroker {
    async fn access_token(&self) -> std::result::Result<SecretString, ProviderError> {
        tokio::time::timeout(Duration::from_secs(40), self.token())
            .await
            .map_err(|_| ProviderError::Unavailable)?
            .map_err(|e| {
                e.downcast::<ProviderError>()
                    .unwrap_or(ProviderError::Authentication)
            })
    }
    async fn invalidate(&self, rejected: &SecretString) {
        TokenBroker::invalidate(self, rejected).await;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[test]
    fn callback_requires_bound_host_single_matching_state_and_code() {
        let url = Url::parse("http://localhost:1234/").unwrap();
        let good = "GET /?code=hello&state=expected HTTP/1.1\r\nHost: localhost:1234\r\n\r\n";
        assert_eq!(
            parse_callback(good, &url, "expected").unwrap().unwrap(),
            "hello"
        );
        for bad in [
            good.replace("expected", "wrong"),
            good.replace("state=expected", "state=expected&state=expected"),
            good.replace("localhost:1234", "evil.example"),
            good.replace("GET", "POST"),
        ] {
            assert!(parse_callback(&bad, &url, "expected").is_none());
        }
    }
    #[tokio::test]
    async fn authorization_requests_pkce_nonce_read_only_scopes_and_account_picker() {
        let pending = PendingLogin::new(AppRegistration {
            client_id: uuid::Uuid::new_v4().to_string(),
            authority: "common".into(),
        })
        .await
        .unwrap();
        let pairs: HashMap<_, _> = pending.authorization_url().query_pairs().collect();
        assert_eq!(pairs["code_challenge_method"], "S256");
        assert_eq!(pairs["prompt"], "select_account");
        assert!(!pairs["scope"].contains("Write"));
        assert!(!pairs["nonce"].is_empty());
    }
    #[test]
    fn app_registration_does_not_accept_arbitrary_authority_urls() {
        let app = AppRegistration {
            client_id: uuid::Uuid::new_v4().to_string(),
            authority: "https://evil.example".into(),
        };
        assert!(app.validate().is_err());
    }
    #[test]
    fn microsoft_identity_requires_signature_issuer_audience_nonce_and_expiry() {
        use openidconnect::{
            Audience, IdTokenClaims, PrivateSigningKey, StandardClaims, SubjectIdentifier,
            core::CoreRsaPrivateSigningKey,
        };
        let key = CoreRsaPrivateSigningKey::from_pem(
            include_str!("../tests/fixtures/synthetic-rsa.pem"),
            None,
        )
        .unwrap();
        let keys = CoreJsonWebKeySet::new(vec![key.as_verification_key()]);
        let tenant = "00000000-0000-4000-8000-000000000001";
        let client = "00000000-0000-4000-8000-000000000002";
        let nonce = Nonce::new("synthetic-nonce".into());
        let signed = |issuer: String,
                      audience: &str,
                      nonce: Nonce,
                      expires: chrono::DateTime<chrono::Utc>| {
            MicrosoftIdToken::new(
                IdTokenClaims::new(
                    IssuerUrl::new(issuer).unwrap(),
                    vec![Audience::new(audience.into())],
                    expires,
                    chrono::Utc::now(),
                    StandardClaims::new(SubjectIdentifier::new("synthetic-subject".into())),
                    MicrosoftClaims {
                        tid: tenant.into(),
                        preferred_username: "fixture@example.invalid".into(),
                    },
                )
                .set_nonce(Some(nonce)),
                &key,
                CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256,
                None,
                None,
            )
            .unwrap()
            .to_string()
        };
        let issuer = format!("https://login.microsoftonline.com/{tenant}/v2.0");
        let future = chrono::Utc::now() + chrono::Duration::minutes(5);
        let good = signed(issuer.clone(), client, nonce.clone(), future);
        assert_eq!(
            verify_id_token(&good, tenant, client, &nonce, keys.clone())
                .unwrap()
                .0,
            "synthetic-subject"
        );
        for bad in [
            signed("https://evil.example".into(), client, nonce.clone(), future),
            signed(issuer.clone(), "other-client", nonce.clone(), future),
            signed(issuer.clone(), client, Nonce::new("wrong".into()), future),
            signed(
                issuer,
                client,
                nonce.clone(),
                chrono::Utc::now() - chrono::Duration::minutes(5),
            ),
        ] {
            assert!(verify_id_token(&bad, tenant, client, &nonce, keys.clone()).is_err());
        }
        let mut parts: Vec<_> = good.split('.').map(str::to_owned).collect();
        parts[1] = URL_SAFE_NO_PAD.encode(br#"{"sub":"attacker"}"#);
        assert!(verify_id_token(&parts.join("."), tenant, client, &nonce, keys).is_err());
    }
    #[derive(Default)]
    struct MemoryVault {
        value: Mutex<Option<SecretString>>,
        fail_save: std::sync::atomic::AtomicBool,
    }
    #[async_trait]
    impl CredentialVault for MemoryVault {
        async fn load(&self, _: &str) -> Result<Option<SecretString>> {
            Ok(self.value.lock().await.clone())
        }
        async fn save(&self, _: &str, value: SecretString) -> Result<()> {
            if self.fail_save.load(std::sync::atomic::Ordering::SeqCst) {
                bail!("synthetic keyring unavailable");
            }
            *self.value.lock().await = Some(value);
            Ok(())
        }
        async fn remove(&self, _: &str) -> Result<()> {
            *self.value.lock().await = None;
            Ok(())
        }
    }
    async fn broker_fixture() -> (
        Arc<TokenBroker>,
        Arc<MemoryVault>,
        Arc<std::sync::atomic::AtomicUsize>,
        tokio::task::JoinHandle<()>,
    ) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let count = requests.clone();
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let count = count.clone();
                tokio::spawn(async move {
                    let mut bytes = vec![];
                    let mut chunk = [0u8; 1024];
                    loop {
                        let read = socket.read(&mut chunk).await.unwrap();
                        if read == 0 {
                            return;
                        }
                        bytes.extend_from_slice(&chunk[..read]);
                        if bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                        assert!(bytes.len() < 16384);
                    }
                    let request = String::from_utf8_lossy(&bytes);
                    let body = if request.starts_with("POST /token ") {
                        count.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(30)).await;
                        r#"{"token_type":"Bearer","access_token":"synthetic-fresh","refresh_token":"synthetic-rotated","expires_in":3600}"#
                    } else {
                        r#"{"id":"synthetic-user","displayName":"Synthetic","userPrincipalName":"fixture@example.invalid"}"#
                    };
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    socket.write_all(response.as_bytes()).await.unwrap();
                });
            }
        });
        let vault = Arc::new(MemoryVault::default());
        vault
            .save(
                "key",
                SecretString::from(
                    serde_json::to_string(&Credentials {
                        access_token: "synthetic-expired".into(),
                        refresh_token: "synthetic-old-refresh".into(),
                        expires_at: 0,
                    })
                    .unwrap(),
                ),
            )
            .await
            .unwrap();
        let mut broker = TokenBroker::new(
            AppRegistration {
                client_id: uuid::Uuid::new_v4().to_string(),
                authority: "common".into(),
            },
            Identity {
                tenant_id: uuid::Uuid::new_v4().to_string(),
                subject: "subject".into(),
                username: "fixture@example.invalid".into(),
                graph_user_id: "synthetic-user".into(),
                display_name: "Synthetic".into(),
            },
            "key".into(),
            vault.clone(),
        )
        .unwrap();
        broker.token_url = format!("{base}/token");
        broker.identity_url = format!("{base}/me");
        (Arc::new(broker), vault, requests, server)
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn simultaneous_expiry_rotates_once_and_stale_401_cannot_expire_new_token() {
        use std::sync::atomic::Ordering;
        let (broker, vault, requests, server) = broker_fixture().await;
        let mut jobs = tokio::task::JoinSet::new();
        for _ in 0..32 {
            let broker = broker.clone();
            jobs.spawn(async move { broker.access_token().await.unwrap() });
        }
        while let Some(result) = jobs.join_next().await {
            assert_eq!(result.unwrap().expose_secret(), "synthetic-fresh");
        }
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        let stored = vault.load("key").await.unwrap().unwrap();
        let decoded: Credentials = serde_json::from_str(stored.expose_secret()).unwrap();
        assert_eq!(decoded.refresh_token, "synthetic-rotated");
        broker
            .invalidate(&SecretString::from("synthetic-expired"))
            .await;
        broker.access_token().await.unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        server.abort();
        let _ = server.await;
    }
    #[tokio::test]
    async fn credentials_are_not_used_when_rotation_cannot_be_persisted() {
        let (broker, vault, _, server) = broker_fixture().await;
        vault
            .fail_save
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(broker.access_token().await.is_err());
        let stored = vault.load("key").await.unwrap().unwrap();
        let decoded: Credentials = serde_json::from_str(stored.expose_secret()).unwrap();
        assert_eq!(decoded.refresh_token, "synthetic-old-refresh");
        server.abort();
        let _ = server.await;
    }
}
