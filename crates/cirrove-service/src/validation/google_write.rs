//! Explicit Google create and metadata-precondition check. It is wired for a
//! disabled, separately consented account and is never called by the daemon.
use super::*;
use crate::{
    journal::{MutationRecord, MutationState},
    mutations::MutationWorker,
};
use cirrove_core::{
    NodeKind, ReadProvider,
    mutation::{MutationIntent, MutationReceipt, MutationRequest},
};
use cirrove_googledrive::GoogleDrive;

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
    provider: &GoogleDrive,
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
    let node = provider
        .node(&record.scope, &receipt.id, cancel)
        .await
        .context("created Google file cannot be read back by its exact identity")?;
    if node.id != receipt.id || node.size != record.size {
        bail!("created Google file identity or size changed during readback");
    }
    let mut digest = Sha256::new();
    let mut offset = 0;
    while offset < node.size {
        let length = (node.size - offset).min(4 * 1024 * 1024) as u32;
        let bytes = provider
            .read_range(&record.scope, &node, offset, length, cancel)
            .await
            .context("created Google content could not be read back")?;
        if bytes.len() != length as usize {
            bail!("created Google content readback was incomplete");
        }
        digest.update(&bytes);
        offset += bytes.len() as u64;
    }
    if hex::encode(digest.finalize()) != record.sha256 {
        bail!("created Google content did not match its durable local snapshot");
    }
    Ok(())
}

/// Create one exact test folder, one multipart file and one empty file, then
/// characterize stale HTTP ETag handling on the multipart file. The folder and
/// local evidence remain for review; no pre-existing item is changed.
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
