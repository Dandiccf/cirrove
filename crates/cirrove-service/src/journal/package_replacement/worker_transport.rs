#![allow(clippy::unwrap_used)]
use super::*;
use crate::{native_import::ValidatedPackageArchive, transfers::TransferWorker};
use cirrove_auth::CredentialVault;
use cirrove_core::{CancellationToken, upload::UploadRequest};
use cirrove_icloud::{ICloudFileReplace, ROOT_ID, SealedUploadCheckpointVault};
use secrecy::ExposeSecret;
const TRASH_ROOT: &str = "FOLDER::com.apple.CloudDocs::TRASH_ROOT";
use base64::Engine as _;
use cirrove_core::upload::PackageSemanticIdentity;
use cirrove_icloud::{PackageDownload, package_archive_semantic_identity_versioned};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::{
    io::Write,
    sync::{Arc, Mutex},
    time::Duration,
};
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
const FOLDER: &str = "FOLDER::com.apple.CloudDocs::owned";
const OLD: &str = "FILE::com.apple.CloudDocs::old";
const NEW: &str = "FILE::com.apple.CloudDocs::new";
fn archive(root: &str, old: bool, corrupt: bool) -> Vec<u8> {
    crate::native_import::synthetic_package_archive(
        &format!("{root}/Document"),
        if corrupt {
            b"divergent"
        } else if old {
            b"old owned content"
        } else {
            b"new owned content"
        },
    )
}
fn semantic(root: &str, old: bool, version: u32) -> PackageSemanticIdentity {
    let bytes = archive(root, old, false);
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(&bytes).unwrap();
    package_archive_semantic_identity_versioned(
        &file,
        &PackageDownload {
            size: bytes.len() as u64,
            sha256: hex::encode(Sha256::digest(&bytes)),
        },
        root,
        version,
        &CancellationToken::new(),
    )
    .unwrap()
}
struct Plan {
    target_name: String,
    staged_name: String,
}
#[derive(Default)]
struct State {
    registered: bool,
    allocations: usize,
    body_calls: usize,
    registrations: usize,
    trashed: bool,
    installed: bool,
    moved: bool,
    deleted: bool,
    changed: bool,
    corrupt: bool,
    collision: bool,
    trash_calls: usize,
    rename_calls: usize,
    requests: usize,
}
struct Server {
    state: Arc<Mutex<State>>,
    task: tokio::task::JoinHandle<()>,
    client: reqwest::Client,
    paused: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn entry(p: &Plan, s: &State, old: bool) -> serde_json::Value {
    let name = if old || s.installed {
        &p.target_name
    } else {
        &p.staged_name
    };
    json!({"drivewsid":if old{OLD}else{NEW},"docwsid":if old{"old"}else{"new"},"zone":"com.apple.CloudDocs","type":"FILE",
        "name":name.strip_suffix(".pages").unwrap(),"extension":"pages","size":17,
        "parentId":if old&&s.trashed{"TRASH_ROOT"}else if old&&s.moved{"FOLDER::com.apple.CloudDocs::elsewhere"}else{FOLDER},
        "restorePath":if old&&s.trashed{json!(["owned"])}else{json!(null)},
        "etag":if old&&s.changed{"changed"}else if old&&s.trashed{"trash-v2"}else if old{"old-v1"}else if s.installed{"new-v2"}else{"new-v1"}})
}
impl Server {
    async fn start(
        plan: Plan,
        source: Vec<u8>,
        lost_registration: bool,
        lost_trash: bool,
        lost_rename: bool,
        pause: Option<&'static str>,
    ) -> Self {
        let decoder = base64::engine::general_purpose::STANDARD;
        let cert = decoder
            .decode(
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../cirrove-icloud/src/package_create/fixtures/server-cert.b64"
                ))
                .trim(),
            )
            .unwrap();
        let key = decoder
            .decode(
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../cirrove-icloud/src/package_create/fixtures/server-key.b64"
                ))
                .trim(),
            )
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
        let state = Arc::new(Mutex::new(State::default()));
        let observed = state.clone();
        let paused = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let reached = paused.clone();
        let resume = release.clone();
        let task = tokio::spawn(async move {
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                tokio::time::timeout(Duration::from_secs(3),async {
                let mut stream=acceptor.accept(socket).await.unwrap();let mut raw=Vec::new();
                let (path,body)=loop {let mut buf=[0;4096];let n=stream.read(&mut buf).await.unwrap();assert!(n>0);raw.extend_from_slice(&buf[..n]);assert!(raw.len()<128*1024);
                    let Some(end)=raw.windows(4).position(|w|w==b"\r\n\r\n") else{continue};let head=std::str::from_utf8(&raw[..end]).unwrap();
                    let size=head.lines().find_map(|v|v.to_ascii_lowercase().strip_prefix("content-length: ").map(str::to_owned)).map(|v|v.parse::<usize>().unwrap()).unwrap_or(0);
                    if raw.len()>=end+4+size {break(head.lines().next().unwrap().split_whitespace().nth(1).unwrap().to_owned(),raw[end+4..end+4+size].to_vec())}
                };
                let mut reply={let mut s=observed.lock().unwrap();s.requests+=1;let url=reqwest::Url::parse(&format!("{ORIGIN}{path}")).unwrap();match url.path(){
                    "/retrieveItemDetailsInFolders"=>{let request:serde_json::Value=serde_json::from_slice(&body).unwrap();assert_eq!(request[0]["drivewsid"],FOLDER);let mut items=if s.registered{vec![entry(&plan,&s,false)]}else{vec![]};if !s.trashed&&!s.moved&&!s.deleted{items.push(entry(&plan,&s,true));}
                    if s.collision{let mut other=entry(&plan,&s,false);other["drivewsid"]=json!("FILE::com.apple.CloudDocs::foreign");other["name"]=json!("Target");items.push(other);}Some(json!([{"drivewsid":FOLDER,"parentId":ROOT_ID,"name":"Owned","zone":"com.apple.CloudDocs","type":"FOLDER","numberOfItems":items.len(),"items":items}]).to_string().into_bytes())},
                    "/ws/com.apple.CloudDocs/upload/web"=>{let r:serde_json::Value=serde_json::from_slice(&body).unwrap();assert_eq!(r["type"],"PACKAGE");assert_eq!(r["filename"],plan.staged_name);assert_eq!(r["size"],source.len());s.allocations+=1;assert_eq!(s.allocations,1);Some(json!([{"url":format!("{ORIGIN}/signed-upload"),"document_id":"new","owner_id":""}]).to_string().into_bytes())},
                    "/signed-upload"=>{assert_eq!(body,source);s.body_calls+=1;assert_eq!(s.body_calls,1);Some(json!({"hexBrSyntheticChecksum":"aa","ckSectionAssets":[{"hexFileChecksum":"bb","hexReferenceChecksum":"cc","hexWrappingKey":"dd","receiptToken":"YQ==","size":source.len()}]}).to_string().into_bytes())},
                    "/ws/com.apple.CloudDocs/update/documents"=>{let r:serde_json::Value=serde_json::from_slice(&body).unwrap();assert_eq!(r["command"],"add_package");assert_eq!(r["document_id"],"new");assert_eq!(r["path"]["path"],plan.staged_name);assert_eq!(r["path"]["starting_document_id"],"owned");assert_eq!(r["allow_conflict"],false);assert_eq!(s.body_calls,1);s.registrations+=1;assert_eq!(s.registrations,1);s.registered=true;if lost_registration{None}else{Some(json!({"status":{"status_code":0},"results":[{"status":{"status_code":0},"document":{"document_id":"new","item_id":"new-item","etag":"new-v1","size":17,"name":plan.staged_name}}]}).to_string().into_bytes())}},
                    "/retrieveItemDetails"=>{let request:serde_json::Value=serde_json::from_slice(&body).unwrap();let old=request["items"][0]["drivewsid"]==OLD;assert!(old||request["items"][0]["drivewsid"]==NEW);Some(json!({"items":if old&&s.deleted{vec![]}else{vec![entry(&plan,&s,old)]}}).to_string().into_bytes())},
                    "/ws/com.apple.CloudDocs/download/by_id"=>{let id=url.query_pairs().find(|(key,_)|key=="document_id").unwrap().1;assert!(id=="old"||id=="new");Some(json!({"package_token":{"url":format!("{ORIGIN}/archive/{id}")}}).to_string().into_bytes())},
                    "/archive/old"|"/archive/new"=>{let old=url.path().ends_with("old");let name=if old||s.installed{&plan.target_name}else{&plan.staged_name};Some(archive(name,old,s.corrupt))},
                    "/moveItemsToTrash"=>{let request:serde_json::Value=serde_json::from_slice(&body).unwrap();assert_eq!(request["items"][0]["drivewsid"],OLD);assert_eq!(request["items"][0]["etag"],"old-v1");s.trash_calls+=1;assert_eq!(s.trash_calls,1);assert!(!s.changed&&!s.moved&&!s.deleted);s.trashed=true;if lost_trash{None}else{Some(json!({"items":[{"status":"OK"}]}).to_string().into_bytes())}},
                    "/renameItems"=>{let request:serde_json::Value=serde_json::from_slice(&body).unwrap();assert_eq!(request["items"][0]["drivewsid"],NEW);assert_eq!(request["items"][0]["etag"],"new-v1");assert_eq!(request["items"][0]["name"],"Target.pages");s.rename_calls+=1;assert_eq!(s.rename_calls,1);assert!(s.trashed);s.installed=true;if lost_rename{None}else{Some(json!({"items":[{"status":"OK"}]}).to_string().into_bytes())}},
                    _=>panic!("unexpected synthetic native handoff route"),
                }};
                if pause.is_some_and(|route|path.split('?').next()==Some(route)) {reached.notify_one();resume.notified().await;reply=None;}
                if let Some(reply)=reply{stream.write_all(format!("HTTP/1.1 200 Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",reply.len()).as_bytes()).await.unwrap();stream.write_all(&reply).await.unwrap();stream.shutdown().await.unwrap();}
            }).await.unwrap();
            }
        });
        Self {
            state,
            task,
            client,
            paused,
            release,
        }
    }
}

