//! Local journal authority controls. No TLS or child-exit acceptance claim.
use super::*;
use cirrove_core::{Node, NodeKind};
use std::sync::atomic::AtomicUsize;

struct Provider {
    journal: Arc<Mutex<UploadJournal>>,
    calls: AtomicUsize,
    prepare: Option<String>,
    receipt: MutationReceipt,
    uncommitted: bool,
}
impl Provider {
    fn called_without_owner(&self) {
        assert!(
            self.journal.try_lock().is_ok(),
            "journal owner retained across provider await"
        );
        self.calls.fetch_add(1, Ordering::SeqCst);
    }
}
#[async_trait::async_trait]
impl MutationProvider for Provider {
    async fn mutate(
        &self,
        _: &MutationRequest,
        _: &CancellationToken,
    ) -> cirrove_core::mutation::Result<MutationReceipt> {
        panic!("unscoped provider mutation")
    }
    async fn reconcile_mutation(
        &self,
        _: &MutationRequest,
        _: &CancellationToken,
    ) -> cirrove_core::mutation::Result<MutationReconciliation> {
        panic!("unscoped provider reconciliation")
    }
    async fn prepare_mutation_for_operation(
        &self,
        _: &str,
        _: &MutationRequest,
        _: &CancellationToken,
    ) -> cirrove_core::mutation::Result<Option<String>> {
        self.called_without_owner();
        Ok(self.prepare.clone())
    }
    async fn mutate_operation(
        &self,
        _: &str,
        _: &MutationRequest,
        _: Option<&str>,
        _: &CancellationToken,
    ) -> cirrove_core::mutation::Result<MutationReceipt> {
        self.called_without_owner();
        Ok(self.receipt.clone())
    }
    async fn reconcile_operation(
        &self,
        _: &str,
        _: &MutationRequest,
        _: Option<&str>,
        _: &CancellationToken,
    ) -> cirrove_core::mutation::Result<MutationReconciliation> {
        self.called_without_owner();
        Ok(if self.uncommitted {
            MutationReconciliation::Uncommitted
        } else {
            MutationReconciliation::Applied(self.receipt.clone())
        })
    }
}
struct Fixture {
    _root: tempfile::TempDir,
    directory: PathBuf,
    db: PathBuf,
    journal: Arc<Mutex<UploadJournal>>,
    row: MutationRecord,
    request: MutationRequest,
}
impl Fixture {
    fn new(recover: bool) -> anyhow::Result<Self> {
        let root = tempfile::tempdir()?;
        let directory = root.path().join("evidence");
        std::fs::create_dir(&directory)?;
        let journal_path = root.path().join("journal");
        let request = MutationRequest {
            scope: Scope {
                account: Uuid::new_v4().to_string(),
                provider: "icloud".into(),
                collection: "drive".into(),
            },
            intent: MutationIntent::TrashNativeDocument {
                before: Node {
                    id: "FILE::com.apple.CloudDocs::owned-document".into(),
                    parent_id: Some("FOLDER::com.apple.CloudDocs::owned-parent".into()),
                    name: "Owned.numbers".into(),
                    kind: NodeKind::Folder,
                    package: true,
                    size: 17,
                    modified_unix: 1,
                    etag: Some("owned-E1".into()),
                    content_version: Some("owned-E1".into()),
                    target: None,
                },
            },
        };
        let mut journal = UploadJournal::open(&journal_path, &request.scope.account, 1024 * 1024)?;
        let queued = journal.enqueue_mutation(request.clone())?;
        let mut row = journal
            .claim_mutation()?
            .ok_or_else(|| anyhow::anyhow!("claim absent"))?;
        assert_eq!(queued.id, row.id);
        if recover {
            journal.record_prepared_mutation(
                row.id,
                row.attempt
                    .ok_or_else(|| anyhow::anyhow!("attempt absent"))?,
                request
                    .intent
                    .before()
                    .ok_or_else(|| anyhow::anyhow!("original absent"))?
                    .id
                    .clone(),
            )?;
            journal.defer_mutation(
                row.id,
                row.attempt
                    .ok_or_else(|| anyhow::anyhow!("attempt absent"))?,
                MutationState::VerifyRequired,
                std::time::Duration::ZERO,
            )?;
            row = journal
                .claim_mutation()?
                .ok_or_else(|| anyhow::anyhow!("recovery claim absent"))?;
            assert_eq!(row.state, MutationState::Verifying);
        }
        Ok(Self {
            _root: root,
            directory,
            db: journal_path.join("uploads.db"),
            journal: Arc::new(Mutex::new(journal)),
            row,
            request,
        })
    }
    fn item(&self) -> &str {
        self.request
            .intent
            .before()
            .map(|n| n.id.as_str())
            .unwrap_or_default()
    }
    fn receipt(&self) -> MutationReceipt {
        MutationReceipt::Removed {
            item: self.item().into(),
        }
    }
    fn guard(
        &self,
        recover: bool,
        receipt: MutationReceipt,
        uncommitted: bool,
    ) -> cirrove_core::mutation::Result<(Guard, Arc<Provider>)> {
        let provider = Arc::new(Provider {
            journal: self.journal.clone(),
            calls: AtomicUsize::new(0),
            prepare: Some(self.item().into()),
            receipt,
            uncommitted,
        });
        let mode = if recover {
            Mode::Recover {
                operation: self.row.id,
                receipt: self.receipt(),
            }
        } else {
            Mode::Lose
        };
        let mut guard = Guard::new(
            provider.clone(),
            self.request.clone(),
            self.journal.clone(),
            vec![],
            vec![],
            self.directory.clone(),
            Uuid::new_v4(),
            mode,
        )?;
        guard.hold_instead_of_exit = true;
        Ok((guard, provider))
    }
    fn prepared(&self) -> anyhow::Result<()> {
        self.journal
            .lock()
            .map_err(|_| anyhow::anyhow!("owner poisoned"))?
            .record_prepared_mutation(
                self.row.id,
                self.row
                    .attempt
                    .ok_or_else(|| anyhow::anyhow!("attempt absent"))?,
                self.item().into(),
            )?;
        Ok(())
    }
}

