#![allow(clippy::unwrap_used)]
use super::*;
use crate::{PackageDownload, account_hash, package_archive_semantic_identity};
use base64::Engine as _;
use cirrove_core::upload::PackageSemanticIdentity;
use serde_json::json;
use sha2::{Digest, Sha256};
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
        if root.is_empty() {
            "Document".into()
        } else {
            format!("{root}/Document")
        },
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
fn semantic_v2(root: &str, old: bool) -> PackageSemanticIdentity {
    let bytes = archive(root, old, false);
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(&bytes).unwrap();
    let receipt = PackageDownload {
        size: bytes.len() as u64,
        sha256: hex::encode(Sha256::digest(&bytes)),
    };
    if root.is_empty() {
        crate::package_flat_archive_semantic_identity_v2(&file, &receipt, &CancellationToken::new())
            .unwrap()
    } else {
        crate::package_archive_semantic_identity_versioned(
            &file,
            &receipt,
            root,
            2,
            &CancellationToken::new(),
        )
        .unwrap()
    }
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
    archive_calls: usize,
    change_after_stage_lookup: bool,
    wrong_stage_identity: bool,
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
    let (basename, extension) = name.rsplit_once('.').unwrap();
    json!({"drivewsid":if old{OLD}else{NEW},"docwsid":if old{"old"}else{"new"},"zone":"com.apple.CloudDocs","type":"FILE",
        "name":basename,"extension":extension,"size":17,
        "parentId":if old&&s.trashed{"TRASH_ROOT"}else if old&&s.moved{"FOLDER::com.apple.CloudDocs::elsewhere"}else{FOLDER},
        "restorePath":if old&&s.trashed{json!(["owned"])}else{json!(null)},
        "etag":if old&&s.changed{"changed"}else if old&&s.trashed{"trash-v2"}else if old{"old-v1"}else if s.installed{"new-v2"}else{"new-v1"}})
}
impl Server {
    async fn start(
        plan: HandoffPlan,
        source: Vec<u8>,
        lost_registration: bool,
        lost_trash: bool,
        lost_rename: bool,
    ) -> Self {
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
                    "/retrieveItemDetailsInFolders"=>{let request:serde_json::Value=serde_json::from_slice(&body).unwrap();assert_eq!(request[0]["drivewsid"],FOLDER);let mut items=if s.registered{vec![entry(&plan,&s,false)]}else{vec![]};if !s.trashed&&!s.moved&&!s.deleted{items.push(entry(&plan,&s,true));}
                    if s.collision{let mut other=entry(&plan,&s,false);other["drivewsid"]=json!("FILE::com.apple.CloudDocs::foreign");other["name"]=json!("Target");items.push(other);}Some(json!([{"drivewsid":FOLDER,"parentId":ROOT_ID,"name":"Owned","zone":"com.apple.CloudDocs","type":"FOLDER","numberOfItems":items.len(),"items":items}]).to_string().into_bytes())},
                    "/ws/com.apple.CloudDocs/upload/web"=>{let r:Value=serde_json::from_slice(&body).unwrap();assert_eq!(r["type"],"PACKAGE");assert_eq!(r["filename"],plan.staged_name);assert_eq!(r["size"],source.len());s.allocations+=1;assert_eq!(s.allocations,1);Some(json!([{"url":format!("{ORIGIN}/signed-upload"),"document_id":"new","owner_id":""}]).to_string().into_bytes())},
                    "/signed-upload"=>{assert_eq!(body,source);s.body_calls+=1;assert_eq!(s.body_calls,1);Some(json!({"hexBrSyntheticChecksum":"aa","ckSectionAssets":[{"hexFileChecksum":"bb","hexReferenceChecksum":"cc","hexWrappingKey":"dd","receiptToken":"YQ==","size":source.len()}]}).to_string().into_bytes())},
                    "/ws/com.apple.CloudDocs/update/documents"=>{let r:Value=serde_json::from_slice(&body).unwrap();assert_eq!(r["command"],"add_package");assert_eq!(r["document_id"],"new");assert_eq!(r["path"]["path"],plan.staged_name);assert_eq!(r["path"]["starting_document_id"],"owned");assert_eq!(r["allow_conflict"],false);assert_eq!(s.body_calls,1);s.registrations+=1;assert_eq!(s.registrations,1);s.registered=true;if lost_registration{None}else{Some(json!({"status":{"status_code":0},"results":[{"status":{"status_code":0},"document":{"document_id":"new","item_id":"new-item","etag":"new-v1","size":17,"name":plan.staged_name}}]}).to_string().into_bytes())}},
                    "/retrieveItemDetails"=>{let request:serde_json::Value=serde_json::from_slice(&body).unwrap();let old=request["items"][0]["drivewsid"]==OLD;assert!(old||request["items"][0]["drivewsid"]==NEW);let mut observed_entry=entry(&plan,&s,old);if !old&&s.wrong_stage_identity {observed_entry["drivewsid"]=json!("FILE::com.apple.CloudDocs::foreign");}
                    if !old&&s.change_after_stage_lookup{s.changed=true;}Some(json!({"items":if old&&s.deleted{vec![]}else{vec![observed_entry]}}).to_string().into_bytes())},
                    "/ws/com.apple.CloudDocs/download/by_id"=>{let id=url.query_pairs().find(|(key,_)|key=="document_id").unwrap().1;assert!(id=="old"||id=="new");Some(json!({"package_token":{"url":format!("{ORIGIN}/archive/{id}")}}).to_string().into_bytes())},
                    "/archive/old"|"/archive/new"=>{s.archive_calls+=1;let old=url.path().ends_with("old");let name=if old||s.installed{&plan.target_name}else{&plan.staged_name};Some(archive(name,old,s.corrupt))},
                    "/moveItemsToTrash"=>{let request:serde_json::Value=serde_json::from_slice(&body).unwrap();assert_eq!(request["items"][0]["drivewsid"],OLD);assert_eq!(request["items"][0]["etag"],"old-v1");s.trash_calls+=1;assert_eq!(s.trash_calls,1);assert!(!s.changed&&!s.moved&&!s.deleted);s.trashed=true;if lost_trash{None}else{Some(json!({"items":[{"status":"OK"}]}).to_string().into_bytes())}},
                    "/renameItems"=>{let request:serde_json::Value=serde_json::from_slice(&body).unwrap();assert_eq!(request["items"][0]["drivewsid"],NEW);assert_eq!(request["items"][0]["etag"],"new-v1");assert_eq!(request["items"][0]["name"],plan.target_name);s.rename_calls+=1;assert_eq!(s.rename_calls,1);assert!(s.trashed);s.installed=true;if lost_rename{None}else{Some(json!({"items":[{"status":"OK"}]}).to_string().into_bytes())}},
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
fn request(scope: Scope, bytes: &[u8]) -> UploadRequest {
    let original = Node {
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
    };
    UploadRequest {
        scope,
        intent: UploadIntent::Replace {
            item: OLD.into(),
            expected_etag: "old-v1".into(),
        },
        size: bytes.len() as u64,
        sha256: hex::encode(Sha256::digest(bytes)),
        representation: UploadRepresentation::PackageReplacementArchive {
            expected_root: "Source.pages".into(),
            semantic: semantic("Source.pages", false),
            original: Box::new(original),
            original_semantic: semantic("Target.pages", true),
        },
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
fn coordinator(
    server: &Server,
    directory: &Path,
    request: UploadRequest,
    operation: Uuid,
) -> ICloudFileReplace {
    let provider = ICloudPackageCreate::native_handoff_test_provider(
        request.scope.clone(),
        parent(),
        directory,
        server.session(),
    );
    let mut owner = ICloudFileReplace::with_native(
        request,
        parent(),
        operation,
        SessionSource::Snapshot {
            apple_id: "fixture@example.com".into(),
            snapshot: SecretString::from("unused"),
        },
        provider,
        directory,
        account_hash("fixture@example.com").unwrap(),
    )
    .unwrap();
    owner.native.as_mut().unwrap().fixture_transport =
        Some((server.client.clone(), format!("{ORIGIN}/").parse().unwrap()));
    owner
}
fn restart_coordinator(
    server: &Server,
    directory: &Path,
    request: UploadRequest,
    operation: Uuid,
    checkpoint: &SecretString,
    flat: bool,
) -> ICloudFileReplace {
    if !flat {
        return coordinator(server, directory, request, operation);
    }
    let mut owner = ICloudFileReplace::restore_native_package_from_sealed_checkpoint(
        request.clone(),
        operation,
        ICloudSealedSignIn {
            apple_id: "fixture@example.com".into(),
            credential_id: Uuid::new_v4().to_string(),
        },
        directory,
        directory,
        checkpoint,
    )
    .unwrap();
    let context = owner.native.as_mut().unwrap();
    context.provider = ICloudPackageCreate::native_handoff_test_provider(
        request.scope,
        parent(),
        directory,
        server.session(),
    );
    context.fixture_transport =
        Some((server.client.clone(), format!("{ORIGIN}/").parse().unwrap()));
    owner
}
fn take_commit(step: UploadStep) -> SecretString {
    match step {
        UploadStep::Commit(value) => value,
        _ => panic!("expected durable commit"),
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
async fn complete_arm(lost_registration: bool, lost_trash: bool, lost_rename: bool, flat: bool) {
    let operation = Uuid::new_v4();
    let mut plan = plan();
    plan.staged_name = format!("staged-by-cirrove-{operation}.pages");
    plan.recovery_name = format!("recovery-by-cirrove-{operation}.pages");
    if flat {
        plan.target_name = "Target.numbers".into();
        plan.staged_name = format!("staged-by-cirrove-{operation}.numbers");
        plan.recovery_name = format!("recovery-by-cirrove-{operation}.numbers");
        let proof = plan.package.as_mut().unwrap();
        proof.original = semantic_v2(&plan.target_name, true);
        proof.staged = semantic_v2("", false);
    }
    let source = archive(if flat { "" } else { "Source.pages" }, false, false);
    let dir = directory();
    let scope = Scope {
        account: Uuid::new_v4().to_string(),
        provider: "icloud".into(),
        collection: "drive".into(),
    };
    let mut r = request(scope, &source);
    if flat {
        let UploadRepresentation::PackageReplacementArchive { original, .. } = &r.representation
        else {
            panic!("wrapped fixture")
        };
        let mut original = original.clone();
        original.name = "Target.numbers".into();
        r.representation = UploadRepresentation::FlatNumbersReplacementArchive {
            semantic: semantic_v2("", false),
            original,
            original_semantic: semantic_v2("Target.numbers", true),
        };
    }
    assert_ne!(r.size, 17); // archive bytes are not logical package size
    let server = Server::start(
        plan,
        source.clone(),
        lost_registration,
        lost_trash,
        lost_rename,
    )
    .await;
    let mut owner = coordinator(&server, dir.path(), r.clone(), operation);
    let op = operation.to_string();
    let cancel = CancellationToken::new();
    let UploadStep::Allocate(armed) = owner
        .begin_upload_for_operation(&op, &r, &cancel)
        .await
        .unwrap()
    else {
        panic!("allocate")
    };
    assert_eq!(counts(&server), (0, 0, 0, 0, 0));
    if flat {
        let before = server.state.lock().unwrap().requests;
        for outer in [false, true] {
            let mut changed: Value = serde_json::from_str(armed.expose_secret()).unwrap();
            if outer {
                let UploadRepresentation::FlatNumbersReplacementArchive {
                    semantic,
                    original,
                    original_semantic,
                } = &r.representation
                else {
                    panic!("flat")
                };
                changed["request"]["representation"] =
                    serde_json::to_value(UploadRepresentation::PackageReplacementArchive {
                        expected_root: "Source.numbers".into(),
                        semantic: semantic.clone(),
                        original: original.clone(),
                        original_semantic: original_semantic.clone(),
                    })
                    .unwrap();
            } else {
                changed["phase"]["Stage"]["inner"]["request"]["representation"] =
                    serde_json::to_value(UploadRepresentation::PackageArchive {
                        expected_root: "Source.numbers".into(),
                        semantic: semantic_v2("", false),
                    })
                    .unwrap();
            }
            let changed = SecretString::from(changed.to_string());
            assert!(matches!(
                owner
                    .allocate_upload_for_operation(&op, &r, &changed, &cancel)
                    .await,
                Err(UploadError::CheckpointInvalid)
            ));
            assert_eq!(
                server.state.lock().unwrap().requests,
                before,
                "layout tamper must refuse before original verification HTTP"
            );
            assert!(matches!(
                ICloudFileReplace::restore_native_package_from_sealed_checkpoint(
                    r.clone(),
                    operation,
                    ICloudSealedSignIn {
                        apple_id: "fixture@example.com".into(),
                        credential_id: Uuid::new_v4().to_string()
                    },
                    dir.path(),
                    dir.path(),
                    &changed,
                ),
                Err(UploadError::CheckpointInvalid)
            ));
        }
    }
    // Restart with an armed allocation, no returned slot: strictly read-only.
    owner = restart_coordinator(&server, dir.path(), r.clone(), operation, &armed, flat);
    assert!(matches!(
        owner
            .inspect_upload_for_operation(&op, &r, &armed, &cancel)
            .await,
        Err(UploadError::Uncertain)
    ));
    assert_eq!(counts(&server), (0, 0, 0, 0, 0));
    let UploadStep::Stream(body) = owner
        .allocate_upload_for_operation(&op, &r, &armed, &cancel)
        .await
        .unwrap()
    else {
        panic!("body")
    };
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(&source).unwrap();
    let registration = take_commit(
        owner
            .upload_stream_for_operation(&op, &r, &body, file, &cancel)
            .await
            .unwrap(),
    );
    let step = owner
        .commit_upload_for_operation(&op, &r, &registration, &cancel)
        .await;
    let move_old = if lost_registration {
        assert!(step.is_err());
        owner = restart_coordinator(
            &server,
            dir.path(),
            r.clone(),
            operation,
            &registration,
            flat,
        );
        take_commit(
            owner
                .inspect_upload_for_operation(&op, &r, &registration, &cancel)
                .await
                .unwrap(),
        )
    } else {
        take_commit(step.unwrap())
    };
    assert_eq!(counts(&server), (1, 1, 1, 0, 0));
    // An armed but not dispatched Trash phase may not replay on inspection.
    assert!(matches!(
        owner
            .inspect_upload_for_operation(&op, &r, &move_old, &cancel)
            .await,
        Err(UploadError::Uncertain)
    ));
    assert_eq!(counts(&server), (1, 1, 1, 0, 0));
    let step = owner
        .commit_upload_for_operation(&op, &r, &move_old, &cancel)
        .await;
    let inspect = if lost_trash {
        assert!(step.is_err());
        owner = restart_coordinator(&server, dir.path(), r.clone(), operation, &move_old, flat);
        take_commit(
            owner
                .inspect_upload_for_operation(&op, &r, &move_old, &cancel)
                .await
                .unwrap(),
        )
    } else {
        take_commit(step.unwrap())
    };
    assert_eq!(counts(&server), (1, 1, 1, 1, 0));
    let install = take_commit(
        owner
            .commit_upload_for_operation(&op, &r, &inspect, &cancel)
            .await
            .unwrap(),
    );
    owner = restart_coordinator(&server, dir.path(), r.clone(), operation, &install, flat);
    assert!(matches!(
        owner
            .inspect_upload_for_operation(&op, &r, &install, &cancel)
            .await,
        Err(UploadError::Uncertain)
    ));
    assert_eq!(counts(&server), (1, 1, 1, 1, 0));
    let step = owner
        .commit_upload_for_operation(&op, &r, &install, &cancel)
        .await;
    let step = if lost_rename {
        assert!(step.is_err());
        owner = restart_coordinator(&server, dir.path(), r.clone(), operation, &install, flat);
        owner
            .inspect_upload_for_operation(&op, &r, &install, &cancel)
            .await
            .unwrap()
    } else {
        step.unwrap()
    };
    let UploadStep::PackageHandoffComplete(receipt) = step else {
        panic!("typed package receipt")
    };
    assert_eq!(receipt.original.id, OLD);
    assert_eq!(receipt.current.remote.id, NEW);
    assert_eq!(receipt.backup.remote.id, OLD);
    assert_eq!(
        receipt.current.remote.name,
        if flat {
            "Target.numbers"
        } else {
            "Target.pages"
        }
    );
    assert_eq!(receipt.current.remote.size, 17);
    assert_eq!(receipt.backup.remote.parent_id.as_deref(), Some(TRASH_ROOT));
    assert_eq!(
        receipt.current.semantic,
        if flat {
            semantic_v2("", false)
        } else {
            semantic("Source.pages", false)
        }
    );
    assert_eq!(
        receipt.backup.semantic,
        if flat {
            semantic_v2("Target.numbers", true)
        } else {
            semantic("Target.pages", true)
        }
    );
    let result = owner
        .reconcile_upload_for_operation(&op, &r, Some(&install), &cancel)
        .await
        .unwrap();
    assert!(matches!(result, Reconciliation::PackageHandoffCommitted(_)));
    assert_eq!(counts(&server), (1, 1, 1, 1, 1));
}
#[tokio::test]
async fn package_creation_through_typed_handoff_preserves_logical_sizes() {
    complete_arm(false, false, false, false).await;
}
#[tokio::test]
async fn lost_registration_trash_and_install_replies_resume_by_inspection_only() {
    complete_arm(true, true, true, false).await;
}
#[tokio::test]
async fn wrong_bindings_and_proofs_refuse_before_network() {
    let operation = Uuid::new_v4();
    let source = archive("Source.pages", false, false);
    let dir = directory();
    let server = Server::start(plan(), source.clone(), false, false, false).await;
    let r = request(
        Scope {
            account: Uuid::new_v4().to_string(),
            provider: "icloud".into(),
            collection: "drive".into(),
        },
        &source,
    );
    let owner = coordinator(&server, dir.path(), r.clone(), operation);
    let cancel = CancellationToken::new();
    let op = operation.to_string();
    let mut p = plan();
    p.staged_name = owner.stage_name.clone();
    p.recovery_name = owner.recovery_name.clone();
    let valid = owner
        .native_encode(NativePhase::Handoff {
            plan: Box::new(p),
            phase: HandoffPhase::MoveOld,
        })
        .unwrap();
    assert!(owner.native_decode(&op, &r, &valid).is_ok());
    for field in [
        "account",
        "request",
        "original_semantic",
        "staged_semantic",
        "size",
        "operation",
        "legacy",
    ] {
        let mut saved: Value = serde_json::from_str(valid.expose_secret()).unwrap();
        match field {
            "account" => saved["account_hash"] = json!("0".repeat(64)),
            "request" => saved["request"]["sha256"] = json!("0".repeat(64)),
            "original_semantic" => {
                saved["phase"]["Handoff"]["plan"]["package"]["original"]["sha256"] =
                    json!("0".repeat(64))
            }
            "staged_semantic" => {
                saved["phase"]["Handoff"]["plan"]["package"]["staged"]["sha256"] =
                    json!("0".repeat(64))
            }
            "size" => saved["phase"]["Handoff"]["plan"]["package"]["original_size"] = json!(18),
            "operation" => saved["operation"] = json!(Uuid::new_v4()),
            _ => saved["phase"]["Handoff"]["phase"] = json!("install_new"),
        }
        let checkpoint = SecretString::from(saved.to_string());
        assert!(
            matches!(
                owner
                    .inspect_upload_for_operation(&op, &r, &checkpoint, &cancel)
                    .await,
                Err(UploadError::CheckpointInvalid)
            ),
            "accepted {field}"
        );
    }
    assert!(
        owner
            .begin_upload_for_operation(&Uuid::new_v4().to_string(), &r, &cancel)
            .await
            .is_err()
    );
    assert!(matches!(
        owner.begin_upload(&r, &cancel).await,
        Err(UploadError::Unsupported(_))
    ));
    cancel.cancel();
    assert!(matches!(
        owner.begin_upload_for_operation(&op, &r, &cancel).await,
        Err(UploadError::Uncertain)
    ));
    assert_eq!(server.state.lock().unwrap().requests, 0);
}
#[test]
fn structural_checkpoint_bounds_do_not_double_escape_registration() {
    let registration = json!({"pkgSignature":"q\\\"".repeat(5000),"manifest":[],"package":{}});
    let inner = json!({"registration":registration.to_string(),"slot":null});
    let packed = pack(SecretString::from(inner.to_string())).unwrap();
    assert!(packed["registration"].is_object());
    let restored: Value = serde_json::from_str(unpack(packed).unwrap().expose_secret()).unwrap();
    assert_eq!(restored, inner);
    let oversized = json!({"registration":null,"slot":{"url":"x".repeat(INNER_LIMIT)}});
    assert!(matches!(
        pack(SecretString::from(oversized.to_string())),
        Err(UploadError::Uncertain)
    ));
    assert!(matches!(
        pack(SecretString::from(" ".repeat(LIMIT + 1))),
        Err(UploadError::CheckpointInvalid)
    ));
}
#[tokio::test]
async fn changed_original_before_stage_never_allocates_or_mutates() {
    for change in ["etag", "move", "delete", "content"] {
        let operation = Uuid::new_v4();
        let source = archive("Source.pages", false, false);
        let dir = directory();
        let server = Server::start(plan(), source.clone(), false, false, false).await;
        {
            let mut state = server.state.lock().unwrap();
            match change {
                "etag" => state.changed = true,
                "move" => state.moved = true,
                "delete" => state.deleted = true,
                _ => state.corrupt = true,
            }
        }
        let r = request(
            Scope {
                account: Uuid::new_v4().to_string(),
                provider: "icloud".into(),
                collection: "drive".into(),
            },
            &source,
        );
        let owner = coordinator(&server, dir.path(), r.clone(), operation);
        assert!(
            owner
                .begin_upload_for_operation(&operation.to_string(), &r, &CancellationToken::new())
                .await
                .is_err(),
            "accepted {change}"
        );
        assert_eq!(counts(&server), (0, 0, 0, 0, 0));
    }
}
#[tokio::test]
async fn complete_envelope_and_header_limits_fail_without_dispatch() {
    let operation = Uuid::new_v4();
    let source = archive("Source.pages", false, false);
    let dir = directory();
    let server = Server::start(plan(), source.clone(), false, false, false).await;
    let r = request(
        Scope {
            account: Uuid::new_v4().to_string(),
            provider: "icloud".into(),
            collection: "drive".into(),
        },
        &source,
    );
    let owner = coordinator(&server, dir.path(), r.clone(), operation);
    let mut oversized = parent();
    oversized.name = "n".repeat(HEADER_LIMIT);
    let provider = ICloudPackageCreate::native_handoff_test_provider(
        r.scope.clone(),
        oversized.clone(),
        dir.path(),
        server.session(),
    );
    assert!(matches!(
        ICloudFileReplace::with_native(
            r.clone(),
            oversized,
            operation,
            SessionSource::Snapshot {
                apple_id: "fixture@example.com".into(),
                snapshot: "unused".into()
            },
            provider,
            dir.path(),
            account_hash("fixture@example.com").unwrap()
        ),
        Err(UploadError::Invalid)
    ));
    assert!(matches!(
        owner.native_encode(NativePhase::Stage {
            inner: json!({"data":"x".repeat(LIMIT)})
        }),
        Err(UploadError::Uncertain)
    ));
    let secret = SecretString::from(" ".repeat(LIMIT + 1));
    assert!(matches!(
        owner
            .inspect_upload_for_operation(
                &operation.to_string(),
                &r,
                &secret,
                &CancellationToken::new()
            )
            .await,
        Err(UploadError::CheckpointInvalid)
    ));
    assert_eq!(server.state.lock().unwrap().requests, 0);
}

#[tokio::test]
async fn native_stage_abandonment_proof_is_exact_and_only_reads_metadata() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let operation = Uuid::new_v4();
        let mut p = plan();
        p.staged_name = format!("staged-by-cirrove-{operation}.pages");
        p.recovery_name = format!("recovery-by-cirrove-{operation}.pages");
        let source = archive("Source.pages", false, false);
        let dir = directory();
        let r = request(
            Scope {
                account: Uuid::new_v4().to_string(),
                provider: "icloud".into(),
                collection: "drive".into(),
            },
            &source,
        );
        let server = Server::start(p, source.clone(), false, false, false).await;
        let owner = coordinator(&server, dir.path(), r.clone(), operation);
        let op = operation.to_string();
        let cancel = CancellationToken::new();
        let UploadStep::Allocate(armed) = owner
            .begin_upload_for_operation(&op, &r, &cancel)
            .await
            .unwrap()
        else {
            panic!("allocate")
        };
        let UploadStep::Stream(body) = owner
            .allocate_upload_for_operation(&op, &r, &armed, &cancel)
            .await
            .unwrap()
        else {
            panic!("stream")
        };
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(&source).unwrap();
        let registration = take_commit(
            owner
                .upload_stream_for_operation(&op, &r, &body, file, &cancel)
                .await
                .unwrap(),
        );
        let handoff = take_commit(
            owner
                .commit_upload_for_operation(&op, &r, &registration, &cancel)
                .await
                .unwrap(),
        );
        #[cfg(feature = "test-support")]
        {
            let keys = Arc::new(AbandonKeys::default());
            let vault = crate::SealedUploadCheckpointVault::with_test_key_vault(
                dir.path(),
                &r.scope.account,
                keys.clone(),
            )
            .unwrap();
            let key = format!("upload/{operation}");
            vault.save(&key, registration.clone()).await.unwrap();
            let proof = owner
                .inspect_native_stage_abandonment(&op, &r, &vault, &cancel)
                .await
                .unwrap();
            assert_eq!(proof.record().operation, operation);
            vault.save(&key, handoff.clone()).await.unwrap();
            let checkpoint_path = dir
                .path()
                .join("accounts")
                .join(&r.scope.account)
                .join("upload-checkpoints")
                .join(&op)
                .join("checkpoint.sealed");
            let later = std::fs::read(&checkpoint_path).unwrap();
            let requests_before = server.state.lock().unwrap().requests;
            assert!(
                owner
                    .inspect_native_stage_abandonment(&op, &r, &vault, &cancel)
                    .await
                    .is_err()
            );
            assert_eq!(
                server.state.lock().unwrap().requests,
                requests_before,
                "current Handoff must refuse before provider access"
            );
            vault.save(&key, registration.clone()).await.unwrap();
            *keys.replace_on_load.lock().unwrap() = Some((2, checkpoint_path, later));
            assert!(
                owner
                    .inspect_native_stage_abandonment(&op, &r, &vault, &cancel)
                    .await
                    .is_err(),
                "checkpoint changed after observation"
            );
            assert!(keys.replace_on_load.lock().unwrap().is_none());
            let foreign = crate::SealedUploadCheckpointVault::with_test_key_vault(
                dir.path(),
                &Uuid::new_v4().to_string(),
                keys,
            )
            .unwrap();
            assert!(
                owner
                    .inspect_native_stage_abandonment(&op, &r, &foreign, &cancel)
                    .await
                    .is_err()
            );
        }
        let counts_before = counts(&server);
        let archives_before = server.state.lock().unwrap().archive_calls;
        let proof = owner
            .inspect_native_stage_abandonment_checkpoint(&op, &r, &registration, &cancel)
            .await
            .unwrap();
        assert_eq!(proof.record().operation, operation);
        assert!(proof.record().request == r);
        assert_eq!(proof.record().original.id, OLD);
        assert_eq!(proof.record().staged.id, NEW);
        assert!(proof.is_fresh());
        assert_eq!(
            proof.record().checkpoint_sha256,
            hex::encode(Sha256::digest(registration.expose_secret().as_bytes()))
        );
        let requests_before = server.state.lock().unwrap().requests;
        for checkpoint in [&armed, &body, &handoff] {
            assert!(
                owner
                    .inspect_native_stage_abandonment_checkpoint(&op, &r, checkpoint, &cancel)
                    .await
                    .is_err()
            );
        }
        let mut wrong_request = r.clone();
        wrong_request.scope.account = Uuid::new_v4().to_string();
        assert!(
            owner
                .inspect_native_stage_abandonment_checkpoint(
                    &op,
                    &wrong_request,
                    &registration,
                    &cancel
                )
                .await
                .is_err()
        );
        assert!(
            owner
                .inspect_native_stage_abandonment_checkpoint(
                    &Uuid::new_v4().to_string(),
                    &r,
                    &registration,
                    &cancel
                )
                .await
                .is_err()
        );
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(
            owner
                .inspect_native_stage_abandonment_checkpoint(&op, &r, &registration, &cancelled)
                .await
                .is_err()
        );
        assert_eq!(server.state.lock().unwrap().requests, requests_before);
        for fault in [
            "revision",
            "moved",
            "deleted",
            "trashed",
            "stage-identity",
            "during-read",
        ] {
            {
                let mut s = server.state.lock().unwrap();
                match fault {
                    "revision" => s.changed = true,
                    "moved" => s.moved = true,
                    "deleted" => s.deleted = true,
                    "trashed" => s.trashed = true,
                    "stage-identity" => s.wrong_stage_identity = true,
                    _ => s.change_after_stage_lookup = true,
                }
            }
            assert!(
                owner
                    .inspect_native_stage_abandonment_checkpoint(&op, &r, &registration, &cancel)
                    .await
                    .is_err(),
                "{fault}"
            );
            {
                let mut s = server.state.lock().unwrap();
                s.changed = false;
                s.moved = false;
                s.deleted = false;
                s.trashed = false;
                s.wrong_stage_identity = false;
                s.change_after_stage_lookup = false;
            }
        }
        assert_eq!(
            counts(&server),
            counts_before,
            "inspection cannot mutate provider state"
        );
        assert_eq!(
            server.state.lock().unwrap().archive_calls,
            archives_before,
            "inspection never downloads content"
        );
    })
    .await
    .unwrap();
}

#[cfg(feature = "test-support")]
#[derive(Default)]
struct AbandonKeys {
    keys: Mutex<std::collections::HashMap<String, SecretString>>,
    replace_on_load: Mutex<Option<(usize, std::path::PathBuf, Vec<u8>)>>,
}
#[cfg(feature = "test-support")]
#[async_trait::async_trait]
impl cirrove_auth::CredentialVault for AbandonKeys {
    async fn load(&self, key: &str) -> anyhow::Result<Option<SecretString>> {
        let replacement = {
            let mut hook = self.replace_on_load.lock().unwrap();
            if let Some((remaining, ..)) = hook.as_mut() {
                *remaining -= 1;
            }
            if hook.as_ref().is_some_and(|(remaining, ..)| *remaining == 0) {
                hook.take()
            } else {
                None
            }
        };
        if let Some((_, path, bytes)) = replacement {
            std::fs::write(path, bytes)?;
        }
        Ok(self.keys.lock().unwrap().get(key).cloned())
    }
    async fn save(&self, key: &str, value: SecretString) -> anyhow::Result<()> {
        self.keys.lock().unwrap().insert(key.to_owned(), value);
        Ok(())
    }
    async fn remove(&self, key: &str) -> anyhow::Result<()> {
        self.keys.lock().unwrap().remove(key);
        Ok(())
    }
}

#[tokio::test]
async fn native_internal_formats_bind_root_target_and_recovery_without_data_authority() {
    let scope = Scope {
        account: Uuid::new_v4().to_string(),
        provider: "icloud".into(),
        collection: "drive".into(),
    };
    for suffix in [".pages", ".numbers", ".key"] {
        let root = format!("Source{suffix}");
        let bytes = archive(&root, false, false);
        let mut request = request(scope.clone(), &bytes);
        if let UploadRepresentation::PackageReplacementArchive {
            expected_root,
            original,
            semantic: proof,
            original_semantic,
            ..
        } = &mut request.representation
        {
            *expected_root = root.clone();
            original.name = format!("Target{suffix}");
            *proof = semantic(&root, false);
            *original_semantic = semantic(&original.name, true);
        }
        ICloudFileReplace::native_identity(&request, &parent()).unwrap();
        let dir = directory();
        let operation = Uuid::new_v4();
        let owner = ICloudFileReplace::native_package_from_sealed_session(
            request.clone(),
            parent(),
            operation,
            ICloudSealedSignIn {
                apple_id: "fixture@example.com".into(),
                credential_id: Uuid::new_v4().to_string(),
            },
            dir.path(),
            dir.path(),
        )
        .unwrap();
        assert_eq!(
            owner.stage_name,
            format!("staged-by-cirrove-{operation}{suffix}")
        );
        assert_eq!(
            owner.recovery_name,
            format!("recovery-by-cirrove-{operation}{suffix}")
        );

        for bad in ["root", "data", "app"] {
            let mut changed = request.clone();
            if let UploadRepresentation::PackageReplacementArchive {
                expected_root,
                original,
                ..
            } = &mut changed.representation
            {
                match bad {
                    "root" => {
                        *expected_root = if suffix == ".key" {
                            "Source.numbers"
                        } else {
                            "Source.key"
                        }
                        .into()
                    }
                    "data" => {
                        original.package = false;
                        original.kind = NodeKind::File;
                    }
                    _ => original.parent_id = Some("FOLDER::com.apple.Numbers::documents".into()),
                }
            }
            assert!(
                ICloudFileReplace::native_identity(&changed, &parent()).is_err(),
                "{suffix} {bad}"
            );
        }
        let mut handoff = plan();
        handoff.target_name = format!("Target{suffix}");
        handoff.staged_name = format!("staged-by-cirrove-{}{suffix}", Uuid::new_v4());
        handoff.recovery_name = format!("recovery-by-cirrove-{}{suffix}", Uuid::new_v4());
        handoff.validate().unwrap();
        let good = handoff.clone();
        for bad in ["staged", "recovery", "proof"] {
            let mut changed = good.clone();
            let other = if suffix == ".key" { ".numbers" } else { ".key" };
            match bad {
                "staged" => {
                    changed.staged_name = format!("staged-by-cirrove-{}{other}", Uuid::new_v4())
                }
                "recovery" => {
                    changed.recovery_name = format!("recovery-by-cirrove-{}{other}", Uuid::new_v4())
                }
                _ => changed.package = None,
            }
            assert!(changed.validate().is_err(), "{suffix} {bad}");
        }
    }
}

#[tokio::test]
async fn native_legacy_suffix_only_pages_request_and_handoff_remain_valid() {
    let scope = Scope {
        account: Uuid::new_v4().to_string(),
        provider: "icloud".into(),
        collection: "drive".into(),
    };
    let body = archive(".pages", false, false);
    let mut r = request(scope, &body);
    if let UploadRepresentation::PackageReplacementArchive {
        expected_root,
        original,
        semantic: proof,
        original_semantic,
    } = &mut r.representation
    {
        *expected_root = ".pages".into();
        original.name = ".pages".into();
        *proof = semantic(".pages", false);
        *original_semantic = semantic(".pages", true);
    }
    ICloudFileReplace::native_identity(&r, &parent()).unwrap();
    let mut retained = plan();
    retained.target_name = ".pages".into();
    retained.validate().unwrap();
    let encoded = serde_json::to_string(&retained).unwrap();
    let restored: HandoffPlan = serde_json::from_str(&encoded).unwrap();
    restored.validate().unwrap();
    let dir = directory();
    let operation = Uuid::new_v4();
    let sign_in = || ICloudSealedSignIn {
        apple_id: "fixture@example.com".into(),
        credential_id: "00000000-0000-4000-8000-000000000001".into(),
    };
    let owner = ICloudFileReplace::native_package_from_sealed_session(
        r.clone(),
        parent(),
        operation,
        sign_in(),
        dir.path(),
        dir.path(),
    )
    .unwrap();
    retained.staged_name = owner.stage_name.clone();
    retained.recovery_name = owner.recovery_name.clone();
    if let UploadRepresentation::PackageReplacementArchive {
        semantic,
        original_semantic,
        ..
    } = &r.representation
    {
        let proof = retained.package.as_mut().unwrap();
        proof.original = original_semantic.clone();
        proof.staged = semantic.clone();
    }
    let checkpoint = owner
        .native_encode(NativePhase::Handoff {
            plan: Box::new(retained),
            phase: HandoffPhase::MoveOld,
        })
        .unwrap();
    let restored = ICloudFileReplace::restore_native_package_from_sealed_checkpoint(
        r.clone(),
        operation,
        sign_in(),
        dir.path(),
        dir.path(),
        &checkpoint,
    )
    .unwrap();
    assert!(matches!(
        restored
            .native_decode(&operation.to_string(), &r, &checkpoint)
            .unwrap(),
        NativePhase::Handoff {
            phase: HandoffPhase::MoveOld,
            ..
        }
    ));
}

#[tokio::test]
async fn native_mixed_case_pages_checkpoint_keeps_exact_names_and_phase() {
    let scope = Scope {
        account: Uuid::new_v4().to_string(),
        provider: "icloud".into(),
        collection: "drive".into(),
    };
    let body = archive(".PaGeS", false, false);
    let mut r = request(scope, &body);
    if let UploadRepresentation::PackageReplacementArchive {
        expected_root,
        original,
        semantic: proof,
        original_semantic,
    } = &mut r.representation
    {
        *expected_root = ".PaGeS".into();
        original.name = ".PaGeS".into();
        *proof = semantic(".PaGeS", false);
        *original_semantic = semantic(".PaGeS", true);
    }
    ICloudFileReplace::native_identity(&r, &parent()).unwrap();
    let mut retained = plan();
    retained.target_name = ".PaGeS".into();
    retained.validate().unwrap();
    let encoded = serde_json::to_string(&retained).unwrap();
    let restored: HandoffPlan = serde_json::from_str(&encoded).unwrap();
    restored.validate().unwrap();
    let dir = directory();
    let operation = Uuid::new_v4();
    let sign_in = || ICloudSealedSignIn {
        apple_id: "fixture@example.com".into(),
        credential_id: "00000000-0000-4000-8000-000000000001".into(),
    };
    let owner = ICloudFileReplace::native_package_from_sealed_session(
        r.clone(),
        parent(),
        operation,
        sign_in(),
        dir.path(),
        dir.path(),
    )
    .unwrap();
    retained.staged_name = owner.stage_name.clone();
    retained.recovery_name = owner.recovery_name.clone();
    if let UploadRepresentation::PackageReplacementArchive {
        semantic,
        original_semantic,
        ..
    } = &r.representation
    {
        let proof = retained.package.as_mut().unwrap();
        proof.original = original_semantic.clone();
        proof.staged = semantic.clone();
    }
    let checkpoint = owner
        .native_encode(NativePhase::Handoff {
            plan: Box::new(retained),
            phase: HandoffPhase::MoveOld,
        })
        .unwrap();
    let restored = ICloudFileReplace::restore_native_package_from_sealed_checkpoint(
        r.clone(),
        operation,
        sign_in(),
        dir.path(),
        dir.path(),
        &checkpoint,
    )
    .unwrap();
    assert!(matches!(
        restored
            .native_decode(&operation.to_string(), &r, &checkpoint)
            .unwrap(),
        NativePhase::Handoff {
            phase: HandoffPhase::MoveOld,
            ..
        }
    ));
}

/// Exercise the real HTTPS Stage observation through its final record validator.
/// The lost registration reply retains Stage authority; no Trash/rename occurs.
async fn native_abandon_format_arm(suffix: &str) {
    tokio::time::timeout(Duration::from_secs(20), async {
        let operation = Uuid::new_v4();
        let op = operation.to_string();
        let root = format!("Source{suffix}");
        let target = format!("Target{suffix}");
        let canonical = cirrove_core::upload::native_package_suffix(&target).unwrap();
        let source = archive(&root, false, false);
        let semantic_v2 = |root: &str, old: bool| {
            let bytes = archive(root, old, false);
            let mut file = tempfile::tempfile().unwrap();
            file.write_all(&bytes).unwrap();
            crate::package_archive_semantic_identity_versioned(
                &file,
                &PackageDownload {
                    size: bytes.len() as u64,
                    sha256: hex::encode(Sha256::digest(&bytes)),
                },
                root,
                2,
                &CancellationToken::new(),
            )
            .unwrap()
        };
        let mut r = request(
            Scope {
                account: Uuid::new_v4().to_string(),
                provider: "icloud".into(),
                collection: "drive".into(),
            },
            &source,
        );
        let UploadRepresentation::PackageReplacementArchive {
            expected_root,
            semantic,
            original,
            original_semantic,
        } = &mut r.representation
        else {
            panic!("native request fixture");
        };
        *expected_root = root.clone();
        original.name = target.clone();
        *semantic = semantic_v2(&root, false);
        *original_semantic = semantic_v2(&target, true);
        let original = original.as_ref().clone();
        r.validate().unwrap();
        let mut p = plan();
        p.target_name = target;
        p.staged_name = format!("staged-by-cirrove-{operation}{canonical}");
        p.recovery_name = format!("recovery-by-cirrove-{operation}{canonical}");
        if let UploadRepresentation::PackageReplacementArchive {
            semantic,
            original_semantic,
            ..
        } = &r.representation
        {
            let proof = p.package.as_mut().unwrap();
            proof.original = original_semantic.clone();
            proof.staged = semantic.clone();
        }
        p.validate().unwrap();
        let dir = directory();
        let source_path = dir.path().join("source.zip");
        std::fs::write(&source_path, &source).unwrap();
        let server = Server::start(p, source.clone(), true, false, false).await;
        let owner = coordinator(&server, dir.path(), r.clone(), operation);
        let cancel = CancellationToken::new();
        assert_eq!(owner.stage_name, format!("staged-by-cirrove-{operation}{canonical}"));
        let UploadStep::Allocate(armed) = owner
            .begin_upload_for_operation(&op, &r, &cancel)
            .await
            .unwrap()
        else {
            panic!("allocate");
        };
        let UploadStep::Stream(body) = owner
            .allocate_upload_for_operation(&op, &r, &armed, &cancel)
            .await
            .unwrap()
        else {
            panic!("stream");
        };
        let registration = take_commit(
            owner
                .upload_stream_for_operation(
                    &op,
                    &r,
                    &body,
                    File::open(&source_path).unwrap(),
                    &cancel,
                )
                .await
                .unwrap(),
        );
        assert!(matches!(
            owner.commit_upload_for_operation(&op, &r, &registration, &cancel).await,
            Err(UploadError::Uncertain)
        ));
        assert_eq!(counts(&server), (1, 1, 1, 0, 0));
        let requests_before = server.state.lock().unwrap().requests;
        let archives_before = server.state.lock().unwrap().archive_calls;
        let observed = owner
            .inspect_native_stage_abandonment_checkpoint(&op, &r, &registration, &cancel)
            .await;
        // A fixture panic/metadata mismatch cannot masquerade as the regression:
        // all five exact read-only metadata/representation requests must finish.
        assert_eq!(server.state.lock().unwrap().requests, requests_before + 5);
        assert_eq!(server.state.lock().unwrap().archive_calls, archives_before);
        assert_eq!(counts(&server), (1, 1, 1, 0, 0));
        {
            let state = server.state.lock().unwrap();
            assert!(state.registered);
            assert!(!state.trashed && !state.installed && !state.changed);
            assert!(!state.moved && !state.deleted && !state.corrupt);
        }
        // Independently reverify original semantic-v2 content after observation,
        // including baseline refusal, before asserting the final proof endpoint.
        owner.native_before_stage(&cancel).await.unwrap();
        assert_eq!(std::fs::read(&source_path).unwrap(), source);
        assert_eq!(counts(&server), (1, 1, 1, 0, 0));
        let proof = match observed {
            Ok(proof) => proof,
            Err(UploadError::Invalid) => panic!(
                "valid native Stage format rejected at final record validation after exact metadata reads: {suffix}"
            ),
            Err(_) => panic!("unexpected native Stage observation failure: {suffix}"),
        };
        assert!(proof.is_fresh());
        let record = proof.record();
        record.validate().unwrap();
        assert_eq!(record.operation, operation);
        assert!(record.request == r);
        assert!(record.original == original);
        assert_eq!(record.staged.id, NEW);
        assert_eq!(record.staged.parent_id, original.parent_id);
        assert_eq!(record.staged.name, owner.stage_name);
        assert_eq!(record.staged.etag.as_deref(), Some("new-v1"));
        assert_eq!(record.checkpoint_sha256, hex::encode(Sha256::digest(registration.expose_secret().as_bytes())));
        let wrong_suffix = if canonical == ".key" { ".numbers" } else { ".key" };
        for bad_name in [
            format!("staged-by-cirrove-{operation}{wrong_suffix}"),
            format!("staged-by-cirrove-{}{canonical}", Uuid::new_v4()),
            format!("staged-by-cirrove-{operation}{}", canonical.to_ascii_uppercase()),
        ] {
            let mut changed = record.clone();
            changed.staged.name = bad_name;
            assert!(matches!(changed.validate(), Err(UploadError::Invalid)),
                "changed staged format, operation or exact canonical case was accepted");
        }
        assert_eq!(counts(&server), (1, 1, 1, 0, 0));
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn native_stage_abandonment_numbers_keeps_exact_operation_and_original() {
    for suffix in [".numbers", ".NuMbErS"] {
        native_abandon_format_arm(suffix).await;
    }
}

#[tokio::test]
async fn native_stage_abandonment_keynote_keeps_exact_operation_and_original() {
    for suffix in [".key", ".KEY"] {
        native_abandon_format_arm(suffix).await;
    }
}

#[tokio::test]
async fn native_stage_abandonment_mixed_pages_keeps_exact_operation_and_original() {
    native_abandon_format_arm(".PAGES").await;
}

#[tokio::test]
async fn flat_numbers_typed_handoff_and_checkpoint_restore_keep_source_layout() {
    for lost in [false, true] {
        tokio::time::timeout(
            Duration::from_secs(30),
            complete_arm(lost, lost, lost, true),
        )
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn flat_numbers_inner_layout_tamper_refuses_before_original_http() {
    let operation = Uuid::new_v4();
    let mut plan = plan();
    plan.target_name = "Target.numbers".into();
    plan.staged_name = format!("staged-by-cirrove-{operation}.numbers");
    plan.recovery_name = format!("recovery-by-cirrove-{operation}.numbers");
    let proof = plan.package.as_mut().unwrap();
    proof.original = semantic_v2(&plan.target_name, true);
    proof.staged = semantic_v2("", false);
    let source = archive("", false, false);
    let scope = Scope {
        account: Uuid::new_v4().to_string(),
        provider: "icloud".into(),
        collection: "drive".into(),
    };
    let mut request = request(scope, &source);
    let UploadRepresentation::PackageReplacementArchive { original, .. } = &request.representation
    else {
        panic!("wrapped fixture")
    };
    let mut original = original.clone();
    original.name = "Target.numbers".into();
    request.representation = UploadRepresentation::FlatNumbersReplacementArchive {
        semantic: semantic_v2("", false),
        original,
        original_semantic: semantic_v2("Target.numbers", true),
    };
    let dir = directory();
    let server = Server::start(plan, source, false, false, false).await;
    let owner = coordinator(&server, dir.path(), request.clone(), operation);
    let token = CancellationToken::new();
    let op = operation.to_string();
    let UploadStep::Allocate(armed) = owner
        .begin_upload_for_operation(&op, &request, &token)
        .await
        .unwrap()
    else {
        panic!("real allocation arm")
    };
    let mut value: Value = serde_json::from_str(armed.expose_secret()).unwrap();
    value["phase"]["Stage"]["inner"]["request"]["representation"] =
        serde_json::to_value(UploadRepresentation::PackageArchive {
            expected_root: "Source.numbers".into(),
            semantic: semantic_v2("", false),
        })
        .unwrap();
    let changed = SecretString::from(value.to_string());
    let requests_before = server.state.lock().unwrap().requests;
    assert!(
        requests_before > 0,
        "genuine original package was independently verified before arming"
    );
    assert_eq!(counts(&server), (0, 0, 0, 0, 0));
    assert!(matches!(
        owner
            .allocate_upload_for_operation(&op, &request, &changed, &token)
            .await,
        Err(UploadError::CheckpointInvalid)
    ));
    assert_eq!(
        counts(&server),
        (0, 0, 0, 0, 0),
        "tamper never authorizes provider mutation"
    );
    assert_eq!(
        server.state.lock().unwrap().requests,
        requests_before,
        "inner source-layout tamper must refuse before any original verification HTTP"
    );
}
