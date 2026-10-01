//! Real public protocol, durable workers and metadata publication; no Apple IO.
use super::*;
use crate::jobs::{Job, JobState};
use anyhow::{Context, ensure};
use cirrove_auth::CredentialVault;
use cirrove_core::{
    mutation::{
        MutationError, MutationProvider, MutationReceipt, MutationReconciliation, MutationRequest,
    },
    upload::{
        PackageHandoffReceipt, PackageUploadReceipt, Reconciliation, UploadError, UploadProvider,
        UploadRequest, UploadStep,
    },
};
use secrecy::{ExposeSecret, SecretString};

#[derive(Default)]
struct Vault(Mutex<HashMap<String, SecretString>>);
#[async_trait::async_trait]
impl CredentialVault for Vault {
    async fn load(&self, key: &str) -> anyhow::Result<Option<SecretString>> {
        Ok(self.0.lock().unwrap().get(key).cloned())
    }
    async fn save(&self, key: &str, value: SecretString) -> anyhow::Result<()> {
        self.0.lock().unwrap().insert(key.into(), value);
        Ok(())
    }
    async fn remove(&self, key: &str) -> anyhow::Result<()> {
        self.0.lock().unwrap().remove(key);
        Ok(())
    }
}
struct ReplacementWriter {
    remote: Arc<Provider>,
    scope: Scope,
    allocations: AtomicUsize,
    bodies: AtomicUsize,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
impl ReplacementWriter {
    fn check(&self, r: &UploadRequest) -> std::result::Result<(), UploadError> {
        if r.scope != self.scope
            || !matches!(r.intent, UploadIntent::Replace { .. })
            || !matches!(
                r.representation,
                UploadRepresentation::PackageReplacementArchive { .. }
            )
        {
            return Err(UploadError::Invalid);
        }
        r.validate()
    }
}
#[async_trait::async_trait]
impl UploadProvider for ReplacementWriter {
    fn staged_recovery_location(
        &self,
        operation: &str,
        request: &UploadRequest,
    ) -> Option<cirrove_core::upload::RecoveryLocation> {
        self.check(request).ok()?;
        let operation = uuid::Uuid::parse_str(operation).ok()?;
        Some(cirrove_core::upload::RecoveryLocation::Trash {
            local_name: format!("recovery-by-cirrove-{operation}.pages"),
            parent: "FOLDER::com.apple.CloudDocs::TRASH_ROOT".into(),
        })
    }