#[tokio::test]
async fn native_trash_guard_exact_preparation_and_receipt_retains_unacknowledged_row()
-> anyhow::Result<()> {
    let f = Fixture::new(false)?;
    let (guard, provider) = f.guard(false, f.receipt(), false)?;
    let cancel = CancellationToken::new();
    let operation = f.row.id.to_string();
    assert_eq!(
        guard
            .prepare_mutation_for_operation(&operation, &f.request, &cancel)
            .await?,
        Some(f.item().into())
    );
    f.prepared()?;
    assert!(matches!(
        guard
            .mutate_operation(&operation, &f.request, Some(f.item()), &cancel)
            .await,
        Err(MutationError::Uncertain)
    ));
    let marker: LossMarker = serde_json::from_slice(&std::fs::read(
        f.directory.join("lost-native-trash-confirmation.json"),
    )?)?;
    assert_eq!(marker.version, 1);
    assert_eq!(marker.operation, f.row.id);
    assert_eq!(marker.pid, std::process::id());
    assert!(!marker.acknowledgement_returned);
    assert!(marker.request == f.request && marker.receipt == f.receipt());
    assert_eq!(marker.prepared_item, f.item());
    let row = f
        .journal
        .lock()
        .map_err(|_| anyhow::anyhow!("owner"))?
        .native_trash_record(f.row.id)?;
    assert_eq!(row.state, MutationState::Applying);
    assert!(row.attempt.is_some() && row.receipt.is_none());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert!(
        guard
            .mutate_operation(&operation, &f.request, Some(f.item()), &cancel)
            .await
            .is_err()
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert_eq!(guard.counts.value()["mutations"], 1);
    Ok(())
}

#[tokio::test]
async fn native_trash_guard_foreign_scope_uuid_node_prepared_and_queue_refuse_before_delegate()
-> anyhow::Result<()> {
    for arm in 0..6 {
        let f = Fixture::new(false)?;
        f.prepared()?;
        let (guard, provider) = f.guard(false, f.receipt(), false)?;
        let mut request = f.request.clone();
        let mut operation = f.row.id.to_string();
        let mut prepared = f.item().to_owned();
        match arm {
            0 => request.scope.account = Uuid::new_v4().to_string(),
            1 => operation = Uuid::new_v4().to_string(),
            2 => {
                if let MutationIntent::TrashNativeDocument { before } = &mut request.intent {
                    before.etag = Some("changed-E2".into());
                }
            }
            3 => prepared = "FILE::com.apple.CloudDocs::foreign".into(),
            4 => {
                rusqlite::Connection::open(&f.db)?.execute(
                    "UPDATE write_queue SET complete=1 WHERE id=?1",
                    [f.row.id.to_string()],
                )?;
            }
            5 => request.scope.collection = "foreign".into(),
            _ => unreachable!(),
        }
        assert!(
            guard
                .mutate_operation(
                    &operation,
                    &request,
                    Some(&prepared),
                    &CancellationToken::new()
                )
                .await
                .is_err(),
            "authority arm {arm}"
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        assert!(
            !f.directory
                .join("lost-native-trash-confirmation.json")
                .exists()
        );
    }
    Ok(())
}

#[tokio::test]
async fn native_trash_guard_wrong_receipt_never_persists_loss_marker() -> anyhow::Result<()> {
    let f = Fixture::new(false)?;
    f.prepared()?;
    let (guard, provider) = f.guard(
        false,
        MutationReceipt::Removed {
            item: "foreign".into(),
        },
        false,
    )?;
    assert!(
        guard
            .mutate_operation(
                &f.row.id.to_string(),
                &f.request,
                Some(f.item()),
                &CancellationToken::new()
            )
            .await
            .is_err()
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert!(
        !f.directory
            .join("lost-native-trash-confirmation.json")
            .exists()
    );
    Ok(())
}

#[tokio::test]
async fn native_trash_guard_recovery_exact_receipt_and_uncommitted_cannot_replay()
-> anyhow::Result<()> {
    for uncommitted in [false, true] {
        let f = Fixture::new(true)?;
        let (guard, provider) = f.guard(true, f.receipt(), uncommitted)?;
        let before = serde_json::to_vec(
            &f.journal
                .lock()
                .map_err(|_| anyhow::anyhow!("owner"))?
                .native_trash_record(f.row.id)?,
        )?;
        let outcome = guard
            .reconcile_operation(
                &f.row.id.to_string(),
                &f.request,
                Some(f.item()),
                &CancellationToken::new(),
            )
            .await?;
        assert!(if uncommitted {
            matches!(outcome, MutationReconciliation::Indeterminate)
        } else {
            matches!(outcome,MutationReconciliation::Applied(r) if r==f.receipt())
        });
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            before,
            serde_json::to_vec(
                &f.journal
                    .lock()
                    .map_err(|_| anyhow::anyhow!("owner"))?
                    .native_trash_record(f.row.id)?
            )?
        );
        assert!(
            guard
                .prepare_mutation_for_operation(
                    &f.row.id.to_string(),
                    &f.request,
                    &CancellationToken::new()
                )
                .await
                .is_err()
        );
        assert!(
            guard
                .mutate_operation(
                    &f.row.id.to_string(),
                    &f.request,
                    Some(f.item()),
                    &CancellationToken::new()
                )
                .await
                .is_err()
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    }
    let f = Fixture::new(true)?;
    let (guard, _) = f.guard(
        true,
        MutationReceipt::Removed {
            item: "foreign".into(),
        },
        false,
    )?;
    assert!(
        guard
            .reconcile_operation(
                &f.row.id.to_string(),
                &f.request,
                Some(f.item()),
                &CancellationToken::new()
            )
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn native_trash_guard_unscoped_and_permanent_mutations_and_all_upload_hooks_refuse()
-> anyhow::Result<()> {
    let f = Fixture::new(true)?;
    let (guard, provider) = f.guard(true, f.receipt(), false)?;
    let c = CancellationToken::new();
    assert!(
        guard
            .delete_permanently(&f.request.scope, f.item(), Some("owned-E1"), &c)
            .await
            .is_err()
    );
    assert!(guard.prepare_mutation(&f.request, &c).await.is_err());
    assert!(guard.mutate(&f.request, &c).await.is_err());
    assert!(
        guard
            .mutate_prepared(&f.request, Some(f.item()), &c)
            .await
            .is_err()
    );
    assert!(guard.reconcile_mutation(&f.request, &c).await.is_err());
    assert!(
        guard
            .reconcile_prepared_mutation(&f.request, Some(f.item()), &c)
            .await
            .is_err()
    );
    let request = UploadRequest {
        scope: f.request.scope.clone(),
        intent: cirrove_core::upload::UploadIntent::Create {
            parent: "FOLDER::com.apple.CloudDocs::owned-parent".into(),
            name: "unwanted.txt".into(),
        },
        representation: cirrove_core::upload::UploadRepresentation::FileBytes,
        size: 1,
        sha256: "a".repeat(64),
    };
    let checkpoint = SecretString::new("opaque synthetic checkpoint".into());
    let operation = Uuid::new_v4().to_string();
    assert!(
        guard
            .begin_upload_for_operation(&operation, &request, &c)
            .await
            .is_err()
    );
    assert!(
        guard
            .begin_upload_from_payload_for_operation(&operation, &request, File::open(&f.db)?, &c)
            .await
            .is_err()
    );
    assert!(
        guard
            .allocate_upload_for_operation(&operation, &request, &checkpoint, &c)
            .await
            .is_err()
    );
    assert!(
        guard
            .inspect_upload_for_operation(&operation, &request, &checkpoint, &c)
            .await
            .is_err()
    );
    assert!(
        guard
            .upload_part_for_operation(&operation, &request, &checkpoint, 0, vec![1], &c)
            .await
            .is_err()
    );
    assert!(
        guard
            .upload_stream_for_operation(&operation, &request, &checkpoint, File::open(&f.db)?, &c)
            .await
            .is_err()
    );
    assert!(
        guard
            .commit_upload_for_operation(&operation, &request, &checkpoint, &c)
            .await
            .is_err()
    );
    assert!(
        guard
            .reconcile_upload_for_operation(&operation, &request, Some(&checkpoint), &c)
            .await
            .is_err()
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert_eq!(guard.counts.value()["refused_uploads"], 8);
    Ok(())
}
