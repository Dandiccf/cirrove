//! Actual node() → HTTP envelope → existing Node projection, no cloud account.
#![allow(clippy::unwrap_used)]
use super::*;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
const ID: &str = "FOLDER::com.apple.CloudDocs::owned-parent";
fn envelope() -> Value {
    json!({"drivewsid":ID,"type":"FOLDER","zone":"com.apple.CloudDocs","parentId":ROOT_ID,"name":"Import destination","etag":"folder-v1","numberOfItems":0,"items":[]})
}
async fn fixture(entry: Value) -> (ICloudDrive, Scope, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(3), async move {
            let (mut peer, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut bytes = [0; 4096];
                let n = peer.read(&mut bytes).await.unwrap();
                assert!(n > 0 && request.len() + n <= 16384);
                request.extend_from_slice(&bytes[..n]);
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
                let body: Value = serde_json::from_slice(&request[end..end + size]).unwrap();
                assert_eq!(
                    body,
                    json!([{"drivewsid":ID,"partialData":false,"includeHierarchy":false}])
                );
                break;
            }
            let body = json!([entry]).to_string();
            peer.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        })
        .await
        .unwrap();
    });
    let mut session = ICloudReadSession::new().unwrap();
    session.drive_endpoint = Some(endpoint.parse().unwrap());
    let scope = Scope {
        account: "folder-node-fixture".into(),
        provider: PROVIDER_ID.into(),
        collection: COLLECTION.into(),
    };
    let drive = ICloudDrive::on_demand_from_live_session(scope.clone(), session).unwrap();
    (drive, scope, server)
}
#[tokio::test]
async fn cold_plain_folder_node_uses_exact_folder_envelope_without_file_lookup() {
    let (drive, scope, server) = fixture(envelope()).await;
    let node = tokio::time::timeout(
        Duration::from_secs(2),
        drive.node(&scope, ID, &CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(node.id, ID);
    assert_eq!(node.parent_id.as_deref(), Some(ROOT_ID));
    assert_eq!(node.name, "Import destination");
    assert_eq!(node.kind, NodeKind::Folder);
    assert!(!node.package);
    assert!(node.target.is_none());
    assert_eq!(node.etag.as_deref(), Some("folder-v1"));
    server.await.unwrap();
}
#[tokio::test]
async fn cold_folder_node_refuses_foreign_protected_incomplete_and_recovered_envelopes() {
    for case in [
        "id",
        "file",
        "app-container",
        "app-library",
        "zone",
        "parent-zone",
        "parent-self",
        "parent-missing",
        "trash",
        "restore",
        "incomplete",
    ] {
        let mut entry = envelope();
        match case {
            "id" => entry["drivewsid"] = "FOLDER::com.apple.CloudDocs::other".into(),
            "file" => entry["type"] = "FILE".into(),
            "app-container" => entry["type"] = "APP_CONTAINER".into(),
            "app-library" => entry["type"] = "APP_LIBRARY".into(),
            "zone" => entry["zone"] = "shared.zone".into(),
            "parent-zone" => entry["parentId"] = "FOLDER::shared.zone::other".into(),
            "parent-self" => entry["parentId"] = ID.into(),
            "parent-missing" => {
                entry.as_object_mut().unwrap().remove("parentId");
            }
            "trash" => entry["parentId"] = "TRASH_ROOT".into(),
            "restore" => entry["restorePath"] = json!(["old-parent"]),
            _ => entry["numberOfItems"] = 1.into(),
        }
        let (drive, scope, server) = fixture(entry).await;
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            drive.node(&scope, ID, &CancellationToken::new()),
        )
        .await
        .unwrap();
        if matches!(case, "restore" | "trash") {
            assert!(
                matches!(result, Err(ProviderError::NotFound)),
                "{case}: {result:?}"
            );
        } else {
            assert!(result.is_err(), "{case}");
        }
        server.await.unwrap();
    }
}
#[tokio::test]
async fn cold_folder_node_rejects_foreign_scope_cancel_and_nonowned_ids_before_network() {
    let scope = Scope {
        account: "folder-node-fixture".into(),
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
    for id in [
        "FOLDER::shared.zone::folder",
        "FOLDER::com.apple.CloudDocs::folder::owner",
        "FOLDER::com.apple.CloudDocs::",
        "APP_CONTAINER::com.apple.CloudDocs::folder",
        "FOLDER::com.apple.CloudDocs::TRASH_ROOT",
    ] {
        assert!(
            matches!(
                drive.node(&scope, id, &CancellationToken::new()).await,
                Err(ProviderError::Unavailable) | Err(ProviderError::NotFound)
            ),
            "{id}"
        );
    }
}
