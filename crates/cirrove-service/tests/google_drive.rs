//! The real Google HTTP adapter through the existing store, cache, engine and
//! kernel mount. Synthetic loopback only; no Google credentials or cloud writes.
#![allow(clippy::unwrap_used)]
#[path = "google_drive/fixture.rs"]
mod fixture;
use cirrove_auth::{AccessMode, AppRegistration, Identity};
use cirrove_core::{
    CancellationToken, Change, ChangePage, Checkpoint, CollectionInfo, Cursor, ReadProvider, Scope,
};
use cirrove_googledrive::GoogleDrive;
use cirrove_service::{
    accounts::{Account, Settings},
    content::ContentCache,
    engine::Engine,
    filesystem::CloudFs,
    refresh,
};
use cirrove_store::Store;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
const ACCOUNT: &str = "00000000-0000-4000-8000-000000000001";
fn scope(provider: &str) -> Scope {
    Scope {
        account: ACCOUNT.into(),
        provider: provider.into(),
        collection: "root-id".into(),
    }
}
fn account(mount_path: PathBuf) -> Account {
    Account {
        id: ACCOUNT.into(),
        label: "google-fixture".into(),
        registration: AppRegistration::Google {
            client_id: "123-example.apps.googleusercontent.com".into(),
        },
        identity: Identity {
            tenant_id: String::new(),
            subject: "synthetic-user".into(),
            username: "fixture@example.invalid".into(),
            graph_user_id: "synthetic-user".into(),
            display_name: "Fixture".into(),
        },
        credential_id: "00000000-0000-4000-8000-000000000002".into(),
        access: AccessMode::ReadOnly,
        drive: CollectionInfo {
            id: "root-id".into(),
            name: "My Drive".into(),
            drive_type: "my_drive".into(),
            web_url: "https://drive.google.com/drive/my-drive".into(),
        },
        root_id: "root-id".into(),
        mount_path,
        enabled: true,
        poll_seconds: 30,
        cache_bytes: 16 * 1024 * 1024,
    }
}

#[tokio::test]
#[ignore = "reads the locally connected Google account and lists accessible drives"]
async fn live_google_shared_drive_discovery() {
    let state = cirrove_service::state_dir().unwrap();
    let settings = Settings::load(&state).unwrap();
    let google = settings
        .accounts
        .iter()
        .find(|account| matches!(account.registration, AppRegistration::Google { .. }))
        .expect("no connected Google account");
    let provider = cirrove_service::accounts::google_provider(google).unwrap();
    let drives = provider
        .collections(&CancellationToken::new())
        .await
        .unwrap();
    let shared = drives
        .iter()
        .filter(|drive| drive.drive_type == "shared_drive")
        .count();
    println!("accessible shared drives: {shared}");
}

