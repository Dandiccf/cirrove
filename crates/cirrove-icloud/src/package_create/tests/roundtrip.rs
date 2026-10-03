//! Actual HTTPS body transfer and independent package readback, without relaxing URL policy.
use super::*;
use base64::Engine as _;
use std::sync::Mutex as StdMutex;
use tokio_rustls::{
    TlsAcceptor,
    rustls::{
        ServerConfig,
        pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    },
};
const ORIGIN: &str = "https://fixture.icloud-content.com";
const LOGICAL_SIZE: u64 = 17;
fn archive(root: &str, changed: bool, explicit_root: bool) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    if explicit_root {
        writer
            .add_directory(format!("{root}/"), zip::write::SimpleFileOptions::default())
            .unwrap();
    }
    writer
        .start_file(
            format!("{root}/Document"),
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
    writer
        .write_all(if changed {
            b"different content"
        } else {
            b"synthetic content"
        })
        .unwrap();
    writer.finish().unwrap().into_inner()
}
#[derive(Default)]
struct Observed {
    registered: bool,
    calls: Vec<(String, String)>,
    uploaded: Vec<u8>,
}
struct Server {
    state: Arc<StdMutex<Observed>>,
    stop: CancellationToken,
    task: Option<tokio::task::JoinHandle<()>>,
    client: reqwest::Client,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.cancel();
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
impl Server {
    async fn start(source: Vec<u8>, remote: Vec<u8>, lose_registration: bool) -> Self {
        let decoder = base64::engine::general_purpose::STANDARD;
        let cert = decoder
            .decode(include_str!("../fixtures/server-cert.b64").trim())
            .unwrap();
        let key = decoder
            .decode(include_str!("../fixtures/server-key.b64").trim())
            .unwrap();
        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(cert.clone())],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
            )
            .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = reqwest::Client::builder()
            .no_proxy()
            .resolve("fixture.icloud-content.com", listener.local_addr().unwrap())
            .tls_certs_only([reqwest::Certificate::from_der(&cert).unwrap()])
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        let state = Arc::new(StdMutex::new(Observed::default()));
        let observed = state.clone();
        let stop = CancellationToken::new();
        let stopping = stop.clone();
        let task = tokio::spawn(async move {
            loop {
                let socket = tokio::select! {biased; _=stopping.cancelled()=>break, accepted=listener.accept()=>accepted.unwrap().0};
                tokio::time::timeout(Duration::from_secs(3),async {
                    let mut stream=acceptor.accept(socket).await.unwrap();let mut raw=Vec::new();
                    let (headers,body)=loop {
                        let mut buffer=[0;4096];let n=stream.read(&mut buffer).await.unwrap();assert!(n>0);raw.extend_from_slice(&buffer[..n]);assert!(raw.len()<128*1024);
                        let Some(end)=raw.windows(4).position(|w|w==b"\r\n\r\n") else {continue;};
                        let headers=std::str::from_utf8(&raw[..end]).unwrap().to_owned();
                        let size=headers.lines().find_map(|line|line.to_ascii_lowercase().strip_prefix("content-length: ").map(str::to_owned)).map(|n|n.parse::<usize>().unwrap()).unwrap_or(0);
                        if raw.len()>=end+4+size {break (headers,raw[end+4..end+4+size].to_vec());}
                    };
                    let mut line=headers.lines().next().unwrap().split_whitespace();let method=line.next().unwrap();let path=line.next().unwrap();
                    let reply={
                        let mut state=observed.lock().unwrap();state.calls.push((method.into(),path.into()));
                        match (method,path.split('?').next().unwrap()) {
                            ("POST","/retrieveItemDetailsInFolders")=>{
                                let request:serde_json::Value=serde_json::from_slice(&body).unwrap();assert_eq!(request[0]["drivewsid"],ROOT_ID);
                                let items=if state.registered {vec![json!({"drivewsid":"FILE::com.apple.CloudDocs::new-package","docwsid":"new-package","zone":"com.apple.CloudDocs","type":"FILE","name":"Target","extension":"pages","parentId":ROOT_ID,"etag":"native-v1","size":LOGICAL_SIZE})]} else {Vec::new()};
                                Some(json!([{"drivewsid":ROOT_ID,"type":"FOLDER","numberOfItems":items.len(),"items":items}]).to_string().into_bytes())
                            },
                            ("POST","/ws/com.apple.CloudDocs/upload/web")=>{
                                let request:serde_json::Value=serde_json::from_slice(&body).unwrap();assert_eq!(request["type"],"PACKAGE");assert_eq!(request["filename"],"Target.pages");assert_eq!(request["size"],source.len());
                                Some(json!([{"url":format!("{ORIGIN}/signed-upload"),"document_id":"new-package","owner_id":""}]).to_string().into_bytes())
                            },
                            ("POST","/signed-upload")=>{
                                assert_eq!(body,source);assert!(headers.to_ascii_lowercase().contains("content-type: application/zip"));state.uploaded=body;
                                Some(json!({"hexBrSyntheticChecksum":"aa","ckSectionAssets":[{"hexFileChecksum":"bb","hexReferenceChecksum":"cc","hexWrappingKey":"dd","receiptToken":"YQ==","size":source.len()}]}).to_string().into_bytes())
                            },
                            ("POST","/ws/com.apple.CloudDocs/update/documents")=>{
                                assert_eq!(state.uploaded,source);assert!(!state.registered);
                                let request:serde_json::Value=serde_json::from_slice(&body).unwrap();assert_eq!(request["command"],"add_package");assert_eq!(request["document_id"],"new-package");assert_eq!(request["path"]["path"],"Target.pages");assert_eq!(request["path"]["starting_document_id"],"root");assert_eq!(request["allow_conflict"],false);assert!(request.get("owner").is_none());
                                state.registered=true;
                                if lose_registration {None} else {Some(json!({"status":{"status_code":0},"results":[{"status":{"status_code":0},"document":{"document_id":"new-package","item_id":"item-native","etag":"native-v1","size":LOGICAL_SIZE,"name":"Target.pages"}}]}).to_string().into_bytes())}
                            },
                            ("GET","/ws/com.apple.CloudDocs/download/by_id")=>{
                                assert!(state.registered);assert!(path.contains("document_id=new-package"));Some(json!({"package_token":{"url":format!("{ORIGIN}/signed-download")}}).to_string().into_bytes())
                            },
                            ("GET","/signed-download")=>{assert!(state.registered);Some(remote.clone())},
                            _=>panic!("unexpected fixture route"),
                        }
                    };
                    if let Some(reply)=reply {
                        stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",reply.len()).as_bytes()).await.unwrap();stream.write_all(&reply).await.unwrap();stream.shutdown().await.unwrap();
                    }
                }).await.unwrap();
            }
        });
        Self {
            state,
            stop,
            task: Some(task),
            client,
        }
    }
    async fn finish(mut self) -> Vec<(String, String)> {
        self.stop.cancel();
        self.task.take().unwrap().await.unwrap();
        self.state.lock().unwrap().calls.clone()
    }
}
async fn arm(
    lose_registration: bool,
    changed_remote: bool,
    version: u32,
    remote_directories: bool,
) {
    let expected_conflict = changed_remote || (version == 1 && remote_directories);
    let (_dir, provider, mut request, saved) = fixture();
    let source = archive("Source.pages", false, false);
    let remote = archive("Target.pages", changed_remote, remote_directories);
    assert_ne!(source, remote);
    assert_ne!(source.len() as u64, LOGICAL_SIZE);
    let server = Server::start(source.clone(), remote, lose_registration).await;
    {
        let mut state = provider.session.lock().await;
        let Session::Ready(session) = &mut *state else {
            panic!("ready");
        };
        session.http = server.client.clone();
        session.drive_endpoint = Some(format!("{ORIGIN}/").parse().unwrap());
        session.docs_endpoint = session.drive_endpoint.clone();
    }
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(&source).unwrap();
    request.size = source.len() as u64;
    request.sha256 = hex::encode(Sha256::digest(&source));
    let cancel = CancellationToken::new();
    let semantic = crate::package_archive_semantic_identity_versioned(
        &file,
        &crate::PackageDownload {
            size: request.size,
            sha256: request.sha256.clone(),
        },
        "Source.pages",
        version,
        &cancel,
    )
    .unwrap();
    request.representation = UploadRepresentation::PackageArchive {
        expected_root: "Source.pages".into(),
        semantic: semantic.clone(),
    };
    let UploadStep::Allocate(allocation) = provider
        .begin_upload_for_operation(&saved.operation, &request, &cancel)
        .await
        .unwrap()
    else {
        panic!("allocate checkpoint");
    };
    assert!(allocation.expose_secret().len() <= MAX_CHECKPOINT);
    let UploadStep::Stream(body) = provider
        .allocate_upload_for_operation(&saved.operation, &request, &allocation, &cancel)
        .await
        .unwrap()
    else {
        panic!("body checkpoint");
    };
    assert!(body.expose_secret().len() <= MAX_CHECKPOINT);
    let UploadStep::Commit(registration) = provider
        .upload_stream_for_operation(&saved.operation, &request, &body, file, &cancel)
        .await
        .unwrap()
    else {
        panic!("registration checkpoint");
    };
    assert!(registration.expose_secret().len() <= MAX_CHECKPOINT);
    let result = provider
        .commit_upload_for_operation(&saved.operation, &request, &registration, &cancel)
        .await;
    let receipt = if expected_conflict {
        assert!(matches!(result, Err(UploadError::Conflict)));
        None
    } else if lose_registration {
        assert!(matches!(result, Err(UploadError::Uncertain)));
        let before = server.state.lock().unwrap().calls.len();
        // Restored state observes only; even a saved registration receipt is not
        // permission to send registration again after its reply was lost.
        let mut restored_session = ICloudReadSession::new().unwrap();
        restored_session.http = server.client.clone();
        restored_session.account_hash = Some("test-account".into());
        restored_session.drive_endpoint = Some(format!("{ORIGIN}/").parse().unwrap());
        restored_session.docs_endpoint = restored_session.drive_endpoint.clone();
        let restarted = ICloudPackageCreate {
            scope: provider.scope.clone(),
            parent: provider.parent.clone(),
            staging: provider.staging.clone(),
            staging_budget: provider.staging_budget.clone(),
            body_dispatch_probe: None,
            session: Mutex::new(Session::Ready(Box::new(restored_session))),
        };
        let Reconciliation::PackageCommitted(receipt) = restarted
            .reconcile_upload_for_operation(
                &saved.operation,
                &request,
                Some(&registration),
                &cancel,
            )
            .await
            .unwrap()
        else {
            panic!("verified recovery");
        };
        assert!(
            server.state.lock().unwrap().calls[before..]
                .iter()
                .all(|(method, path)| method == "GET" || path == "/retrieveItemDetailsInFolders")
        );
        Some(receipt)
    } else {
        let UploadStep::PackageComplete(receipt) = result.unwrap() else {
            panic!("semantic completion");
        };
        Some(receipt)
    };
    if let Some(receipt) = receipt {
        assert_eq!(receipt.remote.id, "FILE::com.apple.CloudDocs::new-package");
        assert_eq!(receipt.remote.parent_id.as_deref(), Some(ROOT_ID));
        assert_eq!(receipt.remote.name, "Target.pages");
        assert_eq!(receipt.remote.size, LOGICAL_SIZE);
        assert!(receipt.remote.package);
        assert_eq!(receipt.remote.kind, NodeKind::Folder);
        assert_eq!(receipt.remote.etag.as_deref(), Some("native-v1"));
        assert_eq!(
            receipt.remote.content_version, None,
            "package folder receipts must use the read provider's ETag namespace"
        );
        assert_eq!(
            receipt.remote.content_revision(),
            Some(("etag", "native-v1"))
        );
        assert_eq!(receipt.semantic, semantic);
    }
    #[cfg(feature = "write-probe")]
    {
        let mut source_file = tempfile::tempfile().unwrap();
        source_file.write_all(&source).unwrap();
        let (diagnostic, verified) = provider
            .diagnostic_inspect(
                &saved.operation,
                &request,
                &registration,
                source_file,
                "Source.pages".into(),
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(
            diagnostic["fence"],
            if expected_conflict {
                "archive-semantic-equality"
            } else {
                "verified"
            }
        );
        assert_eq!(
            diagnostic["archive_comparison"]["exact_file_paths_sizes_hashes_equal"],
            !changed_remote
        );
        assert_eq!(verified.is_some(), !expected_conflict);
        // The assertions below still require exactly one mutation of each kind;
        // this extra diagnostic can only add metadata/content read requests.
    }
    let calls = server.finish().await;
    for suffix in [
        "/ws/com.apple.CloudDocs/upload/web",
        "/signed-upload",
        "/ws/com.apple.CloudDocs/update/documents?errorBreakdown=true",
    ] {
        assert_eq!(calls.iter().filter(|(_, path)| path == suffix).count(), 1);
    }
    assert_eq!(
        calls
            .iter()
            .filter(|(_, path)| path == "/signed-download")
            .count(),
        if cfg!(feature = "write-probe") { 2 } else { 1 }
    );
}

#[tokio::test]
async fn exhausted_staging_budget_refuses_package_allocation_before_any_http() {
    let (dir, provider, mut request, mut saved) = fixture();
    let source = archive("Source.pages", false, false);
    request.size = source.len() as u64;
    request.sha256 = hex::encode(Sha256::digest(&source));
    saved.request = request.clone();
    let checkpoint = ICloudPackageCreate::encode(&saved).unwrap();
    let server = Server::start(source, archive("Target.pages", false, false), false).await;
    {
        let mut state = provider.session.lock().await;
        let Session::Ready(session) = &mut *state else {
            panic!("ready")
        };
        session.http = server.client.clone();
        session.drive_endpoint = Some(format!("{ORIGIN}/").parse().unwrap());
        session.docs_endpoint = session.drive_endpoint.clone();
    }
    let mut held = Vec::new();
    for _ in 0..4 {
        held.push(provider.staging_budget.create(dir.path()).await.unwrap());
    }
    assert!(
        matches!(
            provider
                .allocate_upload_for_operation(
                    &saved.operation,
                    &request,
                    &checkpoint,
                    &CancellationToken::new()
                )
                .await,
            Err(UploadError::Uncertain)
        ),
        "allocation must refuse exhausted staging before provider dispatch"
    );
    assert!(server.state.lock().unwrap().calls.is_empty());
    drop(held);
    assert!(matches!(
        provider
            .allocate_upload_for_operation(
                &saved.operation,
                &request,
                &checkpoint,
                &CancellationToken::new()
            )
            .await
            .unwrap(),
        UploadStep::Stream(_)
    ));
    let calls = server.finish().await;
    assert_eq!(
        calls
            .iter()
            .filter(|(_, path)| path.starts_with("/ws/com.apple.CloudDocs/upload/web"))
            .count(),
        1
    );
}
#[tokio::test]
async fn package_create_real_https_roundtrip_verifies_repacked_archive_and_logical_size() {
    tokio::time::timeout(Duration::from_secs(15), arm(false, false, 1, false))
        .await
        .unwrap();
}
#[tokio::test]
async fn package_create_lost_registration_reply_recovers_by_readback_without_replay() {
    tokio::time::timeout(Duration::from_secs(15), arm(true, false, 1, false))
        .await
        .unwrap();
}
#[tokio::test]
async fn package_create_successful_registration_with_wrong_remote_content_is_not_a_receipt() {
    tokio::time::timeout(Duration::from_secs(15), arm(false, true, 1, false))
        .await
        .unwrap();
}

#[tokio::test]
async fn package_create_v2_stored_proof_survives_added_directory_entries_and_lost_reply() {
    for lost in [false, true] {
        tokio::time::timeout(Duration::from_secs(15), arm(lost, false, 2, true))
            .await
            .unwrap();
    }
}
#[tokio::test]
async fn package_create_v2_still_refuses_changed_file_bytes() {
    tokio::time::timeout(Duration::from_secs(15), arm(false, true, 2, true))
        .await
        .unwrap();
}

#[tokio::test]
async fn package_create_retained_v1_directory_conflict_is_not_reinterpreted_as_v2() {
    tokio::time::timeout(Duration::from_secs(15), arm(false, false, 1, true))
        .await
        .unwrap();
}
