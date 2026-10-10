#![allow(clippy::unwrap_used)]
use super::*;
use async_trait::async_trait;
use base64::Engine as _;
use cirrove_core::mutation::{MutationIntent, MutationRequest};
use serde_json::json;
use std::os::unix::fs::PermissionsExt;
use std::{io::Write, sync::Mutex as StdMutex, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_rustls::{
    TlsAcceptor,
    rustls::{
        ServerConfig,
        pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    },
};
const ORIGIN: &str = "https://fixture.icloud-content.com";
const ID: &str = "FILE::com.apple.CloudDocs::native";
#[derive(Default)]
struct Vault(
    StdMutex<std::collections::HashMap<String, String>>,
    std::sync::atomic::AtomicUsize,
    std::sync::atomic::AtomicUsize,
    StdMutex<Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>>,
);
#[async_trait]
impl CredentialVault for Vault {
    async fn load(&self, key: &str) -> anyhow::Result<Option<SecretString>> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .get(key)
            .cloned()
            .map(SecretString::from))
    }
    async fn save(&self, key: &str, value: SecretString) -> anyhow::Result<()> {
        let count = self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        if count == self.2.load(std::sync::atomic::Ordering::SeqCst) {
            anyhow::bail!("injected durable checkpoint failure");
        }
        self.0
            .lock()
            .unwrap()
            .insert(key.into(), value.expose_secret().into());
        if count == 2 {
            let pause = self.3.lock().unwrap().clone();
            if let Some((ready, release)) = pause {
                ready.notify_one();
                release.notified().await;
            }
        }
        Ok(())
    }
    async fn remove(&self, _: &str) -> anyhow::Result<()> {
        anyhow::bail!("never remove retained checkpoint")
    }
}
#[derive(Default)]
struct Seen {
    folders: Vec<Node>,
    parent: String,
    restore_path: String,
    trashed: bool,
    restore_calls: usize,
    reads: usize,
    changed_revision: bool,
    changed_body: bool,
    wrong_identity: bool,
    denied: bool,
    storage: bool,
}
struct Server {
    state: Arc<StdMutex<Seen>>,
    task: tokio::task::JoinHandle<()>,
    client: reqwest::Client,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Server {
    async fn start(lost_reply: bool, keep_trash: bool) -> Self {
        let decoder = base64::engine::general_purpose::STANDARD;
        let cert = decoder
            .decode(include_str!("../package_create/fixtures/server-cert.b64").trim())
            .unwrap();
        let key = decoder
            .decode(include_str!("../package_create/fixtures/server-key.b64").trim())
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
        let state = Arc::new(StdMutex::new(Seen {
            trashed: true,
            folders: request().parent_route,
            parent: crate::ROOT_ID.into(),
            restore_path: "Target.pages".into(),
            ..Default::default()
        }));
        let observed = state.clone();
        let task = tokio::spawn(async move {
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                tokio::time::timeout(Duration::from_secs(3),async {
                let mut stream=acceptor.accept(socket).await.unwrap(); let mut raw=Vec::new();
                let (path,body)=loop {
                    let mut buf=[0;4096]; let n=stream.read(&mut buf).await.unwrap(); assert!(n>0);raw.extend_from_slice(&buf[..n]);assert!(raw.len()<128*1024);
                    let Some(end)=raw.windows(4).position(|w| w==b"\r\n\r\n") else {continue;};
                    let head=std::str::from_utf8(&raw[..end]).unwrap();
                    let size=head.lines().find_map(|v|v.to_ascii_lowercase().strip_prefix("content-length: ").map(str::to_owned)).map(|v|v.parse::<usize>().unwrap()).unwrap_or(0);
                    if raw.len()>=end+4+size {break (head.lines().next().unwrap().split_whitespace().nth(1).unwrap().to_owned(),raw[end+4..end+4+size].to_vec());}
                };
                let reply={let mut s=observed.lock().unwrap();s.reads+=1;
                    match path.split('?').next().unwrap() {
                        "/retrieveItemDetails"=>Some(json!({"items":[item(&s)]}).to_string().into_bytes()),
                        "/retrieveItemDetailsInFolders"=>{
                            let v:serde_json::Value=serde_json::from_slice(&body).unwrap();let id=v[0]["drivewsid"].as_str().unwrap();
                            let folder=s.folders.iter().find(|n|n.id==id).unwrap();
                            let mut children:Vec<_>=s.folders.iter().filter(|n|n.parent_id.as_deref()==Some(id)).map(|n|json!({"drivewsid":n.id,"type":"FOLDER","name":n.name,"parentId":n.parent_id,"zone":"com.apple.CloudDocs","etag":n.etag})).collect();
                            if !s.trashed&&s.parent==id{children.push(item(&s));}
                            Some(json!([{"drivewsid":id,"type":"FOLDER","name":folder.name,"parentId":folder.parent_id,"zone":"com.apple.CloudDocs","etag":folder.etag,"items":children,"numberOfItems":children.len()}]).to_string().into_bytes())
                        },
                        "/ws/com.apple.CloudDocs/download/by_id"=>Some(json!({"package_token":{"url":format!("{ORIGIN}/archive")}}).to_string().into_bytes()),
                        "/archive"=>Some(archive(s.changed_body)),
                        "/putBackItemsFromTrash"=>{let v:serde_json::Value=serde_json::from_slice(&body).unwrap();assert_eq!(v["items"][0]["drivewsid"],ID);assert_eq!(v["items"][0]["etag"],"trash-v2");s.restore_calls+=1;assert_eq!(s.restore_calls,1);s.trashed=keep_trash;if lost_reply{None}else{Some(json!({"items":[{"status":"OK","drivewsid":ID,"docwsid":"native","parentId":s.parent,"etag":"restored-v3"}]}).to_string().into_bytes())}},
                        _=>panic!("unexpected synthetic native restore route"),
                    }
                };
                if let Some(reply)=reply {let status={let state=observed.lock().unwrap();if state.storage{507}else if state.denied{403}else{200}};stream.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",reply.len()).as_bytes()).await.unwrap();stream.write_all(&reply).await.unwrap();stream.shutdown().await.unwrap();}
            }).await.unwrap();
            }
        });
        Self {
            state,
            task,
            client,
        }
    }
}

fn item(s: &Seen) -> serde_json::Value {
    json!({"drivewsid":if s.wrong_identity{"FILE::com.apple.CloudDocs::other"}else{ID},"docwsid":"native","zone":"com.apple.CloudDocs","type":"FILE","name":"Target","extension":"pages","parentId":if s.trashed{"TRASH_ROOT"}else{s.parent.as_str()},"restorePath":if s.trashed{json!(s.restore_path)}else{json!(null)},"etag":if s.changed_revision{"changed"}else if s.trashed{"trash-v2"}else{"restored-v3"},"size":17})
}
fn archive(changed: bool) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    zip.start_file(
        "Target.pages/Document",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    zip.write_all(if changed {
        b"changed content"
    } else {
        b"synthetic content"
    })
    .unwrap();
    zip.finish().unwrap().into_inner()
}
fn request() -> NativeRestoreRequest {
    let root = Node {
        id: crate::ROOT_ID.into(),
        parent_id: None,
        name: "Root".into(),
        kind: NodeKind::Folder,
        size: 0,
        modified_unix: 0,
        etag: None,
        content_version: None,
        target: None,
        package: false,
    };
    NativeRestoreRequest {
        scope: Scope {
            account: Uuid::new_v4().to_string(),
            provider: "icloud".into(),
            collection: "drive".into(),
        },
        removal_operation: Uuid::new_v4(),
        before: Node {
            id: ID.into(),
            parent_id: Some(crate::ROOT_ID.into()),
            name: "Target.pages".into(),
            kind: NodeKind::Folder,
            size: 17,
            modified_unix: 0,
            etag: Some("original-v1".into()),
            content_version: None,
            target: None,
            package: true,
        },
        parent_route: vec![root],
    }
}
fn fixture(
    server: &Server,
    request: NativeRestoreRequest,
    dir: &Path,
    source: Arc<Vault>,
    vault: Arc<Vault>,
) -> ICloudNativeRestore {
    let mut adapter = ICloudNativeRestore::from_sealed_session(
        "fixture@example.com".into(),
        Uuid::new_v4().to_string(),
        dir,
        request.clone(),
        dir.into(),
    )
    .unwrap();
    adapter.source.checkpoint = source;
    adapter.checkpoint = vault;
    let mut session = ICloudReadSession::new().unwrap();
    session.account_hash = Some(crate::account_hash("fixture@example.com").unwrap());
    session.http = server.client.clone();
    session.drive_endpoint = Some(format!("{ORIGIN}/").parse().unwrap());
    session.docs_endpoint = session.drive_endpoint.clone();
    adapter.session = Mutex::new(Session::Ready(Box::new(session)));
    adapter
}
fn authority(request: &NativeRestoreRequest) -> Arc<Vault> {
    let bytes = archive(false);
    let mut file = tempfile::tempfile_in("/var/tmp").unwrap();
    file.write_all(&bytes).unwrap();
    let receipt = crate::PackageDownload {
        size: bytes.len() as u64,
        sha256: hex::encode(Sha256::digest(&bytes)),
    };
    let semantic = crate::package_archive_semantic_identity(
        &file,
        &receipt,
        "Target.pages",
        &CancellationToken::new(),
    )
    .unwrap();
    let source = Arc::new(Vault::default());
    source.0.lock().unwrap().insert(crate::SealedNativeTrashCheckpointVault::key(&request.scope.account,request.removal_operation),json!({"version":1,"operation":request.removal_operation,"request":MutationRequest{scope:request.scope.clone(),intent:MutationIntent::TrashNativeDocument{before:request.before.clone()}},"account_hash":crate::account_hash("fixture@example.com").unwrap(),"semantic":semantic,"phase":"may_have_sent"}).to_string());
    source
}
fn private() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/var/tmp")
        .unwrap()
}
#[tokio::test]
async fn native_restore_failed_durable_arm_never_sends() {
    let dir = private();
    let server = Server::start(false, false).await;
    let request = request();
    let source = authority(&request);
    let vault = Arc::new(Vault::default());
    let adapter = fixture(&server, request, dir.path(), source, vault.clone());
    let op = Uuid::new_v4();
    let cancel = CancellationToken::new();
    adapter.prepare(op, &cancel).await.unwrap();
    vault.2.store(2, std::sync::atomic::Ordering::SeqCst);
    assert!(adapter.execute(op, &cancel).await.is_err());
    assert_eq!(server.state.lock().unwrap().restore_calls, 0);
}
#[tokio::test]
async fn native_restore_lost_reply_restarts_into_semantic_inspection_without_resend() {
    for remains_trash in [false, true] {
        let dir = private();
        let server = Server::start(true, remains_trash).await;
        let request = request();
        let source = authority(&request);
        let vault = Arc::new(Vault::default());
        let adapter = fixture(
            &server,
            request.clone(),
            dir.path(),
            source.clone(),
            vault.clone(),
        );
        let op = Uuid::new_v4();
        let cancel = CancellationToken::new();
        adapter.prepare(op, &cancel).await.unwrap();
        assert!(adapter.execute(op, &cancel).await.is_err());
        drop(adapter);
        let restarted = fixture(&server, request, dir.path(), source, vault);
        let observed = restarted.reconcile(op, &cancel).await.unwrap();
        assert_eq!(
            matches!(observed,MutationReconciliation::Applied(MutationReceipt::Upsert(ref n)) if n.id==ID&&n.etag.as_deref()==Some("restored-v3")),
            !remains_trash
        );
        assert!(restarted.execute(op, &cancel).await.is_err());
        assert_eq!(server.state.lock().unwrap().restore_calls, 1);
        server.state.lock().unwrap().changed_body = true;
        assert!(matches!(
            restarted.reconcile(op, &cancel).await.unwrap(),
            MutationReconciliation::Indeterminate
        ));
        assert_eq!(server.state.lock().unwrap().restore_calls, 1);
    }
}
#[tokio::test]
async fn native_restore_exact_receipt_and_semantics_complete_once() {
    let dir = private();
    let server = Server::start(false, false).await;
    let request = request();
    let source = authority(&request);
    let vault = Arc::new(Vault::default());
    let adapter = fixture(&server, request, dir.path(), source, vault);
    let op = Uuid::new_v4();
    let cancel = CancellationToken::new();
    adapter.prepare(op, &cancel).await.unwrap();
    let node = adapter.execute(op, &cancel).await.unwrap();
    assert_eq!(node.id, ID);
    assert_eq!(node.name, "Target.pages");
    assert!(node.package);
    assert_eq!(node.size, 17);
    assert_eq!(server.state.lock().unwrap().restore_calls, 1);
    assert!(adapter.execute(op, &cancel).await.is_err());
}
#[tokio::test]
async fn native_restore_wrong_lineage_account_and_unconfirmed_source_refuse_before_dispatch() {
    for arm in 0..4 {
        let dir = private();
        let server = Server::start(false, false).await;
        let mut request = request();
        let source = authority(&request);
        let vault = Arc::new(Vault::default());
        if arm == 0 {
            request.removal_operation = Uuid::new_v4();
        }
        if arm == 1 {
            request.scope.account = Uuid::new_v4().to_string();
        }
        if arm == 2 {
            let mut values = source.0.lock().unwrap();
            let value = values.values_mut().next().unwrap();
            let mut decoded: serde_json::Value = serde_json::from_str(value).unwrap();
            decoded["phase"] = json!("prepared");
            *value = decoded.to_string();
        }
        let adapter = fixture(&server, request, dir.path(), source, vault);
        if arm == 3 {
            server.state.lock().unwrap().wrong_identity = true;
        }
        assert!(
            adapter
                .prepare(Uuid::new_v4(), &CancellationToken::new())
                .await
                .is_err()
        );
        assert_eq!(server.state.lock().unwrap().restore_calls, 0);
    }
}
#[test]
fn native_restore_route_expresses_root_direct_and_deep_and_rejects_aliases() {
    let mut request = request();
    assert!(request.validate().is_ok());
    assert_eq!(request.restore_path(), "Target.pages");
    for name in ["Parent", "Child"] {
        let parent = request.parent_route.last().unwrap().id.clone();
        let mut node = request.parent_route[0].clone();
        node.id = format!("FOLDER::com.apple.CloudDocs::{name}");
        node.parent_id = Some(parent);
        node.name = name.into();
        node.etag = Some("folder-etag".into());
        request.before.parent_id = Some(node.id.clone());
        request.parent_route.push(node);
        assert!(request.validate().is_ok());
    }
    assert_eq!(request.restore_path(), "Parent/Child/Target.pages");
    request.parent_route[1].package = true;
    assert!(request.validate().is_err());
}

