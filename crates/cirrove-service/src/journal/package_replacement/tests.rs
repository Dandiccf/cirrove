#![allow(clippy::unwrap_used)]
use super::*;
use crate::native_import::ValidatedPackageArchive;
use cirrove_core::{
    CancellationToken,
    upload::{PackageHandoffReceipt, PackageUploadReceipt, RecoveryLocation},
};
fn scope() -> Scope {
    Scope {
        account: "native-replacement".into(),
        provider: "icloud".into(),
        collection: "drive".into(),
    }
}
fn before() -> Node {
    Node {
        id: "FILE::com.apple.CloudDocs::original".into(),
        parent_id: Some("FOLDER::com.apple.CloudDocs::owned".into()),
        name: "Owned.pages".into(),
        kind: NodeKind::Folder,
        size: 123,
        modified_unix: 0,
        etag: Some("original-v1".into()),
        content_version: None,
        target: None,
        package: true,
    }
}
fn original_semantic() -> PackageSemanticIdentity {
    PackageSemanticIdentity {
        version: 1,
        sha256: "a".repeat(64),
        entries: 1,
        files: 1,
        expanded_bytes: 123,
    }
}
fn temp() -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    std::fs::set_permissions(t.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    t
}
fn archive(t: &tempfile::TempDir) -> ValidatedPackageArchive {
    let path = t.path().join(format!("source-{}.zip", Uuid::new_v4()));
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .unwrap();
    f.write_all(&crate::native_import::synthetic_package_archive(
        "Source.pages/Document",
        b"edited owned native content",
    ))
    .unwrap();
    f.sync_all().unwrap();
    ValidatedPackageArchive::capture(&path, t.path(), "Source.pages", &CancellationToken::new())
        .unwrap()
}
fn enqueue(j: &mut UploadJournal, t: &tempfile::TempDir) -> UploadRecord {
    j.enqueue_validated_package_replacement(
        scope(),
        before(),
        original_semantic(),
        archive(t),
        &CancellationToken::new(),
    )
    .unwrap()
}
fn location() -> RecoveryLocation {
    RecoveryLocation::Trash {
        local_name: "recovery-cirrove.pages".into(),
        parent: "FOLDER::com.apple.CloudDocs::TRASH_ROOT".into(),
    }
}
fn receipt(row: &UploadRecord) -> PackageHandoffReceipt {
    let UploadRepresentation::PackageReplacementArchive { semantic, .. } = &row.representation
    else {
        panic!("typed replacement")
    };
    let mut current = before();
    current.id = "FILE::com.apple.CloudDocs::replacement".into();
    current.etag = Some("replacement-v1".into());
    current.size = 555;
    let mut backup = before();
    backup.parent_id = Some("FOLDER::com.apple.CloudDocs::TRASH_ROOT".into());
    backup.etag = Some("trash-v2".into());
    PackageHandoffReceipt {
        original: before(),
        current: PackageUploadReceipt {
            remote: current,
            semantic: semantic.clone(),
        },
        backup: PackageUploadReceipt {
            remote: backup,
            semantic: original_semantic(),
        },
    }
}
#[test]
fn package_handoff_two_id_ack_is_atomic_and_retains_recovery_across_restart() {
    let root = temp();
    let staging = temp();
    let mut j = UploadJournal::open(root.path(), &scope().account, 1024 * 1024).unwrap();
    let row = enqueue(&mut j, &staging);
    let bytes = std::fs::read(j.objects.join(row.id.to_string())).unwrap();
    let claimed = j.claim_next().unwrap().unwrap();
    let attempt = claimed.attempt.unwrap();
    let recovery = j
        .reserve_identity_handoff(row.id, attempt, location())
        .unwrap();
    assert!(!j.namespace_object(recovery).unwrap().remote_owned);
    let r = receipt(&row);
    let current = r.current.remote.clone();
    let backup = r.backup.remote.clone();
    assert!(j.acknowledge(row.id, attempt, current.clone()).is_err());
    assert!(
        j.acknowledge_package(
            row.id,
            attempt,
            PackageUploadReceipt {
                remote: current.clone(),
                semantic: r.current.semantic.clone()
            }
        )
        .is_err()
    );
    assert!(
        j.acknowledge_identity_handoff(row.id, attempt, current.clone(), backup.clone())
            .is_err()
    );
    j.acknowledge_package_handoff(row.id, attempt, r).unwrap();
    let completed = j.get(row.id).unwrap();
    assert_eq!(completed.state, UploadState::Uploaded);
    assert!(completed.package_completion.is_some());
    let owner = j.namespace_for_operation(row.id).unwrap().unwrap();
    assert_eq!(owner.remote, Some(current));
    let old = j.namespace_object(recovery).unwrap();
    assert!(old.unlinked && old.remote_owned);
    assert_eq!(old.remote, Some(backup));
    assert_eq!(
        j.package_publication_status(row.id).unwrap(),
        PackagePublicationStatus::Pending
    );
    assert_eq!(
        j.package_publication_due(now_seconds())
            .unwrap()
            .unwrap()
            .id,
        row.id
    );
    drop(j);
    let ro = RecoveryJournal::open(root.path(), &scope().account).unwrap();
    assert_eq!(ro.list(0, 100).unwrap().len(), 1);
    drop(ro);
    let j = UploadJournal::open(root.path(), &scope().account, 1024 * 1024).unwrap();
    assert_eq!(j.get(row.id).unwrap().state, UploadState::Uploaded);
    assert_eq!(
        std::fs::read(j.objects.join(row.id.to_string())).unwrap(),
        bytes
    );
    assert!(j.namespace_object(recovery).unwrap().remote_owned);
    let version: u32 =
        j.db.pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
    assert_eq!(version, JOURNAL_SCHEMA);
}
#[test]
fn package_handoff_forged_receipts_do_not_transfer_either_identity() {
    let root = temp();
    let staging = temp();
    let mut j = UploadJournal::open(root.path(), &scope().account, 1024 * 1024).unwrap();
    let row = enqueue(&mut j, &staging);
    let attempt = j.claim_next().unwrap().unwrap().attempt.unwrap();
    let recovery = j
        .reserve_identity_handoff(row.id, attempt, location())
        .unwrap();
    let owner_before = serde_json::to_value(j.namespace_for_operation(row.id).unwrap()).unwrap();
    for fault in 0..8 {
        let mut r = receipt(&row);
        match fault {
            0 => r.original.etag = Some("stale".into()),
            1 => r.current.semantic.sha256 = "b".repeat(64),
            2 => r.backup.semantic.sha256 = "b".repeat(64),
            3 => r.backup.remote.id = "FILE::com.apple.CloudDocs::foreign".into(),
            4 => r.current.remote.id = before().id,
            5 => r.backup.remote.parent_id = before().parent_id,
            6 => r.current.remote.name = "Another.pages".into(),
            _ => r.backup.remote.size += 1,
        }
        assert!(
            j.acknowledge_package_handoff(row.id, attempt, r).is_err(),
            "fault {fault}"
        );
        assert_eq!(j.get(row.id).unwrap().state, UploadState::Uploading);
        assert!(!j.namespace_object(recovery).unwrap().remote_owned);
        assert_eq!(
            serde_json::to_value(j.namespace_for_operation(row.id).unwrap()).unwrap(),
            owner_before
        );
    }
}
#[test]
fn package_replacement_admission_is_explicit_and_refuses_conflicting_pending_owner() {
    let root = temp();
    let staging = temp();
    let mut j = UploadJournal::open(root.path(), &scope().account, 1024 * 1024).unwrap();
    let row = enqueue(&mut j, &staging);
    assert!(matches!(
        j.enqueue_validated_package_replacement(
            scope(),
            before(),
            original_semantic(),
            archive(&staging),
            &CancellationToken::new()
        ),
        Err(JournalError::Stale)
    ));
    assert_eq!(j.list(0, 100).unwrap().len(), 1);
    assert!(
        j.enqueue_represented(
            scope(),
            row.intent.clone(),
            WriteOrder::default(),
            None,
            row.representation.clone(),
            b"unvalidated".as_slice()
        )
        .is_err()
    );
    let attempt = j.claim_next().unwrap().unwrap().attempt.unwrap();
    assert!(
        j.reserve_identity_handoff(
            row.id,
            attempt,
            RecoveryLocation::Sibling {
                name: "recovery.pages".into()
            }
        )
        .is_err()
    );
    let recovered = j
        .reserve_identity_handoff(row.id, attempt, location())
        .unwrap();
    drop(j);
    let mut restarted = UploadJournal::open(root.path(), &scope().account, 1024 * 1024).unwrap();
    let pending = restarted.get(row.id).unwrap();
    assert_eq!(pending.state, UploadState::VerifyRequired);
    assert!(pending.representation == row.representation);
    restarted.request_retry(row.id).unwrap();
    // A retry request cannot turn an uncertain attempt into a fresh write.
    assert!(restarted.claim_next().unwrap().is_none());
    let claimed = restarted.claim_next_verification().unwrap().unwrap();
    assert_eq!(claimed.id, row.id);
    let attempt = claimed.attempt.unwrap();
    assert_eq!(
        restarted
            .reserve_identity_handoff(row.id, attempt, location())
            .unwrap(),
        recovered
    );
    restarted
        .acknowledge_package_handoff(row.id, attempt, receipt(&row))
        .unwrap();
}
#[test]
fn package_replacement_wrong_scope_and_cancel_leave_no_queue() {
    let root = temp();
    let staging = temp();
    let mut j = UploadJournal::open(root.path(), &scope().account, 1024 * 1024).unwrap();
    let mut other = scope();
    other.account = "other".into();
    assert!(matches!(
        j.enqueue_validated_package_replacement(
            other,
            before(),
            original_semantic(),
            archive(&staging),
            &CancellationToken::new()
        ),
        Err(JournalError::Account)
    ));
    let cancel = CancellationToken::new();
    let captured = archive(&staging);
    cancel.cancel();
    assert!(
        j.enqueue_validated_package_replacement(
            scope(),
            before(),
            original_semantic(),
            captured,
            &cancel
        )
        .is_err()
    );
    assert!(j.list(0, 100).unwrap().is_empty());
    assert!(j.namespace_objects().unwrap().is_empty());
}

