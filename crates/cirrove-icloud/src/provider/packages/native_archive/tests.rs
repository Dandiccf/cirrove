#![allow(clippy::unwrap_used)]
use super::*;
use std::{io::Write, sync::Mutex as StdMutex};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use uuid::Uuid;
const SOURCE: &str = "FILE::com.apple.CloudDocs::native";
#[derive(Default)]
struct State {
    requests: usize,
    lookups: usize,
    details: usize,
    data: bool,
    app_parent: bool,
    app_library: bool,
    stale: bool,
    change_after_lookup: bool,
    foreign_zone: bool,
    missing: bool,
}
struct Fixture {
    drive: Arc<ICloudDrive>,
    scope: Scope,
    archive: Node,
    semantic: cirrove_core::upload::PackageSemanticIdentity,
    state: Arc<StdMutex<State>>,
    server: tokio::task::JoinHandle<()>,
    _dir: tempfile::TempDir,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
fn bytes() -> Vec<u8> {
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    z.start_file(
        "Owned.pages/Document",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    z.write_all(b"owned native source").unwrap();
    z.finish().unwrap().into_inner()
}
fn item(s: &State) -> serde_json::Value {
    serde_json::json!({"drivewsid":SOURCE,"docwsid":"native","zone":if s.foreign_zone{"other.zone"}else{"com.apple.CloudDocs"},"type":"FILE","name":"Owned","extension":"pages","parentId":ROOT_ID,"size":19,"etag":if s.stale||(s.change_after_lookup&&s.lookups>0){"v2"}else{"v1"}})
}
impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in("/var/tmp")
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let state = Arc::new(StdMutex::new(State::default()));
        let observed = state.clone();
        let server = tokio::spawn(async move {
            let mut peers = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                            result = peers.join_next(), if !peers.is_empty() => {
                                result.unwrap().expect("resolver fixture peer task");
                            }
                            accepted = listener.accept(), if peers.len() < 8 => {
                                let (mut peer, _) = accepted.unwrap();
                                let observed = observed.clone();
                                peers.spawn(async move {
                                    // An idle speculative connection must not block other
                                    // requests. Each peer is bounded, and JoinSet aborts
                                    // remaining peers when the fixture is dropped.
                                    let _deadline = tokio::time::timeout(Duration::from_secs(3),async {
                let mut bytes=Vec::new();let path=loop {let mut buf=[0;4096];let n=peer.read(&mut buf).await.unwrap();if n==0 && bytes.is_empty(){return;}assert!(n>0);bytes.extend_from_slice(&buf[..n]);assert!(bytes.len()<32768);
                  let Some(end)=bytes.windows(4).position(|v|v==b"\r\n\r\n")else{continue};let header=std::str::from_utf8(&bytes[..end]).unwrap();
                  let len=header.lines().find_map(|l|l.to_ascii_lowercase().strip_prefix("content-length: ").map(str::to_owned)).map(|v|v.parse::<usize>().unwrap()).unwrap_or(0);
                  if bytes.len()>=end+4+len{break header.lines().next().unwrap().split_whitespace().nth(1).unwrap().to_owned()}
                };
                let body={let mut s=observed.lock().unwrap();s.requests+=1;match path.split('?').next().unwrap(){
                  "/retrieveItemDetailsInFolders"=>{let items=if s.missing{vec![]}else{vec![item(&s)]};serde_json::json!([{"drivewsid":ROOT_ID,"zone":"com.apple.CloudDocs","type":if s.app_parent{"APP_CONTAINER"}else if s.app_library{"APP_LIBRARY"}else{"FOLDER"},"name":"iCloud Drive","numberOfItems":items.len(),"items":items}])},
                  "/retrieveItemDetails"=>{s.details+=1;serde_json::json!({"items":[item(&s)]})},
                  "/ws/com.apple.CloudDocs/download/by_id"=>{s.lookups+=1;let field=if s.data{"data_token"}else{"package_token"};serde_json::json!({field:{"url":"https://fixture.icloud-content.com/not-requested"}})},
                  _=>panic!("unexpected resolver request; cached archive must not download"),
                }};let body=body.to_string();peer.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await.unwrap();peer.write_all(body.as_bytes()).await.unwrap();
                                    }).await;
                                });
                            }
                        }
            }
        });
        let scope = Scope {
            account: Uuid::new_v4().to_string(),
            provider: "icloud".into(),
            collection: "drive".into(),
        };
        let mut session = ICloudReadSession::new().unwrap();
        session.drive_endpoint = Some(format!("http://{address}/").parse().unwrap());
        session.docs_endpoint = session.drive_endpoint.clone();
        // No production/external endpoint can be contacted even if a negative
        // control accidentally misses the artifact cache.
        session.http = reqwest::Client::builder()
            .no_proxy()
            .resolve("fixture.icloud-content.com", address)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(1))
            .build()
            .unwrap();
        let drive = ICloudDrive::on_demand_from_live_session(scope.clone(), session)
            .unwrap()
            .with_package_artifacts(dir.path(), 64 * 1024 * 1024)
            .unwrap();
        let bytes = bytes();
        let raw_hash = hex::encode(Sha256::digest(&bytes));
        let revision = Revision {
            source_etag: "v1".into(),
            source_size: 19,
            source_parent: ROOT_ID.into(),
            sha256: raw_hash.clone(),
        };
        let archive = Node {
            id: format!("{ITEM}{SOURCE}"),
            parent_id: Some(SOURCE.into()),
            name: "Owned.pages".into(),
            kind: NodeKind::File,
            size: bytes.len() as u64,
            modified_unix: 0,
            etag: None,
            content_version: Some(format!(
                "{VERSION}{}",
                serde_json::to_string(&revision).unwrap()
            )),
            target: None,
            package: false,
        };
        let packages = drive.packages.as_ref().unwrap();
        let file = packages.stage_file().await.unwrap();
        file.write_all_at(&bytes, 0).unwrap();
        let semantic = crate::package_archive_semantic_identity_versioned(
            &file,
            &crate::PackageDownload {
                size: archive.size,
                sha256: raw_hash.clone(),
            },
            &archive.name,
            2,
            &CancellationToken::new(),
        )
        .unwrap();
        let disk = DiskArtifact::new(
            ReadIdentity::new(&scope, &archive).unwrap(),
            file,
            &raw_hash,
        )
        .unwrap();
        packages
            .artifacts
            .lock()
            .await
            .push_back((archive.clone(), Arc::new(disk)));
        Self {
            drive: Arc::new(drive),
            scope,
            archive,
            semantic,
            state,
            server,
            _dir: dir,
        }
    }
    async fn resolve(&self) -> Outcome<Option<NativeArchiveBinding>> {
        self.drive
            .resolve_native_archive(&self.scope, &self.archive, &CancellationToken::new())
            .await
    }
}
#[tokio::test]
async fn exact_cached_archive_resolves_source_and_semantics_without_duplicate_download() {
    let f = Fixture::new().await;
    for _ in 0..2 {
        let binding = f.resolve().await.unwrap().unwrap();
        assert_eq!(binding.scope, f.scope);
        assert_eq!(binding.archive, f.archive);
        assert_eq!(binding.semantic, f.semantic);
        assert_eq!(binding.semantic.version, 2);
        assert_eq!(binding.source.id, SOURCE);
        assert_eq!(binding.source.parent_id.as_deref(), Some(ROOT_ID));
        assert_eq!(binding.source.etag.as_deref(), Some("v1"));
        assert_eq!(binding.source.kind, NodeKind::Folder);
        assert!(binding.source.package);
        assert_eq!(binding.source.size, 19);
        assert_ne!(binding.source.size, binding.archive.size);
    }
    {
        let s = f.state.lock().unwrap();
        assert_eq!(s.lookups, 2);
        assert_eq!(s.details, 4);
        assert_eq!(s.requests, 10);
    }
    // Resolving is not ordinary-write admission.
    assert!(matches!(
        f.drive
            .validate_write_target(&f.scope, &f.archive, &CancellationToken::new())
            .await,
        Err(ProviderError::Permission)
    ));
}
#[tokio::test]
async fn ordinary_files_native_roots_and_app_containers_are_not_archive_capabilities() {
    let f = Fixture::new().await;
    for (id, kind, package) in [
        (SOURCE, NodeKind::File, false),
        (SOURCE, NodeKind::Folder, true),
        (
            "APP_CONTAINER::com.apple.Pages::documents",
            NodeKind::Folder,
            true,
        ),
        (
            "FOLDER::com.apple.CloudDocs::ordinary",
            NodeKind::Folder,
            false,
        ),
    ] {
        let mut node = f.archive.clone();
        node.id = id.into();
        node.kind = kind;
        node.package = package;
        assert!(
            f.drive
                .resolve_native_archive(&f.scope, &node, &CancellationToken::new())
                .await
                .unwrap()
                .is_none()
        );
    }
    assert_eq!(f.state.lock().unwrap().requests, 0);
}
#[tokio::test]
async fn data_representation_app_parent_and_foreign_source_never_return_a_binding() {
    for mode in ["data", "app", "library", "zone", "missing", "stale"] {
        let f = Fixture::new().await;
        {
            let mut s = f.state.lock().unwrap();
            match mode {
                "data" => s.data = true,
                "app" => s.app_parent = true,
                "library" => s.app_library = true,
                "zone" => s.foreign_zone = true,
                "missing" => s.missing = true,
                _ => s.stale = true,
            }
        }
        assert!(f.resolve().await.is_err(), "accepted {mode}");
    }
}
#[tokio::test]
async fn forged_archive_identity_and_foreign_scope_refuse_without_network() {
    let f = Fixture::new().await;
    for mode in ["id", "parent", "kind", "package", "etag", "name", "version"] {
        let mut node = f.archive.clone();
        match mode {
            "id" => node.id = format!("{ITEM}FILE::foreign::native"),
            "parent" => node.parent_id = Some("FILE::com.apple.CloudDocs::other".into()),
            "kind" => node.kind = NodeKind::Folder,
            "package" => node.package = true,
            "etag" => node.etag = Some("v1".into()),
            "name" => node.name = "../Owned.pages".into(),
            _ => node.content_version = Some("x".repeat(8193)),
        }
        assert!(
            f.drive
                .resolve_native_archive(&f.scope, &node, &CancellationToken::new())
                .await
                .is_err(),
            "accepted {mode}"
        );
    }
    let mut scope = f.scope.clone();
    scope.account = Uuid::new_v4().to_string();
    assert!(matches!(
        f.drive
            .resolve_native_archive(&scope, &f.archive, &CancellationToken::new())
            .await,
        Err(ProviderError::Permission)
    ));
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        f.drive
            .resolve_native_archive(&f.scope, &f.archive, &cancel)
            .await,
        Err(ProviderError::Cancelled)
    ));
    assert_eq!(f.state.lock().unwrap().requests, 0);
}
#[tokio::test]
async fn revision_change_during_resolution_refuses_cached_semantic_proof() {
    let f = Fixture::new().await;
    f.state.lock().unwrap().change_after_lookup = true;
    assert!(matches!(
        f.resolve().await,
        Err(ProviderError::VersionChanged)
    ));
    let s = f.state.lock().unwrap();
    assert_eq!(s.lookups, 1);
    assert!(s.requests >= 4);
}
#[tokio::test]
async fn cached_archive_corruption_cannot_produce_semantic_authority() {
    let f = Fixture::new().await;
    {
        let cache = f.drive.packages.as_ref().unwrap().artifacts.lock().await;
        cache[0].1.file.write_all_at(b"broken", 0).unwrap();
    }
    assert!(f.resolve().await.is_err());
    assert_eq!(f.state.lock().unwrap().lookups, 1);
}

