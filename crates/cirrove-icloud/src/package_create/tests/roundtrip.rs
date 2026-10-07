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
            if root.is_empty() {
                "Document".into()
            } else {
                format!("{root}/Document")
            },
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
// Test model inferred from the retained bec9 PACKAGE response, not a provider
// protocol guarantee. Parse only tiny synthetic archives, without extraction.
const MODEL_LIMIT: usize = 64 * 1024;
fn numbers_tree(root: Option<&str>) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (path, bytes) in [
        ("Index/Document.iwa", b"document-iwa".as_slice()),
        ("Index/Tables/Table.iwa", b"nested-table-iwa".as_slice()),
        (
            "Metadata/DocumentIdentifier",
            b"metadata-identifier".as_slice(),
        ),
        ("preview.jpg", b"full-preview".as_slice()),
        ("preview-micro.jpg", b"micro-preview".as_slice()),
        ("preview-web.jpg", b"web-preview".as_slice()),
    ] {
        let path = root.map_or_else(|| path.to_owned(), |root| format!("{root}/{path}"));
        zip.start_file(
            path,
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}
fn model_files(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    assert!(bytes.len() <= MODEL_LIMIT);
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    assert!(zip.len() <= 64);
    let mut files = Vec::new();
    let mut expanded = 0;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).unwrap();
        if entry.is_dir() {
            continue;
        }
        assert!(entry.size() <= MODEL_LIMIT as u64);
        let name = entry.name().to_owned();
        assert!(
            name.len() <= 1024
                && name.split('/').all(|part| !part.is_empty()
                    && part != "."
                    && part != ".."
                    && !part.contains('\\'))
        );
        let mut content = Vec::new();
        entry
            .by_ref()
            .take(MODEL_LIMIT as u64 + 1)
            .read_to_end(&mut content)
            .unwrap();
        assert_eq!(content.len() as u64, entry.size());
        expanded += content.len();
        assert!(expanded <= MODEL_LIMIT);
        files.push((name, content));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(files.windows(2).all(|pair| pair[0].0 != pair[1].0));
    files
}
fn imported_body_model(posted: &[u8], target: &str) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let mut names = std::collections::BTreeSet::new();
    for (name, content) in model_files(posted) {
        // Observed hypothesis: one leading component is a package wrapper;
        // root-level files have no retained inner path.
        let Some((_, relative)) = name.split_once('/') else {
            continue;
        };
        assert!(!relative.is_empty() && names.insert(relative.to_owned()));
        zip.start_file(
            format!("{target}/{relative}"),
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
        zip.write_all(&content).unwrap();
    }
    let bytes = zip.finish().unwrap().into_inner();
    assert!(bytes.len() <= MODEL_LIMIT);
    bytes
}

