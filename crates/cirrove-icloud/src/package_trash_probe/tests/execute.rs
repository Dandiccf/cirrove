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
    FailEvidence,
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
        if matches!(self.fault, Fault::FailEvidence)
            && saved.phase == PackageTrashPhase::StaleTrashArmed
            && saved.revision_refusal.is_some()
        {
            anyhow::bail!("injected evidence save failure");
        }
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
    archive_with_content(root, b"owned synthetic native document")
}
fn archive_with_content(root: &str, content: &[u8]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    zip.start_file(
        format!("{root}/Index/Document.iwa"),
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    zip.write_all(content).unwrap();
    zip.finish().unwrap().into_inner()
}
struct Remote {
    restore_path: Option<serde_json::Value>,
    change_after_stale: bool,
    corrupt_after_stale: bool,
    content_changed: bool,
    stale_reply: Option<serde_json::Value>,
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
            restore_path: None,
            change_after_stale: false,
            corrupt_after_stale: false,
            content_changed: false,
            stale_reply: None,
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
                                let mut item=serde_json::to_value(&state.current).unwrap();if state.trashed{item["parentId"]=json!("TRASH_ROOT");item["restorePath"]=state.restore_path.clone().unwrap_or_else(||json!([plan.parent]));}
                                Some((200,serde_json::to_vec(&json!({"items":[item]})).unwrap()))
                            },
                            ("GET","/ws/com.apple.CloudDocs/download/by_id")=>{
                                assert!(path.ends_with("document_id=allocated"));Some((200,serde_json::to_vec(&json!({"package_token":{"url":format!("{ORIGIN}/content")}})).unwrap()))
                            },
                            ("GET","/content")=>Some((200,if state.content_changed {archive_with_content(&state.current.display_name(), b"different synthetic native content")}else{archive(&state.current.display_name())})),
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
                                if stale && state.change_after_stale { state.current.etag = "E2".into(); }
                                if stale && state.corrupt_after_stale { state.content_changed = true; }
                                if stale{Some((stale_status,state.stale_reply.as_ref().map(|reply|serde_json::to_vec(reply).unwrap()).unwrap_or_else(||br#"{"items":[{"status":"OTHER_REFUSAL"}]}"#.to_vec())))}
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
    saved.version = 2;
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
                    "package stale Trash lacks explicit revision refusal: {} {{ http_status: {status} }}",
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

#[tokio::test]
async fn package_trash_execute_bound_etag_conflict_proves_recovery_and_rejects_unbound_receipts() {
    tokio::time::timeout(Duration::from_secs(30), async {
        for mismatch in ["none", "id", "parent", "etag", "missing-id", "missing-parent", "missing-etag", "old-etag", "duplicate", "generic-conflict"] {
            let (dir, owner, vault, server) = fixture(Fault::None, 200, false, false).await;
            let mut item = json!({
                "status": "ETAG_CONFLICT",
                "drivewsid": owner.saved.imported.drivewsid,
                "parentId": owner.saved.plan.parent,
                "etag": "E1"
            });
            match mismatch {
                "none" | "duplicate" => (),
                "generic-conflict" => item["status"] = json!("CONFLICT"),
                "id" => item["drivewsid"] = json!("FILE::com.apple.CloudDocs::foreign"),
                "parent" => item["parentId"] = json!("FOLDER::com.apple.CloudDocs::foreign"),
                "etag" => item["etag"] = json!("foreign-E1"),
                "old-etag" => item["etag"] = json!("E0"),
                "missing-id" => { item.as_object_mut().unwrap().remove("drivewsid"); },
                "missing-parent" => { item.as_object_mut().unwrap().remove("parentId"); },
                "missing-etag" => { item.as_object_mut().unwrap().remove("etag"); },
                _ => unreachable!(),
            }
            let reply = if mismatch == "duplicate" {
                json!({"items": [item.clone(), item]})
            } else {
                json!({"items": [item]})
            };
            server.state.lock().unwrap().stale_reply = Some(reply);
            let outcome = owner.execute(&CancellationToken::new()).await;
            if mismatch == "none" {
                let result = outcome.unwrap();
                let evidence = Some(PackageTrashRefusal::ItemEtagConflict { http_status: 200 });
                assert_eq!(result.revision_refusal, evidence);
                assert!(result.metadata_stale_refusal_recorded && result.current_trash_semantic_recovery_verified);
                let restored: Checkpoint = serde_json::from_str(vault.latest.lock().unwrap().as_deref().unwrap()).unwrap();
                assert_eq!(restored.revision_refusal, evidence);
                let mut fresh = ICloudReadSession::new().unwrap();
                fresh.http = server.client.clone();
                fresh.drive_endpoint = Some(format!("{ORIGIN}/").parse().unwrap());
                fresh.docs_endpoint = fresh.drive_endpoint.clone();
                fresh.account_hash = Some(restored.account_hash.clone());
                restored.validate(&fresh, &restored.plan, "fixture@example.com").unwrap();
                let observed = inspect_saved(&mut fresh, &restored, "fixture@example.com", dir.path(), &CancellationToken::new()).await.unwrap();
                {
                    let mut remote = server.state.lock().unwrap();
                    remote.restore_path = Some(json!(format!("{}/{}", restored.plan.parent_name, restored.renamed_name())));
                }
                let shape = restore_shape::observe(&mut fresh, &restored, "fixture@example.com", dir.path(), &CancellationToken::new()).await.unwrap();
                assert!(shape.restore_path.matches_owned_relative_form);
                assert!(shape.exact_trash_metadata_stable && shape.semantic_recovery_verified && shape.owned_parent_stable_and_vacant);
                assert!(!shape.restore_authorized);
                server.state.lock().unwrap().restore_path = Some(json!(["unclassified", "array"]));
                let unknown = restore_shape::observe(&mut fresh, &restored, "fixture@example.com", dir.path(), &CancellationToken::new()).await.unwrap();
                assert_eq!(unknown.restore_path.json_type, "array");
                assert!(!unknown.restore_path.matches_owned_relative_form && !unknown.restore_authorized);
                server.state.lock().unwrap().current.drivewsid = "FILE::com.apple.CloudDocs::foreign".into();
                assert!(restore_shape::observe(&mut fresh, &restored, "fixture@example.com", dir.path(), &CancellationToken::new()).await.is_err());
                assert_eq!(observed.revision_refusal, evidence);
                assert!(observed.metadata_stale_refusal_recorded && observed.current_trash_semantic_recovery_verified);
                assert_eq!(server.finish().await, vec![PackageTrashPhase::RenameArmed, PackageTrashPhase::StaleTrashArmed, PackageTrashPhase::CurrentTrashArmed]);
                continue;
            }
            let error = outcome.err().unwrap().to_string();
            let expected = match mismatch {
                "duplicate" => "iCloud conditional Trash receipt is incomplete",
                "generic-conflict" => "package stale Trash lacks explicit revision refusal: Conflict { http_status: 200 }",
                _ => "package stale Trash lacks explicit revision refusal: Rejected { http_status: 200 }",
            };
            assert_eq!(error, expected, "{mismatch}");
            assert_eq!(vault.phase(), PackageTrashPhase::StaleTrashArmed);
            assert!(!server.state.lock().unwrap().trashed);
            assert_eq!(server.finish().await, vec![PackageTrashPhase::RenameArmed, PackageTrashPhase::StaleTrashArmed]);
        }
    }).await.unwrap();
}

#[tokio::test]
async fn package_trash_bound_refusal_requires_durable_evidence_and_unchanged_active_revision() {
    tokio::time::timeout(Duration::from_secs(15), async {
        for (fault, changed, corrupted) in [
            (Fault::FailEvidence, false, false),
            (Fault::None, true, false),
            (Fault::None, false, true),
        ] {
            let (_dir, owner, vault, server) = fixture(fault, 200, false, false).await;
            {
                let mut remote = server.state.lock().unwrap();
                remote.change_after_stale = changed;
                remote.corrupt_after_stale = corrupted;
                remote.stale_reply = Some(json!({"items": [{
                    "status": "ETAG_CONFLICT", "drivewsid": owner.saved.imported.drivewsid,
                    "parentId": owner.saved.plan.parent, "etag": "E1"
                }]}));
            }
            let error = owner
                .execute(&CancellationToken::new())
                .await
                .err()
                .expect("must stop");
            if !changed && !corrupted {
                assert_eq!(error.to_string(), "injected evidence save failure");
            }
            let restored: Checkpoint =
                serde_json::from_str(vault.latest.lock().unwrap().as_deref().unwrap()).unwrap();
            assert_eq!(restored.phase, PackageTrashPhase::StaleTrashArmed);
            assert_eq!(restored.revision_refusal.is_some(), changed || corrupted);
            assert!(
                !inspection(&restored, PackageTrashLocation::RenamedActive)
                    .metadata_stale_refusal_recorded
            );
            assert!(!server.state.lock().unwrap().trashed);
            assert_eq!(
                server.finish().await,
                vec![
                    PackageTrashPhase::RenameArmed,
                    PackageTrashPhase::StaleTrashArmed
                ]
            );
        }
    })
    .await
    .unwrap();
}