#[tokio::test]
async fn native_restore_cancel_during_saved_arm_stays_inspect_only_without_send() {
    let dir = private();
    let server = Server::start(false, false).await;
    let request = request();
    let source = authority(&request);
    let vault = Arc::new(Vault::default());
    let adapter = Arc::new(fixture(&server, request, dir.path(), source, vault.clone()));
    let op = Uuid::new_v4();
    let cancel = CancellationToken::new();
    adapter.prepare(op, &cancel).await.unwrap();
    let ready = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    *vault.3.lock().unwrap() = Some((ready.clone(), release));
    let run = adapter.clone();
    let token = cancel.clone();
    let task = tokio::spawn(async move { run.execute(op, &token).await });
    tokio::time::timeout(Duration::from_secs(3), ready.notified())
        .await
        .unwrap();
    assert_eq!(server.state.lock().unwrap().restore_calls, 0);
    cancel.cancel();
    assert!(task.await.unwrap().is_err());
    assert!(
        adapter
            .execute(op, &CancellationToken::new())
            .await
            .is_err()
    );
    assert_eq!(server.state.lock().unwrap().restore_calls, 0);
    assert!(matches!(
        adapter
            .reconcile(op, &CancellationToken::new())
            .await
            .unwrap(),
        MutationReconciliation::Indeterminate
    ));
}

