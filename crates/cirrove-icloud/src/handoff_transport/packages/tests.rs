#![allow(clippy::unwrap_used)]
use super::*;
use base64::Engine as _;
use std::os::unix::fs::PermissionsExt;
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
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    zip.start_file(
        format!("{root}/Document"),
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    zip.write_all(if corrupt {
        b"divergent"
    } else if old {
        b"old owned content"
    } else {
        b"new owned content"
    })
    .unwrap();
    zip.finish().unwrap().into_inner()
}
fn semantic(root: &str, old: bool) -> PackageSemanticIdentity {
    let bytes = archive(root, old, false);
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(&bytes).unwrap();
    package_archive_semantic_identity(
        &file,
        &PackageDownload {
            size: bytes.len() as u64,
            sha256: hex::encode(Sha256::digest(&bytes)),
        },
        root,
        &CancellationToken::new(),
    )
    .unwrap()
}
fn plan() -> HandoffPlan {
    let staged_name = format!("staged-by-cirrove-{}.pages", Uuid::new_v4());
    HandoffPlan {
        version: 6,
        folder_parent_id: ROOT_ID.into(),
        folder_id: FOLDER.into(),
        folder_name: "Owned".into(),
        original_id: OLD.into(),
        original_doc_id: "old".into(),
        original_etag: "old-v1".into(),
        staged_id: NEW.into(),
        staged_doc_id: "new".into(),
        staged_etag: "new-v1".into(),
        recovery_name: format!("recovery-by-cirrove-{}.pages", Uuid::new_v4()),
        target_name: "Target.pages".into(),
        original_sha256: String::new(),
        staged_sha256: String::new(),
        package: Some(NativePackageProof {
            version: 1,
            account_hash: account_hash("fixture@example.com").unwrap(),
            original: semantic("Target.pages", true),
            staged: semantic(&staged_name, false),
            original_size: 17,
            staged_size: 17,
        }),
        staged_name,
    }
}
#[derive(Default)]
struct State {
    trashed: bool,
    installed: bool,
    moved: bool,
    deleted: bool,
    changed: bool,
    corrupt: bool,
    collision: bool,
    recovery_collision: bool,
    trash_calls: usize,
    rename_calls: usize,
    requests: usize,
}
struct Server {
    state: Arc<Mutex<State>>,
    task: tokio::task::JoinHandle<()>,
    client: reqwest::Client,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn entry(p: &HandoffPlan, s: &State, old: bool) -> serde_json::Value {
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
    async fn start(plan: HandoffPlan, lost_trash: bool, lost_rename: bool) -> Self {
        let decoder = base64::engine::general_purpose::STANDARD;
        let cert = decoder
            .decode(include_str!("../../package_create/fixtures/server-cert.b64").trim())
            .unwrap();
        let key = decoder
            .decode(include_str!("../../package_create/fixtures/server-key.b64").trim())
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
                let reply={let mut s=observed.lock().unwrap();s.requests+=1;let url=url::Url::parse(&format!("{ORIGIN}{path}")).unwrap();match url.path(){
                    "/retrieveItemDetailsInFolders"=>{let request:serde_json::Value=serde_json::from_slice(&body).unwrap();assert_eq!(request[0]["drivewsid"],FOLDER);let mut items=vec![entry(&plan,&s,false)];if !s.trashed&&!s.moved&&!s.deleted{items.push(entry(&plan,&s,true));}
                    if s.collision{let mut other=entry(&plan,&s,false);other["drivewsid"]=json!("FILE::com.apple.CloudDocs::foreign");other["name"]=json!("Target");items.push(other);}
                    if s.recovery_collision{let mut other=entry(&plan,&s,false);other["drivewsid"]=json!("FILE::com.apple.CloudDocs::recovery-occupant");other["docwsid"]=json!("recovery-occupant");other["name"]=json!(plan.recovery_name.strip_suffix(".pages").unwrap());other["etag"]=json!("occupant-v1");items.push(other);}Some(json!([{"drivewsid":FOLDER,"parentId":ROOT_ID,"name":"Owned","type":"FOLDER","numberOfItems":items.len(),"items":items}]).to_string().into_bytes())},
                    "/retrieveItemDetails"=>{let request:serde_json::Value=serde_json::from_slice(&body).unwrap();let old=request["items"][0]["drivewsid"]==OLD;assert!(old||request["items"][0]["drivewsid"]==NEW);Some(json!({"items":if old&&s.deleted{vec![]}else{vec![entry(&plan,&s,old)]}}).to_string().into_bytes())},
                    "/ws/com.apple.CloudDocs/download/by_id"=>{let id=url.query_pairs().find(|(key,_)|key=="document_id").unwrap().1;assert!(id=="old"||id=="new");Some(json!({"package_token":{"url":format!("{ORIGIN}/archive/{id}")}}).to_string().into_bytes())},
                    "/archive/old"|"/archive/new"=>{let old=url.path().ends_with("old");let name=if old||s.installed{&plan.target_name}else{&plan.staged_name};Some(archive(name,old,s.corrupt))},
                    "/moveItemsToTrash"=>{let request:serde_json::Value=serde_json::from_slice(&body).unwrap();assert_eq!(request["items"][0]["drivewsid"],OLD);assert_eq!(request["items"][0]["etag"],"old-v1");s.trash_calls+=1;assert_eq!(s.trash_calls,1);assert!(!s.changed&&!s.moved&&!s.deleted);s.trashed=true;if lost_trash{None}else{Some(json!({"items":[{"status":"OK"}]}).to_string().into_bytes())}},
                    "/renameItems"=>{let request:serde_json::Value=serde_json::from_slice(&body).unwrap();assert_eq!(request["items"][0]["drivewsid"],NEW);assert_eq!(request["items"][0]["etag"],"new-v1");assert_eq!(request["items"][0]["name"],"Target.pages");s.rename_calls+=1;assert_eq!(s.rename_calls,1);assert!(s.trashed);s.installed=true;if lost_rename{None}else{Some(json!({"items":[{"status":"OK"}]}).to_string().into_bytes())}},
                    _=>panic!("unexpected synthetic native handoff route"),
                }};
                if let Some(reply)=reply{stream.write_all(format!("HTTP/1.1 200 Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",reply.len()).as_bytes()).await.unwrap();stream.write_all(&reply).await.unwrap();stream.shutdown().await.unwrap();}
            }).await.unwrap();
            }
        });
        Self {
            state,
            task,
            client,
        }
    }
    fn session(&self) -> ICloudReadSession {
        let mut session = ICloudReadSession::new().unwrap();
        session.account_hash = Some(account_hash("fixture@example.com").unwrap());
        session.http = self.client.clone();
        session.drive_endpoint = Some(format!("{ORIGIN}/").parse().unwrap());
        session.docs_endpoint = session.drive_endpoint.clone();
        session
    }
}
fn directory() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}
#[tokio::test]
async fn native_handoff_existing_stage_transport_and_lost_reply_reconciliation() {
    for lost in [false, true] {
        let plan = plan();
        let server = Server::start(plan.clone(), lost, lost).await;
        let dir = directory();
        let cancel = CancellationToken::new();
        let mut session = server.session();
        assert_eq!(
            session
                .inspect_native_package_handoff(&plan, dir.path(), &cancel)
                .await
                .unwrap(),
            HandoffObserved::Prepared
        );
        let trash = session
            .trash_native_handoff_original(&plan, dir.path(), &cancel)
            .await;
        if lost {
            assert!(trash.is_err());
        } else {
            assert!(trash.unwrap());
        }
        drop(session);
        let mut session = server.session();
        assert_eq!(
            session
                .inspect_native_package_handoff(&plan, dir.path(), &cancel)
                .await
                .unwrap(),
            HandoffObserved::OldAtRecovery
        );
        assert!(
            !session
                .trash_native_handoff_original(&plan, dir.path(), &cancel)
                .await
                .unwrap()
        );
        let rename = session
            .install_native_handoff_stage(&plan, dir.path(), &cancel)
            .await;
        if lost {
            assert!(rename.is_err());
        } else {
            assert!(rename.unwrap());
        }
        drop(session);
        let mut session = server.session();
        let (current, backup) = session
            .verified_native_handoff_nodes(&plan, dir.path(), &cancel)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(current.id, NEW);
        assert_eq!(backup.id, OLD);
        assert!(current.package && backup.package);
        assert_eq!(current.kind, NodeKind::Folder);
        assert_eq!(current.content_version, None);
        assert_eq!(
            backup.parent_id.as_deref(),
            Some(crate::write_transport::TRASH_ROOT)
        );
        assert!(
            !session
                .install_native_handoff_stage(&plan, dir.path(), &cancel)
                .await
                .unwrap()
        );
        let state = server.state.lock().unwrap();
        assert_eq!((state.trash_calls, state.rename_calls), (1, 1));
    }
}
#[tokio::test]
async fn native_handoff_original_divergence_after_stage_never_mutates() {
    for fault in 0..5 {
        let plan = plan();
        let server = Server::start(plan.clone(), false, false).await;
        let dir = directory();
        {
            let mut state = server.state.lock().unwrap();
            match fault {
                0 => state.changed = true,
                1 => state.moved = true,
                2 => state.deleted = true,
                3 => state.corrupt = true,
                _ => state.collision = true,
            }
        }
        let result = server
            .session()
            .trash_native_handoff_original(&plan, dir.path(), &CancellationToken::new())
            .await;
        assert!(!matches!(result, Ok(true)));
        let state = server.state.lock().unwrap();
        assert_eq!((state.trash_calls, state.rename_calls), (0, 0));
    }
}
#[tokio::test]
async fn native_handoff_binding_cancellation_and_ordinary_boundary_are_local() {
    let plan = plan();
    let server = Server::start(plan.clone(), false, false).await;
    let dir = directory();
    let mut session = server.session();
    let mut wrong = plan.clone();
    wrong.package.as_mut().unwrap().account_hash = "f".repeat(64);
    assert!(
        session
            .inspect_native_package_handoff(&wrong, dir.path(), &CancellationToken::new())
            .await
            .is_err()
    );
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(
        session
            .inspect_native_package_handoff(&plan, dir.path(), &cancel)
            .await
            .is_err()
    );
    assert!(session.inspect_durable_handoff(&plan).await.is_err());
    assert_eq!(server.state.lock().unwrap().requests, 0);
}
#[test]
fn native_handoff_proof_is_explicit_and_legacy_deserialization_preserved() {
    let plan = plan();
    assert!(plan.validate().is_ok());
    let mut wrong = plan.clone();
    wrong.package = None;
    assert!(wrong.validate().is_err());
    let mut wrong = plan.clone();
    wrong.version = 5;
    assert!(wrong.validate().is_err());
    let mut wrong = plan.clone();
    wrong.package.as_mut().unwrap().version = 2;
    assert!(wrong.validate().is_err());
    let mut wrong = plan.clone();
    wrong.original_sha256 = "a".repeat(64);
    assert!(wrong.validate().is_err());
    let mut legacy = plan.clone();
    legacy.version = 4;
    legacy.package = None;
    legacy.original_sha256 = "a".repeat(64);
    legacy.staged_sha256 = "b".repeat(64);
    legacy.staged_name = legacy.staged_name.replace(".pages", ".txt");
    legacy.recovery_name = legacy.recovery_name.replace(".pages", ".txt");
    legacy.folder_id = ROOT_ID.into();
    legacy.folder_parent_id.clear();
    let mut value = serde_json::to_value(&legacy).unwrap();
    value.as_object_mut().unwrap().remove("package");
    let decoded: HandoffPlan = serde_json::from_value(value).unwrap();
    assert!(decoded.package.is_none());
    assert!(decoded.validate().is_ok());
}

