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
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn new(delays: [u64; 4], final_tag: &str) -> Self {
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
                if stream.write_all(&body).await.is_err() {
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
    // Total34s must reach the final revision fence; total30s loses good bytes.
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