#[tokio::test]
async fn native_restore_http_binds_direct_and_deeper_original_routes() {
    for depth in [1, 2] {
        let dir = private();
        let server = Server::start(false, false).await;
        let mut request = request();
        for index in 0..depth {
            let mut folder = request.parent_route[0].clone();
            folder.id = format!("FOLDER::com.apple.CloudDocs::folder-{index}");
            folder.parent_id = Some(request.parent_route.last().unwrap().id.clone());
            folder.name = format!("Folder {index}");
            folder.etag = Some(format!("folder-{index}-v1"));
            request.before.parent_id = Some(folder.id.clone());
            request.parent_route.push(folder);
        }
        {
            let mut state = server.state.lock().unwrap();
            state.folders = request.parent_route.clone();
            state.parent = request.before.parent_id.clone().unwrap();
            state.restore_path = request.restore_path();
        }
        let source = authority(&request);
        let adapter = fixture(
            &server,
            request.clone(),
            dir.path(),
            source,
            Arc::new(Vault::default()),
        );
        let op = Uuid::new_v4();
        let cancel = CancellationToken::new();
        adapter.prepare(op, &cancel).await.unwrap();
        let restored = adapter.execute(op, &cancel).await.unwrap();
        assert_eq!(restored.parent_id, request.before.parent_id);
        assert_eq!(server.state.lock().unwrap().restore_calls, 1);
    }
}