#[derive(Default)]
struct Observed {
    registered: bool,
    calls: Vec<(String, String)>,
    uploaded: Vec<u8>,
    allocated_size: Option<u64>,
    downloaded: Vec<u8>,
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
    async fn start(
        source: Vec<u8>,
        remote: Vec<u8>,
        lose_registration: bool,
        target: &str,
    ) -> Self {
        Self::start_response(source, remote, lose_registration, target, false).await
    }
    async fn start_import_model(source: Vec<u8>, target: &str) -> Self {
        Self::start_response(source, Vec::new(), false, target, true).await
    }
    async fn start_response(
        source: Vec<u8>,
        remote: Vec<u8>,
        lose_registration: bool,
        target: &str,
        import_from_uploaded: bool,
    ) -> Self {
        let target = target.to_owned();
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
                                let items=if state.registered {vec![json!({"drivewsid":"FILE::com.apple.CloudDocs::new-package","docwsid":"new-package","zone":"com.apple.CloudDocs","type":"FILE","name":"Target","extension":target.rsplit_once('.').unwrap().1,"parentId":ROOT_ID,"etag":"native-v1","size":LOGICAL_SIZE})]} else {Vec::new()};
                                Some(json!([{"drivewsid":ROOT_ID,"type":"FOLDER","numberOfItems":items.len(),"items":items}]).to_string().into_bytes())
                            },
                            ("POST","/ws/com.apple.CloudDocs/upload/web")=>{
                                let request:serde_json::Value=serde_json::from_slice(&body).unwrap();assert_eq!(request["type"],"PACKAGE");assert_eq!(request["filename"],target);let size=request["size"].as_u64().unwrap();if import_from_uploaded {assert!(size>0 && size<=MODEL_LIMIT as u64);}else {assert_eq!(request["size"],source.len());}state.allocated_size=Some(size);
                                Some(json!([{"url":format!("{ORIGIN}/signed-upload"),"document_id":"new-package","owner_id":""}]).to_string().into_bytes())
                            },
                            ("POST","/signed-upload")=>{
                                if !import_from_uploaded {assert_eq!(body,source);}assert_eq!(Some(body.len() as u64),state.allocated_size);assert!(headers.to_ascii_lowercase().contains("content-type: application/zip"));state.uploaded=body;
                                Some(json!({"hexBrSyntheticChecksum":"aa","ckSectionAssets":[{"hexFileChecksum":"bb","hexReferenceChecksum":"cc","hexWrappingKey":"dd","receiptToken":"YQ==","size":state.uploaded.len()}]}).to_string().into_bytes())
                            },
                            ("POST","/ws/com.apple.CloudDocs/update/documents")=>{
                                if !import_from_uploaded {assert_eq!(state.uploaded,source);}assert!(!state.registered);
                                let request:serde_json::Value=serde_json::from_slice(&body).unwrap();assert_eq!(request["command"],"add_package");assert_eq!(request["document_id"],"new-package");assert_eq!(request["path"]["path"],target);assert_eq!(request["path"]["starting_document_id"],"root");assert_eq!(request["allow_conflict"],false);assert!(request.get("owner").is_none());
                                state.registered=true;
                                if lose_registration {None} else {Some(json!({"status":{"status_code":0},"results":[{"status":{"status_code":0},"document":{"document_id":"new-package","item_id":"item-native","etag":"native-v1","size":LOGICAL_SIZE,"name":target}}]}).to_string().into_bytes())}
                            },
                            ("GET","/ws/com.apple.CloudDocs/download/by_id")=>{
                                assert!(state.registered);assert!(path.contains("document_id=new-package"));Some(json!({"package_token":{"url":format!("{ORIGIN}/signed-download")}}).to_string().into_bytes())
                            },
                            ("GET","/signed-download")=>{assert!(state.registered);let bytes=if import_from_uploaded {imported_body_model(&state.uploaded,&target)} else {remote.clone()};state.downloaded=bytes.clone();Some(bytes)},
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
    flat: bool,
) {
    arm_format(
        lose_registration,
        changed_remote,
        version,
        remote_directories,
        flat,
        false,
    )
    .await;
}
async fn arm_format(
    lose_registration: bool,
    changed_remote: bool,
    version: u32,
    remote_directories: bool,
    flat: bool,
    pages: bool,
) {
    let expected_conflict = changed_remote || (version == 1 && remote_directories);
    let (dir, provider, mut request, saved) = fixture();
    let (_fixture_root, _owner) = if pages {
        (dir.keep(), None)
    } else {
        (dir.path().to_owned(), Some(dir))
    };
    let source_root = if flat { None } else { Some("Source.pages") };
    let target = if flat && !pages {
        "Target.numbers"
    } else {
        "Target.pages"
    };
    let source = archive(source_root.unwrap_or(""), false, false);
    let remote = archive(target, changed_remote, remote_directories);
    if flat {
        let UploadIntent::Create { name, .. } = &mut request.intent else {
            panic!("create")
        };
        *name = target.into();
    }
    assert_ne!(source, remote);
    assert_ne!(source.len() as u64, LOGICAL_SIZE);
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(&source).unwrap();
    request.size = source.len() as u64;
    request.sha256 = hex::encode(Sha256::digest(&source));
    let cancel = CancellationToken::new();
    let receipt = crate::PackageDownload {
        size: request.size,
        sha256: request.sha256.clone(),
    };
    let semantic = if flat {
        crate::package_flat_archive_semantic_identity_v2(&file, &receipt, &cancel).unwrap()
    } else {
        crate::package_archive_semantic_identity_versioned(
            &file,
            &receipt,
            "Source.pages",
            version,
            &cancel,
        )
        .unwrap()
    };
    request.representation = if pages {
        UploadRepresentation::FlatPagesArchive {
            semantic: semantic.clone(),
        }
    } else if flat {
        UploadRepresentation::FlatNumbersArchive {
            semantic: semantic.clone(),
        }
    } else {
        UploadRepresentation::PackageArchive {
            expected_root: "Source.pages".into(),
            semantic: semantic.clone(),
        }
    };
    let expected_uploaded = if flat {
        let (wire, _) = provider
            .flat_wire(file.try_clone().unwrap(), &request, &cancel)
            .await
            .unwrap();
        let mut bytes = vec![0; wire.borrowed_file().metadata().unwrap().len() as usize];
        std::os::unix::fs::FileExt::read_exact_at(wire.borrowed_file(), &mut bytes, 0).unwrap();
        bytes
    } else {
        source.clone()
    };
    let server = Server::start(expected_uploaded, remote, lose_registration, target).await;
    {
        let mut state = provider.session.lock().await;
        let Session::Ready(session) = &mut *state else {
            panic!("ready");
        };
        session.http = server.client.clone();
        session.drive_endpoint = Some(format!("{ORIGIN}/").parse().unwrap());
        session.docs_endpoint = session.drive_endpoint.clone();
    }
    let begin = if flat {
        provider
            .begin_upload_from_payload_for_operation(
                &saved.operation,
                &request,
                file.try_clone().unwrap(),
                &cancel,
            )
            .await
    } else {
        provider
            .begin_upload_for_operation(&saved.operation, &request, &cancel)
            .await
    };
    let UploadStep::Allocate(allocation) = begin.unwrap() else {
        panic!("allocate checkpoint");
    };
    assert!(allocation.expose_secret().len() <= MAX_CHECKPOINT);
    if flat {
        let mut wrong = request.clone();
        wrong.representation = UploadRepresentation::PackageArchive {
            expected_root: "Source.numbers".into(),
            semantic: semantic.clone(),
        };
        let before = server.state.lock().unwrap().calls.len();
        assert!(matches!(
            provider
                .inspect_upload_for_operation(&saved.operation, &wrong, &allocation, &cancel)
                .await,
            Err(UploadError::CheckpointInvalid)
        ));
        let mut value: serde_json::Value =
            serde_json::from_str(allocation.expose_secret()).unwrap();
        value["request"] = serde_json::to_value(&wrong).unwrap();
        let tampered = SecretString::from(value.to_string());
        assert!(matches!(
            provider
                .inspect_upload_for_operation(&saved.operation, &request, &tampered, &cancel)
                .await,
            Err(UploadError::CheckpointInvalid)
        ));
        assert_eq!(server.state.lock().unwrap().calls.len(), before);
    }
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
        assert_eq!(receipt.remote.name, target);
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
                source_root.map(str::to_owned),
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
    let server = Server::start(
        source,
        archive("Target.pages", false, false),
        false,
        "Target.pages",
    )
    .await;
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
    tokio::time::timeout(Duration::from_secs(15), arm(false, false, 1, false, false))
        .await
        .unwrap();
}
#[tokio::test]
async fn package_create_lost_registration_reply_recovers_by_readback_without_replay() {
    tokio::time::timeout(Duration::from_secs(15), arm(true, false, 1, false, false))
        .await
        .unwrap();
}
#[tokio::test]
async fn package_create_successful_registration_with_wrong_remote_content_is_not_a_receipt() {
    tokio::time::timeout(Duration::from_secs(15), arm(false, true, 1, false, false))
        .await
        .unwrap();
}

