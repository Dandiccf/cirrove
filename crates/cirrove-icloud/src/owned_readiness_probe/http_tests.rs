//! Synthetic TLS only; production URL roles and redirects remain unchanged.
use super::*;
use base64::Engine as _;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_rustls::{
    TlsAcceptor,
    rustls::{
        ServerConfig,
        pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    },
};

struct Fixture {
    session: ICloudReadSession,
    calls: Arc<AtomicUsize>,
    address: std::net::SocketAddr,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fixture {
    async fn new(replies: Vec<(u16, String, String)>) -> Self {
        Self::start(replies, false).await
    }
    async fn start(replies: Vec<(u16, String, String)>, login_first: bool) -> Self {
        let decode = base64::engine::general_purpose::STANDARD;
        let cert = decode
            .decode(include_str!("../package_create/fixtures/server-cert.b64").trim())
            .expect("fixture certificate");
        let key = decode
            .decode(include_str!("../package_create/fixtures/server-key.b64").trim())
            .expect("fixture key");
        let server = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(cert)],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
            )
            .expect("fixture TLS");
        let acceptor = TlsAcceptor::from(Arc::new(server));
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fixture listener");
        let address = listener.local_addr().expect("fixture address");
        let mut session = ICloudReadSession::new().expect("fixture session");
        session.account_hash =
            Some(account_hash("fixture@example.invalid").expect("fixture account"));
        // This fixture certificate has a different SAN. Only this synthetic
        // client's TLS validation is disabled, not production endpoint policy.
        session.http = reqwest::Client::builder()
            .no_proxy()
            .danger_accept_invalid_certs(true)
            .resolve("setup.icloud.com", address)
            .cookie_provider(session.cookies.clone())
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(2))
            .build()
            .expect("fixture client");
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let task = tokio::spawn(async move {
            for (index, (status, body, extra)) in replies.into_iter().enumerate() {
                tokio::time::timeout(Duration::from_secs(3), async {
                    let (socket, _) = listener.accept().await.expect("fixture accept");
                    let mut stream = acceptor.accept(socket).await.expect("fixture handshake");
                    let mut request = Vec::new();
                    loop {
                        let mut buffer = [0; 4096]; let read = stream.read(&mut buffer).await.expect("fixture read");
                        assert!(read > 0 && request.len() + read <= 16 * 1024);
                        request.extend_from_slice(&buffer[..read]);
                        if let Some(end) = request.windows(4).position(|v| v == b"\r\n\r\n") {
                            let headers = std::str::from_utf8(&request[..end]).expect("headers");
                            let length = headers.lines().find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length: ").map(str::to_owned)).map(|value| value.parse::<usize>().expect("length")).unwrap_or(0);
                            assert!(length <= 4096);
                            if request.len() >= end + 4 + length { break; }
                        }
                    }
                    let request = std::str::from_utf8(&request).expect("fixture request text");
                    if login_first && index == 0 {
                        assert!(request.starts_with("POST /setup/ws/1/accountLogin "));
                        let end = request.find("\r\n\r\n").expect("headers");
                        let payload: serde_json::Value = serde_json::from_str(&request[end+4..]).expect("synthetic login body");
                        assert_eq!(payload["dsWebAuthToken"], "synthetic-session");
                    } else {
                    assert!(request.starts_with("POST /setup/ws/1/validate?requestId="));
                    assert!(request.contains("clientBuildNumber=2636Build34"));
                    assert!(request.contains("clientMasteringNumber=2636Build34"));
                    assert!(!request.contains("dsWebAuthToken") && !request.contains("trustToken"));
                    let end = request.find("\r\n\r\n").expect("headers");
                    assert_eq!(request.len(), end + 4, "validate must be bodyless");
                    }
                    assert!(!request.to_ascii_lowercase().contains("authorization:"));
                    count.fetch_add(1, Ordering::SeqCst);
                    let response = format!("HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}", body.len());
                    stream.write_all(response.as_bytes()).await.expect("fixture response");
                    let _ = stream.shutdown().await;
                }).await.expect("bounded fixture request");
            }
        });
        Self {
            session,
            calls,
            address,
            task,
        }
    }
    fn configure_independent_session(&self, session: &mut ICloudReadSession) {
        session.http = reqwest::Client::builder()
            .no_proxy()
            .danger_accept_invalid_certs(true)
            .resolve("setup.icloud.com", self.address)
            .cookie_provider(session.cookies.clone())
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(2))
            .build()
            .expect("isolated fixture client");
    }
    fn make_legacy_snapshot_available(&mut self) {
        self.session.headers.session_token = "synthetic-session".into();
        self.session.accept_account_login(serde_json::from_value(serde_json::json!({"webservices":{"drivews":{"url":"https://drivews.icloud.com/"},"docws":{"url":"https://docws.icloud.com/"}}})).expect("synthetic initial login metadata")).expect("legacy session endpoints");
    }
    async fn observe(&self) -> Result<OwnedReadinessObservation> {
        self.session
            .probe_owned_pages_readiness("fixture@example.invalid", &CancellationToken::new())
            .await
    }
}

