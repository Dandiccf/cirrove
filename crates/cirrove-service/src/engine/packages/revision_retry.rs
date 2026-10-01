//! Strict generated-package revision binding, independent of live iCloud data.
use super::*;
struct RevisionProvider {
    source: std::sync::Mutex<Node>,
    listings: AtomicUsize,
    metadata: AtomicUsize,
    change_again: AtomicBool,
    opt_in: bool,
    unavailable: AtomicBool,
    partial_before_stale: AtomicBool,
    metadata_gate: tokio::sync::Notify,
    pause_metadata: AtomicBool,
    deadline_ms: AtomicUsize,
}
fn package(revision: usize) -> Node {
    Node {
        etag: Some(format!("v{revision}")),
        content_version: None,
        size: revision as u64,
        ..node(
            "package",
            Some("root"),
            "Owned.pages",
            NodeKind::Folder,
            true,
        )
    }
}
impl RevisionProvider {
    fn new(opt_in: bool) -> Self {
        Self {
            source: std::sync::Mutex::new(package(1)),
            listings: AtomicUsize::new(0),
            metadata: AtomicUsize::new(0),
            change_again: AtomicBool::new(false),
            opt_in,
            unavailable: AtomicBool::new(false),
            partial_before_stale: AtomicBool::new(false),
            metadata_gate: tokio::sync::Notify::new(),
            pause_metadata: AtomicBool::new(false),
            deadline_ms: AtomicUsize::new(3000),
        }
    }
}
#[async_trait::async_trait]
impl MetadataProvider for RevisionProvider {
    fn provider_id(&self) -> &'static str {
        "fixture"
    }
    async fn changes(
        &self,
        _: &Scope,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> Result<ChangePage, ProviderError> {
        Ok(ChangePage {
            changes: vec![
                Change::Upsert(node("root", None, "Root", NodeKind::Folder, false)),
                Change::Upsert(self.source.lock().unwrap().clone()),
            ],
            checkpoint: Checkpoint::Complete(Cursor("complete".into())),
        })
    }
}
#[async_trait::async_trait]
impl ReadProvider for RevisionProvider {
    fn refresh_cached_packages_on_first_open(&self) -> bool {
        false
    }
    fn retry_package_source_on_version_change(&self, parent: &Node) -> bool {
        self.opt_in && parent.package && parent.kind == NodeKind::Folder
    }
    fn directory_fetch_timeout(&self, _: Option<&Node>) -> Duration {
        Duration::from_millis(self.deadline_ms.load(Ordering::SeqCst) as u64)
    }
    async fn node(
        &self,
        _: &Scope,
        id: &str,
        _: &CancellationToken,
    ) -> Result<Node, ProviderError> {
        assert_eq!(id, "package");
        self.metadata.fetch_add(1, Ordering::SeqCst);
        self.metadata_gate.notify_one();
        if self.pause_metadata.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        Ok(self.source.lock().unwrap().clone())
    }
    async fn children(
        &self,
        _: &Scope,
        _: &str,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> Result<DirectoryPage, ProviderError> {
        Err(ProviderError::Protocol(
            "generated enumeration requires source metadata",
        ))
    }
    async fn children_for_node(
        &self,
        _: &Scope,
        parent: &Node,
        cursor: Option<&Cursor>,
        cancel: &CancellationToken,
    ) -> Result<DirectoryPage, ProviderError> {
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let call = self.listings.fetch_add(1, Ordering::SeqCst);
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(ProviderError::Unavailable);
        }
        if call == 1 && self.change_again.load(Ordering::SeqCst) {
            *self.source.lock().unwrap() = package(3);
        }
        let source = self.source.lock().unwrap().clone();
        if parent != &source {
            if cursor.is_none() && self.partial_before_stale.load(Ordering::SeqCst) {
                return Ok(DirectoryPage {
                    nodes: vec![Node {
                        id: "unpublished-old-page".into(),
                        parent_id: Some(parent.id.clone()),
                        name: "Partial.pages".into(),
                        kind: NodeKind::File,
                        size: 8,
                        modified_unix: 0,
                        etag: None,
                        content_version: Some("v1".into()),
                        target: None,
                        package: false,
                    }],
                    next: Some(Cursor("partial-old-generation".into())),
                });
            }
            return Err(ProviderError::VersionChanged);
        }
        assert!(
            cursor.is_none(),
            "retry retained a previous generation cursor"
        );
        let revision = source.etag.as_deref().unwrap();
        Ok(DirectoryPage {
            nodes: vec![Node {
                id: format!("archive-{revision}"),
                parent_id: Some(source.id),
                name: "Owned.pages".into(),
                kind: NodeKind::File,
                size: 8,
                modified_unix: 0,
                etag: None,
                content_version: Some(revision.into()),
                target: None,
                package: false,
            }],
            next: None,
        })
    }
    async fn staged_content(
        &self,
        _: &Scope,
        child: &Node,
        _: &CancellationToken,
    ) -> Result<Option<Arc<[u8]>>, ProviderError> {
        let bytes = match child.content_version.as_deref() {
            Some("v1") => b"oldbytes",
            Some("v2") => b"newbytes",
            _ => return Err(ProviderError::VersionChanged),
        };
        Ok(Some(Arc::from(&bytes[..])))
    }
    async fn read_range(
        &self,
        _: &Scope,
        _: &Node,
        _: u64,
        _: u32,
        _: &CancellationToken,
    ) -> Result<Vec<u8>, ProviderError> {
        Err(ProviderError::Permission)
    }
}
async fn fixture(opt_in: bool) -> (tempfile::TempDir, Arc<RevisionProvider>, Arc<Engine>, Scope) {
    let root =
        tempfile::tempdir_in(std::env::var_os("TMPDIR").unwrap_or_else(|| "/var/tmp".into()))
            .unwrap();
    let provider = Arc::new(RevisionProvider::new(opt_in));
    let engine = Engine::new(
        fixture_account(root.path().join("mount")),
        provider.clone(),
        root.path().join("state"),
    )
    .await
    .unwrap();
    let scope = engine.scope("primary");
    crate::refresh(
        provider.as_ref(),
        &scope,
        &engine.db,
        false,
        &engine.cancel,
        None,
    )
    .await
    .unwrap();
    (root, provider, engine, scope)
}
#[tokio::test]
async fn stale_package_initial_open_refreshes_exact_parent_once() {
    let (_root, provider, engine, scope) = fixture(true).await;
    *provider.source.lock().unwrap() = package(2);
    let children = engine.children(&scope, "package").await.unwrap();
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].content_version.as_deref(), Some("v2"));
    assert_eq!(provider.listings.load(Ordering::SeqCst), 2);
    assert_eq!(provider.metadata.load(Ordering::SeqCst), 1);
    assert_eq!(
        engine
            .cache
            .read(
                provider.as_ref(),
                &scope,
                &children[0],
                0,
                8,
                &engine.cancel
            )
            .await
            .unwrap(),
        b"newbytes"
    );
    assert_eq!(engine.children(&scope, "package").await.unwrap(), children);
    assert_eq!(provider.metadata.load(Ordering::SeqCst), 1);
    engine.stop().await;
}
#[tokio::test]
async fn stale_package_retry_preserves_old_artifact_and_refuses_second_revision_race() {
    let (_root, provider, engine, scope) = fixture(true).await;
    let old = engine.children(&scope, "package").await.unwrap().remove(0);
    *provider.source.lock().unwrap() = package(2);
    provider.listings.store(0, Ordering::SeqCst);
    provider.change_again.store(true, Ordering::SeqCst);
    assert!(matches!(
        engine.fetch_directory(&scope, "package").await,
        Err(ProviderError::VersionChanged)
    ));
    assert_eq!(provider.listings.load(Ordering::SeqCst), 2);
    assert_eq!(provider.metadata.load(Ordering::SeqCst), 1);
    let cached = Store::open(&engine.db)
        .unwrap()
        .children(&scope, "package")
        .unwrap()
        .unwrap();
    assert_eq!(
        cached,
        vec![old.clone()],
        "failed retry replaced completed children"
    );
    assert_eq!(
        engine
            .cache
            .read(provider.as_ref(), &scope, &old, 0, 8, &engine.cancel)
            .await
            .unwrap(),
        b"oldbytes"
    );
    provider.change_again.store(false, Ordering::SeqCst);
    *provider.source.lock().unwrap() = package(2);
    engine.fetch_directory(&scope, "package").await.unwrap();
    let current = engine.children(&scope, "package").await.unwrap().remove(0);
    assert_eq!(current.content_version.as_deref(), Some("v2"));
    assert_eq!(
        engine
            .cache
            .read(provider.as_ref(), &scope, &current, 0, 8, &engine.cancel)
            .await
            .unwrap(),
        b"newbytes"
    );
    assert_eq!(
        engine
            .cache
            .read(provider.as_ref(), &scope, &old, 0, 8, &engine.cancel)
            .await
            .unwrap(),
        b"oldbytes"
    );
    engine.stop().await;
}
#[tokio::test]
async fn stale_package_retry_refuses_changed_identity_shape_and_nonopted_adapter() {
    for fault in 0..6 {
        let (_root, provider, engine, scope) = fixture(fault != 5).await;
        let mut changed = package(2);
        match fault {
            0 => changed.id = "foreign".into(),
            1 => changed.parent_id = Some("moved-parent".into()),
            2 => changed.name = "renamed.pages".into(),
            3 => changed.kind = NodeKind::File,
            4 => changed.package = false,
            _ => {}
        }
        *provider.source.lock().unwrap() = changed;
        assert!(
            engine.children(&scope, "package").await.is_err(),
            "fault {fault}"
        );
        assert_eq!(provider.listings.load(Ordering::SeqCst), 1, "fault {fault}");
        assert_eq!(
            provider.metadata.load(Ordering::SeqCst),
            usize::from(fault != 5)
        );
        engine.stop().await;
    }
}
#[tokio::test]
async fn stale_package_refresh_stays_inside_original_timeout_and_cancel_scope() {
    for cancelled in [false, true] {
        let (_root, provider, engine, scope) = fixture(true).await;
        *provider.source.lock().unwrap() = package(2);
        provider.pause_metadata.store(true, Ordering::SeqCst);
        provider
            .deadline_ms
            .store(if cancelled { 3000 } else { 100 }, Ordering::SeqCst);
        let cloned = engine.clone();
        let selected = scope.clone();
        let task = tokio::spawn(async move { cloned.fetch_directory(&selected, "package").await });
        tokio::time::timeout(Duration::from_secs(2), provider.metadata_gate.notified())
            .await
            .unwrap();
        if cancelled {
            engine.cancel.cancel();
        }
        let result = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        if cancelled {
            assert!(matches!(result, Err(ProviderError::Cancelled)));
        } else {
            assert!(matches!(result, Err(ProviderError::Unavailable)));
        }
        assert_eq!(provider.listings.load(Ordering::SeqCst), 1);
        engine.stop().await;
    }
}

