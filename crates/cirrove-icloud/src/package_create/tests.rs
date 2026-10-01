#![allow(clippy::unwrap_used)]
use super::*;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
fn fixture() -> (
    tempfile::TempDir,
    ICloudPackageCreate,
    UploadRequest,
    Checkpoint,
) {
    let dir = tempfile::tempdir().unwrap();
    let scope = Scope {
        account: Uuid::new_v4().to_string(),
        provider: "icloud".into(),
        collection: "drive".into(),
    };
    let parent = Node {
        id: ROOT_ID.into(),
        parent_id: None,
        name: "iCloud Drive".into(),
        kind: NodeKind::Folder,
        size: 0,
        modified_unix: 0,
        etag: None,
        content_version: None,
        target: None,
        package: false,
    };
    let mut session = ICloudReadSession::new().unwrap();
    session.account_hash = Some("test-account".into());
    let provider = ICloudPackageCreate {
        scope: scope.clone(),
        parent: parent.clone(),
        staging: dir.path().into(),
        body_dispatch_probe: None,
        session: Mutex::new(Session::Ready(Box::new(session))),
    };
    let request = UploadRequest {
        scope,
        intent: UploadIntent::Create {
            parent: ROOT_ID.into(),
            name: "Target.pages".into(),
        },
        size: 3,
        sha256: hex::encode(Sha256::digest(b"zip")),
        representation: UploadRepresentation::PackageArchive {
            expected_root: "Source.pages".into(),
            semantic: cirrove_core::upload::PackageSemanticIdentity {
                version: 1,
                sha256: "a".repeat(64),
                entries: 1,
                files: 1,
                expanded_bytes: 3,
            },
        },
    };
    let saved = Checkpoint {
        version: 1,
        operation: Uuid::new_v4().to_string(),
        request: request.clone(),
        account_hash: "test-account".into(),
        parent,
        phase: Phase::AllocationArmed,
        slot: None,
        registration: None,
    };
    (dir, provider, request, saved)
}
fn slot() -> wire::Slot {
    serde_json::from_value(json!({"url":"https://fixture.icloud-content.com/content", "document_id":"new-package", "owner_id":""})).unwrap()
}
#[tokio::test]
async fn armed_unknown_allocation_never_restarts_or_claims_absence() {
    let (_dir, provider, request, saved) = fixture();
    let checkpoint = ICloudPackageCreate::encode(&saved).unwrap();
    let cancel = CancellationToken::new();
    assert!(matches!(
        provider
            .inspect_upload_for_operation(&saved.operation, &request, &checkpoint, &cancel)
            .await,
        Err(UploadError::Uncertain)
    ));
    assert!(matches!(
        provider
            .reconcile_upload_for_operation(&saved.operation, &request, Some(&checkpoint), &cancel)
            .await,
        Err(UploadError::Uncertain)
    ));
    assert!(matches!(
        provider
            .reconcile_upload_for_operation(&saved.operation, &request, None, &cancel)
            .await,
        Err(UploadError::Uncertain)
    ));
}
#[test]
fn checkpoint_binding_and_total_bound_are_enforced() {
    let (_dir, provider, request, mut saved) = fixture();
    let checkpoint = ICloudPackageCreate::encode(&saved).unwrap();
    assert!(
        provider
            .decode(&saved.operation, &request, &checkpoint)
            .is_ok()
    );
    assert!(
        provider
            .decode(&Uuid::new_v4().to_string(), &request, &checkpoint)
            .is_err()
    );
    let mut changed = request.clone();
    changed.scope.account = Uuid::new_v4().to_string();
    assert!(
        provider
            .decode(&saved.operation, &changed, &checkpoint)
            .is_err()
    );
    changed = request.clone();
    let UploadRepresentation::PackageArchive { expected_root, .. } = &mut changed.representation
    else {
        panic!("package");
    };
    *expected_root = "Other.pages".into();
    assert!(
        provider
            .decode(&saved.operation, &changed, &checkpoint)
            .is_err()
    );
    saved.account_hash = "x".repeat(MAX_CHECKPOINT);
    assert!(ICloudPackageCreate::encode(&saved).is_err());
    assert!(
        provider
            .decode(
                &saved.operation,
                &request,
                &SecretString::from(" ".repeat(MAX_CHECKPOINT + 1))
            )
            .is_err()
    );
}
#[tokio::test]
async fn packages_cannot_replace_or_use_unbound_entry_points() {
    let (_dir, provider, mut request, saved) = fixture();
    let token = CancellationToken::new();
    assert!(matches!(
        provider.begin_upload(&request, &token).await,
        Err(UploadError::Unsupported(_))
    ));
    request.intent = UploadIntent::Replace {
        item: "FILE::com.apple.CloudDocs::old".into(),
        expected_etag: "v1".into(),
    };
    assert!(!provider.begin_is_mutation_free_until_checkpoint(&request));
    assert!(matches!(
        provider
            .begin_upload_for_operation(&saved.operation, &request, &token)
            .await,
        Err(UploadError::Unsupported(_))
    ));
}
#[tokio::test]
async fn cancelled_allocation_stops_before_any_request() {
    let (_dir, provider, request, saved) = fixture();
    let token = CancellationToken::new();
    token.cancel();
    let checkpoint = ICloudPackageCreate::encode(&saved).unwrap();
    assert!(matches!(
        provider
            .allocate_upload_for_operation(&saved.operation, &request, &checkpoint, &token)
            .await,
        Err(UploadError::Uncertain)
    ));
}
#[tokio::test]
async fn malformed_or_semantically_different_archive_never_transmits_body() {
    for malformed in [true, false] {
        let (_dir, provider, mut request, mut saved) = fixture();
        let mut source = tempfile::tempfile().unwrap();
        let bytes = if malformed {
            b"zip".to_vec()
        } else {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
            zip.start_file(
                "Source.pages/Document",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
            zip.write_all(b"synthetic native content").unwrap();
            zip.finish().unwrap().into_inner()
        };
        source.write_all(&bytes).unwrap();
        request.size = bytes.len() as u64;
        request.sha256 = hex::encode(Sha256::digest(&bytes));
        saved.request = request.clone();
        saved.phase = Phase::BodyArmed;
        saved.slot = Some(slot());
        // Poison the session binding. Reaching session/body handling would return
        // CheckpointInvalid; archive validation must fail earlier instead.
        saved.account_hash = "other-account".into();
        let checkpoint = ICloudPackageCreate::encode(&saved).unwrap();
        let result = provider
            .upload_stream_for_operation(
                &saved.operation,
                &request,
                &checkpoint,
                source,
                &CancellationToken::new(),
            )
            .await;
        assert!(matches!(
            result,
            Err(UploadError::Invalid) | Err(UploadError::Provider(ProviderError::Protocol(_)))
        ));
        // No continuation authorizes another body; a restart can only read back.
        let unchanged = provider
            .decode(&saved.operation, &request, &checkpoint)
            .unwrap();
        assert!(unchanged.phase == Phase::BodyArmed && unchanged.registration.is_none());
    }
}
#[tokio::test]
async fn valid_archive_snapshot_checks_exact_root_and_semantics() {
    let (_dir, provider, mut request, _) = fixture();
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    zip.start_file(
        "Source.pages/Document",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    zip.write_all(b"content").unwrap();
    let bytes = zip.finish().unwrap().into_inner();
    let mut source = tempfile::tempfile().unwrap();
    source.write_all(&bytes).unwrap();
    request.size = bytes.len() as u64;
    request.sha256 = hex::encode(Sha256::digest(&bytes));
    let receipt = crate::PackageDownload {
        size: request.size,
        sha256: request.sha256.clone(),
    };
    let cancel = CancellationToken::new();
    let semantic =
        crate::package_archive_semantic_identity(&source, &receipt, "Source.pages", &cancel)
            .unwrap();
    request.representation = UploadRepresentation::PackageArchive {
        expected_root: "Source.pages".into(),
        semantic,
    };
    let mut snapshot = provider.payload(source, &request, &cancel).await.unwrap();
    let mut actual = Vec::new();
    snapshot.read_to_end(&mut actual).unwrap();
    assert_eq!(actual, bytes);
}
#[tokio::test]
async fn allocation_response_loss_retains_exact_armed_checkpoint_and_one_request() {
    let (_dir, provider, request, saved) = fixture();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    {
        let mut guard = provider.session.lock().await;
        let Session::Ready(session) = &mut *guard else {
            panic!("ready");
        };
        session.drive_endpoint = Some(format!("http://{address}/").parse().unwrap());
        session.docs_endpoint = session.drive_endpoint.clone();
    }
    let task = tokio::spawn(async move {
        for allocation in [false, true] {
            let (mut stream, _) = tokio::time::timeout(Duration::from_secs(3), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut chunk = [0; 4096];
                let n = stream.read(&mut chunk).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&chunk[..n]);
                if bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let line = std::str::from_utf8(&bytes).unwrap().lines().next().unwrap();
            if allocation {
                assert!(line.contains("upload/web"));
                break;
            }
            assert!(line.contains("retrieveItemDetailsInFolders"));
            let body = json!([{"drivewsid":ROOT_ID,"type":"FOLDER","numberOfItems":0,"items":[]}])
                .to_string();
            stream
                .write_all(
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
            tokio::time::timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err()
        );
    });
    let checkpoint = ICloudPackageCreate::encode(&saved).unwrap();
    let cancel = CancellationToken::new();
    assert!(matches!(
        provider
            .allocate_upload_for_operation(&saved.operation, &request, &checkpoint, &cancel)
            .await,
        Err(UploadError::Uncertain)
    ));
    assert!(matches!(
        provider
            .inspect_upload_for_operation(&saved.operation, &request, &checkpoint, &cancel)
            .await,
        Err(UploadError::Uncertain)
    ));
    task.await.unwrap();
}

mod roundtrip;

struct CancellingVault {
    snapshot: SecretString,
    cancel: CancellationToken,
    loads: std::sync::atomic::AtomicUsize,
}
#[async_trait]
impl CredentialVault for CancellingVault {
    async fn load(&self, _: &str) -> anyhow::Result<Option<SecretString>> {
        self.loads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        // Deliberately returns Ready in this poll after cancelling: no timer or
        // scheduler ordering can make the negative control pass accidentally.
        self.cancel.cancel();
        Ok(Some(self.snapshot.clone()))
    }
    async fn save(&self, _: &str, _: SecretString) -> anyhow::Result<()> {
        panic!("unexpected credential save")
    }
    async fn remove(&self, _: &str) -> anyhow::Result<()> {
        panic!("unexpected credential removal")
    }
}
#[tokio::test]
async fn cancelling_vault_cannot_dispatch_signed_package_body_in_same_poll() {
    let (_dir, mut provider, mut request, mut saved) = fixture();
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    zip.start_file(
        "Source.pages/Document",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    zip.write_all(b"synthetic content").unwrap();
    let bytes = zip.finish().unwrap().into_inner();
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(&bytes).unwrap();
    request.size = bytes.len() as u64;
    request.sha256 = hex::encode(Sha256::digest(&bytes));
    let cancel = CancellationToken::new();
    let semantic = crate::package_archive_semantic_identity(
        &file,
        &crate::PackageDownload {
            size: request.size,
            sha256: request.sha256.clone(),
        },
        "Source.pages",
        &cancel,
    )
    .unwrap();
    request.representation = UploadRepresentation::PackageArchive {
        expected_root: "Source.pages".into(),
        semantic,
    };
    let apple_id = "fixture@example.invalid";
    let hash = crate::account_hash(apple_id).unwrap();
    let mut source_session = ICloudReadSession::new().unwrap();
    source_session.account_hash = Some(hash.clone());
    source_session.headers.session_token = "synthetic-session".into();
    source_session.drive_endpoint = Some("https://p01-drivews.icloud.com/".parse().unwrap());
    source_session.docs_endpoint = Some("https://p01-docws.icloud.com/".parse().unwrap());
    let vault = Arc::new(CancellingVault {
        snapshot: source_session.session_snapshot().unwrap(),
        cancel: cancel.clone(),
        loads: std::sync::atomic::AtomicUsize::new(0),
    });
    provider.session = Mutex::new(Session::Sealed {
        apple_id: apple_id.into(),
        credential_id: Uuid::new_v4().to_string(),
        vault: vault.clone(),
    });
    let dispatched = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    provider.body_dispatch_probe = Some(dispatched.clone());
    saved.request = request.clone();
    saved.account_hash = hash;
    saved.phase = Phase::BodyArmed;
    saved.slot = Some(slot());
    let checkpoint = ICloudPackageCreate::encode(&saved).unwrap();
    assert!(matches!(
        provider
            .upload_stream_for_operation(&saved.operation, &request, &checkpoint, file, &cancel)
            .await,
        Err(UploadError::Uncertain)
    ));
    assert_eq!(vault.loads.load(std::sync::atomic::Ordering::SeqCst), 1);
    // Ensure the same-poll vault result really passed restoration and binding.
    let mut state = provider.session.lock().await;
    let session = ICloudPackageCreate::active(&mut state).await.unwrap();
    ICloudPackageCreate::binding(session, &saved).unwrap();
    assert_eq!(dispatched.load(std::sync::atomic::Ordering::SeqCst), 0);
    let retained = provider
        .decode(&saved.operation, &request, &checkpoint)
        .unwrap();
    assert!(retained.phase == Phase::BodyArmed && retained.registration.is_none());
}