#[tokio::test]
async fn package_create_v2_stored_proof_survives_added_directory_entries_and_lost_reply() {
    for lost in [false, true] {
        tokio::time::timeout(Duration::from_secs(15), arm(lost, false, 2, true, false))
            .await
            .unwrap();
    }
}
#[tokio::test]
async fn package_create_v2_still_refuses_changed_file_bytes() {
    tokio::time::timeout(Duration::from_secs(15), arm(false, true, 2, true, false))
        .await
        .unwrap();
}

#[tokio::test]
async fn package_create_retained_v1_directory_conflict_is_not_reinterpreted_as_v2() {
    tokio::time::timeout(Duration::from_secs(15), arm(false, false, 1, true, false))
        .await
        .unwrap();
}

#[tokio::test]
async fn package_create_flat_numbers_real_https_and_lost_reply_keep_layout_bound() {
    for lost in [false, true] {
        tokio::time::timeout(Duration::from_secs(15), arm(lost, false, 2, true, true))
            .await
            .unwrap();
    }
}
#[tokio::test]
async fn package_create_flat_numbers_wrong_remote_content_is_not_a_receipt() {
    tokio::time::timeout(Duration::from_secs(15), arm(false, true, 2, true, true))
        .await
        .unwrap();
}

#[test]
fn flat_numbers_import_model_calibration_preserves_only_genuine_wrapped_tree() {
    let source = numbers_tree(None);
    let wrapped = numbers_tree(Some("Wire.numbers"));
    let output = imported_body_model(&source, "Target.numbers");
    let actual = model_files(&output);
    assert_eq!(
        actual
            .iter()
            .map(|(path, _)| path.as_str())
            .collect::<Vec<_>>(),
        vec![
            "Target.numbers/Document.iwa",
            "Target.numbers/DocumentIdentifier",
            "Target.numbers/Tables/Table.iwa",
        ]
    );
    let preserved = imported_body_model(&wrapped, "Target.numbers");
    assert_eq!(
        model_files(&preserved),
        model_files(&numbers_tree(Some("Target.numbers")))
    );
    assert_eq!(model_files(&source).len(), 6);
}

