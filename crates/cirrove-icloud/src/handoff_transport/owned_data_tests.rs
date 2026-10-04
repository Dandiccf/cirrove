#![allow(clippy::unwrap_used)]
use super::*;
use base64::Engine as _;
use cirrove_core::{Node, NodeKind};
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
use tokio_util::sync::CancellationToken;

const ID: &str = "FILE::com.apple.CloudDocs::owned-data";
const PARENT: &str = "FOLDER::com.apple.CloudDocs::owned-parent";

fn nodes(size: u64) -> (Node, Node) {
    let original = Node {
        id: ID.into(),
        parent_id: Some(PARENT.into()),
        name: "Owned.numbers".into(),
        kind: NodeKind::File,
        size,
        modified_unix: 0,
        etag: Some("original-v1".into()),
        content_version: Some("original-v1".into()),
        target: None,
        package: false,
    };
    let backup = Node {
        parent_id: Some(TRASH_ROOT.into()),
        etag: Some("trash-v2".into()),
        content_version: Some("trash-v2".into()),
        ..original.clone()
    };
    (original, backup)
}
fn metadata(size: u64) -> serde_json::Value {
    json!({"drivewsid":ID,"docwsid":"owned-data","parentId":"TRASH_ROOT",
        "restorePath":"/owned-parent/Owned.numbers","type":"FILE",
        "etag":"trash-v2","name":"Owned","extension":"numbers","size":size})
}

// Only local TLS and the exact read-only item/download request sequence. A
// server stopped after an early refusal does not imply the remaining reads ran.
async fn fixture(
    first: serde_json::Value,
    second: serde_json::Value,
    body: &[u8],
    package: bool,
) -> (
    ICloudReadSession,
    Arc<AtomicUsize>,
    CancellationToken,
    tokio::task::JoinHandle<()>,
) {
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
    let size = first["size"].as_u64().unwrap();
    let mut replies = vec![(
        "POST /retrieveItemDetails",
        serde_json::to_vec(&json!({"items":[first]})).unwrap(),
    )];
    if size > 0 {
        let token = json!({"url":"https://fixture.icloud-content.com/content"});
        replies.push((
            "GET /ws/com.apple.CloudDocs/download/by_id?document_id=owned-data",
            serde_json::to_vec(&if package {
                json!({"package_token":token})
            } else {
                json!({"data_token":token})
            })
            .unwrap(),
        ));
        replies.push(("GET /content", body.to_vec()));
    }
    replies.push((
        "POST /retrieveItemDetails",
        serde_json::to_vec(&json!({"items":[second]})).unwrap(),
    ));
    let count = Arc::new(AtomicUsize::new(0));
    let calls = count.clone();
    let stop = CancellationToken::new();
    let stopping = stop.clone();
    let task = tokio::spawn(async move {
        for (expected, response) in replies {
            if stopping.is_cancelled() {
                break;
            }
            tokio::time::timeout(Duration::from_secs(4), async {
                let (peer, _) = tokio::select! {biased; _=stopping.cancelled()=>return, value=listener.accept()=>value.unwrap()};
                let mut stream = acceptor.accept(peer).await.unwrap(); let mut request = Vec::new();
                loop {
                    let mut buffer = [0;4096]; let n = stream.read(&mut buffer).await.unwrap();
                    assert!(n > 0 && request.len() < 16384); request.extend_from_slice(&buffer[..n]);
                    let Some(end) = request.windows(4).position(|w|w==b"\r\n\r\n") else {continue;};
                    let headers = std::str::from_utf8(&request[..end]).unwrap();
                    assert!(headers.starts_with(expected), "only exact registered read-only requests");
                    let length = headers.lines().find_map(|l|l.to_ascii_lowercase().strip_prefix("content-length: ").map(str::to_owned))
                        .map(|v|v.parse::<usize>().unwrap()).unwrap_or(0);
                    if request.len() < end+4+length {continue;}
                    if expected.starts_with("POST") {
                        let parsed: serde_json::Value = serde_json::from_slice(&request[end+4..end+4+length]).unwrap();
                        assert!(parsed==json!({"items":[{"drivewsid":ID,"partialData":false,"includeHierarchy":false}]}));
                    }
                    break;
                }
                calls.fetch_add(1, Ordering::SeqCst);
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",response.len()).as_bytes()).await.unwrap();
                stream.write_all(&response).await.unwrap();
            }).await.unwrap();
        }
    });
    let mut session = ICloudReadSession::new().unwrap();
    session.http = client;
    session.drive_endpoint = Some("https://fixture.icloud-content.com/".parse().unwrap());
    session.docs_endpoint = session.drive_endpoint.clone();
    (session, count, stop, task)
}
fn sanitized(error: &anyhow::Error) {
    let text = format!("{error:#}");
    assert!(
        !text.contains("http://")
            && !text.contains("https://")
            && !text.contains("/owned-parent/")
            && !text.contains("trash-v2")
    );
}

