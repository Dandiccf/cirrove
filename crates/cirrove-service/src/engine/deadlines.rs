//! A filesystem path must not wait on an adapter indefinitely. The service
//! enforces its own deadline rather than trusting a provider to honour the
//! cancellation token it was handed.
#![allow(clippy::unwrap_used)]
use super::discovery::fixture_account;
use super::*;
use cirrove_core::{ChangePage, Checkpoint, Cursor, DirectoryPage, MetadataProvider};

struct NeverAnswers {
    entered: Arc<tokio::sync::Notify>,
}
#[async_trait::async_trait]
impl MetadataProvider for NeverAnswers {
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
            changes: vec![],
            checkpoint: Checkpoint::Complete(Cursor("fixture".into())),
        })
    }
}
#[async_trait::async_trait]
impl ReadProvider for NeverAnswers {
    async fn node(
        &self,
        _: &Scope,
        _: &str,
        _: &CancellationToken,
    ) -> std::result::Result<Node, ProviderError> {
        self.entered.notify_one();
        // Ignores the token entirely, which is the case the deadline exists for.
        std::future::pending().await
    }
    async fn children(
        &self,
        _: &Scope,
        _: &str,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<DirectoryPage, ProviderError> {
        std::future::pending().await
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

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn an_unanswered_single_item_fetch_gives_up_instead_of_holding_the_caller() {
    let temp = tempfile::tempdir().unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let provider = Arc::new(NeverAnswers {
        entered: entered.clone(),
    });
    let engine = Engine::new(
        fixture_account(temp.path().join("mount")),
        provider,
        temp.path().join("state"),
    )
    .await
    .unwrap();
    let scope = engine.scope("primary");

    // No row exists for this item, so the fetch is reached; the provider then
    // never answers and never observes its token.
    let waiting = tokio::spawn({
        let engine = engine.clone();
        let scope = scope.clone();
        async move { engine.node(&scope, "absent-item").await }
    });
    entered.notified().await;
    let outcome = tokio::time::timeout(Duration::from_secs(600), waiting)
        .await
        .expect("the fetch never gave up")
        .unwrap();
    assert!(
        matches!(outcome, Err(ProviderError::Unavailable)),
        "expected the deadline to surface as Unavailable, got {outcome:?}"
    );
    engine.stop().await;
}
