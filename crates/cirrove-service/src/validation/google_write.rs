//! Explicit Google create check. It is wired for a disabled, separately
//! consented account and is never called by the daemon.
use super::*;
use cirrove_core::ReadProvider;
use cirrove_googledrive::GoogleDrive;

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

/// Create one exact test folder, one multipart file and one empty file. The
/// folder and local evidence remain for review; no existing item is changed.
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
    println!("Checking Google creates only in the new folder {name}.");
    println!("Private local evidence: {}", directory.display());

    let google = accounts::google_provider(&account)?;
    let cancel = CancellationToken::new();
    let plan = google
        .prepare_folder(&scope, &account.root_id, &name, &cancel)
        .await?;
    // The exact generated identity is durable before the first mutation. If a
    // response is lost, the run never searches or adopts by non-unique name.
    event(
        &mut log,
        serde_json::json!({"stage":"folder_prepared", "folder":plan}),
    )?;
    let folder = match google
        .create_prepared_folder(&scope, &plan, &cancel)
        .await
    {
        Ok(folder) => folder,
        Err(UploadError::Uncertain) => google
            .inspect_prepared_folder(&scope, &plan, &cancel)
            .await?
            .context("Google folder-create outcome is uncertain; the prepared identity is retained in the private evidence")?,
        Err(error) => return Err(error.into()),
    };
    let inspected = google
        .inspect_prepared_folder(&scope, &plan, &cancel)
        .await?
        .context("created Google validation folder is absent")?;
    if inspected.id != folder.id {
        bail!("Google validation folder identity changed after creation");
    }
    event(
        &mut log,
        serde_json::json!({"stage":"folder_created", "node":folder}),
    )?;

    let journal_path = directory.join("journal");
    let account_id = account.id.clone();
    let journal = Arc::new(Mutex::new(
        tokio::task::spawn_blocking(move || {
            UploadJournal::open(&journal_path, &account_id, 64 * 1024 * 1024)
        })
        .await??,
    ));
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

    println!("Checking a multipart Google create and independent exact-ID readback.");
    let multipart = transfer(
        &journal,
        &worker,
        scope.clone(),
        create("Kärnten & Grüße #1.bin"),
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
    let empty = transfer(&journal, &worker, scope, create("Empty.txt"), Vec::new()).await?;
    event(
        &mut log,
        serde_json::json!({"stage":"empty_result", "operation":empty}),
    )?;
    verify_created(google.as_ref(), &empty, &cancel).await?;
    event(
        &mut log,
        serde_json::json!({"stage":"create_checks_passed", "fixture_retained":true}),
    )?;
    println!("Google create checks passed. The test folder and local snapshots remain for review.");
    println!("This does not validate replacement or enable writable Google mounts.");
    Ok(())
}