    fn begin_is_mutation_free_until_checkpoint(&self, _: &UploadRequest) -> bool {
        true
    }
    async fn begin_upload(
        &self,
        r: &UploadRequest,
        _: &CancellationToken,
    ) -> std::result::Result<UploadStep, UploadError> {
        self.check(r)?;
        Ok(UploadStep::Allocate(SecretString::from("allocation-armed")))
    }
    async fn allocate_upload_for_operation(
        &self,
        operation: &str,
        r: &UploadRequest,
        checkpoint: &SecretString,
        _: &CancellationToken,
    ) -> std::result::Result<UploadStep, UploadError> {
        self.check(r)?;
        if checkpoint.expose_secret() != "allocation-armed"
            || uuid::Uuid::parse_str(operation).is_err()
        {
            return Err(UploadError::CheckpointInvalid);
        }
        self.allocations.fetch_add(1, Ordering::SeqCst);
        Ok(UploadStep::Stream(SecretString::from(operation.to_owned())))
    }
    async fn inspect_upload(
        &self,
        _: &UploadRequest,
        _: &SecretString,
        _: &CancellationToken,
    ) -> std::result::Result<UploadStep, UploadError> {
        Err(UploadError::Uncertain)
    }
    async fn upload_part(
        &self,
        _: &UploadRequest,
        _: &SecretString,
        _: u64,
        _: Vec<u8>,
        _: &CancellationToken,
    ) -> std::result::Result<UploadStep, UploadError> {
        Err(UploadError::Invalid)
    }
    async fn commit_upload(
        &self,
        _: &UploadRequest,
        _: &SecretString,
        _: &CancellationToken,
    ) -> std::result::Result<UploadStep, UploadError> {
        Err(UploadError::Invalid)
    }
    async fn reconcile_upload(
        &self,
        _: &UploadRequest,
        _: Option<&SecretString>,
        _: &CancellationToken,
    ) -> std::result::Result<Reconciliation, UploadError> {
        Err(UploadError::Uncertain)
    }
    async fn upload_stream(
        &self,
        r: &UploadRequest,
        checkpoint: &SecretString,
        payload: std::fs::File,
        cancel: &CancellationToken,
    ) -> std::result::Result<UploadStep, UploadError> {
        self.check(r)?;
        let UploadRepresentation::PackageReplacementArchive {
            expected_root,
            semantic,
            original,
            original_semantic,
        } = &r.representation
        else {
            return Err(UploadError::Invalid);
        };
        let identity = cirrove_icloud::package_archive_semantic_identity_versioned(
            &payload,
            &cirrove_icloud::PackageDownload {
                size: r.size,
                sha256: r.sha256.clone(),
            },
            expected_root,
            semantic.version,
            cancel,
        )
        .map_err(|_| UploadError::Invalid)?;
        if &identity != semantic {
            return Err(UploadError::Invalid);
        }
        let operation = uuid::Uuid::parse_str(checkpoint.expose_secret())
            .map_err(|_| UploadError::CheckpointInvalid)?;
        self.bodies.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(ProviderError::Cancelled.into()),
            _ = self.release.notified() => {}
        }
        let current = Node {
            id: format!("FILE::com.apple.CloudDocs::{operation}"),
            parent_id: original.parent_id.clone(),
            name: original.name.clone(),
            kind: NodeKind::Folder,
            size: semantic.expanded_bytes,
            modified_unix: 1,
            etag: Some("new-package-v1".into()),
            content_version: None,
            target: None,
            package: true,
        };
        let backup = Node {
            parent_id: Some("FOLDER::com.apple.CloudDocs::TRASH_ROOT".into()),
            etag: Some("trash-v2".into()),
            ..*original.clone()
        };
        {
            let mut nodes = self.remote.nodes.lock().unwrap();
            let before = nodes
                .iter()
                .find(|n| n.id == original.id)
                .ok_or(UploadError::Conflict)?;
            if before != original.as_ref() {
                return Err(UploadError::Conflict);
            }
            nodes.retain(|n| n.id != original.id);
            nodes.push(current.clone());
        }
        Ok(UploadStep::PackageHandoffComplete(Box::new(
            PackageHandoffReceipt {
                original: *original.clone(),
                current: PackageUploadReceipt {
                    remote: current,
                    semantic: semantic.clone(),
                },
                backup: PackageUploadReceipt {
                    remote: backup,
                    semantic: original_semantic.clone(),
                },
            },
        )))
    }
}
#[async_trait::async_trait]
impl MutationProvider for ReplacementWriter {
    async fn mutate(
        &self,
        _: &MutationRequest,
        _: &CancellationToken,
    ) -> std::result::Result<MutationReceipt, MutationError> {
        Err(MutationError::Invalid)
    }
    async fn reconcile_mutation(
        &self,
        _: &MutationRequest,
        _: &CancellationToken,
    ) -> std::result::Result<MutationReconciliation, MutationError> {
        Err(MutationError::Invalid)
    }
}
async fn public_job(socket: &Path, id: &str) -> anyhow::Result<Job> {
    crate::status(socket)
        .await?
        .accounts
        .into_iter()
        .flat_map(|a| a.jobs)
        .find(|j| j.id == id)
        .context("public job disappeared")
}
async fn wait_job(socket: &Path, id: &str, ready: impl Fn(&Job) -> bool) -> anyhow::Result<Job> {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let job = public_job(socket, id).await?;
            if ready(&job) {
                return Ok(job);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .context("public job timed out")?
}
fn terminal(job: &Job) -> bool {
    matches!(
        job.state,
        JobState::Succeeded | JobState::Stopped | JobState::Failed
    )
}
/// Synthetic provider receipts; real public socket, admission, worker and publication.
/// Actual iCloud HTTP checkpoint/CAS behavior is tested in the separate worker fixture.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_replacement_public_socket_worker_discovery_watch_and_stop() -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt;
    let Fixture {
        _temp,
        manager,
        engine,
        control,
        journal,
        provider,
        source,
    } = Fixture::new().await;
    let root = _temp.keep();
    let scope = engine.scope("drive");
    let mut before = node(
        "FILE::com.apple.CloudDocs::old-socket",
        Some(ROOT),
        "Socket.pages",
    );
    before.package = true;
    provider.nodes.lock().unwrap().push(before.clone());
    let capture = Arc::new(crate::manager::NativeReplaceCaptureFixture {
        account: engine.account.id.clone(),
        original: before.clone(),
        semantic: cirrove_core::upload::PackageSemanticIdentity {
            version: 1,
            sha256: "a".repeat(64),
            entries: 1,
            files: 1,
            expanded_bytes: 3,
        },
        calls: AtomicUsize::new(0),
    });
    *manager.native_replace_capture_fixture.lock().unwrap() = Some(capture.clone());
    let writer = Arc::new(ReplacementWriter {
        remote: provider.clone(),
        scope: scope.clone(),
        allocations: AtomicUsize::new(0),
        bodies: AtomicUsize::new(0),
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let worker_cancel = CancellationToken::new();
    let workers = crate::writable::WriteWorkers::spawn(
        control.clone(),
        journal.clone(),
        writer.clone(),
        Arc::new(Vault::default()),
        &worker_cancel,
    );
    manager
        .writers
        .write()
        .await
        .insert(engine.account.id.clone(), workers.control());
    let runtime = tempfile::Builder::new()
        .prefix("cirrove-replace-socket-")
        .tempdir_in("/var/tmp")?
        .keep();
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700))?;
    let socket = runtime.join("control.sock");
    let mut server_cancel = CancellationToken::new();
    let mut server = Some(tokio::spawn(crate::serve_managed(
        root.join("socket.sqlite"),
        socket.clone(),
        server_cancel.clone(),
        Some(manager.clone()),
    )));
    let outcome: anyhow::Result<()> = async {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !socket.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        let request = crate::ReplaceNativePackageRequest {
            label: engine.account.label.clone(),
            expected_account_id: engine.account.id.clone(),
            path: before.name.clone(),
            item_id: before.id.clone(),
            etag: before.etag.clone().unwrap(),
            archive: source,
            expected_root: "Source.pages".into(),
        };
        ensure!(
            crate::replace_native_package(&runtime.join("absent.sock"), &request)
                .await
                .is_err()
        );
        let mut wrong = request.clone();
        wrong.expected_account_id = uuid::Uuid::new_v4().to_string();
        let refusal = crate::replace_native_package(&socket, &wrong).await?;
        ensure!(refusal.job.is_none() && refusal.refusal.is_some());
        ensure!(capture.calls.load(Ordering::SeqCst) == 0);
        ensure!(
            engine
                .children(&scope, ROOT)
                .await?
                .iter()
                .any(|n| n.id == before.id),
            "original metadata not warmed"
        );
        // Deliberately discard the first reply. Discovery must not require that job ID.
        let mut peer = tokio::net::UnixStream::connect(&socket).await?;
        peer.write_all(
            format!(
                "replace-native-package {}\n",
                serde_json::to_string(&request)?
            )
            .as_bytes(),
        )
        .await?;
        drop(peer);
        tokio::time::timeout(Duration::from_secs(10), writer.entered.notified()).await?;
        let page = crate::list_native_replacements(
            &socket,
            &crate::ListNativeReplacementsRequest {
                label: request.label.clone(),
                expected_account_id: request.expected_account_id.clone(),
                after: None,
                limit: 100,
            },
        )
        .await?;
        ensure!(
            page.refusal.is_none() && page.operations.len() == 1,
            "lost reply did not leave one discoverable operation"
        );
        let selected = &page.operations[0];
        let operation = selected.operation;
        ensure!(selected.original.item == before.id && !selected.handoff_receipt_recorded);
        ensure!(
            writer.allocations.load(Ordering::SeqCst) == 1 && writer.bodies.load(Ordering::SeqCst) == 1
        );
        let watch_request = crate::WatchNativeReplacementRequest {
            label: request.label.clone(),
            expected_account_id: request.expected_account_id.clone(),
            operation,
        };
        let first = crate::watch_native_replacement(&socket, &watch_request)
            .await?
            .job
            .context("watch did not attach")?;
        ensure!(
            first
                .native_replace
                .as_ref()
                .is_some_and(|p| p.operation == operation && p.current.is_none())
        );
        crate::stop_job(
            &socket,
            &crate::StopJobRequest {
                label: request.label.clone(),
                id: first.id.clone(),
            },
        )
        .await?;
        let stopped = wait_job(&socket, &first.id, terminal).await?;
        ensure!(
            stopped.state == JobState::Stopped
                && stopped
                    .native_replace
                    .as_ref()
                    .is_some_and(|p| p.operation == operation && p.current.is_none())
        );
        let watching = crate::watch_native_replacement(&socket, &watch_request)
            .await?
            .job
            .context("second watch did not attach")?;
        writer.release.notify_one();
        let complete = wait_job(&socket, &watching.id, terminal).await?;
        ensure!(
            complete.state == JobState::Succeeded,
            "replacement did not complete: {:?}",
            complete.issue
        );
        let receipt = complete
            .native_replace
            .context("missing typed public receipt")?;
        let current = receipt.current.context("missing replacement identity")?;
        let recovery = receipt.recovery.context("missing old recovery identity")?;
        ensure!(
            receipt.operation == operation
                && receipt.original == before
                && current.id != before.id
                && recovery.id == before.id
        );
        let retained = journal.lock().unwrap().get(operation)?;
        let frozen = serde_json::to_vec(&retained)?;
        ensure!(
            matches!(journal.lock().unwrap().package_publication_status(operation)?,crate::journal::PackagePublicationStatus::Present(ref n) if n==&current),
            "typed handoff lacked durable canonical publication"
        );
        let (visible_scope, visible) = control
            .resolve_visible_path_mode(&engine, &before.name, true)
            .await?;
        ensure!(
            visible_scope == scope && visible == current,
            "warm mounted name did not resolve the replacement identity"
        );
        ensure!(
            cirrove_store::Store::open(&engine.db)?
                .node(&scope, &current.id)?
                .as_ref()
                == Some(&current)
        );
        // Forget every in-memory job, including the deliberately lost submit reply.
        // Subsequent discovery can only be backed by the durable operation row.
        for job in engine.jobs.list() {
            wait_job(&socket, &job.id, terminal).await?;
            crate::stop_job(
                &socket,
                &crate::StopJobRequest {
                    label: request.label.clone(),
                    id: job.id,
                },
            )
            .await?;
        }
        ensure!(engine.jobs.list().is_empty());
        // Restart only the public server/Manager, with no admission capture seam.
        server_cancel.cancel();
        tokio::time::timeout(
            Duration::from_secs(5),
            server.take().context("missing server")?,
        )
        .await???;
        let restarted = Arc::new(Manager::default());
        restarted
            .status
            .write()
            .await
            .extend(manager.status.read().await.clone());
        restarted
            .engines
            .write()
            .await
            .insert(engine.account.id.clone(), engine.clone());
        restarted
            .writers
            .write()
            .await
            .insert(engine.account.id.clone(), control.clone());
        server_cancel = CancellationToken::new();
        server = Some(tokio::spawn(crate::serve_managed(
            root.join("socket-restarted.sqlite"),
            socket.clone(),
            server_cancel.clone(),
            Some(restarted),
        )));
        tokio::time::timeout(Duration::from_secs(5), async {
            while !socket.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        let page = crate::list_native_replacements(
            &socket,
            &crate::ListNativeReplacementsRequest {
                label: request.label.clone(),
                expected_account_id: request.expected_account_id.clone(),
                after: None,
                limit: 1,
            },
        )
        .await?;
        ensure!(
            page.operations.len() == 1
                && page.operations[0].operation == operation
                && page.operations[0].handoff_receipt_recorded
        );
        let final_watch = crate::watch_native_replacement(&socket, &watch_request)
            .await?
            .job
            .context("restart watch refused")?;
        ensure!(wait_job(&socket, &final_watch.id, terminal).await?.state == JobState::Succeeded);
        ensure!(
            serde_json::to_vec(&journal.lock().unwrap().get(operation)?)? == frozen,
            "observation rewrote upload record"
        );
        ensure!(
            writer.allocations.load(Ordering::SeqCst) == 1
                && writer.bodies.load(Ordering::SeqCst) == 1
                && capture.calls.load(Ordering::SeqCst) == 1,
            "watch/list/stop resubmitted replacement"
        );
        ensure!(
            journal.lock().unwrap().list(0, 100)?.len() == 1
                && provider.reads.load(Ordering::SeqCst) == 0
        );
        // A second explicit user selection must resolve the new identity even
        // while raw cached directory membership still contains the old one.
        // This is a new operation, after the one-allocation observation checks.
        let selected = crate::native_trash::NativeTrashInput {
            expected_account_id: engine.account.id.clone(),
            path: current.name.clone(),
            item_id: current.id.clone(),
            etag: current.etag.clone().context("new revision missing")?,
        };
        let selection = control
            .native_trash_selection(&engine, &selected, &CancellationToken::new())
            .await?;
        ensure!(selection.target == current);
        {
            let journal = journal.lock().unwrap();
            journal.validate_native_selection(&selection, &CancellationToken::new()).context("follow-up journal selection")?;
            // Continue this scenario with a second replacement. The separate
            // native_trash_after_replacement_requires_following_exact_new_owner
            // regression covers Trash admission and its resource reservation.
        }
        let semantic = match &retained.representation {
            UploadRepresentation::PackageReplacementArchive { semantic, .. } => semantic.clone(),
            _ => anyhow::bail!("replacement representation lost"),
        };
        let expected = current.clone();
        let second = manager
            .enqueue_native_replacement_with(
                engine.clone(),
                crate::native_import::NativeReplaceInput {
                    selected,
                    source: request.archive.clone(),
                    expected_root: request.expected_root.clone(),
                },
                CancellationToken::new(),
                move |node, _, _| async move {
                    ensure!(node == expected, "second selection changed");
                    Ok(semantic)
                },
            )
            .await?;
        ensure!(second.id != operation);
        ensure!(matches!(second.intent,
                UploadIntent::Replace { ref item, ref expected_etag }
                if item == &current.id && Some(expected_etag) == current.etag.as_ref()));
        ensure!(journal.lock().unwrap().list(0, 100)?.len() == 2);
        Ok(())
    }.await;
    writer.release.notify_one();
    worker_cancel.cancel();
    server_cancel.cancel();
    let drained = tokio::time::timeout(Duration::from_secs(10), workers.drain()).await;
    let served = if let Some(server) = server {
        Some(tokio::time::timeout(Duration::from_secs(5), server).await)
    } else {
        None
    };
    engine.stop().await;
    outcome?;
    drained??;
    if let Some(served) = served {
        served???;
    }
    Ok(())
}
