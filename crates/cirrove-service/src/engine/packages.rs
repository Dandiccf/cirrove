#![allow(clippy::unwrap_used)]

use super::discovery::fixture_account;
use super::*;
use cirrove_core::{
    Change, ChangePage, Checkpoint, Cursor, DirectoryPage, MetadataProvider, NodeKind,
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Default)]
struct PackageProvider {
    used_metadata_hook: AtomicBool,
    representation_count: AtomicUsize,
    listing_calls: AtomicUsize,
    fail_listing: AtomicBool,
    streamed: AtomicBool,
    stream_fails: Arc<AtomicBool>,
    stream_reads: Arc<AtomicUsize>,
}

fn node(id: &str, parent: Option<&str>, name: &str, kind: NodeKind, package: bool) -> Node {
    Node {
        id: id.into(),
        parent_id: parent.map(str::to_owned),
        name: name.into(),
        kind,
        size: 0,
        modified_unix: 0,
        etag: None,
        content_version: Some("v1".into()),
        target: None,
        package,
    }
}

#[async_trait::async_trait]
impl MetadataProvider for PackageProvider {
    fn provider_id(&self) -> &'static str {
        "fixture"
    }

    async fn changes(
        &self,
        _: &Scope,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<ChangePage, ProviderError> {
        Ok(ChangePage {
            changes: vec![
                Change::Upsert(node("root", None, "Root", NodeKind::Folder, false)),
                Change::Upsert(node(
                    "package",
                    Some("root"),
                    "Document.gdoc",
                    NodeKind::Folder,
                    true,
                )),
            ],
            checkpoint: Checkpoint::Complete(Cursor("done".into())),
        })
    }
}

#[async_trait::async_trait]
impl ReadProvider for PackageProvider {
    fn refresh_cached_packages_on_first_open(&self) -> bool {
        true
    }

