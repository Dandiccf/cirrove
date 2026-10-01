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
    assert_eq!(version, 17);
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
    assert_eq!(version, 17);
    assert!(version > 16);
    assert_eq!(
        std::fs::read(j.objects.join(old.id.to_string())).unwrap(),
        b"accepted ordinary bytes"
    );
    assert!(j.get(old.id).unwrap().representation.is_file_bytes());
}
