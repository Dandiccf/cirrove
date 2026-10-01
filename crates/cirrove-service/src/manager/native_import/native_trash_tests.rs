use super::*;
use crate::native_trash::NativeTrashInput;
const ITEM: &str = "FILE::com.apple.CloudDocs::owned";
fn selection(f: &Fixture) -> NativeTrashInput {
    let mut target = node(ITEM, Some(ROOT), "Owned.pages");
    target.package = true;
    f.provider.nodes.lock().unwrap().push(target);
    NativeTrashInput {
        expected_account_id: f.engine.account.id.clone(),
        path: "Owned.pages".into(),
        item_id: ITEM.into(),
        etag: "v1".into(),
    }
}
fn no_mutation(f: &Fixture) {
    assert!(
        !f.journal
            .lock()
            .unwrap()
            .list_mutations(0, 100)
            .unwrap()
            .iter()
            .any(|r| matches!(
                r.request.intent,
                cirrove_core::mutation::MutationIntent::TrashNativeDocument { .. }
            ))
    );
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn native_trash_admission_enqueues_exact_standalone_once_without_content_read() {
    let f = Fixture::new().await;
    let input = selection(&f);
    let id = f
        .manager
        .enqueue_native_trash(f.engine.clone(), input.clone(), CancellationToken::new())
        .await
        .unwrap();
    let state = f
        .manager
        .native_trash_status(&f.engine, &input.expected_account_id, id)
        .await
        .unwrap();
    assert_eq!(state.state, crate::journal::MutationState::Pending);
    assert!(!state.removal_confirmed && !state.metadata_removed);
    let row = f.journal.lock().unwrap().mutation(id).unwrap();
    assert!(row.base.is_none() && row.working_file.is_none());
    assert!(
        matches!(row.request.intent, cirrove_core::mutation::MutationIntent::TrashNativeDocument { ref before } if before.id == ITEM && before.etag.as_deref() == Some("v1"))
    );
    assert!(
        f.manager
            .enqueue_native_trash(f.engine.clone(), input, CancellationToken::new())
            .await
            .is_err()
    );
    assert_eq!(
        f.journal
            .lock()
            .unwrap()
            .list_mutations(0, 100)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), 0);
    assert!(
        f.manager
            .native_trash_status(&f.engine, &uuid::Uuid::new_v4().to_string(), id)
            .await
            .is_err()
    );
}
#[tokio::test]
async fn native_trash_admission_refuses_wrong_identity_revision_projection_and_cancel() {
    for arm in 0..10 {
        let f = Fixture::new().await;
        let mut input = selection(&f);
        let token = CancellationToken::new();
        match arm {
            0 => input.expected_account_id = uuid::Uuid::new_v4().to_string(),
            1 => input.etag = "old".into(),
            2 => input.item_id = "FILE::com.apple.CloudDocs::other".into(),
            3 => input.path = "Owned.pages/Metadata/file".into(),
            4 => f.provider.nodes.lock().unwrap().last_mut().unwrap().package = false,
            5 => token.cancel(),
            6 => f.manager.status.write().await[0].mounted = false,
            7 => input.path = "Owned.PAGES".into(),
            8 => input.etag = "*".into(),
            _ => {
                f.manager.writers.write().await.remove(&f.engine.account.id);
            }
        }
        assert!(
            f.manager
                .enqueue_native_trash(f.engine.clone(), input, token)
                .await
                .is_err(),
            "arm {arm}"
        );
        no_mutation(&f);
    }
}
#[tokio::test]
async fn native_trash_admission_rechecks_mount_and_namespace_after_metadata_await() {
    for arm in 0..3 {
        let f = Fixture::new().await;
        let input = selection(&f);
        let ready = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        f.provider.roots.store(1, Ordering::SeqCst);
        *f.provider.pause.lock().unwrap() = Some((ready.clone(), release.clone()));
        let manager = f.manager.clone();
        let engine = f.engine.clone();
        let task = tokio::spawn(async move {
            manager
                .enqueue_native_trash(engine, input, CancellationToken::new())
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), ready.notified())
            .await
            .unwrap();
        match arm {
            0 => {
                f.manager.writers.write().await.remove(&f.engine.account.id);
            }
            1 => {
                f.control.freeze();
            }
            _ => {
                f.journal
                    .lock()
                    .unwrap()
                    .create_namespace_directory(
                        f.engine.scope("drive"),
                        ROOT.into(),
                        "changed".into(),
                    )
                    .unwrap();
            }
        }
        release.notify_one();
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), task)
                .await
                .unwrap()
                .unwrap()
                .is_err(),
            "arm {arm}"
        );
        no_mutation(&f);
    }
}