#[tokio::test]
async fn shared_drive_refresh_keeps_its_root_and_content_in_its_collection() {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    let temp = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let mut request = Vec::new();
            loop {
                let mut chunk = [0; 2048];
                let count = socket.read(&mut chunk).await.unwrap();
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..count]);
                if request.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
                assert!(request.len() < 65536);
            }
            let request = String::from_utf8(request).unwrap();
            let path = request.split_ascii_whitespace().nth(1).unwrap();
            assert!(request.starts_with("GET "));
            let route = path.split('?').next().unwrap();
            let has = |key: &str, value: &str| path.contains(&format!("{key}={value}"));
            let (status, extra_headers, body) = match route {
                "/drive/v3/changes/startPageToken" => {
                    assert!(has("driveId", "shared-root"));
                    assert!(has("supportsAllDrives", "true"));
                    (200, String::new(), serde_json::to_vec(&json!({"startPageToken":"first"})).unwrap())
                }
                "/drive/v3/files" => {
                    assert!(has("corpora", "drive"));
                    assert!(has("driveId", "shared-root"));
                    assert!(has("supportsAllDrives", "true"));
                    (200, String::new(), serde_json::to_vec(&json!({"files":[{"id":"shared-file","name":"report.txt","mimeType":"text/plain","parents":["shared-root"],"size":"6","version":"1","headRevisionId":"r1","driveId":"shared-root","capabilities":{"canDownload":true}}]})).unwrap())
                }
                "/drive/v3/files/shared-root" => {
                    assert!(has("supportsAllDrives", "true"));
                    (200, String::new(), serde_json::to_vec(&json!({"id":"shared-root","name":"Team","mimeType":"application/vnd.google-apps.folder","version":"1","driveId":"shared-root"})).unwrap())
                }
                "/drive/v3/changes" => {
                    assert!(has("driveId", "shared-root"));
                    (200, String::new(), serde_json::to_vec(&json!({"changes":[],"newStartPageToken":"second"})).unwrap())
                }
                "/drive/v3/files/shared-file" if has("alt", "media") => {
                    assert!(has("supportsAllDrives", "true"));
                    (206, "Content-Range: bytes 0-5/6\r\n".into(), b"shared".to_vec())
                }
                "/drive/v3/files/shared-file" => {
                    (200, String::new(), serde_json::to_vec(&json!({"id":"shared-file","name":"report.txt","mimeType":"text/plain","parents":["shared-root"],"size":"6","version":"1","headRevisionId":"r1","driveId":"shared-root","capabilities":{"canDownload":true}})).unwrap())
                }
                _ => panic!("unexpected synthetic request path"),
            };
            let header = format!(
                "HTTP/1.1 {status} Synthetic\r\nContent-Length: {}\r\nConnection: close\r\n{extra_headers}\r\n",
                body.len()
            );
            if socket.write_all(header.as_bytes()).await.is_ok() {
                let _ = socket.write_all(&body).await;
            }
        }
    });
    let collection = CollectionInfo {
        id: "shared-root".into(),
        name: "Team".into(),
        drive_type: "shared_drive".into(),
        web_url: String::new(),
    };
    let provider = GoogleDrive::synthetic_loopback(
        ACCOUNT.into(),
        "shared-root".into(),
        &format!("{origin}/drive/v3/"),
    )
    .unwrap()
    .for_collection(&collection)
    .unwrap();
    let scope = Scope {
        account: ACCOUNT.into(),
        provider: "googledrive".into(),
        collection: "shared-root".into(),
    };
    let cancel = CancellationToken::new();
    let db = temp.path().join("metadata.db");
    refresh(&provider, &scope, &db, false, &cancel, None)
        .await
        .unwrap();
    let store = Store::open(&db).unwrap();
    assert!(store.node(&scope, "shared-root").unwrap().is_some());
    let node = store.node(&scope, "shared-file").unwrap().unwrap();
    assert_eq!(node.parent_id.as_deref(), Some("shared-root"));
    let cache = ContentCache::new(
        temp.path().join("cache"),
        temp.path().join("blocks.db"),
        16 * 1024 * 1024,
    )
    .unwrap();
    assert_eq!(
        cache
            .read(&provider, &scope, &node, 0, 6, &cancel)
            .await
            .unwrap(),
        b"shared"
    );
    task.abort();
}

