#![allow(clippy::unwrap_used)]
use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinSet,
};

#[derive(Clone)]
struct Remote {
    version: u32,
    generation: u32,
    mode: &'static str,
    metadata: usize,
}
struct Fixture {
    graph: OneDrive,
    remote: Arc<StdMutex<Remote>>,
    requests: Arc<StdMutex<Vec<String>>>,
    entered: Arc<tokio::sync::Notify>,
    task: tokio::task::JoinHandle<()>,
    node: Node,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn scope() -> Scope {
    Scope {
        account: "account".into(),
        provider: "onedrive".into(),
        collection: "drive".into(),
    }
}
impl Fixture {
    fn session(&self) -> Arc<GraphReadSession> {
        Arc::new(GraphReadSession::new(self.graph.clone(), &scope(), &self.node).unwrap())
    }
    fn mode(&self, mode: &'static str) {
        self.remote.lock().unwrap().mode = mode;
    }
}
async fn fixture(size: u64) -> Fixture {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let graph = OneDrive::build(
        "account".into(),
        Arc::new(StaticToken(SecretString::from("test-token"))),
        Url::parse(&format!("{origin}/v1.0/")).unwrap(),
    )
    .unwrap()
    .with_experimental_read_sessions();
    let remote = Arc::new(StdMutex::new(Remote {
        version: 1,
        generation: 1,
        mode: "normal",
        metadata: 0,
    }));
    let requests = Arc::new(StdMutex::new(vec![]));
    let entered = Arc::new(tokio::sync::Notify::new());
    let (state, seen, start) = (remote.clone(), requests.clone(), entered.clone());
    let task = tokio::spawn(async move {
        let mut tasks = JoinSet::new();
        loop {
            tokio::select! {
                connection = listener.accept() => {
                    let (socket, _) = connection.unwrap();
                    tasks.spawn(serve(socket, origin.clone(), size, state.clone(), seen.clone(), start.clone()));
                }
                _ = tasks.join_next(), if !tasks.is_empty() => (),
            }
        }
    });
    Fixture {
        graph,
        remote,
        requests,
        entered,
        task,
        node: Node {
            id: "file".into(),
            name: "private-name".into(),
            parent_id: None,
            kind: NodeKind::File,
            size,
            modified_unix: 0,
            etag: Some("metadata-1".into()),
            content_version: Some("content-1".into()),
            target: None,
        },
    }
}
async fn serve(
    mut socket: TcpStream,
    origin: String,
    size: u64,
    state: Arc<StdMutex<Remote>>,
    requests: Arc<StdMutex<Vec<String>>>,
    entered: Arc<tokio::sync::Notify>,
) {
    let mut request = String::new();
    while !request.contains("\r\n\r\n") {
        let mut bytes = [0; 2048];
        let Ok(n) = socket.read(&mut bytes).await else {
            return;
        };
        if n == 0 {
            return;
        }
        request.push_str(&String::from_utf8_lossy(&bytes[..n]));
        assert!(request.len() < 16384);
    }
    let lower = request.to_ascii_lowercase();
    let download = lower.starts_with("get /download");
    requests.lock().unwrap().push(request);
    let remote = {
        let mut remote = state.lock().unwrap();
        if !download {
            remote.metadata += 1;
        }
        remote.clone()
    };
    let mut status = 200;
    let mut headers = String::new();
    let mut body = Vec::new();
    let mut repeat = None;
    if !download {
        if remote.mode == "denied" {
            status = 403;
        } else {
            let version = if remote.mode == "changed_during_setup" && remote.metadata > 1 {
                2
            } else if remote.mode == "metadata_only" {
                1
            } else {
                remote.version
            };
            let id = if remote.mode == "wrong_identity" {
                "other"
            } else {
                "file"
            };
            body = serde_json::to_vec(&serde_json::json!({"id":id,"file":{},"size":size,"cTag":format!("content-{version}"),"eTag":"unrelated-metadata-tag", "parentReference":{"driveId":"drive"},"@microsoft.graph.downloadUrl":format!("{origin}/download/{}?private-signature",remote.generation)})).unwrap();
        }
    } else {
        assert!(!lower.contains("authorization:"));
        assert!(lower.contains("accept-encoding: identity"));
        entered.notify_one();
        if remote.mode == "stall" {
            std::future::pending::<()>().await;
        }
        let range = lower
            .lines()
            .find_map(|l| l.strip_prefix("range: bytes="))
            .unwrap();
        let (a, b) = range.split_once('-').unwrap();
        let (offset, end): (u64, u64) = (a.parse().unwrap(), b.parse().unwrap());
        let tag = format!("\"body-{}\"", remote.version);
        let conditional = lower.lines().find_map(|l| l.strip_prefix("if-match: "));
        status = 206;
        if !lower.starts_with(&format!("get /download/{}?", remote.generation)) {
            status = 403;
        } else if remote.mode == "throttle" {
            status = 429;
            headers.push_str("Retry-After: 2\r\n");
        } else if conditional.is_some_and(|value| value != tag) && remote.mode != "ignore_condition"
        {
            status = 412;
        } else if remote.mode == "ignore_range" {
            status = 200;
        }
        if remote.mode != "missing" {
            headers.push_str(&format!(
                "ETag: {}{tag}\r\n",
                if remote.mode == "weak" { "W/" } else { "" }
            ));
        }
        headers.push_str(&format!(
            "Content-Range: bytes {}-{end}/{size}\r\n",
            if remote.mode == "wrong_range" {
                offset + 1
            } else {
                offset
            }
        ));
        if remote.mode == "encoded" {
            headers.push_str("Content-Encoding: gzip\r\n");
        }
        if status == 206 {
            let count = end - offset + 1;
            repeat = Some((
                if remote.mode == "short" {
                    count - 1
                } else if remote.mode == "oversize" {
                    count + 1
                } else {
                    count
                },
                if remote.version == 1 || remote.mode == "metadata_only" {
                    b'A'
                } else {
                    b'B'
                },
            ));
        }
    }
    let length = repeat.map_or(body.len() as u64, |r| r.0);
    let head = format!(
        "HTTP/1.1 {status} Test\r\nContent-Length: {length}\r\nConnection: close\r\n{headers}\r\n"
    );
    if socket.write_all(head.as_bytes()).await.is_err() {
        return;
    }
    if let Some((mut remaining, byte)) = repeat {
        let chunk = [byte; 64 * 1024];
        while remaining > 0 {
            let n = remaining.min(chunk.len() as u64) as usize;
            if socket.write_all(&chunk[..n]).await.is_err() {
                return;
            }
            remaining -= n as u64;
        }
    } else {
        let _ = socket.write_all(&body).await;
    }
}
async fn read(
    session: &GraphReadSession,
    offset: u64,
    size: u32,
) -> Result<Vec<u8>, ProviderError> {
    tokio::time::timeout(
        Duration::from_secs(5),
        session.read_range(offset, size, &CancellationToken::new()),
    )
    .await
    .unwrap()
}
fn expire(session: &GraphReadSession) {
    let mut state = session.state.lock().unwrap();
    let State::Bound(bound) = &*state else {
        panic!("not bound")
    };
    let mut replacement = (**bound).clone();
    replacement.expires = Instant::now() - Duration::from_secs(1);
    *state = State::Bound(Arc::new(replacement));
}
async fn concurrent(session: Arc<GraphReadSession>, count: u64) {
    let mut tasks = JoinSet::new();
    for index in 0..count {
        let session = session.clone();
        tasks.spawn(async move {
            assert_eq!(
                read(&session, index * 32, 32).await.unwrap(),
                vec![b'A'; 32]
            );
        });
    }
    while let Some(result) = tasks.join_next().await {
        result.unwrap();
    }
}

#[tokio::test]
async fn sequential_gibibyte_uses_one_setup_and_no_graph_per_later_block() {
    let f = fixture(1024 * 1024 * 1024).await;
    let s = f.session();
    for block in 0..256 {
        let bytes = read(&s, block * 4 * 1024 * 1024, 4 * 1024 * 1024)
            .await
            .unwrap();
        assert_eq!(bytes.len(), 4 * 1024 * 1024);
        assert!(bytes.iter().all(|b| *b == b'A'));
    }
    let c = f.graph.read_counters();
    assert_eq!(
        (
            c.graph_get_attempts,
            c.content_get_attempts,
            c.setups,
            c.renewals,
            c.conditional_ranges
        ),
        (2, 256, 1, 0, 255)
    );
    assert_eq!(c.content_body_bytes, 1024 * 1024 * 1024);
}

#[tokio::test]
async fn preview_random_reads_and_concurrent_readers_share_setup() {
    let f = fixture(3_100_000).await;
    let s = f.session();
    assert_eq!(read(&s, 0, 3_100_000).await.unwrap().len(), 3_100_000);
    assert_eq!(f.graph.read_counters().graph_get_attempts, 2);
    for offset in [100_000, 8192, 2_000_000, 0] {
        assert_eq!(read(&s, offset, 32).await.unwrap(), vec![b'A'; 32]);
    }
    concurrent(s, 32).await;
    assert_eq!(f.graph.read_counters().graph_get_attempts, 2);
    let f = fixture(3_100_000).await;
    concurrent(f.session(), 32).await;
    let c = f.graph.read_counters();
    assert_eq!(
        (c.graph_get_attempts, c.setups, c.content_get_attempts),
        (2, 1, 32)
    );
}

#[tokio::test]
async fn expiry_and_rejected_url_renew_once_for_concurrent_readers() {
    for by_url in [false, true] {
        let f = fixture(3_100_000).await;
        let s = f.session();
        read(&s, 0, 32).await.unwrap();
        if by_url {
            f.remote.lock().unwrap().generation = 2;
        } else {
            expire(&s);
        }
        concurrent(s, 32).await;
        let c = f.graph.read_counters();
        assert_eq!((c.graph_get_attempts, c.setups, c.renewals), (4, 1, 1));
    }
}

#[tokio::test]
async fn same_size_replacement_is_rejected_with_or_without_condition_support() {
    for mode in ["normal", "ignore_condition"] {
        let f = fixture(4096).await;
        let s = f.session();
        read(&s, 0, 32).await.unwrap();
        f.mode(mode);
        f.remote.lock().unwrap().version = 2;
        assert!(matches!(
            read(&s, 32, 32).await,
            Err(ProviderError::VersionChanged)
        ));
        assert_eq!(f.graph.read_counters().content_body_bytes, 32);
        expire(&s);
        assert!(matches!(
            read(&s, 32, 32).await,
            Err(ProviderError::VersionChanged)
        ));
    }
}

#[tokio::test]
async fn setup_checks_identity_and_both_metadata_observations() {
    for mode in ["changed_during_setup", "wrong_identity"] {
        let f = fixture(4096).await;
        f.mode(mode);
        assert!(matches!(
            read(&f.session(), 0, 32).await,
            Err(ProviderError::VersionChanged)
        ));
        assert_eq!(f.graph.read_counters().setups, 0);
    }
}

#[tokio::test]
async fn weak_or_missing_validators_keep_conservative_reads() {
    for mode in ["weak", "missing"] {
        let f = fixture(4096).await;
        f.mode(mode);
        let s = f.session();
        for offset in [0, 32, 64] {
            read(&s, offset, 32).await.unwrap();
        }
        let c = f.graph.read_counters();
        assert_eq!(
            (
                c.graph_get_attempts,
                c.content_get_attempts,
                c.fallback_ranges
            ),
            (6, 3, 2)
        );
    }
}

#[tokio::test]
async fn invalid_responses_never_release_bytes_and_throttle_is_shared() {
    for mode in [
        "wrong_range",
        "ignore_range",
        "encoded",
        "short",
        "oversize",
        "throttle",
    ] {
        let f = fixture(4096).await;
        let s = f.session();
        read(&s, 0, 32).await.unwrap();
        f.mode(mode);
        assert!(read(&s, 32, 32).await.is_err(), "{mode}");
        if mode == "throttle" {
            let c = f.graph.read_counters();
            assert!(matches!(
                f.graph
                    .node(&scope(), "file", &CancellationToken::new())
                    .await,
                Err(ProviderError::Throttled(_))
            ));
            assert!(matches!(
                read(&s, 64, 32).await,
                Err(ProviderError::Throttled(_))
            ));
            assert_eq!(
                f.graph.read_counters().graph_get_attempts,
                c.graph_get_attempts
            );
            assert_eq!(
                f.graph.read_counters().content_get_attempts,
                c.content_get_attempts
            );
        }
    }
}

#[tokio::test]
async fn renewal_denial_is_shared_without_serialized_retry_storm() {
    let f = fixture(4096).await;
    let s = f.session();
    read(&s, 0, 32).await.unwrap();
    expire(&s);
    f.mode("denied");
    let mut tasks = JoinSet::new();
    for _ in 0..32 {
        let s = s.clone();
        tasks.spawn(async move {
            assert!(matches!(
                read(&s, 32, 32).await,
                Err(ProviderError::Permission)
            ));
        });
    }
    while let Some(result) = tasks.join_next().await {
        result.unwrap();
    }
    assert_eq!(f.graph.read_counters().graph_get_attempts, 3);
}

#[tokio::test]
async fn cancelled_setup_releases_gate_and_does_not_publish_a_session() {
    let f = fixture(4096).await;
    f.mode("stall");
    let s = f.session();
    let cancel = CancellationToken::new();
    let future = s.read_range(0, 32, &cancel);
    tokio::pin!(future);
    tokio::select! {
        result = &mut future => panic!("unexpected {result:?}"),
        _ = tokio::time::timeout(Duration::from_secs(2),f.entered.notified()) => cancel.cancel(),
    }
    assert!(matches!(future.await, Err(ProviderError::Cancelled)));
    assert!(matches!(s.state().unwrap(), State::Cold));
    f.mode("normal");
    read(&s, 0, 32).await.unwrap();
    let count = f.requests.lock().unwrap().len();
    assert!(matches!(
        s.read_range(32, 32, &cancel).await,
        Err(ProviderError::Cancelled)
    ));
    assert_eq!(f.requests.lock().unwrap().len(), count);
}

#[tokio::test]
async fn ordinary_construction_stays_conservative_and_scope_is_checked() {
    let f = fixture(4096).await;
    let mut ordinary = f.graph.clone();
    ordinary.conditional_reads = false;
    assert!(
        ordinary
            .open_read_session(&scope(), &f.node, &CancellationToken::new())
            .await
            .unwrap()
            .is_none()
    );
    let mut wrong = scope();
    wrong.account = "other".into();
    assert!(
        f.graph
            .open_read_session(&wrong, &f.node, &CancellationToken::new())
            .await
            .is_err()
    );
    assert!(f.requests.lock().unwrap().is_empty());
    assert!(
        serde_json::to_value(f.graph.read_counters())
            .unwrap()
            .as_object()
            .unwrap()
            .values()
            .all(|v| v.is_u64())
    );
}

#[tokio::test]
async fn metadata_only_origin_tag_change_revalidates_without_stalling_reopens() {
    let f = fixture(4096).await;
    let s = f.session();
    read(&s, 0, 32).await.unwrap();
    f.mode("metadata_only");
    f.remote.lock().unwrap().version = 2;
    assert_eq!(read(&s, 32, 32).await.unwrap(), vec![b'A'; 32]);
    assert_eq!(read(&s, 64, 32).await.unwrap(), vec![b'A'; 32]);
    let c = f.graph.read_counters();
    assert_eq!((c.graph_get_attempts, c.setups, c.renewals), (4, 1, 1));
}

#[tokio::test]
async fn live_probe_exercises_the_session_and_reports_only_comparisons_and_counters() {
    let f = fixture(3_100_000).await;
    let report = f
        .graph
        .inspect_read_session(&scope(), &f.node, &CancellationToken::new())
        .await
        .unwrap();
    assert!(report.all_samples_match);
    assert_eq!(report.samples, 5);
    assert_eq!(report.after_first.graph_get_attempts, 2);
    assert_eq!(report.after_concurrent.graph_get_attempts, 2);
    assert_eq!(report.after_comparison.graph_get_attempts, 12);
    assert_eq!(report.after_comparison.content_body_bytes, 10 * 256 * 1024);
    let text = format!("{report:?} {}", serde_json::to_string(&report).unwrap());
    for private in [
        "test-token",
        "private-name",
        "private-signature",
        "body-1",
        "http://",
    ] {
        assert!(!text.contains(private));
    }
}

struct WindowSink {
    bytes: u64,
    fail: bool,
}

mod strong_windows;
#[async_trait]
impl ReadWindowSink for WindowSink {
    async fn write_chunk(&mut self, bytes: &[u8]) -> Result<(), ProviderError> {
        assert!(bytes.len() <= 64 * 1024);
        assert!(bytes.iter().all(|b| *b == b'A'));
        if self.fail {
            return Err(ProviderError::Unavailable);
        }
        self.bytes += bytes.len() as u64;
        Ok(())
    }
}
#[tokio::test]
async fn streamed_window_has_one_metadata_pair_and_bounded_chunks() {
    let f = fixture(128 * 1024 * 1024).await;
    f.mode("weak");
    let s = f.session();
    read(&s, 0, 32).await.unwrap();
    assert_eq!(s.window_limit(), 64 * 1024 * 1024);
    let mut sink = WindowSink {
        bytes: 0,
        fail: false,
    };
    s.read_window(
        4 * 1024 * 1024,
        64 * 1024 * 1024,
        &mut sink,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(sink.bytes, 64 * 1024 * 1024);
    let c = f.graph.read_counters();
    assert_eq!((c.graph_get_attempts, c.content_get_attempts), (4, 2));
    assert_eq!(c.content_body_bytes, 64 * 1024 * 1024 + 32);
}
#[tokio::test]
async fn a_fallback_session_reports_the_windows_it_serves() {
    // A session that cannot bind serves every later window through the conservative
    // path, and used to do so without touching any counter. Nothing distinguished
    // that from a read the session never saw, which is the shape a live measurement
    // actually produced: three windows, one setup, and a later read that reached the
    // provider while every counter stood still.
    let f = fixture(128 * 1024 * 1024).await;
    f.mode("weak");
    let s = f.session();
    read(&s, 0, 32).await.unwrap();
    for offset in [4 * 1024 * 1024, 16 * 1024 * 1024] {
        let mut sink = WindowSink {
            bytes: 0,
            fail: false,
        };
        s.read_window(
            offset,
            8 * 1024 * 1024,
            &mut sink,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(sink.bytes, 8 * 1024 * 1024);
    }
    let c = f.graph.read_counters();
    assert_eq!(c.fallback_windows, 2);
    // The session never bound, so it can never re-bind. A zero in `renewals` beside
    // a rising `fallback_windows` is the expected shape and not a missing renewal.
    assert_eq!((c.renewals, c.conditional_windows), (0, 0));
    // `setups` counts the attempt made from Cold, not a binding that succeeded: this
    // session recorded one and then served everything conservatively. A reading of
    // `setups: 1` on its own therefore does not establish that a session exists.
    assert_eq!(c.setups, 1);
}
#[tokio::test]
async fn streamed_window_rejects_response_faults_and_failed_final_version_check() {
    for mode in [
        "wrong_range",
        "encoded",
        "ignore_range",
        "short",
        "oversize",
        "changed_during_setup",
        "normal",
    ] {
        let f = fixture(16 * 1024 * 1024).await;
        f.mode("weak");
        let s = f.session();
        read(&s, 0, 32).await.unwrap();
        f.remote.lock().unwrap().metadata = 0;
        f.mode(mode);
        let mut sink = WindowSink {
            bytes: 0,
            fail: mode == "normal",
        };
        assert!(
            s.read_window(0, 8 * 1024 * 1024, &mut sink, &CancellationToken::new())
                .await
                .is_err(),
            "{mode}"
        );
        if mode == "changed_during_setup" {
            assert_eq!(sink.bytes, 8 * 1024 * 1024);
        }
        if ["wrong_range", "encoded", "ignore_range"].contains(&mode) {
            assert_eq!(sink.bytes, 0);
        }
    }
}

#[cfg(feature = "test-support")]
#[test]
fn synthetic_adapter_cannot_connect_to_remote_or_credentialed_endpoints() {
    for endpoint in [
        "https://127.0.0.1/v1.0/",
        "http://example.invalid/v1.0/",
        "http://user:secret@127.0.0.1/v1.0/",
        "https://graph.microsoft.com/v1.0/",
    ] {
        assert!(OneDrive::synthetic_loopback("account".into(), endpoint).is_err());
    }
}
