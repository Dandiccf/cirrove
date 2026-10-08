//! The service deadline covers all ordinary revision-fenced read stages.
#![allow(clippy::unwrap_used)]
use super::*;
use base64::Engine as _;
use std::sync::atomic::{AtomicUsize, Ordering};
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

const ORIGIN: &str = "https://fixture.icloud-content.com";
const ITEM: &str = "FILE::com.apple.CloudDocs::deadline-fixture";
const BODY: &[u8] = b"fenced ordinary bytes";

struct Fixture {
    drive: ICloudDrive,
    scope: Scope,
    node: Node,
    started: Arc<AtomicUsize>,
    completed: Arc<AtomicUsize>,
    first_half_sent: Arc<AtomicUsize>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn new(delays: [u64; 4], final_tag: &str) -> Self {
        Self::with_body_pause(delays, final_tag, 0).await
    }
    async fn with_body_pause(delays: [u64; 4], final_tag: &str, body_pause: u64) -> Self {
        let decode = base64::engine::general_purpose::STANDARD;
        let cert = decode
            .decode(include_str!("../../package_create/fixtures/server-cert.b64").trim())
            .unwrap();
        let key = decode
            .decode(include_str!("../../package_create/fixtures/server-key.b64").trim())
            .unwrap();
        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(cert)],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
            )
            .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let started = Arc::new(AtomicUsize::new(0));
        let completed = Arc::new(AtomicUsize::new(0));
        let first_half_sent = Arc::new(AtomicUsize::new(0));
        let prefix_sent = first_half_sent.clone();
        let entered = started.clone();
        let finished = completed.clone();
        let final_tag = final_tag.to_owned();
        let server = tokio::spawn(async move {
            for (index, delay) in delays.into_iter().enumerate() {
                let (socket, _) = listener.accept().await.unwrap();
                let mut stream = acceptor.accept(socket).await.unwrap();
                let mut request = Vec::new();
                let path = loop {
                    let mut buf = [0; 4096];
                    let n = stream.read(&mut buf).await.unwrap();
                    assert!(n > 0 && request.len() + n <= 16384);
                    request.extend_from_slice(&buf[..n]);
                    let Some(end) = request.windows(4).position(|v| v == b"\r\n\r\n") else {
                        continue;
                    };
                    let headers = std::str::from_utf8(&request[..end]).unwrap();
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .map(str::to_owned)
                        })
                        .map(|v| v.parse::<usize>().unwrap())
                        .unwrap_or(0);
                    if request.len() >= end + 4 + length {
                        break headers
                            .lines()
                            .next()
                            .unwrap()
                            .split_whitespace()
                            .nth(1)
                            .unwrap()
                            .split('?')
                            .next()
                            .unwrap()
                            .to_owned();
                    }
                };
                let expected = match index {
                    0 | 3 => "/retrieveItemDetailsInFolders",
                    1 => "/ws/com.apple.CloudDocs/download/by_id",
                    2 => "/content",
                    _ => unreachable!(),
                };
                assert_eq!(path, expected);
                entered.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_secs(delay)).await;
                let (status, extra, body) = match index {
                    0|3 => ("200 OK", String::new(), serde_json::json!([{
                        "drivewsid":ROOT_ID,"type":"FOLDER","numberOfItems":1,
                        "items":[{"drivewsid":ITEM,"type":"FILE","name":"fixture","size":BODY.len(),"etag":if index==3 {final_tag.as_str()} else {"v1"}}]
                    }]).to_string().into_bytes()),
                    1 => ("200 OK",String::new(),serde_json::json!({"data_token":{"url":format!("{ORIGIN}/content")}}).to_string().into_bytes()),
                    2 => ("206 Partial Content",format!("Content-Range: bytes 0-{}/{}\r\n",BODY.len()-1,BODY.len()),BODY.to_vec()),
                    _ => unreachable!(),
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n",
                    body.len()
                );
                if stream.write_all(head.as_bytes()).await.is_err() {
                    break;
                }
                if index == 2 && body_pause > 0 {
                    let half = body.len() / 2;
                    if stream.write_all(&body[..half]).await.is_err()
                        || stream.flush().await.is_err()
                    {
                        break;
                    }
                    prefix_sent.store(half, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_secs(body_pause)).await;
                    if stream.write_all(&body[half..]).await.is_err() {
                        break;
                    }
                } else if stream.write_all(&body).await.is_err() {
                    break;
                }
                finished.fetch_add(1, Ordering::SeqCst);
            }
        });
        let scope = Scope {
            account: "synthetic-deadline".into(),
            provider: "icloud".into(),
            collection: "drive".into(),
        };
        let node = Node {
            id: ITEM.into(),
            parent_id: Some(ROOT_ID.into()),
            name: "fixture".into(),
            kind: NodeKind::File,
            size: BODY.len() as u64,
            modified_unix: 0,
            etag: Some("v1".into()),
            content_version: None,
            target: None,
            package: false,
        };
        let mut session = ICloudReadSession::new().unwrap();
        // This test-only client reaches only the localhost listener, without
        // credentials, proxies, redirects or relaxed production URL policies.
        session.http = reqwest::Client::builder()
            .no_proxy()
            .danger_accept_invalid_certs(true)
            .resolve("fixture.icloud-content.com", address)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap();
        session.drive_endpoint = Some(format!("{ORIGIN}/").parse().unwrap());
        session.docs_endpoint = session.drive_endpoint.clone();
        let drive = ICloudDrive::on_demand_from_live_session(scope.clone(), session).unwrap();
        Self {
            drive,
            scope,
            node,
            started,
            completed,
            first_half_sent,
            server,
        }
    }
    async fn read(&self, cancel: &CancellationToken) -> Result<Vec<u8>, ProviderError> {
        // This is the service's actual ContentCache::block deadline expression,
        // with its real iCloud adapter policy and exact normal ordinary session.
        tokio::time::timeout(
            self.drive
                .content_read_timeout(&self.node)
                .min(Duration::from_secs(360)),
            async {
                let session = self
                    .drive
                    .open_read_session(&self.scope, &self.node, cancel)
                    .await?
                    .unwrap();
                session.read_range(0, BODY.len() as u32, cancel).await
            },
        )
        .await
        .unwrap_or(Err(ProviderError::Unavailable))
    }
}

