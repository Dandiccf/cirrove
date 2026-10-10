//! Worker/router coverage with an injected synthetic adapter. Real adapter HTTP
//! boundaries live in cirrove-icloud::folder_create::recovery_tests.
#![allow(clippy::unwrap_used)]
use super::*;
use crate::mutations::MutationWorker;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

struct Adapter {
    vault: Arc<dyn CredentialVault>,
    endpoint: std::net::SocketAddr,
    preflight_refused: AtomicBool,
    lose_response: bool,
}
impl Adapter {
    fn key(operation: &str) -> String {
        format!("synthetic-create/{operation}")
    }
    async fn phase(&self, operation: &str) -> MutationResult<Option<String>> {
        Ok(self
            .vault
            .load(&Self::key(operation))
            .await
            .map_err(|_| MutationError::Uncertain)?
            .map(|v| v.expose_secret().to_owned()))
    }
    async fn save(&self, operation: &str, phase: &str) -> MutationResult<()> {
        self.vault
            .save(&Self::key(operation), SecretString::from(phase.to_owned()))
            .await
            .map_err(|_| MutationError::Uncertain)
    }
}
#[async_trait]
impl MutationProvider for Adapter {
    async fn prepare_mutation_for_operation(
        &self,
        operation: &str,
        _: &MutationRequest,
        _: &CancellationToken,
    ) -> MutationResult<Option<String>> {
        match self.phase(operation).await?.as_deref() {
            None => self.save(operation, "not_sent").await?,
            Some("not_sent") => {}
            _ => return Err(MutationError::Uncertain),
        }
        Ok(None)
    }
    async fn mutate(
        &self,
        _: &MutationRequest,
        _: &CancellationToken,
    ) -> MutationResult<MutationReceipt> {
        Err(MutationError::Invalid)
    }
    async fn reconcile_mutation(
        &self,
        _: &MutationRequest,
        _: &CancellationToken,
    ) -> MutationResult<MutationReconciliation> {
        Ok(MutationReconciliation::Indeterminate)
    }
    async fn mutate_operation(
        &self,
        operation: &str,
        request: &MutationRequest,
        _: Option<&str>,
        _: &CancellationToken,
    ) -> MutationResult<MutationReceipt> {
        if self.preflight_refused.swap(false, Ordering::SeqCst) {
            return Err(MutationError::InsufficientStorage);
        }
        if self.phase(operation).await?.as_deref() != Some("not_sent") {
            return Err(MutationError::Uncertain);
        }
        self.save(operation, "may_have_sent").await?;
        let mut peer = TcpStream::connect(self.endpoint)
            .await
            .map_err(|_| MutationError::Uncertain)?;
        peer.write_all(b"POST /createFolders HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.map_err(|_| MutationError::Uncertain)?;
        let mut response = [0; 128];
        peer.read(&mut response)
            .await
            .map_err(|_| MutationError::Uncertain)?;
        if self.lose_response {
            return Err(MutationError::Uncertain);
        }
        let MutationIntent::CreateFolder { parent, name } = &request.intent else {
            return Err(MutationError::Invalid);
        };
        let mut node = super::super::tests::folder(parent);
        node.id = "FOLDER::com.apple.CloudDocs::created".into();
        node.name = name.clone();
        self.save(operation, "created").await?;
        Ok(MutationReceipt::Upsert(node))
    }
    async fn reconcile_operation(
        &self,
        operation: &str,
        _: &MutationRequest,
        _: Option<&str>,
        _: &CancellationToken,
    ) -> MutationResult<MutationReconciliation> {
        Ok(
            if self.phase(operation).await?.as_deref() == Some("not_sent") {
                MutationReconciliation::Uncommitted
            } else {
                MutationReconciliation::Indeterminate
            },
        )
    }
}
async fn server() -> (
    std::net::SocketAddr,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let posts = Arc::new(AtomicUsize::new(0));
    let count = posts.clone();
    let task = tokio::spawn(async move {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let (mut peer, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut bytes = [0; 1024];
                let n = peer.read(&mut bytes).await.unwrap();
                assert!(n > 0 && request.len() + n <= 2048);
                request.extend_from_slice(&bytes[..n]);
                if request.windows(4).any(|b| b == b"\r\n\r\n") {
                    break;
                }
            }
            assert!(request.starts_with(b"POST /createFolders HTTP/1.1\r\n"));
            count.fetch_add(1, Ordering::SeqCst);
            peer.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            drop(peer);
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(150), listener.accept())
                    .await
                    .is_err(),
                "no duplicate create POST"
            );
        })
        .await
        .expect("bounded synthetic adapter HTTP");
    });
    (addr, posts, task)
}
#[tokio::test]
async fn router_worker_preflight_refusal_explicit_retry_posts_once() {
    let (_temp, mut p) = super::super::tests::fixture();
    let (endpoint, posts, server) = server().await;
    p.folder_create_test_adapter = Some(Arc::new(Adapter {
        vault: p.folder_vault.clone(),
        endpoint,
        preflight_refused: AtomicBool::new(true),
        lose_response: false,
    }));
    let request = MutationRequest {
        scope: p.scope.clone(),
        intent: MutationIntent::CreateFolder {
            parent: ROOT_ID.into(),
            name: "Child".into(),
        },
    };
    let record = p
        .journal
        .lock()
        .unwrap()
        .enqueue_mutation(request.clone())
        .unwrap();
    let p = Arc::new(p);
    let worker = MutationWorker::new(p.journal.clone(), p.clone(), CancellationToken::new());
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        MutationState::Failed
    );
    assert_eq!(posts.load(Ordering::SeqCst), 0);
    assert!(
        worker.run_once().await.unwrap().is_none(),
        "terminal failure must not retry automatically"
    );
    p.journal
        .lock()
        .unwrap()
        .request_mutation_retry(record.id)
        .unwrap();
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        MutationState::Pending
    );
    assert!(
        p.saved_plan(record.id, &request, None)
            .await
            .unwrap()
            .unwrap()
            .phase
            == PlanPhase::Prepared
    );
    assert_eq!(posts.load(Ordering::SeqCst), 0);
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        MutationState::Applied
    );
    assert_eq!(posts.load(Ordering::SeqCst), 1);
    server.await.unwrap();
}
#[tokio::test]
async fn router_worker_lost_create_response_never_replays() {
    let (_temp, mut p) = super::super::tests::fixture();
    let (endpoint, posts, server) = server().await;
    p.folder_create_test_adapter = Some(Arc::new(Adapter {
        vault: p.folder_vault.clone(),
        endpoint,
        preflight_refused: AtomicBool::new(false),
        lose_response: true,
    }));
    let request = MutationRequest {
        scope: p.scope.clone(),
        intent: MutationIntent::CreateFolder {
            parent: ROOT_ID.into(),
            name: "Child".into(),
        },
    };
    let record = p.journal.lock().unwrap().enqueue_mutation(request).unwrap();
    let p = Arc::new(p);
    let worker = MutationWorker::new(p.journal.clone(), p.clone(), CancellationToken::new());
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        MutationState::VerifyRequired
    );
    p.journal
        .lock()
        .unwrap()
        .request_mutation_retry(record.id)
        .unwrap();
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        MutationState::NeedsReview
    );
    p.journal
        .lock()
        .unwrap()
        .request_mutation_retry(record.id)
        .unwrap();
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        MutationState::NeedsReview
    );
    assert_eq!(posts.load(Ordering::SeqCst), 1);
    server.await.unwrap();
}