#[tokio::test]
async fn forged_same_zone_source_id_and_name_refuse_before_representation_lookup() {
    for change_id in [true, false] {
        let f = Fixture::new().await;
        let mut archive = f.archive.clone();
        if change_id {
            archive.id = format!("{ITEM}FILE::com.apple.CloudDocs::other");
            archive.parent_id = Some("FILE::com.apple.CloudDocs::other".into());
        } else {
            archive.name = "Other.pages".into();
        }
        assert!(matches!(
            f.drive
                .resolve_native_archive(&f.scope, &archive, &CancellationToken::new())
                .await,
            Err(ProviderError::VersionChanged)
        ));
        assert_eq!(f.state.lock().unwrap().lookups, 0);
    }
}

#[tokio::test]
async fn cached_resolution_capacity_outlives_cancelled_parser_waiters() {
    use std::sync::atomic::Ordering;
    let f = Fixture::new().await;
    let packages = f.drive.packages.as_ref().unwrap();
    let probe = Arc::new(ParserProbe::default());
    *packages.resolution_probe.lock().await = Some(probe.clone());
    let start = |cancel: CancellationToken| {
        let drive = f.drive.clone();
        let scope = f.scope.clone();
        let archive = f.archive.clone();
        tokio::spawn(async move {
            drive
                .resolve_native_archive(&scope, &archive, &cancel)
                .await
        })
    };
    let cancelled = CancellationToken::new();
    let first = start(cancelled.clone());
    let second = start(CancellationToken::new());
    while probe.entered.load(Ordering::SeqCst) < 2 {
        tokio::time::timeout(Duration::from_secs(3), probe.notify.notified())
            .await
            .unwrap();
    }
    assert_eq!(packages.resolutions.available_permits(), 0);
    let before = f.state.lock().unwrap().requests;
    let token = CancellationToken::new();
    {
        let third = f.drive.resolve_native_archive(&f.scope, &f.archive, &token);
        tokio::pin!(third);
        // Drop this waiter before cancelling the first call. Otherwise a queued
        // acquisition can consume an incorrectly released permit invisibly.
        tokio::select! {biased; _=&mut third=>panic!("resolver exceeded capacity"), _=tokio::task::yield_now()=>{}}
        assert_eq!(f.state.lock().unwrap().requests, before);
    }
    cancelled.cancel();
    assert!(matches!(
        first.await.unwrap(),
        Err(ProviderError::Cancelled)
    ));
    // The detached blocking parser still owns its slot after its waiter exits.
    assert_eq!(
        packages.resolutions.available_permits(),
        0,
        "cancelled waiter released a still-running blocking parser slot"
    );
    let third = f.drive.resolve_native_archive(&f.scope, &f.archive, &token);
    tokio::pin!(third);
    tokio::select! {biased; _=&mut third=>panic!("cancel released active parser slot"), _=tokio::task::yield_now()=>{}}
    assert_eq!(f.state.lock().unwrap().requests, before);
    probe.release();
    assert!(second.await.unwrap().unwrap().is_some());
    assert!(third.await.unwrap().is_some());
    assert_eq!(probe.entered.load(Ordering::SeqCst), 3);
    // Join capacity rather than assuming the cancelled blocking task already
    // exited just because the other two async calls completed.
    let drained = tokio::time::timeout(
        Duration::from_secs(3),
        packages.resolutions.clone().acquire_many_owned(2),
    )
    .await
    .unwrap()
    .unwrap();
    drop(drained);
    assert_eq!(packages.resolutions.available_permits(), 2);
}
