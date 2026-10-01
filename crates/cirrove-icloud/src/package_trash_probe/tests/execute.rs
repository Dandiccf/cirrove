//! Actual coordinator execute path, HTTPS package reads, scripted mutations and vault barriers.
use super::*;
use base64::Engine as _;
use sha2::Digest;
use std::{io::Write, time::Duration};
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
#[derive(Clone, Copy)]
enum Fault {
    None,
    Fail(PackageTrashPhase),
    Pause(PackageTrashPhase),
}
struct ScriptVault {
    latest: Mutex<Option<String>>,
    fault: Fault,
    entered: Notify,
    release: Notify,
}
#[async_trait::async_trait]
impl CredentialVault for ScriptVault {
    async fn load(&self, _: &str) -> Result<Option<SecretString>> {
        Ok(self.latest.lock().unwrap().clone().map(SecretString::from))
    }
    async fn save(&self, _: &str, value: SecretString) -> Result<()> {
        let saved: Checkpoint = serde_json::from_str(value.expose_secret())?;
        if matches!(self.fault,Fault::Fail(phase) if phase==saved.phase) {
            anyhow::bail!("injected save failure");
        }
        *self.latest.lock().unwrap() = Some(value.expose_secret().into());
        if matches!(self.fault,Fault::Pause(phase) if phase==saved.phase) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(())
    }
    async fn remove(&self, _: &str) -> Result<()> {
        anyhow::bail!("unexpected checkpoint removal")
    }
}
impl ScriptVault {
    fn new(fault: Fault) -> Self {
        Self {
            latest: Mutex::new(None),
            fault,
            entered: Notify::new(),
            release: Notify::new(),
        }
    }
    fn phase(&self) -> PackageTrashPhase {
        serde_json::from_str::<Checkpoint>(
            self.latest
                .lock()
                .unwrap()
                .as_deref()
                .expect("durable checkpoint before request"),
        )
        .unwrap()
        .phase
    }
}
fn archive(root: &str) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    zip.start_file(
        format!("{root}/Index/Document.iwa"),
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    zip.write_all(b"owned synthetic native document").unwrap();
    zip.finish().unwrap().into_inner()
}
struct Remote {
    current: DriveEntry,
    trashed: bool,
    mutations: Vec<PackageTrashPhase>,
}
struct Server {
    state: Arc<Mutex<Remote>>,
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
    async fn new(
        saved: &Checkpoint,
        vault: Arc<ScriptVault>,
        stale_status: u16,
        lose_rename: bool,
        trash_parent: bool,
    ) -> Self {
        let decoder = base64::engine::general_purpose::STANDARD;
        let cert = decoder
            .decode(include_str!("../../package_create/fixtures/server-cert.b64").trim())
            .unwrap();
        let key = decoder
            .decode(include_str!("../../package_create/fixtures/server-key.b64").trim())
            .unwrap();
        let tls = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(cert.clone())],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
            )
            .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(tls));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = reqwest::Client::builder()
            .no_proxy()
            .resolve("fixture.icloud-content.com", listener.local_addr().unwrap())
            .tls_certs_only([reqwest::Certificate::from_der(&cert).unwrap()])
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        let state = Arc::new(Mutex::new(Remote {
            current: saved.imported.clone(),
            trashed: false,
            mutations: Vec::new(),
        }));
        let remote = state.clone();
        let plan = saved.plan.clone();
        let renamed = saved.renamed_name();
        let original = saved.imported.clone();
        let stop = CancellationToken::new();
        let stopping = stop.clone();
        let task = tokio::spawn(async move {
            loop {
                let socket = tokio::select! {biased;_=stopping.cancelled()=>break,accepted=listener.accept()=>accepted.unwrap().0};
                tokio::time::timeout(Duration::from_secs(3),async {
                    let mut peer=acceptor.accept(socket).await.unwrap();let mut raw=Vec::new();
                    let (headers,body)=loop {
                        let mut bytes=[0;4096];let n=peer.read(&mut bytes).await.unwrap();assert!(n>0);raw.extend_from_slice(&bytes[..n]);assert!(raw.len()<96*1024);
                        let Some(end)=raw.windows(4).position(|x|x==b"\r\n\r\n") else{continue;};
                        let headers=std::str::from_utf8(&raw[..end]).unwrap().to_owned();
                        let length=headers.lines().find_map(|s|s.to_lowercase().strip_prefix("content-length: ").map(str::to_owned)).map(|n|n.parse::<usize>().unwrap()).unwrap_or(0);
                        if raw.len()>=end+4+length {break(headers,raw[end+4..end+4+length].to_vec());}
                    };
                    let mut parts=headers.lines().next().unwrap().split_whitespace();let method=parts.next().unwrap();let path=parts.next().unwrap();
                    let response={
                        let mut state=remote.lock().unwrap();
                        match (method,path.split('?').next().unwrap()) {
                            ("POST","/retrieveItemDetailsInFolders")=>{
                                let body:serde_json::Value=serde_json::from_slice(&body).unwrap();assert_eq!(body[0]["drivewsid"],plan.parent);
                                let items=if state.trashed{vec![plan.source.clone()]}else{vec![plan.source.clone(),state.current.clone()]};
                                let mut folder=json!({"drivewsid":plan.parent,"docwsid":"parent","zone":"com.apple.CloudDocs","parentId":ROOT_ID,"name":plan.parent_name,"type":"FOLDER","etag":"parent-version","status":"OK","numberOfItems":items.len(),"items":items});
                                if trash_parent{folder["restorePath"]=json!(["former-root"]);}
                                Some((200,serde_json::to_vec(&json!([folder])).unwrap()))
                            },
                            ("POST","/retrieveItemDetails")=>{
                                let body:serde_json::Value=serde_json::from_slice(&body).unwrap();assert_eq!(body["items"][0]["drivewsid"],original.drivewsid);
                                let mut item=serde_json::to_value(&state.current).unwrap();if state.trashed{item["parentId"]=json!("TRASH_ROOT");item["restorePath"]=json!([plan.parent]);}
                                Some((200,serde_json::to_vec(&json!({"items":[item]})).unwrap()))
                            },
                            ("GET","/ws/com.apple.CloudDocs/download/by_id")=>{
                                assert!(path.ends_with("document_id=allocated"));Some((200,serde_json::to_vec(&json!({"package_token":{"url":format!("{ORIGIN}/content")}})).unwrap()))
                            },
                            ("GET","/content")=>Some((200,archive(&state.current.display_name()))),
                            ("POST","/renameItems")=>{
                                assert_eq!(vault.phase(),PackageTrashPhase::RenameArmed,"rename before durable arm");
                                let body:serde_json::Value=serde_json::from_slice(&body).unwrap();assert_eq!(body,json!({"items":[{"drivewsid":original.drivewsid,"name":renamed,"etag":"E0"}]}));
                                state.mutations.push(PackageTrashPhase::RenameArmed);state.current.name=renamed.strip_suffix(".pages").unwrap().into();state.current.etag="E1".into();
                                if lose_rename{None}else{Some((200,br#"{"items":[{"status":"OK"}]}"#.to_vec()))}
                            },
                            ("POST","/moveItemsToTrash")=>{
                                let body:serde_json::Value=serde_json::from_slice(&body).unwrap();let stale=body["items"][0]["etag"]=="E0";
                                let phase=if stale{PackageTrashPhase::StaleTrashArmed}else{PackageTrashPhase::CurrentTrashArmed};assert_eq!(vault.phase(),phase,"Trash before durable arm");
                                assert_eq!(body,json!({"items":[{"drivewsid":original.drivewsid,"etag":if stale{"E0"}else{"E1"},"clientId":original.drivewsid}]}));
                                state.mutations.push(phase);
                                if stale{Some((stale_status,br#"{"items":[{"status":"OTHER_REFUSAL"}]}"#.to_vec()))}
                                else{state.trashed=true;state.current.etag="trash-E1".into();Some((200,br#"{"items":[{"status":"OK"}]}"#.to_vec()))}
                            },
                            _=>panic!("unexpected scripted route"),
                        }
                    };
                    if let Some((status,body))=response{
                        peer.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await.unwrap();peer.write_all(&body).await.unwrap();peer.shutdown().await.unwrap();
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
    async fn finish(mut self) -> Vec<PackageTrashPhase> {
        self.stop.cancel();
        self.task.take().unwrap().await.unwrap();
        self.state.lock().unwrap().mutations.clone()
    }
}
async fn fixture(
    fault: Fault,
    stale_status: u16,
    lose_rename: bool,
    trash_parent: bool,
) -> (
    tempfile::TempDir,
    OwnedPackageTrashProbe,
    Arc<ScriptVault>,
    Server,
) {
    let dir = tempfile::tempdir().unwrap();
    let mut saved = saved();
    let bytes = archive(&saved.imported.display_name());
    let mut file = anonymous_staging(dir.path()).unwrap();
    file.write_all(&bytes).unwrap();
    let receipt = PackageDownload {
        size: bytes.len() as u64,
        sha256: hex::encode(crate::Sha256::digest(&bytes)),
    };
    saved.semantic = package_archive_semantic_identity(
        &file,
        &receipt,
        &saved.imported.display_name(),
        &CancellationToken::new(),
    )
    .unwrap();
    let vault = Arc::new(ScriptVault::new(fault));
    let server = Server::new(
        &saved,
        vault.clone(),
        stale_status,
        lose_rename,
        trash_parent,
    )
    .await;
    let mut session = ICloudReadSession::new().unwrap();
    session.http = server.client.clone();
    session.drive_endpoint = Some(format!("{ORIGIN}/").parse().unwrap());
    session.docs_endpoint = session.drive_endpoint.clone();
    session.account_hash = Some(saved.account_hash.clone());
    let owner = OwnedPackageTrashProbe {
        session,
        apple_account: "fixture@example.com".into(),
        saved,
        vault: vault.clone(),
        staging: dir.path().into(),
        _marker: anonymous_staging(dir.path()).unwrap(),
    };
    (dir, owner, vault, server)
}
#[tokio::test]
async fn package_trash_execute_https_success_requires_durable_order_and_semantic_recovery() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let (_dir, owner, vault, server) = fixture(Fault::None, 412, false, false).await;
        let result = owner.execute(&CancellationToken::new()).await.unwrap();
        assert!(
            result.metadata_stale_refusal_recorded
                && result.current_trash_semantic_recovery_verified
        );
        assert_eq!(vault.phase(), PackageTrashPhase::Recovered);
        assert_eq!(
            server.finish().await,
            vec![
                PackageTrashPhase::RenameArmed,
                PackageTrashPhase::StaleTrashArmed,
                PackageTrashPhase::CurrentTrashArmed
            ]
        );
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn package_trash_execute_vault_failure_never_dispatches_unpersisted_boundary() {
    tokio::time::timeout(Duration::from_secs(20), async {
        for (index, (_, phase)) in phases().into_iter().enumerate() {
            let (_dir, owner, _vault, server) =
                fixture(Fault::Fail(phase), 412, false, false).await;
            let error = owner
                .execute(&CancellationToken::new())
                .await
                .err()
                .unwrap();
            assert_eq!(error.to_string(), "injected save failure");
            assert_eq!(
                server.finish().await.len(),
                index,
                "unpersisted boundary reached HTTP"
            );
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn package_trash_execute_cancelled_while_saving_never_dispatches_next_http_mutation() {
    tokio::time::timeout(Duration::from_secs(20), async {
        for (index, (_, phase)) in phases().into_iter().enumerate() {
            let (_dir, owner, vault, server) =
                fixture(Fault::Pause(phase), 412, false, false).await;
            let cancel = CancellationToken::new();
            let token = cancel.clone();
            let task = tokio::spawn(async move { owner.execute(&token).await });
            vault.entered.notified().await;
            assert_eq!(
                server.state.lock().unwrap().mutations.len(),
                index,
                "request sent before save returned"
            );
            cancel.cancel();
            vault.release.notify_one();
            assert_eq!(
                task.await.unwrap().err().unwrap().to_string(),
                "package Trash probe cancelled"
            );
            assert_eq!(
                server.finish().await.len(),
                index,
                "cancelled mutation reached HTTP"
            );
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn package_trash_execute_generic_refusal_never_claims_stale_revision_enforcement() {
    tokio::time::timeout(Duration::from_secs(20), async {
        for status in [200, 400, 404, 409] {
            let (_dir, owner, vault, server) = fixture(Fault::None, status, false, false).await;
            let error = owner
                .execute(&CancellationToken::new())
                .await
                .err()
                .unwrap();
            assert_eq!(
                error.to_string(),
                format!(
                    "package stale Trash lacks explicit revision refusal: {}",
                    if status == 409 {
                        "Conflict"
                    } else {
                        "Rejected"
                    }
                )
            );
            assert_eq!(vault.phase(), PackageTrashPhase::StaleTrashArmed);
            assert_eq!(server.finish().await.len(), 2);
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn package_trash_execute_lost_rename_response_and_recovered_parent_stop_without_replay() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let (dir, owner, vault, server) = fixture(Fault::None, 412, true, false).await;
        assert!(owner.execute(&CancellationToken::new()).await.is_err());
        assert_eq!(vault.phase(), PackageTrashPhase::RenameArmed);
        let restored: Checkpoint =
            serde_json::from_str(vault.latest.lock().unwrap().as_deref().unwrap()).unwrap();
        let mut fresh = ICloudReadSession::new().unwrap();
        fresh.http = server.client.clone();
        fresh.drive_endpoint = Some(format!("{ORIGIN}/").parse().unwrap());
        fresh.docs_endpoint = fresh.drive_endpoint.clone();
        fresh.account_hash = Some(restored.account_hash.clone());
        restored
            .validate(&fresh, &restored.plan, "fixture@example.com")
            .unwrap();
        let observation = inspect_saved(
            &mut fresh,
            &restored,
            "fixture@example.com",
            dir.path(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(matches!(
            observation.location,
            PackageTrashLocation::RenamedActive
        ));
        assert!(!observation.metadata_stale_refusal_recorded);
        assert_eq!(
            server.finish().await.len(),
            1,
            "inspection replayed a mutation"
        );
        let (_dir, owner, _vault, server) = fixture(Fault::None, 412, false, true).await;
        assert!(owner.execute(&CancellationToken::new()).await.is_err());
        assert!(server.finish().await.is_empty());
    })
    .await
    .unwrap();
}
