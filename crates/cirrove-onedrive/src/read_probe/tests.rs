#![allow(clippy::unwrap_used)]
use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

struct Fixture {
    graph: OneDrive,
    requests: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn fixture(mode: &'static str) -> Fixture {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let graph = OneDrive::build(
        "account".into(),
        Arc::new(StaticToken(SecretString::from("synthetic-token"))),
        Url::parse(&format!("{origin}/v1.0/")).unwrap(),
    )
    .unwrap();
    let requests = Arc::new(Mutex::new(vec![]));
    let seen = requests.clone();
    let task = tokio::spawn(async move {
        let mut metadata = 0;
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = String::new();
            while !request.contains("\r\n\r\n") {
                let mut buf = [0; 2048];
                let n = socket.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                request.push_str(&String::from_utf8_lossy(&buf[..n]));
            }
            let lower = request.to_ascii_lowercase();
            let download = request.starts_with("GET /download");
            seen.lock().await.push(request);
            if mode == "stall" && download {
                std::future::pending::<()>().await;
            }
            let (status, headers, body) = if download {
                let wrong = lower.contains("cirrove-probe-mismatch");
                let status = if mode == "throttle" {
                    429
                } else if mode != "ignore_conditions" && wrong && lower.contains("if-match:") {
                    412
                } else if mode == "ignore_range"
                    || (mode != "ignore_conditions" && wrong && lower.contains("if-range:"))
                {
                    200
                } else {
                    206
                };
                let tag = match mode {
                    "missing" => String::new(),
                    "weak" => "ETag: W/\"private-representation\"\r\n".into(),
                    "changed_etag" if lower.contains("if-match:") && !wrong => {
                        "ETag: \"other-representation\"\r\n".into()
                    }
                    _ => "ETag: \"private-representation\"\r\n".into(),
                };
                let range = if mode == "wrong_range" {
                    "bytes 1-8/9"
                } else {
                    "bytes 0-7/9"
                };
                let body = if mode == "oversize" {
                    "123456789"
                } else if mode == "short" {
                    "1234"
                } else if mode == "changed_bytes" && lower.contains("if-match:") && !wrong {
                    "87654321"
                } else {
                    "12345678"
                };
                (
                    status,
                    format!("{tag}Content-Range: {range}\r\nRetry-After: 1\r\n"),
                    body.to_owned(),
                )
            } else {
                metadata += 1;
                let version = if mode == "changed" && metadata > 1 {
                    "v2"
                } else {
                    "v1"
                };
                (200, String::new(), serde_json::json!({"id":"file","size":9,"file":{},"eTag":version,"@microsoft.graph.downloadUrl":format!("{origin}/download?private-signed-url")}).to_string())
            };
            let response = format!(
                "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    Fixture {
        graph,
        requests,
        task,
    }
}
fn scope() -> Scope {
    Scope {
        account: "account".into(),
        provider: "onedrive".into(),
        collection: "drive".into(),
    }
}
fn node() -> Node {
    Node {
        id: "file".into(),
        name: "private-name".into(),
        parent_id: None,
        kind: NodeKind::File,
        size: 9,
        modified_unix: 0,
        etag: Some("v1".into()),
        content_version: None,
        target: None,
    }
}
fn observation<'a>(report: &'a ReadValidationReport, condition: &str) -> &'a Observation {
    report
        .observations
        .iter()
        .find(|o| o.condition == condition)
        .unwrap()
}

#[tokio::test]
async fn observes_positive_and_negative_conditions_without_disclosing_transport_material() {
    let f = fixture("honors").await;
    let report = f
        .graph
        .inspect_read_validation(&scope(), &node(), &CancellationToken::new())
        .await
        .unwrap();
    assert!(report.metadata_unchanged);
    assert_eq!(report.metadata_operations, 2);
    assert_eq!(report.content_operations, 5);
    assert_eq!(report.sample_bytes, 8);
    assert_eq!(observation(&report, "if_match_mismatch").status, 412);
    assert_eq!(observation(&report, "if_range_mismatch").status, 200);
    assert_eq!(
        observation(&report, "if_range_mismatch").retained_body_bytes,
        0
    );
    for name in ["if_match_same", "if_range_same"] {
        let o = observation(&report, name);
        assert!(o.exact_range && o.body_complete && o.strong_etag);
        assert_eq!(o.body_matches_initial, Some(true));
        assert_eq!(o.etag_matches_initial, Some(true));
    }
    let json = serde_json::to_string(&report).unwrap();
    let debug = format!("{report:?}");
    for private in [
        "synthetic-token",
        "private-representation",
        "private-signed-url",
        "private-name",
        "12345678",
        "http://",
    ] {
        assert!(!json.contains(private) && !debug.contains(private));
    }
    let requests = f.requests.lock().await;
    assert_eq!(requests.len(), 7);
    assert!(requests.iter().all(|r| r.starts_with("GET ")));
    for request in requests.iter().filter(|r| r.starts_with("GET /download")) {
        assert!(!request.to_ascii_lowercase().contains("authorization:"));
        assert!(request.to_ascii_lowercase().contains("range: bytes=0-7"));
    }
}

