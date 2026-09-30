#![allow(clippy::unwrap_used)]
use super::*;
use cirrove_core::{MetadataProvider, NodeKind};

struct NoNetwork;
#[async_trait::async_trait]
impl MetadataProvider for NoNetwork {
    fn provider_id(&self) -> &'static str {
        "fixture"
    }
    async fn changes(
        &self,
        _: &Scope,
        _: Option<&cirrove_core::Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<cirrove_core::ChangePage, ProviderError> {
        panic!("status contacted provider")
    }
}
#[async_trait::async_trait]
impl ReadProvider for NoNetwork {
    async fn node(
        &self,
        _: &Scope,
        _: &str,
        _: &CancellationToken,
    ) -> std::result::Result<Node, ProviderError> {
        panic!("status fetched metadata")
    }
    async fn children(
        &self,
        _: &Scope,
        _: &str,
        _: Option<&cirrove_core::Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<cirrove_core::DirectoryPage, ProviderError> {
        panic!("status hydrated an export")
    }
    async fn read_range(
        &self,
        _: &Scope,
        _: &Node,
        _: u64,
        _: u32,
        _: &CancellationToken,
    ) -> std::result::Result<Vec<u8>, ProviderError> {
        panic!("status downloaded content")
    }
}
fn node(id: &str, folder: bool) -> Node {
    Node {
        id: id.into(),
        name: id.into(),
        parent_id: Some("root".into()),
        kind: if folder {
            NodeKind::Folder
        } else {
            NodeKind::File
        },
        size: 10,
        etag: Some("v1".into()),
        content_version: None,
        modified_unix: 0,
        target: None,
        package: false,
    }
}
#[tokio::test]
async fn cached_status_reports_pins_and_refuses_cold_exports_without_provider_calls() {
    let temp = tempfile::tempdir().unwrap();
    let mut account = crate::engine::discovery::fixture_account(temp.path().join("mount"));
    account.cache_bytes = 64 * 1024 * 1024;
    let engine = Engine::new(account, Arc::new(NoNetwork), temp.path().join("engine"))
        .await
        .unwrap();
    let scope = engine.scope(&engine.account.drive.id);
    let mut root = node("root", true);
    root.parent_id = None;
    let file = node("file", false);
    let mut package = node("export.gsheet", true);
    package.package = true;
    let mut store = Store::open(&engine.db).unwrap();
    store.observe_node(&scope, &root).unwrap();
    store
        .observe_directory(&scope, "root", &[file, package])
        .unwrap();
    engine
        .pin(scope.clone(), "file".into(), false, 1024)
        .await
        .unwrap()
        .unwrap();
    let states = engine
        .cached_path_states(&[
            "file".into(),
            "export.gsheet".into(),
            "export.gsheet/xlsx".into(),
            "missing".into(),
            "../file".into(),
        ])
        .await
        .unwrap();
    assert_eq!(states[0].pinned.as_deref(), Some("direct"));
    assert!(states[0].can_pin);
    assert!(!states[1].can_pin);
    assert!(states[2..].iter().all(|s| s.refusal.is_some()));
    assert!(
        engine
            .status_children(&scope, "export.gsheet", true)
            .await
            .is_err()
    );
    assert!(engine.status_node(&scope, "missing", true).await.is_err());
}

#[tokio::test]
async fn cached_status_does_not_fetch_missing_ancestors_when_checking_inherited_pins() {
    let temp = tempfile::tempdir().unwrap();
    let mut account = crate::engine::discovery::fixture_account(temp.path().join("mount"));
    account.cache_bytes = 64 * 1024 * 1024;
    let engine = Engine::new(account, Arc::new(NoNetwork), temp.path().join("engine"))
        .await
        .unwrap();
    let scope = engine.scope(&engine.account.drive.id);
    let mut root = node("root", true);
    root.parent_id = Some("unindexed-parent".into());
    Store::open(&engine.db)
        .unwrap()
        .observe_node(&scope, &root)
        .unwrap();
    engine
        .pin(scope, "unrelated-folder".into(), true, 1024)
        .await
        .unwrap()
        .unwrap();
    let states = engine.cached_path_states(&["".into()]).await.unwrap();
    assert_eq!(states[0].item, "root");
    assert_eq!(states[0].pinned, None);
}
