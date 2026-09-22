//! Google-specific OAuth/OIDC endpoints, scopes and claims. The callback, PKCE,
//! keyring and serialized refresh lifecycle are shared with Microsoft.
use super::*;
use openidconnect::core::CoreIdToken;

/// Public Desktop OAuth client ID for Cirrove's Google project. The Cloud app
/// remains limited to approved testers until its production review is complete.
pub const CIRROVE_DESKTOP_CLIENT_ID: &str =
    "608639658219-mg7dhl8lappv8sstdlbv0k00vtoos9sl.apps.googleusercontent.com";

pub const SCOPES: &str = "openid email profile https://www.googleapis.com/auth/drive.readonly";
/// A writable filesystem must be able to update existing files, not only files
/// created by Cirrove. Google limits `drive.file` to app-created or explicitly
/// selected files, so a whole-My-Drive writable mount requires the full Drive
/// scope. It is requested only by the explicit `--write-access` flow.
pub const WRITE_SCOPES: &str = "openid email profile https://www.googleapis.com/auth/drive";

pub(super) fn validate_grant(access: AccessMode, granted: Option<&str>) -> Result<()> {
    if let Some(granted) = granted {
        let scopes: Vec<_> = granted.split_ascii_whitespace().collect();
        let full = scopes.contains(&"https://www.googleapis.com/auth/drive");
        let readable = full || scopes.contains(&"https://www.googleapis.com/auth/drive.readonly");
        if !readable || (access == AccessMode::ReadWrite && !full) {
            return Err(ProviderError::Authentication.into());
        }
        if !scopes.contains(&"openid") {
            return Err(ProviderError::Authentication.into());
        }
    }
    Ok(())
}

/// Google's downloaded Desktop app client JSON. Never serialize or Debug this
/// object: its client secret is kept with the refresh grant in Secret Service.
pub struct DesktopClient {
    pub registration: AppRegistration,
    pub secret: Option<SecretString>,
}
impl DesktopClient {
    pub fn load(path: &std::path::Path) -> Result<Self> {
        use std::{
            io::Read,
            os::unix::fs::{MetadataExt, PermissionsExt},
        };
        let file = std::fs::File::open(path).context("cannot open Google desktop client file")?;
        let meta = file.metadata()?;
        if !meta.is_file()
            || meta.permissions().mode() & 0o077 != 0
            || meta.uid() != std::fs::metadata("/proc/self")?.uid()
        {
            bail!("Google desktop client file must be an owned private regular file (chmod 600)");
        }
        let mut bytes = Vec::new();
        file.take(65537).read_to_end(&mut bytes)?;
        if bytes.len() > 65536 {
            bail!("Google desktop client file exceeds 64 KiB");
        }
        #[derive(Deserialize)]
        struct ClientFile {
            installed: Installed,
        }
        #[derive(Deserialize)]
        struct Installed {
            client_id: String,
            client_secret: Option<String>,
        }
        let client: ClientFile = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("expected Google OAuth client JSON for a Desktop app"))?;
        let registration = AppRegistration::Google {
            client_id: client.installed.client_id,
        };
        registration.validate()?;
        Ok(Self {
            registration,
            secret: client
                .installed
                .client_secret
                .filter(|s| !s.is_empty())
                .map(SecretString::from),
        })
    }
}