#[tokio::test]
async fn native_restore_inspection_preserves_authentication_and_storage_refusals() {
    for storage in [false, true] {
        let dir = private();
        let server = Server::start(true, false).await;
        let request = request();
        let source = authority(&request);
        let vault = Arc::new(Vault::default());
        let adapter = fixture(&server, request, dir.path(), source, vault);
        let op = Uuid::new_v4();
        let cancel = CancellationToken::new();
        adapter.prepare(op, &cancel).await.unwrap();
        assert!(adapter.execute(op, &cancel).await.is_err());
        {
            let mut state = server.state.lock().unwrap();
            state.storage = storage;
            state.denied = !storage;
        }
        let result = adapter.reconcile(op, &cancel).await;
        if storage {
            assert!(matches!(result, Err(MutationError::InsufficientStorage)));
        } else {
            assert!(matches!(
                result,
                Err(MutationError::Provider(
                    cirrove_core::ProviderError::Authentication
                ))
            ));
        }
        assert_eq!(server.state.lock().unwrap().restore_calls, 1);
    }
}

#[test]
fn native_restore_route_rejects_malformed_opaque_ancestors() {
    for suffix in [
        String::new(),
        "x".repeat(513),
        "TRASH_ROOT".into(),
        "bad/id".into(),
        "bad:id".into(),
        "bad\nidentity".into(),
        "bad\\identity".into(),
    ] {
        let mut request = request();
        let mut parent = request.parent_route[0].clone();
        parent.id = format!("FOLDER::com.apple.CloudDocs::{suffix}");
        parent.parent_id = Some(crate::ROOT_ID.into());
        parent.name = "Folder".into();
        parent.etag = Some("v1".into());
        let mut leaf = parent.clone();
        leaf.id = "FOLDER::com.apple.CloudDocs::valid-leaf".into();
        leaf.parent_id = Some(parent.id.clone());
        leaf.name = "Leaf".into();
        request.before.parent_id = Some(leaf.id.clone());
        request.parent_route.push(parent);
        request.parent_route.push(leaf);
        assert!(request.validate().is_err());
    }
}

