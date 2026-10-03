#![allow(clippy::unwrap_used)]
use super::*;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const ID: &str = "FILE::com.apple.CloudDocs::cold";

async fn fixture(
    changed: bool,
    rejected: Option<&'static str>,
) -> (ICloudDrive, Scope, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        for step in 0..if rejected.is_some() { 1 } else { 2 } {
            let (mut peer, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut chunk = [0; 4096];
                let n = peer.read(&mut chunk).await.unwrap();
                assert!(n > 0 && request.len() < 16384);
                request.extend_from_slice(&chunk[..n]);
                let Some(end) = request.windows(4).position(|p| p == b"\r\n\r\n") else {
                    continue;
                };
                let end = end + 4;
                let headers = std::str::from_utf8(&request[..end]).unwrap().to_lowercase();
                let len: usize = headers
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length: "))
                    .unwrap()
                    .parse()
                    .unwrap();
                if request.len() < end + len {
                    continue;
                }
                let body: serde_json::Value =
                    serde_json::from_slice(&request[end..end + len]).unwrap();
                if step == 0 {
                    assert!(headers.starts_with("post /retrieveitemdetails "));
                    assert_eq!(body["items"][0]["drivewsid"], ID);
                } else {
                    assert!(headers.starts_with("post /retrieveitemdetailsinfolders "));
                    assert_eq!(body[0]["drivewsid"], ROOT_ID);
                }
                break;
            }
            let mut entry = json!({"drivewsid":ID,"type":"FILE","name":"Cold","extension":"txt","size":3,"etag":if changed && step == 1 {"v2"} else {"v1"},"parentId":ROOT_ID});
            match rejected {
                Some("trash") => entry["parentId"] = "TRASH_ROOT".into(),
                Some("restore") => entry["restorePath"] = json!(["former-parent"]),
                Some("unqualified") => entry["parentId"] = "root".into(),
                Some("size") => {
                    entry.as_object_mut().unwrap().remove("size");
                }
                Some("identity") => entry["drivewsid"] = "FILE::com.apple.CloudDocs::other".into(),
                _ => {}
            }
            let body = if step == 0 {
                json!({"items":[entry]})
            } else {
                json!([{"drivewsid":ROOT_ID,"type":"FOLDER","numberOfItems":1,"items":[entry]}])
            }
            .to_string();
            peer.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        }
    });
    let mut session = ICloudReadSession::new().unwrap();
    session.drive_endpoint = Some(endpoint.parse().unwrap());
    let scope = Scope {
        account: "cold-fixture".into(),
        provider: PROVIDER_ID.into(),
        collection: COLLECTION.into(),
    };
    (
        ICloudDrive::on_demand_from_live_session(scope.clone(), session).unwrap(),
        scope,
        server,
    )
}

#[tokio::test]
async fn cold_file_resolves_only_after_parent_revision_confirmation() {
    let (drive, scope, server) = fixture(false, None).await;
    let node = drive
        .node(&scope, ID, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(node.id, ID);
    assert_eq!(node.parent_id.as_deref(), Some(ROOT_ID));
    assert_eq!(node.name, "Cold.txt");
    assert_eq!(node.etag.as_deref(), Some("v1"));
    assert_eq!(node.size, 3);
    server.await.unwrap();
}

#[tokio::test]
async fn cold_file_refuses_a_revision_changed_during_resolution() {
    let (drive, scope, server) = fixture(true, None).await;
    assert!(matches!(
        drive.node(&scope, ID, &CancellationToken::new()).await,
        Err(ProviderError::VersionChanged)
    ));
    server.await.unwrap();
}

#[tokio::test]
async fn cold_file_never_publishes_trash_incomplete_or_foreign_metadata() {
    for case in ["trash", "restore", "unqualified", "size", "identity"] {
        let (drive, scope, server) = fixture(false, Some(case)).await;
        assert!(
            drive
                .node(&scope, ID, &CancellationToken::new())
                .await
                .is_err(),
            "{case}"
        );
        server.await.unwrap();
    }
}

#[tokio::test]
async fn cold_file_checks_scope_and_cancellation_before_network() {
    let scope = Scope {
        account: "local".into(),
        provider: PROVIDER_ID.into(),
        collection: COLLECTION.into(),
    };
    let drive =
        ICloudDrive::on_demand_from_live_session(scope.clone(), ICloudReadSession::new().unwrap())
            .unwrap();
    let foreign = Scope {
        account: "other".into(),
        ..scope.clone()
    };
    let cancel = CancellationToken::new();
    assert!(matches!(
        drive.node(&foreign, ID, &cancel).await,
        Err(ProviderError::Permission)
    ));
    cancel.cancel();
    assert!(matches!(
        drive.node(&scope, ID, &cancel).await,
        Err(ProviderError::Cancelled)
    ));
}