pub(super) async fn login_identity(
    client: &Client,
    reply: &TokenReply,
    client_id: &str,
    nonce: &Nonce,
) -> Result<Identity> {
    let raw = reply
        .id_token
        .as_deref()
        .context("Google did not provide an identity token")?;
    let keys: CoreJsonWebKeySet = bounded_json(
        client
            .get("https://www.googleapis.com/oauth2/v3/certs")
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("Google identity keys unavailable"))?,
    )
    .await?;
    let identity = verify_identity(raw, client_id, nonce, keys)?;
    // Bind the supplied access token to the signed subject, as on Microsoft.
    let subject = subject_at(
        client,
        &reply.access_token,
        "https://openidconnect.googleapis.com/v1/userinfo",
    )
    .await?;
    if subject != identity.subject {
        bail!("Google access token and signed identity disagree");
    }
    Ok(identity)
}
fn verify_identity(
    raw: &str,
    client_id: &str,
    nonce: &Nonce,
    keys: CoreJsonWebKeySet,
) -> Result<Identity> {
    let token: CoreIdToken = raw
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid Google identity token"))?;
    let verifier = CoreIdTokenVerifier::new_public_client(
        ClientId::new(client_id.into()),
        IssuerUrl::new("https://accounts.google.com".into())?,
        keys,
    )
    .set_allowed_algs([CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256]);
    let claims = token
        .claims(&verifier, nonce)
        .map_err(|_| anyhow::anyhow!("Google identity verification failed"))?;
    let subject = claims.subject().as_str().to_owned();
    if subject.is_empty() || claims.email_verified() != Some(true) {
        bail!("Google identity has no verified email");
    }
    let username = claims
        .email()
        .context("Google identity has no email")?
        .as_str()
        .to_owned();
    let display_name = claims
        .name()
        .and_then(|n| n.get(None))
        .map(|n| n.as_str().to_owned())
        .unwrap_or_else(|| username.clone());
    Ok(Identity {
        tenant_id: String::new(),
        subject: subject.clone(),
        username,
        // Kept under the legacy field spelling for status compatibility. Google
        // uses its immutable OIDC subject, never a fabricated Microsoft identity.
        graph_user_id: subject,
        display_name,
    })
}
pub(super) async fn subject_at(client: &Client, token: &str, endpoint: &str) -> Result<String> {
    #[derive(Deserialize)]
    struct User {
        sub: String,
    }
    let user: User = bounded_json(
        client
            .get(endpoint)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| anyhow::Error::from(ProviderError::Unavailable))?,
    )
    .await?;
    if user.sub.is_empty() {
        bail!("Google returned an empty account identity");
    }
    Ok(user.sub)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn google_login_uses_pkce_loopback_offline_consent_and_explicit_scopes() {
        let app = AppRegistration::Google {
            client_id: "123-example.apps.googleusercontent.com".into(),
        };
        let pending = PendingLogin::new(app.clone()).await.unwrap();
        assert_eq!(pending.url.host_str(), Some("accounts.google.com"));
        assert_eq!(pending.redirect.host_str(), Some("127.0.0.1"));
        let q: HashMap<_, _> = pending.url.query_pairs().collect();
        assert_eq!(q.get("scope").unwrap(), SCOPES);
        assert_eq!(q.get("access_type").unwrap(), "offline");
        assert_eq!(q.get("code_challenge_method").unwrap(), "S256");
        assert_eq!(q.get("prompt").unwrap(), "consent select_account");
        let write = PendingLogin::with_access(app, AccessMode::ReadWrite)
            .await
            .unwrap();
        let write_query: HashMap<_, _> = write.url.query_pairs().collect();
        assert_eq!(write_query.get("scope").unwrap(), WRITE_SCOPES);
        assert!(
            validate_grant(
                AccessMode::ReadOnly,
                Some("openid https://www.googleapis.com/auth/drive.file")
            )
            .is_err()
        );
        assert!(validate_grant(AccessMode::ReadOnly, Some(SCOPES)).is_ok());
        assert!(validate_grant(AccessMode::ReadWrite, Some(SCOPES)).is_err());
        assert!(
            validate_grant(
                AccessMode::ReadWrite,
                Some("openid https://www.googleapis.com/auth/drive.readonly https://www.googleapis.com/auth/drive.file")
            )
            .is_err()
        );
        assert!(validate_grant(AccessMode::ReadWrite, Some(WRITE_SCOPES)).is_ok());
        assert!(
            validate_grant(
                AccessMode::ReadWrite,
                Some("openid https://www.googleapis.com/auth/drive")
            )
            .is_ok()
        );
    }
    #[test]
    fn google_identity_requires_signature_issuer_audience_nonce_expiry_and_verified_email() {
        use openidconnect::{
            Audience, EmptyAdditionalClaims, EndUserEmail, IdTokenClaims, PrivateSigningKey,
            StandardClaims, SubjectIdentifier, core::CoreRsaPrivateSigningKey,
        };
        let key = CoreRsaPrivateSigningKey::from_pem(
            include_str!("../tests/fixtures/synthetic-rsa.pem"),
            None,
        )
        .unwrap();
        let keys = CoreJsonWebKeySet::new(vec![key.as_verification_key()]);
        let nonce = Nonce::new("synthetic-nonce".into());
        let client = "123-example.apps.googleusercontent.com";
        let signed = |issuer: &str, audience: &str, nonce: Nonce, expiry, verified| {
            CoreIdToken::new(
                IdTokenClaims::new(
                    IssuerUrl::new(issuer.into()).unwrap(),
                    vec![Audience::new(audience.into())],
                    expiry,
                    chrono::Utc::now(),
                    StandardClaims::new(SubjectIdentifier::new("synthetic-user".into()))
                        .set_email(Some(EndUserEmail::new("fixture@example.invalid".into())))
                        .set_email_verified(Some(verified)),
                    EmptyAdditionalClaims {},
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
        let future = chrono::Utc::now() + chrono::Duration::minutes(5);
        let good = signed(
            "https://accounts.google.com",
            client,
            nonce.clone(),
            future,
            true,
        );
        assert_eq!(
            verify_identity(&good, client, &nonce, keys.clone())
                .unwrap()
                .subject,
            "synthetic-user"
        );
        for bad in [
            signed("https://evil.invalid", client, nonce.clone(), future, true),
            signed(
                "https://accounts.google.com",
                "other-client",
                nonce.clone(),
                future,
                true,
            ),
            signed(
                "https://accounts.google.com",
                client,
                Nonce::new("wrong".into()),
                future,
                true,
            ),
            signed(
                "https://accounts.google.com",
                client,
                nonce.clone(),
                chrono::Utc::now() - chrono::Duration::minutes(5),
                true,
            ),
            signed(
                "https://accounts.google.com",
                client,
                nonce.clone(),
                future,
                false,
            ),
        ] {
            assert!(verify_identity(&bad, client, &nonce, keys.clone()).is_err());
        }
        let mut parts: Vec<_> = good.split('.').map(str::to_owned).collect();
        parts[1] = URL_SAFE_NO_PAD.encode(br#"{"sub":"attacker"}"#);
        assert!(verify_identity(&parts.join("."), client, &nonce, keys).is_err());
    }
}
