#![allow(clippy::unwrap_used)]
use super::*;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

pub(crate) fn scope() -> Scope {
    Scope {
        account: Uuid::new_v4().to_string(),
        provider: "icloud".into(),
        collection: "drive".into(),
    }
}
pub(crate) fn node(id: &str, parent: &str) -> Node {
    Node {
        id: format!("FOLDER::com.apple.CloudDocs::{id}"),
        parent_id: Some(parent.into()),
        name: id.into(),
        kind: NodeKind::Folder,
        size: 0,
        modified_unix: 0,
        etag: Some("v1".into()),
        content_version: None,
        target: None,
        package: false,
    }
}
pub(crate) fn entry(node: &Node, kind: &str) -> Value {
    json!({"drivewsid":node.id,"parentId":node.parent_id,"name":node.name,"type":kind,"etag":"v1","size":node.size})
}
pub(crate) async fn fixture(
    replies: Vec<(String, Vec<Value>)>,
) -> (ICloudReadSession, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        for (id, entries) in replies {
            let (mut peer, _) =
                tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept())
                    .await
                    .unwrap()
                    .unwrap();
            let mut request = Vec::new();
            loop {
                let mut bytes = [0; 4096];
                let n =
                    tokio::time::timeout(std::time::Duration::from_secs(5), peer.read(&mut bytes))
                        .await
                        .unwrap()
                        .unwrap();
                assert!(n > 0 && request.len() + n <= 16384);
                request.extend_from_slice(&bytes[..n]);
                let Some(end) = request.windows(4).position(|p| p == b"\r\n\r\n") else {
                    continue;
                };
                let end = end + 4;
                let headers = std::str::from_utf8(&request[..end]).unwrap().to_lowercase();
                assert!(
                    headers.starts_with("post /retrieveitemdetailsinfolders "),
                    "only read-only preflight allowed"
                );
                let size: usize = headers
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length: "))
                    .unwrap()
                    .parse()
                    .unwrap();
                if request.len() < end + size {
                    continue;
                }
                let body: Value = serde_json::from_slice(&request[end..end + size]).unwrap();
                assert_eq!(body[0]["drivewsid"], id);
                break;
            }
            let body=json!([{"drivewsid":id,"type":"FOLDER","numberOfItems":entries.len(),"items":entries}]).to_string();
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
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), listener.accept())
                .await
                .is_err(),
            "no request after rejected container preflight"
        );
    });
    let mut session = ICloudReadSession::new().unwrap();
    session.drive_endpoint = Some(endpoint.parse().unwrap());
    (session, server)
}
struct EmptyVault;
#[async_trait]
impl CredentialVault for EmptyVault {
    async fn load(&self, _: &str) -> anyhow::Result<Option<SecretString>> {
        Ok(None)
    }
    async fn save(&self, _: &str, _: SecretString) -> anyhow::Result<()> {
        panic!("no checkpoint write before rejected preflight")
    }
    async fn remove(&self, _: &str) -> anyhow::Result<()> {
        panic!("no checkpoint removal before rejected preflight")
    }
}
#[tokio::test]
async fn folder_create_refuses_app_owned_parent_before_mutation() {
    for kind in ["APP_CONTAINER", "APP_LIBRARY"] {
        let parent = node("parent", ROOT_ID);
        let (session, server) = fixture(vec![(ROOT_ID.into(), vec![entry(&parent, kind)])]).await;
        let p = ICloudFolderCreate {
            scope: scope(),
            parent: parent.clone(),
            checkpoint_vault: Arc::new(EmptyVault),
            session: Mutex::new(SessionState::Ready(Box::new(session))),
            #[cfg(feature = "write-probe")]
            reconciliation_only: false,
        };
        let request = MutationRequest {
            scope: p.scope.clone(),
            intent: MutationIntent::CreateFolder {
                parent: parent.id,
                name: "new".into(),
            },
        };
        assert!(matches!(
            p.mutate_operation(
                &Uuid::new_v4().to_string(),
                &request,
                None,
                &CancellationToken::new()
            )
            .await,
            Err(MutationError::Conflict)
        ));
        server.await.unwrap();
    }
}
#[tokio::test]
async fn folder_create_reconciliation_never_promotes_app_container_to_plain_folder() {
    for kind in ["APP_CONTAINER", "APP_LIBRARY", "FOLDER"] {
        let parent = node("parent", ROOT_ID);
        let child = node("child", &parent.id);
        let (session, server) = fixture(vec![(parent.id.clone(), vec![entry(&child, kind)])]).await;
        let p = ICloudFolderCreate {
            scope: scope(),
            parent,
            checkpoint_vault: Arc::new(EmptyVault),
            session: Mutex::new(SessionState::Ready(Box::new(session))),
            #[cfg(feature = "write-probe")]
            reconciliation_only: false,
        };
        let result = p.observe_child(&child.name, Some(&child.id)).await;
        if kind == "FOLDER" {
            assert_eq!(result.unwrap().unwrap().id, child.id);
        } else {
            assert!(matches!(result, Err(MutationError::Conflict)));
        }
        server.await.unwrap();
    }
}