#[derive(Default)]
struct Vault(std::sync::Mutex<Option<secrecy::SecretString>>);
#[async_trait::async_trait]
impl cirrove_auth::CredentialVault for Vault {
    async fn load(&self, _: &str) -> anyhow::Result<Option<secrecy::SecretString>> {
        Ok(self.0.lock().unwrap().clone())
    }
    async fn save(&self, _: &str, value: secrecy::SecretString) -> anyhow::Result<()> {
        *self.0.lock().unwrap() = Some(value);
        Ok(())
    }
    async fn remove(&self, _: &str) -> anyhow::Result<()> {
        *self.0.lock().unwrap() = None;
        Ok(())
    }
}
struct Provider {
    row: UploadRecord,
    uncertain: bool,
    begins: std::sync::atomic::AtomicUsize,
    commits: std::sync::atomic::AtomicUsize,
    reconciliations: std::sync::atomic::AtomicUsize,
}
#[async_trait::async_trait]
impl cirrove_core::upload::UploadProvider for Provider {
    fn staged_recovery_location(
        &self,
        op: &str,
        r: &cirrove_core::upload::UploadRequest,
    ) -> Option<RecoveryLocation> {
        assert_eq!(op, self.row.id.to_string());
        assert!(r.representation == self.row.representation);
        Some(location())
    }
    async fn begin_upload(
        &self,
        r: &cirrove_core::upload::UploadRequest,
        _: &CancellationToken,
    ) -> cirrove_core::upload::Result<cirrove_core::upload::UploadStep> {
        assert!(r.representation == self.row.representation);
        r.validate()?;
        self.begins
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(cirrove_core::upload::UploadStep::Commit(
            "retained-native-handoff".into(),
        ))
    }
    async fn inspect_upload(
        &self,
        _: &cirrove_core::upload::UploadRequest,
        _: &secrecy::SecretString,
        _: &CancellationToken,
    ) -> cirrove_core::upload::Result<cirrove_core::upload::UploadStep> {
        Err(cirrove_core::upload::UploadError::Uncertain)
    }
    async fn upload_part(
        &self,
        _: &cirrove_core::upload::UploadRequest,
        _: &secrecy::SecretString,
        _: u64,
        _: Vec<u8>,
        _: &CancellationToken,
    ) -> cirrove_core::upload::Result<cirrove_core::upload::UploadStep> {
        panic!("no ordinary file parts")
    }
    async fn commit_upload(
        &self,
        _: &cirrove_core::upload::UploadRequest,
        _: &secrecy::SecretString,
        _: &CancellationToken,
    ) -> cirrove_core::upload::Result<cirrove_core::upload::UploadStep> {
        self.commits
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.uncertain {
            Err(cirrove_core::upload::UploadError::Uncertain)
        } else {
            Ok(cirrove_core::upload::UploadStep::PackageHandoffComplete(
                Box::new(receipt(&self.row)),
            ))
        }
    }
    async fn reconcile_upload(
        &self,
        r: &cirrove_core::upload::UploadRequest,
        checkpoint: Option<&secrecy::SecretString>,
        _: &CancellationToken,
    ) -> cirrove_core::upload::Result<cirrove_core::upload::Reconciliation> {
        use secrecy::ExposeSecret;
        assert!(r.representation == self.row.representation);
        assert_eq!(
            checkpoint.unwrap().expose_secret(),
            "retained-native-handoff"
        );
        self.reconciliations
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(
            cirrove_core::upload::Reconciliation::PackageHandoffCommitted(Box::new(receipt(
                &self.row,
            ))),
        )
    }
}
#[tokio::test]
async fn package_handoff_shared_worker_reconciles_restart_without_second_commit() {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    for uncertain in [false, true] {
        let root = temp();
        let staging = temp();
        let mut j = UploadJournal::open(root.path(), &scope().account, 1024 * 1024).unwrap();
        let row = enqueue(&mut j, &staging);
        let provider = Arc::new(Provider {
            row: row.clone(),
            uncertain,
            begins: AtomicUsize::new(0),
            commits: AtomicUsize::new(0),
            reconciliations: AtomicUsize::new(0),
        });
        let vault = Arc::new(Vault::default());
        let journal = Arc::new(Mutex::new(j));
        let worker = crate::transfers::TransferWorker::new(
            journal.clone(),
            provider.clone(),
            vault.clone(),
            CancellationToken::new(),
        );
        let first = worker.run_once().await.unwrap().unwrap();
        if uncertain {
            assert_eq!(first.state, UploadState::VerifyRequired);
            assert!(vault.0.lock().unwrap().is_some());
        } else {
            assert_eq!(first.state, UploadState::Uploaded);
        }
        drop(worker);
        drop(journal);
        if uncertain {
            let ro = RecoveryJournal::open(root.path(), &scope().account).unwrap();
            let expected = serde_json::to_value(&ro.list(0, 100).unwrap()[0]).unwrap();
            let export = staging.path().join("retained-edit.zip");
            let exported = ro
                .local_export_source(row.id)
                .unwrap()
                .copy_to(&export, &CancellationToken::new(), |_| {})
                .unwrap();
            assert_eq!(exported.sha256, row.sha256);
            assert_eq!(
                serde_json::to_value(&ro.list(0, 100).unwrap()[0]).unwrap(),
                expected
            );
            drop(ro);
            let mut j = UploadJournal::open(root.path(), &scope().account, 1024 * 1024).unwrap();
            j.request_retry(row.id).unwrap();
            let journal = Arc::new(Mutex::new(j));
            let worker = crate::transfers::TransferWorker::new(
                journal.clone(),
                provider.clone(),
                vault.clone(),
                CancellationToken::new(),
            );
            assert_eq!(
                worker.run_once().await.unwrap().unwrap().state,
                UploadState::Uploaded
            );
            assert!(
                journal
                    .lock()
                    .unwrap()
                    .get(row.id)
                    .unwrap()
                    .package_completion
                    .is_some()
            );
            drop(worker);
            drop(journal);
        }
        assert_eq!(provider.begins.load(Ordering::SeqCst), 1);
        assert_eq!(provider.commits.load(Ordering::SeqCst), 1);
        assert_eq!(
            provider.reconciliations.load(Ordering::SeqCst),
            usize::from(uncertain)
        );
        assert!(vault.0.lock().unwrap().is_none());
    }
}

