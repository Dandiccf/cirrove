//! Synthetic protocol boundaries; no real-account or live quota claim.
#![allow(clippy::unwrap_used)]
use super::*;
use serde_json::{Value, json};
use std::sync::{
    Mutex as StdMutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[derive(Default)]
struct Vault {
    values: StdMutex<std::collections::HashMap<String, String>>,
    // Phase to refuse, and whether durable replacement happened before refusal.
    fail: StdMutex<Option<(&'static str, bool)>>,
}
#[async_trait]
impl CredentialVault for Vault {
    async fn load(&self, key: &str) -> anyhow::Result<Option<SecretString>> {
        Ok(self
            .values
            .lock()
            .unwrap()
            .get(key)
            .cloned()
            .map(SecretString::from))
    }
    async fn save(&self, key: &str, value: SecretString) -> anyhow::Result<()> {
        let phase: Value = serde_json::from_str(value.expose_secret())?;
        let failure = *self.fail.lock().unwrap();
        let fails = failure.is_some_and(|(name, _)| phase["state"]["phase"] == name);
        if !fails || failure.unwrap().1 {
            self.values
                .lock()
                .unwrap()
                .insert(key.into(), value.expose_secret().into());
        }
        if fails {
            anyhow::bail!("synthetic checkpoint persistence refusal");
        }
        Ok(())
    }
    async fn remove(&self, _: &str) -> anyhow::Result<()> {
        panic!("recovery never removes evidence")
    }
}
fn provider(scope: &Scope, endpoint: &str, vault: Arc<Vault>) -> ICloudFolderCreate {
    let mut session = ICloudReadSession::new().unwrap();
    session.drive_endpoint = Some(endpoint.parse().unwrap());
    ICloudFolderCreate {
        scope: scope.clone(),
        parent: Node {
            id: ROOT_ID.into(),
            parent_id: None,
            name: "iCloud Drive".into(),
            kind: NodeKind::Folder,
            size: 0,
            modified_unix: 0,
            etag: None,
            content_version: None,
            target: None,
            package: false,
        },
        checkpoint_vault: vault,
        session: Mutex::new(SessionState::Ready(Box::new(session))),
        #[cfg(feature = "write-probe")]
        reconciliation_only: false,
    }
}
fn request(scope: &Scope) -> MutationRequest {
    MutationRequest {
        scope: scope.clone(),
        intent: MutationIntent::CreateFolder {
            parent: ROOT_ID.into(),
            name: "Child".into(),
        },
    }
}
fn listing(created: bool) -> Value {
    let items = if created {
        vec![
            json!({"drivewsid":"FOLDER::com.apple.CloudDocs::created", "parentId": ROOT_ID, "name":"Child", "type":"FOLDER", "etag":"v1"}),
        ]
    } else {
        vec![]
    };
    json!([{"drivewsid":ROOT_ID,"type":"FOLDER","numberOfItems":items.len(),"items":items}])
}
fn receipt() -> Value {
    json!({"folders":[{"drivewsid":"FOLDER::com.apple.CloudDocs::created","name":"Child","status":"OK"}]})
}
async fn server(
    replies: Vec<(&'static str, u16, Value)>,
) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/", listener.local_addr().unwrap());
    let posts = Arc::new(AtomicUsize::new(0));
    let count = posts.clone();
    let task = tokio::spawn(async move {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            for (path, status, body) in replies {
                let (mut peer, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                loop {
                    let mut bytes = [0; 4096];
                    let n = peer.read(&mut bytes).await.unwrap();
                    assert!(n > 0 && request.len() + n <= 16384);
                    request.extend_from_slice(&bytes[..n]);
                    let Some(end) = request.windows(4).position(|b| b == b"\r\n\r\n") else { continue };
                    let end = end + 4;
                    let header = std::str::from_utf8(&request[..end]).unwrap().to_lowercase();
                    assert!(header.starts_with(&format!("post {} ", path.to_lowercase())));
                    let len: usize = header.lines().find_map(|l| l.strip_prefix("content-length: ")).unwrap().parse().unwrap();
                    if request.len() >= end + len { break; }
                }
                if path == "/createFolders" { count.fetch_add(1, Ordering::SeqCst); }
                // 0 simulates the peer disappearing after accepting the request.
                if status != 0 {
                    let body = body.to_string();
                    peer.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
                }
            }
            assert!(tokio::time::timeout(std::time::Duration::from_millis(100), listener.accept()).await.is_err(), "unexpected replay");
        }).await.expect("bounded local HTTP fixture");
    });
    (endpoint, posts, task)
}

#[tokio::test]
async fn folder_create_preflight_storage_refusal_restarts_without_duplicate_post() {
    let (endpoint, posts, task) = server(vec![
        ("/retrieveItemDetailsInFolders", 507, json!({})),
        ("/retrieveItemDetailsInFolders", 200, listing(false)),
        ("/createFolders", 200, receipt()),
        ("/retrieveItemDetailsInFolders", 200, listing(true)),
    ])
    .await;
    let scope = super::container_tests::scope();
    let request = request(&scope);
    let vault = Arc::new(Vault::default());
    let operation = Uuid::new_v4();
    let cancel = CancellationToken::new();
    let first = provider(&scope, &endpoint, vault.clone());
    first
        .prepare_mutation_for_operation(&operation.to_string(), &request, &cancel)
        .await
        .unwrap();
    assert!(matches!(
        first
            .mutate_operation(&operation.to_string(), &request, None, &cancel)
            .await,
        Err(MutationError::InsufficientStorage)
    ));
    assert_eq!(posts.load(Ordering::SeqCst), 0);
    drop(first);
    let reopened = provider(&scope, &endpoint, vault);
    assert!(matches!(
        reopened
            .reconcile_operation(&operation.to_string(), &request, None, &cancel)
            .await
            .unwrap(),
        MutationReconciliation::Uncommitted
    ));
    reopened
        .prepare_mutation_for_operation(&operation.to_string(), &request, &cancel)
        .await
        .unwrap();
    assert!(matches!(
        reopened
            .mutate_operation(&operation.to_string(), &request, None, &cancel)
            .await
            .unwrap(),
        MutationReceipt::Upsert(_)
    ));
    assert_eq!(posts.load(Ordering::SeqCst), 1);
    task.await.unwrap();
}

#[tokio::test]
async fn folder_create_lost_response_never_replays_without_created_identity() {
    let (endpoint, posts, task) = server(vec![
        ("/retrieveItemDetailsInFolders", 200, listing(false)),
        ("/createFolders", 0, json!({})),
    ])
    .await;
    let scope = super::container_tests::scope();
    let request = request(&scope);
    let vault = Arc::new(Vault::default());
    let operation = Uuid::new_v4();
    let cancel = CancellationToken::new();
    let first = provider(&scope, &endpoint, vault.clone());
    first
        .prepare_mutation_for_operation(&operation.to_string(), &request, &cancel)
        .await
        .unwrap();
    assert!(
        first
            .mutate_operation(&operation.to_string(), &request, None, &cancel)
            .await
            .is_err()
    );
    drop(first);
    let reopened = provider(&scope, &endpoint, vault);
    assert!(matches!(
        reopened
            .reconcile_operation(&operation.to_string(), &request, None, &cancel)
            .await
            .unwrap(),
        MutationReconciliation::Indeterminate
    ));
    assert!(
        reopened
            .prepare_mutation_for_operation(&operation.to_string(), &request, &cancel)
            .await
            .is_err()
    );
    assert!(
        reopened
            .mutate_operation(&operation.to_string(), &request, None, &cancel)
            .await
            .is_err()
    );
    assert_eq!(posts.load(Ordering::SeqCst), 1);
    task.await.unwrap();
}

#[tokio::test]
async fn folder_create_failed_dispatch_checkpoint_never_posts() {
    for persisted in [false, true] {
        let (endpoint, posts, task) =
            server(vec![("/retrieveItemDetailsInFolders", 200, listing(false))]).await;
        let scope = super::container_tests::scope();
        let request = request(&scope);
        let vault = Arc::new(Vault::default());
        let operation = Uuid::new_v4();
        let cancel = CancellationToken::new();
        let p = provider(&scope, &endpoint, vault.clone());
        p.prepare_mutation_for_operation(&operation.to_string(), &request, &cancel)
            .await
            .unwrap();
        *vault.fail.lock().unwrap() = Some(("may_have_sent", persisted));
        assert!(
            p.mutate_operation(&operation.to_string(), &request, None, &cancel)
                .await
                .is_err()
        );
        let result = p
            .reconcile_operation(&operation.to_string(), &request, None, &cancel)
            .await
            .unwrap();
        if persisted {
            assert!(matches!(result, MutationReconciliation::Indeterminate));
        } else {
            assert!(matches!(result, MutationReconciliation::Uncommitted));
        }
        assert_eq!(posts.load(Ordering::SeqCst), 0);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn folder_create_checkpoint_requires_exact_binding_and_preserves_legacy_receipts() {
    let scope = super::container_tests::scope();
    let request = request(&scope);
    let vault = Arc::new(Vault::default());
    let operation = Uuid::new_v4();
    let cancel = CancellationToken::new();
    let p = provider(&scope, "http://127.0.0.1:1/", vault.clone());
    assert!(matches!(
        p.reconcile_operation(&operation.to_string(), &request, None, &cancel)
            .await
            .unwrap(),
        MutationReconciliation::Indeterminate
    ));
    p.prepare_mutation_for_operation(&operation.to_string(), &request, &cancel)
        .await
        .unwrap();
    let original: Value =
        serde_json::from_str(vault.values.lock().unwrap().get(&p.key(operation)).unwrap()).unwrap();
    let mut foreign_scope = scope.clone();
    foreign_scope.account = Uuid::new_v4().to_string();
    for (field, value) in [
        ("operation", json!(Uuid::new_v4())),
        ("scope", json!(foreign_scope)),
        ("parent", json!("FOLDER::com.apple.CloudDocs::foreign")),
        ("name", json!("Foreign")),
        ("version", json!(3)),
        ("state", json!({"phase":"unknown"})),
        ("id", json!("FOLDER::com.apple.CloudDocs::ambiguous")),
    ] {
        let mut changed = original.clone();
        changed[field] = value;
        assert!(
            p.check_checkpoint(
                operation,
                &request,
                &SecretString::from(changed.to_string())
            )
            .is_err(),
            "{field}"
        );
    }
    let mut legacy = original;
    legacy["version"] = json!(1);
    legacy.as_object_mut().unwrap().remove("state");
    legacy["id"] = json!("FOLDER::com.apple.CloudDocs::created");
    assert!(matches!(
        p.check_checkpoint(operation, &request, &SecretString::from(legacy.to_string()))
            .unwrap(),
        CreatePhase::Created { .. }
    ));
    legacy.as_object_mut().unwrap().remove("id");
    assert!(
        p.check_checkpoint(operation, &request, &SecretString::from(legacy.to_string()))
            .is_err()
    );
}

#[tokio::test]
async fn folder_create_initial_checkpoint_failure_and_foreign_name_never_dispatch() {
    let scope = super::container_tests::scope();
    let request = request(&scope);
    let vault = Arc::new(Vault::default());
    let operation = Uuid::new_v4();
    let cancel = CancellationToken::new();
    let p = provider(&scope, "http://127.0.0.1:1/", vault.clone());
    *vault.fail.lock().unwrap() = Some(("not_sent", false));
    assert!(
        p.prepare_mutation_for_operation(&operation.to_string(), &request, &cancel)
            .await
            .is_err()
    );
    assert!(p.saved_phase(operation, &request).await.unwrap().is_none());
    assert!(matches!(
        p.reconcile_operation(&operation.to_string(), &request, None, &cancel)
            .await
            .unwrap(),
        MutationReconciliation::Indeterminate
    ));
    *vault.fail.lock().unwrap() = None;
    let (endpoint, posts, task) =
        server(vec![("/retrieveItemDetailsInFolders", 200, listing(true))]).await;
    let p = provider(&scope, &endpoint, vault);
    p.prepare_mutation_for_operation(&operation.to_string(), &request, &cancel)
        .await
        .unwrap();
    assert!(matches!(
        p.mutate_operation(&operation.to_string(), &request, None, &cancel)
            .await,
        Err(MutationError::Conflict)
    ));
    assert_eq!(posts.load(Ordering::SeqCst), 0);
    task.await.unwrap();
}

#[tokio::test]
async fn folder_create_receipt_save_failure_preserves_potentially_sent_boundary() {
    let (endpoint, posts, task) = server(vec![
        ("/retrieveItemDetailsInFolders", 200, listing(false)),
        ("/createFolders", 200, receipt()),
    ])
    .await;
    let scope = super::container_tests::scope();
    let request = request(&scope);
    let vault = Arc::new(Vault::default());
    let operation = Uuid::new_v4();
    let cancel = CancellationToken::new();
    let p = provider(&scope, &endpoint, vault.clone());
    p.prepare_mutation_for_operation(&operation.to_string(), &request, &cancel)
        .await
        .unwrap();
    *vault.fail.lock().unwrap() = Some(("created", false));
    assert!(
        p.mutate_operation(&operation.to_string(), &request, None, &cancel)
            .await
            .is_err()
    );
    assert!(matches!(
        p.reconcile_operation(&operation.to_string(), &request, None, &cancel)
            .await
            .unwrap(),
        MutationReconciliation::Indeterminate
    ));
    assert!(
        p.mutate_operation(&operation.to_string(), &request, None, &cancel)
            .await
            .is_err()
    );
    assert_eq!(posts.load(Ordering::SeqCst), 1);
    task.await.unwrap();
}