#[tokio::test]
async fn owned_readiness_http_bound_validate_boolean_output() {
    let body = serde_json::json!({"dsInfo":{"appleId":"fixture@example.invalid","isWebAccessAllowed":true},"webservices":{},"pcsServiceIdentitiesIncluded":false});
    let fixture = Fixture::new(vec![(200, body.to_string(), String::new())]).await;
    let observed = fixture.observe().await.expect("valid readiness");
    assert_eq!(observed.pcs_service_identities_included, Some(false));
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn owned_readiness_http_refuses_redirect_session_and_oversize_without_replay() {
    for (status, body, extra, expected) in [
        (
            302,
            String::new(),
            "Location: https://setup.icloud.com/unrelated\r\n".into(),
            "readiness validation refused (HTTP 302)",
        ),
        (
            421,
            "private-response".into(),
            String::new(),
            "readiness validation refused (HTTP 421)",
        ),
        (
            200,
            serde_json::json!({"dsInfo":{"appleId":"fixture@example.invalid"},"webservices":{},"ignored_padding":"x".repeat(256 * 1024 + 1)}).to_string(),
            String::new(),
            "readiness response invalid or oversized",
        ),
        (
            200,
            serde_json::json!({"dsInfo":{"appleId":"different@example.invalid"},"webservices":{}})
                .to_string(),
            String::new(),
            "readiness account identity mismatch",
        ),
    ] {
        let fixture = Fixture::new(vec![(status, body, extra)]).await;
        assert_eq!(
            fixture
                .observe()
                .await
                .expect_err("unsafe response refused")
                .to_string(),
            expected
        );
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn trusted_dsid_http_login_capture_then_alias_validate_without_anchor_mutation() {
    let login = serde_json::json!({"dsInfo":{"dsid":"stable-synthetic-id","appleId":"canonical@example.invalid"},"webservices":{"drivews":{"url":"https://drivews.icloud.com/"},"docws":{"url":"https://docws.icloud.com/"}}});
    let validate = serde_json::json!({"dsInfo":{"dsid":"stable-synthetic-id","appleId":"canonical@example.invalid"},"webservices":{},"pcsServiceIdentitiesIncluded":false});
    let mut fixture = Fixture::start(
        vec![
            (200, login.to_string(), String::new()),
            (200, validate.to_string(), String::new()),
        ],
        true,
    )
    .await;
    fixture.session.headers.session_token = "synthetic-session".into();
    fixture
        .session
        .account_login()
        .await
        .expect("successful bound login");
    let anchor = fixture.session.trusted_dsid_hash.clone();
    assert_eq!(
        anchor,
        super::super::session_identity::dsid_hash(&serde_json::json!("stable-synthetic-id"))
    );
    assert!(
        fixture
            .observe()
            .await
            .expect("canonical address with same anchored identity")
            .account_identity_verified
    );
    assert_eq!(fixture.session.trusted_dsid_hash, anchor);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn trusted_dsid_http_validate_never_bootstraps_or_overrides_anchor() {
    for (anchor, address, expected) in [
        (
            None,
            "canonical@example.invalid",
            Some("readiness account identity mismatch"),
        ),
        (None, "fixture@example.invalid", None),
        (
            Some("other-stable-id"),
            "fixture@example.invalid",
            Some("readiness stable account identity mismatch"),
        ),
    ] {
        let reply = serde_json::json!({"dsInfo":{"dsid":"returned-stable-id","appleId":address},"webservices":{}});
        let mut fixture = Fixture::new(vec![(200, reply.to_string(), String::new())]).await;
        fixture.session.trusted_dsid_hash = anchor
            .and_then(|value| super::super::session_identity::dsid_hash(&serde_json::json!(value)));
        let before = fixture.session.trusted_dsid_hash.clone();
        let result = fixture.observe().await;
        if let Some(expected) = expected {
            assert_eq!(result.expect_err("identity refused").to_string(), expected);
        } else {
            assert!(result.is_ok());
        }
        assert_eq!(fixture.session.trusted_dsid_hash, before);
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    }
    let mut failed = Fixture::start(
        vec![(403, "synthetic-rejection".into(), String::new())],
        true,
    )
    .await;
    failed.session.headers.session_token = "synthetic-session".into();
    assert!(failed.session.account_login().await.is_err());
    assert!(failed.session.trusted_dsid_hash.is_none());
    assert_eq!(failed.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn owned_renewal_independent_jar_and_original_snapshot_unchanged() {
    use secrecy::ExposeSecret;
    let login = serde_json::json!({"dsInfo":{"dsid":"renewed-synthetic-id","appleId":"canonical@example.invalid"},"webservices":{"drivews":{"url":"https://drivews.icloud.com/"},"docws":{"url":"https://docws.icloud.com/"}}});
    let validate = serde_json::json!({"dsInfo":{"dsid":"renewed-synthetic-id","appleId":"canonical@example.invalid"},"webservices":{},"pcsServiceIdentitiesIncluded":false});
    let mut fixture = Fixture::start(
        vec![
            (
                200,
                login.to_string(),
                "Set-Cookie: fixture-renewal=changed; Domain=.icloud.com; Path=/; Secure\r\n"
                    .into(),
            ),
            (200, validate.to_string(), String::new()),
        ],
        true,
    )
    .await;
    fixture.make_legacy_snapshot_available();
    let original = fixture
        .session
        .session_snapshot()
        .expect("original snapshot");
    let mut renewed = fixture
        .session
        .independent_renewal_session("fixture@example.invalid")
        .expect("independent clone");
    assert!(!Arc::ptr_eq(&fixture.session.cookies, &renewed.cookies));
    fixture.configure_independent_session(&mut renewed);
    let observed = fixture
        .session
        .renewal_readiness_with_session(
            &mut renewed,
            "fixture@example.invalid",
            &CancellationToken::new(),
        )
        .await
        .expect("renewal and readiness");
    assert!(observed.account_identity_verified);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
    assert!(fixture.session.trusted_dsid_hash.is_none());
    assert!(renewed.trusted_dsid_hash.is_some());
    assert_eq!(
        original.expose_secret(),
        fixture
            .session
            .session_snapshot()
            .expect("unchanged snapshot")
            .expose_secret()
    );
    assert!(
        fixture
            .session
            .cookies
            .records()
            .expect("original records")
            .is_empty()
    );
    assert_eq!(
        renewed
            .cookies
            .records()
            .expect("independent response cookie")
            .len(),
        1
    );
}
#[tokio::test]
async fn owned_renewal_refusal_stops_without_validate_or_second_login() {
    for (status, old_anchor, dsid, expected) in [
        (
            403,
            None,
            serde_json::Value::Null,
            "iCloud account session was rejected (403)",
        ),
        (
            200,
            Some("original-synthetic-id"),
            serde_json::json!("wrong-synthetic-id"),
            "renewal stable account identity mismatch",
        ),
        (
            200,
            None,
            serde_json::Value::Null,
            "renewal response has no stable account anchor",
        ),
    ] {
        let response = serde_json::json!({"dsInfo":{"dsid":dsid},"webservices":{"drivews":{"url":"https://drivews.icloud.com/"},"docws":{"url":"https://docws.icloud.com/"}}});
        let mut fixture =
            Fixture::start(vec![(status, response.to_string(), String::new())], true).await;
        fixture.make_legacy_snapshot_available();
        fixture.session.trusted_dsid_hash = old_anchor
            .and_then(|value| super::super::session_identity::dsid_hash(&serde_json::json!(value)));
        let mut renewed = fixture
            .session
            .independent_renewal_session("fixture@example.invalid")
            .expect("isolated clone");
        fixture.configure_independent_session(&mut renewed);
        let result = fixture
            .session
            .renewal_readiness_with_session(
                &mut renewed,
                "fixture@example.invalid",
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(
            result.expect_err("stop after refusal").to_string(),
            expected
        );
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn owned_renewal_validate_rejection_does_not_retry_login() {
    let response = serde_json::json!({"dsInfo":{"dsid":"synthetic-id"},"webservices":{"drivews":{"url":"https://drivews.icloud.com/"},"docws":{"url":"https://docws.icloud.com/"}}});
    let mut fixture = Fixture::start(
        vec![
            (200, response.to_string(), String::new()),
            (421, String::new(), String::new()),
        ],
        true,
    )
    .await;
    fixture.make_legacy_snapshot_available();
    let mut renewed = fixture
        .session
        .independent_renewal_session("fixture@example.invalid")
        .expect("isolated clone");
    fixture.configure_independent_session(&mut renewed);
    assert_eq!(
        fixture
            .session
            .renewal_readiness_with_session(
                &mut renewed,
                "fixture@example.invalid",
                &CancellationToken::new()
            )
            .await
            .expect_err("validation refusal")
            .to_string(),
        "readiness validation refused (HTTP 421)"
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn owned_renewal_wrong_account_and_cancelled_refuse_before_network() {
    let mut session = ICloudReadSession::new().expect("session");
    session.account_hash = Some(account_hash("fixture@example.invalid").expect("account"));
    assert!(
        session
            .probe_owned_pages_renewal_readiness("other@example.invalid", &CancellationToken::new())
            .await
            .expect_err("wrong account")
            .to_string()
            .contains("binding mismatch")
    );
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        session
            .probe_owned_pages_renewal_readiness("fixture@example.invalid", &cancel)
            .await
            .expect_err("cancelled")
            .to_string(),
        "renewal request cancelled"
    );
}
