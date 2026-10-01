//! Process loss while the HTTP upload body is incomplete, followed by read-only
//! reconciliation, verified local export, then a proved-uncommitted retry.
use super::*;
use cirrove_core::upload::UploadRequest;
use std::{io::Read, sync::atomic::Ordering};
mod guard;
mod registration;
pub use registration::{
    icloud_account_registration_interrupt, icloud_account_registration_recover,
};
const AFTER: u64 = 8 * 1024 * 1024;
const BUDGET: u64 = 256 * 1024 * 1024;

pub async fn icloud_account_stream_interrupt(run: Uuid) -> Result<()> {
    interrupt(run, false).await
}
pub async fn icloud_account_replace_stream_interrupt(run: Uuid) -> Result<()> {
    interrupt(run, true).await
}
fn kind(replace: bool) -> &'static str {
    if replace {
        "replace-stream-recovery"
    } else {
        "stream-recovery"
    }
}
async fn interrupt(run: Uuid, replace: bool) -> Result<()> {
    let f = prepare_with_budget(run, kind(replace), BUDGET).await?;
    let original = if replace {
        let node = transfer(
            &f.state,
            &f.account,
            &f.scope,
            &f.snapshot,
            &f.parent,
            None,
            FIRST,
        )
        .await?;
        verify(&f.snapshot, &f.account, &f.parent, &node, FIRST).await?;
        record(&f.run_dir.join("original.json"), &node)?;
        Some(node)
    } else {
        None
    };
    let bytes = super::read_windows::payload();
    let engine = engine(&f.state, &f.account, &f.scope, &f.snapshot).await?;
    let context = WriteContext::open(&engine, &f.state).await?;
    {
        let mut store = Store::open(context.metadata_db())?;
        store.observe_node(&f.scope, &f.parent)?;
        if let Some(node) = &original {
            store.observe_node(&f.scope, node)?;
        }
    }
    let row = {
        let journal = context.journal();
        let mut journal = journal
            .lock()
            .map_err(|_| anyhow::anyhow!("journal lock failed"))?;
        queue(&mut journal, &f.scope, &f.parent, original.as_ref(), &bytes)?
    };
    let name = if replace {
        format!("staged-by-cirrove-{}.txt", row.id)
    } else {
        NAME.into()
    };
    let boundary = cirrove_icloud::install_upload_stream_boundary(name, bytes.len() as u64, AFTER)?;
    record(
        &f.run_dir.join("prepared.json"),
        &serde_json::json!({"operation":row.id,"replacement":replace,"size":bytes.len(),"sha256":hex::encode(Sha256::digest(&bytes)),"after":AFTER}),
    )?;
    let worker = TransferWorker::new(
        context.journal(),
        Arc::new(ICloudWriteProvider::new(&f.account, &context)?),
        context.checkpoints(),
        CancellationToken::new(),
    );
    let transfer = worker.run_once();
    tokio::pin!(transfer);
    let yielded = tokio::select! {
        result = &mut transfer => { result?; anyhow::bail!("upload completed without the stream interruption"); },
        yielded = boundary.wait_reached() => yielded,
    };
    ensure!(
        yielded >= AFTER && yielded < bytes.len() as u64,
        "boundary did not leave incomplete content"
    );
    record(
        &f.run_dir.join("interrupted.json"),
        &serde_json::json!({"pid":std::process::id(),"body_bytes_yielded":yielded,"complete_body":false,"remote_acceptance_unknown":true,"size":bytes.len()}),
    )?;
    std::fs::File::open(&f.run_dir)?.sync_all()?;
    // No worker return or orderly journal shutdown after this body boundary.
    std::process::exit(86);
}
fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = std::fs::read(path)?;
    ensure!(bytes.len() <= 32768, "validation record too large");
    serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid validation record"))
}
pub async fn icloud_account_stream_recover(run: Uuid) -> Result<()> {
    recover(run, false).await
}
pub async fn icloud_account_replace_stream_recover(run: Uuid) -> Result<()> {
    recover(run, true).await
}
async fn recover(run: Uuid, replace: bool) -> Result<()> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local-state")
        .join(format!("icloud-account-{}-{run}", kind(replace)))
        .canonicalize()?;
    let account: Account = read(&dir.join("account.json"))?;
    let parent: Node = read(&dir.join("owned-folder.json"))?;
    let interrupted: serde_json::Value = read(&dir.join("interrupted.json"))?;
    let prepared: serde_json::Value = read(&dir.join("prepared.json"))?;
    ensure!(
        !account.enabled
            && parent.name == format!("Cirrove Write Validation-{run}")
            && parent.parent_id.as_deref() == Some(ROOT_ID)
            && parent.kind == NodeKind::Folder
            && !parent.package
            && parent.target.is_none(),
        "not the owned isolated source"
    );
    ensure!(
        prepared["replacement"] == replace,
        "wrong interruption kind"
    );
    let operation = Uuid::parse_str(
        prepared["operation"]
            .as_str()
            .context("missing operation")?,
    )?;
    let original: Option<Node> = if replace {
        Some(read(&dir.join("original.json"))?)
    } else {
        None
    };
    let size = prepared["size"].as_u64().context("missing size")?;
    let sha = prepared["sha256"].as_str().context("missing digest")?;
    let yielded = interrupted["body_bytes_yielded"]
        .as_u64()
        .context("missing body boundary")?;
    ensure!(
        yielded >= AFTER && yielded < size && interrupted["complete_body"] == false,
        "not an incomplete body boundary"
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
        let rows = journal.list(0, 3)?;
        ensure!(
            rows.len() == 1 + usize::from(replace),
            "unexpected interruption history"
        );
        if let Some(original) = &original {
            ensure!(
                rows[0].state == UploadState::Uploaded && rows[0].remote.as_ref() == Some(original),
                "original receipt changed"
            );
        }
        let row = journal.get(operation)?;
        let intent = if let Some(original) = &original {
            UploadIntent::Replace {
                item: original.id.clone(),
                expected_etag: original.etag.clone().context("original revision missing")?,
            }
        } else {
            UploadIntent::Create {
                parent: parent.id.clone(),
                name: NAME.into(),
            }
        };
        ensure!(
            row.state == UploadState::VerifyRequired
                && row.scope == scope
                && row.size == size
                && row.sha256 == sha
                && row.remote.is_none()
                && row.intent == intent,
            "interrupted create changed"
        );
        let mut source = journal.payload(row.id)?;
        let mut digest = Sha256::new();
        let mut received = 0;
        let mut block = [0; 65536];
        loop {
            let n = source.read(&mut block)?;
            if n == 0 {
                break;
            }
            received += n as u64;
            digest.update(&block[..n]);
        }
        ensure!(
            received == size && hex::encode(digest.finalize()) == sha,
            "interrupted local payload changed"
        );
        row
    };
    let checkpoint = context
        .checkpoints()
        .load(&format!("upload/{}", pending.id))
        .await?
        .context("missing durable stream checkpoint")?;
    let old_document = guard::allocated_document(&checkpoint)
        .context("stream checkpoint is not allocated without a receipt")?;
    let mut remote =
        ICloudReadSession::from_session_snapshot(&snapshot, &account.identity.username)?;
    unchanged_before_retry(&snapshot, &account, &parent, original.as_ref()).await?;
    let provider = Arc::new(guard::Guard {
        inner: ICloudWriteProvider::new(&account, &context)?,
        request: UploadRequest {
            scope: scope.clone(),
            intent: pending.intent.clone(),
            size,
            sha256: sha.into(),
        },
        operation: pending.id.to_string(),
        complete_body: false,
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
        .context("interrupted create was not selected for verification")?;
    ensure!(
        result.id == pending.id && result.state == UploadState::Pending,
        "unregistered stream was not proved uncommitted"
    );
    ensure!(
        provider.refused.load(Ordering::Relaxed) == 0
            && provider.inspections.load(Ordering::Relaxed) > 0
            && provider.reconciliations.load(Ordering::Relaxed) > 0,
        "recovery attempted a replay or skipped reconciliation"
    );
    unchanged_before_retry(&snapshot, &account, &parent, original.as_ref()).await?;
    let source = context
        .journal()
        .lock()
        .map_err(|_| anyhow::anyhow!("journal lock failed"))?
        .local_export_source(pending.id)?;
    let destination = dir.join("recovered-local.bin");
    let receipt = tokio::task::spawn_blocking(move || {
        source.copy_to(&destination, &CancellationToken::new(), |_| {})
    })
    .await??;
    ensure!(
        receipt.operation == pending.id && receipt.size == size && receipt.sha256 == sha,
        "local export receipt differs"
    );
    let after = context
        .journal()
        .lock()
        .map_err(|_| anyhow::anyhow!("journal lock failed"))?
        .get(pending.id)?;
    ensure!(
        after.state == UploadState::Pending
            && after.sha256 == pending.sha256
            && after.size == pending.size
            && after.remote.is_none(),
        "export changed uncertain operation"
    );
    ensure!(
        context
            .checkpoints()
            .load(&format!("upload/{}", pending.id))
            .await?
            .is_none(),
        "old transport checkpoint was not cleared after proof"
    );
    record(
        &dir.join("reconciled.json"),
        &serde_json::json!({"state":"pending","local_export_verified":true,"refused_replays":0,"visible_folder_empty":!replace,"original_unchanged":replace}),
    )?;
    let retry = TransferWorker::new(
        context.journal(),
        Arc::new(ICloudWriteProvider::new(&account, &context)?),
        context.checkpoints(),
        CancellationToken::new(),
    );
    let completed = retry
        .run_once()
        .await?
        .context("verified-uncommitted create was not retried")?;
    ensure!(
        completed.id == pending.id && completed.state == UploadState::Uploaded,
        "fresh upload did not complete"
    );
    let row = context
        .journal()
        .lock()
        .map_err(|_| anyhow::anyhow!("journal lock failed"))?
        .get(pending.id)?;
    let current = row.remote.context("retry lacks receipt")?;
    ensure!(
        current.id.rsplit("::").next() != Some(old_document.as_str()),
        "retry reused the interrupted document identity"
    );
    let bytes = super::read_windows::payload();
    verify(&snapshot, &account, &parent, &current, &bytes).await?;
    let entries = remote.list_folder(&parent.id).await?;
    ensure!(
        entries.len() == 1 && entries[0].drivewsid == current.id,
        "retry left duplicates or an unexpected item"
    );
    if let Some(original) = &original {
        ensure!(
            current.id != original.id && remote.exact_item_in_trash(&original.id).await?,
            "original recovery identity missing after replacement"
        );
    }
    record(&dir.join("completed.json"), &current)?;
    record(
        &dir.join("passed.json"),
        &serde_json::json!({"run":run,"size":size,"body_bytes_yielded":yielded,"remote_acceptance_unknown":true,"local_payload_verified":true,"allocated_checkpoint_retained":true,"inspection_calls":provider.inspections.load(Ordering::Relaxed),"reconciliation_calls":provider.reconciliations.load(Ordering::Relaxed),"refused_replays":0,"visible_folder_empty_before_retry":!replace,"original_unchanged_before_retry":replace,"original_in_trash_after_retry":replace,"replacement":replace,"state":"uploaded","uncommitted_before_retry":true,"fresh_document_identity":true,"exactly_one_remote_file":true,"independent_remote_digest":true,"local_export_verified":true,"installed_service_changed":false}),
    )?;
    println!(
        "Interrupted body: preserved bytes, read-only reconciliation, local export and fresh retry verified"
    );
    Ok(())
}

async fn unchanged_before_retry(
    snapshot: &SecretString,
    account: &Account,
    parent: &Node,
    original: Option<&Node>,
) -> Result<()> {
    let mut remote =
        ICloudReadSession::from_session_snapshot(snapshot, &account.identity.username)?;
    let entries = remote.list_folder(&parent.id).await?;
    if let Some(original) = original {
        ensure!(
            entries.len() == 1 && entries[0].drivewsid == original.id,
            "original not alone in the owned folder"
        );
        verify(snapshot, account, parent, original, FIRST).await?;
    } else {
        ensure!(entries.is_empty(), "unexpected visible item before retry");
    }
    Ok(())
}
