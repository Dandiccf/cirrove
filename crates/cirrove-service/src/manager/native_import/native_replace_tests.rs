use super::*;
use crate::native_import::NativeReplaceInput;
use cirrove_core::upload::{PackageSemanticIdentity, UploadRepresentation};
fn input(f: &Fixture) -> NativeReplaceInput {
    let mut original = node(
        "FILE::com.apple.CloudDocs::owned",
        Some(ROOT),
        "Owned.pages",
    );
    original.package = true;
    f.provider.nodes.lock().unwrap().push(original);
    NativeReplaceInput {
        selected: crate::native_trash::NativeTrashInput {
            expected_account_id: f.engine.account.id.clone(),
            path: "Owned.pages".into(),
            item_id: "FILE::com.apple.CloudDocs::owned".into(),
            etag: "v1".into(),
        },
        source: f.source.clone(),
        expected_root: "Source.pages".into(),
    }
}
fn semantic() -> PackageSemanticIdentity {
    PackageSemanticIdentity {
        version: 1,
        sha256: "a".repeat(64),
        entries: 1,
        files: 1,
        expanded_bytes: 3,
    }
}
fn empty(f: &Fixture) {
    assert!(f.journal.lock().unwrap().list(0, 100).unwrap().is_empty());
}
#[tokio::test]
async fn native_replace_admission_qualifies_once_and_retains_after_cancel() {
    let f = Fixture::new().await;
    let request = input(&f);
    let cancel = CancellationToken::new();
    let row = f
        .manager
        .enqueue_native_replacement_with(
            f.engine.clone(),
            request,
            cancel.clone(),
            |_, _, _| async { Ok(semantic()) },
        )
        .await
        .unwrap();
    cancel.cancel();
    let saved = f.journal.lock().unwrap().get(row.id).unwrap();
    assert!(
        matches!(&saved.representation,UploadRepresentation::PackageReplacementArchive{original,original_semantic,..} if original.id=="FILE::com.apple.CloudDocs::owned"&&original_semantic==&semantic())
    );
    assert_eq!(saved.id, row.id);
    let request = NativeReplaceInput {
        selected: crate::native_trash::NativeTrashInput {
            expected_account_id: f.engine.account.id.clone(),
            path: "Owned.pages".into(),
            item_id: "FILE::com.apple.CloudDocs::owned".into(),
            etag: "v1".into(),
        },
        source: f.source.clone(),
        expected_root: "Source.pages".into(),
    };
    assert!(
        f.manager
            .enqueue_native_replacement_with(
                f.engine.clone(),
                request,
                CancellationToken::new(),
                |_, _, _| async { Ok(semantic()) }
            )
            .await
            .is_err()
    );
    assert_eq!(f.journal.lock().unwrap().list(0, 100).unwrap().len(), 1);
}
#[tokio::test]
async fn native_replace_capture_races_refuse_before_queue() {
    for arm in 0..4 {
        let f = Fixture::new().await;
        let request = input(&f);
        let token = CancellationToken::new();
        let provider = f.provider.clone();
        let manager = f.manager.clone();
        let id = f.engine.account.id.clone();
        let cancel = token.clone();
        let result = f
            .manager
            .enqueue_native_replacement_with(
                f.engine.clone(),
                request,
                token,
                move |_, _, _| async move {
                    match arm {
                        0 => {
                            provider
                                .nodes
                                .lock()
                                .unwrap()
                                .iter_mut()
                                .find(|n| n.package)
                                .unwrap()
                                .etag = Some("v2".into())
                        }
                        1 => {
                            manager.writers.write().await.remove(&id);
                        }
                        2 => {
                            provider
                                .nodes
                                .lock()
                                .unwrap()
                                .iter_mut()
                                .find(|n| n.id == ROOT)
                                .unwrap()
                                .package = true
                        }
                        _ => cancel.cancel(),
                    }
                    Ok(semantic())
                },
            )
            .await;
        assert!(result.is_err(), "capture race {arm} accepted");
        empty(&f);
    }
}
#[tokio::test]
async fn native_replace_wrong_account_and_pre_cancel_never_capture() {
    for arm in 0..2 {
        let f = Fixture::new().await;
        let mut request = input(&f);
        let cancel = CancellationToken::new();
        if arm == 0 {
            request.selected.expected_account_id = uuid::Uuid::new_v4().to_string();
        } else {
            cancel.cancel();
        }
        assert!(
            f.manager
                .enqueue_native_replacement_with(
                    f.engine.clone(),
                    request,
                    cancel,
                    |_, _, _| async { panic!("capture must not run") }
                )
                .await
                .is_err()
        );
        empty(&f);
    }
}

#[tokio::test]
async fn native_replace_cancel_after_durable_enqueue_returns_exact_operation() {
    let f = Fixture::new().await;
    let input = input(&f);
    let cancel = CancellationToken::new();
    let (ready, observed) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    *f.manager.native_replace_after_enqueue.lock().unwrap() =
        Some(crate::manager::NativeReplaceEnqueuePause {
            ready,
            release: wait,
        });
    let manager = f.manager.clone();
    let engine = f.engine.clone();
    let token = cancel.clone();
    let task = tokio::spawn(async move {
        manager
            .enqueue_native_replacement_with(engine, input, token, |_, _, _| async {
                Ok(semantic())
            })
            .await
    });
    let operation = tokio::time::timeout(std::time::Duration::from_secs(3), observed)
        .await
        .unwrap()
        .unwrap();
    assert!(
        !task.is_finished(),
        "outer result escaped durable enqueue pause"
    );
    assert_eq!(
        f.journal.lock().unwrap().get(operation).unwrap().id,
        operation
    );
    cancel.cancel();
    release.send(()).unwrap();
    let result = task
        .await
        .unwrap()
        .expect("durable operation lost to late cancellation");
    assert_eq!(result.id, operation);
    assert_eq!(f.journal.lock().unwrap().list(0, 100).unwrap().len(), 1);
}
