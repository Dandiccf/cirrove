//! Lost registration confirmation: recover a committed create without replay.
use super::*;
const KIND: &str = "registration-recovery";

pub async fn icloud_account_registration_interrupt(run: Uuid) -> Result<()> {
    let f = prepare_with_budget(run, KIND, BUDGET).await?;
    let bytes = super::super::read_windows::payload();
    let engine = engine(&f.state, &f.account, &f.scope, &f.snapshot).await?;
    let context = WriteContext::open(&engine, &f.state).await?;
    Store::open(context.metadata_db())?.observe_node(&f.scope, &f.parent)?;
    let row = {
        let journal = context.journal();
        let mut journal = journal
            .lock()
            .map_err(|_| anyhow::anyhow!("journal lock failed"))?;
        queue(&mut journal, &f.scope, &f.parent, None, &bytes)?
    };
    record(
        &f.run_dir.join("prepared.json"),
        &serde_json::json!({"operation":row.id,"size":row.size,"sha256":row.sha256}),
    )?;
    let provider = ICloudWriteProvider::new(&f.account, &context)?
        .validation_discard_create_registration(row.id);
    let worker = TransferWorker::new(
        context.journal(),
        Arc::new(provider),
        context.checkpoints(),
        CancellationToken::new(),
    );
    let result = worker
        .run_once()
        .await?
        .context("create was not selected")?;
    ensure!(
        result.id == row.id && result.state == UploadState::VerifyRequired,
        "registration did not retain an uncertain outcome"
    );
    let checkpoint = context
        .checkpoints()
        .load(&format!("upload/{}", row.id))
        .await?
        .context("lost registration checkpoint")?;
    let document =
        guard::completed_body_document(&checkpoint).context("no completed-body checkpoint")?;
    let node = observed(&f.snapshot, &f.account, &f.parent, &document, &bytes).await?;
    record(&f.run_dir.join("observed.json"), &node)?;
    record(
        &f.run_dir.join("interrupted.json"),
        &serde_json::json!({"pid":std::process::id(),"state":"verify_required","remote_registered":true,"independent_remote_digest":true,"size":row.size}),
    )?;
    std::fs::File::open(&f.run_dir)?.sync_all()?;
    // Worker has durably recorded uncertainty. Do not reconcile in this process.
    std::process::exit(86);
}
async fn observed(
    snapshot: &SecretString,
    account: &Account,
    parent: &Node,
    document: &str,
    bytes: &[u8],
) -> Result<Node> {
    let mut remote =
        ICloudReadSession::from_session_snapshot(snapshot, &account.identity.username)?;
    let entries = remote.list_folder(&parent.id).await?;
    ensure!(
        entries.len() == 1,
        "owned folder does not contain exactly one file"
    );
    let entry = &entries[0];
    ensure!(
        entry.drivewsid == format!("FILE::com.apple.CloudDocs::{document}")
            && entry.parent_id == parent.id
            && entry.display_name() == NAME
            && !entry.is_folder()
            && entry.size == bytes.len() as u64
            && !entry.etag.is_empty(),
        "registered file differs from captured identity"
    );
    let node = Node {
        id: entry.drivewsid.clone(),
        parent_id: Some(parent.id.clone()),
        name: entry.display_name(),
        kind: NodeKind::File,
        size: entry.size,
        modified_unix: 0,
        etag: Some(entry.etag.clone()),
        content_version: None,
        target: None,
        package: false,
    };
    verify(snapshot, account, parent, &node, bytes).await?;
    Ok(node)
}
pub async fn icloud_account_registration_recover(run: Uuid) -> Result<()> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local-state")
        .join(format!("icloud-account-{KIND}-{run}"))
        .canonicalize()?;
    let account: Account = read(&dir.join("account.json"))?;
    let parent: Node = read(&dir.join("owned-folder.json"))?;
    let prepared: serde_json::Value = read(&dir.join("prepared.json"))?;
    let interrupted: serde_json::Value = read(&dir.join("interrupted.json"))?;
    let prior: Node = read(&dir.join("observed.json"))?;
    ensure!(
        !account.enabled
            && parent.name == format!("Cirrove Write Validation-{run}")
            && parent.parent_id.as_deref() == Some(ROOT_ID)
            && parent.kind == NodeKind::Folder
            && !parent.package
            && parent.target.is_none()
            && interrupted["state"] == "verify_required"
            && interrupted["remote_registered"] == true,
        "not the owned registration boundary"
    );
    let operation = Uuid::parse_str(
        prepared["operation"]
            .as_str()
            .context("missing operation")?,
    )?;
    let bytes = super::super::read_windows::payload();
    let size = bytes.len() as u64;
    let sha = hex::encode(Sha256::digest(&bytes));
    ensure!(
        prepared["size"] == size && prepared["sha256"] == sha,
        "payload contract changed"
    );
    record(
        &dir.join("recovery-started.json"),
        &serde_json::json!({"pid":std::process::id()}),
    )?;
    let state = dir.join("state");
    let snapshot = SealedSessionVault::new(&state, &account.id)?
        .load(&account.credential_id)
        .await?
        .context("isolated session unavailable")?;
    let scope = Scope {
        account: account.id.clone(),
        provider: "icloud".into(),
        collection: "drive".into(),
    };
    let engine = engine(&state, &account, &scope, &snapshot).await?;
    let context = WriteContext::open(&engine, &state).await?;
    let pending = {
        let journal = context.journal();
        let journal = journal
            .lock()
            .map_err(|_| anyhow::anyhow!("journal lock failed"))?;
        let rows = journal.list(0, 2)?;
        ensure!(rows.len() == 1, "unexpected history");
        journal.get(operation)?
    };
    ensure!(
        pending.state == UploadState::VerifyRequired
            && pending.remote.is_none()
            && pending.scope == scope
            && pending.size == size
            && pending.sha256 == sha
            && pending.intent
                == (UploadIntent::Create {
                    parent: parent.id.clone(),
                    name: NAME.into()
                }),
        "retained create changed"
    );
    let checkpoint = context
        .checkpoints()
        .load(&format!("upload/{operation}"))
        .await?
        .context("missing checkpoint")?;
    let document =
        guard::completed_body_document(&checkpoint).context("completed-body checkpoint missing")?;
    let before = observed(&snapshot, &account, &parent, &document, &bytes).await?;
    ensure!(
        before.id == prior.id && before.etag == prior.etag,
        "remote version changed before recovery"
    );
    let source = context
        .journal()
        .lock()
        .map_err(|_| anyhow::anyhow!("journal lock failed"))?
        .local_export_source(operation)?;
    let destination = dir.join("recovered-local.bin");
    let exported = tokio::task::spawn_blocking(move || {
        source.copy_to(&destination, &CancellationToken::new(), |_| {})
    })
    .await??;
    ensure!(
        exported.operation == operation && exported.size == size && exported.sha256 == sha,
        "retained payload/export mismatch"
    );
    let provider = Arc::new(guard::Guard {
        inner: ICloudWriteProvider::new(&account, &context)?,
        request: UploadRequest {
            scope,
            intent: pending.intent,
            size,
            sha256: sha,
        },
        operation: operation.to_string(),
        complete_body: true,
        inspections: Default::default(),
        reconciliations: Default::default(),
        refused: Default::default(),
    });
    let worker = TransferWorker::new(
        context.journal(),
        provider.clone(),
        context.checkpoints(),
        CancellationToken::new(),
    );
    let result = worker
        .run_once()
        .await?
        .context("verification not selected")?;
    ensure!(
        result.id == operation
            && result.state == UploadState::Uploaded
            && provider.refused.load(Ordering::Relaxed) == 0
            && provider.inspections.load(Ordering::Relaxed)
                + provider.reconciliations.load(Ordering::Relaxed)
                > 0,
        "recovery did not finish through inspection alone"
    );
    let completed = context
        .journal()
        .lock()
        .map_err(|_| anyhow::anyhow!("journal lock failed"))?
        .get(operation)?
        .remote
        .context("missing final receipt")?;
    ensure!(
        completed.id == before.id && completed.etag == before.etag,
        "recovery changed document or revision"
    );
    let after = observed(&snapshot, &account, &parent, &document, &bytes).await?;
    ensure!(
        after.id == before.id && after.etag == before.etag,
        "remote changed during reconciliation"
    );
    ensure!(
        context
            .checkpoints()
            .load(&format!("upload/{operation}"))
            .await?
            .is_none(),
        "completed checkpoint retained"
    );
    record(&dir.join("completed.json"), &completed)?;
    record(
        &dir.join("passed.json"),
        &serde_json::json!({"run":run,"size":size,"state":"uploaded","same_document_identity":true,"same_remote_revision":true,"local_export_verified":true,"independent_remote_digest":true,"exactly_one_remote_file":true,"inspection_calls":provider.inspections.load(Ordering::Relaxed),"reconciliation_calls":provider.reconciliations.load(Ordering::Relaxed),"refused_replays":0,"installed_service_changed":false}),
    )?;
    println!("Lost registration confirmation: recovered exact committed identity without replay");
    Ok(())
}
