#![allow(clippy::unwrap_used)]
use super::*;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const ID: &str = "FILE::com.apple.CloudDocs::window";
fn node(size: u64) -> Node {
    Node {
        id: ID.into(),
        parent_id: Some(ROOT_ID.into()),
        name: "Synthetic".into(),
        kind: NodeKind::File,
        size,
        modified_unix: 0,
        etag: Some("v1".into()),
        content_version: None,
        target: None,
        package: false,
    }
}
fn scope() -> Scope {
    Scope {
        account: "window-fixture".into(),
        provider: "icloud".into(),
        collection: "drive".into(),
    }
}
// Only metadata uses this synthetic endpoint. Production content URL validation
// remains HTTPS-only and restricted to Apple's content hosts.
async fn fixture(size: u64, tag: &str) -> (OrdinarySession, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/", listener.local_addr().unwrap());
    let body = json!([{"drivewsid":ROOT_ID,"type":"FOLDER","numberOfItems":1,"items":[{
        "drivewsid":ID,"type":"FILE","name":"Synthetic","etag":tag,"size":size
    }]}])
    .to_string();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        loop {
            let mut buf = [0; 4096];
            let n = socket.read(&mut buf).await.unwrap();
            assert!(n > 0 && request.len() < 16384);
            request.extend_from_slice(&buf[..n]);
            if request.windows(4).any(|v| v == b"\r\n\r\n") {
                break;
            }
        }
        assert!(request.starts_with(b"POST /retrieveItemDetailsInFolders"));
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    let mut transport = ICloudReadSession::new().unwrap();
    transport.drive_endpoint = Some(endpoint.parse().unwrap());
    let session = OrdinarySession::new(
        &scope(),
        &node(size),
        transport,
        Arc::new(Semaphore::new(4)),
    )
    .unwrap();
    (session, task)
}
async fn response(headers: String, bytes: Vec<u8>) -> reqwest::Response {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0; 4096];
        let _ = socket.read(&mut buf).await;
        if socket.write_all(headers.as_bytes()).await.is_ok() {
            let _ = socket.write_all(&bytes).await;
        }
    });
    reqwest::get(format!("http://{address}/")).await.unwrap()
}
fn headers(range: &str, length: usize) -> String {
    format!(
        "HTTP/1.1 206 Partial Content\r\nContent-Range: {range}\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n"
    )
}
#[derive(Default)]
struct Sink {
    bytes: Vec<u8>,
    fail: bool,
}
#[async_trait]
impl ReadWindowSink for Sink {
    async fn write_chunk(&mut self, bytes: &[u8]) -> Result<(), ProviderError> {
        assert!(bytes.len() <= 64 * 1024);
        if self.fail {
            return Err(ProviderError::Unavailable);
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
}
#[tokio::test]
async fn streamed_varied_bytes_and_partial_final_block_require_final_revision() {
    let bytes: Vec<u8> = (0..200_017)
        .map(|i| ((i * 17 + i / 251) % 256) as u8)
        .collect();
    for tag in ["v1", "v2"] {
        let (session, task) = fixture(bytes.len() as u64, tag).await;
        let mut transport = session.transport.read_only_fork();
        let mut sink = Sink::default();
        let reply = response(
            headers("bytes 7-200016/200017", bytes.len() - 7),
            bytes[7..].to_vec(),
        )
        .await;
        let result = session
            .finish_window(&mut transport, reply, 7, 200_016, &mut sink)
            .await;
        assert_eq!(sink.bytes, bytes[7..]);
        if tag == "v1" {
            result.unwrap();
        } else {
            assert!(matches!(result, Err(ProviderError::VersionChanged)));
        }
        task.await.unwrap();
    }
}
#[tokio::test]
async fn rejects_wrong_ranges_short_long_encoded_and_failed_sink() {
    let cases = [
        (headers("bytes 1-3/4",3),b"abc".to_vec(),false),
        (headers("bytes 0-3/4",3),b"abc".to_vec(),false),
        (headers("bytes 0-3/4",4),b"abc".to_vec(),false),
        ("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-3/4\r\nConnection: close\r\n\r\n".into(),b"abcde".to_vec(),false),
        ("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-3/4\r\nContent-Encoding: gzip\r\nContent-Length: 4\r\nConnection: close\r\n\r\n".into(),b"abcd".to_vec(),false),
        (headers("bytes 0-3/4",4),b"abcd".to_vec(),true),
    ];
    for (head, bytes, fail) in cases {
        let (session, task) = fixture(4, "v1").await;
        let mut transport = session.transport.read_only_fork();
        let mut sink = Sink {
            fail,
            ..Default::default()
        };
        let result = session
            .finish_window(&mut transport, response(head, bytes).await, 0, 3, &mut sink)
            .await;
        assert!(result.is_err());
        // Rejected transfers must not reach final metadata validation.
        assert!(!task.is_finished());
        task.abort();
    }
}
#[tokio::test]
async fn changed_initial_revision_fails_even_at_eof_without_content_lookup() {
    for offset in [0, 4] {
        let (session, task) = fixture(4, "v2").await;
        let mut sink = Sink::default();
        assert!(matches!(
            session
                .read_window(offset, 4, &mut sink, &CancellationToken::new())
                .await,
            Err(ProviderError::VersionChanged)
        ));
        assert!(sink.bytes.is_empty());
        task.await.unwrap();
    }
}
#[tokio::test]
async fn cancellation_interrupts_a_waiting_shared_read_permit() {
    let (session, task) = fixture(4, "v1").await;
    let permits = session.reads.acquire_many(4).await.unwrap();
    let cancel = CancellationToken::new();
    let signal = cancel.clone();
    let mut sink = Sink::default();
    let read = session.read_window(0, 4, &mut sink, &cancel);
    let stop = async {
        tokio::task::yield_now().await;
        signal.cancel();
    };
    let (result, ()) =
        tokio::time::timeout(Duration::from_secs(1), async { tokio::join!(read, stop) })
            .await
            .unwrap();
    assert!(matches!(result, Err(ProviderError::Cancelled)));
    assert!(sink.bytes.is_empty());
    assert!(!task.is_finished());
    drop(permits);
    task.abort();
}
#[tokio::test]
async fn adapter_binds_scope_revision_and_shared_budget_without_fetching_content() {
    let (session, task) = fixture(4, "v1").await;
    let drive =
        ICloudDrive::on_demand_from_live_session(scope(), session.transport.read_only_fork())
            .unwrap();
    let cancel = CancellationToken::new();
    let opened = drive
        .open_read_session(&scope(), &node(4), &cancel)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        opened.identity(),
        &ReadIdentity::new(&scope(), &node(4)).unwrap()
    );
    assert_eq!(opened.window_limit(), WINDOW_LIMIT);
    let mut wrong = scope();
    wrong.account = "other".into();
    assert!(
        drive
            .open_read_session(&wrong, &node(4), &cancel)
            .await
            .is_err()
    );
    let mut sink = Sink::default();
    assert!(
        opened
            .read_window(0, WINDOW_LIMIT + 1, &mut sink, &cancel)
            .await
            .is_err()
    );
    let mut invalid = node(4);
    invalid.etag = None;
    assert!(
        drive
            .open_read_session(&scope(), &invalid, &cancel)
            .await
            .is_err()
    );
    invalid = node(4);
    invalid.content_version = Some("unverified".into());
    assert!(
        drive
            .open_read_session(&scope(), &invalid, &cancel)
            .await
            .unwrap()
            .is_none()
    );
    assert!(!task.is_finished());
    task.abort();
}

#[tokio::test]
async fn uploaded_receipt_with_content_revision_equal_to_etag_opens_a_session() {
    let (session, task) = fixture(4, "v2").await;
    let drive =
        ICloudDrive::on_demand_from_live_session(scope(), session.transport.read_only_fork())
            .unwrap();
    let mut receipt = node(4);
    receipt.content_version = receipt.etag.clone();
    let cancel = CancellationToken::new();
    let opened = drive
        .open_read_session(&scope(), &receipt, &cancel)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        opened.identity(),
        &ReadIdentity::new(&scope(), &receipt).unwrap()
    );
    let mut sink = Sink::default();
    assert!(matches!(
        opened.read_window(0, 4, &mut sink, &cancel).await,
        Err(ProviderError::VersionChanged)
    ));
    assert!(sink.bytes.is_empty());
    task.await.unwrap();
}