#[tokio::test]
async fn stale_package_retry_discards_all_old_pages_and_cursor() {
    let (_root, provider, engine, scope) = fixture(true).await;
    *provider.source.lock().unwrap() = package(2);
    provider.partial_before_stale.store(true, Ordering::SeqCst);
    let children = engine.children(&scope, "package").await.unwrap();
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].id, "archive-v2");
    assert_eq!(provider.listings.load(Ordering::SeqCst), 3);
    assert_eq!(provider.metadata.load(Ordering::SeqCst), 1);
    assert!(
        Store::open(&engine.db)
            .unwrap()
            .node(&scope, "unpublished-old-page")
            .unwrap()
            .is_none()
    );
    engine.stop().await;
}

#[tokio::test]
async fn observed_package_source_refreshes_warm_children_for_all_publication_routes() {
    for route in 0..3 {
        let (_root, provider, engine, scope) = fixture(true).await;
        let old = engine.children(&scope, "package").await.unwrap().remove(0);
        assert_eq!(provider.listings.load(Ordering::SeqCst), 1);
        *provider.source.lock().unwrap() = package(2);
        {
            let mut store = Store::open(&engine.db).unwrap();
            match route {
                0 => {
                    let ticket = store.node_observation(&scope, "package").unwrap();
                    store.publish_node(&ticket, &package(2)).unwrap();
                }
                1 => {
                    let ticket = store.directory_observation(&scope, "root").unwrap();
                    store.publish_directory(&ticket, &[package(2)]).unwrap();
                }
                _ => {
                    let cursor = store.begin(&scope, false).unwrap();
                    store
                        .stage(
                            &scope,
                            cursor.as_ref(),
                            &ChangePage {
                                changes: vec![Change::Upsert(package(2))],
                                checkpoint: Checkpoint::Complete(Cursor("next".into())),
                            },
                        )
                        .unwrap();
                }
            }
        }
        let new = engine
            .child(&scope, "package", "Owned.pages")
            .await
            .unwrap();
        assert_eq!(new.id, "archive-v2", "route {route}");
        assert_eq!(
            provider.metadata.load(Ordering::SeqCst),
            0,
            "local observation should supply the source"
        );
        assert_eq!(provider.listings.load(Ordering::SeqCst), 2);
        assert_eq!(
            engine.children(&scope, "package").await.unwrap(),
            vec![new.clone()]
        );
        assert_eq!(
            provider.listings.load(Ordering::SeqCst),
            2,
            "warm unchanged read redownloaded"
        );
        assert_eq!(
            engine
                .cache
                .read(provider.as_ref(), &scope, &old, 0, 8, &engine.cancel)
                .await
                .unwrap(),
            b"oldbytes"
        );
        assert_eq!(
            engine
                .cache
                .read(provider.as_ref(), &scope, &new, 0, 8, &engine.cancel)
                .await
                .unwrap(),
            b"newbytes"
        );
    }
}
#[tokio::test]
async fn observed_package_source_unavailable_keeps_complete_old_snapshot_but_not_missing_source() {
    let (_root, provider, engine, scope) = fixture(true).await;
    let old = engine.children(&scope, "package").await.unwrap();
    *provider.source.lock().unwrap() = package(2);
    Store::open(&engine.db)
        .unwrap()
        .observe_node(&scope, &package(2))
        .unwrap();
    provider.unavailable.store(true, Ordering::SeqCst);
    assert_eq!(engine.children(&scope, "package").await.unwrap(), old);
    let calls = provider.listings.load(Ordering::SeqCst);
    assert_eq!(engine.children(&scope, "package").await.unwrap(), old);
    assert_eq!(
        provider.listings.load(Ordering::SeqCst),
        calls,
        "offline repeated read retried conversion"
    );
    assert!(
        Store::open(&engine.db)
            .unwrap()
            .directory_source_state(&scope, "package")
            .unwrap()
            .unwrap()
            .changed
    );
    // Ordinary activity refresh may recover independently of the foreground
    // failure memo; it publishes the new complete binding atomically.
    provider.unavailable.store(false, Ordering::SeqCst);
    engine.fetch_directory(&scope, "package").await.unwrap();
    assert_eq!(
        engine.children(&scope, "package").await.unwrap()[0].id,
        "archive-v2"
    );
    {
        let mut store = Store::open(&engine.db).unwrap();
        let ticket = store.node_observation(&scope, "package").unwrap();
        store.publish_absence(&ticket).unwrap();
    }
    assert!(matches!(
        engine.children(&scope, "package").await,
        Err(ProviderError::NotFound)
    ));
    assert!(matches!(
        engine.child(&scope, "package", "Owned.pages").await,
        Err(ProviderError::NotFound)
    ));
}
#[tokio::test]
async fn observed_package_source_invalid_shape_never_uses_previous_generated_snapshot() {
    let (_root, provider, engine, scope) = fixture(true).await;
    engine.children(&scope, "package").await.unwrap();
    let mut changed = package(2);
    changed.package = false;
    changed.kind = NodeKind::File;
    Store::open(&engine.db)
        .unwrap()
        .observe_node(&scope, &changed)
        .unwrap();
    assert!(matches!(
        engine.children(&scope, "package").await,
        Err(ProviderError::VersionChanged)
    ));
    assert_eq!(provider.listings.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn observed_package_source_stable_content_tag_does_not_redownload_metadata_only_changes() {
    let (_root, provider, engine, scope) = fixture(true).await;
    let mut original = package(1);
    original.content_version = Some("stable-content".into());
    *provider.source.lock().unwrap() = original.clone();
    Store::open(&engine.db)
        .unwrap()
        .observe_node(&scope, &original)
        .unwrap();
    let before = engine.children(&scope, "package").await.unwrap();
    let mut updated = original;
    updated.etag = Some("new-metadata-tag".into());
    updated.modified_unix = 22;
    *provider.source.lock().unwrap() = updated.clone();
    Store::open(&engine.db)
        .unwrap()
        .observe_node(&scope, &updated)
        .unwrap();
    assert_eq!(engine.children(&scope, "package").await.unwrap(), before);
    assert_eq!(provider.listings.load(Ordering::SeqCst), 1);
    assert_eq!(provider.metadata.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn observed_package_source_legacy_migration_refuses_missing_or_reclassified_source() {
    for arm in 0..3 {
        let (root, provider, engine, scope) = fixture(true).await;
        engine.children(&scope, "package").await.unwrap();
        let account = engine.account.clone();
        let db = engine.db.clone();
        if arm == 2 {
            let mut store = Store::open(&db).unwrap();
            let ticket = store.node_observation(&scope, "package").unwrap();
            store.publish_absence(&ticket).unwrap();
        }
        drop(engine);
        {
            // A real v7 on-disk snapshot has no source-binding table. Retain
            // its old metadata, absence marker and completed children exactly.
            let db = rusqlite::Connection::open(&db).unwrap();
            db.execute_batch("DROP TABLE directory_sources; PRAGMA user_version=7;")
                .unwrap();
        }
        let engine = Engine::new(account, provider.clone(), root.path().join("state"))
            .await
            .unwrap();
        if arm < 2 {
            let mut store = Store::open(&db).unwrap();
            if arm == 0 {
                let ticket = store.node_observation(&scope, "package").unwrap();
                store.publish_absence(&ticket).unwrap();
            } else {
                let mut ordinary = package(2);
                ordinary.package = false;
                ordinary.kind = NodeKind::File;
                store.observe_node(&scope, &ordinary).unwrap();
            }
        }
        let state = Store::open(&db)
            .unwrap()
            .directory_source_state(&scope, "package")
            .unwrap()
            .unwrap();
        assert!(state.bound.is_none());
        assert!(state.legacy_classification.is_some());
        let result = engine.children(&scope, "package").await;
        if arm == 1 {
            assert!(
                matches!(result, Err(ProviderError::VersionChanged)),
                "legacy reclassification returned old archive"
            );
        } else {
            assert!(
                matches!(result, Err(ProviderError::NotFound)),
                "legacy disappearance returned old archive"
            );
        }
        assert!(
            engine
                .child(&scope, "package", "Owned.pages")
                .await
                .is_err()
        );
        assert_eq!(provider.listings.load(Ordering::SeqCst), 1);
        assert_eq!(provider.metadata.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn observed_package_source_opted_out_legacy_delta_and_reset_invalidate_positive_child() {
    for reset in [false, true] {
        let (root, provider, engine, scope) = fixture(false).await;
        let original = engine.children(&scope, "package").await.unwrap().remove(0);
        assert_eq!(original.id, "archive-v1");
        let account = engine.account.clone();
        let db = engine.db.clone();
        drop(engine);
        {
            let db = rusqlite::Connection::open(&db).unwrap();
            db.execute_batch("DROP TABLE directory_sources; PRAGMA user_version=7;")
                .unwrap();
        }
        let engine = Engine::new(account, provider.clone(), root.path().join("state"))
            .await
            .unwrap();
        let state = Store::open(&db)
            .unwrap()
            .directory_source_state(&scope, "package")
            .unwrap()
            .unwrap();
        assert!(state.bound.is_none());
        assert!(state.legacy_classification.is_some());
        // Exercise a positive cached name lookup, not a forced refresh or
        // first-open package conversion that could hide incorrect retention.
        assert_eq!(
            engine
                .child(&scope, "package", "Owned.pages")
                .await
                .unwrap(),
            original
        );
        assert_eq!(provider.listings.load(Ordering::SeqCst), 1);
        *provider.source.lock().unwrap() = package(2);
        {
            let mut store = Store::open(&db).unwrap();
            let cursor = store.begin(&scope, reset).unwrap();
            store
                .stage(
                    &scope,
                    cursor.as_ref(),
                    &ChangePage {
                        changes: vec![
                            Change::Upsert(node("root", None, "Root", NodeKind::Folder, false)),
                            Change::Upsert(package(2)),
                        ],
                        checkpoint: Checkpoint::Complete(Cursor("next".into())),
                    },
                )
                .unwrap();
        }
        let current = engine
            .child(&scope, "package", "Owned.pages")
            .await
            .unwrap();
        assert_eq!(
            current.id, "archive-v2",
            "opted-out migrated snapshot survived reset={reset}"
        );
        assert_eq!(provider.listings.load(Ordering::SeqCst), 2);
        assert_eq!(provider.metadata.load(Ordering::SeqCst), 0);
        assert_eq!(
            engine.children(&scope, "package").await.unwrap(),
            vec![current]
        );
        assert_eq!(provider.listings.load(Ordering::SeqCst), 2);
        // No trusted binding may be manufactured for an opted-out converter.
        let state = Store::open(&db)
            .unwrap()
            .directory_source_state(&scope, "package")
            .unwrap()
            .unwrap();
        assert!(state.bound.is_none());
        assert!(state.legacy_classification.is_none());
        engine.stop().await;
    }
}
