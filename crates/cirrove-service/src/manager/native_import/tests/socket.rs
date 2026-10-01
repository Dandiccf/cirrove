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
        PackageUploadReceipt, Reconciliation, UploadError, UploadProvider, UploadRequest,
        UploadStep,
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
struct PackageWriter {
    remote: Arc<Provider>,
    scope: Scope,
    allocations: AtomicUsize,
    bodies: AtomicUsize,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
impl PackageWriter {
    fn check(&self, r: &UploadRequest) -> std::result::Result<(), UploadError> {
        if r.scope != self.scope
            || !matches!(r.intent, UploadIntent::Create { .. })
            || !matches!(
                r.representation,
                UploadRepresentation::PackageArchive { .. }
            )
        {
            return Err(UploadError::Invalid);
        }
        r.validate()
    }
}
#[async_trait::async_trait]
impl UploadProvider for PackageWriter {
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
        let UploadRepresentation::PackageArchive {
            expected_root,
            semantic,
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
        tokio::select! {biased; _=cancel.cancelled()=>return Err(ProviderError::Cancelled.into()), _=self.release.notified()=>{}}
        let UploadIntent::Create { parent, name } = &r.intent else {
            return Err(UploadError::Invalid);
        };
        let remote = Node {
            id: format!("FILE::com.apple.CloudDocs::{operation}"),
            parent_id: Some(parent.clone()),
            name: name.clone(),
            kind: NodeKind::Folder,
            size: semantic.expanded_bytes,
            modified_unix: 1,
            etag: Some("verified-package-v1".into()),
            content_version: Some("verified-package-v1".into()),
            target: None,
            package: true,
        };
        // Read projection is canonical; retain the old receipt's exact ETag
        // alias to exercise compatibility through the real public observer.
        let mut observed = remote.clone();
        observed.content_version = None;
        self.remote.nodes.lock().unwrap().push(observed);
        Ok(UploadStep::PackageComplete(PackageUploadReceipt {
            remote,
            semantic: semantic.clone(),
        }))
    }
}
#[async_trait::async_trait]
impl MutationProvider for PackageWriter {
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
async fn scenario(mounted: bool) -> anyhow::Result<()> {
    let Fixture {
        _temp,
        manager,
        engine,
        mut control,
        journal,
        provider,
        source,
    } = Fixture::new().await;
    // Never recursively clean a fixture that may contain a live mount on failure.
    let root = _temp.keep();
    let mut kernel = None;
    if mounted {
        let fs = CloudFs::new_experimental_writable(engine.clone(), journal.clone()).await?;
        control = fs.write_control()?;
        kernel = Some(fs.mount(&engine.account.mount_path)?);
        manager
            .writers
            .write()
            .await
            .insert(engine.account.id.clone(), control.clone());
    }
    let scope = engine.scope("drive");
    let writer = Arc::new(PackageWriter {
        remote: provider.clone(),
        scope: scope.clone(),
        allocations: AtomicUsize::new(0),
        bodies: AtomicUsize::new(0),
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let cancel = CancellationToken::new();
    let workers = crate::writable::WriteWorkers::spawn(
        control.clone(),
        journal.clone(),
        writer.clone(),
        Arc::new(Vault::default()),
        &cancel,
    );
    manager
        .writers
        .write()
        .await
        .insert(engine.account.id.clone(), workers.control());
    // Unix socket paths are capped independently of the parent's disk TMPDIR.
    let runtime = tempfile::Builder::new()
        .prefix("cirrove-native-socket-")
        .tempdir_in("/var/tmp")?
        .keep();
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700))?;
    let socket = runtime.join("control.sock");
    let server_socket = socket.clone();
    let server_cancel = cancel.clone();
    let server_manager = manager.clone();
    let db = root.join("socket-status.sqlite");
    let server = tokio::spawn(async move {
        crate::serve_managed(db, server_socket, server_cancel, Some(server_manager)).await
    });
    let outcome: anyhow::Result<()> = async {
        let mut request = crate::ImportNativePackageRequest {
            label: engine.account.label.clone(),
            expected_account_id: None,
            archive: source,
            expected_root: "Source.pages".into(),
            parent: String::new(),
            name: "Socket import.pages".into(),
        };
        ensure!(
            crate::import_native_package(&runtime.join("absent.sock"), &request)
                .await
                .is_err(),
            "absent daemon accepted import"
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            while !socket.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        for expected in [String::new(), uuid::Uuid::new_v4().to_string()] {
            request.expected_account_id = Some(expected);
            let refused = crate::import_native_package(&socket, &request).await?;
            ensure!(
                refused.job.is_none() && refused.refusal.is_some(),
                "matching label bypassed selected account identity"
            );
            ensure!(
                engine.jobs.list().is_empty()
                    && writer.allocations.load(Ordering::SeqCst) == 0
                    && journal.lock().unwrap().list(0, 100)?.is_empty(),
                "identity refusal created a job or durable import"
            );
        }
        request.expected_account_id = None;
        request.label = "not-this-account".into();
        let refused = crate::import_native_package(&socket, &request).await?;
        ensure!(
            refused.job.is_none() && refused.refusal.is_some(),
            "foreign label admitted"
        );
        // A real read-only Account/Engine may expose the public action, but it
        // must fail admission before any durable upload or allocation.
        let mut readonly_account = engine.account.clone();
        readonly_account.id = uuid::Uuid::new_v4().to_string();
        readonly_account.label = "read-only-import".into();
        readonly_account.access = cirrove_auth::AccessMode::ReadOnly;
        readonly_account.mount_path = root.join("readonly-mount");
        std::fs::create_dir(&readonly_account.mount_path)?;
        let readonly = Engine::new(
            readonly_account.clone(),
            provider.clone(),
            root.join("state"),
        )
        .await?;
        manager
            .engines
            .write()
            .await
            .insert(readonly_account.id.clone(), readonly.clone());
        manager.status.write().await.push(AccountStatus {
            account_id: readonly_account.id.clone(),
            label: readonly_account.label.clone(),
            mount_path: readonly_account.mount_path.clone(),
            enabled: true,
            mounted: true,
            ..Default::default()
        });
        request.label.clear();
        ensure!(
            crate::import_native_package(&socket, &request)
                .await?
                .refusal
                .is_some(),
            "ambiguous account silently selected"
        );
        request.label = readonly_account.label.clone();
        let ro_job = crate::import_native_package(&socket, &request)
            .await?
            .job
            .context("read-only observation job unavailable")?;
        let ro_done = wait_job(&socket, &ro_job.id, terminal).await?;
        ensure!(
            ro_done.state == JobState::Failed && ro_done.native_import.is_none(),
            "read-only account acquired an upload operation"
        );
        ensure!(
            writer.allocations.load(Ordering::SeqCst) == 0
                && journal.lock().unwrap().list(0, 100)?.is_empty(),
            "read-only or foreign request performed upload work"
        );
        manager.engines.write().await.remove(&readonly_account.id);
        manager
            .status
            .write()
            .await
            .retain(|a| a.account_id != readonly_account.id);
        readonly.cancel.cancel();
        request.label = engine.account.label.clone();
        request.expected_account_id = Some(engine.account.id.clone());
        ensure!(
            engine.children(&scope, ROOT).await?.is_empty(),
            "fixture root not empty"
        );
        if mounted {
            let mount = engine.account.mount_path.clone();
            tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                ensure!(
                    std::fs::read_dir(mount)?.next().is_none(),
                    "warm mount not empty"
                );
                Ok(())
            })
            .await??;
        }
        let initial = crate::import_native_package(&socket, &request)
            .await?
            .job
            .context("public import refused")?;
        tokio::time::timeout(Duration::from_secs(10), writer.entered.notified()).await?;
        let queued = wait_job(&socket, &initial.id, |j| j.native_import.is_some()).await?;
        ensure!(
            queued.state == JobState::Running
                && queued.native_import.as_ref().unwrap().remote.is_none(),
            "queued was misreported as cloud completion"
        );
        let operation = queued.native_import.as_ref().unwrap().operation;
        for _ in 0..3 {
            let observed = public_job(&socket, &initial.id).await?;
            ensure!(
                observed.native_import.as_ref().unwrap().operation == operation,
                "observation rebound operation"
            );
        }
        ensure!(
            writer.allocations.load(Ordering::SeqCst) == 1,
            "observation replayed allocation"
        );
        writer.release.notify_one();
        let complete = wait_job(&socket, &initial.id, terminal).await?;
        ensure!(
            complete.state == JobState::Succeeded,
            "import did not reach verified/published success"
        );
        let receipt = complete
            .native_import
            .context("missing typed import receipt")?;
        ensure!(receipt.operation == operation, "wrong operation receipt");
        let remote = receipt.remote.context("missing remote receipt")?;
        ensure!(
            remote.id == format!("FILE::com.apple.CloudDocs::{operation}")
                && remote.package
                && remote.name == request.name,
            "wrong package receipt"
        );
        ensure!(
            remote.content_version.is_none(),
            "public completion must report canonical read projection"
        );
        let retained = journal.lock().unwrap().get(operation)?;
        let saved = retained.remote.context("missing retained legacy receipt")?;
        ensure!(
            saved.content_version == saved.etag && saved.content_version.is_some(),
            "observation rewrote the retained legacy receipt"
        );
        let visible = engine.children(&scope, ROOT).await?;
        ensure!(
            visible.iter().any(|n| n == &remote),
            "warm metadata did not receive confirmed package"
        );
        if mounted {
            let mount = engine.account.mount_path.clone();
            let name = request.name.clone();
            let path = mount.join(&name);
            tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                let names = std::fs::read_dir(&mount)?
                    .map(|entry| entry.map(|entry| entry.file_name()))
                    .collect::<std::io::Result<Vec<_>>>()?;
                ensure!(
                    names
                        .iter()
                        .any(|entry| entry == std::ffi::OsStr::new(&name)),
                    "warm mounted directory did not receive import"
                );
                ensure!(
                    std::fs::metadata(path)?.is_dir(),
                    "mounted package missing or flattened"
                );
                Ok(())
            })
            .await??;
        }
        // Stop only observation while a second durable upload is deliberately held.
        request.name = "Stopped observer.pages".into();
        request.expected_account_id = None; // Intentional CLI label lookup remains supported.
        let second = crate::import_native_package(&socket, &request)
            .await?
            .job
            .context("second import refused")?;
        tokio::time::timeout(Duration::from_secs(10), writer.entered.notified()).await?;
        let queued = wait_job(&socket, &second.id, |j| j.native_import.is_some()).await?;
        let second_operation = queued.native_import.unwrap().operation;
        let stopped = crate::stop_job(
            &socket,
            &crate::StopJobRequest {
                label: request.label.clone(),
                id: second.id.clone(),
            },
        )
        .await?;
        ensure!(stopped.stopped, "observer stop refused");
        let ended = wait_job(&socket, &second.id, terminal).await?;
        ensure!(
            ended.state == JobState::Stopped
                && ended.native_import.as_ref().unwrap().operation == second_operation,
            "stop discarded durable identity"
        );
        writer.release.notify_one();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if matches!(
                    manager
                        .native_import_publication(&engine, second_operation)
                        .await?,
                    crate::journal::PackagePublicationStatus::Present(_)
                ) {
                    return Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await??;
        ensure!(
            writer.allocations.load(Ordering::SeqCst) == 2
                && writer.bodies.load(Ordering::SeqCst) == 2,
            "stop/reobservation replayed upload"
        );
        ensure!(
            public_job(&socket, &second.id).await?.state == JobState::Stopped,
            "late upload rewrote accepted stop"
        );
        ensure!(
            journal.lock().unwrap().list(0, 100)?.len() == 2,
            "unexpected extra durable import"
        );
        ensure!(
            provider.reads.load(Ordering::SeqCst) == 0,
            "metadata rendering downloaded cloud content"
        );
        Ok(())
    }
    .await;
    writer.release.notify_one();
    cancel.cancel();
    let drained = tokio::time::timeout(Duration::from_secs(10), workers.drain()).await;
    if let Some(kernel) = kernel {
        tokio::task::spawn_blocking(move || kernel.umount_and_join()).await??;
    }
    let served = tokio::time::timeout(Duration::from_secs(5), server).await;
    outcome?;
    drained??;
    served???;
    Ok(())
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn public_socket_native_import_reaches_receipt_and_warm_metadata_without_replay()
-> anyhow::Result<()> {
    scenario(false).await
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires /dev/fuse; synthetic public import socket to mounted package metadata"]
async fn public_socket_native_import_reaches_real_fuse_package_metadata_without_replay()
-> anyhow::Result<()> {
    scenario(true).await
}

#[test]
fn legacy_native_import_request_preserves_intentional_label_lookup() {
    let request: crate::ImportNativePackageRequest = serde_json::from_value(serde_json::json!({
        "label":"chosen-label", "archive":"/owned/source.pages", "expected_root":"Source.pages", "parent":"", "name":"Copy.pages"
    })).unwrap();
    assert_eq!(request.expected_account_id, None);
    assert!(
        serde_json::to_value(&request)
            .unwrap()
            .get("expected_account_id")
            .is_none()
    );
}
