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
    assert!(settings.validate().is_err());
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
