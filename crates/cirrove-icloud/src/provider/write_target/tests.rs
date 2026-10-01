#![allow(clippy::unwrap_used)]
use super::*;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

fn node() -> Node {
    Node {
        id: "FILE::com.apple.CloudDocs::fixture".into(),
        parent_id: Some(ROOT_ID.into()),
        name: "Unknown.unsupported".into(),
        kind: NodeKind::File,
        size: 3,
        modified_unix: 1,
        etag: Some("v1".into()),
        content_version: None,
        target: None,
        package: false,
    }
}
fn metadata(node: &Node) -> Value {
    json!({"items":[{"drivewsid":node.id,"parentId":node.parent_id,"type":"FILE",
        "name":node.name,"etag":node.etag,"size":node.size}]})
}
fn data() -> Value {
    json!({"data_token":{"url":"https://fixture.icloud-content.com/private"}})
}
struct Fixture {
    drive: ICloudDrive,
    scope: Scope,
    calls: Arc<std::sync::atomic::AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fixture {
    async fn new(replies: Vec<(&'static str, u16, Value)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = calls.clone();
        let task = tokio::spawn(async move {
            for (method, status, body) in replies {
                let (mut peer, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                loop {
                    let mut bytes = [0; 4096];
                    let n = peer.read(&mut bytes).await.unwrap();
                    assert!(n > 0 && request.len() + n < 16384);
                    request.extend_from_slice(&bytes[..n]);
                    let Some(end) = request.windows(4).position(|p| p == b"\r\n\r\n") else {
                        continue;
                    };
                    let end = end + 4;
                    let header = std::str::from_utf8(&request[..end]).unwrap();
                    assert!(
                        header.starts_with(method),
                        "unexpected endpoint or content request"
                    );
                    let len: usize = header
                        .lines()
                        .find_map(|s| {
                            s.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .map(str::to_owned)
                        })
                        .map(|s| s.parse().unwrap())
                        .unwrap_or(0);
                    if request.len() < end + len {
                        continue;
                    }
                    if method.starts_with("POST") {
                        let body: Value = serde_json::from_slice(&request[end..end + len]).unwrap();
                        assert_eq!(body["items"][0]["drivewsid"], node().id);
                    }
                    break;
                }
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if status == 0 {
                    std::future::pending::<()>().await;
                }
                let body = body.to_string();
                peer.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            }
        });
        let mut session = ICloudReadSession::new().unwrap();
        session.drive_endpoint = Some(endpoint.parse().unwrap());
        session.docs_endpoint = Some(endpoint.parse().unwrap());
        let scope = Scope {
            account: "write-target-fixture".into(),
            provider: PROVIDER_ID.into(),
            collection: COLLECTION.into(),
        };
        let drive = ICloudDrive::on_demand_from_live_session(scope.clone(), session).unwrap();
        Self {
            drive,
            scope,
            calls,
            task,
        }
    }
    async fn validate(&self, node: &Node) -> Result<(), ProviderError> {
        tokio::time::timeout(
            Duration::from_secs(3),
            self.drive
                .validate_write_target(&self.scope, node, &CancellationToken::new()),
        )
        .await
        .unwrap()
    }
    fn count(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}
const META: &str = "POST /retrieveItemDetails ";
const LOOKUP: &str = "GET /ws/com.apple.CloudDocs/download/by_id?document_id=fixture ";

#[tokio::test]
async fn unknown_extension_package_and_ambiguous_targets_refuse_without_content_get() {
    for reply in [
        json!({"package_token":{"url":"https://fixture.icloud-content.com/private"}}),
        json!({"package_token":{"url":"https://fixture.icloud-content.com/private"},"data_token":{"url":"https://fixture.icloud-content.com/private"}}),
        json!({}),
    ] {
        // Supply the second valid metadata observation even for a package:
        // removing the representation guard must admit it, not fail merely
        // because the synthetic server ran out of responses.
        let fixture = Fixture::new(vec![
            (META, 200, metadata(&node())),
            (LOOKUP, 200, reply),
            (META, 200, metadata(&node())),
        ])
        .await;
        assert!(fixture.validate(&node()).await.is_err());
        assert_eq!(fixture.count(), 2);
        assert!(fixture.drive.write_targets.lock().unwrap().0.is_empty());
    }
}

#[tokio::test]
async fn ordinary_data_admits_after_bracketing_metadata_and_cache_rechecks_revision() {
    let n = node();
    let mut changed = n.clone();
    changed.etag = Some("v2".into());
    let fixture = Fixture::new(vec![
        (META, 200, metadata(&n)),
        (LOOKUP, 200, data()),
        (META, 200, metadata(&n)),
        (META, 200, metadata(&n)),
        (META, 200, metadata(&changed)),
        (LOOKUP, 200, data()),
        (META, 200, metadata(&changed)),
    ])
    .await;
    fixture.validate(&n).await.unwrap();
    assert_eq!(fixture.count(), 3);
    fixture.validate(&n).await.unwrap();
    assert_eq!(
        fixture.count(),
        4,
        "cache must still revalidate exact metadata"
    );
    fixture.validate(&changed).await.unwrap();
    assert_eq!(
        fixture.count(),
        7,
        "changed revision must repeat representation lookup"
    );
    assert_eq!(fixture.drive.write_targets.lock().unwrap().0.len(), 1);
}

#[tokio::test]
async fn stale_identity_parent_etag_size_and_missing_size_never_admit() {
    for field in ["drivewsid", "parentId", "etag", "size", "missing-size"] {
        let n = node();
        let mut changed = metadata(&n);
        match field {
            "size" => changed["items"][0][field] = json!(99),
            "missing-size" => {
                changed["items"][0].as_object_mut().unwrap().remove("size");
            }
            _ => changed["items"][0][field] = json!("foreign"),
        }
        let fixture = Fixture::new(vec![
            (META, 200, metadata(&n)),
            (LOOKUP, 200, data()),
            (META, 200, changed),
        ])
        .await;
        assert!(fixture.validate(&n).await.is_err(), "{field}");
        assert_eq!(fixture.count(), 3);
        assert!(fixture.drive.write_targets.lock().unwrap().0.is_empty());
    }
}

#[tokio::test]
async fn cancelled_foreign_scope_and_lookup_failure_do_not_admit_or_cache() {
    let fixture = Fixture::new(vec![
        (META, 200, metadata(&node())),
        (LOOKUP, 503, json!({})),
    ])
    .await;
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        fixture
            .drive
            .validate_write_target(&fixture.scope, &node(), &cancel)
            .await,
        Err(ProviderError::Cancelled)
    ));
    let mut foreign = fixture.scope.clone();
    foreign.account = "other".into();
    assert!(
        fixture
            .drive
            .validate_write_target(&foreign, &node(), &CancellationToken::new())
            .await
            .is_err()
    );
    assert_eq!(fixture.count(), 0);
    assert!(fixture.validate(&node()).await.is_err());
    assert_eq!(fixture.count(), 2);
    assert!(fixture.drive.write_targets.lock().unwrap().0.is_empty());
}

#[test]
fn classification_cache_is_bounded_expires_and_invalidates_changed_nodes() {
    let mut cache = Cache::default();
    for i in 0..CAPACITY + 1 {
        let mut n = node();
        n.id = format!("file-{i}");
        cache.insert(&n);
    }
    assert_eq!(cache.0.len(), CAPACITY);
    cache.insert(&node());
    assert!(cache.matches(&node()));
    let mut changed = node();
    changed.size += 1;
    assert!(!cache.matches(&changed));
    cache.insert(&node());
    cache.0.back_mut().unwrap().1 = Instant::now() - MAX_AGE;
    assert!(!cache.matches(&node()));
}

#[tokio::test]
async fn cancellation_during_metadata_request_releases_admission_and_never_caches() {
    let fixture = Fixture::new(vec![(META, 0, json!({}))]).await;
    let cancel = CancellationToken::new();
    let signal = cancel.clone();
    let calls = fixture.calls.clone();
    let canceller = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(2), async {
            while calls.load(std::sync::atomic::Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        signal.cancel();
    });
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        fixture
            .drive
            .validate_write_target(&fixture.scope, &node(), &cancel),
    )
    .await
    .unwrap();
    canceller.await.unwrap();
    assert!(matches!(result, Err(ProviderError::Cancelled)));
    assert_eq!(fixture.drive.reads.available_permits(), 4);
    assert!(fixture.drive.write_targets.lock().unwrap().0.is_empty());
}

#[tokio::test]
async fn stale_observation_invalidates_previously_accepted_cache_entry() {
    let n = node();
    let mut changed = n.clone();
    changed.etag = Some("foreign".into());
    let fixture = Fixture::new(vec![
        (META, 200, metadata(&n)),
        (LOOKUP, 200, data()),
        (META, 200, metadata(&n)),
        (META, 200, metadata(&changed)),
    ])
    .await;
    fixture.validate(&n).await.unwrap();
    assert_eq!(fixture.drive.write_targets.lock().unwrap().0.len(), 1);
    assert!(matches!(
        fixture.validate(&n).await,
        Err(ProviderError::VersionChanged)
    ));
    assert_eq!(fixture.count(), 4);
    assert!(fixture.drive.write_targets.lock().unwrap().0.is_empty());
}
