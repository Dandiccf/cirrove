#![allow(clippy::unwrap_used)]
use super::*;
use anyhow::bail;
use serde_json::json;
use std::io::Write;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
fn plan() -> OwnedPackagePlan {
    let operation = Uuid::from_u128(7);
    OwnedPackagePlan { scope: Scope {account:Uuid::from_u128(8).to_string(), provider:"icloud".into(), collection:"drive".into()}, operation,
        parent:"FOLDER::com.apple.CloudDocs::owned".into(), parent_name:format!("Cirrove Package Validation {operation}"),
        source: serde_json::from_value(json!({"drivewsid":"FILE::com.apple.CloudDocs::source", "docwsid":"source", "zone":"com.apple.CloudDocs", "name":format!("Cirrove Package Source {operation}"), "extension":"pages", "parentId":"FOLDER::com.apple.CloudDocs::owned", "etag":"revision", "size":3, "type":"FILE"})).unwrap(),
        destination:format!("Cirrove Package Import {operation}.pages"), archive_size:3, archive_sha256:hex::encode(Sha256::digest(b"zip")) }
}
fn slot() -> Slot {
    serde_json::from_value(json!({"url":"https://fixture.icloud-content.com/content", "document_id":"new-id", "owner_id":"owner"})).unwrap()
}
fn receipt() -> SecretString {
    crate::parse_package_upload_receipt(br#"{"hexBrSyntheticChecksum":"aa","ckSectionAssets":[{"hexFileChecksum":"bb","hexReferenceChecksum":"cc","hexWrappingKey":"dd","receiptToken":"YQ==","size":3}]}"#).unwrap().registration_data().unwrap()
}
fn checkpoint() -> Checkpoint {
    Checkpoint {
        version: 1,
        plan: plan(),
        account_hash: "synthetic".into(),
        phase: Phase::RegistrationStarted,
        slot: Some(slot()),
        registration: Some(receipt().expose_secret().to_owned()),
    }
}
async fn server(
    reply: Option<String>,
) -> (
    ICloudReadSession,
    tokio::task::JoinHandle<serde_json::Value>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            let (mut peer, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let body = loop {
                let mut chunk = [0; 4096];
                let n = peer.read(&mut chunk).await.unwrap();
                assert!(n > 0 && bytes.len() + n < 100000);
                bytes.extend_from_slice(&chunk[..n]);
                let Some(end) = bytes.windows(4).position(|p| p == b"\r\n\r\n") else {
                    continue;
                };
                let header = std::str::from_utf8(&bytes[..end]).unwrap().to_lowercase();
                assert!(header.starts_with("post /ws/com.apple.clouddocs/"));
                assert!(header.contains("content-type: text/plain"));
                let size: usize = header
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length: "))
                    .unwrap()
                    .parse()
                    .unwrap();
                if bytes.len() < end + 4 + size {
                    continue;
                }
                break serde_json::from_slice(&bytes[end + 4..end + 4 + size]).unwrap();
            };
            if let Some(reply) = reply {
                peer.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{reply}",
                        reply.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            }
            body
        })
        .await
        .unwrap()
    });
    let mut session = ICloudReadSession::new().unwrap();
    session.docs_endpoint = Some(endpoint.parse().unwrap());
    session.account_hash = Some("synthetic".into());
    (session, task)
}
#[tokio::test]
async fn package_create_allocation_uses_package_numeric_size_and_exact_new_id() {
    let (mut session, request)=server(Some(json!([{"url":"https://fixture.icloud-content.com/content","document_id":"allocated","owner_id":"owner"}]).to_string())).await;
    let allocated = transport::allocate(&mut session, &plan()).await.unwrap();
    assert_eq!(allocated.document_id, "allocated");
    let body = request.await.unwrap();
    assert_eq!(body["type"], "PACKAGE");
    assert_eq!(body["size"], 3);
    assert_eq!(
        body["filename"].as_str(),
        Some(plan().destination.replace(' ', "%20").as_str())
    );
    assert_eq!(body["content_type"], "application/zip");
}
#[tokio::test]
async fn package_create_registration_has_exact_authority_and_rejects_other_identity() {
    for id in ["new-id", "foreign"] {
        let reply = json!({"status":{"status_code":0},"results":[{"status":{"status_code":0},"document":{"document_id":id,"item_id":"item","etag":"new-etag","size":3}}]});
        let (mut session, request) = server(Some(reply.to_string())).await;
        assert_eq!(
            transport::register(&mut session, &checkpoint())
                .await
                .is_ok(),
            id == "new-id"
        );
        let body = request.await.unwrap();
        assert_eq!(body["command"], "add_package");
        assert_eq!(body["document_id"], "new-id");
        assert_eq!(body["allow_conflict"], false);
        assert_eq!(body["path"]["starting_document_id"], "owned");
        assert_eq!(
            body["path"]["path"].as_str(),
            Some(plan().destination.as_str())
        );
        assert!(body.get("data").is_none());
        assert!(body.get("owner").is_none());
        assert_eq!(body["pkgSignature"], "qg==");
    }
}
#[tokio::test]
async fn package_create_lost_registration_response_preserves_armed_phase() {
    let saved = checkpoint();
    let (mut session, request) = server(None).await;
    assert!(transport::register(&mut session, &saved).await.is_err());
    request.await.unwrap();
    assert!(saved.phase == Phase::RegistrationStarted);
    assert_eq!(saved.slot.unwrap().document_id, "new-id");
    // No retry loop exists. A new executor is additionally denied by its retained
    // create-new operation marker; recovery exposes inspection only.
}
#[tokio::test]
async fn package_create_unarmed_registration_never_sends_request() {
    let mut session = ICloudReadSession::new().unwrap();
    for phase in [
        Phase::Prepared,
        Phase::AllocationStarted,
        Phase::Allocated,
        Phase::BodyStarted,
        Phase::BodyComplete,
        Phase::Registered,
    ] {
        let mut saved = checkpoint();
        saved.phase = phase;
        let error = transport::register(&mut session, &saved)
            .await
            .err()
            .unwrap();
        assert_eq!(error.to_string(), "package registration not durably armed");
    }
}
#[test]
fn package_create_plan_refuses_unowned_native_sources_and_cross_scope() {
    assert!(plan().validate().is_ok());
    let mut p = plan();
    p.source.zone = "com.apple.Pages".into();
    assert!(p.validate().is_err());
    let mut p = plan();
    p.source.parent_id = ROOT_ID.into();
    assert!(p.validate().is_err());
    let mut p = plan();
    p.source.name = "personal document".into();
    assert!(p.validate().is_err());
    let mut p = plan();
    p.destination = "existing.pages".into();
    assert!(p.validate().is_err());
    let mut p = plan();
    p.scope.collection = "shared".into();
    assert!(p.validate().is_err());
    let mut p = plan();
    p.archive_size = MAX_BODY + 1;
    assert!(p.validate().is_err());
}
#[tokio::test]
async fn package_create_reused_operation_marker_refuses_before_keyring_or_network() {
    let dir = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    File::create(
        dir.path()
            .join(format!("package-create-{}.owner", plan().operation)),
    )
    .unwrap();
    let mut session = ICloudReadSession::new().unwrap();
    session.account_hash = Some("synthetic".into());
    assert!(
        OwnedPackageCreate::prepare(session, plan(), dir.path())
            .await
            .err()
            .unwrap()
            .to_string()
            .starts_with("package operation already started or unavailable")
    );
}
#[test]
fn package_create_mixed_receipt_cannot_become_registration() {
    assert!(
        crate::parse_package_upload_receipt(
            br#"{"singleFile":{},"hexBrSyntheticChecksum":"aa","ckSectionAssets":[]}"#
        )
        .is_err()
    );
}
#[derive(Default)]
struct MemoryVault {
    saved: std::sync::Mutex<Option<String>>,
    fail: bool,
}
#[async_trait::async_trait]
impl CredentialVault for MemoryVault {
    async fn load(&self, _: &str) -> Result<Option<SecretString>> {
        Ok(self.saved.lock().unwrap().clone().map(SecretString::from))
    }
    async fn save(&self, _: &str, value: SecretString) -> Result<()> {
        ensure!(!self.fail, "synthetic persistence failure");
        *self.saved.lock().unwrap() = Some(value.expose_secret().to_owned());
        Ok(())
    }
    async fn remove(&self, _: &str) -> Result<()> {
        bail!("unexpected checkpoint removal")
    }
}
#[tokio::test]
async fn package_create_durable_phase_precedes_lost_http_and_is_not_replayable() {
    let dir = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let vault = Arc::new(MemoryVault::default());
    let (session, request) = server(None).await;
    let mut saved = checkpoint();
    saved.phase = Phase::BodyComplete;
    let mut owner = OwnedPackageCreate {
        session,
        saved,
        vault: vault.clone(),
        staging: dir.path().into(),
        _marker: tempfile::tempfile_in(dir.path()).unwrap(),
    };
    owner.persist(Phase::RegistrationStarted).await.unwrap();
    assert_eq!(
        transport::register(&mut owner.session, &owner.saved)
            .await
            .err()
            .unwrap()
            .to_string(),
        "package operation outcome uncertain"
    );
    request.await.unwrap();
    let disk: Checkpoint =
        serde_json::from_str(vault.saved.lock().unwrap().as_ref().unwrap()).unwrap();
    assert!(disk.phase == Phase::RegistrationStarted);
    assert!(disk.plan == plan());
    assert_eq!(disk.slot.unwrap().document_id, "new-id");
    // execute rejects the armed owner before source staging or any HTTP request.
    assert_eq!(
        owner
            .execute(
                tempfile::tempfile_in(dir.path()).unwrap(),
                &CancellationToken::new()
            )
            .await
            .err()
            .unwrap()
            .to_string(),
        "package operation already started"
    );
}
#[tokio::test]
async fn package_create_failed_checkpoint_save_cannot_leave_executable_prepared_owner() {
    let dir = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let vault = Arc::new(MemoryVault {
        fail: true,
        ..Default::default()
    });
    let mut saved = checkpoint();
    saved.phase = Phase::Prepared;
    let mut owner = OwnedPackageCreate {
        session: ICloudReadSession::new().unwrap(),
        saved,
        vault,
        staging: dir.path().into(),
        _marker: tempfile::tempfile_in(dir.path()).unwrap(),
    };
    assert!(owner.persist(Phase::AllocationStarted).await.is_err());
    assert!(owner.saved.phase == Phase::AllocationStarted);
    assert_eq!(
        owner
            .execute(
                tempfile::tempfile_in(dir.path()).unwrap(),
                &CancellationToken::new()
            )
            .await
            .err()
            .unwrap()
            .to_string(),
        "package operation already started"
    );
}
struct PausedVault {
    saves: std::sync::atomic::AtomicUsize,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    saved: std::sync::Mutex<Option<String>>,
}
impl Default for PausedVault {
    fn default() -> Self {
        Self {
            saves: std::sync::atomic::AtomicUsize::new(0),
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
            saved: std::sync::Mutex::new(None),
        }
    }
}
#[async_trait::async_trait]
impl CredentialVault for PausedVault {
    async fn load(&self, _: &str) -> Result<Option<SecretString>> {
        Ok(self.saved.lock().unwrap().clone().map(SecretString::from))
    }
    async fn save(&self, _: &str, value: SecretString) -> Result<()> {
        if self.saves.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            self.entered.notify_one();
            self.release.notified().await;
        }
        *self.saved.lock().unwrap() = Some(value.expose_secret().to_owned());
        Ok(())
    }
    async fn remove(&self, _: &str) -> Result<()> {
        bail!("unexpected checkpoint removal")
    }
}
fn valid_zip() -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    writer
        .start_file(
            "document",
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
    writer.write_all(b"synthetic content").unwrap();
    writer.finish().unwrap().into_inner()
}
async fn preflight_server(
    p: &OwnedPackagePlan,
    allocation_override: Option<serde_json::Value>,
) -> (
    ICloudReadSession,
    CancellationToken,
    tokio::task::JoinHandle<usize>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/", listener.local_addr().unwrap());
    let folder=json!([{"drivewsid":p.parent,"docwsid":"owned","zone":"com.apple.CloudDocs","name":p.parent_name,"parentId":ROOT_ID,"type":"FOLDER","numberOfItems":1,"items":[p.source]}]).to_string();
    let allocation = allocation_override.unwrap_or_else(|| json!([{"url":"https://fixture.icloud-content.com/content","document_id":"allocated","owner_id":"owner"}])).to_string();
    let stop = CancellationToken::new();
    let finished = stop.clone();
    let task = tokio::spawn(async move {
        let mut mutations = 0;
        loop {
            let accepted = tokio::select! { _=finished.cancelled()=>break, result=listener.accept()=>result.unwrap() };
            let (mut peer, _) = accepted;
            let mut bytes = Vec::new();
            let header = loop {
                let mut chunk = [0; 4096];
                let n = peer.read(&mut chunk).await.unwrap();
                assert!(n > 0 && bytes.len() + n < 65536);
                bytes.extend_from_slice(&chunk[..n]);
                let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") else {
                    continue;
                };
                let text = std::str::from_utf8(&bytes[..end]).unwrap().to_lowercase();
                let size: usize = text
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length: "))
                    .map(|v| v.parse().unwrap())
                    .unwrap_or(0);
                if bytes.len() < end + 4 + size {
                    continue;
                }
                break text;
            };
            let response = if header.starts_with("post /retrieveitemdetailsinfolders ") {
                folder.clone()
            } else if header.starts_with("get /ws/com.apple.clouddocs/download/by_id?") {
                json!({"package_token":{"url":"https://fixture.icloud-content.com/package"}})
                    .to_string()
            } else {
                mutations += 1;
                allocation.clone()
            };
            peer.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{response}",
                    response.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        }
        mutations
    });
    let mut session = ICloudReadSession::new().unwrap();
    session.drive_endpoint = Some(endpoint.parse().unwrap());
    session.docs_endpoint = Some(endpoint.parse().unwrap());
    session.account_hash = Some("synthetic".into());
    (session, stop, task)
}
#[tokio::test]
async fn package_create_cancellation_during_started_vault_save_sends_no_mutation() {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let dir = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let bytes = valid_zip();
        let mut saved = checkpoint();
        saved.phase = Phase::Prepared;
        saved.slot = None;
        saved.registration = None;
        saved.plan.archive_size = bytes.len() as u64;
        saved.plan.archive_sha256 = hex::encode(Sha256::digest(&bytes));
        let (session, stop, server) = preflight_server(&saved.plan, None).await;
        let vault = Arc::new(PausedVault::default());
        let owner = OwnedPackageCreate {
            session,
            saved,
            vault: vault.clone(),
            staging: dir.path().into(),
            _marker: tempfile::tempfile_in(dir.path()).unwrap(),
        };
        let mut source = tempfile::tempfile_in(dir.path()).unwrap();
        source.write_all(&bytes).unwrap();
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let execution = tokio::spawn(async move { owner.execute(source, &token).await });
        vault.entered.notified().await;
        cancel.cancel();
        vault.release.notify_one();
        let error = execution.await.unwrap().err().unwrap();
        assert_eq!(error.to_string(), "package import cancelled");
        stop.cancel();
        assert_eq!(server.await.unwrap(), 0);
        let saved: Checkpoint =
            serde_json::from_str(vault.saved.lock().unwrap().as_ref().unwrap()).unwrap();
        assert!(saved.phase == Phase::AllocationStarted);
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn package_create_each_started_phase_rechecks_cancel_after_persistence() {
    for phase in [
        Phase::AllocationStarted,
        Phase::BodyStarted,
        Phase::RegistrationStarted,
    ] {
        let dir = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let vault = Arc::new(PausedVault::default());
        let mut owner = OwnedPackageCreate {
            session: ICloudReadSession::new().unwrap(),
            saved: checkpoint(),
            vault: vault.clone(),
            staging: dir.path().into(),
            _marker: tempfile::tempfile_in(dir.path()).unwrap(),
        };
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let task = tokio::spawn(async move { owner.arm(phase, &token).await });
        tokio::time::timeout(std::time::Duration::from_secs(3), vault.entered.notified())
            .await
            .unwrap();
        cancel.cancel();
        vault.release.notify_one();
        assert_eq!(
            task.await.unwrap().err().unwrap().to_string(),
            "package import cancelled"
        );
    }
}
#[test]
fn package_create_duplicate_exact_document_readback_is_refused() {
    let entry = plan().source;
    assert!(unique_observed(vec![entry.clone(), entry.clone()], &entry.docwsid).is_err());
    assert!(
        unique_observed(vec![entry.clone()], &entry.docwsid)
            .unwrap()
            .is_some()
    );
    assert!(unique_observed(vec![entry], "absent").unwrap().is_none());
}
#[tokio::test]
async fn package_create_invalid_zip_is_refused_before_metadata_or_allocation() {
    let dir = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let mut saved = checkpoint();
    saved.phase = Phase::Prepared;
    saved.slot = None;
    saved.registration = None;
    // Correct hash of b"zip" is deliberately not a ZIP archive. No endpoint is
    // configured: a missing preallocation ZIP guard returns a different error.
    let owner = OwnedPackageCreate {
        session: ICloudReadSession::new().unwrap(),
        saved,
        vault: Arc::new(MemoryVault::default()),
        staging: dir.path().into(),
        _marker: tempfile::tempfile_in(dir.path()).unwrap(),
    };
    let mut source = tempfile::tempfile_in(dir.path()).unwrap();
    source.write_all(b"zip").unwrap();
    let error = owner
        .execute(source, &CancellationToken::new())
        .await
        .err()
        .unwrap();
    assert!(
        error
            .downcast_ref::<cirrove_core::ProviderError>()
            .is_some()
    );
    assert!(!error.to_string().contains("session"));
}
#[tokio::test]
async fn package_create_optional_owner_and_empty_owner_id_do_not_imply_shared_target() {
    for owner in [None, Some(""), Some("synthetic-owner-metadata")] {
        for owner_id in ["", "synthetic-owner-id"] {
            let mut value = json!({"url":"https://fixture.icloud-content.com/content","document_id":"allocated","owner_id":owner_id});
            if let Some(owner) = owner {
                value["owner"] = json!(owner);
            }
            let (mut session, request) = server(Some(json!([value]).to_string())).await;
            let slot = transport::allocate(&mut session, &plan()).await.unwrap();
            assert_eq!(slot.document_id, "allocated");
            request.await.unwrap();
        }
    }
}
#[tokio::test]
async fn package_create_returned_owner_is_never_forwarded_as_registration_authority() {
    let mut saved = checkpoint();
    saved.slot=Some(serde_json::from_value(json!({"url":"https://fixture.icloud-content.com/content","document_id":"new-id","owner_id":"","owner":"OWNER-METADATA-NOT-AUTHORITY"})).unwrap());
    let reply = json!({"status":{"status_code":0},"results":[{"status":{"status_code":0},"document":{"document_id":"new-id","item_id":"item","etag":"revision","size":3}}]});
    let (mut session, request) = server(Some(reply.to_string())).await;
    transport::register(&mut session, &saved).await.unwrap();
    let body = request.await.unwrap();
    assert_eq!(body["document_id"], "new-id");
    assert_eq!(body["path"]["starting_document_id"], "owned");
    assert_eq!(body["allow_conflict"], false);
    assert!(!body.to_string().contains("OWNER-METADATA-NOT-AUTHORITY"));
    assert!(body.get("owner").is_none());
    assert!(body.get("owner_id").is_none());
    assert!(
        body["package"]["sections"]
            .as_array()
            .unwrap()
            .iter()
            .all(|asset| asset.get("owner").is_none())
    );
}
#[test]
fn package_create_allocation_refusals_are_typed_static_and_do_not_expose_values() {
    use transport::PackageAllocationRefusal as R;
    let baseline = json!({"url":"https://fixture.icloud-content.com/PRIVATE-URL","document_id":"PRIVATE-ID","owner_id":"","owner":"PRIVATE-OWNER"});
    for (field, value, category) in [
        (
            "document_id",
            json!("PRIVATE-ID/INVALID"),
            R::DocumentIdentity,
        ),
        (
            "owner_id",
            json!("PRIVATE-OWNER-ID".repeat(40)),
            R::OwnerIdentifierTooLong,
        ),
        ("owner", json!("PRIVATE-OWNER".repeat(40)), R::OwnerTooLong),
        (
            "url",
            json!(format!(
                "https://fixture.icloud-content.com/{}",
                "PRIVATE-URL".repeat(1000)
            )),
            R::UrlTooLong,
        ),
        (
            "url",
            json!("http://foreign.example/PRIVATE-URL"),
            R::ContentLocation,
        ),
    ] {
        let mut raw = baseline.clone();
        raw[field] = value;
        let slot: Slot = serde_json::from_value(raw).unwrap();
        let error = slot.validate().err().unwrap();
        assert_eq!(error.downcast_ref::<R>(), Some(&category));
        for output in [
            error.to_string(),
            format!("{error:?}"),
            serde_json::to_string(&category).unwrap(),
        ] {
            assert!(!output.contains("PRIVATE"));
            assert!(!output.contains("foreign.example"));
            assert!(!output.contains("icloud-content"));
        }
    }
}
#[tokio::test]
async fn package_create_malformed_allocation_shape_and_count_remain_refused() {
    use transport::PackageAllocationRefusal as R;
    for (reply, category) in [
        (
            json!([{"url":"https://fixture.icloud-content.com/PRIVATE","document_id":"PRIVATE","owner_id":null}]),
            R::Schema,
        ),
        (json!([]), R::ResultCount),
    ] {
        let (mut session, request) = server(Some(reply.to_string())).await;
        let error = transport::allocate(&mut session, &plan())
            .await
            .err()
            .unwrap();
        request.await.unwrap();
        assert_eq!(error.downcast_ref::<R>(), Some(&category));
        assert!(!format!("{error:?}").contains("PRIVATE"));
    }
}
#[tokio::test]
async fn package_create_refused_allocation_retains_started_without_body_or_registration() {
    tokio::time::timeout(std::time::Duration::from_secs(5),async {
        let dir=tempfile::Builder::new().permissions(std::fs::Permissions::from_mode(0o700)).tempdir().unwrap();let bytes=valid_zip();let mut saved=checkpoint();saved.phase=Phase::Prepared;saved.slot=None;saved.registration=None;saved.plan.archive_size=bytes.len() as u64;saved.plan.archive_sha256=hex::encode(Sha256::digest(&bytes));
        let (session,stop,server)=preflight_server(&saved.plan,Some(json!([{"url":"https://fixture.icloud-content.com/PRIVATE","document_id":"","owner_id":"","owner":"allowed-owner-metadata"}]))).await;
        let vault=Arc::new(MemoryVault::default());let owner=OwnedPackageCreate {session,saved,vault:vault.clone(),staging:dir.path().into(),_marker:tempfile::tempfile_in(dir.path()).unwrap()};
        let mut source=tempfile::tempfile_in(dir.path()).unwrap();source.write_all(&bytes).unwrap();
        let error=owner.execute(source,&CancellationToken::new()).await.err().unwrap();stop.cancel();assert_eq!(server.await.unwrap(),1);
        assert_eq!(error.downcast_ref::<transport::PackageAllocationRefusal>(),Some(&transport::PackageAllocationRefusal::DocumentIdentity));
        let saved:Checkpoint=serde_json::from_str(vault.saved.lock().unwrap().as_ref().unwrap()).unwrap();assert!(saved.phase==Phase::AllocationStarted);assert!(saved.slot.is_none());assert!(saved.registration.is_none());
    }).await.unwrap();
}