#[tokio::test]
async fn interrupted_google_baseline_resumes_and_publishes_metadata_with_its_cursor() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("metadata.db");
    let server = fixture::Server::new().await;
    let scope = scope("googledrive");
    let cancel = CancellationToken::new();
    let old = server
        .google
        .node(&scope, "same-id", &cancel)
        .await
        .unwrap();
    let cursor = Cursor(
        json!({"scope":scope,"phase":{"phase":"changes","page":"old-complete"}}).to_string(),
    );
    let mut store = Store::open(&db).unwrap();
    store.begin(&scope, false).unwrap();
    store
        .stage(
            &scope,
            None,
            &ChangePage {
                changes: vec![Change::Upsert(old.clone())],
                checkpoint: Checkpoint::Complete(cursor.clone()),
            },
        )
        .unwrap();
    server.fail_page.store(true, Ordering::SeqCst);
    assert!(
        refresh(server.google.as_ref(), &scope, &db, true, &cancel, None)
            .await
            .is_err()
    );
    assert_eq!(store.node(&scope, "same-id").unwrap(), Some(old.clone()));
    assert_eq!(store.cursor(&scope).unwrap(), Some(cursor.clone()));
    drop(store);
    server.fail_catchup.store(true, Ordering::SeqCst);
    assert!(
        refresh(server.google.as_ref(), &scope, &db, false, &cancel, None)
            .await
            .is_err()
    );
    let store = Store::open(&db).unwrap();
    assert_eq!(store.cursor(&scope).unwrap(), Some(cursor));
    assert!(
        store.node(&scope, "other-id").unwrap().is_none(),
        "staged listing leaked before catch-up"
    );
    drop(store);
    refresh(server.google.as_ref(), &scope, &db, false, &cancel, None)
        .await
        .unwrap();
    let store = Store::open(&db).unwrap();
    assert_eq!(store.node(&scope, "same-id").unwrap(), Some(old));
    assert!(store.node(&scope, "other-id").unwrap().is_some());
    assert!(store.cursor(&scope).unwrap().unwrap().0.contains("settled"));
    assert_eq!(
        server.starts.load(Ordering::SeqCst),
        1,
        "resuming must retain the original frontier"
    );
}

#[tokio::test]
async fn both_real_adapters_keep_identical_ids_and_versions_separate_in_one_store_and_cache() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("metadata.db");
    let server = fixture::Server::new().await;
    let cancel = CancellationToken::new();
    let gs = scope("googledrive");
    let ms = scope("onedrive");
    refresh(server.google.as_ref(), &gs, &db, false, &cancel, None)
        .await
        .unwrap();
    refresh(server.onedrive.as_ref(), &ms, &db, false, &cancel, None)
        .await
        .unwrap();
    let store = Store::open(&db).unwrap();
    let g = store.node(&gs, "same-id").unwrap().unwrap();
    let m = store.node(&ms, "same-id").unwrap().unwrap();
    assert_eq!(
        g.content_revision(),
        m.content_revision(),
        "only provider may separate these cache identities"
    );
    assert_ne!(g.name, m.name);
    assert_eq!(store.children(&gs, "root-id").unwrap().unwrap().len(), 2);
    assert_eq!(store.children(&ms, "root-id").unwrap().unwrap().len(), 1);
    let path = temp.path().join("cache");
    let blocks = temp.path().join("blocks.db");
    let cache = ContentCache::new(path.clone(), blocks.clone(), 16 * 1024 * 1024).unwrap();
    assert_eq!(
        cache
            .read(server.google.as_ref(), &gs, &g, 0, 6, &cancel)
            .await
            .unwrap(),
        b"google"
    );
    assert_eq!(
        cache
            .read(server.onedrive.as_ref(), &ms, &m, 0, 6, &cancel)
            .await
            .unwrap(),
        b"onedrv"
    );
    assert_eq!(server.media.load(Ordering::SeqCst), 2);
    drop(cache);
    server.offline();
    let cache = ContentCache::new(path, blocks, 16 * 1024 * 1024).unwrap();
    assert_eq!(
        cache
            .read(server.google.as_ref(), &gs, &g, 0, 6, &cancel)
            .await
            .unwrap(),
        b"google"
    );
    assert_eq!(
        cache
            .read(server.onedrive.as_ref(), &ms, &m, 0, 6, &cancel)
            .await
            .unwrap(),
        b"onedrv"
    );
}