#[tokio::test]
async fn native_handoff_both_semantic_proofs_are_enforced_before_mutation() {
    for original in [false, true] {
        let plan = plan();
        let server = Server::start(plan.clone(), false, false).await;
        let dir = directory();
        let mut mismatched = plan.clone();
        let proof = mismatched.package.as_mut().unwrap();
        if original {
            proof.original.sha256 = "0".repeat(64);
        } else {
            proof.staged.sha256 = "0".repeat(64);
        }
        assert!(
            server
                .session()
                .trash_native_handoff_original(&mismatched, dir.path(), &CancellationToken::new())
                .await
                .is_err()
        );
        let state = server.state.lock().unwrap();
        assert!(state.requests > 0);
        assert_eq!((state.trash_calls, state.rename_calls), (0, 0));
    }
}

#[tokio::test]
async fn native_handoff_recovery_name_occupant_preserves_all_identities() {
    let plan = plan();
    let server = Server::start(plan.clone(), false, false).await;
    let dir = directory();
    let cancel = CancellationToken::new();
    {
        let mut state = server.state.lock().unwrap();
        state.recovery_collision = true;
        // This arm changes only the foreign recovery-name occupant.
        assert!(!state.trashed && !state.installed && !state.moved && !state.deleted);
        assert!(!state.changed && !state.corrupt && !state.collision);
    }
    let mut session = server.session();
    let before = session.list_folder(FOLDER).await.unwrap();
    assert_eq!(before.len(), 3);
    let original = before.iter().find(|entry| entry.drivewsid == OLD).unwrap();
    let staged = before.iter().find(|entry| entry.drivewsid == NEW).unwrap();
    let occupant = before
        .iter()
        .find(|entry| entry.drivewsid == "FILE::com.apple.CloudDocs::recovery-occupant")
        .unwrap();
    assert_eq!(original.display_name(), plan.target_name);
    assert_eq!(original.etag, plan.original_etag);
    assert_eq!(staged.display_name(), plan.staged_name);
    assert_eq!(staged.etag, plan.staged_etag);
    assert_eq!(occupant.display_name(), plan.recovery_name);
    assert_eq!(occupant.parent_id, FOLDER);
    assert_eq!(occupant.etag, "occupant-v1");
    assert_ne!(occupant.drivewsid, original.drivewsid);
    assert_ne!(occupant.drivewsid, staged.drivewsid);
    assert_eq!(original.parent_id, FOLDER);
    assert_eq!(staged.parent_id, FOLDER);
    assert_eq!(original.kind, "FILE");
    assert_eq!(staged.kind, "FILE");
    // Observe all valid actors before the desired refusal assertion.
    let before = serde_json::to_value(before).unwrap();
    let result = session
        .trash_native_handoff_original(&plan, dir.path(), &cancel)
        .await;
    assert!(
        matches!(result, Ok(false)),
        "a foreign recovery-name occupant must refuse before Trash"
    );
    let after = serde_json::to_value(session.list_folder(FOLDER).await.unwrap()).unwrap();
    assert_eq!(
        after, before,
        "original, stage and occupant metadata changed"
    );
    let state = server.state.lock().unwrap();
    assert_eq!((state.trash_calls, state.rename_calls), (0, 0));
    assert!(!state.trashed && !state.installed && !state.moved && !state.deleted);
    assert!(!state.changed && !state.corrupt && !state.collision);
    assert!(state.recovery_collision);
}
