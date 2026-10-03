use super::*;
use crate::jobs::{Job, JobState};
use cirrove_core::upload::{
    PackageHandoffReceipt, PackageSemanticIdentity, PackageUploadReceipt, RecoveryLocation,
};
async fn queued() -> (Fixture, crate::journal::UploadRecord) {
    let f = Fixture::new().await;
    let mut before = node("FILE::com.apple.CloudDocs::old", Some(ROOT), "Owned.pages");
    before.package = true;
    f.provider.nodes.lock().unwrap().push(before);
    let input = crate::native_import::NativeReplaceInput {
        selected: crate::native_trash::NativeTrashInput {
            expected_account_id: f.engine.account.id.clone(),
            path: "Owned.pages".into(),
            item_id: "FILE::com.apple.CloudDocs::old".into(),
            etag: "v1".into(),
        },
        source: f.source.clone(),
        expected_root: "Source.pages".into(),
    };
    let row = f
        .manager
        .enqueue_native_replacement_with(
            f.engine.clone(),
            input,
            CancellationToken::new(),
            |_, _, _| async {
                Ok(PackageSemanticIdentity {
                    version: 1,
                    sha256: "a".repeat(64),
                    entries: 1,
                    files: 1,
                    expanded_bytes: 1,
                })
            },
        )
        .await
        .unwrap();
    (f, row)
}
fn acknowledge(f: &Fixture, id: uuid::Uuid) -> crate::journal::UploadRecord {
    let mut j = f.journal.lock().unwrap();
    let row = j.get(id).unwrap();
    let attempt = j.claim_next().unwrap().unwrap();
    assert_eq!(attempt.id, id);
    let attempt = attempt.attempt.unwrap();
    j.reserve_identity_handoff(
        id,
        attempt,
        RecoveryLocation::Trash {
            local_name: format!("recovery-by-cirrove-{id}.pages"),
            parent: "FOLDER::com.apple.CloudDocs::TRASH_ROOT".into(),
        },
    )
    .unwrap();
    let UploadRepresentation::PackageReplacementArchive {
        original,
        semantic,
        original_semantic,
        ..
    } = row.representation
    else {
        panic!()
    };
    let current = Node {
        id: "FILE::com.apple.CloudDocs::new".into(),
        etag: Some("new-v1".into()),
        ..*original.clone()
    };
    let backup = Node {
        parent_id: Some("FOLDER::com.apple.CloudDocs::TRASH_ROOT".into()),
        etag: Some("trash-v1".into()),
        ..*original.clone()
    };
    {
        let mut nodes = f.provider.nodes.lock().unwrap();
        nodes.retain(|n| n.id != original.id);
        nodes.push(current.clone());
    }
    j.acknowledge_package_handoff(
        id,
        attempt,
        PackageHandoffReceipt {
            original: *original,
            current: PackageUploadReceipt {
                remote: current,
                semantic,
            },
            backup: PackageUploadReceipt {
                remote: backup,
                semantic: original_semantic,
            },
        },
    )
    .unwrap();
    j.get(id).unwrap()
}
async fn terminal(f: &Fixture, id: &str) -> Job {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let j = f.engine.jobs.find(id).unwrap();
            if !j.running() {
                return j;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn native_replace_watch_requires_publication_and_retains_operation_on_stop() {
    let (f, row) = queued().await;
    let confirmed = acknowledge(&f, row.id);
    let saved = serde_json::to_vec(&confirmed).unwrap();
    let watch = f
        .manager
        .watch_native_replacement(f.engine.clone(), &f.engine.account.id, row.id)
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    assert!(
        f.engine.jobs.find(&watch.id).unwrap().running(),
        "receipt alone published success"
    );
    f.engine.jobs.stop(&watch.id);
    let stopped = terminal(&f, &watch.id).await;
    assert_eq!(stopped.state, JobState::Stopped);
    let progress = stopped.native_replace.unwrap();
    assert_eq!(progress.operation, row.id);
    assert!(progress.current.is_none() && progress.recovery.is_none());
    assert_eq!(
        serde_json::to_vec(&f.journal.lock().unwrap().get(row.id).unwrap()).unwrap(),
        saved
    );
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn native_replace_watch_rejoins_saved_receipt_with_new_manager_and_no_old_job() {
    let (f, row) = queued().await;
    let confirmed = acknowledge(&f, row.id);
    let first = f
        .manager
        .watch_native_replacement(f.engine.clone(), &f.engine.account.id, row.id)
        .await
        .unwrap();
    f.engine.jobs.stop(&first.id);
    terminal(&f, &first.id).await;
    f.engine.jobs.stop(&first.id);
    assert!(f.engine.jobs.find(&first.id).is_none());
    f.journal
        .lock()
        .unwrap()
        .finish_package_publication(
            &confirmed,
            crate::journal::PackagePublicationStatus::Present(confirmed.remote.clone().unwrap()),
            0,
        )
        .unwrap();
    let manager = Arc::new(Manager::default());
    manager
        .status
        .write()
        .await
        .extend(f.manager.status.read().await.clone());
    manager
        .engines
        .write()
        .await
        .insert(f.engine.account.id.clone(), f.engine.clone());
    manager
        .writers
        .write()
        .await
        .insert(f.engine.account.id.clone(), f.control.clone());
    let watch = manager
        .watch_native_replacement(f.engine.clone(), &f.engine.account.id, row.id)
        .await
        .unwrap();
    let completed = terminal(&f, &watch.id).await;
    assert_eq!(completed.state, JobState::Succeeded);
    let proof = completed.native_replace.unwrap();
    assert_eq!(proof.operation, row.id);
    assert_eq!(proof.original.id, "FILE::com.apple.CloudDocs::old");
    assert_eq!(proof.current.unwrap().id, "FILE::com.apple.CloudDocs::new");
    assert_eq!(proof.recovery.unwrap().id, proof.original.id);
    {
        let mut nodes = f.provider.nodes.lock().unwrap();
        nodes
            .iter_mut()
            .find(|n| n.id == "FILE::com.apple.CloudDocs::new")
            .unwrap()
            .etag = Some("later-v2".into());
    }
    let stale = manager
        .watch_native_replacement(f.engine.clone(), &f.engine.account.id, row.id)
        .await
        .unwrap();
    assert_eq!(terminal(&f, &stale.id).await.state, JobState::Failed);
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn native_replace_watch_rejects_wrong_account_operation_and_withdrawn_mount() {
    let (f, row) = queued().await;
    assert!(
        f.manager
            .watch_native_replacement(f.engine.clone(), "wrong", row.id)
            .await
            .is_err()
    );
    assert!(
        f.manager
            .watch_native_replacement(f.engine.clone(), &f.engine.account.id, uuid::Uuid::new_v4())
            .await
            .is_err()
    );
    let watch = f
        .manager
        .watch_native_replacement(f.engine.clone(), &f.engine.account.id, row.id)
        .await
        .unwrap();
    f.manager.writers.write().await.remove(&f.engine.account.id);
    assert_eq!(terminal(&f, &watch.id).await.state, JobState::Failed);
    assert!(
        f.manager
            .watch_native_replacement(f.engine.clone(), &f.engine.account.id, row.id)
            .await
            .is_err()
    );
    assert_eq!(
        f.journal.lock().unwrap().get(row.id).unwrap().state,
        crate::journal::UploadState::Pending
    );
}
#[tokio::test]
async fn native_replace_recorded_receipt_rejects_semantic_and_identity_tampering() {
    let (f, row) = queued().await;
    let confirmed = acknowledge(&f, row.id);
    assert!(confirmed.native_replacement_receipt().is_some());
    for field in ["old_item", "backup", "newid", "semantic"] {
        let mut wire = serde_json::to_value(&confirmed).unwrap();
        match field {
            "old_item" => wire["identity_handoff"]["old_item"] = serde_json::json!("wrong"),
            "backup" => wire["identity_handoff"]["backup"]["id"] = serde_json::json!("wrong"),
            "newid" => wire["remote"]["id"] = serde_json::json!("FILE::com.apple.CloudDocs::old"),
            _ => wire["package_completion"]["sha256"] = serde_json::json!("b".repeat(64)),
        };
        let changed: crate::journal::UploadRecord = serde_json::from_value(wire).unwrap();
        assert!(changed.native_replacement_receipt().is_none(), "{field}");
    }
}

#[tokio::test]
async fn native_trash_after_replacement_requires_following_exact_new_owner() {
    let (f, row) = queued().await;
    let confirmed = acknowledge(&f, row.id);
    let remote = confirmed.remote.clone().unwrap();
    // Match the real publication worker: persist its fresh exact-ID observation
    // in Engine metadata before marking the journal publication complete. Merely
    // changing the provider fixture leaves the following overlay on stale cache.
    assert_eq!(
        f.engine
            .refresh_node(&confirmed.scope, &remote.id)
            .await
            .unwrap(),
        remote
    );
    let input = crate::native_trash::NativeTrashInput {
        expected_account_id: f.engine.account.id.clone(),
        path: "Owned.pages".into(),
        item_id: remote.id.clone(),
        etag: remote.etag.clone().unwrap(),
    };
    {
        let j = f.journal.lock().unwrap();
        j.finish_package_publication(
            &confirmed,
            crate::journal::PackagePublicationStatus::Present(remote.clone()),
            0,
        )
        .unwrap();
        assert!(j.list_mutations(0, 100).unwrap().is_empty());
    }
    // A clean but still authoritative owner must not be treated as an alias.
    assert!(
        f.manager
            .enqueue_native_trash(f.engine.clone(), input.clone(), CancellationToken::new())
            .await
            .is_err()
    );
    assert!(
        f.journal
            .lock()
            .unwrap()
            .list_mutations(0, 100)
            .unwrap()
            .is_empty()
    );
    let retained = {
        let mut j = f.journal.lock().unwrap();
        let owner = j
            .namespace_by_remote(&confirmed.scope, &remote.id)
            .unwrap()
            .unwrap();
        assert!(!owner.follows_remote);
        j.handoff_namespace(owner.id, owner.revision, remote.clone())
            .unwrap()
    };
    assert!(retained.follows_remote);
    assert!(retained.latest.is_none() && retained.working_file.is_none());
    let saved_owner = serde_json::to_vec(&retained).unwrap();
    // Old identity is the recovery copy, never the currently selected target.
    for bad in ["old_id", "old_etag", "account"] {
        let mut changed = input.clone();
        match bad {
            "old_id" => changed.item_id = "FILE::com.apple.CloudDocs::old".into(),
            "old_etag" => changed.etag = "v1".into(),
            _ => changed.expected_account_id = uuid::Uuid::new_v4().to_string(),
        }
        assert!(
            f.manager
                .enqueue_native_trash(f.engine.clone(), changed, CancellationToken::new())
                .await
                .is_err(),
            "{bad}"
        );
        assert!(
            f.journal
                .lock()
                .unwrap()
                .list_mutations(0, 100)
                .unwrap()
                .is_empty()
        );
    }
    let operation = f
        .manager
        .enqueue_native_trash(f.engine.clone(), input.clone(), CancellationToken::new())
        .await
        .unwrap();
    {
        let j = f.journal.lock().unwrap();
        let removal = j.mutation(operation).unwrap();
        assert!(
            matches!(&removal.request.intent, cirrove_core::mutation::MutationIntent::TrashNativeDocument { before } if before == &remote)
        );
        assert_ne!(remote.id, "FILE::com.apple.CloudDocs::old");
        assert!(removal.base.is_none() && removal.working_file.is_none());
        // Do not retire the stable identity potentially held by existing views.
        assert_eq!(
            serde_json::to_vec(&j.namespace_object(retained.id).unwrap()).unwrap(),
            saved_owner
        );
        let present = j
            .namespace_overlay(&confirmed.scope, ROOT, vec![remote.clone()])
            .unwrap();
        assert!(present.nodes.iter().any(|n| n.id == retained.node.id));
        let absent = j.namespace_overlay(&confirmed.scope, ROOT, vec![]).unwrap();
        assert!(absent.nodes.is_empty());
    }
    // Pending standalone work still reserves the same provider resource.
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
}
