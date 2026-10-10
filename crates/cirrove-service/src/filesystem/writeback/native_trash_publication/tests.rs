#![allow(clippy::unwrap_used)]
use super::*;
use crate::journal::{MutationRecord, PackagePublicationStatus};
use cirrove_core::mutation::{MutationIntent, MutationReceipt, MutationRequest};
use cirrove_core::{ChangePage, Cursor, DirectoryPage, MetadataProvider, ReadProvider};
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

fn acknowledge(journal: &Arc<Mutex<UploadJournal>>, scope: Scope) -> MutationRecord {
    let mut journal = journal.lock().unwrap();
    let row = journal
        .enqueue_mutation(MutationRequest {
            scope,
            intent: MutationIntent::TrashNativeDocument { before: native() },
        })
        .unwrap();
    let claimed = journal.claim_mutation().unwrap().unwrap();
    assert_eq!(row.id, claimed.id);
    journal
        .acknowledge_mutation(
            row.id,
            claimed.attempt.unwrap(),
            MutationReceipt::Removed { item: native().id },
        )
        .unwrap();
    journal.mutation(row.id).unwrap()
}
fn cached(engine: &Engine) -> Option<Node> {
    cirrove_store::Store::open(&engine.db)
        .unwrap()
        .node(&engine.scope("drive"), "native")
        .unwrap()
}
#[tokio::test]
async fn native_trash_receipt_does_not_hide_metadata_before_ordered_absence() {
    let (_temp, engine, remote, journal, writer) = fixture().await;
    let scope = engine.scope("drive");
    cirrove_store::Store::open(&engine.db)
        .unwrap()
        .observe_node(&scope, &native())
        .unwrap();
    let row = acknowledge(&journal, scope);
    let retained = serde_json::to_vec(&row).unwrap();
    assert_eq!(cached(&engine), Some(native()));
    assert_eq!(
        journal
            .lock()
            .unwrap()
            .native_trash_publication_status(row.id)
            .unwrap(),
        PackagePublicationStatus::Pending
    );
    *remote.response.lock().unwrap() = Err(ProviderError::NotFound);
    assert!(
        writer
            .publish_completed_native_trash(&engine)
            .await
            .unwrap()
    );
    assert_eq!(cached(&engine), None);
    assert_eq!(
        journal
            .lock()
            .unwrap()
            .native_trash_publication_status(row.id)
            .unwrap(),
        PackagePublicationStatus::Absent
    );
    assert_eq!(
        serde_json::to_vec(&journal.lock().unwrap().mutation(row.id).unwrap()).unwrap(),
        retained
    );
    assert!(
        !writer
            .publish_completed_native_trash(&engine)
            .await
            .unwrap()
    );
    assert_eq!(remote.reads.load(Ordering::SeqCst), 1);
    assert_eq!(remote.listings.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn native_trash_publication_survives_restart_and_keeps_active_or_failed_observations_visible()
{
    let (temp, engine, remote, journal, writer) = fixture().await;
    let row = acknowledge(&journal, engine.scope("drive"));
    assert!(
        writer
            .publish_completed_native_trash(&engine)
            .await
            .is_err()
    );
    assert_eq!(cached(&engine), Some(native()));
    assert_eq!(
        journal
            .lock()
            .unwrap()
            .native_trash_publication_status(row.id)
            .unwrap(),
        PackagePublicationStatus::Present(native())
    );
    assert!(
        !writer
            .publish_completed_native_trash(&engine)
            .await
            .unwrap()
    );
    assert_eq!(remote.reads.load(Ordering::SeqCst), 1);
    drop(writer);
    drop(journal);
    let reopened = Arc::new(Mutex::new(
        UploadJournal::open(
            &temp.path().join("journal"),
            &engine.account.id,
            1024 * 1024,
        )
        .unwrap(),
    ));
    *remote.journal.lock().unwrap() = Arc::downgrade(&reopened);
    assert_eq!(
        reopened
            .lock()
            .unwrap()
            .native_trash_publication_due(publication_now() + 61)
            .unwrap()
            .unwrap()
            .id,
        row.id
    );
    assert_eq!(
        reopened
            .lock()
            .unwrap()
            .native_trash_publication_status(row.id)
            .unwrap(),
        PackagePublicationStatus::Present(native())
    );
    reopened
        .lock()
        .unwrap()
        .finish_native_trash_publication(&row, PackagePublicationStatus::Pending, 0)
        .unwrap();
    let writer = Writeback::new(&engine, reopened.clone()).await.unwrap();
    *remote.response.lock().unwrap() = Err(ProviderError::Unavailable);
    assert!(
        writer
            .publish_completed_native_trash(&engine)
            .await
            .is_err()
    );
    assert_eq!(cached(&engine), Some(native()));
    assert_eq!(
        reopened
            .lock()
            .unwrap()
            .native_trash_publication_status(row.id)
            .unwrap(),
        PackagePublicationStatus::Pending
    );
    reopened
        .lock()
        .unwrap()
        .finish_native_trash_publication(&row, PackagePublicationStatus::Pending, 0)
        .unwrap();
    *remote.response.lock().unwrap() = Err(ProviderError::NotFound);
    assert!(
        writer
            .publish_completed_native_trash(&engine)
            .await
            .unwrap()
    );
    assert_eq!(cached(&engine), None);
}
#[tokio::test]
async fn native_trash_stale_absence_cannot_erase_a_newer_observation() {
    let (_temp, engine, remote, journal, writer) = fixture().await;
    let scope = engine.scope("drive");
    let row = acknowledge(&journal, scope.clone());
    *remote.response.lock().unwrap() = Err(ProviderError::NotFound);
    remote.pause.store(true, Ordering::SeqCst);
    let task = {
        let engine = engine.clone();
        let writer = writer.clone();
        tokio::spawn(async move { writer.publish_completed_native_trash(&engine).await })
    };
    tokio::time::timeout(std::time::Duration::from_secs(3), remote.entered.notified())
        .await
        .unwrap();
    let restored = Node {
        etag: Some("restored-v2".into()),
        content_version: Some("restored-v2".into()),
        ..native()
    };
    cirrove_store::Store::open(&engine.db)
        .unwrap()
        .observe_node(&scope, &restored)
        .unwrap();
    remote.release.notify_one();
    assert!(task.await.unwrap().is_err());
    assert_eq!(cached(&engine), Some(restored.clone()));
    assert_eq!(
        journal
            .lock()
            .unwrap()
            .native_trash_publication_status(row.id)
            .unwrap(),
        PackagePublicationStatus::Present(restored)
    );
}
#[tokio::test]
async fn native_trash_publication_rejects_mismatched_receipts_and_observations() {
    let (_temp, engine, _remote, journal, _writer) = fixture().await;
    let row = acknowledge(&journal, engine.scope("drive"));
    let mut changed = row.clone();
    if let MutationIntent::TrashNativeDocument { before } = &mut changed.request.intent {
        before.etag = Some("wrong".into());
    }
    assert!(
        journal
            .lock()
            .unwrap()
            .finish_native_trash_publication(&changed, PackagePublicationStatus::Absent, 0)
            .is_err()
    );
    assert!(
        journal
            .lock()
            .unwrap()
            .finish_native_trash_publication(
                &row,
                PackagePublicationStatus::Present(Node {
                    id: "foreign".into(),
                    ..native()
                }),
                0
            )
            .is_err()
    );
    assert_eq!(
        journal
            .lock()
            .unwrap()
            .native_trash_publication_status(row.id)
            .unwrap(),
        PackagePublicationStatus::Pending
    );
}
