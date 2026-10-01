#![allow(clippy::unwrap_used)]
use super::*;
use base64::Engine as _;
use std::{sync::Arc, time::Duration};
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
#[derive(Default)]
struct Sink(Vec<u8>);
#[async_trait::async_trait]
impl ReadWindowSink for Sink {
    async fn write_chunk(
        &mut self,
        bytes: &[u8],
    ) -> std::result::Result<(), cirrove_core::ProviderError> {
        self.0.extend_from_slice(bytes);
        Ok(())
    }
}
async fn arm(fault: u8, expected: FixtureRepresentation) {
    let decoder = base64::engine::general_purpose::STANDARD;
    let cert = decoder
        .decode(include_str!("../package_create/fixtures/server-cert.b64").trim())
        .unwrap();
    let key = decoder
        .decode(include_str!("../package_create/fixtures/server-key.b64").trim())
        .unwrap();
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(cert.clone())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
        )
        .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = reqwest::Client::builder()
        .no_proxy()
        .resolve("fixture.icloud-content.com", listener.local_addr().unwrap())
        .tls_certs_only([reqwest::Certificate::from_der(&cert).unwrap()])
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap();
    let parent:DriveEntry=serde_json::from_value(json!({"drivewsid":"FOLDER::com.apple.CloudDocs::owned","zone":"com.apple.CloudDocs","type":"FOLDER","name":"Owned","etag":"parent-v1"})).unwrap();
    let source:DriveEntry=serde_json::from_value(json!({"drivewsid":"FILE::com.apple.CloudDocs::doc","docwsid":"doc","parentId":parent.drivewsid,"zone":"com.apple.CloudDocs","type":"FILE","name":"Fixture","etag":"v1","size":3})).unwrap();
    let folder = |item: &DriveEntry| {
        let mut p = parent.clone();
        p.items = vec![item.clone()];
        p.number_of_items = Some(1);
        serde_json::to_vec(&vec![p]).unwrap()
    };
    let token = |r| {
        serde_json::to_vec(&match r {
            FixtureRepresentation::Data => {
                json!({"data_token":{"url":"https://fixture.icloud-content.com/content"}})
            }
            FixtureRepresentation::Package => {
                json!({"package_token":{"url":"https://fixture.icloud-content.com/content"}})
            }
        })
        .unwrap()
    };
    let mut replies = vec![
        ("POST /retrieveItemDetailsInFolders", folder(&source)),
        ("POST /retrieveItemDetailsInFolders", folder(&source)),
        (
            "GET /ws/com.apple.CloudDocs/download/by_id",
            token(expected),
        ),
        ("GET /content", b"abc".to_vec()),
    ];
    let opposite = if expected == FixtureRepresentation::Data {
        FixtureRepresentation::Package
    } else {
        FixtureRepresentation::Data
    };
    replies.push((
        "GET /ws/com.apple.CloudDocs/download/by_id",
        token(if fault == 1 { opposite } else { expected }),
    ));
    let mut final_source = source.clone();
    if fault == 2 {
        final_source.etag = "v2".into();
    }
    replies.push(("POST /retrieveItemDetailsInFolders", folder(&final_source)));
    replies.push(("POST /retrieveItemDetailsInFolders", folder(&source)));
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls = count.clone();
    let expected_calls = match fault {
        1 => 5,
        2 => 6,
        _ => 7,
    };
    let stop = CancellationToken::new();
    let stopping = stop.clone();
    let task = tokio::spawn(async move {
        for (method, body) in replies {
            if stopping.is_cancelled() {
                break;
            }
            tokio::time::timeout(Duration::from_secs(4), async {
                let (peer, _) = tokio::select!{biased; _=stopping.cancelled()=>return, value=listener.accept()=>value.unwrap()};
                let mut stream = acceptor.accept(peer).await.unwrap();
                let mut request = Vec::new();
                loop {
                    let mut buffer = [0; 4096];
                    let n = stream.read(&mut buffer).await.unwrap();
                    assert!(n > 0);
                    request.extend_from_slice(&buffer[..n]);
                    assert!(request.len() < 16384);
                    if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = std::str::from_utf8(&request[..end]).unwrap();
                        assert!(headers.starts_with(method));
                        let length = headers
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length: ")
                                    .map(str::to_owned)
                            })
                            .map(|v| v.parse::<usize>().unwrap())
                            .unwrap_or(0);
                        if request.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                stream.write_all(&body).await.unwrap();
            })
            .await
            .unwrap();
        }
    });
    let mut session = ICloudReadSession::new().unwrap();
    session.http = client;
    session.drive_endpoint = Some("https://fixture.icloud-content.com/".parse().unwrap());
    session.docs_endpoint = session.drive_endpoint.clone();
    let mut sink = Sink::default();
    let result = tokio::time::timeout(
        Duration::from_secs(12),
        session.read_owned_fixture(
            &parent,
            &source,
            expected,
            &mut sink,
            &CancellationToken::new(),
        ),
    )
    .await
    .unwrap();
    stop.cancel();
    task.await.unwrap();
    assert_eq!(
        count.load(std::sync::atomic::Ordering::SeqCst),
        expected_calls
    );
    assert_eq!(sink.0, b"abc");
    if fault == 0 {
        let receipt = result.unwrap();
        assert_eq!(receipt.size, 3);
        assert_eq!(receipt.sha256, hex::encode(Sha256::digest(b"abc")));
    } else {
        assert!(result.is_err(), "post-body fence was bypassed");
    }
}
#[tokio::test]
async fn owned_fixture_https_complete_data_and_package() {
    arm(0, FixtureRepresentation::Data).await;
    arm(0, FixtureRepresentation::Package).await;
}
#[tokio::test]
async fn owned_fixture_https_representation_flip_after_body_refused() {
    arm(1, FixtureRepresentation::Package).await;
}
#[tokio::test]
async fn owned_fixture_https_revision_race_after_body_refused() {
    arm(2, FixtureRepresentation::Data).await;
}
