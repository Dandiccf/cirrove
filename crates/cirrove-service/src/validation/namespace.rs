//! All mutation targets are created in this invocation's unique test directory.
use super::*;
use crate::{
    journal::{MutationRecord, MutationState},
    mutations::MutationWorker,
};
use cirrove_core::mutation::{MutationIntent, MutationReceipt, MutationRequest};
use cirrove_core::{ProviderError, ReadProvider};

async fn apply(
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
            .map_err(|_| anyhow::anyhow!("journal unavailable"))?
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
            .map_err(|_| anyhow::anyhow!("journal unavailable"))?
            .mutation(id)?;
        if matches!(
            record.state,
            MutationState::Applied
                | MutationState::Conflict
                | MutationState::Failed
                | MutationState::NeedsReview
        ) {
            event(log, serde_json::json!({"stage":stage,"record":record}))?;
            if record.state != expected {
                bail!(
                    "namespace check did not reach its expected state; retained operation requires review"
                );
            }
            return Ok(record);
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("namespace validation deadline reached; local evidence is retained");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
fn node(record: MutationRecord) -> Result<Node> {
    match record.receipt {
        Some(MutationReceipt::Upsert(node)) => Ok(node),
        _ => bail!("namespace check has no item receipt"),
    }
}
async fn create_file(
    journal: &Arc<Mutex<UploadJournal>>,
    worker: &TransferWorker,
    graph: &OneDrive,
    location: (&Scope, &str, &str),
    bytes: &[u8],
    cancel: &CancellationToken,
) -> Result<Node> {
    let (scope, parent, name) = location;
    let record = transfer(
        journal,
        worker,
        scope.clone(),
        UploadIntent::Create {
            parent: parent.into(),
            name: name.into(),
        },
        bytes.to_vec(),
    )
    .await?;
    verify(graph, &record, cancel).await?;
    record.remote.context("fixture upload has no receipt")
}
pub async fn onedrive_mutations(state: &Path, label: &str) -> Result<()> {
    let account = test_account(state, label)?;
    let _operation = accounts::account_operation(state, &account.id)?;
    let _owner = accounts::account_lock(&state.join("accounts").join(&account.id))?;
    let run = uuid::Uuid::new_v4();
    let directory = state.join("namespace-checks").join(run.to_string());
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
    let name = format!("Cirrove-Namespace-Validation-{run}");
    event(
        &mut log,
        serde_json::json!({"stage":"planned","folder":name,"scope":scope}),
    )?;
    for ancestor in directory.ancestors() {
        File::open(ancestor)?.sync_all()?;
    }
    println!("Checking namespace changes only in the new folder {name}.");
    println!("Private local evidence: {}", directory.display());
    let graph = accounts::provider(&account)?;
    let cancel = CancellationToken::new();
    let folder = graph
        .create_folder(&scope, &account.root_id, &name, &cancel)
        .await?;
    event(
        &mut log,
        serde_json::json!({"stage":"folder_created","node":folder}),
    )?;
    let path = directory.join("journal");
    let account_id = account.id.clone();
    let journal = Arc::new(Mutex::new(
        tokio::task::spawn_blocking(move || {
            UploadJournal::open(&path, &account_id, 16 * 1024 * 1024)
        })
        .await??,
    ));
    let uploads = TransferWorker::new(
        journal.clone(),
        graph.clone(),
        Arc::new(DesktopVault),
        cancel.clone(),
    );
    let mutations = MutationWorker::new(journal.clone(), graph.clone(), cancel.clone());
    let request = |intent| MutationRequest {
        scope: scope.clone(),
        intent,
    };
    println!("Checking durable folder creation.");
    let destination = node(
        apply(
            &journal,
            &mutations,
            request(MutationIntent::CreateFolder {
                parent: folder.id.clone(),
                name: "Destination".into(),
            }),
            MutationState::Applied,
            &mut log,
            "directory_created",
        )
        .await?,
    )?;
    const DATA: &[u8] = b"Cirrove namespace fixture\n";
    let original = create_file(
        &journal,
        &uploads,
        &graph,
        (&scope, &folder.id, "Original.txt"),
        DATA,
        &cancel,
    )
    .await?;
    println!("Checking Unicode rename, then move, with content readback.");
    let renamed = node(
        apply(
            &journal,
            &mutations,
            request(MutationIntent::Relocate {
                before: original.clone(),
                parent: folder.id.clone(),
                name: "Kärnten & Grüße #1.txt".into(),
            }),
            MutationState::Applied,
            &mut log,
            "renamed",
        )
        .await?,
    )?;
    let moved = node(
        apply(
            &journal,
            &mutations,
            request(MutationIntent::Relocate {
                before: renamed.clone(),
                parent: destination.id.clone(),
                name: renamed.name.clone(),
            }),
            MutationState::Applied,
            &mut log,
            "moved",
        )
        .await?,
    )?;
    let remote = graph.node(&scope, &moved.id, &cancel).await?;
    if remote.name != moved.name
        || remote.parent_id != moved.parent_id
        || graph.read_range(&scope, &remote, 0, 1024, &cancel).await? != DATA
    {
        bail!("rename/move did not preserve the fixture and its bytes");
    }
    event(
        &mut log,
        serde_json::json!({"stage":"move_readback_passed"}),
    )?;
    println!("Checking that a colliding rename preserves both files.");
    let collision = create_file(
        &journal,
        &uploads,
        &graph,
        (&scope, &folder.id, "Collision.txt"),
        b"keep this second fixture",
        &cancel,
    )
    .await?;
    apply(
        &journal,
        &mutations,
        request(MutationIntent::Relocate {
            before: collision.clone(),
            parent: destination.id.clone(),
            name: moved.name.clone(),
        }),
        MutationState::Conflict,
        &mut log,
        "collision_refused",
    )
    .await?;
    let unchanged = graph.node(&scope, &collision.id, &cancel).await?;
    if unchanged.etag != collision.etag
        || unchanged.parent_id != collision.parent_id
        || unchanged.name != collision.name
    {
        bail!("collision source changed unexpectedly");
    }
    let unchanged = graph.node(&scope, &moved.id, &cancel).await?;
    if unchanged.etag != moved.etag
        || graph
            .read_range(&scope, &unchanged, 0, 1024, &cancel)
            .await?
            != DATA
    {
        bail!("collision destination changed unexpectedly");
    }
    println!("Checking rejection of a stale rename.");
    apply(
        &journal,
        &mutations,
        request(MutationIntent::Relocate {
            before: original,
            parent: folder.id.clone(),
            name: "Stale.txt".into(),
        }),
        MutationState::Conflict,
        &mut log,
        "stale_rename_refused",
    )
    .await?;
    let unchanged = graph.node(&scope, &moved.id, &cancel).await?;
    if unchanged.etag != moved.etag {
        bail!("stale rename changed the fixture");
    }
    println!("Checking conditional removal of another new fixture file.");
    let disposable = create_file(
        &journal,
        &uploads,
        &graph,
        (&scope, &folder.id, "Remove-me.txt"),
        b"disposable synthetic fixture",
        &cancel,
    )
    .await?;
    apply(
        &journal,
        &mutations,
        request(MutationIntent::RemoveFile {
            before: disposable.clone(),
        }),
        MutationState::Applied,
        &mut log,
        "file_removed",
    )
    .await?;
    if !matches!(
        graph.node(&scope, &disposable.id, &cancel).await,
        Err(ProviderError::NotFound)
    ) {
        bail!("removed fixture is still visible or its status could not be checked");
    }
    println!("Checking that a stale delete preserves the newer revision.");
    let stale = create_file(
        &journal,
        &uploads,
        &graph,
        (&scope, &folder.id, "Keep-me.txt"),
        DATA,
        &cancel,
    )
    .await?;
    let current = node(
        apply(
            &journal,
            &mutations,
            request(MutationIntent::Relocate {
                before: stale.clone(),
                parent: folder.id.clone(),
                name: "Keep-newer.txt".into(),
            }),
            MutationState::Applied,
            &mut log,
            "delete_fixture_renamed",
        )
        .await?,
    )?;
    apply(
        &journal,
        &mutations,
        request(MutationIntent::RemoveFile { before: stale }),
        MutationState::Conflict,
        &mut log,
        "stale_delete_refused",
    )
    .await?;
    let kept = graph.node(&scope, &current.id, &cancel).await?;
    if kept.etag != current.etag || graph.read_range(&scope, &kept, 0, 1024, &cancel).await? != DATA
    {
        bail!("stale delete did not preserve the newer fixture");
    }
    println!("Checking folder rename and move with an existing child.");
    let current_folder = graph.node(&scope, &destination.id, &cancel).await?;
    let renamed_folder = node(
        apply(
            &journal,
            &mutations,
            request(MutationIntent::Relocate {
                before: current_folder,
                parent: folder.id.clone(),
                name: "Renamed destination".into(),
            }),
            MutationState::Applied,
            &mut log,
            "folder_renamed",
        )
        .await?,
    )?;
    let container = node(
        apply(
            &journal,
            &mutations,
            request(MutationIntent::CreateFolder {
                parent: folder.id.clone(),
                name: "Container".into(),
            }),
            MutationState::Applied,
            &mut log,
            "container_created",
        )
        .await?,
    )?;
    let moved_folder = node(
        apply(
            &journal,
            &mutations,
            request(MutationIntent::Relocate {
                before: renamed_folder.clone(),
                parent: container.id.clone(),
                name: renamed_folder.name.clone(),
            }),
            MutationState::Applied,
            &mut log,
            "folder_moved",
        )
        .await?,
    )?;
    let confirmed_folder = graph.node(&scope, &moved_folder.id, &cancel).await?;
    let child = graph.node(&scope, &moved.id, &cancel).await?;
    if confirmed_folder.parent_id.as_ref() != Some(&container.id)
        || child.parent_id.as_ref() != Some(&moved_folder.id)
        || graph.read_range(&scope, &child, 0, 1024, &cancel).await? != DATA
    {
        bail!("moving the fixture folder did not preserve the child and its bytes");
    }
    event(
        &mut log,
        serde_json::json!({"stage":"folder_child_readback_passed"}),
    )?;
    event(
        &mut log,
        serde_json::json!({"stage":"namespace_checks_passed"}),
    )?;
    println!(
        "Namespace checks passed. Other test files, local snapshots and conflict records remain for inspection."
    );
    println!(
        "Writable FUSE saves, folder removal and the broader recovery matrix remain separate release gates."
    );
    Ok(())
}