fn directory() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/var/tmp")
        .unwrap()
}
fn original() -> Node {
    Node {
        id: OLD.into(),
        parent_id: Some(FOLDER.into()),
        name: "Target.pages".into(),
        kind: NodeKind::Folder,
        size: 17,
        modified_unix: 0,
        etag: Some("old-v1".into()),
        content_version: None,
        target: None,
        package: true,
    }
}
fn parent() -> Node {
    Node {
        id: FOLDER.into(),
        parent_id: Some(ROOT_ID.into()),
        name: "Owned".into(),
        kind: NodeKind::Folder,
        size: 0,
        modified_unix: 0,
        etag: None,
        content_version: None,
        target: None,
        package: false,
    }
}
fn request(row: &UploadRecord) -> UploadRequest {
    UploadRequest {
        scope: row.scope.clone(),
        intent: row.intent.clone(),
        size: row.size,
        sha256: row.sha256.clone(),
        representation: row.representation.clone(),
    }
}
fn provider(server: &Server, staging: &Path, row: &UploadRecord) -> Arc<ICloudFileReplace> {
    Arc::new(
        ICloudFileReplace::synthetic_native_package(
            request(row),
            parent(),
            row.id,
            staging,
            server.client.clone(),
        )
        .unwrap(),
    )
}
#[derive(Default)]
struct WrappingKeys(Mutex<std::collections::BTreeMap<String, secrecy::SecretString>>);
#[async_trait::async_trait]
impl CredentialVault for WrappingKeys {
    async fn load(&self, key: &str) -> anyhow::Result<Option<secrecy::SecretString>> {
        Ok(self.0.lock().unwrap().get(key).cloned())
    }
    async fn save(&self, key: &str, value: secrecy::SecretString) -> anyhow::Result<()> {
        // This store must receive wrapping keys only, never checkpoint text.
        assert!(value.expose_secret().starts_with("icloud-seal-v1:"));
        assert!(value.expose_secret().len() < 256);
        self.0.lock().unwrap().insert(key.into(), value);
        Ok(())
    }
    async fn remove(&self, key: &str) -> anyhow::Result<()> {
        self.0.lock().unwrap().remove(key);
        Ok(())
    }
}
struct Arm {
    root: tempfile::TempDir,
    staging: tempfile::TempDir,
    state: tempfile::TempDir,
    row: UploadRecord,
    bytes: Vec<u8>,
    keys: Arc<WrappingKeys>,
}
impl Arm {
    fn create() -> Self {
        let root = directory();
        let staging = directory();
        let state = directory();
        let scope = Scope {
            account: Uuid::new_v4().to_string(),
            provider: "icloud".into(),
            collection: "drive".into(),
        };
        let bytes = archive("Source.pages", false, false);
        let source = staging.path().join("edited.pages");
        {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&source)
                .unwrap();
            f.write_all(&bytes).unwrap();
            f.sync_all().unwrap();
        }
        let validated = ValidatedPackageArchive::capture(
            &source,
            staging.path(),
            "Source.pages",
            &CancellationToken::new(),
        )
        .unwrap();
        let mut journal = UploadJournal::open(root.path(), &scope.account, 1024 * 1024).unwrap();
        let row = journal
            .enqueue_validated_package_replacement(
                scope,
                original(),
                semantic("Target.pages", true, 1),
                validated,
                &CancellationToken::new(),
            )
            .unwrap();
        drop(journal);
        Self {
            root,
            staging,
            state,
            row,
            bytes,
            keys: Arc::new(WrappingKeys::default()),
        }
    }
    fn journal(&self) -> UploadJournal {
        UploadJournal::open(self.root.path(), &self.row.scope.account, 1024 * 1024).unwrap()
    }
    fn vault(&self) -> Arc<SealedUploadCheckpointVault> {
        Arc::new(
            SealedUploadCheckpointVault::with_test_key_vault(
                self.state.path(),
                &self.row.scope.account,
                self.keys.clone(),
            )
            .unwrap(),
        )
    }
    fn plan(&self) -> Plan {
        Plan {
            target_name: "Target.pages".into(),
            staged_name: format!("staged-by-cirrove-{}.pages", self.row.id),
        }
    }
    fn retained_export(&self, suffix: usize) {
        let recovery = RecoveryJournal::open(self.root.path(), &self.row.scope.account).unwrap();
        let before = serde_json::to_value(recovery.list(0, 10).unwrap()).unwrap();
        let export = self.staging.path().join(format!("recovery-{suffix}.zip"));
        let receipt = recovery
            .local_export_source(self.row.id)
            .unwrap()
            .copy_to(&export, &CancellationToken::new(), |_| {})
            .unwrap();
        assert_eq!(receipt.sha256, self.row.sha256);
        assert_eq!(std::fs::read(export).unwrap(), self.bytes);
        assert_eq!(
            serde_json::to_value(recovery.list(0, 10).unwrap()).unwrap(),
            before
        );
    }
    async fn sealed_checkpoint(&self, expected_phase: &str) {
        let key = format!("upload/{}", self.row.id);
        assert!(self.keys.0.lock().unwrap().contains_key(&key));
        let secret = self
            .vault()
            .load(&key)
            .await
            .unwrap()
            .expect("durable checkpoint");
        assert!(secret.expose_secret().len() <= 96 * 1024);
        let path = self
            .state
            .path()
            .join("accounts")
            .join(&self.row.scope.account)
            .join("upload-checkpoints")
            .join(self.row.id.to_string())
            .join("checkpoint.sealed");
        let encrypted = std::fs::read(path).unwrap();
        for forbidden in [
            b"fixture.icloud-content.com".as_slice(),
            b"account_hash".as_slice(),
            b"PackageReplacementArchive".as_slice(),
        ] {
            assert!(!encrypted.windows(forbidden.len()).any(|v| v == forbidden));
        }
        let saved: serde_json::Value = serde_json::from_str(secret.expose_secret()).unwrap();
        assert_eq!(saved["operation"], self.row.id.to_string());
        if expected_phase == "registration" {
            assert_eq!(
                saved["phase"]["Stage"]["inner"]["phase"],
                "RegistrationArmed"
            );
        } else {
            assert_eq!(saved["phase"]["Handoff"]["phase"], expected_phase);
        }
    }
    fn complete(&self) {
        let journal = self.journal();
        let row = journal.get(self.row.id).unwrap();
        assert_eq!(row.state, UploadState::Uploaded);
        let remote = row.remote.unwrap();
        assert_eq!(remote.id, NEW);
        assert_eq!(remote.name, "Target.pages");
        assert_eq!(remote.size, 17);
        assert!(remote.package);
        assert_eq!(
            row.package_completion,
            Some(semantic("Source.pages", false, 2))
        );
        let backup = row
            .identity_handoff
            .as_ref()
            .and_then(|r| r.backup.as_ref())
            .expect("old identity receipt");
        assert_eq!(backup.id, OLD);
        assert_eq!(backup.parent_id.as_deref(), Some(TRASH_ROOT));
        assert_eq!(
            std::fs::read(journal.objects.join(row.id.to_string())).unwrap(),
            self.bytes
        );
    }
}
fn counts(server: &Server) -> (usize, usize, usize, usize, usize) {
    let s = server.state.lock().unwrap();
    (
        s.allocations,
        s.body_calls,
        s.registrations,
        s.trash_calls,
        s.rename_calls,
    )
}
async fn pass(arm: &Arm, server: &Server, retry: bool) -> UploadState {
    let mut journal = arm.journal();
    if retry {
        journal.request_retry(arm.row.id).unwrap();
    }
    let journal = Arc::new(Mutex::new(journal));
    let worker = TransferWorker::new(
        journal.clone(),
        provider(server, arm.staging.path(), &arm.row),
        arm.vault(),
        CancellationToken::new(),
    );
    let result = tokio::time::timeout(Duration::from_secs(30), worker.run_once())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    drop(worker);
    drop(journal);
    result.state
}
#[tokio::test]
async fn native_coordinator_real_worker_reopens_sealed_checkpoints_after_each_lost_reply() {
    let arm = Arm::create();
    let server = Server::start(arm.plan(), arm.bytes.clone(), true, true, true, None).await;
    for attempt in 0..3 {
        assert_eq!(
            pass(&arm, &server, attempt > 0).await,
            UploadState::VerifyRequired
        );
        arm.sealed_checkpoint(match attempt {
            0 => "registration",
            1 => "move_old",
            _ => "install_inspected",
        })
        .await;
        arm.retained_export(attempt);
        let expected = match attempt {
            0 => (1, 1, 1, 0, 0),
            1 => (1, 1, 1, 1, 0),
            _ => (1, 1, 1, 1, 1),
        };
        assert_eq!(counts(&server), expected);
    }
    assert_eq!(pass(&arm, &server, true).await, UploadState::Uploaded);
    arm.complete();
    assert_eq!(counts(&server), (1, 1, 1, 1, 1));
    assert!(
        arm.vault()
            .load(&format!("upload/{}", arm.row.id))
            .await
            .unwrap()
            .is_none()
    );
    assert!(arm.keys.0.lock().unwrap().is_empty());
}
#[tokio::test]
async fn native_coordinator_worker_abort_and_cancellation_keep_armed_checkpoint_without_replay() {
    for abort in [true, false] {
        let arm = Arm::create();
        let server = Server::start(
            arm.plan(),
            arm.bytes.clone(),
            false,
            false,
            false,
            Some("/moveItemsToTrash"),
        )
        .await;
        let journal = Arc::new(Mutex::new(arm.journal()));
        let cancel = CancellationToken::new();
        let worker = TransferWorker::new(
            journal.clone(),
            provider(&server, arm.staging.path(), &arm.row),
            arm.vault(),
            cancel.clone(),
        );
        let task = tokio::spawn(async move { worker.run_once().await });
        tokio::time::timeout(Duration::from_secs(20), server.paused.notified())
            .await
            .unwrap();
        assert_eq!(counts(&server), (1, 1, 1, 1, 0));
        if abort {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        } else {
            cancel.cancel();
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(2), task)
                    .await
                    .expect("cancel before held response release")
                    .unwrap()
                    .unwrap()
                    .unwrap()
                    .state,
                UploadState::VerifyRequired
            );
        }
        drop(journal);
        server.release.notify_one();
        arm.sealed_checkpoint("move_old").await;
        arm.retained_export(0);
        assert_eq!(pass(&arm, &server, true).await, UploadState::Uploaded);
        arm.complete();
        assert_eq!(counts(&server), (1, 1, 1, 1, 1));
    }
}
