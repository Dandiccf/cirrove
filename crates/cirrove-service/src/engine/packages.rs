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
            size: 7,
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

    async fn staged_content(
        &self,
        _: &Scope,
        node: &Node,
        _: &CancellationToken,
    ) -> std::result::Result<Option<Arc<[u8]>>, ProviderError> {
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