#[test]
fn settings_migrate_old_microsoft_in_memory_and_require_explicit_provider_in_v2() {
    let temp = tempfile::tempdir().unwrap();
    let mut a = account(temp.path().join("mount"));
    a.registration = AppRegistration::Microsoft {
        client_id: "00000000-0000-4000-8000-000000000003".into(),
        authority: "common".into(),
    };
    let mut legacy = serde_json::to_value(Settings {
        version: 2,
        accounts: vec![a],
    })
    .unwrap();
    legacy["version"] = 1.into();
    legacy["accounts"][0]["registration"]
        .as_object_mut()
        .unwrap()
        .remove("provider");
    let bytes = serde_json::to_vec(&legacy).unwrap();
    let path = temp.path().join("accounts.json");
    std::fs::write(&path, &bytes).unwrap();
    let settings = Settings::load(temp.path()).unwrap();
    assert_eq!(settings.version, 2);
    assert_eq!(settings.accounts[0].registration.provider_id(), "onedrive");
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    legacy["version"] = 2.into();
    std::fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();
    assert!(Settings::load(temp.path()).is_err());
    let a = account(temp.path().join("google"));
    let mut settings = Settings {
        version: 2,
        accounts: vec![a],
    };
    settings.validate().unwrap();
    assert!(cirrove_service::accounts::onedrive_provider(&settings.accounts[0]).is_err());
    settings.accounts[0].access = AccessMode::ReadWrite;
    settings
        .validate()
        .expect("an enabled Google My Drive write connection is valid");
    assert!(cirrove_service::accounts::provider(&settings.accounts[0]).is_ok());
    settings.accounts[0].drive.drive_type = "shared_drive".into();
    assert!(settings.validate().is_err());
    assert!(cirrove_service::accounts::write_provider(&settings.accounts[0]).is_err());
    settings.accounts[0].access = AccessMode::ReadOnly;
    settings
        .validate()
        .expect("a read-only shared drive is valid");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires /dev/fuse; synthetic Google HTTP only"]
async fn real_google_mount_uses_shared_engine_preserves_duplicates_and_reopens_offline() {
    use std::os::unix::fs::MetadataExt;
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let state = temp.path().join("state");
    let config = account(mount.clone());
    let server = fixture::Server::new().await;
    let engine = Engine::new(config.clone(), server.google.clone(), state.clone())
        .await
        .unwrap();
    refresh(
        server.google.as_ref(),
        &scope("googledrive"),
        &engine.db,
        false,
        &engine.cancel,
        None,
    )
    .await
    .unwrap();
    let session = CloudFs::new(engine.clone()).unwrap().mount(&mount).unwrap();
    let path = mount.clone();
    let result = tokio::task::spawn_blocking(move || -> std::io::Result<u64> {
        let mut names = std::fs::read_dir(&path)?
            .map(|e| e.map(|e| e.file_name()))
            .collect::<std::io::Result<Vec<_>>>()?;
        names.sort();
        assert_eq!(names, vec!["same [other-id].txt", "same [same-id].txt"]);
        assert_eq!(std::fs::read(path.join("same [same-id].txt"))?, b"google");
        assert_eq!(std::fs::read(path.join("same [other-id].txt"))?, b"google");
        assert!(
            std::fs::OpenOptions::new()
                .write(true)
                .open(path.join("same [same-id].txt"))
                .is_err()
        );
        Ok(std::fs::metadata(path.join("same [same-id].txt"))?.ino())
    })
    .await;
    engine.stop().await;
    tokio::task::spawn_blocking(move || session.umount_and_join())
        .await
        .unwrap()
        .unwrap();
    drop(engine);
    let inode = result.unwrap().unwrap();
    let before = server.requests.load(Ordering::SeqCst);
    server.offline();
    let engine = Engine::new(config, server.google.clone(), state)
        .await
        .unwrap();
    let session = CloudFs::new(engine.clone()).unwrap().mount(&mount).unwrap();
    let result = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        assert_eq!(
            std::fs::metadata(mount.join("same [same-id].txt"))?.ino(),
            inode
        );
        assert_eq!(std::fs::read(mount.join("same [same-id].txt"))?, b"google");
        assert_eq!(std::fs::read_dir(&mount)?.count(), 2);
        Ok(())
    })
    .await;
    engine.stop().await;
    tokio::task::spawn_blocking(move || session.umount_and_join())
        .await
        .unwrap()
        .unwrap();
    result.unwrap().unwrap();
    assert_eq!(server.requests.load(Ordering::SeqCst), before);
}