#[tokio::test]
async fn native_trash_admission_checks_frontier_at_durable_queue_boundary() {
    let f = Fixture::new().await;
    let input = selection(&f);
    let token = CancellationToken::new();
    let selected = f
        .control
        .native_trash_selection(&f.engine, &input, &token)
        .await
        .unwrap();
    f.journal
        .lock()
        .unwrap()
        .create_namespace_directory(f.engine.scope("drive"), ROOT.into(), "changed".into())
        .unwrap();
    assert!(f.control.enqueue_native_trash(selected, &token).is_err());
    no_mutation(&f);
}

#[tokio::test]
async fn native_trash_admission_syntax_matches_adapter_before_resolution() {
    let f = Fixture::new().await;
    let mut input = selection(&f);
    assert!(crate::native_trash::validate(&input));
    input.path = "Owned.PAGES".into();
    assert!(!crate::native_trash::validate(&input));
    input.path = "Owned.pages".into();
    input.etag = "*".into();
    assert!(!crate::native_trash::validate(&input));
    no_mutation(&f);
}

#[tokio::test]
async fn native_trash_socket_admission_watch_and_stop_retain_one_operation() {
    let f = Fixture::new().await;
    let input = selection(&f);
    let socket = f._temp.path().join("control.sock");
    let request = crate::TrashNativeDocumentRequest {
        label: f.engine.account.label.clone(),
        expected_account_id: input.expected_account_id.clone(),
        path: input.path,
        item_id: input.item_id,
        etag: input.etag,
    };
    assert!(
        crate::trash_native_document(&socket, &request)
            .await
            .is_err()
    );
    no_mutation(&f);
    let cancel = CancellationToken::new();
    let server = tokio::spawn(crate::serve_managed(
        f.engine.db.clone(),
        socket.clone(),
        cancel.clone(),
        Some(f.manager.clone()),
    ));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !socket.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let mut wrong = request.clone();
    wrong.expected_account_id = uuid::Uuid::new_v4().to_string();
    let refused = crate::trash_native_document(&socket, &wrong).await.unwrap();
    assert!(refused.job.is_none() && refused.refusal.is_some());
    no_mutation(&f);
    let job = crate::trash_native_document(&socket, &request)
        .await
        .unwrap()
        .job
        .unwrap();
    let progress = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let status = crate::status(&socket).await.unwrap();
            if let Some(progress) = status
                .accounts
                .iter()
                .flat_map(|a| &a.jobs)
                .find(|j| j.id == job.id)
                .and_then(|j| j.native_trash.clone())
            {
                break progress;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(progress.account_id, request.expected_account_id);
    assert!(!progress.removal_receipt_recorded && !progress.metadata_absence_recorded);
    assert!(
        crate::stop_job(
            &socket,
            &crate::StopJobRequest {
                label: request.label.clone(),
                id: job.id.clone()
            }
        )
        .await
        .unwrap()
        .stopped
    );
    let watched = crate::watch_native_trash(
        &socket,
        &crate::WatchNativeTrashRequest {
            label: request.label.clone(),
            expected_account_id: request.expected_account_id.clone(),
            operation: progress.operation,
        },
    )
    .await
    .unwrap()
    .job
    .unwrap();
    let observed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let status = crate::status(&socket).await.unwrap();
            if let Some(p) = status
                .accounts
                .iter()
                .flat_map(|a| &a.jobs)
                .find(|j| j.id == watched.id)
                .and_then(|j| j.native_trash.clone())
            {
                break p;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(observed.operation, progress.operation);
    assert!(!observed.metadata_absence_recorded);
    assert!(
        crate::stop_job(
            &socket,
            &crate::StopJobRequest {
                label: request.label,
                id: watched.id
            }
        )
        .await
        .unwrap()
        .stopped
    );
    let rows = f.journal.lock().unwrap().list_mutations(0, 100).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, progress.operation);
    assert_eq!(rows[0].state, crate::journal::MutationState::Pending);
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), 0);
    cancel.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
