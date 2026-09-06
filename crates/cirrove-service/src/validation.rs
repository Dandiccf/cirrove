//! Explicit developer-only cloud mutation checks. Never called by the daemon.
//! Every target is created by this run; no existing document is accepted as input.
use crate::{
    accounts,
    journal::{UploadJournal, UploadRecord, UploadState},
    transfers::TransferWorker,
};
use anyhow::{Context, Result, bail};
use cirrove_auth::{AccessMode, DesktopVault};
use cirrove_core::{
    CancellationToken, Node, Scope,
    upload::{
        Reconciliation, UploadError, UploadIntent, UploadProvider, UploadRequest, UploadStep,
    },
};
use cirrove_onedrive::OneDrive;
use secrecy::SecretString;
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

/// This fixture changes only a file created by this run, after its competing
/// replacement has staged every byte but before the final commit request.
struct CompetingEdit {
    inner: Arc<OneDrive>,
    target: Node,
    fired: AtomicBool,
    completed: Mutex<Option<UploadRequest>>,
}
#[async_trait::async_trait]
impl UploadProvider for CompetingEdit {
    async fn begin_upload(
        &self,
        r: &UploadRequest,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.inner.begin_upload(r, c).await
    }
    async fn inspect_upload(
        &self,
        r: &UploadRequest,
        s: &SecretString,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.inner.inspect_upload(r, s, c).await
    }
    async fn upload_part(
        &self,
        r: &UploadRequest,
        s: &SecretString,
        o: u64,
        b: Vec<u8>,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.inner.upload_part(r, s, o, b, c).await
    }
    async fn commit_upload(
        &self,
        r: &UploadRequest,
        s: &SecretString,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        let UploadIntent::Replace {
            item,
            expected_etag,
        } = &r.intent
        else {
            return Err(UploadError::Invalid);
        };
        if item != &self.target.id || Some(expected_etag) != self.target.etag.as_ref() {
            return Err(UploadError::Invalid);
        }
        if !self.fired.swap(true, Ordering::SeqCst) {
            println!("  creating a competing edit after all replacement bytes were staged");
            let competing = UploadRequest {
                scope: r.scope.clone(),
                intent: r.intent.clone(),
                size: 0,
                sha256: format!("{:x}", Sha256::digest([])),
            };
            // Empty synthetic content cannot be lost locally. Its request and
            // receipt are used only to verify this run's isolated conflict test.
            let UploadStep::Complete(node) = self.inner.begin_upload(&competing, c).await? else {
                return Err(UploadError::Uncertain);
            };
            if node.etag.as_ref() == Some(expected_etag) || node.size != 0 {
                return Err(UploadError::Uncertain);
            }
            *self.completed.lock().map_err(|_| UploadError::Invalid)? = Some(competing);
        }
        if self
            .completed
            .lock()
            .map_err(|_| UploadError::Invalid)?
            .is_none()
        {
            return Err(UploadError::Unsupported(
                "competing test edit has an uncertain result",
            ));
        }
        self.inner.commit_upload(r, s, c).await
    }
    async fn reconcile_upload(
        &self,
        r: &UploadRequest,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<Reconciliation> {
        self.inner.reconcile_upload(r, c).await
    }
}

fn event(log: &mut File, value: serde_json::Value) -> Result<()> {
    serde_json::to_writer(&mut *log, &value)?;
    log.write_all(b"\n")?;
    log.sync_all()?;
    Ok(())
}
fn record(journal: &Arc<Mutex<UploadJournal>>, id: uuid::Uuid) -> Result<UploadRecord> {
    Ok(journal
        .lock()
        .map_err(|_| anyhow::anyhow!("validation journal unavailable"))?
        .get(id)?)
}
async fn transfer(
    journal: &Arc<Mutex<UploadJournal>>,
    worker: &TransferWorker,
    scope: Scope,
    intent: UploadIntent,
    bytes: Vec<u8>,
) -> Result<UploadRecord> {
    let shared = journal.clone();
    let id = tokio::task::spawn_blocking(move || -> Result<_> {
        Ok(shared
            .lock()
            .map_err(|_| anyhow::anyhow!("validation journal unavailable"))?
            .enqueue(scope, intent, bytes.as_slice())?
            .id)
    })
    .await??;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(300);
    loop {
        if let Some(result) = worker.run_once().await? {
            println!("  transfer: {:?}", result.state);
            if let Some(issue) = result.issue {
                println!("  {issue}");
            }
        }
        let current = record(journal, id)?;
        if matches!(
            current.state,
            UploadState::Uploaded | UploadState::Conflict | UploadState::Failed
        ) {
            return Ok(current);
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("validation transfer deadline reached; its local journal and bytes are retained");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
async fn verify(
    provider: &dyn UploadProvider,
    record: &UploadRecord,
    cancel: &CancellationToken,
) -> Result<()> {
    if record.state != UploadState::Uploaded {
        bail!("upload was not acknowledged; local bytes are retained");
    }
    let request = UploadRequest {
        scope: record.scope.clone(),
        intent: record.intent.clone(),
        size: record.size,
        sha256: record.sha256.clone(),
    };
    if !matches!(
        provider.reconcile_upload(&request, cancel).await?,
        Reconciliation::Committed(_)
    ) {
        bail!("uploaded content did not pass independent readback");
    }
    Ok(())
}

/// Explicit write grant and an idle isolated account are both required. The test
/// folder and local receipts remain for review, including after a failed check.
pub async fn onedrive_uploads(state: &Path, label: &str) -> Result<()> {
    let account = accounts::Settings::load(state)?
        .accounts
        .into_iter()
        .find(|a| a.label == label)
        .context("unknown validation account")?;
    if account.access != AccessMode::ReadWrite {
        bail!("use a separate connection with --write-access for this developer check");
    }
    if account.enabled {
        bail!(
            "disable this validation account before the check; use a separate account state to keep ordinary mounts running"
        );
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
        provider: "onedrive".into(),
        collection: account.drive.id.clone(),
    };
    let name = format!("Cirrove-Write-Validation-{run}");
    // Persist the unique name before the first mutation. A lost folder-creation
    // response stops this run; it never adopts an existing folder automatically.
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
    println!("Checking uploads only in the new folder {name}.");
    println!("Private local evidence: {}", directory.display());
    let graph = accounts::provider(&account)?;
    let cancel = CancellationToken::new();
    let folder = graph
        .create_folder(&scope, &account.root_id, &name, &cancel)
        .await?;
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
        graph.clone(),
        Arc::new(DesktopVault),
        cancel.clone(),
    );
    let create = |name: &str| UploadIntent::Create {
        parent: folder.id.clone(),
        name: name.into(),
    };
    println!("Checking a multi-part upload and independent content readback.");
    let first = transfer(
        &journal,
        &worker,
        scope.clone(),
        create("Kärnten & Grüße #1.bin"),
        vec![0x31; 5 * 1024 * 1024 + 13],
    )
    .await?;
    event(
        &mut log,
        serde_json::json!({"stage":"create_result", "operation":first}),
    )?;
    verify(graph.as_ref(), &first, &cancel).await?;
    event(
        &mut log,
        serde_json::json!({"stage":"create_readback_passed"}),
    )?;
    println!("Checking that an existing name cannot be overwritten by a create.");
    let collision = transfer(
        &journal,
        &worker,
        scope.clone(),
        create("Kärnten & Grüße #1.bin"),
        b"collision must fail".to_vec(),
    )
    .await?;
    event(
        &mut log,
        serde_json::json!({"stage":"collision_result", "operation":collision}),
    )?;
    if collision.state != UploadState::Conflict {
        bail!("name-collision protection did not pass");
    }
    verify(graph.as_ref(), &first, &cancel).await?;
    println!("Checking an empty file.");
    let empty = transfer(
        &journal,
        &worker,
        scope.clone(),
        create("Empty.txt"),
        vec![],
    )
    .await?;
    event(
        &mut log,
        serde_json::json!({"stage":"empty_result", "operation":empty}),
    )?;
    verify(graph.as_ref(), &empty, &cancel).await?;
    let remote = first
        .remote
        .as_ref()
        .context("created fixture has no receipt")?;
    let original_tag = remote
        .etag
        .clone()
        .context("created fixture has no metadata ETag")?;
    println!("Checking a conditional replacement of this run's own fixture.");
    let replace = UploadIntent::Replace {
        item: remote.id.clone(),
        expected_etag: original_tag,
    };
    let second = transfer(
        &journal,
        &worker,
        scope.clone(),
        replace.clone(),
        vec![0x52; 5 * 1024 * 1024 + 29],
    )
    .await?;
    event(
        &mut log,
        serde_json::json!({"stage":"replace_result", "operation":second}),
    )?;
    verify(graph.as_ref(), &second, &cancel).await?;
    println!("Checking rejection of the old file revision.");
    let stale = transfer(
        &journal,
        &worker,
        scope.clone(),
        replace,
        b"stale edit must fail".to_vec(),
    )
    .await?;
    event(
        &mut log,
        serde_json::json!({"stage":"stale_result", "operation":stale}),
    )?;
    if stale.state != UploadState::Conflict {
        bail!("stale-revision protection did not pass");
    }
    verify(graph.as_ref(), &second, &cancel).await?;
    println!("Checking a competing edit during the upload, before final commit.");
    let baseline = transfer(
        &journal,
        &worker,
        scope.clone(),
        create("Concurrent.bin"),
        b"concurrent test baseline".to_vec(),
    )
    .await?;
    event(
        &mut log,
        serde_json::json!({"stage":"race_baseline", "operation":baseline}),
    )?;
    verify(graph.as_ref(), &baseline, &cancel).await?;
    let target = baseline
        .remote
        .clone()
        .context("race fixture has no receipt")?;
    let race = Arc::new(CompetingEdit {
        inner: graph.clone(),
        target: target.clone(),
        fired: AtomicBool::new(false),
        completed: Mutex::new(None),
    });
    let race_worker = TransferWorker::new(
        journal.clone(),
        race.clone(),
        Arc::new(DesktopVault),
        cancel.clone(),
    );
    let racing = transfer(
        &journal,
        &race_worker,
        scope.clone(),
        UploadIntent::Replace {
            item: target.id,
            expected_etag: target.etag.context("race fixture has no ETag")?,
        },
        vec![0x73; 5 * 1024 * 1024 + 47],
    )
    .await?;
    event(
        &mut log,
        serde_json::json!({"stage":"race_result", "operation":racing, "competing_edit_attempted":race.fired.load(Ordering::SeqCst)}),
    )?;
    if racing.state != UploadState::Conflict {
        bail!("concurrent-edit protection did not pass; do not enable writable mounts");
    }
    let competing = race
        .completed
        .lock()
        .map_err(|_| anyhow::anyhow!("race fixture unavailable"))?
        .clone()
        .context("competing edit did not finish")?;
    if !matches!(
        graph.reconcile_upload(&competing, &cancel).await?,
        Reconciliation::Committed(_)
    ) {
        bail!("competing edit was not preserved; do not enable writable mounts");
    }
    event(
        &mut log,
        serde_json::json!({"stage":"race_readback_passed"}),
    )?;
    event(
        &mut log,
        serde_json::json!({"stage":"basic_checks_passed", "remaining":"process interruption, writable FUSE application saves, broader account matrix"}),
    )?;
    println!("Basic upload checks passed. The test folder and local snapshots remain for review.");
    println!(
        "This does not complete the broader account/recovery matrix or enable writable mounts."
    );
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn validation_refuses_legacy_grants_enabled_accounts_and_competing_owners() {
        let temp = tempfile::tempdir().unwrap();
        let id = "00000000-0000-4000-8000-000000000001";
        let mut settings = serde_json::json!({"version":1, "accounts":[{
            "id":id, "label":"fixture", "registration":{"client_id":id,"authority":"common"},
            "identity":{"tenant_id":id,"subject":"fixture","username":"fixture@example.invalid","graph_user_id":"fixture","display_name":"Fixture"},
            "credential_id":id, "drive":{"id":"drive","name":"Fixture","driveType":"business","webUrl":"https://example.invalid"},
            "root_id":"root", "mount_path":temp.path().join("mount"), "enabled":false,"poll_seconds":30,"cache_bytes":16777216
        }]});
        let save = |settings: &serde_json::Value| {
            std::fs::write(
                temp.path().join("accounts.json"),
                serde_json::to_vec(settings).unwrap(),
            )
            .unwrap()
        };
        save(&settings);
        let loaded = accounts::Settings::load(temp.path()).unwrap();
        assert_eq!(loaded.accounts[0].access, AccessMode::ReadOnly);
        assert!(
            onedrive_uploads(temp.path(), "fixture")
                .await
                .unwrap_err()
                .to_string()
                .contains("--write-access")
        );
        settings["accounts"][0]["access"] = "read_write".into();
        settings["accounts"][0]["enabled"] = true.into();
        save(&settings);
        assert!(
            onedrive_uploads(temp.path(), "fixture")
                .await
                .unwrap_err()
                .to_string()
                .contains("disable")
        );
        settings["accounts"][0]["enabled"] = false.into();
        save(&settings);
        let _owner = accounts::account_lock(&temp.path().join("accounts").join(id)).unwrap();
        assert!(onedrive_uploads(temp.path(), "fixture").await.is_err());
        assert!(!temp.path().join("write-checks").exists());
    }
}
