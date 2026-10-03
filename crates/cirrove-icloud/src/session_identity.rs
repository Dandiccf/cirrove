//! Stable account identity captured only by the successful authentication path.
use serde_json::Value;
use sha2::{Digest, Sha256};

pub(super) fn dsid_hash(value: &Value) -> Option<String> {
    let id = value.as_str()?;
    // Public AccountInfo declares an opaque string, not an integer. Preserve
    // exact spelling; reject malformed/unbounded identifiers without guessing
    // a numeric conversion or treating a contact address as a stable identity.
    if id.is_empty() || id.len() > 256 || id.trim() != id || id.chars().any(char::is_control) {
        return None;
    }
    let mut digest = Sha256::new();
    digest.update(b"cirrove:icloud:authenticated-dsid:v1\0");
    digest.update(id.as_bytes());
    Some(hex::encode(digest.finalize()))
}
pub(super) fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AccountInfo, ICloudReadSession, account_hash};
    use secrecy::{ExposeSecret, SecretString};
    fn login(dsid: Value) -> AccountInfo {
        serde_json::from_value(serde_json::json!({"dsInfo":{"dsid":dsid},"webservices":{"drivews":{"url":"https://drivews.icloud.com/"},"docws":{"url":"https://docws.icloud.com/"}}})).expect("synthetic login")
    }
    #[test]
    fn trusted_dsid_capture_only_after_successful_login_metadata() {
        let mut session = ICloudReadSession::new().expect("session");
        session
            .accept_account_login(login(Value::String("synthetic-dsid".into())))
            .expect("login");
        assert_eq!(
            session.trusted_dsid_hash,
            dsid_hash(&Value::String("synthetic-dsid".into()))
        );
        let saved = session.trusted_dsid_hash.clone();
        let invalid: AccountInfo =
            serde_json::from_value(serde_json::json!({"dsInfo":{"dsid":"other"},"webservices":{}}))
                .expect("incomplete login");
        assert!(session.accept_account_login(invalid).is_err());
        assert_eq!(session.trusted_dsid_hash, saved);
        for missing in [
            Value::Null,
            Value::Bool(true),
            Value::Number(123.into()),
            Value::String(String::new()),
            Value::String("x".repeat(257)),
            Value::String(" x".into()),
            Value::String("x\ny".into()),
        ] {
            assert!(dsid_hash(&missing).is_none());
            session
                .accept_account_login(login(missing))
                .expect("ordinary Drive still connects without optional anchor");
            assert!(session.trusted_dsid_hash.is_none());
        }
    }
    #[test]
    fn trusted_dsid_snapshot_roundtrip_and_legacy_absence() {
        let mut session = ICloudReadSession::new().expect("session");
        session.account_hash = Some(account_hash("alias@example.invalid").expect("login binding"));
        session.headers.session_token = "synthetic-session".into();
        session
            .accept_account_login(login(Value::String("synthetic-dsid".into())))
            .expect("login");
        let snapshot = session.session_snapshot().expect("snapshot");
        let restored = ICloudReadSession::from_session_snapshot(&snapshot, "alias@example.invalid")
            .expect("restored");
        assert_eq!(restored.trusted_dsid_hash, session.trusted_dsid_hash);
        assert_ne!(
            restored.trusted_dsid_hash.as_deref(),
            Some("synthetic-dsid")
        );
        assert!(
            ICloudReadSession::from_session_snapshot(&snapshot, "wrong@example.invalid").is_err()
        );
        let mut legacy: Value =
            serde_json::from_str(snapshot.expose_secret()).expect("synthetic snapshot JSON");
        legacy
            .as_object_mut()
            .expect("object")
            .remove("trusted_dsid_hash");
        let legacy_secret = SecretString::from(legacy.to_string());
        assert!(
            ICloudReadSession::from_session_snapshot(&legacy_secret, "alias@example.invalid")
                .expect("legacy supported")
                .trusted_dsid_hash
                .is_none()
        );
        for bad in ["short", &"A".repeat(64), &"g".repeat(64)] {
            legacy["trusted_dsid_hash"] = bad.into();
            assert!(
                ICloudReadSession::from_session_snapshot(
                    &SecretString::from(legacy.to_string()),
                    "alias@example.invalid"
                )
                .is_err()
            );
        }
        assert!(restored.read_only_fork().trusted_dsid_hash.is_none());
    }
    #[tokio::test]
    async fn trusted_dsid_reused_session_failed_signin_invalidates_completed_authentication() {
        // Keep a loopback listener open without serving TLS: startup times out
        // locally and must never reach an Apple endpoint.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("local listener");
        let mut session = ICloudReadSession::new().expect("session");
        session.account_hash = Some(account_hash("first@example.invalid").expect("first account"));
        session.headers.session_token = "first-authenticated-token".into();
        session.headers.trust_token = "first-trust-token".into();
        session.headers.session_id = "first-session-id".into();
        session.headers.scnt = "first-scnt".into();
        session.headers.account_country = "first-country".into();
        session.headers.auth_attributes = "first-auth-attributes".into();
        session
            .accept_account_login(login(Value::String("first-stable-id".into())))
            .expect("completed first account");
        assert!(session.session_snapshot().is_ok());
        session.http = reqwest::Client::builder()
            .no_proxy()
            .resolve(
                "idmsa.apple.com",
                listener.local_addr().expect("local address"),
            )
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_millis(100))
            .build()
            .expect("local timeout client");
        assert!(
            session
                .sign_in(
                    "second@example.invalid",
                    &SecretString::from("synthetic-password")
                )
                .await
                .is_err()
        );
        assert_eq!(
            session.account_hash,
            Some(account_hash("second@example.invalid").expect("second account"))
        );
        assert!(session.trusted_dsid_hash.is_none());
        assert!(session.drive_endpoint.is_none() && session.docs_endpoint.is_none());
        assert!(session.headers.session_token.is_empty());
        assert!(session.headers.trust_token.is_empty());
        assert!(session.headers.session_id.is_empty());
        assert!(session.headers.scnt.is_empty());
        assert!(session.headers.account_country.is_empty());
        assert!(session.headers.auth_attributes.is_empty());
        assert!(session.session_snapshot().is_err());
        // The empty token gate fails before setup transport is even constructed.
        assert_eq!(
            session
                .account_login()
                .await
                .expect_err("incomplete login")
                .to_string(),
            "Apple did not issue an account session"
        );
    }
}