#[tokio::test]
async fn records_ignored_conditions_and_never_reads_a_full_file_response() {
    let f = fixture("ignore_conditions").await;
    let report = f
        .graph
        .inspect_read_validation(&scope(), &node(), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(observation(&report, "if_match_mismatch").status, 206);
    assert_eq!(observation(&report, "if_range_mismatch").status, 206);
    let f = fixture("ignore_range").await;
    let report = f
        .graph
        .inspect_read_validation(&scope(), &node(), &CancellationToken::new())
        .await
        .unwrap();
    assert!(
        report
            .observations
            .iter()
            .all(|o| !o.body_complete && o.retained_body_bytes == 0)
    );
}

#[tokio::test]
async fn weak_missing_changed_or_incomplete_representations_are_visible() {
    for mode in ["weak", "missing"] {
        let f = fixture(mode).await;
        let report = f
            .graph
            .inspect_read_validation(&scope(), &node(), &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(report.content_operations, 3);
        assert!(!report.observations[0].strong_etag);
    }
    for mode in ["oversize", "short", "wrong_range"] {
        let f = fixture(mode).await;
        let report = f
            .graph
            .inspect_read_validation(&scope(), &node(), &CancellationToken::new())
            .await
            .unwrap();
        assert!(
            report
                .observations
                .iter()
                .all(|o| !o.body_complete && o.retained_body_bytes <= 8)
        );
    }
    let f = fixture("changed").await;
    let report = f
        .graph
        .inspect_read_validation(&scope(), &node(), &CancellationToken::new())
        .await
        .unwrap();
    assert!(!report.metadata_unchanged);
    let f = fixture("changed_etag").await;
    let report = f
        .graph
        .inspect_read_validation(&scope(), &node(), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        observation(&report, "if_match_same").etag_matches_initial,
        Some(false)
    );
    let f = fixture("changed_bytes").await;
    let report = f
        .graph
        .inspect_read_validation(&scope(), &node(), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        observation(&report, "if_match_same").body_matches_initial,
        Some(false)
    );
}

#[tokio::test]
async fn cancellation_scope_validation_and_shared_throttling_bound_probe_work() {
    let f = fixture("stall").await;
    let cancel = CancellationToken::new();
    let result = tokio::time::timeout(Duration::from_secs(1), async {
        let s = scope();
        let n = node();
        let pending = f.graph.inspect_read_validation(&s, &n, &cancel);
        tokio::pin!(pending);
        tokio::select! {
            result = &mut pending => panic!("unexpected completion: {result:?}"),
            _ = tokio::time::sleep(Duration::from_millis(30)) => cancel.cancel(),
        }
        pending.await
    })
    .await
    .unwrap();
    assert!(matches!(result, Err(ProviderError::Cancelled)));
    let f = fixture("throttle").await;
    let mut wrong = scope();
    wrong.account = "other".into();
    assert!(
        f.graph
            .inspect_read_validation(&wrong, &node(), &CancellationToken::new())
            .await
            .is_err()
    );
    assert!(f.requests.lock().await.is_empty());
    assert!(matches!(
        f.graph
            .inspect_read_validation(&scope(), &node(), &CancellationToken::new())
            .await,
        Err(ProviderError::Throttled(_))
    ));
    let count = f.requests.lock().await.len();
    assert!(matches!(
        f.graph
            .node(&scope(), "file", &CancellationToken::new())
            .await,
        Err(ProviderError::Throttled(_))
    ));
    assert_eq!(count, f.requests.lock().await.len());
}

#[test]
fn strong_entity_tags_require_http_syntax() {
    for valid in ["\"\"", "\"abc\"", "\"!#~\""] {
        assert!(strong_etag(&HeaderValue::from_str(valid).unwrap()));
    }
    for invalid in ["abc", "W/\"abc\"", "\"a b\"", "\"a\"b\""] {
        assert!(!strong_etag(&HeaderValue::from_str(invalid).unwrap()));
    }
}
