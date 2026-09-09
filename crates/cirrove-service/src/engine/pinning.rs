//! A pin only means something if the mount that evicts acts on it. These check
//! the seam between the registry and the cache, which is where a pin recorded
//! correctly and ignored completely would otherwise look identical to one that
//! works.
#![allow(clippy::unwrap_used)]
use super::discovery::{LinkedLibrary, fixture_account};
use super::*;

/// Serves deterministic bytes for one file, so a materialised pin can be checked
/// against blocks that actually exist rather than against an empty cache.
#[derive(Default)]
struct OneFile {
    reads: AtomicU64,
}
impl OneFile {
    fn node(size: u64) -> Node {
        Node {
            id: "pinned-file".into(),
            parent_id: Some("root".into()),
            name: "pinned".into(),
            kind: cirrove_core::NodeKind::File,
            size,
            modified_unix: 0,
            etag: Some("\"v1\"".into()),
            content_version: Some("v1".into()),
            target: None,
        }
    }
}
#[async_trait::async_trait]
impl cirrove_core::MetadataProvider for OneFile {
    fn provider_id(&self) -> &'static str {
        "fixture"
    }
    async fn changes(
        &self,
        _: &Scope,
        _: Option<&cirrove_core::Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<cirrove_core::ChangePage, ProviderError> {
        Ok(cirrove_core::ChangePage {
            changes: vec![],
            checkpoint: cirrove_core::Checkpoint::Complete(cirrove_core::Cursor("done".into())),
        })
    }
}
#[async_trait::async_trait]
impl ReadProvider for OneFile {
    async fn node(
        &self,
        _: &Scope,
        id: &str,
        _: &CancellationToken,
    ) -> std::result::Result<Node, ProviderError> {
        if id == "pinned-file" {
            Ok(Self::node(0))
        } else {
            Err(ProviderError::NotFound)
        }
    }
    async fn children(
        &self,
        _: &Scope,
        _: &str,
        _: Option<&cirrove_core::Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<cirrove_core::DirectoryPage, ProviderError> {
        Err(ProviderError::NotFound)
    }
    async fn read_range(
        &self,
        _: &Scope,
        node: &Node,
        offset: u64,
        length: u32,
        _: &CancellationToken,
    ) -> std::result::Result<Vec<u8>, ProviderError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let end = (offset + length as u64).min(node.size);
        Ok((offset..end).map(|i| (i % 251) as u8).collect())
    }
}
async fn engine(temp: &tempfile::TempDir, budget: u64) -> Arc<Engine> {
    let provider = Arc::new(LinkedLibrary {
        linked: AtomicBool::new(false),
        primary_changes: AtomicU64::new(0),
    });
    let mut account = fixture_account(temp.path().join("mount"));
    account.cache_bytes = budget;
    Engine::new(account, provider, temp.path().join("engine"))
        .await
        .unwrap()
}
fn scope() -> Scope {
    Scope {
        account: "00000000-0000-4000-8000-000000000015".into(),
        provider: "onedrive".into(),
        collection: "primary".into(),
    }
}

#[tokio::test]
async fn a_pin_reaches_the_cache_that_evicts() {
    let temp = tempfile::tempdir().unwrap();
    let engine = engine(&temp, 64 * 1024 * 1024).await;
    let reserved = 8 * 1024 * 1024;
    engine
        .pin(scope(), "item".into(), false, reserved)
        .await
        .unwrap()
        .expect("the budget holds this");
    // Recording the pin is not the property under test. Eviction reads a view
    // beside the cache, so a pin the engine never published would be a row in a
    // table and nothing else.
    assert_eq!(
        engine.cache.reservations().lock().unwrap().reserved,
        reserved
    );
}
#[tokio::test]
async fn a_refused_pin_changes_nothing() {
    let temp = tempfile::tempdir().unwrap();
    let budget = 16 * 1024 * 1024;
    let engine = engine(&temp, budget).await;
    let refusal = engine
        .pin(scope(), "too-large".into(), false, budget * 2)
        .await
        .unwrap()
        .expect_err("twice the budget cannot fit");
    assert!(matches!(
        refusal,
        cirrove_store::pins::PinRefusal::WouldExceedBudget { .. }
    ));
    assert_eq!(
        engine.cache.reservations().lock().unwrap().reserved,
        0,
        "a refusal must not shrink the budget it declined to claim"
    );
}
#[tokio::test]
async fn a_restart_republishes_pins_before_anything_can_evict() {
    let temp = tempfile::tempdir().unwrap();
    let reserved = 8 * 1024 * 1024;
    {
        let engine = engine(&temp, 64 * 1024 * 1024).await;
        engine
            .pin(scope(), "item".into(), true, reserved)
            .await
            .unwrap()
            .expect("fits");
        engine.stop().await;
    }
    // The registry is durable, but the cache's view is not: it is rebuilt in
    // memory. A mount that waited for the next pin change to publish it would
    // treat pinned blocks as ordinary ones for as long as nobody pinned
    // anything, which on a restarted daemon is indefinitely.
    let engine = engine(&temp, 64 * 1024 * 1024).await;
    assert_eq!(engine.cache.reservations().lock().unwrap().reserved, 0);
    engine.start().await.unwrap();
    assert_eq!(
        engine.cache.reservations().lock().unwrap().reserved,
        reserved
    );
    engine.stop().await;
}
#[tokio::test]
async fn unpinning_releases_the_budget_at_the_cache() {
    let temp = tempfile::tempdir().unwrap();
    let engine = engine(&temp, 64 * 1024 * 1024).await;
    engine
        .pin(scope(), "item".into(), false, 8 * 1024 * 1024)
        .await
        .unwrap()
        .expect("fits");
    assert!(engine.unpin(scope(), "item".into()).await.unwrap());
    assert_eq!(engine.cache.reservations().lock().unwrap().reserved, 0);
    assert!(
        !engine.unpin(scope(), "item".into()).await.unwrap(),
        "unpinning what is not pinned reports that rather than pretending"
    );
}

#[tokio::test]
async fn materialising_a_pin_fetches_its_blocks_and_protects_exactly_those() {
    let temp = tempfile::tempdir().unwrap();
    let provider = Arc::new(OneFile::default());
    let mut account = fixture_account(temp.path().join("mount"));
    account.cache_bytes = 64 * 1024 * 1024;
    let engine = Engine::new(account, provider.clone(), temp.path().join("engine"))
        .await
        .unwrap();
    // Two full blocks and a short third, so an off-by-one in the key walk shows
    // up as a count rather than passing on a size that divides evenly.
    let size = 2 * crate::content::BLOCK_SIZE as u64 + 1024;
    let node = OneFile::node(size);
    engine
        .pin(scope(), node.id.clone(), false, size)
        .await
        .unwrap()
        .expect("fits");
    let blocks = engine.materialise_pin(&scope(), &node).await.unwrap();
    assert_eq!(blocks, 3, "a partial last block is still a block");
    assert!(
        provider.reads.load(Ordering::SeqCst) >= 3,
        "content was fetched"
    );
    let reservations = engine.cache.reservations();
    let view = reservations.lock().unwrap();
    assert_eq!(view.protected.len(), 3);
    // Named with the cache's own derivation. A protected set built any other way
    // would cover keys nothing writes, and would look identical to no protection.
    for start in [
        0,
        crate::content::BLOCK_SIZE as u64,
        2 * crate::content::BLOCK_SIZE as u64,
    ] {
        let key = crate::content::block_key(&scope(), &node, start).expect("key");
        assert!(
            view.protected.contains(&key),
            "block at {start} is unprotected"
        );
    }
}
