//! A pin only means something if the mount that evicts acts on it. These check
//! the seam between the registry and the cache, which is where a pin recorded
//! correctly and ignored completely would otherwise look identical to one that
//! works.
#![allow(clippy::unwrap_used)]
use super::discovery::{LinkedLibrary, fixture_account};
use super::*;

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