#[tokio::test]
async fn package_create_flat_numbers_posted_tree_preserves_index_metadata_tables_and_previews() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let (dir, provider, mut request, saved) = fixture();
        let retained = dir.keep(); // Preserve all fixture state, including a failed arm.
        let source = numbers_tree(None);
        let source_path = retained.join("genuine-synthetic-flat.numbers");
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&source_path)
            .unwrap();
        file.write_all(&source).unwrap();
        file.sync_all().unwrap();
        std::fs::set_permissions(&source_path, std::fs::Permissions::from_mode(0o400)).unwrap();
        let source_stamp = file.metadata().unwrap();
        let cancel = CancellationToken::new();
        let raw = crate::PackageDownload {
            size: source.len() as u64,
            sha256: hex::encode(Sha256::digest(&source)),
        };
        let semantic =
            crate::package_flat_archive_semantic_identity_v2(&file, &raw, &cancel).unwrap();
        assert_eq!(semantic.files, 6);
        let UploadIntent::Create { name, .. } = &mut request.intent else {
            panic!("create fixture");
        };
        *name = "Target.numbers".into();
        request.size = raw.size;
        request.sha256 = raw.sha256.clone();
        request.representation = UploadRepresentation::FlatNumbersArchive {
            semantic: semantic.clone(),
        };
        request.validate().unwrap();
        let immutable_request = serde_json::to_value(&request).unwrap();
        let server = Server::start_import_model(source.clone(), "Target.numbers").await;
        {
            let mut state = provider.session.lock().await;
            let Session::Ready(session) = &mut *state else {
                panic!("ready fixture");
            };
            session.http = server.client.clone();
            session.drive_endpoint = Some(format!("{ORIGIN}/").parse().unwrap());
            session.docs_endpoint = session.drive_endpoint.clone();
        }
        let UploadStep::Allocate(allocation) = provider
            .begin_upload_from_payload_for_operation(
                &saved.operation,
                &request,
                file.try_clone().unwrap(),
                &cancel,
            )
            .await
            .unwrap()
        else {
            panic!("allocation checkpoint");
        };
        let UploadStep::Stream(body) = provider
            .allocate_upload_for_operation(&saved.operation, &request, &allocation, &cancel)
            .await
            .unwrap()
        else {
            panic!("body checkpoint");
        };
        let UploadStep::Commit(registration) = provider
            .upload_stream_for_operation(&saved.operation, &request, &body, file, &cancel)
            .await
            .unwrap()
        else {
            panic!("registration checkpoint");
        };
        let result = provider
            .commit_upload_for_operation(&saved.operation, &request, &registration, &cancel)
            .await;
        let downloaded = server.state.lock().unwrap().downloaded.clone();
        let calls = server.finish().await;
        // Preservation and actual transport/frontier facts precede the desired
        // success assertion. The current baseline reaches Err(Conflict) here.
        assert_eq!(std::fs::read(&source_path).unwrap(), source);
        let final_stamp = std::fs::metadata(&source_path).unwrap();
        assert_eq!(
            (
                source_stamp.dev(),
                source_stamp.ino(),
                source_stamp.len(),
                source_stamp.mode(),
                source_stamp.mtime(),
                source_stamp.mtime_nsec()
            ),
            (
                final_stamp.dev(),
                final_stamp.ino(),
                final_stamp.len(),
                final_stamp.mode(),
                final_stamp.mtime(),
                final_stamp.mtime_nsec()
            )
        );
        assert_eq!(serde_json::to_value(&request).unwrap(), immutable_request);
        assert_eq!(
            hex::encode(Sha256::digest(std::fs::read(&source_path).unwrap())),
            raw.sha256
        );
        for route in [
            "/ws/com.apple.CloudDocs/upload/web",
            "/signed-upload",
            "/ws/com.apple.CloudDocs/update/documents?errorBreakdown=true",
            "/signed-download",
        ] {
            assert_eq!(calls.iter().filter(|(_, path)| path == route).count(), 1);
        }
        assert!(!downloaded.is_empty());
        let receipt = match result {
            Ok(UploadStep::PackageComplete(receipt)) => receipt,
            Err(UploadError::Conflict) => panic!(
                "desired preservation endpoint: received Conflict instead of PackageComplete"
            ),
            _ => panic!("unexpected result before desired package preservation endpoint"),
        };
        assert_eq!(receipt.semantic, semantic);
        assert_eq!(receipt.remote.name, "Target.numbers");
        assert_eq!(receipt.remote.id, "FILE::com.apple.CloudDocs::new-package");
        assert_eq!(
            model_files(&downloaded),
            model_files(&numbers_tree(Some("Target.numbers")))
        );
    })
    .await
    .unwrap();
}

