#![allow(clippy::unwrap_used)]

use super::discovery::fixture_account;
use super::*;
use cirrove_core::{
    Change, ChangePage, Checkpoint, Cursor, DirectoryPage, MetadataProvider, NodeKind,
};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Default)]
struct PackageProvider {
    used_metadata_hook: AtomicBool,
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
        Ok(DirectoryPage {
            nodes: vec![Node {
                size: 7,
                ..node(
                    "derived-export",
                    Some("package"),
                    "Document.docx",
                    NodeKind::File,
                    false,
                )
            }],
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

    async fn staged_content(
        &self,
        _: &Scope,
        node: &Node,
        _: &CancellationToken,
    ) -> std::result::Result<Option<Arc<[u8]>>, ProviderError> {
        Ok((node.id == "derived-export").then(|| Arc::from(&b"content"[..])))
    }
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

    engine.fetch_directory(&scope, "package").await.unwrap();
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