#[tokio::test]
async fn ordinary_read_deadline_includes_bounded_metadata_lookup_body_and_final_revision() {
    // All individual HTTP requests remain within their own 30/90 second caps.
    // Total34s must reach the final revision fence; total 30 s loses good bytes.
    let f = Fixture::new([16, 8, 8, 2], "v1").await;
    let result = f.read(&CancellationToken::new()).await;
    assert!(
        result.is_ok(),
        "bounded multistage ordinary read discarded: result={result:?}, started={}, completed={}",
        f.started.load(Ordering::SeqCst),
        f.completed.load(Ordering::SeqCst)
    );
    assert_eq!(result.unwrap(), BODY);
    assert_eq!(f.completed.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn multistage_ordinary_read_rejects_changed_final_revision() {
    let f = Fixture::new([0, 0, 0, 0], "v2").await;
    assert!(matches!(
        f.read(&CancellationToken::new()).await,
        Err(ProviderError::VersionChanged)
    ));
    assert_eq!(f.completed.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn multistage_ordinary_read_cancellation_interrupts_metadata() {
    let f = Fixture::new([16, 0, 0, 0], "v1").await;
    let cancel = CancellationToken::new();
    let trigger = async {
        while f.started.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        cancel.cancel();
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(f.read(&cancel), trigger)
    })
    .await
    .unwrap();
    assert!(matches!(result, Err(ProviderError::Cancelled)));
    assert_eq!(f.started.load(Ordering::SeqCst), 1);
    assert_eq!(f.completed.load(Ordering::SeqCst), 0);
}

// The client default stays 30 s: only the production content request may override
// it. Both controls hold their caller bound at 270 s, independent of policy edits.
#[derive(Default)]
struct BodySink(Vec<u8>);
#[async_trait]
impl ReadWindowSink for BodySink {
    async fn write_chunk(&mut self, bytes: &[u8]) -> Result<(), ProviderError> {
        assert!(bytes.len() <= 64 * 1024);
        self.0.extend_from_slice(bytes);
        Ok(())
    }
}

#[tokio::test]
async fn ordinary_range_body_stall_keeps_good_bytes_and_final_revision() {
    // Headers and the first half arrive immediately; only the remaining body
    // waits 33 s. A total 30 s request cap cuts a validated progressive response.
    let f = Fixture::with_body_pause([0, 0, 0, 0], "v1", 33).await;
    let result = tokio::time::timeout(Duration::from_secs(270), f.read(&CancellationToken::new()))
        .await
        .unwrap();
    assert_eq!(f.first_half_sent.load(Ordering::SeqCst), BODY.len() / 2);
    assert!(
        result.is_ok(),
        "ordinary range body stall discarded good bytes: result={result:?}, started={}, completed={}",
        f.started.load(Ordering::SeqCst),
        f.completed.load(Ordering::SeqCst)
    );
    assert_eq!(result.unwrap(), BODY);
    assert_eq!(f.started.load(Ordering::SeqCst), 4);
    assert_eq!(f.completed.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn ordinary_window_body_stall_keeps_good_bytes_and_final_revision() {
    let f = Fixture::with_body_pause([0, 0, 0, 0], "v1", 33).await;
    let cancel = CancellationToken::new();
    let mut sink = BodySink::default();
    let result = tokio::time::timeout(Duration::from_secs(270), async {
        let session = f
            .drive
            .open_read_session(&f.scope, &f.node, &cancel)
            .await?
            .unwrap();
        session
            .read_window(0, BODY.len() as u32, &mut sink, &cancel)
            .await
    })
    .await
    .unwrap();
    assert_eq!(f.first_half_sent.load(Ordering::SeqCst), BODY.len() / 2);
    assert!(
        result.is_ok(),
        "ordinary window body stall discarded good bytes: result={result:?}, started={}, completed={}, staged={}",
        f.started.load(Ordering::SeqCst),
        f.completed.load(Ordering::SeqCst),
        sink.0.len()
    );
    assert_eq!(sink.0, BODY);
    assert_eq!(f.started.load(Ordering::SeqCst), 4);
    assert_eq!(f.completed.load(Ordering::SeqCst), 4);
}