#[test]
fn package_handoff_schema17_fences_older_writers_and_preserves_schema16_payloads() {
    let root = temp();
    let mut j = UploadJournal::open(root.path(), &scope().account, 1024 * 1024).unwrap();
    let old = j
        .enqueue(
            scope(),
            UploadIntent::Create {
                parent: "root".into(),
                name: "ordinary.txt".into(),
            },
            b"accepted ordinary bytes".as_slice(),
        )
        .unwrap();
    j.db.pragma_update(None, "user_version", 16).unwrap();
    drop(j);
    let before = std::fs::read(root.path().join("uploads.db")).unwrap();
    let ro = RecoveryJournal::open(root.path(), &scope().account).unwrap();
    assert_eq!(ro.list(0, 100).unwrap()[0].id, old.id);
    drop(ro);
    assert_eq!(
        std::fs::read(root.path().join("uploads.db")).unwrap(),
        before
    );
    let j = UploadJournal::open(root.path(), &scope().account, 1024 * 1024).unwrap();
    let version: u32 =
        j.db.pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
    assert_eq!(version, JOURNAL_SCHEMA);
    assert!(version > 16);
    assert_eq!(
        std::fs::read(j.objects.join(old.id.to_string())).unwrap(),
        b"accepted ordinary bytes"
    );
    assert!(j.get(old.id).unwrap().representation.is_file_bytes());
}

