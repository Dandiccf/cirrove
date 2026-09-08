//! A linked drive must still be subscribed after a transient store failure,
//! without waiting for the next successful feed poll.
#![allow(clippy::unwrap_used)]
use super::*;
use cirrove_auth::{AccessMode, AppRegistration, Identity};
use cirrove_core::{
    Change, ChangePage, Checkpoint, Cursor, DirectoryPage, MetadataProvider, NodeKind, RemoteRef,
};
use cirrove_onedrive::DriveInfo;
use std::time::Instant;

struct LinkedLibrary {
    linked: AtomicBool,
    primary_changes: AtomicU64,
}
impl LinkedLibrary {
    fn folder(id: &str, name: &str) -> Node {
        Node {
            id: id.into(),
            parent_id: None,
            name: name.into(),
            kind: NodeKind::Folder,
            size: 0,
            modified_unix: 0,
            etag: Some("1".into()),
            content_version: None,
            target: None,
        }
    }
    fn shortcut() -> Node {
        Node {
            id: "shared-library".into(),
            parent_id: Some("root".into()),
            name: "Shared library".into(),
            kind: NodeKind::Shortcut,
            size: 0,
            modified_unix: 0,
            etag: Some("1".into()),
            content_version: None,
            target: Some(Box::new(RemoteRef {
                collection: "linked".into(),
                item: "linked-root".into(),
                kind: Some(NodeKind::Folder),
            })),
        }
    }
}
#[async_trait::async_trait]
impl MetadataProvider for LinkedLibrary {
    fn provider_id(&self) -> &'static str {
        "fixture"
    }
    async fn changes(
        &self,
        scope: &Scope,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<ChangePage, ProviderError> {
        let changes = match scope.collection.as_str() {
            "primary" => {
                self.primary_changes.fetch_add(1, Ordering::SeqCst);
                let mut changes = vec![Change::Upsert(Self::folder("root", "Primary"))];
                if self.linked.load(Ordering::SeqCst) {
                    changes.push(Change::Upsert(Self::shortcut()));
                }
                changes
            }
            "linked" => vec![Change::Upsert(Self::folder("linked-root", "Linked"))],
            _ => return Err(ProviderError::Permission),
        };
        Ok(ChangePage {
            changes,
            checkpoint: Checkpoint::Complete(Cursor(format!(
                "{}-{}",
                scope.collection,
                self.linked.load(Ordering::SeqCst)
            ))),
        })
    }
}
#[async_trait::async_trait]
impl ReadProvider for LinkedLibrary {
    async fn node(
        &self,
        _: &Scope,
        id: &str,
        _: &CancellationToken,
    ) -> std::result::Result<Node, ProviderError> {
        match id {
            "root" => Ok(Self::folder("root", "Primary")),
            "linked-root" => Ok(Self::folder("linked-root", "Linked")),
            "shared-library" => Ok(Self::shortcut()),
            _ => Err(ProviderError::NotFound),
        }
    }
    async fn children(
        &self,
        _: &Scope,
        _: &str,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<DirectoryPage, ProviderError> {
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
        Err(ProviderError::Permission)
    }
}

async fn settle(engine: &Arc<Engine>, reason: &str, mut ready: impl FnMut(&[FeedHealth]) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let health = engine.health().await;
        if ready(&health) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{reason}; last observed: {health:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_discovery_still_subscribes_the_linked_drive_without_another_poll() {
    let temp = tempfile::tempdir().unwrap();
    let provider = Arc::new(LinkedLibrary {
        linked: AtomicBool::new(false),
        primary_changes: AtomicU64::new(0),
    });
    let account = crate::accounts::Account {
        id: "00000000-0000-4000-8000-000000000015".into(),
        label: "discovery-fixture".into(),
        registration: AppRegistration {
            client_id: "00000000-0000-4000-8000-000000000017".into(),
            authority: "common".into(),
        },
        identity: Identity {
            tenant_id: "00000000-0000-4000-8000-000000000018".into(),
            subject: "synthetic".into(),
            username: "discovery@example.invalid".into(),
            graph_user_id: "synthetic-user".into(),
            display_name: "Synthetic discovery fixture".into(),
        },
        credential_id: "00000000-0000-4000-8000-000000000016".into(),
        access: AccessMode::ReadOnly,
        drive: DriveInfo {
            id: "primary".into(),
            name: "Discovery fixture".into(),
            drive_type: "business".into(),
            web_url: "https://example.invalid".into(),
        },
        root_id: "root".into(),
        mount_path: temp.path().join("mount"),
        enabled: true,
        // An hour of quiet is what makes this test meaningful: nothing but the
        // discovery worker itself can subscribe the linked drive afterwards.
        poll_seconds: 3600,
        cache_bytes: 8 * 1024 * 1024,
    };
    let engine = Engine::new(account, provider.clone(), temp.path().join("state"))
        .await
        .unwrap();
    engine.start().await.unwrap();
    settle(&engine, "the primary feed never became ready", |health| {
        health.len() == 1 && health[0].state == "ready"
    })
    .await;

    // Publish the shortcut without waking any feed, so discovery is the only
    // path from here to a second subscription.
    provider.linked.store(true, Ordering::SeqCst);
    crate::refresh(
        provider.as_ref(),
        &engine.scope("primary"),
        &engine.db,
        false,
        &engine.cancel,
    )
    .await
    .unwrap();
    let polls = provider.primary_changes.load(Ordering::SeqCst);

    // A real held writer, not an injected error: every discovery write now
    // exhausts the store's busy timeout and fails.
    let held = tokio::task::spawn_blocking({
        let db = engine.db.clone();
        move || {
            let db = rusqlite::Connection::open(db).unwrap();
            db.execute_batch("BEGIN IMMEDIATE").unwrap();
            db
        }
    })
    .await
    .unwrap();
    engine.discovery.notify_one();
    let deadline = Instant::now() + Duration::from_secs(20);
    while engine.discovery_failures.load(Ordering::SeqCst) == 0 {
        assert!(
            Instant::now() < deadline,
            "discovery did not fail while the writer was held"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        engine.health().await.len(),
        1,
        "a blocked write still subscribed a drive"
    );

    tokio::task::spawn_blocking(move || held.execute_batch("ROLLBACK").unwrap())
        .await
        .unwrap();
    // Nothing notifies discovery again: the only feed sleeps for an hour.
    settle(
        &engine,
        "the linked drive was never subscribed after the failure",
        |health| health.len() == 2 && health.iter().all(|feed| feed.state == "ready"),
    )
    .await;
    assert_eq!(
        provider.primary_changes.load(Ordering::SeqCst),
        polls,
        "the primary feed polled again and could have notified discovery itself"
    );
    engine.stop().await;
}
