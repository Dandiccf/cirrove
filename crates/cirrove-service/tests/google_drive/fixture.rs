use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

pub struct Server {
    pub google: Arc<GoogleDrive>,
    pub onedrive: Arc<cirrove_onedrive::OneDrive>,
    pub fail_page: Arc<AtomicBool>,
    pub fail_catchup: Arc<AtomicBool>,
    pub starts: Arc<AtomicUsize>,
    pub requests: Arc<AtomicUsize>,
    pub media: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
pub fn file(id: &str, version: u64) -> Value {
    json!({"id":id,"name":"same.txt","mimeType":"text/plain","parents":["root-id"],"size":"6","version":version.to_string(),"capabilities":{"canDownload":true}})
}
impl Server {
    pub async fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let google = Arc::new(
            GoogleDrive::synthetic_loopback(
                ACCOUNT.into(),
                "root-id".into(),
                &format!("{origin}/drive/v3/"),
            )
            .unwrap(),
        );
        let onedrive = Arc::new(
            cirrove_onedrive::OneDrive::synthetic_loopback(
                ACCOUNT.into(),
                &format!("{origin}/v1.0/"),
            )
            .unwrap()
            .without_read_sessions(),
        );
        let fail_page = Arc::new(AtomicBool::new(false));
        let fail_catchup = Arc::new(AtomicBool::new(false));
        let starts = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(AtomicUsize::new(0));
        let media = Arc::new(AtomicUsize::new(0));
        let (fail, catchup, start_count, request_count, media_count) = (
            fail_page.clone(),
            fail_catchup.clone(),
            starts.clone(),
            requests.clone(),
            media.clone(),
        );
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let mut bytes = Vec::new();
                loop {
                    let mut chunk = [0; 2048];
                    let n = socket.read(&mut chunk).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&chunk[..n]);
                    if bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                    assert!(bytes.len() < 65536);
                }
                let request = String::from_utf8(bytes).unwrap();
                assert!(
                    request.starts_with("GET "),
                    "read-only fixture received a mutation"
                );
                request_count.fetch_add(1, Ordering::SeqCst);
                let path = request.split_ascii_whitespace().nth(1).unwrap();
                let is_google = path.starts_with("/drive/v3/");
                let graph_file = json!({"id":"same-id","name":"same.txt","parentReference":{"driveId":"root-id","id":"root-id"},"file":{},"size":6,"eTag":"etag","cTag":"google-version:2","@microsoft.graph.downloadUrl":format!("{origin}/blob")});
                let mut status = 200;
                let mut headers = String::new();
                let body = if path == "/blob" || path.contains("alt=media") {
                    media_count.fetch_add(1, Ordering::SeqCst);
                    let data = if is_google { b"google" } else { b"onedrv" };
                    let range = request.lines().find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("range: bytes=")
                            .map(str::to_owned)
                    });
                    let (start, end) = range.as_deref().unwrap_or("0-5").split_once('-').unwrap();
                    let start = start.parse::<usize>().unwrap();
                    let end = end.parse::<usize>().unwrap().min(5);
                    status = 206;
                    headers = format!("Content-Range: bytes {start}-{end}/6\r\n");
                    data[start..=end].to_vec()
                } else {
                    let body = if path.starts_with("/v1.0/drives/root-id/root/delta") {
                        json!({"value":[graph_file.clone()],"@odata.deltaLink":format!("{origin}/v1.0/drives/root-id/root/delta?token=settled")})
                    } else if path.starts_with("/v1.0/drives/root-id/items/same-id") {
                        graph_file
                    } else if path.starts_with("/drive/v3/changes/startPageToken") {
                        start_count.fetch_add(1, Ordering::SeqCst);
                        json!({"startPageToken":"start"})
                    } else if path.starts_with("/drive/v3/changes?") {
                        if catchup.swap(false, Ordering::SeqCst) {
                            status = 500;
                            json!({"error":{}})
                        } else {
                            json!({"changes":[{"fileId":"same-id","file":file("same-id",2)}],"newStartPageToken":"settled"})
                        }
                    } else if path.starts_with("/drive/v3/files/root-id?")
                        || path.starts_with("/drive/v3/files/root?")
                    {
                        json!({"id":"root-id","name":"My Drive","mimeType":"application/vnd.google-apps.folder","version":"1"})
                    } else if path.starts_with("/drive/v3/files/same-id?") {
                        file("same-id", 2)
                    } else if path.starts_with("/drive/v3/files/other-id?") {
                        file("other-id", 1)
                    } else if path.starts_with("/drive/v3/files?") {
                        if path.contains("pageToken=page2") {
                            if fail.swap(false, Ordering::SeqCst) {
                                status = 500;
                                json!({"error":{}})
                            } else {
                                json!({"files":[file("other-id",1)]})
                            }
                        } else {
                            json!({"files":[file("same-id",1)],"nextPageToken":"page2"})
                        }
                    } else {
                        panic!("unexpected synthetic request path {path}");
                    };
                    serde_json::to_vec(&body).unwrap()
                };
                let reply = format!(
                    "HTTP/1.1 {status} Synthetic\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",
                    body.len()
                );
                if socket.write_all(reply.as_bytes()).await.is_ok() {
                    let _ = socket.write_all(&body).await;
                }
            }
        });
        Self {
            google,
            onedrive,
            fail_page,
            fail_catchup,
            starts,
            requests,
            media,
            task,
        }
    }
    pub fn offline(&self) {
        self.task.abort();
    }
}