struct FailResetVault {
    inner: tests::MemoryVault,
    fail: AtomicBool,
}
#[async_trait]
impl CredentialVault for FailResetVault {
    async fn load(&self, key: &str) -> anyhow::Result<Option<SecretString>> {
        self.inner.load(key).await
    }
    async fn save(&self, key: &str, value: SecretString) -> anyhow::Result<()> {
        if self.fail.load(Ordering::SeqCst)
            && key.contains("icloud-folder-plan/")
            && serde_json::from_str::<serde_json::Value>(value.expose_secret())?["phase"]
                == "prepared"
        {
            anyhow::bail!("synthetic durable reset refusal");
        }
        self.inner.save(key, value).await
    }
    async fn remove(&self, _: &str) -> anyhow::Result<()> {
        panic!("no evidence removal")
    }
}
#[tokio::test]
async fn router_refuses_uncommitted_until_unsent_plan_reset_is_durable() {
    let (_temp, mut p) = super::super::tests::fixture();
    let vault = Arc::new(FailResetVault {
        inner: tests::MemoryVault::default(),
        fail: AtomicBool::new(false),
    });
    p.folder_vault = vault.clone();
    p.folder_create_test_adapter = Some(Arc::new(Adapter {
        vault: vault.clone(),
        endpoint: "127.0.0.1:1".parse().unwrap(),
        preflight_refused: AtomicBool::new(true),
        lose_response: false,
    }));
    let request = MutationRequest {
        scope: p.scope.clone(),
        intent: MutationIntent::CreateFolder {
            parent: ROOT_ID.into(),
            name: "Child".into(),
        },
    };
    let record = p
        .journal
        .lock()
        .unwrap()
        .enqueue_mutation(request.clone())
        .unwrap();
    let operation = record.id.to_string();
    let cancel = CancellationToken::new();
    p.prepare_mutation_for_operation(&operation, &request, &cancel)
        .await
        .unwrap();
    assert!(matches!(
        p.mutate_operation(&operation, &request, None, &cancel)
            .await,
        Err(MutationError::InsufficientStorage)
    ));
    vault.fail.store(true, Ordering::SeqCst);
    assert!(
        p.reconcile_operation(&operation, &request, None, &cancel)
            .await
            .is_err()
    );
    assert!(
        p.saved_plan(record.id, &request, None)
            .await
            .unwrap()
            .unwrap()
            .phase
            == PlanPhase::Sent
    );
    vault.fail.store(false, Ordering::SeqCst);
    assert!(matches!(
        p.reconcile_operation(&operation, &request, None, &cancel)
            .await
            .unwrap(),
        MutationReconciliation::Uncommitted
    ));
    assert!(
        p.saved_plan(record.id, &request, None)
            .await
            .unwrap()
            .unwrap()
            .phase
            == PlanPhase::Prepared
    );
}