#[tokio::test]
async fn owned_data_trash_reader_accepts_full_and_empty_bytes() {
    for bytes in [b"".as_slice(), b"raw Numbers DATA bytes".as_slice()] {
        let (original, backup) = nodes(bytes.len() as u64);
        let item = metadata(bytes.len() as u64);
        let (mut session, calls, stop, task) = fixture(item.clone(), item, bytes, false).await;
        let actual = session
            .verify_owned_data_in_trash(&original, &backup, &hex::encode(Sha256::digest(bytes)))
            .await
            .unwrap();
        stop.cancel();
        task.await.unwrap();
        assert_eq!(actual, backup);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            if bytes.is_empty() { 2 } else { 4 }
        );
    }
}

#[tokio::test]
async fn owned_data_trash_reader_refuses_invalid_registered_nodes() {
    for arm in 0..17 {
        let (mut original, mut backup) = nodes(3);
        let mut sha = hex::encode(Sha256::digest(b"raw"));
        match arm {
            0 => backup.id = "FILE::com.apple.CloudDocs::foreign".into(),
            1 => original.kind = NodeKind::Folder,
            2 => backup.kind = NodeKind::Folder,
            3 => original.package = true,
            4 => backup.package = true,
            5 => original.parent_id = None,
            6 => original.parent_id = Some(TRASH_ROOT.into()),
            7 => backup.parent_id = Some(PARENT.into()),
            8 => backup.name = "Foreign.numbers".into(),
            9 => original.name = "../Owned.numbers".into(),
            10 => backup.size = 4,
            11 => original.etag = None,
            12 => backup.etag = Some("bad\nrevision".into()),
            13 => sha = "A".repeat(64),
            14 => sha = "0".repeat(63),
            15 => {
                original.id = "FILE::foreign.zone::owned-data".into();
                backup.id = original.id.clone();
            }
            16 => original.parent_id = Some("FOLDER::com.apple.CloudDocs::".into()),
            _ => unreachable!(),
        }
        // No endpoints configured: assert the binding guard, not a later
        // sign-in/transport failure, refuses every registered malformed arm.
        let mut session = ICloudReadSession::new().unwrap();
        let error = session
            .verify_owned_data_in_trash(&original, &backup, &sha)
            .await
            .err()
            .expect("invalid binding admitted");
        assert_eq!(
            error.to_string(),
            "invalid owned ordinary DATA Trash binding"
        );
        sanitized(&error);
    }
}

#[tokio::test]
async fn owned_data_trash_reader_refuses_changed_metadata_and_bytes() {
    for arm in 0..12 {
        let (original, backup) = nodes(3);
        let mut first = metadata(3);
        let mut second = first.clone();
        let mut bytes = b"raw".to_vec();
        let mut package = false;
        match arm {
            0 => first["docwsid"] = json!("foreign"),
            1 => first["parentId"] = json!(PARENT),
            2 => first["restorePath"] = json!(null),
            3 => second["restorePath"] = json!("changed"),
            4 => second["etag"] = json!("changed"),
            5 => second["size"] = json!(4),
            6 => second["name"] = json!("Changed"),
            7 => second["extension"] = json!("key"),
            8 => bytes = b"bad".to_vec(),
            9 => bytes = b"raw-extra".to_vec(),
            10 => package = true,
            11 => first["type"] = json!("FOLDER"),
            _ => unreachable!(),
        }
        let (mut session, calls, stop, task) = fixture(first, second, &bytes, package).await;
        let error = session
            .verify_owned_data_in_trash(&original, &backup, &hex::encode(Sha256::digest(b"raw")))
            .await
            .err()
            .expect("changed Trash proof admitted");
        stop.cancel();
        task.await.unwrap();
        sanitized(&error);
        let expected = match arm {
            0..=2 | 11 => 1,
            8 | 9 => 3,
            10 => 2,
            _ => 4,
        };
        assert_eq!(calls.load(Ordering::SeqCst), expected, "arm {arm}");
    }
}

#[tokio::test]
async fn owned_data_trash_reader_refuses_unbound_completed_backup() {
    let (original, mut backup) = nodes(3);
    backup.etag = Some("unbound-v3".into());
    backup.content_version = backup.etag.clone();
    let item = metadata(3);
    let (mut session, calls, stop, task) = fixture(item.clone(), item, b"raw", false).await;
    let result = session
        .verify_owned_data_in_trash(&original, &backup, &hex::encode(Sha256::digest(b"raw")))
        .await;
    stop.cancel();
    task.await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    let error = result
        .err()
        .expect("completed backup revision was not bound");
    assert_eq!(
        error.to_string(),
        "owned ordinary DATA Trash receipt differs from the completed backup"
    );
    sanitized(&error);
}
