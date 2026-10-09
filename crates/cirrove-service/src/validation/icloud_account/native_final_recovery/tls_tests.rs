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

// Proposed controls use the existing genuine TLS fixture and actual worker/vault.
mod pre_trash_pause_controls {
    use super::*;
    use anyhow::Context as _;
    struct Paused {
        fixture: Fixture,
        cancel: CancellationToken,
        task: Option<tokio::task::JoinHandle<anyhow::Result<crate::transfers::TransferResult>>>,
        marker: serde_json::Value,
        source: Vec<u8>,
        after_install_preflight: bool,
        guard: Arc<Guard>,
    }
    impl Paused {
        async fn new(limit: Duration) -> anyhow::Result<Self> {
            Self::new_at(limit, false).await
        }
        async fn new_install(limit: Duration) -> anyhow::Result<Self> {
            Self::new_at(limit, true).await
        }
        async fn new_at(limit: Duration, after_install_preflight: bool) -> anyhow::Result<Self> {
            let mut fixture = Fixture::new_layout(true, true).await;
            fixture.state.disable_cleanup(true);
            fixture.directory.disable_cleanup(true);
            fixture._staging.disable_cleanup(true);
            // Bind the unchanged server to the actual deterministic flat wire receipt.
            // This disposable preparation reads the original and prepares local bytes;
            // the real TransferWorker still claims, persists and drives every transfer.
            let (wire_size, wire_sha, semantic) = {
                let req = request(&fixture.row);
                let cirrove_core::upload::UploadRepresentation::FlatNumbersReplacementArchive {
                    semantic,
                    ..
                } = &req.representation
                else {
                    return Err(anyhow::anyhow!(
                        "explicit flat replacement fixture required"
                    ));
                };
                let semantic = semantic.clone();
                let journal = fixture.context.journal();
                let (payload, before) = {
                    let journal = journal
                        .lock()
                        .map_err(|_| anyhow::anyhow!("journal poisoned"))?;
                    let row = journal.get(fixture.row.id)?;
                    anyhow::ensure!(
                        row.state == UploadState::Pending
                            && row.attempt.is_none()
                            && row.session_key.is_none()
                            && row.remote.is_none()
                    );
                    (journal.payload(row.id)?, serde_json::to_vec(&row)?)
                };
                let router = fixture.router();
                let prepared = router
                    .begin_upload_from_payload_for_operation(
                        &fixture.row.id.to_string(),
                        &req,
                        payload,
                        &CancellationToken::new(),
                    )
                    .await
                    .map_err(|_| anyhow::anyhow!("flat fixture wire preparation refused"))?;
                let cirrove_core::upload::UploadStep::Allocate(saved) = prepared else {
                    return Err(anyhow::anyhow!(
                        "flat fixture did not prepare allocation checkpoint"
                    ));
                };
                let prepared: serde_json::Value = serde_json::from_str(saved.expose_secret())?;
                let inner = &prepared["phase"]["Stage"]["inner"];
                anyhow::ensure!(
                    inner["version"] == 2
                        && inner["wire"]["version"] == 1
                        && inner["wire"]["expected_root"]
                            == format!("staged-by-cirrove-{}.numbers", fixture.row.id)
                );
                let size = inner["wire"]["size"]
                    .as_u64()
                    .context("flat wire size absent")?;
                let sha = inner["wire"]["sha256"]
                    .as_str()
                    .context("flat wire hash absent")?
                    .to_owned();
                anyhow::ensure!(
                    size > 0
                        && size <= 64 * 1024 * 1024
                        && sha.len() == 64
                        && sha.bytes().all(|byte| byte.is_ascii_hexdigit())
                        && sha != req.sha256
                );
                let after = journal
                    .lock()
                    .map_err(|_| anyhow::anyhow!("journal poisoned"))?
                    .get(fixture.row.id)?;
                anyhow::ensure!(serde_json::to_vec(&after)? == before);
                anyhow::ensure!(counts(&fixture.server) == (0, 0, 0, 0, 0));
                // All disposable router/checkpoint/preparation values drop here.
                (size, sha, semantic)
            };
            fixture
                .server
                .state
                .lock()
                .map_err(|_| anyhow::anyhow!("TLS state poisoned"))?
                .flat_wire = Some((wire_size, wire_sha, semantic));
            let guard = fixture.loss();
            let active = tokio::time::Instant::now() + Duration::from_secs(60);
            let guard = Arc::new(
                if after_install_preflight {
                    guard.with_install_preflight_pause(active, limit)
                } else {
                    guard.with_pre_trash_pause(active, limit)
                }
                .map_err(|_| anyhow::anyhow!("pause configuration refused"))?,
            );
            let cancel = CancellationToken::new();
            let worker = TransferWorker::new(
                fixture.context.journal(),
                guard.clone(),
                fixture.vault(),
                cancel.clone(),
            );
            let mut task = tokio::spawn(async move {
                worker
                    .run_once()
                    .await
                    .map_err(|_| anyhow::anyhow!("worker transport error"))?
                    .context("worker found no operation")
            });
            let path = fixture.directory.path().join(if after_install_preflight {
                "install-preflight-paused.json"
            } else {
                "pre-trash-paused.json"
            });
            let observed = tokio::time::timeout(Duration::from_secs(20), async {
                loop {
                    if path.exists() {
                        // record() publishes the name before its JSON write completes.
                        // Wait only for an incomplete write; malformed JSON is a setup error.
                        match serde_json::from_slice::<serde_json::Value>(&std::fs::read(&path)?) {
                            Ok(marker) => return Ok::<_, anyhow::Error>(marker),
                            Err(error) if error.is_eof() => {}
                            Err(error) => return Err(error.into()),
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await;
            let marker = match observed {
                Ok(Ok(marker)) => marker,
                _ => {
                    cancel.cancel();
                    let _ = tokio::time::timeout(Duration::from_secs(5), &mut task).await;
                    task.abort();
                    return Err(anyhow::anyhow!("TLS pause marker not reached"));
                }
            };
            let mut paused = Self {
                fixture,
                cancel,
                task: Some(task),
                marker,
                source: Vec::new(),
                after_install_preflight,
                guard,
            };
            let prepared = async {
                let journal = paused.fixture.context.journal();
                let row = journal
                    .try_lock()
                    .map_err(|_| anyhow::anyhow!("journal mutex held across pause"))?
                    .get(paused.fixture.row.id)?;
                anyhow::ensure!(
                    row.state == UploadState::Uploading
                        && row.attempt.is_some()
                        && row.session_key == Some(row.id)
                        && row.remote.is_none()
                );
                anyhow::ensure!(
                    paused.marker["operation"] == row.id.to_string()
                        && paused.marker["phase"]
                            == if after_install_preflight {
                                "after-final-install-preflight"
                            } else {
                                "handoff-move-old-armed"
                            }
                        && paused.marker[if after_install_preflight {
                            "rename_called"
                        } else {
                            "inner_commit_called"
                        }] == false
                );
                let saved = paused
                    .fixture
                    .vault()
                    .load(&format!("upload/{}", row.id))
                    .await?
                    .context("persisted encrypted checkpoint absent")?;
                anyhow::ensure!(
                    hex::encode(Sha256::digest(saved.expose_secret().as_bytes()))
                        == paused.marker["checkpoint_sha256"]
                );
                anyhow::ensure!(paused.marker["original_id"] != paused.marker["staged_id"]);
                anyhow::ensure!(
                    counts(&paused.fixture.server)
                        == (1, 1, 1, usize::from(after_install_preflight), 0)
                );
                paused.source = std::fs::read(
                    journal
                        .lock()
                        .map_err(|_| anyhow::anyhow!("journal poisoned"))?
                        .objects
                        .join(row.id.to_string()),
                )?;
                Ok::<(), anyhow::Error>(())
            }
            .await;
            if let Err(error) = prepared {
                paused.cancel.cancel();
                let _ = paused.finished().await;
                return Err(error);
            }
            Ok(paused)
        }
        fn release(&self) -> serde_json::Value {
            let mut value = serde_json::json!({"version":1,"run":self.marker["run"],"operation":self.marker["operation"],
                "attempt":self.marker["attempt"],"checkpoint_sha256":self.marker["checkpoint_sha256"]});
            if self.after_install_preflight {
                for key in ["phase", "original_id", "staged_id", "parent_id", "scope"] {
                    value[key] = self.marker[key].clone();
                }
            }
            value
        }
        fn publish(&self, value: &serde_json::Value) -> anyhow::Result<()> {
            let path = self.fixture.directory.path();
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o400)
                .open(path.join("release.prepared.json"))?;
            file.write_all(&serde_json::to_vec(value)?)?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(
                path.join("release.prepared.json"),
                path.join(if self.after_install_preflight {
                    "install-preflight-release.json"
                } else {
                    "pre-trash-release.json"
                }),
            )?;
            std::fs::File::open(path)?.sync_all()?;
            Ok(())
        }
        async fn finished(&mut self) -> anyhow::Result<crate::transfers::TransferResult> {
            let mut task = self.task.take().context("worker already joined")?;
            match tokio::time::timeout(Duration::from_secs(5), &mut task).await {
                Ok(result) => result?,
                Err(_) => {
                    self.cancel.cancel();
                    task.abort();
                    let _ = task.await;
                    Err(anyhow::anyhow!("worker close timeout"))
                }
            }
        }
        fn preserved_before_trash(&self) -> anyhow::Result<()> {
            anyhow::ensure!(
                counts(&self.fixture.server)
                    == (1, 1, 1, usize::from(self.after_install_preflight), 0)
            );
            let journal = self.fixture.context.journal();
            let journal = journal
                .lock()
                .map_err(|_| anyhow::anyhow!("journal poisoned"))?;
            let row = journal.get(self.fixture.row.id)?;
            anyhow::ensure!(row.remote.is_none() && row.package_completion.is_none());
            anyhow::ensure!(
                std::fs::read(journal.objects.join(row.id.to_string()))? == self.source
            );
            Ok(())
        }
    }
    impl Drop for Paused {
        fn drop(&mut self) {
            self.cancel.cancel();
            if let Some(task) = self.task.take() {
                task.abort();
            }
        }
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn native_final_pre_trash_pause_fifo_refuses_without_waiting_for_writer()
    -> anyhow::Result<()> {
        let mut p = Paused::new(Duration::from_secs(120)).await?;
        let path = p.fixture.directory.path().join("pre-trash-release.json");
        anyhow::ensure!(
            std::process::Command::new("mkfifo")
                .args(["--mode=400", "--"])
                .arg(&path)
                .status()?
                .success(),
            "FIFO fixture creation failed"
        );
        // The omission can block a Tokio worker in open(2). Its watchdog must
        // use an independent OS-thread clock, never the runtime's stalled timer.
        let completion = p.task.as_ref().context("worker missing")?.abort_handle();
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<()>(1);
        let (joined_tx, joined_rx) = std::sync::mpsc::sync_channel::<()>(1);
        let watchdog = std::thread::Builder::new()
            .name("pre-trash-fifo-watchdog".into())
            .spawn(move || -> anyhow::Result<bool> {
                match done_rx.recv_timeout(Duration::from_secs(2)) {
                    Ok(()) => return Ok(true),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        // A completed task whose join continuation was delayed is
                        // not evidence of a blocking release-file open.
                        if completion.is_finished() {
                            return Ok(true);
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        return Err(anyhow::anyhow!("worker completion channel closed"));
                    }
                }
                // Rescue the blocking counterfactual independently of Tokio.
                // RDWR opens without a peer and retains the writer through join.
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
                let mut writer = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(&path)?;
                writer.write_all(b"{}")?;
                joined_rx
                    .recv_timeout(Duration::from_secs(10))
                    .context("rescued worker was not joined")?;
                drop(writer);
                Ok(false)
            })?;
        let task = p.task.take().context("worker missing")?;
        let worker_result = task.await;
        // Notify before propagating any worker error; always join the watchdog.
        let _ = done_tx.send(());
        let _ = joined_tx.send(());
        let watchdog_result = watchdog.join();
        let completed_before_writer =
            watchdog_result.map_err(|_| anyhow::anyhow!("FIFO watchdog panicked"))??;
        worker_result??;
        p.preserved_before_trash()?;
        anyhow::ensure!(
            completed_before_writer,
            "release open waited for a FIFO writer"
        );
        Ok(())
    }
    #[tokio::test]
    async fn native_final_pre_trash_pause_symlink_release_refuses() -> anyhow::Result<()> {
        let mut p = Paused::new(Duration::from_secs(120)).await?;
        let target = p.fixture.directory.path().join("synthetic-target.json");
        let bytes = serde_json::to_vec(&p.release())?;
        std::fs::write(&target, &bytes)?;
        std::os::unix::fs::symlink(
            &target,
            p.fixture.directory.path().join("pre-trash-release.json"),
        )?;
        p.finished().await?;
        p.preserved_before_trash()?;
        anyhow::ensure!(std::fs::read(target)? == bytes);
        Ok(())
    }
    #[tokio::test]
    async fn native_final_pre_trash_pause_invalid_release_tuple_refuses() -> anyhow::Result<()> {
        for field in [
            "run",
            "operation",
            "attempt",
            "checkpoint_sha256",
            "unknown",
        ] {
            let mut p = Paused::new(Duration::from_secs(120)).await?;
            let mut value = p.release();
            value[field] = if field == "checkpoint_sha256" {
                "0".repeat(64).into()
            } else if field == "unknown" {
                true.into()
            } else {
                Uuid::new_v4().to_string().into()
            };
            p.publish(&value)?;
            p.finished().await?;
            p.preserved_before_trash()?;
        }
        Ok(())
    }
    #[tokio::test]
    async fn native_final_pre_trash_pause_cancel_refuses_before_commit() -> anyhow::Result<()> {
        let mut p = Paused::new(Duration::from_secs(120)).await?;
        p.cancel.cancel();
        p.finished().await?;
        p.preserved_before_trash()?;
        Ok(())
    }
    #[tokio::test]
    async fn native_final_pre_trash_pause_expiry_never_grants_release() -> anyhow::Result<()> {
        let mut p = Paused::new(Duration::from_secs(1)).await?;
        p.finished().await?;
        p.publish(&p.release())?;
        p.preserved_before_trash()?;
        Ok(())
    }
    #[tokio::test]
    async fn native_final_pre_trash_pause_valid_release_delegates_once() -> anyhow::Result<()> {
        let mut p = Paused::new(Duration::from_secs(120)).await?;
        p.publish(&p.release())?;
        let row = p.finished().await?;
        anyhow::ensure!(row.id == p.fixture.row.id && row.state == UploadState::VerifyRequired);
        anyhow::ensure!(counts(&p.fixture.server) == (1, 1, 1, 1, 1));
        let lost = p.fixture.marker();
        anyhow::ensure!(lost.operation == row.id && !lost.acknowledgement_returned);
        let journal = p.fixture.context.journal();
        anyhow::ensure!(
            std::fs::read(
                journal
                    .lock()
                    .map_err(|_| anyhow::anyhow!("journal poisoned"))?
                    .objects
                    .join(row.id.to_string())
            )? == p.source
        );
        Ok(())
    }
    #[tokio::test]
    async fn native_final_pre_trash_pause_changed_frontier_refuses() -> anyhow::Result<()> {
        let mut p = Paused::new(Duration::from_secs(120)).await?;
        let journal = p.fixture.context.journal();
        {
            let journal = journal
                .lock()
                .map_err(|_| anyhow::anyhow!("journal poisoned"))?;
            let mut row = journal.get(p.fixture.row.id)?;
            row.failed_attempts += 1;
            journal.db.execute(
                "UPDATE uploads SET body=?1 WHERE id=?2",
                rusqlite::params![serde_json::to_string(&row)?, row.id.to_string()],
            )?;
        }
        p.publish(&p.release())?;
        p.finished().await?;
        p.preserved_before_trash()?;
        Ok(())
    }
    #[tokio::test]
    async fn native_final_install_pause_release_identity_and_phase_refuse() -> anyhow::Result<()> {
        for key in [
            "run",
            "operation",
            "attempt",
            "checkpoint_sha256",
            "phase",
            "original_id",
            "staged_id",
            "parent_id",
            "scope",
            "unknown",
        ] {
            let mut p = Paused::new_install(Duration::from_secs(120)).await?;
            let mut value = p.release();
            value[key] = "foreign".into();
            p.publish(&value)?;
            p.finished().await?;
            p.preserved_before_trash()?;
        }
        Ok(())
    }
    #[tokio::test]
    async fn native_final_install_pause_cancel_and_expiry_never_send() -> anyhow::Result<()> {
        for cancel in [true, false] {
            let mut p =
                Paused::new_install(Duration::from_secs(if cancel { 120 } else { 1 })).await?;
            if cancel {
                p.cancel.cancel();
            }
            p.finished().await?;
            p.publish(&p.release())?;
            p.preserved_before_trash()?;
        }
        Ok(())
    }
    #[tokio::test]
    async fn native_final_install_pause_changed_frontier_refuses() -> anyhow::Result<()> {
        let mut p = Paused::new_install(Duration::from_secs(120)).await?;
        {
            let journal = p.fixture.context.journal();
            let journal = journal
                .lock()
                .map_err(|_| anyhow::anyhow!("journal poisoned"))?;
            let mut row = journal.get(p.fixture.row.id)?;
            row.failed_attempts += 1;
            journal.db.execute(
                "UPDATE uploads SET body=?1 WHERE id=?2",
                rusqlite::params![serde_json::to_string(&row)?, row.id.to_string()],
            )?;
        }
        p.publish(&p.release())?;
        p.finished().await?;
        p.preserved_before_trash()?;
        Ok(())
    }
    #[tokio::test]
    async fn native_final_install_pause_valid_release_and_same_operation_inspection_no_replay()
    -> anyhow::Result<()> {
        let mut p = Paused::new_install(Duration::from_secs(120)).await?;
        let op = p.fixture.row.id.to_string();
        let req = request(&p.fixture.row);
        let saved = p
            .fixture
            .vault()
            .load(&format!("upload/{op}"))
            .await?
            .context("saved checkpoint absent")?;
        let probe = p.guard.install_probe_for_test(&op, &req, &saved);
        use cirrove_icloud::NativeInstallPreflightProbe;
        anyhow::ensure!(
            probe
                .after_final_preflight(
                    p.marker["original_id"].as_str().context("old ID missing")?,
                    p.marker["staged_id"].as_str().context("stage ID missing")?,
                    p.marker["parent_id"]
                        .as_str()
                        .context("parent ID missing")?,
                    &CancellationToken::new(),
                )
                .await
                .is_err()
        );
        drop(probe);
        p.preserved_before_trash()?;
        p.publish(&p.release())?;
        let row = p.finished().await?;
        anyhow::ensure!(row.state == UploadState::VerifyRequired);
        anyhow::ensure!(counts(&p.fixture.server) == (1, 1, 1, 1, 1));
        let recovered = Arc::new(p.fixture.recover(&p.fixture.marker()));
        let worker = TransferWorker::new(
            p.fixture.context.journal(),
            recovered,
            p.fixture.vault(),
            CancellationToken::new(),
        );
        // Match Fixture::withheld: the retained VerifyRequired row has a
        // two-second retry frontier before its one verification-only inspection.
        tokio::time::sleep(Duration::from_secs(2)).await;
        let result = worker.run_once().await?.context("same operation missing")?;
        anyhow::ensure!(result.id == row.id && result.state == UploadState::Uploaded);
        anyhow::ensure!(counts(&p.fixture.server) == (1, 1, 1, 1, 1));
        anyhow::ensure!(p.guard.install_pause_was_used_for_test());
        Ok(())
    }
}