    async fn node(
        &self,
        _: &Scope,
        id: &str,
        _: &CancellationToken,
    ) -> std::result::Result<Node, ProviderError> {
        match id {
            "root" => Ok(node("root", None, "Root", NodeKind::Folder, false)),
            "package" => Ok(node(
                "package",
                Some("root"),
                "Document.gdoc",
                NodeKind::Folder,
                true,
            )),
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
        Err(ProviderError::Protocol(
            "package listing lost its parent metadata",
        ))
    }

    async fn children_for_node(
        &self,
        _: &Scope,
        parent: &Node,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<DirectoryPage, ProviderError> {
        assert_eq!(parent.id, "package");
        assert!(parent.package);
        self.used_metadata_hook.store(true, Ordering::SeqCst);
        self.listing_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_listing.load(Ordering::SeqCst) {
            return Err(ProviderError::Unavailable);
        }
        let mut nodes = vec![Node {
            size: if self.streamed.load(Ordering::SeqCst) {
                2 * crate::content::BLOCK_SIZE as u64 + 17
            } else {
                7
            },
            ..node(
                "derived-export",
                Some("package"),
                "Document.docx",
                NodeKind::File,
                false,
            )
        }];
        if self.representation_count.load(Ordering::SeqCst) == 2 {
            nodes.push(Node {
                size: 7,
                ..node(
                    "derived-pdf",
                    Some("package"),
                    "Document.pdf",
                    NodeKind::File,
                    false,
                )
            });
        }
        Ok(DirectoryPage { nodes, next: None })
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

    async fn staged_content_session(
        &self,
        scope: &Scope,
        node: &Node,
        _: &CancellationToken,
    ) -> std::result::Result<Option<Arc<dyn cirrove_core::reads::ReadSession>>, ProviderError> {
        if !self.streamed.load(Ordering::SeqCst) {
            return Ok(None);
        }
        Ok(Some(Arc::new(PackageSession {
            identity: cirrove_core::reads::ReadIdentity::new(scope, node)?,
            reads: self.stream_reads.clone(),
            fails: self.stream_fails.clone(),
            invalid: 0,
        })))
    }

    async fn staged_content(
        &self,
        _: &Scope,
        node: &Node,
        _: &CancellationToken,
    ) -> std::result::Result<Option<Arc<[u8]>>, ProviderError> {
        assert!(
            !self.streamed.load(Ordering::SeqCst),
            "disk artifact must not request a whole-memory copy"
        );
        Ok(matches!(node.id.as_str(), "derived-export" | "derived-pdf")
            .then(|| Arc::from(&b"content"[..])))
    }
}

#[tokio::test]
async fn upgraded_package_replaces_cached_children_on_first_open_after_restart() {
    let temp = tempfile::tempdir().unwrap();
    let account = fixture_account(temp.path().join("mount"));
    let provider = Arc::new(PackageProvider::default());
    let engine = Engine::new(account.clone(), provider.clone(), temp.path().join("state"))
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
    let old = engine.children(&scope, "package").await.unwrap();
    assert_eq!(old.len(), 1);
    engine.stop().await;
    drop(engine);

    provider.representation_count.store(2, Ordering::SeqCst);
    provider.used_metadata_hook.store(false, Ordering::SeqCst);
    provider.listing_calls.store(0, Ordering::SeqCst);
    let engine = Engine::new(account, provider.clone(), temp.path().join("state"))
        .await
        .unwrap();
    let scope = engine.scope("primary");
    let consumer_calls = Arc::new(AtomicUsize::new(0));
    let count = consumer_calls.clone();
    let (first, second) = tokio::join!(
        engine.with_children(&scope, "package", move |rows| {
            count.fetch_add(1, Ordering::SeqCst);
            rows.collect::<cirrove_store::Result<Vec<_>>>()
        }),
        engine.children(&scope, "package")
    );
    let children = first.unwrap().unwrap();
    assert_eq!(second.unwrap(), children);
    assert_eq!(consumer_calls.load(Ordering::SeqCst), 1);
    assert!(provider.used_metadata_hook.load(Ordering::SeqCst));
    assert_eq!(children.len(), 2);
    assert!(children.iter().any(|child| child.name == "Document.pdf"));
    assert_eq!(provider.listing_calls.load(Ordering::SeqCst), 1);
    let repeated = engine.children(&scope, "package").await.unwrap();
    assert_eq!(repeated, children);
    assert_eq!(provider.listing_calls.load(Ordering::SeqCst), 1);
    engine.stop().await;
}

#[tokio::test]
async fn newly_added_package_child_is_found_by_direct_name_after_restart() {
    let temp = tempfile::tempdir().unwrap();
    let account = fixture_account(temp.path().join("mount"));
    let provider = Arc::new(PackageProvider::default());
    let engine = Engine::new(account.clone(), provider.clone(), temp.path().join("state"))
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
    assert_eq!(engine.children(&scope, "package").await.unwrap().len(), 1);
    engine.stop().await;
    drop(engine);

    provider.representation_count.store(2, Ordering::SeqCst);
    provider.listing_calls.store(0, Ordering::SeqCst);
    let engine = Engine::new(account, provider.clone(), temp.path().join("state"))
        .await
        .unwrap();
    let scope = engine.scope("primary");
    let pdf = engine
        .child(&scope, "package", "Document.pdf")
        .await
        .unwrap();
    assert_eq!(pdf.id, "derived-pdf");
    assert_eq!(provider.listing_calls.load(Ordering::SeqCst), 1);
    engine.stop().await;
}

#[tokio::test]
async fn cached_package_remains_readable_when_first_recheck_is_unavailable() {
    let temp = tempfile::tempdir().unwrap();
    let account = fixture_account(temp.path().join("mount"));
    let provider = Arc::new(PackageProvider::default());
    let engine = Engine::new(account.clone(), provider.clone(), temp.path().join("state"))
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
    assert_eq!(engine.children(&scope, "package").await.unwrap().len(), 1);
    engine.stop().await;
    drop(engine);

    provider.representation_count.store(2, Ordering::SeqCst);
    provider.fail_listing.store(true, Ordering::SeqCst);
    provider.listing_calls.store(0, Ordering::SeqCst);
    let engine = Engine::new(account, provider.clone(), temp.path().join("state"))
        .await
        .unwrap();
    let scope = engine.scope("primary");
    let old = engine.children(&scope, "package").await.unwrap();
    assert_eq!(old.len(), 1);
    assert_eq!(old[0].name, "Document.docx");
    assert_eq!(provider.listing_calls.load(Ordering::SeqCst), 1);
    let repeated = engine.children(&scope, "package").await.unwrap();
    assert_eq!(repeated, old);
    assert_eq!(provider.listing_calls.load(Ordering::SeqCst), 1);
    engine.stop().await;
}

#[tokio::test]
async fn directory_fetch_passes_committed_parent_metadata_to_provider_packages() {
    let temp = tempfile::tempdir().unwrap();
    let provider = Arc::new(PackageProvider::default());
    let engine = Engine::new(
        fixture_account(temp.path().join("mount")),
        provider.clone(),
        temp.path().join("state"),
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

    // A complete feed contains the package itself, but never its derived children.
    // Opening it must fetch the package even after the feed completes.
    let children = engine.children(&scope, "package").await.unwrap();
    assert!(provider.used_metadata_hook.load(Ordering::SeqCst));
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].name, "Document.docx");
    assert_eq!(children[0].parent_id.as_deref(), Some("package"));
    let bytes = engine
        .cache
        .read(
            provider.as_ref(),
            &scope,
            &children[0],
            0,
            7,
            &engine.cancel,
        )
        .await
        .unwrap();
    assert_eq!(bytes, b"content");
    engine.stop().await;
}

struct PackageSession {
    identity: cirrove_core::reads::ReadIdentity,
    reads: Arc<AtomicUsize>,
    fails: Arc<AtomicBool>,
    invalid: u8,
}
#[async_trait::async_trait]
impl cirrove_core::reads::ReadSession for PackageSession {
    fn identity(&self) -> &cirrove_core::reads::ReadIdentity {
        &self.identity
    }
    async fn read_range(
        &self,
        offset: u64,
        length: u32,
        cancel: &CancellationToken,
    ) -> std::result::Result<Vec<u8>, ProviderError> {
        assert!(length <= crate::content::BLOCK_SIZE);
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.fails.load(Ordering::SeqCst) && offset > 0 {
            return Err(ProviderError::VersionChanged);
        }
        let mut bytes: Vec<_> = (offset..offset + u64::from(length))
            .map(|v| (v % 251) as u8)
            .collect();
        if self.invalid == 1 {
            bytes.pop();
        }
        if self.invalid == 2 {
            cancel.cancel();
        }
        Ok(bytes)
    }
}

#[tokio::test]
async fn streamed_generated_content_is_cached_before_publication_and_survives_restart() {
    let temp = tempfile::tempdir().unwrap();
    let mut account = fixture_account(temp.path().join("mount"));
    account.cache_bytes = 64 * 1024 * 1024;
    let provider = Arc::new(PackageProvider::default());
    provider.streamed.store(true, Ordering::SeqCst);
    let state = temp.path().join("state");
    let engine = Engine::new(account.clone(), provider.clone(), state.clone())
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
    let children = engine.children(&scope, "package").await.unwrap();
    assert_eq!(children.len(), 1);
    assert_eq!(provider.stream_reads.load(Ordering::SeqCst), 3);
    let child = children[0].clone();
    engine.stop().await;
    drop(engine);
    let engine = Engine::new(account, provider.clone(), state).await.unwrap();
    let stored = engine.node(&scope, &child.id).await.unwrap();
    assert_eq!(stored, child);
    for offset in [
        0,
        crate::content::BLOCK_SIZE as u64 - 17,
        2 * crate::content::BLOCK_SIZE as u64,
    ] {
        let length = (child.size - offset).min(64) as u32;
        let bytes = engine
            .cache
            .read(
                provider.as_ref(),
                &scope,
                &child,
                offset,
                length,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            bytes,
            (offset..offset + length as u64)
                .map(|v| (v % 251) as u8)
                .collect::<Vec<_>>()
        );
    }
    assert_eq!(
        provider.stream_reads.load(Ordering::SeqCst),
        3,
        "reopened reads must come from the shared cache"
    );
    engine.stop().await;
}

#[tokio::test]
async fn failed_generated_stream_does_not_publish_its_directory_entry() {
    let temp = tempfile::tempdir().unwrap();
    let mut account = fixture_account(temp.path().join("mount"));
    account.cache_bytes = 64 * 1024 * 1024;
    let provider = Arc::new(PackageProvider::default());
    provider.streamed.store(true, Ordering::SeqCst);
    provider.stream_fails.store(true, Ordering::SeqCst);
    let engine = Engine::new(account, provider.clone(), temp.path().join("state"))
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
    assert!(engine.children(&scope, "package").await.is_err());
    assert_eq!(provider.stream_reads.load(Ordering::SeqCst), 2);
    assert!(
        Store::open(&engine.db)
            .unwrap()
            .node(&scope, "derived-export")
            .unwrap()
            .is_none()
    );
    engine.stop().await;
}

#[tokio::test]
async fn staged_session_refuses_foreign_identity_short_ranges_and_cancelled_results() {
    let temp = tempfile::tempdir().unwrap();
    let cache = crate::content::ContentCache::new(
        temp.path().join("cache"),
        temp.path().join("blocks"),
        64 * 1024 * 1024,
    )
    .unwrap();
    let scope = Scope {
        account: "account".into(),
        provider: "fixture".into(),
        collection: "drive".into(),
    };
    for invalid in [1, 2, 3] {
        let item = Node {
            size: 7,
            ..node(
                &format!("case-{invalid}"),
                Some("package"),
                "Archive",
                NodeKind::File,
                false,
            )
        };
        let mut identity = cirrove_core::reads::ReadIdentity::new(&scope, &item).unwrap();
        if invalid == 3 {
            identity.scope.account = "foreign".into();
        }
        let reads = Arc::new(AtomicUsize::new(0));
        let session = PackageSession {
            identity,
            reads: reads.clone(),
            fails: Arc::new(AtomicBool::new(false)),
            invalid,
        };
        let result = cache
            .stage_session(&scope, &item, &session, &CancellationToken::new())
            .await;
        assert!(result.is_err());
        assert_eq!(
            reads.load(Ordering::SeqCst),
            if invalid == 3 { 0 } else { 1 }
        );
        let key = crate::content::block_key(&scope, &item, 0).unwrap();
        assert!(!temp.path().join("cache").join(key).exists());
    }
}
