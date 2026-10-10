//! A saved session can be rejected after the mount's initial validation.
#![allow(clippy::unwrap_used)]
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

async fn fixture(
    statuses: Vec<u16>,
) -> (
    ICloudDrive,
    Scope,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/", listener.local_addr().unwrap());
    let requests = Arc::new(AtomicUsize::new(0));
    let counted = requests.clone();
    let server = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(5), async move {
            for status in statuses {
                let (mut peer, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                loop {
                    let mut chunk = [0; 4096];
                    let n = peer.read(&mut chunk).await.unwrap();
                    assert!(n > 0 && request.len() + n <= 16384);
                    request.extend_from_slice(&chunk[..n]);
                    let Some(end) = request.windows(4).position(|p| p == b"\r\n\r\n") else {
                        continue;
                    };
                    let end = end + 4;
                    let header = std::str::from_utf8(&request[..end]).unwrap().to_lowercase();
                    assert!(header.starts_with("post /retrieveitemdetailsinfolders "));
                    let size: usize = header
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .unwrap()
                        .parse()
                        .unwrap();
                    if request.len() < end + size {
                        continue;
                    }
                    let body: serde_json::Value =
                        serde_json::from_slice(&request[end..end + size]).unwrap();
                    assert_eq!(body[0]["drivewsid"], ROOT_ID);
                    assert_eq!(body[0]["partialData"], false);
                    break;
                }
                counted.fetch_add(1, Ordering::SeqCst);
                let body = if status == 200 {
                    serde_json::json!([{"drivewsid":ROOT_ID,"type":"FOLDER",
                        "numberOfItems":0,"items":[]}])
                    .to_string()
                } else {
                    "PRIVATE synthetic response".into()
                };
                peer.write_all(
                    format!("HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes(),
                )
                .await
                .unwrap();
            }
        })
        .await
        .expect("bounded loopback session-health fixture");
    });
    let mut session = ICloudReadSession::new().unwrap();
    session.drive_endpoint = Some(endpoint.parse().unwrap());
    let scope = Scope {
        account: "session-health-fixture".into(),
        provider: PROVIDER_ID.into(),
        collection: COLLECTION.into(),
    };
    let mut drive = ICloudDrive::on_demand_from_live_session(scope.clone(), session).unwrap();
    // The normal sealed provider has this flag and reaches Ready after loading
    // its vault. Supply only synthetic in-memory material for that same state.
    drive.keyring_backed = true;
    (drive, scope, requests, server)
}

#[tokio::test]
async fn saved_session_rejection_after_initial_success_reaches_later_feed_poll() {
    for status in [401, 403, 421] {
        let (drive, scope, requests, server) = fixture(vec![200, status, status]).await;
        let cancel = CancellationToken::new();
        let first = drive.changes(&scope, None, &cancel).await.unwrap();
        let cursor = first.checkpoint.cursor().clone();
        assert!(
            matches!(
                drive.children(&scope, ROOT_ID, None, &cancel).await,
                Err(ProviderError::Authentication)
            ),
            "folder metadata session refusal must reach authentication health"
        );
        let result = drive.changes(&scope, Some(&cursor), &cancel).await;
        assert!(
            matches!(result, Err(ProviderError::Authentication)),
            "a previously valid saved session must not keep publishing local feed success"
        );
        server.await.unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), 3);
    }
}

#[tokio::test]
async fn saved_session_transient_failure_is_not_authentication_and_retains_cursor() {
    let (drive, scope, requests, server) = fixture(vec![200, 503, 200]).await;
    let cancel = CancellationToken::new();
    let first = drive.changes(&scope, None, &cancel).await.unwrap();
    let cursor = first.checkpoint.cursor().clone();
    assert!(matches!(
        drive.changes(&scope, Some(&cursor), &cancel).await,
        Err(ProviderError::Unavailable)
    ));
    let next = drive.changes(&scope, Some(&cursor), &cancel).await.unwrap();
    assert!(next.changes.is_empty());
    assert_eq!(next.checkpoint.cursor(), &cursor);
    server.await.unwrap();
    assert_eq!(requests.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn foreground_live_session_feed_keeps_its_local_only_projection() {
    let scope = Scope {
        account: "foreground-fixture".into(),
        provider: PROVIDER_ID.into(),
        collection: COLLECTION.into(),
    };
    let drive =
        ICloudDrive::on_demand_from_live_session(scope.clone(), ICloudReadSession::new().unwrap())
            .unwrap();
    let cancel = CancellationToken::new();
    let first = drive.changes(&scope, None, &cancel).await.unwrap();
    let next = drive
        .changes(&scope, Some(first.checkpoint.cursor()), &cancel)
        .await
        .unwrap();
    assert!(next.changes.is_empty());
    assert_eq!(next.checkpoint.cursor(), first.checkpoint.cursor());
}

#[test]
fn folder_session_refusal_keeps_other_http_boundaries_uncertain() {
    for code in [421, 500, 503] {
        let status = reqwest::StatusCode::from_u16(code).unwrap();
        let error = crate::drive_request_failure(status, "generic Drive boundary");
        assert!(matches!(map_read_error(&error), ProviderError::Unavailable));
        assert!(matches!(
            crate::mutation_error(error),
            cirrove_core::mutation::MutationError::Uncertain
        ));
    }
    for code in [401, 403, 421, 500, 503] {
        let error = crate::content_request_failure(
            reqwest::StatusCode::from_u16(code).unwrap(),
            "signed content boundary",
        );
        assert!(matches!(map_read_error(&error), ProviderError::Unavailable));
        assert!(matches!(
            crate::mutation_error(error),
            cirrove_core::mutation::MutationError::Uncertain
        ));
    }
    assert!(matches!(
        crate::mutation_error(crate::content_request_failure(
            reqwest::StatusCode::INSUFFICIENT_STORAGE,
            "signed content boundary",
        )),
        cirrove_core::mutation::MutationError::InsufficientStorage
    ));
}
