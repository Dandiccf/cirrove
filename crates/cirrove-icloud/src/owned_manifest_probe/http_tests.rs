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
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fixture {
    async fn new(replies: Vec<(u16, String, String)>) -> Self {
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
        session.account_hash = Some("synthetic".into());
        // This fixture certificate has a different SAN. Only this synthetic
        // client's TLS validation is disabled, not production endpoint policy.
        session.http = reqwest::Client::builder()
            .no_proxy()
            .danger_accept_invalid_certs(true)
            .resolve("iwmb.icloud.com", address)
            .resolve("p123-iwmb.icloud.com", address)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(2))
            .build()
            .expect("fixture client");
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let task = tokio::spawn(async move {
            for (status, body, extra) in replies {
                tokio::time::timeout(Duration::from_secs(3), async {
                    let (socket, _) = listener.accept().await.expect("fixture accept");
                    let mut stream = acceptor.accept(socket).await.expect("fixture handshake");
                    let mut request = Vec::new();
                    loop {
                        let mut buffer = [0; 4096]; let read = stream.read(&mut buffer).await.expect("fixture read");
                        assert!(read > 0 && request.len() + read <= 16 * 1024);
                        request.extend_from_slice(&buffer[..read]);
                        if request.windows(4).any(|v| v == b"\r\n\r\n") { break; }
                    }
                    let request = std::str::from_utf8(&request).expect("fixture request text");
                    assert!(request.starts_with(&format!("GET /iwmb/pages/{SHORT}/manifest?version=1540&clientType=G15.4&clientBuildNumber=15D120")));
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
            task,
        }
    }
    async fn observe(&self) -> Result<OwnedManifestObservation> {
        self.session
            .probe_owned_pages_manifest(
                &format!("FILE::com.apple.CloudDocs::{DOCUMENT}"),
                &CancellationToken::new(),
            )
            .await
    }
}
#[tokio::test]
async fn owned_manifest_http_affinity_then_exact_owned_identity() {
    let fixture = Fixture::new(vec![
        (330, serde_json::json!({"X-Apple-MMe-Host":"p123-iwmb.icloud.com","StickySessionId":"synthetic"}).to_string(), String::new()),
        (200, super::tests::manifest().to_string(), String::new()),
    ]).await;
    assert!(
        fixture
            .observe()
            .await
            .expect("owned manifest")
            .exact_document
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn owned_manifest_http_rejects_status_redirect_and_unbounded_body() {
    for (status, body, extra) in [
        (401, "synthetic-secret".to_owned(), String::new()),
        (403, "synthetic-secret".to_owned(), String::new()),
        (421, "synthetic-secret".to_owned(), String::new()),
        (302, String::new(), "Location: https://iwmb.icloud.com/unrelated\r\n".into()),
        (330, serde_json::json!({"X-Apple-MMe-Host":"example.com","StickySessionId":"synthetic-secret"}).to_string(), String::new()),
        (200, "x".repeat(LIMIT + 1), String::new()),
    ] {
        let fixture = Fixture::new(vec![(status,body,extra)]).await;
        let error = fixture.observe().await.expect_err("unsafe reply refused").to_string();
        assert!(!error.contains("synthetic-secret") && !error.contains("example.com"));
        let expected = match status {
            401 => "manifest session rejected (HTTP 401)",
            403 => "manifest session rejected (HTTP 403)",
            421 => "manifest session rejected (HTTP 421)",
            302 => "manifest response status refused",
            330 => "manifest affinity refused",
            200 => "manifest response too large",
            _ => unreachable!("fixed synthetic status inventory"),
        };
        // A redirect-following client may time out on an unserved second
        // request. That transport failure must not count as a policy refusal.
        assert_eq!(error, expected);
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    }
}