// Actual backend callbacks with the existing HTTPS fixture. These tests add
// corruption coverage; they are not claimed as pre-adapter behavioral REDs.
struct WireControl {
    root: PathBuf,
    provider: ICloudPackageCreate,
    request: UploadRequest,
    operation: String,
    source_path: PathBuf,
    source: Vec<u8>,
    server: Server,
    allocation: SecretString,
    cancel: CancellationToken,
}
impl WireControl {
    fn file(&self) -> File {
        File::open(&self.source_path).unwrap()
    }
    fn checkpoint(&self) -> serde_json::Value {
        serde_json::from_str(self.allocation.expose_secret()).unwrap()
    }
    fn preserve(&self) {
        assert_eq!(std::fs::read(&self.source_path).unwrap(), self.source);
        let metadata = std::fs::metadata(&self.source_path).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, 0o400);
        assert_eq!(metadata.len(), self.request.size);
        assert_eq!(
            hex::encode(Sha256::digest(&self.source)),
            self.request.sha256
        );
    }
}
async fn wire_control() -> WireControl {
    wire_control_format(false).await
}
async fn wire_control_format(pages: bool) -> WireControl {
    let (dir, provider, mut request, saved) = fixture();
    let root = dir.keep();
    let source = numbers_tree(None);
    let target = if pages {
        "Target.pages"
    } else {
        "Target.numbers"
    };
    let source_path = root.join(if pages {
        "checkpoint-source.pages"
    } else {
        "checkpoint-source.numbers"
    });
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&source_path)
        .unwrap();
    file.write_all(&source).unwrap();
    file.sync_all().unwrap();
    std::fs::set_permissions(&source_path, std::fs::Permissions::from_mode(0o400)).unwrap();
    let cancel = CancellationToken::new();
    request.intent = UploadIntent::Create {
        parent: ROOT_ID.into(),
        name: target.into(),
    };
    request.size = source.len() as u64;
    request.sha256 = hex::encode(Sha256::digest(&source));
    let raw = crate::PackageDownload {
        size: request.size,
        sha256: request.sha256.clone(),
    };
    let semantic = crate::package_flat_archive_semantic_identity_v2(&file, &raw, &cancel).unwrap();
    request.representation = if pages {
        UploadRepresentation::FlatPagesArchive { semantic }
    } else {
        UploadRepresentation::FlatNumbersArchive { semantic }
    };
    let server = Server::start_import_model(source.clone(), target).await;
    {
        let mut state = provider.session.lock().await;
        let Session::Ready(session) = &mut *state else {
            panic!("ready synthetic session");
        };
        session.http = server.client.clone();
        session.account_hash = Some(crate::account_hash("fixture@example.test").unwrap());
        session.drive_endpoint = Some(format!("{ORIGIN}/").parse().unwrap());
        session.docs_endpoint = session.drive_endpoint.clone();
    }
    let UploadStep::Allocate(allocation) = provider
        .begin_upload_from_payload_for_operation(&saved.operation, &request, file, &cancel)
        .await
        .unwrap()
    else {
        panic!("prepared allocation");
    };
    let parsed: serde_json::Value = serde_json::from_str(allocation.expose_secret()).unwrap();
    assert_eq!(parsed["version"], 2);
    assert!(parsed["wire"].is_object());
    WireControl {
        root,
        provider,
        request,
        operation: saved.operation,
        source_path,
        source,
        server,
        allocation,
        cancel,
    }
}
fn wire_checkpoint(value: &serde_json::Value) -> SecretString {
    SecretString::from(serde_json::to_string(value).unwrap())
}

