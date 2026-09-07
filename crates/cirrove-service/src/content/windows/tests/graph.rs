//! Actual OneDrive HTTP adapter -> shared sessions -> disk cache, synthetic only.
use super::*;
use cirrove_core::{CancellationToken, Node, NodeKind, Scope};
use cirrove_onedrive::OneDrive;
use std::sync::atomic::AtomicUsize;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinSet,
};
struct Server {
    task: tokio::task::JoinHandle<()>,
    provider: OneDrive,
    graph: Arc<AtomicUsize>,
    content: Arc<AtomicUsize>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn server(size: u64) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let provider = OneDrive::synthetic_loopback("account".into(), &format!("{origin}/v1.0/"))
        .unwrap()
        .with_experimental_read_sessions();
    let graph = Arc::new(AtomicUsize::new(0));
    let content = Arc::new(AtomicUsize::new(0));
    let (g, c) = (graph.clone(), content.clone());
    let task = tokio::spawn(async move {
        let mut tasks = JoinSet::new();
        loop {
            tokio::select! {
                socket=listener.accept()=>{
                    let (mut socket,_)=socket.unwrap();let (origin,g,c)=(origin.clone(),g.clone(),c.clone());
                    tasks.spawn(async move {
                        let mut request=String::new();
                        while !request.contains("\r\n\r\n") {
                            let mut buf=[0;2048];let Ok(n)=socket.read(&mut buf).await else { return; };if n==0 { return; }
                            request.push_str(&String::from_utf8_lossy(&buf[..n]));assert!(request.len()<16384);
                        }
                        let lower=request.to_ascii_lowercase();
                        if lower.starts_with("get /download ") {
                            c.fetch_add(1,Ordering::SeqCst);assert!(!lower.contains("authorization:"));
                            let range=lower.lines().find_map(|line|line.strip_prefix("range: bytes=")).unwrap();
                            let (start,end)=range.split_once('-').unwrap();let (start,end):(u64,u64)=(start.parse().unwrap(),end.parse().unwrap());
                            assert!(end>=start && end<size);let mut count=end-start+1;
                            assert!(count<=64*1024*1024);
                            let head=format!("HTTP/1.1 206 Partial Content\r\nETag: W/\"weak-origin\"\r\nContent-Length: {count}\r\nContent-Range: bytes {start}-{end}/{size}\r\nConnection: close\r\n\r\n");
                            if socket.write_all(head.as_bytes()).await.is_err() { return; }
                            let chunk=[b'A';64*1024];
                            while count>0 { let n=count.min(chunk.len() as u64) as usize;if socket.write_all(&chunk[..n]).await.is_err() { return; }count-=n as u64; }
                        } else {
                            assert!(lower.starts_with("get /v1.0/drives/drive/items/file "));
                            g.fetch_add(1,Ordering::SeqCst);
                            let body=serde_json::json!({"id":"file","name":"synthetic","size":size,"file":{},"cTag":"v1","eTag":"meta","parentReference":{"driveId":"drive"},"@microsoft.graph.downloadUrl":format!("{origin}/download")}).to_string();
                            let reply=format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());let _=socket.write_all(reply.as_bytes()).await;
                        }
                    });
                }
                _=tasks.join_next(),if !tasks.is_empty()=>(),
            }
        }
    });
    Server {
        task,
        provider,
        graph,
        content,
    }
}
#[tokio::test]
async fn graph_gibibyte_through_shared_disk_cache_uses_forty_metadata_requests() {
    let temp = tempfile::tempdir().unwrap();
    let size = 1024 * 1024 * 1024;
    let server = server(size).await;
    let cache_path = temp.path().join("cache");
    let db = temp.path().join("db");
    let cache =
        crate::content::ContentCache::new(cache_path.clone(), db.clone(), 512 * 1024 * 1024)
            .unwrap();
    let scope = Scope {
        account: "account".into(),
        provider: "onedrive".into(),
        collection: "drive".into(),
    };
    let node = Node {
        id: "file".into(),
        name: "synthetic".into(),
        parent_id: None,
        kind: NodeKind::File,
        size,
        modified_unix: 0,
        etag: Some("meta".into()),
        content_version: Some("v1".into()),
        target: None,
    };
    let cancel = CancellationToken::new();
    let baseline_rss = rss_bytes();
    let mut peak_rss = baseline_rss;
    let start = std::time::Instant::now();
    let mut latencies = Vec::with_capacity(256);
    for block in 0..256 {
        let block_start = std::time::Instant::now();
        let bytes = cache
            .read(
                &server.provider,
                &scope,
                &node,
                block * BLOCK_SIZE as u64,
                BLOCK_SIZE,
                &cancel,
            )
            .await
            .unwrap();
        latencies.push(block_start.elapsed().as_secs_f64() * 1000.0);
        peak_rss = peak_rss.max(rss_bytes());
        assert_eq!(bytes.len(), BLOCK_SIZE as usize);
        assert!(bytes.iter().all(|b| *b == b'A'));
    }
    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
    let first_ms = latencies[0];
    latencies.sort_by(f64::total_cmp);
    assert!(
        peak_rss.saturating_sub(baseline_rss) < 256 * 1024 * 1024,
        "whole-file-sized process growth"
    );
    let counters = server.provider.read_counters();
    assert_eq!(server.graph.load(Ordering::SeqCst), 40);
    assert_eq!(server.content.load(Ordering::SeqCst), 20);
    assert_eq!(counters.graph_get_attempts, 40);
    assert_eq!(counters.content_get_attempts, 20);
    assert_eq!(counters.content_body_bytes, size);
    let stats = cache.window_stats();
    assert_eq!(stats.validated_windows, 18);
    assert_eq!(stats.staging_reserved_bytes, 0);
    assert_eq!(stats.staging_peak_bytes, 64 * 1024 * 1024);
    drop(cache);
    let cache = crate::content::ContentCache::new(cache_path, db, 512 * 1024 * 1024).unwrap();
    cache
        .read(
            &server.provider,
            &scope,
            &node,
            size - BLOCK_SIZE as u64,
            32,
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(server.graph.load(Ordering::SeqCst), 40);
    assert_eq!(server.content.load(Ordering::SeqCst), 20);
    eprintln!(
        "CIRROVE_GRAPH_WINDOWS {}",
        serde_json::json!({"graph_gets":40,"content_gets":20,"bytes":size,"staging":stats,"elapsed_ms":elapsed_ms,"first_block_ms":first_ms,"block_p50_ms":latencies[128],"block_p95_ms":latencies[243],"sampled_rss_baseline_bytes":baseline_rss,"sampled_rss_peak_bytes":peak_rss,"context":"synthetic loopback Graph, debug build, shared disk cache; no kernel mount or cloud account"})
    );
}

fn rss_bytes() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse::<u64>()
        .unwrap()
        * 1024
}
