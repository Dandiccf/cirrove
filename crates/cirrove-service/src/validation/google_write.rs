//! Explicit Google create, replacement and namespace-precondition checks. They
//! use a disabled, separately consented account and are never called by the
//! daemon.
use super::*;
use crate::{
    journal::{MutationRecord, MutationState},
    mutations::MutationWorker,
};
use cirrove_core::{
    NodeKind, ReadProvider,
    mutation::{MutationIntent, MutationReceipt, MutationRequest},
};
use std::io::{BufRead, BufReader};

async fn apply_mutation(
    journal: &Arc<Mutex<UploadJournal>>,
    worker: &MutationWorker,
    request: MutationRequest,
    expected: MutationState,
    log: &mut File,
    stage: &str,
) -> Result<MutationRecord> {
    let shared = journal.clone();
    let id = tokio::task::spawn_blocking(move || -> Result<_> {
        Ok(shared
            .lock()
            .map_err(|_| anyhow::anyhow!("validation journal unavailable"))?
            .enqueue_mutation(request)?
            .id)
    })
    .await??;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
    loop {
        if let Some(result) = worker.run_once().await? {
            println!("  namespace change: {:?}", result.state);
            if let Some(issue) = result.issue {
                println!("  {issue}");
            }
        }
        let record = journal
            .lock()
            .map_err(|_| anyhow::anyhow!("validation journal unavailable"))?
            .mutation(id)?;
        if matches!(
            record.state,
            MutationState::Applied
                | MutationState::Conflict
                | MutationState::Failed
                | MutationState::NeedsReview
        ) {
            event(
                log,
                serde_json::json!({
                    "stage":stage,
                    "operation":record.id,
                    "state":record.state
                }),
            )?;
            if record.state != expected {
                bail!(
                    "Google namespace check did not reach its expected state; retained operation requires review"
                );
            }
            return Ok(record);
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("Google namespace validation deadline reached; local evidence is retained");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn verify_created(
    provider: &dyn ReadProvider,
    record: &UploadRecord,
    cancel: &CancellationToken,
) -> Result<()> {
    if record.state != UploadState::Uploaded {
        bail!("Google create was not acknowledged; local bytes are retained");
    }
    let receipt = record
        .remote
        .as_ref()
        .context("Google create has no exact provider receipt")?;
    for attempt in 0..8 {
        let node = provider
            .node(&record.scope, &receipt.id, cancel)
            .await
            .context("created Google file cannot be read back by its exact identity")?;
        if node.id != receipt.id || node.size != record.size {
            bail!("created Google file identity or size changed during readback");
        }
        let mut digest = Sha256::new();
        let mut offset = 0;
        let mut changed = false;
        while offset < node.size {
            let length = (node.size - offset).min(4 * 1024 * 1024) as u32;
            let bytes = match provider
                .read_range(&record.scope, &node, offset, length, cancel)
                .await
            {
                Ok(bytes) => bytes,
                Err(cirrove_core::ProviderError::VersionChanged) => {
                    changed = true;
                    break;
                }
                Err(error) => {
                    return Err(error).context("created Google content could not be read back");
                }
            };
            if bytes.len() != length as usize {
                bail!("created Google content readback was incomplete");
            }
            digest.update(&bytes);
            offset += bytes.len() as u64;
        }
        if changed {
            if attempt == 7 {
                bail!("created Google content version did not settle for readback");
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
            continue;
        }
        if hex::encode(digest.finalize()) != record.sha256 {
            bail!("created Google content did not match its durable local snapshot");
        }
        return Ok(());
    }
    unreachable!("bounded readback loop always returns or fails")
}

async fn exact_sha256(
    provider: &dyn ReadProvider,
    scope: &Scope,
    node: &Node,
    cancel: &CancellationToken,
) -> std::result::Result<String, cirrove_core::ProviderError> {
    let mut digest = Sha256::new();
    let mut offset = 0;
    while offset < node.size {
        let length = (node.size - offset).min(4 * 1024 * 1024) as u32;
        let bytes = provider
            .read_range(scope, node, offset, length, cancel)
            .await?;
        if bytes.len() != length as usize {
            return Err(cirrove_core::ProviderError::Protocol(
                "Google validation readback was incomplete",
            ));
        }
        digest.update(&bytes);
        offset += bytes.len() as u64;
    }
    Ok(hex::encode(digest.finalize()))
}

/// Create an isolated folder tree, a multipart file and an empty file, then
/// characterize stale HTTP ETag handling on run-owned files and folders. The
/// tree and local evidence remain for review; no pre-existing item is changed.
pub async fn google_create(state: &Path, label: &str) -> Result<()> {
    let account = test_account(state, label)?;
    if !matches!(
        account.registration,
        cirrove_auth::AppRegistration::Google { .. }
    ) {
        bail!("this operation requires a Google Drive connection");
    }
    let _operation = accounts::account_operation(state, &account.id)?;
    let _owner = accounts::account_lock(&state.join("accounts").join(&account.id))?;
    let run = uuid::Uuid::new_v4();
    let directory = state.join("write-checks").join(run.to_string());
    crate::private_dir(&directory)?;
    let mut log = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(directory.join("events.jsonl"))?;
    let scope = Scope {
        account: account.id.clone(),
        provider: cirrove_googledrive::PROVIDER_ID.into(),
        collection: account.drive.id.clone(),
    };
    let name = format!("Cirrove-Google-Create-Validation-{run}");
    event(
        &mut log,
        serde_json::json!({"stage":"planned", "folder":name, "scope":scope}),
    )?;
    File::open(&directory)?.sync_all()?;
    File::open(
        directory
            .parent()
            .context("validation directory has no parent")?,
    )?
    .sync_all()?;
    println!("Checking Google writes only in the new folder {name}.");
    println!("Private local evidence: {}", directory.display());

    let google = accounts::google_provider(&account)?;
    let cancel = CancellationToken::new();
    let journal_path = directory.join("journal");
    let account_id = account.id.clone();
    let journal = Arc::new(Mutex::new(
        tokio::task::spawn_blocking(move || {
            UploadJournal::open(&journal_path, &account_id, 64 * 1024 * 1024)
        })
        .await??,
    ));
    let mutation_worker = MutationWorker::new(
        journal.clone(),
        Arc::new(google.validation_mutations()),
        cancel.clone(),
    );
    println!("Checking Google folder creation through the provider-neutral mutation worker.");
    let folder_record = apply_mutation(
        &journal,
        &mutation_worker,
        MutationRequest {
            scope: scope.clone(),
            intent: MutationIntent::CreateFolder {
                parent: account.root_id.clone(),
                name: name.clone(),
            },
        },
        MutationState::Applied,
        &mut log,
        "folder_created",
    )
    .await?;
    let Some(MutationReceipt::Upsert(folder)) = folder_record.receipt else {
        bail!("Google worker folder create has no exact provider receipt");
    };
    if folder_record.prepared_item.as_ref() != Some(&folder.id)
        || folder.parent_id.as_ref() != Some(&account.root_id)
        || folder.name != name
        || folder.kind != NodeKind::Folder
    {
        bail!("Google worker folder create returned a different item or destination");
    }
    let inspected = google
        .node(&scope, &folder.id, &cancel)
        .await
        .context("created Google validation folder is absent")?;
    if inspected.id != folder.id
        || inspected.parent_id != folder.parent_id
        || inspected.kind != NodeKind::Folder
    {
        bail!("Google validation folder identity changed after creation");
    }

    let worker = TransferWorker::new(
        journal.clone(),
        google.clone(),
        Arc::new(DesktopVault),
        cancel.clone(),
    );
    let create = |name: &str| UploadIntent::Create {
        parent: folder.id.clone(),
        name: name.into(),
    };
    let multipart_name = "Kärnten & Grüße #1.bin";

    println!("Checking a multipart Google create and independent exact-ID readback.");
    let multipart = transfer(
        &journal,
        &worker,
        scope.clone(),
        create(multipart_name),
        vec![0x47; 8 * 1024 * 1024 + 13],
    )
    .await?;
    event(
        &mut log,
        serde_json::json!({"stage":"multipart_result", "operation":multipart}),
    )?;
    verify_created(google.as_ref(), &multipart, &cancel).await?;
    event(
        &mut log,
        serde_json::json!({"stage":"multipart_readback_passed"}),
    )?;

    println!("Checking an empty Google file with its prepared identity.");
    let empty = transfer(
        &journal,
        &worker,
        scope.clone(),
        create("Empty.txt"),
        Vec::new(),
    )
    .await?;
    event(
        &mut log,
        serde_json::json!({"stage":"empty_result", "operation":empty}),
    )?;
    verify_created(google.as_ref(), &empty, &cancel).await?;
    event(
        &mut log,
        serde_json::json!({"stage":"empty_readback_passed"}),
    )?;

    let multipart_id = multipart
        .remote
        .as_ref()
        .context("multipart Google create has no exact provider receipt")?
        .id
        .clone();
    let accepted_name = "Kärnten & Grüße #1-renamed.bin";
    let stale_name = "Kärnten & Grüße #1-stale.bin";
    let precondition_plan = google.prepare_metadata_precondition_probe(
        &scope,
        &multipart_id,
        &folder.id,
        multipart_name,
        accepted_name,
        stale_name,
    )?;
    event(
        &mut log,
        serde_json::json!({
            "stage":"metadata_precondition_planned",
            "plan":precondition_plan
        }),
    )?;
    println!("Checking whether Google rejects a stale HTTP ETag on its own test file.");
    let precondition = google
        .probe_metadata_precondition(&scope, &precondition_plan, &cancel)
        .await?;
    event(
        &mut log,
        serde_json::json!({
            "stage":"metadata_precondition_result",
            "result":precondition
        }),
    )?;
    match precondition.stale_rejected() {
        Some(true) => println!("Google rejected the stale metadata precondition."),
        Some(false) => bail!(
            "Google accepted a stale metadata precondition; safe shared mutation remains unavailable"
        ),
        None => bail!(
            "Google returned no strong metadata ETag; safe shared mutation remains unavailable"
        ),
    }

    let accepted_content = vec![0x41; 4 * 1024 + 7];
    let stale_content = vec![0x42; 8 * 1024 + 13];
    let content_plan = google.prepare_content_precondition_probe(
        &scope,
        &multipart_id,
        &folder.id,
        accepted_name,
        &accepted_content,
        &stale_content,
    )?;
    event(
        &mut log,
        serde_json::json!({
            "stage":"content_precondition_planned",
            "plan":content_plan
        }),
    )?;
    println!("Checking whether Google rejects a stale HTTP ETag on content replacement.");
    let content_precondition = google
        .probe_content_precondition(
            &scope,
            &content_plan,
            accepted_content,
            stale_content,
            &cancel,
        )
        .await?;
    event(
        &mut log,
        serde_json::json!({
            "stage":"content_precondition_result",
            "result":content_precondition
        }),
    )?;
    match content_precondition.stale_rejected() {
        Some(true) => println!("Google rejected the stale content precondition."),
        Some(false) => bail!(
            "Google accepted a stale content precondition; safe replacement remains unavailable"
        ),
        None => {
            bail!("Google returned no strong content ETag; safe replacement remains unavailable")
        }
    }

    let resumable_content = vec![0x52; 8 * 1024 * 1024 + 29];
    let stale_resumable_content = vec![0x53; 8 * 1024 * 1024 + 31];
    let resumable_plan = google.prepare_resumable_precondition_probe(
        &scope,
        &multipart_id,
        &folder.id,
        accepted_name,
        &resumable_content,
        &stale_resumable_content,
    )?;
    event(
        &mut log,
        serde_json::json!({
            "stage":"resumable_precondition_planned",
            "plan":resumable_plan
        }),
    )?;
    println!("Checking the same stale ETag rule on a resumable content replacement.");
    let resumable_precondition = google
        .probe_resumable_precondition(
            &scope,
            &resumable_plan,
            resumable_content,
            stale_resumable_content,
            &cancel,
        )
        .await?;
    event(
        &mut log,
        serde_json::json!({
            "stage":"resumable_precondition_result",
            "result":resumable_precondition
        }),
    )?;
    match resumable_precondition.stale_rejected() {
        Some(true) => println!("Google rejected the stale resumable precondition."),
        Some(false) => bail!(
            "Google accepted a stale resumable precondition; safe replacement remains unavailable"
        ),
        None => bail!(
            "Google returned no strong ETag for resumable replacement; safe replacement remains unavailable"
        ),
    }

    let replacement_base = google
        .validation_replacement_base(&scope, &multipart_id, &folder.id, accepted_name, &cancel)
        .await?;
    let expected_etag = replacement_base
        .etag
        .clone()
        .context("Google returned no strong ETag for the shared-worker replacement")?;
    let worker_content = vec![0x57; 8 * 1024 * 1024 + 47];
    event(
        &mut log,
        serde_json::json!({
            "stage":"worker_replacement_planned",
            "item":multipart_id,
            "size":worker_content.len(),
            "sha256":hex::encode(Sha256::digest(&worker_content))
        }),
    )?;
    println!("Checking Google replacement through the provider-neutral durable worker.");
    let worker_replacement = transfer(
        &journal,
        &worker,
        scope.clone(),
        UploadIntent::Replace {
            item: multipart_id.clone(),
            expected_etag: expected_etag.clone(),
        },
        worker_content,
    )
    .await?;
    event(
        &mut log,
        serde_json::json!({
            "stage":"worker_replacement_result",
            "operation":worker_replacement.id,
            "state":worker_replacement.state,
            "size":worker_replacement.size,
            "sha256":worker_replacement.sha256
        }),
    )?;
    verify_created(google.as_ref(), &worker_replacement, &cancel).await?;

    let mutation_base = google
        .validation_replacement_base(&scope, &multipart_id, &folder.id, accepted_name, &cancel)
        .await?;
    let worker_name = "Kärnten & Grüße #1-worker-renamed.bin";
    event(
        &mut log,
        serde_json::json!({
            "stage":"worker_relocation_planned",
            "item":multipart_id,
            "parent":folder.id,
            "name":worker_name
        }),
    )?;
    println!("Checking Google rename through the provider-neutral mutation worker.");
    let relocate = |before: Node, name: &str| MutationRequest {
        scope: scope.clone(),
        intent: MutationIntent::Relocate {
            before,
            parent: folder.id.clone(),
            name: name.into(),
        },
    };
    let moved = apply_mutation(
        &journal,
        &mutation_worker,
        relocate(mutation_base.clone(), worker_name),
        MutationState::Applied,
        &mut log,
        "worker_relocation_result",
    )
    .await?;
    let Some(MutationReceipt::Upsert(moved_node)) = moved.receipt else {
        bail!("Google worker rename has no exact provider receipt");
    };
    if moved_node.id != multipart_id
        || moved_node.parent_id.as_ref() != Some(&folder.id)
        || moved_node.name != worker_name
    {
        bail!("Google worker rename returned a different item or destination");
    }
    let inspected_move = google
        .validation_replacement_base(&scope, &multipart_id, &folder.id, worker_name, &cancel)
        .await?;
    if inspected_move.etag == mutation_base.etag {
        bail!("Google worker rename did not advance the strong ETag");
    }

    println!("Checking nested Google folder creation through the same mutation worker.");
    let destination_record = apply_mutation(
        &journal,
        &mutation_worker,
        MutationRequest {
            scope: scope.clone(),
            intent: MutationIntent::CreateFolder {
                parent: folder.id.clone(),
                name: "Destination".into(),
            },
        },
        MutationState::Applied,
        &mut log,
        "move_destination_created",
    )
    .await?;
    let Some(MutationReceipt::Upsert(destination)) = destination_record.receipt else {
        bail!("Google worker destination create has no exact provider receipt");
    };
    if destination_record.prepared_item.as_ref() != Some(&destination.id)
        || destination.parent_id.as_ref() != Some(&folder.id)
        || destination.name != "Destination"
        || destination.kind != NodeKind::Folder
    {
        bail!("Google worker destination create returned a different item or destination");
    }
    let moved_name = "Kärnten & Grüße #1-worker-moved.bin";
    println!("Checking Google move through the same provider-neutral mutation worker.");
    let moved_again = apply_mutation(
        &journal,
        &mutation_worker,
        MutationRequest {
            scope: scope.clone(),
            intent: MutationIntent::Relocate {
                before: inspected_move.clone(),
                parent: destination.id.clone(),
                name: moved_name.into(),
            },
        },
        MutationState::Applied,
        &mut log,
        "worker_move_result",
    )
    .await?;
    let Some(MutationReceipt::Upsert(moved_again_node)) = moved_again.receipt else {
        bail!("Google worker move has no exact provider receipt");
    };
    if moved_again_node.id != multipart_id
        || moved_again_node.parent_id.as_ref() != Some(&destination.id)
        || moved_again_node.name != moved_name
    {
        bail!("Google worker move returned a different item or destination");
    }
    google
        .validation_replacement_base(&scope, &multipart_id, &destination.id, moved_name, &cancel)
        .await?;
    apply_mutation(
        &journal,
        &mutation_worker,
        relocate(inspected_move, "Kärnten & Grüße #1-worker-stale.bin"),
        MutationState::Conflict,
        &mut log,
        "worker_stale_relocation_result",
    )
    .await?;

    println!("Checking a parent folder move while its created file remains addressable.");
    let archive_record = apply_mutation(
        &journal,
        &mutation_worker,
        MutationRequest {
            scope: scope.clone(),
            intent: MutationIntent::CreateFolder {
                parent: folder.id.clone(),
                name: "Archive".into(),
            },
        },
        MutationState::Applied,
        &mut log,
        "folder_move_parent_created",
    )
    .await?;
    let Some(MutationReceipt::Upsert(archive)) = archive_record.receipt else {
        bail!("Google worker folder-move parent has no exact provider receipt");
    };
    if archive_record.prepared_item.as_ref() != Some(&archive.id)
        || archive.parent_id.as_ref() != Some(&folder.id)
        || archive.name != "Archive"
        || archive.kind != NodeKind::Folder
    {
        bail!("Google worker folder-move parent returned a different item or destination");
    }
    let destination_base = google
        .validation_namespace_base(
            &scope,
            &destination.id,
            &folder.id,
            "Destination",
            NodeKind::Folder,
            &cancel,
        )
        .await?;
    let moved_folder_name = "Destination-Moved";
    let moved_folder = apply_mutation(
        &journal,
        &mutation_worker,
        MutationRequest {
            scope: scope.clone(),
            intent: MutationIntent::Relocate {
                before: destination_base.clone(),
                parent: archive.id.clone(),
                name: moved_folder_name.into(),
            },
        },
        MutationState::Applied,
        &mut log,
        "worker_folder_move_result",
    )
    .await?;
    let Some(MutationReceipt::Upsert(moved_folder_node)) = moved_folder.receipt else {
        bail!("Google worker folder move has no exact provider receipt");
    };
    if moved_folder_node.id != destination.id
        || moved_folder_node.parent_id.as_ref() != Some(&archive.id)
        || moved_folder_node.name != moved_folder_name
        || moved_folder_node.kind != NodeKind::Folder
    {
        bail!("Google worker folder move returned a different item or destination");
    }
    google
        .validation_namespace_base(
            &scope,
            &destination.id,
            &archive.id,
            moved_folder_name,
            NodeKind::Folder,
            &cancel,
        )
        .await?;
    google
        .validation_replacement_base(&scope, &multipart_id, &destination.id, moved_name, &cancel)
        .await?;
    apply_mutation(
        &journal,
        &mutation_worker,
        MutationRequest {
            scope: scope.clone(),
            intent: MutationIntent::Relocate {
                before: destination_base,
                parent: folder.id.clone(),
                name: "Destination-Stale".into(),
            },
        },
        MutationState::Conflict,
        &mut log,
        "worker_stale_folder_relocation_result",
    )
    .await?;

    let stale_worker = transfer(
        &journal,
        &worker,
        scope.clone(),
        UploadIntent::Replace {
            item: multipart_id.clone(),
            expected_etag,
        },
        vec![0x58; 8 * 1024 * 1024 + 59],
    )
    .await?;
    event(
        &mut log,
        serde_json::json!({
            "stage":"worker_stale_replacement_result",
            "operation":stale_worker.id,
            "state":stale_worker.state,
            "size":stale_worker.size,
            "sha256":stale_worker.sha256
        }),
    )?;
    if stale_worker.state != UploadState::Conflict {
        bail!("the provider-neutral worker did not preserve the stale Google conflict");
    }
    event(
        &mut log,
        serde_json::json!({
            "stage":"google_write_checks_passed",
            "fixture_retained":true
        }),
    )?;
    println!(
        "Google create and metadata checks passed. The test folder and local snapshots remain for review."
    );
    println!("This does not enable writable Google mounts.");
    Ok(())
}

/// Probe Drive v2's documented file ETag on the exact multipart file retained
/// by an earlier Google create-validation run. The file is renamed twice under
/// preconditions and restored before success is reported.
pub async fn google_v2_etag(state: &Path, label: &str, run: uuid::Uuid) -> Result<()> {
    let account = test_account(state, label)?;
    if !matches!(
        account.registration,
        cirrove_auth::AppRegistration::Google { .. }
    ) {
        bail!("this operation requires a Google Drive connection");
    }
    let _operation = accounts::account_operation(state, &account.id)?;
    let _owner = accounts::account_lock(&state.join("accounts").join(&account.id))?;
    let run_directory = state.join("write-checks").join(run.to_string());
    let evidence = File::open(run_directory.join("events.jsonl"))
        .context("Google create-validation evidence is unavailable")?;
    let mut multipart = None;
    for line in BufReader::new(evidence).lines() {
        let value: serde_json::Value = serde_json::from_str(&line?)?;
        if value.get("stage").and_then(|stage| stage.as_str()) == Some("multipart_result") {
            multipart = Some(serde_json::from_value::<UploadRecord>(
                value
                    .get("operation")
                    .cloned()
                    .context("multipart evidence has no operation")?,
            )?);
        }
    }
    let multipart = multipart.context("run has no completed multipart create")?;
    let UploadIntent::Create { parent, name } = &multipart.intent else {
        bail!("retained Google operation is not a create");
    };
    let remote = multipart
        .remote
        .as_ref()
        .context("retained Google create has no exact provider receipt")?;
    if multipart.state != UploadState::Uploaded
        || multipart.scope.account != account.id
        || multipart.scope.provider != cirrove_googledrive::PROVIDER_ID
        || multipart.scope.collection != account.drive.id
        || remote.parent_id.as_ref() != Some(parent)
        || remote.name != *name
    {
        bail!("retained Google create does not belong to this validation account and run");
    }

    let probe = uuid::Uuid::new_v4();
    let directory = run_directory.join(format!("v2-etag-{probe}"));
    crate::private_dir(&directory)?;
    let mut log = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(directory.join("events.jsonl"))?;
    let current_name = format!("Kärnten & Grüße #1-v2-current-{probe}.bin");
    let stale_name = format!("Kärnten & Grüße #1-v2-stale-{probe}.bin");
    let google = accounts::google_provider(&account)?;
    let plan = google.prepare_metadata_precondition_probe(
        &multipart.scope,
        &remote.id,
        parent,
        name,
        &current_name,
        &stale_name,
    )?;
    event(
        &mut log,
        serde_json::json!({
            "stage":"planned",
            "source_run":run,
            "probe":probe,
            "plan":plan,
            "decision_rule":"current v2 ETag must permit one rename; reuse of the stale ETag must return 412; the current ETag must restore the original name"
        }),
    )?;
    File::open(&directory)?.sync_all()?;
    println!("Checking Drive v2 ETag behavior on the retained Cirrove test file.");
    println!("Private local evidence: {}", directory.display());
    let result = google
        .probe_v2_metadata_precondition(&multipart.scope, &plan, &CancellationToken::new())
        .await?;
    event(
        &mut log,
        serde_json::json!({"stage":"v2_metadata_precondition_result", "result":result}),
    )?;
    match result.stale_rejected() {
        Some(true) => {
            println!("Drive v2 rejected the stale ETag and restored the original name.");
            Ok(())
        }
        Some(false) => {
            bail!("Drive v2 accepted a stale ETag; safe replacement remains unavailable")
        }
        None => {
            bail!("Drive v2 returned no strong file ETag; safe replacement remains unavailable")
        }
    }
}

/// Probe Drive v2's resumable replacement precondition on the same retained
/// run-owned file, including a concurrent metadata change after session start.
/// Both the original name and deterministic original bytes are restored before
/// success is reported.
pub async fn google_v2_content(state: &Path, label: &str, run: uuid::Uuid) -> Result<()> {
    let account = test_account(state, label)?;
    if !matches!(
        account.registration,
        cirrove_auth::AppRegistration::Google { .. }
    ) {
        bail!("this operation requires a Google Drive connection");
    }
    let _operation = accounts::account_operation(state, &account.id)?;
    let _owner = accounts::account_lock(&state.join("accounts").join(&account.id))?;
    let run_directory = state.join("write-checks").join(run.to_string());
    let evidence = File::open(run_directory.join("events.jsonl"))
        .context("Google create-validation evidence is unavailable")?;
    let mut multipart = None;
    for line in BufReader::new(evidence).lines() {
        let value: serde_json::Value = serde_json::from_str(&line?)?;
        if value.get("stage").and_then(|stage| stage.as_str()) == Some("multipart_result") {
            multipart = Some(serde_json::from_value::<UploadRecord>(
                value
                    .get("operation")
                    .cloned()
                    .context("multipart evidence has no operation")?,
            )?);
        }
    }
    let multipart = multipart.context("run has no completed multipart create")?;
    let UploadIntent::Create { parent, name } = &multipart.intent else {
        bail!("retained Google operation is not a create");
    };
    let remote = multipart
        .remote
        .as_ref()
        .context("retained Google create has no exact provider receipt")?;
    if multipart.state != UploadState::Uploaded
        || multipart.scope.account != account.id
        || multipart.scope.provider != cirrove_googledrive::PROVIDER_ID
        || multipart.scope.collection != account.drive.id
        || remote.parent_id.as_ref() != Some(parent)
        || remote.name != *name
        || multipart.size != 8 * 1024 * 1024 + 13
        || multipart.sha256 != hex::encode(Sha256::digest(vec![0x47; 8 * 1024 * 1024 + 13]))
    {
        bail!("retained Google create does not match the deterministic validation file");
    }

    let probe = uuid::Uuid::new_v4();
    let directory = run_directory.join(format!("v2-content-{probe}"));
    crate::private_dir(&directory)?;
    let mut log = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(directory.join("events.jsonl"))?;
    let original = vec![0x47; 8 * 1024 * 1024 + 13];
    let accepted = vec![0x52; 8 * 1024 * 1024 + 29];
    event(
        &mut log,
        serde_json::json!({
            "stage":"planned",
            "source_run":run,
            "probe":probe,
            "item":remote.id,
            "original_size":original.len(),
            "original_sha256":hex::encode(Sha256::digest(&original)),
            "accepted_size":accepted.len(),
            "accepted_sha256":hex::encode(Sha256::digest(&accepted)),
            "decision_rule":"the final range of a session opened before an intervening metadata change must fail; a current session must replace content; a stale session start must fail; original name and bytes must be restored"
        }),
    )?;
    File::open(&directory)?.sync_all()?;
    println!("Checking Drive v2 resumable preconditions on the retained Cirrove test file.");
    println!("Private local evidence: {}", directory.display());
    let google = accounts::google_provider(&account)?;
    let journal_path = directory.join("journal");
    let account_id = account.id.clone();
    let journal = Arc::new(Mutex::new(
        tokio::task::spawn_blocking(move || {
            UploadJournal::open(&journal_path, &account_id, 64 * 1024 * 1024)
        })
        .await??,
    ));
    let cancel = CancellationToken::new();
    let transfer_worker = TransferWorker::new(
        journal.clone(),
        google.clone(),
        Arc::new(DesktopVault),
        cancel.clone(),
    );
    let mutation_worker = MutationWorker::new(journal.clone(), google.clone(), cancel.clone());

    // A previous run can lose its credential after acknowledging the test
    // replacement but before restoring the fixture. Recover only the exact
    // deterministic worker payload; any other bytes remain untouched for review.
    let worker_size = 8 * 1024 * 1024 + 47;
    let worker_sha256 = hex::encode(Sha256::digest(vec![0x57; worker_size]));
    let mut current = google
        .validation_versioned_item(
            &multipart.scope,
            &remote.id,
            parent,
            NodeKind::File,
            &cancel,
        )
        .await?;
    if current.name != *name {
        let prior_probe = current
            .name
            .strip_prefix("Kärnten & Grüße #1-v2-worker-")
            .and_then(|value| value.strip_suffix(".bin"))
            .and_then(|value| uuid::Uuid::parse_str(value).ok())
            .context("retained Google fixture has an unexpected name; refusing recovery")?;
        let prior_evidence = File::open(
            run_directory
                .join(format!("v2-content-{prior_probe}"))
                .join("events.jsonl"),
        )
        .context("interrupted rename has no local evidence; refusing recovery")?;
        let mut planned_for_item = false;
        let mut rename_applied = false;
        for line in BufReader::new(prior_evidence).lines() {
            let value: serde_json::Value = serde_json::from_str(&line?)?;
            match value.get("stage").and_then(|stage| stage.as_str()) {
                Some("planned") => {
                    planned_for_item = value.get("item").and_then(|item| item.as_str())
                        == Some(remote.id.as_str());
                }
                Some("versioned_worker_rename_result") => {
                    rename_applied = matches!(
                        value.get("state").and_then(|state| state.as_str()),
                        Some("applied" | "conflict")
                    );
                }
                _ => {}
            }
        }
        if !planned_for_item || !rename_applied {
            bail!(
                "interrupted rename evidence does not match the exact fixture; refusing recovery"
            );
        }
        apply_mutation(
            &journal,
            &mutation_worker,
            MutationRequest {
                scope: multipart.scope.clone(),
                intent: MutationIntent::Relocate {
                    before: current,
                    parent: parent.clone(),
                    name: name.clone(),
                },
            },
            MutationState::Applied,
            &mut log,
            "interrupted_worker_name_restore_result",
        )
        .await?;
        println!("Restored the run-owned fixture name left by the interrupted worker run.");
        current = google
            .validation_versioned_namespace_base(
                &multipart.scope,
                &remote.id,
                parent,
                name,
                NodeKind::File,
                &cancel,
            )
            .await?;
    }
    if current.size != original.len() as u64 {
        if current.size != worker_size as u64 {
            bail!("retained Google fixture has unexpected content; refusing recovery");
        }
        let mut observed = None;
        for attempt in 0..8 {
            match exact_sha256(google.as_ref(), &multipart.scope, &current, &cancel).await {
                Ok(digest) => {
                    observed = Some(digest);
                    break;
                }
                Err(cirrove_core::ProviderError::VersionChanged) if attempt < 7 => {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    current = google
                        .validation_versioned_namespace_base(
                            &multipart.scope,
                            &remote.id,
                            parent,
                            name,
                            NodeKind::File,
                            &cancel,
                        )
                        .await?;
                }
                Err(error) => return Err(error.into()),
            }
        }
        if observed.as_deref() != Some(worker_sha256.as_str()) {
            bail!(
                "retained Google fixture is not the interrupted worker payload; refusing recovery"
            );
        }
        event(
            &mut log,
            serde_json::json!({
                "stage":"interrupted_worker_restore_planned",
                "item":remote.id,
                "observed_size":current.size,
                "observed_sha256":worker_sha256
            }),
        )?;
        let recovery = transfer(
            &journal,
            &transfer_worker,
            multipart.scope.clone(),
            UploadIntent::Replace {
                item: remote.id.clone(),
                expected_etag: current
                    .etag
                    .clone()
                    .context("interrupted Google worker has no recovery precondition")?,
            },
            original.clone(),
        )
        .await?;
        verify_created(google.as_ref(), &recovery, &cancel).await?;
        event(
            &mut log,
            serde_json::json!({
                "stage":"interrupted_worker_restore_result",
                "operation":recovery.id,
                "state":recovery.state,
                "remote":recovery.remote
            }),
        )?;
        println!("Restored the deterministic fixture left by the interrupted worker run.");
    }
    let plan = google.prepare_v2_content_precondition_probe(
        &multipart.scope,
        &remote.id,
        parent,
        name,
        &original,
        &accepted,
    )?;
    let result = google
        .probe_v2_content_precondition(&multipart.scope, &plan, original, accepted, &cancel)
        .await?;
    event(
        &mut log,
        serde_json::json!({"stage":"v2_content_precondition_result", "result":result}),
    )?;
    if !result.intervening_change_rejected_at_commit
        || !result.stale_session_rejected
        || !result.original_name_restored
        || !result.original_content_restored
    {
        bail!("Drive v2 resumable replacement did not satisfy the writable-mount contract")
    }
    println!("Drive v2 protected the final upload commit and restored the original file.");

    let current = google
        .validation_versioned_namespace_base(
            &multipart.scope,
            &remote.id,
            parent,
            name,
            NodeKind::File,
            &cancel,
        )
        .await?;
    let replacement = vec![0x57; 8 * 1024 * 1024 + 47];
    event(
        &mut log,
        serde_json::json!({
            "stage":"versioned_worker_replacement_planned",
            "item":remote.id,
            "size":replacement.len(),
            "sha256":hex::encode(Sha256::digest(&replacement))
        }),
    )?;
    let replaced = transfer(
        &journal,
        &transfer_worker,
        multipart.scope.clone(),
        UploadIntent::Replace {
            item: remote.id.clone(),
            expected_etag: current
                .etag
                .clone()
                .context("versioned Google replacement has no precondition")?,
        },
        replacement,
    )
    .await?;
    verify_created(google.as_ref(), &replaced, &cancel).await?;
    event(
        &mut log,
        serde_json::json!({
            "stage":"versioned_worker_replacement_result",
            "operation":replaced.id,
            "state":replaced.state,
            "remote":replaced.remote
        }),
    )?;

    let replaced_base = google
        .validation_versioned_namespace_base(
            &multipart.scope,
            &remote.id,
            parent,
            name,
            NodeKind::File,
            &cancel,
        )
        .await?;
    let restored = transfer(
        &journal,
        &transfer_worker,
        multipart.scope.clone(),
        UploadIntent::Replace {
            item: remote.id.clone(),
            expected_etag: replaced_base
                .etag
                .clone()
                .context("versioned Google restore has no precondition")?,
        },
        vec![0x47; 8 * 1024 * 1024 + 13],
    )
    .await?;
    verify_created(google.as_ref(), &restored, &cancel).await?;
    event(
        &mut log,
        serde_json::json!({
            "stage":"versioned_worker_restore_result",
            "operation":restored.id,
            "state":restored.state,
            "remote":restored.remote
        }),
    )?;

    let rename_base = google
        .validation_versioned_namespace_base(
            &multipart.scope,
            &remote.id,
            parent,
            name,
            NodeKind::File,
            &cancel,
        )
        .await?;
    let worker_name = format!("Kärnten & Grüße #1-v2-worker-{probe}.bin");
    let renamed = apply_mutation(
        &journal,
        &mutation_worker,
        MutationRequest {
            scope: multipart.scope.clone(),
            intent: MutationIntent::Relocate {
                before: rename_base.clone(),
                parent: parent.clone(),
                name: worker_name.clone(),
            },
        },
        MutationState::Applied,
        &mut log,
        "versioned_worker_rename_result",
    )
    .await?;
    let Some(MutationReceipt::Upsert(renamed_node)) = renamed.receipt else {
        bail!("versioned Google worker rename has no exact receipt");
    };
    if renamed_node.id != remote.id || renamed_node.name != worker_name {
        bail!("versioned Google worker renamed a different item");
    }
    let renamed_base = google
        .validation_versioned_namespace_base(
            &multipart.scope,
            &remote.id,
            parent,
            &worker_name,
            NodeKind::File,
            &cancel,
        )
        .await?;
    apply_mutation(
        &journal,
        &mutation_worker,
        MutationRequest {
            scope: multipart.scope.clone(),
            intent: MutationIntent::Relocate {
                before: renamed_base,
                parent: parent.clone(),
                name: name.clone(),
            },
        },
        MutationState::Applied,
        &mut log,
        "versioned_worker_name_restore_result",
    )
    .await?;
    apply_mutation(
        &journal,
        &mutation_worker,
        MutationRequest {
            scope: multipart.scope.clone(),
            intent: MutationIntent::Relocate {
                before: rename_base,
                parent: parent.clone(),
                name: format!("Kärnten & Grüße #1-v2-stale-{probe}.bin"),
            },
        },
        MutationState::Conflict,
        &mut log,
        "versioned_worker_stale_rename_result",
    )
    .await?;
    event(
        &mut log,
        serde_json::json!({
            "stage":"versioned_worker_checks_passed",
            "original_name_restored":true,
            "original_content_restored":true
        }),
    )?;
    println!("The durable transfer and mutation workers also passed and restored the fixture.");
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use cirrove_core::{ChangePage, Cursor, DirectoryPage, MetadataProvider, ProviderError};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct SettlingRead {
        bytes: Vec<u8>,
        node: cirrove_core::Node,
        reads: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl MetadataProvider for SettlingRead {
        fn provider_id(&self) -> &'static str {
            "settling-fixture"
        }

        async fn changes(
            &self,
            _scope: &Scope,
            _cursor: Option<&Cursor>,
            _cancel: &CancellationToken,
        ) -> std::result::Result<ChangePage, ProviderError> {
            Err(ProviderError::Protocol("unused fixture operation"))
        }
    }

    #[async_trait::async_trait]
    impl ReadProvider for SettlingRead {
        async fn node(
            &self,
            _scope: &Scope,
            _id: &str,
            _cancel: &CancellationToken,
        ) -> std::result::Result<cirrove_core::Node, ProviderError> {
            Ok(self.node.clone())
        }

        async fn children(
            &self,
            _scope: &Scope,
            _parent: &str,
            _cursor: Option<&Cursor>,
            _cancel: &CancellationToken,
        ) -> std::result::Result<DirectoryPage, ProviderError> {
            Err(ProviderError::Protocol("unused fixture operation"))
        }

        async fn read_range(
            &self,
            _scope: &Scope,
            _node: &cirrove_core::Node,
            offset: u64,
            length: u32,
            _cancel: &CancellationToken,
        ) -> std::result::Result<Vec<u8>, ProviderError> {
            if self.reads.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(ProviderError::VersionChanged);
            }
            let end = (offset + u64::from(length)).min(self.bytes.len() as u64);
            Ok(self.bytes[offset as usize..end as usize].to_vec())
        }
    }

    #[tokio::test]
    async fn created_readback_reopens_after_google_version_settles() {
        let bytes = b"settled".to_vec();
        let scope = Scope {
            account: "account".into(),
            provider: "googledrive".into(),
            collection: "root".into(),
        };
        let node = cirrove_core::Node {
            id: "item".into(),
            parent_id: Some("root".into()),
            name: "file.bin".into(),
            kind: NodeKind::File,
            size: bytes.len() as u64,
            modified_unix: 0,
            etag: None,
            content_version: Some("google-version:6".into()),
            target: None,
            package: false,
        };
        let provider = SettlingRead {
            bytes: bytes.clone(),
            node: node.clone(),
            reads: AtomicUsize::new(0),
        };
        let record = UploadRecord {
            id: uuid::Uuid::new_v4(),
            sequence: 1,
            scope,
            intent: UploadIntent::Create {
                parent: "root".into(),
                name: "file.bin".into(),
            },
            state: UploadState::Uploaded,
            size: bytes.len() as u64,
            sha256: hex::encode(Sha256::digest(&bytes)),
            attempt: None,
            remote: Some(node),
            base: None,
            working_file: None,
            session_key: None,
            transferred_bytes: bytes.len() as u64,
            retry_at: 0,
            failed_attempts: 0,
            saved_at: 0,
        };
        verify_created(&provider, &record, &CancellationToken::new())
            .await
            .expect("settling readback should reopen and pass");
        assert_eq!(provider.reads.load(Ordering::SeqCst), 2);
    }
}
