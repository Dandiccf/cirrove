#![allow(clippy::unwrap_used)]
use super::*;
use base64::Engine as _;
use serde_json::json;
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
struct Vault(StdMutex<std::collections::HashMap<String, String>>);
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
        self.0
            .lock()
            .unwrap()
            .insert(key.into(), value.expose_secret().into());
        Ok(())
    }
    async fn remove(&self, _: &str) -> anyhow::Result<()> {
        anyhow::bail!("never remove retained checkpoint")
    }
}
#[derive(Default)]
struct Seen {
    trashed: bool,
    trash_calls: usize,
    reads: usize,
    changed_revision: bool,
    changed_body: bool,
    wrong_identity: bool,
    denied: bool,
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
    async fn start(lost_reply: bool, keep_active: bool) -> Self {
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
        let state = Arc::new(StdMutex::new(Seen::default()));
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
                        "/retrieveItemDetails"=>Some(json!({"items":[{"drivewsid":if s.wrong_identity {"FILE::com.apple.CloudDocs::other"}else{ID},"docwsid":"native","zone":"com.apple.CloudDocs","type":"FILE","name":"Target","extension":"pages","parentId":if s.trashed {"TRASH_ROOT"}else{crate::ROOT_ID},"restorePath":if s.trashed {json!(["root"])}else{json!(null)},"etag":if s.changed_revision {"changed"}else if s.trashed {"trash-v2"}else{"original-v1"},"size":17}]}).to_string().into_bytes()),
                        "/ws/com.apple.CloudDocs/download/by_id"=>Some(json!({"package_token":{"url":format!("{ORIGIN}/archive")}}).to_string().into_bytes()),
                        "/archive"=>{let mut zip=zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));zip.start_file("Target.pages/Document",zip::write::SimpleFileOptions::default()).unwrap();zip.write_all(if s.changed_body {b"changed content"}else{b"synthetic content"}).unwrap();Some(zip.finish().unwrap().into_inner())},
                        "/moveItemsToTrash"=>{let value:serde_json::Value=serde_json::from_slice(&body).unwrap();assert_eq!(value["items"][0]["drivewsid"],ID);assert_eq!(value["items"][0]["etag"],"original-v1");s.trash_calls+=1;assert_eq!(s.trash_calls,1);s.trashed = !keep_active;if lost_reply {None}else{Some(json!({"items":[{"status":"OK"}]}).to_string().into_bytes())}},
                        _=>panic!("unexpected synthetic native Trash route"),
                    }
                };
                if let Some(reply)=reply {let status=if observed.lock().unwrap().denied {403} else {200};stream.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",reply.len()).as_bytes()).await.unwrap();stream.write_all(&reply).await.unwrap();stream.shutdown().await.unwrap();}
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
fn private_directory() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}
fn request() -> MutationRequest {
    MutationRequest {
        scope: Scope {
            account: Uuid::new_v4().to_string(),
            provider: "icloud".into(),
            collection: "drive".into(),
        },
        intent: MutationIntent::TrashNativeDocument {
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
        },
    }
}
fn adapter(
    request: &MutationRequest,
    server: &Server,
    dir: &Path,
    vault: Arc<dyn CredentialVault>,
) -> ICloudNativeTrash {
    let mut session = ICloudReadSession::new().unwrap();
    session.account_hash = Some(crate::account_hash("fixture@example.com").unwrap());
    session.http = server.client.clone();
    session.drive_endpoint = Some(format!("{ORIGIN}/").parse().unwrap());
    session.docs_endpoint = session.drive_endpoint.clone();
    ICloudNativeTrash {
        scope: request.scope.clone(),
        before: request.intent.before().unwrap().clone(),
        apple_id: "fixture@example.com".into(),
        staging: dir.into(),
        session: Mutex::new(Session::Ready(Box::new(session))),
        checkpoint: vault,
        operation_lock: Mutex::new(()),
    }
}
#[tokio::test]
async fn native_trash_lost_reply_restart_inspects_exact_semantics_without_replay() {
    let dir = private_directory();
    let server = Server::start(true, false).await;
    let vault = Arc::new(Vault::default());
    let request = request();
    let op = Uuid::new_v4().to_string();
    let cancel = CancellationToken::new();
    let first = adapter(&request, &server, dir.path(), vault.clone());
    let prepared = first
        .prepare_mutation_for_operation(&op, &request, &cancel)
        .await
        .unwrap();
    assert!(
        first
            .mutate_operation(&op, &request, prepared.as_deref(), &cancel)
            .await
            .is_err()
    );
    drop(first);
    let restarted = adapter(&request, &server, dir.path(), vault);
    assert!(
        matches!(restarted.reconcile_operation(&op,&request,prepared.as_deref(),&cancel).await.unwrap(),MutationReconciliation::Applied(MutationReceipt::Removed{item}) if item==ID)
    );
    assert!(matches!(
        restarted
            .mutate_operation(&op, &request, prepared.as_deref(), &cancel)
            .await,
        Err(MutationError::Uncertain)
    ));
    assert_eq!(server.state.lock().unwrap().trash_calls, 1);
    server.state.lock().unwrap().changed_body = true;
    assert!(matches!(
        restarted
            .reconcile_operation(&op, &request, prepared.as_deref(), &cancel)
            .await
            .unwrap(),
        MutationReconciliation::Indeterminate
    ));
    assert_eq!(server.state.lock().unwrap().trash_calls, 1);
    server.state.lock().unwrap().denied = true;
    assert!(matches!(
        restarted
            .reconcile_operation(&op, &request, prepared.as_deref(), &cancel)
            .await,
        Err(MutationError::Provider(ProviderError::Authentication))
    ));
    assert_eq!(server.state.lock().unwrap().trash_calls, 1);
}
#[tokio::test]
async fn native_trash_uncertain_active_source_never_authorizes_replay() {
    let dir = private_directory();
    let server = Server::start(true, true).await;
    let vault = Arc::new(Vault::default());
    let request = request();
    let op = Uuid::new_v4().to_string();
    let cancel = CancellationToken::new();
    let first = adapter(&request, &server, dir.path(), vault.clone());
    let prepared = first
        .prepare_mutation_for_operation(&op, &request, &cancel)
        .await
        .unwrap();
    assert!(
        first
            .mutate_operation(&op, &request, prepared.as_deref(), &cancel)
            .await
            .is_err()
    );
    drop(first);
    let restarted = adapter(&request, &server, dir.path(), vault);
    assert!(matches!(
        restarted
            .reconcile_operation(&op, &request, prepared.as_deref(), &cancel)
            .await
            .unwrap(),
        MutationReconciliation::Indeterminate
    ));
    assert!(
        restarted
            .prepare_mutation_for_operation(&op, &request, &cancel)
            .await
            .is_err()
    );
    // A recovered caller may still retain the original prepared identity.
    // Refuse direct dispatch too, not merely another preparation attempt.
    assert!(matches!(
        restarted
            .mutate_operation(&op, &request, prepared.as_deref(), &cancel)
            .await,
        Err(MutationError::Uncertain)
    ));
    assert_eq!(server.state.lock().unwrap().trash_calls, 1);
}
#[tokio::test]
async fn native_trash_changed_original_revision_or_semantics_refused_before_dispatch() {
    for revision in [true, false] {
        let dir = private_directory();
        let server = Server::start(false, false).await;
        let request = request();
        let op = Uuid::new_v4().to_string();
        let cancel = CancellationToken::new();
        let provider = adapter(&request, &server, dir.path(), Arc::new(Vault::default()));
        let prepared = provider
            .prepare_mutation_for_operation(&op, &request, &cancel)
            .await
            .unwrap();
        {
            let mut s = server.state.lock().unwrap();
            s.changed_revision = revision;
            s.changed_body = !revision;
        }
        assert!(matches!(
            provider
                .mutate_operation(&op, &request, prepared.as_deref(), &cancel)
                .await,
            Err(MutationError::Conflict)
        ));
        assert_eq!(server.state.lock().unwrap().trash_calls, 0);
    }
}
#[tokio::test]
async fn native_trash_wrong_checkpoint_binding_and_wrong_remote_identity_refused() {
    let dir = private_directory();
    let server = Server::start(false, false).await;
    let request = request();
    let op = Uuid::new_v4().to_string();
    let cancel = CancellationToken::new();
    let vault = Arc::new(Vault::default());
    let provider = adapter(&request, &server, dir.path(), vault.clone());
    provider
        .prepare_mutation_for_operation(&op, &request, &cancel)
        .await
        .unwrap();
    let key = crate::SealedNativeTrashCheckpointVault::key(
        &request.scope.account,
        Uuid::parse_str(&op).unwrap(),
    );
    {
        let mut values = vault.0.lock().unwrap();
        let mut value: serde_json::Value = serde_json::from_str(values.get(&key).unwrap()).unwrap();
        value["operation"] = json!(Uuid::new_v4());
        values.insert(key, value.to_string());
    }
    let reads = server.state.lock().unwrap().reads;
    assert!(matches!(
        provider
            .reconcile_operation(&op, &request, Some(ID), &cancel)
            .await,
        Err(MutationError::Invalid)
    ));
    assert_eq!(reads, server.state.lock().unwrap().reads);
    server.state.lock().unwrap().wrong_identity = true;
    assert!(
        provider
            .prepare_mutation_for_operation(&Uuid::new_v4().to_string(), &request, &cancel)
            .await
            .is_err()
    );
    assert_eq!(server.state.lock().unwrap().trash_calls, 0);
}
struct CancellingVault {
    inner: Arc<Vault>,
    cancel: CancellationToken,
}
#[async_trait]
impl CredentialVault for CancellingVault {
    async fn load(&self, key: &str) -> anyhow::Result<Option<SecretString>> {
        self.inner.load(key).await
    }
    async fn save(&self, key: &str, value: SecretString) -> anyhow::Result<()> {
        let armed = serde_json::from_str::<serde_json::Value>(value.expose_secret())?["phase"]
            == "may_have_sent";
        self.inner.save(key, value).await?;
        if armed {
            self.cancel.cancel();
        }
        Ok(())
    }
    async fn remove(&self, _: &str) -> anyhow::Result<()> {
        anyhow::bail!("no removal")
    }
}
#[tokio::test]
async fn native_trash_cancel_during_durable_arm_never_dispatches_or_replays() {
    let dir = private_directory();
    let server = Server::start(false, false).await;
    let request = request();
    let op = Uuid::new_v4().to_string();
    let cancel = CancellationToken::new();
    let saved = Arc::new(Vault::default());
    let provider = adapter(
        &request,
        &server,
        dir.path(),
        Arc::new(CancellingVault {
            inner: saved.clone(),
            cancel: cancel.clone(),
        }),
    );
    let prepared = provider
        .prepare_mutation_for_operation(&op, &request, &cancel)
        .await
        .unwrap();
    assert!(matches!(
        provider
            .mutate_operation(&op, &request, prepared.as_deref(), &cancel)
            .await,
        Err(MutationError::Provider(ProviderError::Cancelled))
    ));
    assert_eq!(server.state.lock().unwrap().trash_calls, 0);
    drop(provider);
    let restarted = adapter(&request, &server, dir.path(), saved);
    assert!(matches!(
        restarted
            .reconcile_operation(
                &op,
                &request,
                prepared.as_deref(),
                &CancellationToken::new()
            )
            .await
            .unwrap(),
        MutationReconciliation::Indeterminate
    ));
    assert_eq!(server.state.lock().unwrap().trash_calls, 0);
}
#[tokio::test]
async fn native_trash_success_requires_semantic_recovery_and_keeps_checkpoint() {
    let dir = private_directory();
    let server = Server::start(false, false).await;
    let request = request();
    let op = Uuid::new_v4().to_string();
    let cancel = CancellationToken::new();
    let saved = Arc::new(Vault::default());
    let provider = adapter(&request, &server, dir.path(), saved.clone());
    let prepared = provider
        .prepare_mutation_for_operation(&op, &request, &cancel)
        .await
        .unwrap();
    assert!(matches!(
        provider
            .reconcile_operation(&op, &request, prepared.as_deref(), &cancel)
            .await
            .unwrap(),
        MutationReconciliation::Uncommitted
    ));
    assert!(
        matches!(provider.mutate_operation(&op,&request,prepared.as_deref(),&cancel).await.unwrap(),MutationReceipt::Removed{item} if item==ID)
    );
    assert_eq!(server.state.lock().unwrap().trash_calls, 1);
    let key = crate::SealedNativeTrashCheckpointVault::key(
        &request.scope.account,
        Uuid::parse_str(&op).unwrap(),
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(saved.0.lock().unwrap().get(&key).unwrap())
            .unwrap()["phase"],
        "may_have_sent"
    );
}
#[test]
fn native_trash_eligibility_never_reuses_ordinary_folder_or_file_capability() {
    let request = request();
    let before = request.intent.before().unwrap();
    ICloudNativeTrash::identity(&request.scope, before).unwrap();
    let mut changed = before.clone();
    changed.package = false;
    assert!(ICloudNativeTrash::identity(&request.scope, &changed).is_err());
    let mut changed = before.clone();
    changed.kind = NodeKind::File;
    assert!(ICloudNativeTrash::identity(&request.scope, &changed).is_err());
    changed = before.clone();
    changed.id = "FOLDER::com.apple.CloudDocs::native".into();
    assert!(ICloudNativeTrash::identity(&request.scope, &changed).is_err());
    changed = before.clone();
    changed.name = "Other.numbers".into();
    assert!(ICloudNativeTrash::identity(&request.scope, &changed).is_err());
    changed = before.clone();
    changed.parent_id = Some(crate::write_transport::TRASH_ROOT.into());
    assert!(ICloudNativeTrash::identity(&request.scope, &changed).is_err());
}
struct FailedArmVault {
    inner: Arc<Vault>,
    publish: bool,
}
#[async_trait]
impl CredentialVault for FailedArmVault {
    async fn load(&self, key: &str) -> anyhow::Result<Option<SecretString>> {
        self.inner.load(key).await
    }
    async fn save(&self, key: &str, value: SecretString) -> anyhow::Result<()> {
        let armed = serde_json::from_str::<serde_json::Value>(value.expose_secret())?["phase"]
            == "may_have_sent";
        if !armed {
            return self.inner.save(key, value).await;
        }
        if self.publish {
            self.inner.save(key, value).await?;
        }
        anyhow::bail!("synthetic arm persistence failure")
    }
    async fn remove(&self, _: &str) -> anyhow::Result<()> {
        anyhow::bail!("no removal")
    }
}
#[tokio::test]
async fn native_trash_failed_arm_save_never_sends_before_or_after_publication() {
    for publish in [false, true] {
        let dir = private_directory();
        let server = Server::start(false, false).await;
        let request = request();
        let op = Uuid::new_v4().to_string();
        let cancel = CancellationToken::new();
        let saved = Arc::new(Vault::default());
        let provider = adapter(
            &request,
            &server,
            dir.path(),
            Arc::new(FailedArmVault {
                inner: saved.clone(),
                publish,
            }),
        );
        let prepared = provider
            .prepare_mutation_for_operation(&op, &request, &cancel)
            .await
            .unwrap();
        assert!(matches!(
            provider
                .mutate_operation(&op, &request, prepared.as_deref(), &cancel)
                .await,
            Err(MutationError::Uncertain)
        ));
        assert_eq!(server.state.lock().unwrap().trash_calls, 0);
        drop(provider);
        let restarted = adapter(&request, &server, dir.path(), saved);
        let result = restarted
            .reconcile_operation(&op, &request, prepared.as_deref(), &cancel)
            .await
            .unwrap();
        if publish {
            assert!(matches!(result, MutationReconciliation::Indeterminate));
            assert!(matches!(
                restarted
                    .mutate_operation(&op, &request, prepared.as_deref(), &cancel)
                    .await,
                Err(MutationError::Uncertain)
            ));
        } else {
            assert!(matches!(result, MutationReconciliation::Uncommitted));
        }
        assert_eq!(server.state.lock().unwrap().trash_calls, 0);
    }
}
#[tokio::test]
async fn native_trash_checkpoint_account_request_and_semantic_schema_bind_before_network() {
    let dir = private_directory();
    let server = Server::start(false, false).await;
    let request = request();
    let op = Uuid::new_v4().to_string();
    let cancel = CancellationToken::new();
    let saved = Arc::new(Vault::default());
    let provider = adapter(&request, &server, dir.path(), saved.clone());
    provider
        .prepare_mutation_for_operation(&op, &request, &cancel)
        .await
        .unwrap();
    let key = crate::SealedNativeTrashCheckpointVault::key(
        &request.scope.account,
        Uuid::parse_str(&op).unwrap(),
    );
    let original = saved.0.lock().unwrap().get(&key).unwrap().clone();
    let reads = server.state.lock().unwrap().reads;
    for arm in 0..3 {
        let mut value: serde_json::Value = serde_json::from_str(&original).unwrap();
        match arm {
            0 => value["account_hash"] = json!(crate::account_hash("other@example.com").unwrap()),
            1 => value["request"]["intent"]["before"]["etag"] = json!("other-revision"),
            _ => value["semantic"]["version"] = json!(0),
        }
        saved
            .0
            .lock()
            .unwrap()
            .insert(key.clone(), value.to_string());
        assert!(
            matches!(
                provider
                    .reconcile_operation(&op, &request, Some(ID), &cancel)
                    .await,
                Err(MutationError::Invalid)
            ),
            "arm {arm}"
        );
        assert_eq!(server.state.lock().unwrap().reads, reads);
        assert_eq!(server.state.lock().unwrap().trash_calls, 0);
    }
}

#[tokio::test]
async fn native_trash_staging_requires_owned_private_directory_without_symlink() {
    let dir = private_directory();
    let server = Server::start(false, false).await;
    let request = request();
    let mut provider = adapter(&request, &server, dir.path(), Arc::new(Vault::default()));
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(provider.staging(), Err(MutationError::Invalid)));
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let file = provider.staging().unwrap();
    assert_eq!(file.metadata().unwrap().nlink(), 0);
    assert_eq!(file.metadata().unwrap().permissions().mode() & 0o077, 0);
    let link = dir.path().join("staging-link");
    std::os::unix::fs::symlink(dir.path(), &link).unwrap();
    provider.staging = link;
    assert!(matches!(provider.staging(), Err(MutationError::Invalid)));
    assert_eq!(server.state.lock().unwrap().reads, 0);
}
