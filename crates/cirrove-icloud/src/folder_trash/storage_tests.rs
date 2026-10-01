#![allow(clippy::unwrap_used)]
use super::*;
use serde_json::json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[tokio::test]
async fn folder_trash_adapter_preserves_storage_refusal_during_preflight_send_and_reconciliation() {
    for stage in ["parent", "children", "trash", "send", "confirmation"] {
        for status in [507, 503] {
            let before = Node {
                id: "FOLDER::com.apple.CloudDocs::fixture".into(),
                parent_id: Some(ROOT_ID.into()),
                name: "Empty".into(),
                kind: NodeKind::Folder,
                size: 0,
                modified_unix: 0,
                etag: Some("v1".into()),
                content_version: None,
                target: None,
                package: false,
            };
            let entry = json!({"drivewsid":before.id,"parentId":ROOT_ID,"name":before.name,"type":"FOLDER","etag":"v1"});
            let parent =
                json!([{"drivewsid":ROOT_ID,"type":"FOLDER","numberOfItems":1,"items":[entry]}])
                    .to_string();
            let empty_parent =
                json!([{"drivewsid":ROOT_ID,"type":"FOLDER","numberOfItems":0,"items":[]}])
                    .to_string();
            let empty_folder =
                json!([{"drivewsid":before.id,"type":"FOLDER","numberOfItems":0,"items":[]}])
                    .to_string();
            let listing = "/retrieveitemdetailsinfolders";
            let mut replies = vec![];
            if stage != "parent" {
                replies.push((
                    listing,
                    200,
                    if stage == "trash" {
                        empty_parent.clone()
                    } else {
                        parent
                    },
                ));
            }
            if matches!(stage, "send" | "confirmation") {
                replies.push((listing, 200, empty_folder));
            }
            if stage == "confirmation" {
                replies.push((
                    "/moveitemstotrash",
                    200,
                    json!({"items":[{"status":"OK"}]}).to_string(),
                ));
                replies.push((listing, 200, empty_parent));
            }
            replies.push((
                if stage == "send" {
                    "/moveitemstotrash"
                } else {
                    listing
                },
                status,
                "PRIVATE RESPONSE".into(),
            ));
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}/", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                tokio::time::timeout(std::time::Duration::from_secs(3),async {
                    for (path,status,body) in replies {
                        let (mut peer,_)=listener.accept().await.unwrap();
                        let mut request=Vec::new();
                        loop {
                            let mut bytes=[0;4096];
                            let n=peer.read(&mut bytes).await.unwrap();
                            assert!(n>0 && request.len()+n<=16384);
                            request.extend_from_slice(&bytes[..n]);
                            let Some(end)=request.windows(4).position(|p|p==b"\r\n\r\n") else {continue};
                            let end=end+4;
                            let headers=std::str::from_utf8(&request[..end]).unwrap().to_lowercase();
                            assert!(headers.starts_with(&format!("post {path} ")));
                            let size:usize=headers.lines().find_map(|l|l.strip_prefix("content-length: ")).unwrap().parse().unwrap();
                            if request.len()<end+size {continue;}
                            break;
                        }
                        peer.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
                    }
                    assert!(tokio::time::timeout(std::time::Duration::from_millis(100),listener.accept()).await.is_err(),"no further request after refused storage");
                }).await.expect("bounded folder Trash adapter fixture");
            });
            let mut session = ICloudReadSession::new().unwrap();
            session.drive_endpoint = Some(endpoint.parse().unwrap());
            let p = ICloudFolderTrash {
                scope: Scope {
                    account: Uuid::new_v4().to_string(),
                    provider: "icloud".into(),
                    collection: "drive".into(),
                },
                before: before.clone(),
                session: Mutex::new(SessionState::Ready(Box::new(session))),
                #[cfg(feature = "write-probe")]
                discard_response: AtomicBool::new(false),
            };
            let request = MutationRequest {
                scope: p.scope.clone(),
                intent: MutationIntent::RemoveFolder {
                    before: before.clone(),
                },
            };
            let error = if matches!(stage, "send" | "confirmation") {
                p.mutate_prepared(&request, Some(&before.id), &CancellationToken::new())
                    .await
                    .err()
                    .unwrap()
            } else {
                p.prepare_mutation(&request, &CancellationToken::new())
                    .await
                    .err()
                    .unwrap()
            };
            if status == 507 {
                assert!(
                    matches!(error, MutationError::InsufficientStorage),
                    "{stage}: {error}"
                );
            } else {
                assert!(
                    matches!(error, MutationError::Uncertain),
                    "{stage}: {error}"
                );
            }
            assert!(!error.to_string().contains("PRIVATE"));
            server.await.unwrap();
        }
    }
}