#[tokio::test]
async fn package_create_flat_wire_checkpoint_structural_corruption_refuses_before_http() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let control = wire_control().await;
        let before = control.server.state.lock().unwrap().calls.len();
        assert!(matches!(
            control
                .provider
                .inspect_upload_for_operation(
                    &control.operation,
                    &control.request,
                    &control.allocation,
                    &control.cancel
                )
                .await,
            Err(UploadError::Uncertain)
        )); // Valid unallocated checkpoint, no guess of success.
        assert_eq!(control.server.state.lock().unwrap().calls.len(), before);
        for arm in 0..9 {
            let mut value = control.checkpoint();
            match arm {
                0 => {
                    value.as_object_mut().unwrap().remove("wire");
                }
                1 => value["wire"] = serde_json::Value::Null,
                2 => value["wire"]["version"] = 99.into(),
                3 => value["wire"]["expected_root"] = "Foreign.numbers".into(),
                4 => value["wire"]["sha256"] = "A".repeat(64).into(),
                5 => value["wire"]["sha256"] = "a".repeat(63).into(),
                6 => value["wire"]["size"] = 0.into(),
                7 => {
                    value["wire"]["size"] =
                        (crate::write_staging::WRITE_STAGING_FILE_LIMIT + 1).into()
                }
                _ => value["version"] = 1.into(),
            }
            let checkpoint = wire_checkpoint(&value);
            assert!(matches!(
                control
                    .provider
                    .inspect_upload_for_operation(
                        &control.operation,
                        &control.request,
                        &checkpoint,
                        &control.cancel
                    )
                    .await,
                Err(UploadError::CheckpointInvalid)
            ));
            assert!(matches!(
                control
                    .provider
                    .allocate_upload_for_operation(
                        &control.operation,
                        &control.request,
                        &checkpoint,
                        &control.cancel
                    )
                    .await,
                Err(UploadError::CheckpointInvalid)
            ));
            assert_eq!(control.server.state.lock().unwrap().calls.len(), before);
            assert_eq!(
                checkpoint.expose_secret(),
                wire_checkpoint(&value).expose_secret()
            );
            control.preserve();
        }
        let calls = control.server.finish().await;
        assert_eq!(calls.len(), before);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn package_create_flat_wire_checkpoint_valid_shape_tamper_refuses_before_body_http() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let control = wire_control().await;
        let UploadStep::Stream(body) = control
            .provider
            .allocate_upload_for_operation(
                &control.operation,
                &control.request,
                &control.allocation,
                &control.cancel,
            )
            .await
            .unwrap()
        else {
            panic!("genuine allocated body checkpoint");
        };
        let before = control.server.state.lock().unwrap().calls.len();
        for arm in 0..2 {
            let mut value: serde_json::Value = serde_json::from_str(body.expose_secret()).unwrap();
            if arm == 0 {
                value["wire"]["sha256"] = "0".repeat(64).into();
            } else {
                let size = value["wire"]["size"].as_u64().unwrap();
                value["wire"]["size"] = (size + 1).into();
            }
            let checkpoint = wire_checkpoint(&value);
            // Both receipts are syntactically valid. Actual stream rederivation,
            // not a pure field validator, must refuse them before body dispatch.
            assert!(
                control
                    .provider
                    .decode(&control.operation, &control.request, &checkpoint)
                    .is_ok()
            );
            assert!(matches!(
                control
                    .provider
                    .upload_stream_for_operation(
                        &control.operation,
                        &control.request,
                        &checkpoint,
                        control.file(),
                        &control.cancel
                    )
                    .await,
                Err(UploadError::CheckpointInvalid)
            ));
            assert_eq!(control.server.state.lock().unwrap().calls.len(), before);
            control.preserve();
        }
        assert!(matches!(
            control
                .provider
                .upload_stream_for_operation(
                    &control.operation,
                    &control.request,
                    &body,
                    control.file(),
                    &control.cancel
                )
                .await
                .unwrap(),
            UploadStep::Commit(_)
        ));
        control.preserve();
        let calls = control.server.finish().await;
        assert_eq!(
            calls
                .iter()
                .filter(|(_, path)| path == "/signed-upload")
                .count(),
            1
        );
        assert_eq!(
            calls
                .iter()
                .filter(|(_, path)| path.starts_with("/ws/com.apple.CloudDocs/upload/web"))
                .count(),
            1
        );
        // No assertion that a forged-but-valid receipt refuses allocation HTTP:
        // this test starts from the genuine already allocated body checkpoint.
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn package_create_flat_wire_checkpoint_legacy_inspects_without_mutating_and_wrapped_refuses()
{
    tokio::time::timeout(Duration::from_secs(15), async {
        let control = wire_control().await;
        let UploadStep::Stream(body) = control
            .provider
            .allocate_upload_for_operation(
                &control.operation,
                &control.request,
                &control.allocation,
                &control.cancel,
            )
            .await
            .unwrap()
        else {
            panic!("body checkpoint");
        };
        let UploadStep::Commit(registration) = control
            .provider
            .upload_stream_for_operation(
                &control.operation,
                &control.request,
                &body,
                control.file(),
                &control.cancel,
            )
            .await
            .unwrap()
        else {
            panic!("registration checkpoint");
        };
        assert!(matches!(
            control
                .provider
                .commit_upload_for_operation(
                    &control.operation,
                    &control.request,
                    &registration,
                    &control.cancel
                )
                .await
                .unwrap(),
            UploadStep::PackageComplete(_)
        ));
        let mut legacy: serde_json::Value =
            serde_json::from_str(registration.expose_secret()).unwrap();
        legacy["version"] = 1.into();
        legacy.as_object_mut().unwrap().remove("wire");
        let legacy_registration = wire_checkpoint(&legacy);
        // Constructor only validates captured scope/request/parent/checkpoint;
        // no sealing key, credentials, or session body is loaded.
        let restored = ICloudPackageCreate::restore_from_sealed_checkpoint(
            control.request.scope.clone(),
            "fixture@example.test".into(),
            Uuid::new_v4().to_string(),
            &control.root,
            &control.root,
            &control.operation,
            &control.request,
            &legacy_registration,
        )
        .unwrap();
        // Fresh synthetic read session replaces the sealed source for this test.
        // No original in-memory session/vault is inherited or consulted.
        let mut session = ICloudReadSession::new().unwrap();
        session.http = control.server.client.clone();
        session.account_hash = Some(crate::account_hash("fixture@example.test").unwrap());
        session.drive_endpoint = Some(format!("{ORIGIN}/").parse().unwrap());
        session.docs_endpoint = session.drive_endpoint.clone();
        *restored.session.lock().await = Session::Ready(Box::new(session));
        let before = control.server.state.lock().unwrap().calls.len();
        assert!(matches!(
            restored
                .inspect_upload_for_operation(
                    &control.operation,
                    &control.request,
                    &legacy_registration,
                    &control.cancel
                )
                .await
                .unwrap(),
            UploadStep::PackageComplete(_)
        ));
        let after = control.server.state.lock().unwrap().calls.len();
        assert!(after > before);
        assert!(
            control.server.state.lock().unwrap().calls[before..]
                .iter()
                .all(|(method, path)| method == "GET" || path == "/retrieveItemDetailsInFolders")
        );
        for phase in 0..3 {
            let original = match phase {
                0 => &control.allocation,
                1 => &body,
                _ => &registration,
            };
            let mut value: serde_json::Value =
                serde_json::from_str(original.expose_secret()).unwrap();
            value["version"] = 1.into();
            value.as_object_mut().unwrap().remove("wire");
            let checkpoint = wire_checkpoint(&value);
            assert!(
                restored
                    .decode(&control.operation, &control.request, &checkpoint)
                    .is_ok()
            );
            let result = match phase {
                0 => {
                    restored
                        .allocate_upload_for_operation(
                            &control.operation,
                            &control.request,
                            &checkpoint,
                            &control.cancel,
                        )
                        .await
                }
                1 => {
                    restored
                        .upload_stream_for_operation(
                            &control.operation,
                            &control.request,
                            &checkpoint,
                            control.file(),
                            &control.cancel,
                        )
                        .await
                }
                _ => {
                    restored
                        .commit_upload_for_operation(
                            &control.operation,
                            &control.request,
                            &checkpoint,
                            &control.cancel,
                        )
                        .await
                }
            };
            assert!(matches!(result, Err(UploadError::CheckpointInvalid)));
            assert_eq!(control.server.state.lock().unwrap().calls.len(), after);
            control.preserve();
        }
        let UploadRepresentation::FlatNumbersArchive { semantic } = &control.request.representation
        else {
            panic!("flat fixture");
        };
        let mut wrapped = control.request.clone();
        wrapped.representation = UploadRepresentation::PackageArchive {
            expected_root: "Source.numbers".into(),
            semantic: semantic.clone(),
        };
        for arm in 0..3 {
            let mut value = control.checkpoint();
            value["request"] = serde_json::to_value(&wrapped).unwrap();
            if arm == 1 {
                value["version"] = 1.into();
            }
            if arm == 2 {
                value.as_object_mut().unwrap().remove("wire");
            }
            assert!(matches!(
                restored
                    .inspect_upload_for_operation(
                        &control.operation,
                        &wrapped,
                        &wire_checkpoint(&value),
                        &control.cancel
                    )
                    .await,
                Err(UploadError::CheckpointInvalid)
            ));
            assert_eq!(control.server.state.lock().unwrap().calls.len(), after);
        }
        control.preserve();
        let calls = control.server.finish().await;
        for route in [
            "/ws/com.apple.CloudDocs/upload/web",
            "/signed-upload",
            "/ws/com.apple.CloudDocs/update/documents?errorBreakdown=true",
        ] {
            assert_eq!(calls.iter().filter(|(_, path)| path == route).count(), 1);
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn package_create_flat_pages_real_https_and_lost_reply_keep_layout_bound() {
    for lost in [false, true] {
        tokio::time::timeout(
            Duration::from_secs(15),
            arm_format(lost, false, 2, true, true, true),
        )
        .await
        .unwrap();
    }
}
#[tokio::test]
async fn package_create_flat_pages_wrong_remote_content_is_not_a_receipt() {
    tokio::time::timeout(
        Duration::from_secs(15),
        arm_format(false, true, 2, true, true, true),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn package_create_flat_pages_checkpoint_structural_corruption_refuses_before_http() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let control = wire_control_format(true).await;
        let before = control.server.state.lock().unwrap().calls.len();
        assert!(matches!(
            control
                .provider
                .inspect_upload_for_operation(
                    &control.operation,
                    &control.request,
                    &control.allocation,
                    &control.cancel
                )
                .await,
            Err(UploadError::Uncertain)
        )); // Valid unallocated checkpoint, no guess of success.
        assert_eq!(control.server.state.lock().unwrap().calls.len(), before);
        for arm in 0..10 {
            let mut value = control.checkpoint();
            match arm {
                0 => {
                    value.as_object_mut().unwrap().remove("wire");
                }
                1 => value["wire"] = serde_json::Value::Null,
                2 => value["wire"]["version"] = 99.into(),
                3 => value["wire"]["expected_root"] = "Foreign.pages".into(),
                4 => value["wire"]["sha256"] = "A".repeat(64).into(),
                5 => value["wire"]["sha256"] = "a".repeat(63).into(),
                6 => value["wire"]["size"] = 0.into(),
                7 => {
                    value["wire"]["size"] =
                        (crate::write_staging::WRITE_STAGING_FILE_LIMIT + 1).into()
                }
                8 => value["version"] = 1.into(),
                _ => {
                    value["version"] = 1.into();
                    value.as_object_mut().unwrap().remove("wire");
                }
            }
            let checkpoint = wire_checkpoint(&value);
            assert!(matches!(
                control
                    .provider
                    .inspect_upload_for_operation(
                        &control.operation,
                        &control.request,
                        &checkpoint,
                        &control.cancel
                    )
                    .await,
                Err(UploadError::CheckpointInvalid)
            ));
            assert!(matches!(
                control
                    .provider
                    .allocate_upload_for_operation(
                        &control.operation,
                        &control.request,
                        &checkpoint,
                        &control.cancel
                    )
                    .await,
                Err(UploadError::CheckpointInvalid)
            ));
            assert_eq!(control.server.state.lock().unwrap().calls.len(), before);
            assert_eq!(
                checkpoint.expose_secret(),
                wire_checkpoint(&value).expose_secret()
            );
            control.preserve();
        }
        let calls = control.server.finish().await;
        assert_eq!(calls.len(), before);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn package_create_flat_pages_checkpoint_rederived_receipt_refuses_before_body_http() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let control = wire_control_format(true).await;
        let UploadStep::Stream(body) = control
            .provider
            .allocate_upload_for_operation(
                &control.operation,
                &control.request,
                &control.allocation,
                &control.cancel,
            )
            .await
            .unwrap()
        else {
            panic!("genuine allocated body checkpoint");
        };
        let before = control.server.state.lock().unwrap().calls.len();
        for arm in 0..2 {
            let mut value: serde_json::Value = serde_json::from_str(body.expose_secret()).unwrap();
            if arm == 0 {
                value["wire"]["sha256"] = "0".repeat(64).into();
            } else {
                let size = value["wire"]["size"].as_u64().unwrap();
                value["wire"]["size"] = (size + 1).into();
            }
            let checkpoint = wire_checkpoint(&value);
            // Both receipts are syntactically valid. Actual stream rederivation,
            // not a pure field validator, must refuse them before body dispatch.
            assert!(
                control
                    .provider
                    .decode(&control.operation, &control.request, &checkpoint)
                    .is_ok()
            );
            assert!(matches!(
                control
                    .provider
                    .upload_stream_for_operation(
                        &control.operation,
                        &control.request,
                        &checkpoint,
                        control.file(),
                        &control.cancel
                    )
                    .await,
                Err(UploadError::CheckpointInvalid)
            ));
            assert_eq!(control.server.state.lock().unwrap().calls.len(), before);
            control.preserve();
        }
        assert!(matches!(
            control
                .provider
                .upload_stream_for_operation(
                    &control.operation,
                    &control.request,
                    &body,
                    control.file(),
                    &control.cancel
                )
                .await
                .unwrap(),
            UploadStep::Commit(_)
        ));
        control.preserve();
        let calls = control.server.finish().await;
        assert_eq!(
            calls
                .iter()
                .filter(|(_, path)| path == "/signed-upload")
                .count(),
            1
        );
        assert_eq!(
            calls
                .iter()
                .filter(|(_, path)| path.starts_with("/ws/com.apple.CloudDocs/upload/web"))
                .count(),
            1
        );
        // No assertion that a forged-but-valid receipt refuses allocation HTTP:
        // this test starts from the genuine already allocated body checkpoint.
    })
    .await
    .unwrap();
}
