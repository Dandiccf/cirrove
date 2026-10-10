//! Raw provider types must preserve protection without hiding readable children.
#![allow(clippy::unwrap_used)]
use super::*;
use serde_json::json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[tokio::test]
async fn app_containers_remain_readable_protected_folders_and_plain_folders_stay_plain() {
    for kind in ["APP_CONTAINER", "APP_LIBRARY", "FOLDER"] {
        let protected = kind != "FOLDER";
        let parent = "FOLDER::com.apple.CloudDocs::container";
        let nested = "FOLDER::com.apple.CloudDocs::nested";
        let file = "FILE::com.apple.CloudDocs::document";
        // The same name and identity shape exercise the provider type, not a
        // filename suffix or an app-zone naming heuristic.
        let entry = json!({"drivewsid":parent,"parentId":ROOT_ID,"type":kind,"name":"Documents","etag":"parent-v1"});
        let child = json!({"drivewsid":nested,"parentId":parent,"type":"FOLDER","name":"Nested","etag":"nested-v1"});
        let leaf = json!({"drivewsid":file,"parentId":nested,"type":"FILE","name":"ordinary","extension":"txt","etag":"file-v1","size":3});
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            for (id, kind, children) in [
                (ROOT_ID, "FOLDER", vec![entry]),
                (parent, kind, vec![child]),
                (nested, "FOLDER", vec![leaf]),
            ] {
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
                    let headers = std::str::from_utf8(&request[..end]).unwrap().to_lowercase();
                    assert!(headers.starts_with("post /retrieveitemdetailsinfolders "));
                    let size: usize = headers
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
                    assert_eq!(body[0]["drivewsid"], id);
                    assert_eq!(body[0]["partialData"], false);
                    break;
                }
                let body = json!([{"drivewsid":id,"type":kind,"numberOfItems":children.len(),"items":children}]).to_string();
                peer.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
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
            account: "container-fixture".into(),
            provider: PROVIDER_ID.into(),
            collection: COLLECTION.into(),
        };
        let drive = ICloudDrive::on_demand_from_live_session(scope.clone(), session).unwrap();
        let cancel = CancellationToken::new();
        tokio::time::timeout(Duration::from_secs(5), async {
            let root = drive
                .children(&scope, ROOT_ID, None, &cancel)
                .await
                .unwrap();
            assert_eq!(root.nodes.len(), 1);
            let container = &root.nodes[0];
            assert_eq!(container.kind, NodeKind::Folder);
            assert_eq!(container.package, protected, "raw type {kind}");
            assert_eq!(container.id, parent);
            assert!(
                !packages::package(container),
                "app folders are not generated FILE archives"
            );
            let page = drive
                .children_for_node(&scope, container, None, &cancel)
                .await
                .unwrap();
            assert_eq!(page.nodes.len(), 1);
            assert_eq!(page.nodes[0].id, nested);
            assert!(!page.nodes[0].package);
            let page = drive
                .children_for_node(&scope, &page.nodes[0], None, &cancel)
                .await
                .unwrap();
            assert_eq!(page.nodes.len(), 1);
            assert_eq!(page.nodes[0].id, file);
            assert_eq!(page.nodes[0].kind, NodeKind::File);
            assert!(!page.nodes[0].package);
            server.await.unwrap();
        })
        .await
        .expect("bounded read-only container traversal");
    }
}