#[test]
fn native_restore_internal_formats_require_original_package_and_plain_route() {
    for suffix in [".pages", ".numbers", ".key"] {
        let mut selected = request();
        selected.before.name = format!("Owned{suffix}");
        selected.validate().unwrap();
        let mut data = selected.clone();
        data.before.package = false;
        data.before.kind = NodeKind::File;
        assert!(data.validate().is_err());
        let mut app = selected.clone();
        app.parent_route[0].id = "FOLDER::com.apple.Keynote::documents".into();
        app.before.parent_id = Some(app.parent_route[0].id.clone());
        assert!(app.validate().is_err());
        selected.parent_route[0].package = true;
        assert!(selected.validate().is_err());
    }
}

#[tokio::test]
async fn native_restore_default_source_and_restore_share_staging_pressure() {
    let dir = private();
    let server = Server::start(false, false).await;
    let request = request();
    let adapter = fixture(
        &server,
        request.clone(),
        dir.path(),
        authority(&request),
        Arc::new(Vault::default()),
    );
    let mut held = Vec::new();
    for _ in 0..4 {
        held.push(adapter.source.staging().await.unwrap());
    }
    assert!(adapter.staging_budget.create(dir.path()).await.is_err());
    assert!(matches!(
        adapter
            .prepare(Uuid::new_v4(), &CancellationToken::new())
            .await,
        Err(MutationError::Uncertain)
    ));
    assert_eq!(server.state.lock().unwrap().reads, 0);
    assert_eq!(server.state.lock().unwrap().restore_calls, 0);
    held.pop();
    adapter
        .prepare(Uuid::new_v4(), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(server.state.lock().unwrap().restore_calls, 0);
    held.push(adapter.staging_budget.create(dir.path()).await.unwrap());
    assert!(adapter.source.staging().await.is_err());
}

#[tokio::test]
async fn native_restore_bound_staging_pressure_refuses_active_readback_before_http() {
    let dir = private();
    let server = Server::start(false, false).await;
    let request = request();
    let budget = crate::ICloudWriteStagingBudget::new();
    let adapter = fixture(
        &server,
        request.clone(),
        dir.path(),
        authority(&request),
        Arc::new(Vault::default()),
    )
    .with_write_staging_budget(budget.clone());
    let operation = Uuid::new_v4();
    let cancel = CancellationToken::new();
    adapter.prepare(operation, &cancel).await.unwrap();
    adapter.execute(operation, &cancel).await.unwrap();
    assert_eq!(server.state.lock().unwrap().restore_calls, 1);
    let mut held = Vec::new();
    for _ in 0..4 {
        held.push(budget.create(dir.path()).await.unwrap());
    }
    assert!(adapter.source.staging().await.is_err());
    let reads = server.state.lock().unwrap().reads;
    assert!(matches!(
        adapter.reconcile(operation, &cancel).await.unwrap(),
        MutationReconciliation::Indeterminate
    ));
    assert_eq!(server.state.lock().unwrap().reads, reads);
    assert_eq!(server.state.lock().unwrap().restore_calls, 1);
    held.pop();
    assert!(
        matches!(adapter.reconcile(operation, &cancel).await.unwrap(),
        MutationReconciliation::Applied(MutationReceipt::Upsert(node)) if node.id == ID)
    );
    assert_eq!(server.state.lock().unwrap().restore_calls, 1);
    held.push(budget.create(dir.path()).await.unwrap());
    assert!(budget.create(dir.path()).await.is_err());
}
