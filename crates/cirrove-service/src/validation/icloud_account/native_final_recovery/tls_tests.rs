#![allow(clippy::unwrap_used)]
//! Child of the existing HTTPS fixture: no second server or receipt generator.
use super::*;
use crate::{
    accounts::Account,
    engine::Engine,
    manager::WriteContext,
    validation::native_final_recovery::{Guard, LossMarker, Mode},
};
use cirrove_auth::{AccessMode, AppRegistration, Identity};
use cirrove_core::{
    ChangePage, Cursor, DirectoryPage, MetadataProvider, ProviderError, ReadProvider,
    upload::UploadProvider,
};
use secrecy::SecretString;
use std::sync::atomic::Ordering;
struct Metadata {
    original: Node,
    state: Mutex<Option<Arc<Mutex<State>>>>,
}
#[async_trait::async_trait]
impl MetadataProvider for Metadata {
    fn provider_id(&self) -> &'static str {
        "icloud"
    }
    async fn changes(
        &self,
        _: &Scope,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<ChangePage, ProviderError> {
        Err(ProviderError::Unavailable)
    }
}
#[async_trait::async_trait]
impl ReadProvider for Metadata {
    async fn node(
        &self,
        _: &Scope,
        id: &str,
        _: &CancellationToken,
    ) -> std::result::Result<Node, ProviderError> {
        if id == FOLDER {
            Ok(parent())
        } else if id == NEW
            && self
                .state
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|state| state.lock().unwrap().installed)
        {
            Ok(Node {
                id: NEW.into(),
                etag: Some("new-v2".into()),
                ..self.original.clone()
            })
        } else if id == OLD
            && !self
                .state
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|state| state.lock().unwrap().trashed)
        {
            Ok(self.original.clone())
        } else {
            Err(ProviderError::NotFound)
        }
    }
    async fn children(
        &self,
        _: &Scope,
        _: &str,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<DirectoryPage, ProviderError> {
        Ok(DirectoryPage {
            nodes: vec![],
            next: None,
        })
    }
    async fn read_range(
        &self,
        _: &Scope,
        _: &Node,
        _: u64,
        _: u32,
        _: &CancellationToken,
    ) -> std::result::Result<Vec<u8>, ProviderError> {
        Err(ProviderError::Unavailable)
    }
}
struct Fixture {
    state: tempfile::TempDir,
    directory: tempfile::TempDir,
    _staging: tempfile::TempDir,
    engine: Arc<Engine>,
    context: WriteContext,
    account: Account,
    row: UploadRecord,
    server: Server,
    keys: Arc<WrappingKeys>,
    completed: Vec<UploadRecord>,
    folder: Option<Uuid>,
}
impl Fixture {
    async fn new() -> Self {
        Self::new_seeded(false).await
    }
    async fn new_seeded(seed: bool) -> Self {
        Self::new_layout(seed, false).await
    }
    async fn new_layout(seed: bool, flat: bool) -> Self {
        let state = directory();
        let owned = directory();
        let staging = directory();
        let mount = owned.path().join("mount");
        std::fs::create_dir(&mount).unwrap();
        let account = Account {
            id: Uuid::new_v4().to_string(),
            label: "synthetic native final boundary".into(),
            registration: AppRegistration::ICloud,
            identity: Identity {
                tenant_id: "fixture".into(),
                subject: "fixture".into(),
                username: "fixture@example.invalid".into(),
                graph_user_id: "fixture".into(),
                display_name: "fixture".into(),
            },
            credential_id: Uuid::new_v4().to_string(),
            access: AccessMode::ReadWrite,
            drive: cirrove_core::CollectionInfo {
                id: "drive".into(),
                name: "Fixture".into(),
                drive_type: "icloud_drive".into(),
                web_url: "https://example.invalid".into(),
            },
            root_id: ROOT_ID.into(),
            mount_path: mount,
            enabled: false,
            poll_seconds: 3600,
            cache_bytes: 8 * 1024 * 1024,
        };
        let source_root = if seed {
            "Source.numbers"
        } else {
            "Source.pages"
        };
        let target_name = if seed {
            "Target.numbers"
        } else {
            "Target.pages"
        };
        let mut old = original();
        old.name = target_name.into();
        let bytes = if flat {
            crate::native_import::synthetic_package_archive("Document", b"new owned content")
        } else {
            archive(source_root, false, false)
        };
        let source = staging.path().join("source.zip");
        std::fs::write(&source, &bytes).unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600)).unwrap();
        // Allocate a normal journal operation, then the unchanged server binds
        // its stage name to that actual UUID before any worker starts.
        let metadata = Arc::new(Metadata {
            original: old.clone(),
            state: Mutex::new(None),
        });
        let engine = Engine::new(account.clone(), metadata.clone(), state.path().to_owned())
            .await
            .unwrap();
        let context = WriteContext::open(&engine, state.path()).await.unwrap();
        let scope = Scope {
            account: account.id.clone(),
            provider: "icloud".into(),
            collection: "drive".into(),
        };
        cirrove_store::Store::open(&engine.db)
            .unwrap()
            .observe_directory(&scope, ROOT_ID, &[parent()])
            .unwrap();
        let mut completed = vec![];
        let mut folder = None;
        if seed {
            use cirrove_core::{
                mutation::MutationReceipt,
                upload::{PackageUploadReceipt, UploadIntent},
            };
            let mut owned_parent = parent();
            owned_parent.etag = Some("folder-v1".into());
            let journal = context.journal();
            let mut j = journal.lock().unwrap();
            let local = j
                .create_namespace_directory(scope.clone(), ROOT_ID.into(), parent().name)
                .unwrap();
            let mutation = j.claim_mutation().unwrap().unwrap();
            folder = Some(mutation.id);
            j.acknowledge_mutation(
                mutation.id,
                mutation.attempt.unwrap(),
                MutationReceipt::Upsert(owned_parent.clone()),
            )
            .unwrap();
            let local = j.namespace_object(local.id).unwrap();
            j.handoff_namespace(local.id, local.revision, owned_parent)
                .unwrap();
            let a_path = staging.path().join("source-a.zip");
            std::fs::write(
                &a_path,
                if flat {
                    crate::native_import::synthetic_package_archive(
                        "Document",
                        b"old owned content",
                    )
                } else {
                    archive(source_root, true, false)
                },
            )
            .unwrap();
            std::fs::set_permissions(&a_path, std::fs::Permissions::from_mode(0o600)).unwrap();
            let a = ValidatedPackageArchive::capture_with_source_layout(
                &a_path,
                staging.path(),
                if flat {
                    cirrove_core::upload::PackageSourceLayout::FlatNumbers
                } else {
                    cirrove_core::upload::PackageSourceLayout::Wrapped
                },
                if flat { None } else { Some(source_root) },
                &CancellationToken::new(),
            )
            .unwrap();
            let imported = j
                .enqueue_validated_package_archive(
                    scope.clone(),
                    UploadIntent::Create {
                        parent: FOLDER.into(),
                        name: target_name.into(),
                    },
                    a,
                    &CancellationToken::new(),
                )
                .unwrap();
            let claimed = j.claim_next().unwrap().unwrap();
            j.acknowledge_package(
                claimed.id,
                claimed.attempt.unwrap(),
                PackageUploadReceipt {
                    remote: old.clone(),
                    semantic: semantic(source_root, true, 2),
                },
            )
            .unwrap();
            let imported = j.get(imported.id).unwrap();
            j.finish_package_publication(
                &imported,
                PackagePublicationStatus::Present(old.clone()),
                0,
            )
            .unwrap();
            completed.push(imported);
            drop(j);
            drop(journal);
        }
        drop(context);
        if seed && !flat {
            let path = state
                .path()
                .join("accounts")
                .join(&account.id)
                .join("journal");
            let read = RecoveryJournal::open(&path, &account.id).unwrap();
            crate::validation::native_final_recovery::registration::test_frontier(
                &read,
                &account.id,
                folder.unwrap(),
                &completed[0],
                None,
                None,
            )
            .unwrap();
            drop(read);
        }
        let context = WriteContext::open(&engine, state.path()).await.unwrap();
        let captured = ValidatedPackageArchive::capture_with_source_layout(
            &source,
            staging.path(),
            if flat {
                cirrove_core::upload::PackageSourceLayout::FlatNumbers
            } else {
                cirrove_core::upload::PackageSourceLayout::Wrapped
            },
            if flat { None } else { Some(source_root) },
            &CancellationToken::new(),
        )
        .unwrap();
        let row = context
            .journal()
            .lock()
            .unwrap()
            .enqueue_validated_package_replacement(
                scope,
                old,
                semantic(target_name, true, 2),
                captured,
                &CancellationToken::new(),
            )
            .unwrap();
        let server = Server::start(
            Plan {
                target_name: target_name.into(),
                staged_name: format!(
                    "staged-by-cirrove-{}.{}",
                    row.id,
                    if seed { "numbers" } else { "pages" }
                ),
            },
            bytes,
            false,
            false,
            false,
            None,
        )
        .await;
        *metadata.state.lock().unwrap() = Some(server.state.clone());
        // The metadata fixture is not used by the native handoff: all package
        // identity/content verification happens through this real TLS server.
        let keys = Arc::new(WrappingKeys::default());
        Self {
            state,
            directory: owned,
            _staging: staging,
            engine,
            context,
            account,
            row,
            server,
            keys,
            completed,
            folder,
        }
    }
    fn router(&self) -> crate::icloud_writes::ICloudWriteProvider {
        crate::icloud_writes::ICloudWriteProvider::new(&self.account, &self.context)
            .unwrap()
            .synthetic_native_transport(self.server.client.clone())
    }
    fn vault(&self) -> Arc<SealedUploadCheckpointVault> {
        Arc::new(
            SealedUploadCheckpointVault::with_test_key_vault(
                self.state.path(),
                &self.account.id,
                self.keys.clone(),
            )
            .unwrap(),
        )
    }
    fn loss(&self) -> Guard {
        let mut guard = Guard::new(
            self.router(),
            request(&self.row),
            self.context.journal(),
            self.completed.clone(),
            self.directory.path().to_owned(),
            Uuid::new_v4(),
            Mode::Lose,
        )
        .unwrap();
        guard.hold_instead_of_exit = true;
        guard
    }
    fn marker(&self) -> LossMarker {
        serde_json::from_slice(
            &std::fs::read(self.directory.path().join("lost-final-confirmation.json")).unwrap(),
        )
        .unwrap()
    }
    fn recover(&self, marker: &LossMarker) -> Guard {
        Guard::new(
            self.router(),
            request(&self.row),
            self.context.journal(),
            self.completed.clone(),
            self.directory.path().to_owned(),
            marker.run,
            Mode::Recover {
                operation: self.row.id,
                checkpoint_sha256: marker.checkpoint_sha256.clone(),
                receipt: Box::new(marker.receipt.clone()),
            },
        )
        .unwrap()
    }
    async fn withheld(&self) -> LossMarker {
        let guard = Arc::new(self.loss());
        let worker = TransferWorker::new(
            self.context.journal(),
            guard,
            self.vault(),
            CancellationToken::new(),
        );
        let result = tokio::time::timeout(Duration::from_secs(30), worker.run_once())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(result.id, self.row.id);
        assert_eq!(result.state, UploadState::VerifyRequired);
        assert_eq!(counts(&self.server), (1, 1, 1, 1, 1));
        assert!(self.server.state.lock().unwrap().installed);
        let marker = self.marker();
        assert!(!marker.acknowledgement_returned);
        assert_eq!(marker.operation, self.row.id);
        assert!(marker.request == request(&self.row));
        let saved = self
            .vault()
            .load(&format!("upload/{}", self.row.id))
            .await
            .unwrap()
            .unwrap();
        let typed = self
            .router()
            .native_package_adapter(&self.row.id.to_string(), &request(&self.row), Some(&saved))
            .await
            .unwrap();
        assert_eq!(
            typed
                .native_checkpoint_diagnostic(&self.row.id.to_string(), &request(&self.row), &saved)
                .unwrap()["phase"],
            "handoff-install-armed"
        );
        let row = self
            .context
            .journal()
            .lock()
            .unwrap()
            .get(self.row.id)
            .unwrap();
        assert!(row.attempt.is_none() && row.remote.is_none() && row.package_completion.is_none());
        assert_eq!(row.session_key, Some(self.row.id));
        assert_eq!(row.transferred_bytes, row.size);
        assert!(row.native_replacement_receipt().is_none());
        let journal = self.context.journal();
        assert_eq!(
            journal
                .lock()
                .unwrap()
                .db
                .query_row(
                    "SELECT count(*) FROM package_metadata_publication WHERE operation=?1",
                    [self.row.id.to_string()],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert_eq!(
            std::fs::read(
                journal
                    .lock()
                    .unwrap()
                    .objects
                    .join(self.row.id.to_string())
            )
            .unwrap(),
            archive(
                if self.folder.is_some() {
                    "Source.numbers"
                } else {
                    "Source.pages"
                },
                false,
                false
            )
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
        marker
    }
}
#[tokio::test]
async fn native_final_real_tls_receipt_is_withheld_then_same_checkpoint_recovers_without_mutation()
{
    let fixture = Fixture::new().await;
    let marker = fixture.withheld().await;
    let before = counts(&fixture.server);
    let guard = Arc::new(fixture.recover(&marker));
    let worker = TransferWorker::new(
        fixture.context.journal(),
        guard.clone(),
        fixture.vault(),
        CancellationToken::new(),
    );
    let result = tokio::time::timeout(Duration::from_secs(30), worker.run_once())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result.id, fixture.row.id);
    assert_eq!(result.state, UploadState::Uploaded);
    assert_eq!(counts(&fixture.server), before);
    assert_eq!(guard.counts.refused_uploads.load(Ordering::SeqCst), 0);
    assert_eq!(guard.counts.inspections.load(Ordering::SeqCst), 1);
    let journal = fixture.context.journal();
    let row = journal.lock().unwrap().get(fixture.row.id).unwrap();
    let (old, current, backup) = row.native_replacement_receipt().unwrap();
    assert_eq!(old, &marker.receipt.original);
    assert_eq!(current, &marker.receipt.current);
    assert_eq!(backup, &marker.receipt.backup);
    assert_eq!(
        row.package_completion,
        Some(marker.receipt.current_semantic)
    );
    assert!(
        fixture
            .vault()
            .load(&format!("upload/{}", fixture.row.id))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        journal
            .lock()
            .unwrap()
            .db
            .query_row(
                "SELECT count(*) FROM package_metadata_publication WHERE operation=?1 AND done=0",
                [fixture.row.id.to_string()],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    // Exercise the unchanged unmounted metadata publication handler.
    let fs = crate::filesystem::CloudFs::new_experimental_writable(
        fixture.engine.clone(),
        journal.clone(),
    )
    .await
    .unwrap();
    let control = fs.write_control().unwrap();
    assert!(control.publish_completed_package().await.unwrap());
    assert!(
        matches!(journal.lock().unwrap().package_publication_status(fixture.row.id).unwrap(),crate::journal::PackagePublicationStatus::Present(node) if node==marker.receipt.current)
    );
    assert_eq!(counts(&fixture.server), before);
    drop(control);
    drop(fs);
    drop(worker);
    drop(guard);
    drop(journal);
    let path = fixture
        .state
        .path()
        .join("accounts")
        .join(&fixture.account.id)
        .join("journal");
    let id = fixture.row.id;
    let account = fixture.account.id.clone();
    drop(fixture.context);
    let read = RecoveryJournal::open(&path, &account).unwrap();
    let row = read.native_validation_upload(id).unwrap();
    assert_eq!(row.state, UploadState::Uploaded);
    assert!(row.native_replacement_receipt().is_some());
}
#[tokio::test]
async fn native_final_recovery_vetoes_all_upload_and_namespace_mutation_entrypoints_before_http() {
    use cirrove_core::mutation::{MutationIntent, MutationProvider, MutationRequest};
    let fixture = Fixture::new().await;
    let marker = fixture.withheld().await;
    let guard = fixture.recover(&marker);
    let request = request(&fixture.row);
    let saved = fixture
        .vault()
        .load(&format!("upload/{}", fixture.row.id))
        .await
        .unwrap()
        .unwrap();
    let cancel = CancellationToken::new();
    let op = fixture.row.id.to_string();
    let requests = fixture.server.state.lock().unwrap().requests;
    assert!(
        fixture
            .context
            .journal()
            .lock()
            .unwrap()
            .claim_next_verification()
            .unwrap()
            .is_some()
    );
    assert!(guard.begin_upload(&request, &cancel).await.is_err());
    assert!(
        guard
            .begin_upload_for_operation(&op, &request, &cancel)
            .await
            .is_err()
    );
    assert!(
        guard
            .allocate_upload_for_operation(&op, &request, &saved, &cancel)
            .await
            .is_err()
    );
    assert!(
        guard
            .upload_part(&request, &saved, 0, vec![], &cancel)
            .await
            .is_err()
    );
    assert!(
        guard
            .upload_part_for_operation(&op, &request, &saved, 0, vec![], &cancel)
            .await
            .is_err()
    );
    assert!(
        guard
            .upload_stream(&request, &saved, tempfile::tempfile().unwrap(), &cancel)
            .await
            .is_err()
    );
    assert!(
        guard
            .upload_stream_for_operation(
                &op,
                &request,
                &saved,
                tempfile::tempfile().unwrap(),
                &cancel
            )
            .await
            .is_err()
    );
    assert!(
        guard
            .commit_upload(&request, &saved, &cancel)
            .await
            .is_err()
    );
    assert!(
        guard
            .commit_upload_for_operation(&op, &request, &saved, &cancel)
            .await
            .is_err()
    );
    assert_eq!(guard.counts.refused_uploads.load(Ordering::SeqCst), 9);
    let mutation = MutationRequest {
        scope: request.scope.clone(),
        intent: MutationIntent::CreateFolder {
            parent: FOLDER.into(),
            name: "not-permitted".into(),
        },
    };
    assert!(
        guard
            .delete_permanently(&request.scope, OLD, Some("old-v1"), &cancel)
            .await
            .is_err()
    );
    assert!(guard.prepare_mutation(&mutation, &cancel).await.is_err());
    assert!(
        guard
            .prepare_mutation_for_operation(&op, &mutation, &cancel)
            .await
            .is_err()
    );
    assert!(guard.mutate(&mutation, &cancel).await.is_err());
    assert!(
        guard
            .mutate_prepared(&mutation, None, &cancel)
            .await
            .is_err()
    );
    assert!(
        guard
            .mutate_operation(&op, &mutation, None, &cancel)
            .await
            .is_err()
    );
    assert!(guard.reconcile_mutation(&mutation, &cancel).await.is_err());
    assert!(
        guard
            .reconcile_prepared_mutation(&mutation, None, &cancel)
            .await
            .is_err()
    );
    assert!(
        guard
            .reconcile_operation(&op, &mutation, None, &cancel)
            .await
            .is_err()
    );
    assert_eq!(guard.counts.refused_namespace.load(Ordering::SeqCst), 9);
    assert_eq!(fixture.server.state.lock().unwrap().requests, requests);
}
#[tokio::test]
async fn native_final_recovery_refuses_foreign_scope_operation_checkpoint_and_receipt_before_ack() {
    let fixture = Fixture::new().await;
    let marker = fixture.withheld().await;
    let guard = fixture.recover(&marker);
    let saved = fixture
        .vault()
        .load(&format!("upload/{}", fixture.row.id))
        .await
        .unwrap()
        .unwrap();
    let cancel = CancellationToken::new();
    let requests = fixture.server.state.lock().unwrap().requests;
    {
        let journal = fixture.context.journal();
        let mut j = journal.lock().unwrap();
        assert!(j.claim_next_verification().unwrap().is_some());
    }
    let mut foreign = request(&fixture.row);
    foreign.scope.account = Uuid::new_v4().to_string();
    assert!(
        guard
            .inspect_upload_for_operation(&fixture.row.id.to_string(), &foreign, &saved, &cancel)
            .await
            .is_err()
    );
    assert!(
        guard
            .inspect_upload_for_operation(
                &Uuid::new_v4().to_string(),
                &request(&fixture.row),
                &saved,
                &cancel
            )
            .await
            .is_err()
    );
    assert!(
        guard
            .inspect_upload_for_operation(
                &fixture.row.id.to_string(),
                &request(&fixture.row),
                &SecretString::from("{}"),
                &cancel
            )
            .await
            .is_err()
    );
    assert_eq!(fixture.server.state.lock().unwrap().requests, requests);
    let mut tampered = marker.clone();
    tampered.receipt.backup.etag = Some("wrong-backup".into());
    let guard = fixture.recover(&tampered);
    assert!(
        guard
            .inspect_upload_for_operation(
                &fixture.row.id.to_string(),
                &request(&fixture.row),
                &saved,
                &cancel
            )
            .await
            .is_err()
    );
    assert_eq!(counts(&fixture.server), (1, 1, 1, 1, 1));
    assert!(
        fixture
            .context
            .journal()
            .lock()
            .unwrap()
            .get(fixture.row.id)
            .unwrap()
            .remote
            .is_none()
    );
}

#[tokio::test]
async fn native_final_actual_receipt_marker_is_fsynced_before_owned_process_exit() {
    // Re-enter this one actual test as an exact owned child. The parent already
    // obtained the typed marker from the real HTTPS coordinator; no receipt is
    // invented and the child is never a cloud/worker producer.
    if let Some(path) = std::env::var_os("CIRROVE_SYNTHETIC_FINAL_MARKER") {
        let mut marker: LossMarker = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        marker.pid = std::process::id();
        let destination = std::env::var_os("CIRROVE_SYNTHETIC_FINAL_DESTINATION").unwrap();
        crate::validation::native_final_recovery::persist_marker_and_exit(
            Path::new(&destination),
            &marker,
        )
        .unwrap();
        panic!("owned marker boundary returned");
    }
    let fixture = Fixture::new().await;
    let marker = fixture.withheld().await;
    let child_directory = directory();
    let mut child=std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","journal::package_replacement::worker_transport::native_final_tests::native_final_actual_receipt_marker_is_fsynced_before_owned_process_exit","--nocapture"])
        .env("CIRROVE_SYNTHETIC_FINAL_MARKER",fixture.directory.path().join("lost-final-confirmation.json")).env("CIRROVE_SYNTHETIC_FINAL_DESTINATION",child_directory.path())
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().unwrap();
    let pid = child.id();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("owned synthetic child did not reach exit86 within bound");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(status.code(), Some(86));
    let emitted: LossMarker = serde_json::from_slice(
        &std::fs::read(child_directory.path().join("lost-final-confirmation.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(emitted.pid, pid);
    assert_eq!(emitted.operation, marker.operation);
    assert_eq!(emitted.receipt, marker.receipt);
    assert_eq!(emitted.checkpoint_sha256, marker.checkpoint_sha256);
    assert!(!emitted.acknowledgement_returned);
    assert_eq!(counts(&fixture.server), (1, 1, 1, 1, 1));
}

#[tokio::test]
async fn native_final_actual_completed_frontier_binds_queue_pairs_receipts_and_publication() {
    // Folder/A are acknowledged synthetic setup through actual journal APIs.
    // B's receipt is emitted by the unchanged HTTPS native adapter.
    let fixture = Fixture::new_seeded(true).await;
    let marker = fixture.withheld().await;
    let guard = Arc::new(fixture.recover(&marker));
    let worker = TransferWorker::new(
        fixture.context.journal(),
        guard.clone(),
        fixture.vault(),
        CancellationToken::new(),
    );
    let result = worker.run_once().await.unwrap().unwrap();
    assert_eq!(result.state, UploadState::Uploaded);
    assert_eq!(result.id, fixture.row.id);
    let journal = fixture.context.journal();
    assert_eq!(
        journal.lock().unwrap().get(result.id).unwrap().session_key,
        Some(result.id)
    );
    assert!(
        fixture
            .vault()
            .load(&format!("upload/{}", result.id))
            .await
            .unwrap()
            .is_none()
    );
    let fs = crate::filesystem::CloudFs::new_experimental_writable(
        fixture.engine.clone(),
        journal.clone(),
    )
    .await
    .unwrap();
    let control = fs.write_control().unwrap();
    assert!(control.publish_completed_package().await.unwrap());
    drop(control);
    drop(fs);
    drop(worker);
    drop(guard);
    drop(journal);
    let path = fixture
        .state
        .path()
        .join("accounts")
        .join(&fixture.account.id)
        .join("journal");
    let id = result.id;
    let account = fixture.account.id.clone();
    let folder = fixture.folder.unwrap();
    let imported = fixture.completed[0].clone();
    drop(fixture.context);
    let check = || {
        let read = RecoveryJournal::open(&path, &account).unwrap();
        crate::validation::native_final_recovery::registration::test_frontier(
            &read,
            &account,
            folder,
            &imported,
            Some(id),
            Some(&marker.receipt),
        )
    };
    check().unwrap();
    let db = rusqlite::Connection::open(path.join("uploads.db")).unwrap();
    // Each arm changes one genuine frontier component then restores it.
    db.execute(
        "INSERT INTO write_queue(sequence,id,complete) VALUES(999,?1,1)",
        [Uuid::new_v4().to_string()],
    )
    .unwrap();
    assert!(check().is_err(), "extra completed queue accepted");
    db.execute("DELETE FROM write_queue WHERE sequence=999", [])
        .unwrap();
    let owner: String = db
        .query_row(
            "SELECT object FROM namespace_operations WHERE operation=?1",
            [folder.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    db.execute(
        "DELETE FROM namespace_operations WHERE operation=?1",
        [folder.to_string()],
    )
    .unwrap();
    assert!(check().is_err(), "missing folder pair accepted");
    db.execute(
        "INSERT INTO namespace_operations(operation,object) VALUES(?1,?2)",
        rusqlite::params![folder.to_string(), Uuid::new_v4().to_string()],
    )
    .unwrap();
    assert!(check().is_err(), "wrong folder pair accepted");
    db.execute(
        "UPDATE namespace_operations SET object=?1 WHERE operation=?2",
        rusqlite::params![owner, folder.to_string()],
    )
    .unwrap();
    let body: String = db
        .query_row(
            "SELECT body FROM uploads WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    let mut changed: serde_json::Value = serde_json::from_str(&body).unwrap();
    changed["session_key"] = serde_json::Value::Null;
    db.execute(
        "UPDATE uploads SET body=?1 WHERE id=?2",
        rusqlite::params![changed.to_string(), id.to_string()],
    )
    .unwrap();
    assert!(check().is_err(), "missing retained session key accepted");
    db.execute(
        "UPDATE uploads SET body=?1 WHERE id=?2",
        rusqlite::params![body, id.to_string()],
    )
    .unwrap();
    db.execute(
        "UPDATE package_metadata_publication SET done=0 WHERE operation=?1",
        [id.to_string()],
    )
    .unwrap();
    assert!(check().is_err(), "unpublished B accepted");
    db.execute(
        "UPDATE package_metadata_publication SET done=1 WHERE operation=?1",
        [id.to_string()],
    )
    .unwrap();
    check().unwrap();
    assert_eq!(counts(&fixture.server), (1, 1, 1, 1, 1));
}

#[path = "derived_snapshot_tests.rs"]
mod derived_snapshot_tests;

#[path = "flat_tests.rs"]
mod flat_tests;