#[test]
fn native_selection_after_completed_namespace_handoff_refuses_pending_successor() {
    let root = temp();
    let staging = temp();
    let mut j = UploadJournal::open(root.path(), &scope().account, 1024 * 1024).unwrap();
    let row = enqueue(&mut j, &staging);
    let attempt = j.claim_next().unwrap().unwrap().attempt.unwrap();
    j.reserve_identity_handoff(row.id, attempt, location())
        .unwrap();
    let result = receipt(&row);
    let current = result.current.remote.clone();
    let semantic = result.current.semantic.clone();
    j.acknowledge_package_handoff(row.id, attempt, result)
        .unwrap();
    let completed = j.get(row.id).unwrap();
    j.finish_package_publication(
        &completed,
        PackagePublicationStatus::Present(current.clone()),
        now_seconds(),
    )
    .unwrap();
    let owner = j.namespace_for_operation(row.id).unwrap().unwrap();
    let followed = j
        .handoff_namespace(owner.id, owner.revision, current.clone())
        .unwrap();
    assert!(followed.follows_remote && followed.latest.is_none());
    assert!(
        !j.namespace_is_clean(&followed).unwrap(),
        "already-following objects are not handoff candidates"
    );
    let selection = |j: &UploadJournal| {
        let parent = Node {
            id: current.parent_id.clone().unwrap(),
            parent_id: None,
            name: "Owned".into(),
            kind: NodeKind::Folder,
            size: 0,
            modified_unix: 0,
            etag: Some("parent-v1".into()),
            content_version: None,
            target: None,
            package: false,
        };
        crate::native_trash::NativeTrashAdmission {
            parent: crate::native_import::ImportParent {
                scope: scope(),
                route: vec![parent.clone()],
                parent,
                // The old remote membership is deliberately still cached.
                children: vec![before(), current.clone()],
                frontier: j.namespace_publication(0).unwrap().through,
            },
            target: current.clone(),
        }
    };
    j.validate_native_selection(&selection(&j), &CancellationToken::new())
        .unwrap();
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(
        j.validate_native_selection(&selection(&j), &cancelled)
            .is_err()
    );
    let mut stale = selection(&j);
    stale.target.etag = Some("different-revision".into());
    assert!(
        j.validate_native_selection(&stale, &CancellationToken::new())
            .is_err()
    );
    // A concrete queued successor makes this owner dirty again. Its current
    // remote receipt alone must never authorize another overlapping operation.
    let successor = j
        .enqueue_validated_package_replacement(
            scope(),
            current.clone(),
            semantic,
            archive(&staging),
            &CancellationToken::new(),
        )
        .unwrap();
    let pending = j.namespace_for_operation(successor.id).unwrap().unwrap();
    assert!(!pending.follows_remote && pending.latest == Some(successor.id));
    assert!(!j.namespace_is_clean(&pending).unwrap());
    assert!(
        j.validate_native_selection(&selection(&j), &CancellationToken::new())
            .is_err()
    );
    assert_eq!(j.list(0, 100).unwrap().len(), 2);
}

