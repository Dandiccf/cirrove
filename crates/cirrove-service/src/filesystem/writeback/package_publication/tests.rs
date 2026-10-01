#![allow(clippy::unwrap_used)]
use super::*;
use crate::journal::{PackagePublicationStatus, UploadState};
use cirrove_core::{
    ChangePage, Cursor, DirectoryPage, MetadataProvider, ReadProvider,
    upload::{PackageSemanticIdentity, PackageUploadReceipt, UploadIntent},
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
fn native() -> Node {
    Node {
        id: "native".into(),
        parent_id: Some("root".into()),
        name: "Native.pages".into(),
        kind: NodeKind::Folder,
        size: 17,
        modified_unix: 0,
        etag: Some("v1".into()),
        content_version: Some("v1".into()),
        target: None,
        package: true,
    }
}
struct Remote {
    response: Mutex<std::result::Result<Node, ProviderError>>,
    reads: AtomicUsize,
    listings: AtomicUsize,
    pause: AtomicBool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    journal: Mutex<Weak<Mutex<UploadJournal>>>,
}
#[async_trait::async_trait]
impl MetadataProvider for Remote {
    fn provider_id(&self) -> &'static str {
        "fixture"
    }
    async fn changes(
        &self,
        _: &Scope,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<ChangePage, ProviderError> {
        panic!("no delta required")
    }
}
#[async_trait::async_trait]
impl ReadProvider for Remote {
    async fn node(
        &self,
        _: &Scope,
        id: &str,
        _: &CancellationToken,
    ) -> std::result::Result<Node, ProviderError> {
        assert_eq!(id, "native");
        self.reads.fetch_add(1, Ordering::SeqCst);
        let journal = self.journal.lock().unwrap().upgrade().unwrap();
        assert!(
            journal.try_lock().is_ok(),
            "network request must not retain journal lock"
        );
        let result = self.response.lock().unwrap().clone();
        if self.pause.load(Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        result
    }
    async fn children(
        &self,
        _: &Scope,
        parent: &str,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<DirectoryPage, ProviderError> {
        assert_eq!(parent, "root");
        self.listings.fetch_add(1, Ordering::SeqCst);
        Ok(DirectoryPage {
            nodes: vec![],
            next: None,
        })
    }
    async fn read_range(
        &self,
        _: &Scope,
        _: &Node,
        _: u64,
        _: u32,
        _: &CancellationToken,
    ) -> std::result::Result<Vec<u8>, ProviderError> {
        panic!("publication must not download contents")
    }
}
async fn fixture() -> (
    tempfile::TempDir,
    Arc<Engine>,
    Arc<Remote>,
    Arc<Mutex<UploadJournal>>,
    Arc<Writeback>,
) {
    let temp = tempfile::tempdir().unwrap();
    let id = Uuid::new_v4().to_string();
    let account = crate::accounts::Account {
        id: id.clone(),
        label: "package-publication".into(),
        registration: cirrove_auth::AppRegistration::ICloud,
        identity: cirrove_auth::Identity {
            tenant_id: "fixture".into(),
            subject: "synthetic".into(),
            username: "fixture@example.invalid".into(),
            graph_user_id: "fixture".into(),
            display_name: "Fixture".into(),
        },
        credential_id: Uuid::new_v4().to_string(),
        access: cirrove_auth::AccessMode::ReadWrite,
        drive: cirrove_core::CollectionInfo {
            id: "drive".into(),
            name: "Fixture".into(),
            drive_type: "icloud_drive".into(),
            web_url: "https://example.invalid".into(),
        },
        root_id: "root".into(),
        mount_path: temp.path().join("mount"),
        enabled: true,
        poll_seconds: 3600,
        cache_bytes: 8 * 1024 * 1024,
    };
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&temp.path().join("journal"), &id, 1024 * 1024).unwrap(),
    ));
    let provider = Arc::new(Remote {
        response: Mutex::new(Ok(native())),
        reads: AtomicUsize::new(0),
        listings: AtomicUsize::new(0),
        pause: AtomicBool::new(false),
        entered: Default::default(),
        release: Default::default(),
        journal: Mutex::new(Arc::downgrade(&journal)),
    });
    let engine = Engine::new(account, provider.clone(), temp.path().join("state"))
        .await
        .unwrap();
    let root = Node {
        id: "root".into(),
        parent_id: None,
        name: "Root".into(),
        kind: NodeKind::Folder,
        package: false,
        ..native()
    };
    cirrove_store::Store::open(&engine.db)
        .unwrap()
        .observe_node(&engine.scope("drive"), &root)
        .unwrap();
    let writer = Writeback::new(&engine, journal.clone()).await.unwrap();
    (temp, engine, provider, journal, writer)
}
fn acknowledge(journal: &Arc<Mutex<UploadJournal>>, scope: Scope) -> crate::journal::UploadRecord {
    let semantic = PackageSemanticIdentity {
        version: 1,
        sha256: "a".repeat(64),
        entries: 1,
        files: 1,
        expanded_bytes: 3,
    };
    let mut j = journal.lock().unwrap();
    let row = j
        .enqueue_package_archive(
            scope,
            UploadIntent::Create {
                parent: "root".into(),
                name: "Native.pages".into(),
            },
            "Source.pages".into(),
            semantic.clone(),
            &b"zip"[..],
        )
        .unwrap();
    let claimed = j.claim_next().unwrap().unwrap();
    assert_eq!(claimed.id, row.id);
    j.acknowledge_package(
        row.id,
        claimed.attempt.unwrap(),
        PackageUploadReceipt {
            remote: native(),
            semantic,
        },
    )
    .unwrap();
    j.get(row.id).unwrap()
}
#[tokio::test]
async fn package_acknowledgement_publishes_into_warm_directory_without_content_or_receipt_rewrite()
{
    let (_temp, engine, remote, journal, writer) = fixture().await;
    let scope = engine.scope("drive");
    assert!(engine.children(&scope, "root").await.unwrap().is_empty());
    let row = acknowledge(&journal, scope.clone());
    let original = serde_json::to_string(&row).unwrap();
    assert_eq!(
        journal
            .lock()
            .unwrap()
            .package_publication_status(row.id)
            .unwrap(),
        PackagePublicationStatus::Pending
    );
    assert!(writer.publish_completed_package(&engine).await.unwrap());
    assert_eq!(
        engine.children(&scope, "root").await.unwrap(),
        vec![native()]
    );
    assert_eq!(remote.listings.load(Ordering::SeqCst), 1);
    assert_eq!(
        journal
            .lock()
            .unwrap()
            .package_publication_status(row.id)
            .unwrap(),
        PackagePublicationStatus::Present(native())
    );
    assert_eq!(
        serde_json::to_string(&journal.lock().unwrap().get(row.id).unwrap()).unwrap(),
        original
    );
    assert!(!writer.publish_completed_package(&engine).await.unwrap());
    assert_eq!(remote.reads.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn existing_schema15_ack_before_publication_converges_after_reopen() {
    let (temp, engine, remote, journal, writer) = fixture().await;
    let row = acknowledge(&journal, engine.scope("drive"));
    let original = serde_json::to_string(&row).unwrap();
    drop(writer);
    drop(journal);
    // Emulate an existing schema15 binary that acknowledged before this additive
    // queue existed. This is solely the private fixture's local SQLite file.
    {
        let db = rusqlite::Connection::open(temp.path().join("journal/uploads.db")).unwrap();
        db.execute_batch("DROP TRIGGER package_metadata_on_insert; DROP TRIGGER package_metadata_on_update; DROP TABLE package_metadata_publication;").unwrap();
        // The fixture is created by the current writer; explicitly restore the
        // legacy version for this migration arm (there are no native intents).
        db.pragma_update(None, "user_version", 15).unwrap();
        let version: u32 = db
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, 15);
    }
    let reopened = Arc::new(Mutex::new(
        UploadJournal::open(
            &temp.path().join("journal"),
            &engine.account.id,
            1024 * 1024,
        )
        .unwrap(),
    ));
    *remote.journal.lock().unwrap() = Arc::downgrade(&reopened);
    let writer = Writeback::new(&engine, reopened.clone()).await.unwrap();
    assert!(writer.publish_completed_package(&engine).await.unwrap());
    assert_eq!(
        reopened.lock().unwrap().get(row.id).unwrap().state,
        UploadState::Uploaded
    );
    assert_eq!(
        serde_json::to_string(&reopened.lock().unwrap().get(row.id).unwrap()).unwrap(),
        original
    );
    assert_eq!(
        reopened
            .lock()
            .unwrap()
            .package_publication_status(row.id)
            .unwrap(),
        PackagePublicationStatus::Present(native())
    );
}
#[tokio::test]
async fn failed_metadata_refresh_cools_down_without_replaying_or_changing_upload() {
    let (_temp, engine, remote, journal, writer) = fixture().await;
    let row = acknowledge(&journal, engine.scope("drive"));
    *remote.response.lock().unwrap() = Err(ProviderError::Unavailable);
    assert!(writer.publish_completed_package(&engine).await.is_err());
    assert!(!writer.publish_completed_package(&engine).await.unwrap());
    assert_eq!(remote.reads.load(Ordering::SeqCst), 1);
    assert_eq!(
        journal.lock().unwrap().get(row.id).unwrap().state,
        UploadState::Uploaded
    );
    assert_eq!(
        journal
            .lock()
            .unwrap()
            .package_publication_status(row.id)
            .unwrap(),
        PackagePublicationStatus::Pending
    );
    assert!(
        journal
            .lock()
            .unwrap()
            .package_publication_due(publication_now() + 61)
            .unwrap()
            .is_some()
    );
}
#[tokio::test]
async fn deleted_package_converges_to_absence_without_upload_replay() {
    let (_temp, engine, remote, journal, writer) = fixture().await;
    let scope = engine.scope("drive");
    let row = acknowledge(&journal, scope.clone());
    cirrove_store::Store::open(&engine.db)
        .unwrap()
        .observe_node(&scope, &native())
        .unwrap();
    *remote.response.lock().unwrap() = Err(ProviderError::NotFound);
    assert!(writer.publish_completed_package(&engine).await.unwrap());
    assert!(
        cirrove_store::Store::open(&engine.db)
            .unwrap()
            .node(&scope, "native")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        journal
            .lock()
            .unwrap()
            .package_publication_status(row.id)
            .unwrap(),
        PackagePublicationStatus::Absent
    );
    assert!(!writer.publish_completed_package(&engine).await.unwrap());
}
#[tokio::test]
async fn later_metadata_observation_wins_over_inflight_package_receipt_refresh() {
    let (_temp, engine, remote, journal, writer) = fixture().await;
    let scope = engine.scope("drive");
    let row = acknowledge(&journal, scope.clone());
    remote.pause.store(true, Ordering::SeqCst);
    let task = {
        let writer = writer.clone();
        let engine = engine.clone();
        tokio::spawn(async move { writer.publish_completed_package(&engine).await })
    };
    tokio::time::timeout(std::time::Duration::from_secs(3), remote.entered.notified())
        .await
        .unwrap();
    let newer = Node {
        name: "Moved.pages".into(),
        parent_id: Some("other-folder".into()),
        etag: Some("v2".into()),
        content_version: Some("v2".into()),
        ..native()
    };
    cirrove_store::Store::open(&engine.db)
        .unwrap()
        .observe_node(&scope, &newer)
        .unwrap();
    remote.release.notify_one();
    assert!(task.await.unwrap().unwrap());
    assert_eq!(
        cirrove_store::Store::open(&engine.db)
            .unwrap()
            .node(&scope, "native")
            .unwrap(),
        Some(newer.clone())
    );
    assert_eq!(
        journal
            .lock()
            .unwrap()
            .package_publication_status(row.id)
            .unwrap(),
        PackagePublicationStatus::Present(newer)
    );
}
#[tokio::test]
async fn publication_acknowledgement_requires_exact_saved_receipt() {
    let (_temp, engine, _remote, journal, _writer) = fixture().await;
    let mut row = acknowledge(&journal, engine.scope("drive"));
    row.remote.as_mut().unwrap().etag = Some("forged".into());
    assert!(matches!(
        journal.lock().unwrap().finish_package_publication(
            &row,
            PackagePublicationStatus::Present(native()),
            publication_now()
        ),
        Err(JournalError::Stale)
    ));
    assert_eq!(
        journal
            .lock()
            .unwrap()
            .package_publication_status(row.id)
            .unwrap(),
        PackagePublicationStatus::Pending
    );
}