fn abandon_conflict(j: &mut UploadJournal, t: &tempfile::TempDir) -> UploadRecord {
    let row = enqueue(j, t);
    let attempt = j.claim_next().unwrap().unwrap().attempt.unwrap();
    j.reserve_identity_handoff(row.id, attempt, location())
        .unwrap();
    j.record_session(row.id, attempt, row.id, row.size).unwrap();
    j.stop_attempt(row.id, attempt, UploadState::Conflict)
        .unwrap();
    j.get(row.id).unwrap()
}
fn abandon_evidence(row: &UploadRecord) -> cirrove_icloud::NativeReplacementAbandonEvidence {
    let mut staged = before();
    staged.id = "FILE::com.apple.CloudDocs::staged".into();
    staged.name = format!("staged-by-cirrove-{}.pages", row.id);
    staged.etag = Some("stage-v1".into());
    cirrove_icloud::NativeReplacementAbandonEvidence::synthetic_for_test(
        cirrove_icloud::NativeReplacementAbandonRecord {
            version: 1,
            operation: row.id,
            request: cirrove_core::upload::UploadRequest {
                scope: row.scope.clone(),
                intent: row.intent.clone(),
                representation: row.representation.clone(),
                size: row.size,
                sha256: row.sha256.clone(),
            },
            original: before(),
            staged,
            checkpoint_sha256: "a".repeat(64),
            observed_unix: 1,
        },
    )
    .unwrap()
}
#[test]
fn native_stage_abandonment_retains_export_and_identity_history_then_allows_fresh_operation() {
    let root = temp();
    let staging = temp();
    let mut j = UploadJournal::open(root.path(), &scope().account, 1024 * 1024).unwrap();
    let row = abandon_conflict(&mut j, &staging);
    let before_bytes = std::fs::read(j.objects.join(row.id.to_string())).unwrap();
    let recovery_id = row
        .identity_handoff
        .as_ref()
        .unwrap()
        .unconfirmed_native(&before())
        .unwrap()
        .0;
    let recovery_before = serde_json::to_value(j.namespace_object(recovery_id).unwrap()).unwrap();
    let prep = j.prepare_native_stage_abandonment(row.id).unwrap();
    let receipt = j
        .abandon_native_stage(prep, abandon_evidence(&row), &CancellationToken::new())
        .unwrap();
    assert_eq!(receipt.operation, row.id);
    let after = j.get(row.id).unwrap();
    assert_eq!(after.state, UploadState::Resolved);
    assert_eq!(after.session_key, row.session_key);
    assert!(after.remote.is_none() && after.package_completion.is_none());
    assert_eq!(
        serde_json::to_value(&after.identity_handoff).unwrap(),
        serde_json::to_value(&row.identity_handoff).unwrap()
    );
    assert_eq!(
        serde_json::to_value(j.namespace_object(recovery_id).unwrap()).unwrap(),
        recovery_before
    );
    assert_eq!(
        std::fs::read(j.objects.join(row.id.to_string())).unwrap(),
        before_bytes
    );
    assert!(j.request_retry(row.id).is_err());
    assert!(j.claim_next().unwrap().is_none());
    assert!(j.claim_next_verification().unwrap().is_none());
    assert!(j.prepare_native_stage_abandonment(row.id).is_err());
    let owner = j.namespace_for_operation(row.id).unwrap().unwrap();
    assert!(owner.follows_remote && owner.remote_owned && owner.latest.is_none());
    assert_eq!(owner.remote, Some(before()));
    drop(j);
    let ro = RecoveryJournal::open(root.path(), &scope().account).unwrap();
    let retained = ro.native_stage_abandonment(row.id).unwrap().unwrap();
    assert_eq!(retained.staged, receipt.staged);
    let destination = staging.path().join("abandoned-save.zip");
    let exported = ro
        .local_export_source(row.id)
        .unwrap()
        .copy_to(&destination, &CancellationToken::new(), |_| {})
        .unwrap();
    assert_eq!(exported.sha256, row.sha256);
    assert_eq!(std::fs::read(destination).unwrap(), before_bytes);
    drop(ro);
    let mut j = UploadJournal::open(root.path(), &scope().account, 1024 * 1024).unwrap();
    let fresh = enqueue(&mut j, &staging);
    assert_ne!(fresh.id, row.id);
    assert_eq!(j.claim_next().unwrap().unwrap().id, fresh.id);
    assert_eq!(j.get(row.id).unwrap().state, UploadState::Resolved);
}
#[test]
fn native_stage_abandonment_rechecks_local_snapshot_and_all_dependency_frontiers() {
    for fault in [
        "row",
        "owner",
        "recovery",
        "successor",
        "prerequisite",
        "destination",
        "working",
        "native-working",
        "resource",
        "native-binding",
        "replacement",
        "publication",
    ] {
        let root = temp();
        let staging = temp();
        let mut j = UploadJournal::open(root.path(), &scope().account, 1024 * 1024).unwrap();
        let row = abandon_conflict(&mut j, &staging);
        let prep = j.prepare_native_stage_abandonment(row.id).unwrap();
        let owner = j.namespace_for_operation(row.id).unwrap().unwrap();
        let recovery = row
            .identity_handoff
            .as_ref()
            .unwrap()
            .unconfirmed_native(&before())
            .unwrap()
            .0;
        let other = Uuid::new_v4().to_string();
        match fault {
            "row" => {
                j.db.execute(
                    "UPDATE uploads SET body=json_set(body,'$.failed_attempts',99) WHERE id=?1",
                    [row.id.to_string()],
                )
                .unwrap();
            }
            "owner" => {
                j.db.execute(
                    "UPDATE namespace_objects SET body=json_set(body,'$.revision',99) WHERE id=?1",
                    [owner.id.to_string()],
                )
                .unwrap();
            }
            "recovery" => {
                j.db.execute(
                    "UPDATE namespace_objects SET body=json_set(body,'$.revision',99) WHERE id=?1",
                    [recovery.to_string()],
                )
                .unwrap();
            }
            "successor" => {
                j.db.execute(
                    "INSERT INTO write_successors VALUES(?1,?2)",
                    params![row.id.to_string(), other],
                )
                .unwrap();
            }
            "prerequisite" => {
                j.db.execute(
                    "INSERT INTO write_prerequisites VALUES(?1,?2)",
                    params![other, row.id.to_string()],
                )
                .unwrap();
            }
            "destination" => {
                j.db.execute("INSERT INTO write_destinations(operation,parent,predecessor,resolved) VALUES(?1,'owned',?2,0)",params![other,row.id.to_string()]).unwrap();
            }
            "working" => {
                j.db.execute(
                    "INSERT INTO working_files VALUES(?1,?1,?1,?2)",
                    params![other, serde_json::json!({"latest":row.id}).to_string()],
                )
                .unwrap();
            }
            "native-binding" => {
                j.db.execute(
                    "INSERT INTO native_working_bindings VALUES(?1,?2,'{}')",
                    params![
                        other,
                        serde_json::to_string(&(&row.scope, &before().id)).unwrap()
                    ],
                )
                .unwrap();
            }
            "replacement" => {
                j.db.execute(
                    "INSERT INTO file_replacements VALUES(?1,?2,?3,?4,'{}')",
                    params![
                        Uuid::new_v4().to_string(),
                        owner.id.to_string(),
                        recovery.to_string(),
                        other
                    ],
                )
                .unwrap();
            }
            "publication" => {
                j.db.execute(
                    "INSERT INTO package_metadata_publication(operation) VALUES(?1)",
                    [row.id.to_string()],
                )
                .unwrap();
            }
            "native-working" => {
                j.db.execute(
                    "INSERT INTO native_working_operations VALUES(?1,?2,?3)",
                    params![row.id.to_string(), other, owner.id.to_string()],
                )
                .unwrap();
            }
            _ => {
                j.db.execute(
                    "INSERT INTO write_queue(id,complete) VALUES(?1,0)",
                    [&other],
                )
                .unwrap();
                j.db.execute("INSERT INTO write_resources SELECT ?1,resource FROM write_resources WHERE id=?2",params![other,row.id.to_string()]).unwrap();
            }
        }
        assert!(
            j.abandon_native_stage(prep, abandon_evidence(&row), &CancellationToken::new())
                .is_err(),
            "{fault}"
        );
        assert_eq!(j.get(row.id).unwrap().state, UploadState::Conflict);
        assert_eq!(
            j.db.query_row(
                "SELECT count(*) FROM native_replacement_abandonments",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert!(j.local_export_source(row.id).is_ok());
    }
}
#[test]
fn native_stage_abandonment_wrong_evidence_and_commit_failure_leave_reservation_intact() {
    let root = temp();
    let staging = temp();
    let mut j = UploadJournal::open(root.path(), &scope().account, 1024 * 1024).unwrap();
    let row = abandon_conflict(&mut j, &staging);
    for fault in ["account", "operation", "payload", "revision"] {
        let prep = j.prepare_native_stage_abandonment(row.id).unwrap();
        let mut record = abandon_evidence(&row).record().clone();
        match fault {
            "account" => record.request.scope.account = "different".into(),
            "operation" => {
                record.operation = Uuid::new_v4();
                record.staged.name = format!("staged-by-cirrove-{}.pages", record.operation);
            }
            "payload" => record.request.sha256 = "b".repeat(64),
            _ => {
                record.original.etag = Some("changed".into());
                if let UploadIntent::Replace { expected_etag, .. } = &mut record.request.intent {
                    *expected_etag = "changed".into()
                };
                if let UploadRepresentation::PackageReplacementArchive { original, .. } =
                    &mut record.request.representation
                {
                    original.etag = Some("changed".into())
                };
            }
        }
        let evidence =
            cirrove_icloud::NativeReplacementAbandonEvidence::synthetic_for_test(record).unwrap();
        assert!(
            j.abandon_native_stage(prep, evidence, &CancellationToken::new())
                .is_err(),
            "{fault}"
        );
    }
    let cancel = CancellationToken::new();
    cancel.cancel();
    let prep = j.prepare_native_stage_abandonment(row.id).unwrap();
    assert!(
        j.abandon_native_stage(prep, abandon_evidence(&row), &cancel)
            .is_err()
    );
    let owner_before = serde_json::to_value(j.namespace_for_operation(row.id).unwrap()).unwrap();
    j.db.execute_batch("CREATE TEMP TRIGGER fail_native_abandon BEFORE UPDATE ON write_queue BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    let prep = j.prepare_native_stage_abandonment(row.id).unwrap();
    assert!(
        j.abandon_native_stage(prep, abandon_evidence(&row), &CancellationToken::new())
            .is_err()
    );
    assert_eq!(j.get(row.id).unwrap().state, UploadState::Conflict);
    assert_eq!(
        serde_json::to_value(j.namespace_for_operation(row.id).unwrap()).unwrap(),
        owner_before
    );
    assert!(j.native_stage_abandonment(row.id).unwrap().is_none());
    // A bare resolved flag must not grant the narrow abandoned-native export.
    j.db.execute_batch("DROP TRIGGER fail_native_abandon;")
        .unwrap();
    j.db.execute(
        "UPDATE uploads SET state='resolved',body=json_set(body,'$.state','resolved') WHERE id=?1",
        [row.id.to_string()],
    )
    .unwrap();
    assert!(j.local_export_source(row.id).is_err());
}

#[test]
fn flat_numbers_handoff_retains_two_ids_and_publication_across_restart() {
    let root = temp().keep();
    let staging = temp().keep();
    let source = staging.join("flat.numbers");
    let bytes = crate::native_import::synthetic_package_archive(
        "Index/Document.iwa",
        b"new Numbers revision",
    );
    std::fs::write(&source, &bytes).unwrap();
    let archive = ValidatedPackageArchive::capture_with_source_layout(
        &source,
        &staging,
        crate::native_import::PackageSourceLayout::FlatNumbers,
        None,
        &CancellationToken::new(),
    )
    .unwrap();
    let mut original = before();
    original.name = "Owned.numbers".into();
    let mut j = UploadJournal::open(&root, &scope().account, 1 << 20).unwrap();
    let row = j
        .enqueue_validated_package_replacement(
            scope(),
            original.clone(),
            original_semantic(),
            archive,
            &CancellationToken::new(),
        )
        .unwrap();
    let UploadRepresentation::FlatNumbersReplacementArchive {
        semantic,
        original: captured_original,
        ..
    } = &row.representation
    else {
        panic!("flat replacement proof missing")
    };
    assert_eq!(captured_original.as_ref(), &original);
    let semantic = semantic.clone();
    // Recovery exposes pending flat replacement bytes without replaying the
    // handoff or changing either namespace identity.
    drop(j);
    let pending_database = std::fs::read(root.join("uploads.db")).unwrap();
    let ro = RecoveryJournal::open(&root, &scope().account).unwrap();
    assert_eq!(
        serde_json::to_value(&ro.list(0, 10).unwrap()[0]).unwrap(),
        serde_json::to_value(&row).unwrap()
    );
    let exported = staging.join("pending-flat.zip");
    let export = ro
        .local_export_source(row.id)
        .unwrap()
        .copy_to(&exported, &CancellationToken::new(), |_| {})
        .unwrap();
    assert_eq!(export.operation, row.id);
    assert_eq!(export.sha256, row.sha256);
    assert_eq!(std::fs::read(exported).unwrap(), bytes);
    drop(ro);
    assert_eq!(
        std::fs::read(root.join("uploads.db")).unwrap(),
        pending_database
    );
    let mut j = UploadJournal::open(&root, &scope().account, 1 << 20).unwrap();
    let claimed = j.claim_next().unwrap().unwrap();
    let attempt = claimed.attempt.unwrap();
    let recovery = j
        .reserve_identity_handoff(
            row.id,
            attempt,
            RecoveryLocation::Trash {
                local_name: "recovery-cirrove.numbers".into(),
                parent: "FOLDER::com.apple.CloudDocs::TRASH_ROOT".into(),
            },
        )
        .unwrap();
    let mut current = original.clone();
    current.id = "FILE::com.apple.CloudDocs::replacement".into();
    current.etag = Some("replacement-v1".into());
    current.size = semantic.expanded_bytes;
    let mut backup = original.clone();
    backup.parent_id = Some("FOLDER::com.apple.CloudDocs::TRASH_ROOT".into());
    backup.etag = Some("trash-v2".into());
    let proof = || PackageHandoffReceipt {
        original: original.clone(),
        current: PackageUploadReceipt {
            remote: current.clone(),
            semantic: semantic.clone(),
        },
        backup: PackageUploadReceipt {
            remote: backup.clone(),
            semantic: original_semantic(),
        },
    };
    for fault in 0..3 {
        let saved = serde_json::to_value(j.get(row.id).unwrap()).unwrap();
        let mut wrong = proof();
        match fault {
            0 => wrong.backup.remote.id = "FILE::com.apple.CloudDocs::foreign".into(),
            1 => wrong.original.etag = Some("changed-original".into()),
            _ => wrong.current.semantic.sha256 = "b".repeat(64),
        }
        assert!(
            j.acknowledge_package_handoff(row.id, attempt, wrong)
                .is_err()
        );
        assert_eq!(serde_json::to_value(j.get(row.id).unwrap()).unwrap(), saved);
        assert_eq!(
            std::fs::read(j.objects.join(row.id.to_string())).unwrap(),
            bytes
        );
    }
    j.acknowledge_package_handoff(row.id, attempt, proof())
        .unwrap();
    let completed = j.get(row.id).unwrap();
    assert_eq!(completed.state, UploadState::Uploaded);
    assert_eq!(
        j.namespace_for_operation(row.id).unwrap().unwrap().remote,
        Some(current.clone())
    );
    assert_eq!(
        j.namespace_object(recovery).unwrap().remote,
        Some(backup.clone())
    );
    assert_eq!(
        j.native_replacement_list(&scope(), None, 10)
            .unwrap()
            .operations
            .len(),
        1
    );
    assert_eq!(
        j.package_publication_due(now_seconds())
            .unwrap()
            .unwrap()
            .id,
        row.id
    );
    j.finish_package_publication(
        &completed,
        PackagePublicationStatus::Present(current),
        now_seconds(),
    )
    .unwrap();
    drop(j);
    let ro = RecoveryJournal::open(&root, &scope().account).unwrap();
    assert!(matches!(
        ro.local_export_source(row.id),
        Err(JournalError::Stale)
    ));
    drop(ro);
    let j = UploadJournal::open(&root, &scope().account, 1 << 20).unwrap();
    assert_eq!(
        serde_json::to_value(j.get(row.id).unwrap()).unwrap(),
        serde_json::to_value(completed).unwrap()
    );
    assert_eq!(j.namespace_object(recovery).unwrap().remote, Some(backup));
    assert!(j.package_publication_due(now_seconds()).unwrap().is_none());
    assert_eq!(std::fs::read(source).unwrap(), bytes);
}
