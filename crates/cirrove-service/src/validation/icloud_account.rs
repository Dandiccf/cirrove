//! Explicit account-router experiment. No mount and no ordinary service changes.
//! Every queued mutation targets this run's newly created file and folder only.
use crate::{
    accounts::{Account, Settings},
    engine::Engine,
    icloud_writes::ICloudWriteProvider,
    journal::UploadState,
    manager::WriteContext,
    private_dir,
    transfers::TransferWorker,
};
use anyhow::{Context, Result, ensure};
use cirrove_auth::{AccessMode, AppRegistration, CredentialVault};
use cirrove_core::{CancellationToken, Node, NodeKind, Scope, upload::UploadIntent};
use cirrove_icloud::{ICloudDrive, ICloudReadSession, ROOT_ID, SealedSessionVault};
use cirrove_store::Store;
use secrecy::SecretString;
use sha2::{Digest, Sha256};
use std::{
    fs::OpenOptions,
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::Path,
    sync::Arc,
};
use uuid::Uuid;

const FIRST: &[u8] = b"Cirrove account-router original\n";
const SECOND: &[u8] = b"Cirrove account-router replacement\n";
const NAME: &str = "Account Router.txt";

fn record(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    Ok(())
}

/// Called only by the feature-gated developer binary with an explicit run UUID.
/// A reused run directory is refused, so an ambiguous attempt cannot be replayed.
pub async fn icloud_account_uploads(run: Uuid) -> Result<()> {
    let Fixture {
        state,
        account,
        scope,
        snapshot,
        parent,
        run_dir,
    } = prepare(run, "uploads").await?;
    let original = transfer(&state, &account, &scope, &snapshot, &parent, None, FIRST).await?;
    verify(&snapshot, &account, &parent, &original, FIRST).await?;
    record(&run_dir.join("create-verified.json"), &original)?;
    println!("Account router: create and independent digest verified");
    // transfer() drops Engine, router, journal and vault before this second arm.
    let replacement = transfer(
        &state,
        &account,
        &scope,
        &snapshot,
        &parent,
        Some(&original),
        SECOND,
    )
    .await?;
    ensure!(
        replacement.id != original.id,
        "replacement must retain the original under its old identity"
    );
    verify(&snapshot, &account, &parent, &replacement, SECOND).await?;
    let mut independent =
        ICloudReadSession::from_session_snapshot(&snapshot, &account.identity.username)?;
    ensure!(
        independent.exact_item_in_trash(&original.id).await?,
        "original identity is not in recoverable Trash"
    );
    record(&run_dir.join("replace-verified.json"), &replacement)?;
    // Reopen the persisted state, without a writer running, and inspect both receipts.
    let engine = engine(&state, &account, &scope, &snapshot).await?;
    let context = WriteContext::open(&engine, &state).await?;
    let rows = context
        .journal()
        .lock()
        .map_err(|_| anyhow::anyhow!("journal lock failed"))?
        .list(0, 8)?;
    ensure!(
        rows.len() == 2 && rows.iter().all(|row| row.state == UploadState::Uploaded),
        "fresh journal did not retain both successful operations"
    );
    ensure!(
        rows[0].remote.as_ref() == Some(&original) && rows[1].remote.as_ref() == Some(&replacement),
        "persisted receipts changed after reopening"
    );
    record(
        &run_dir.join("passed.json"),
        &serde_json::json!({"run": run, "create": true, "replace": true, "reopened_journal": true, "mounted": false}),
    )?;
    println!("Account router: replacement, independent digest and reopened journal verified");
    Ok(())
}

mod mounted;
pub use mounted::{
    icloud_account_empty_read, icloud_account_mounted, icloud_account_mounted_empty_replace,
    icloud_account_mounted_large,
};
mod namespace;
pub use namespace::{icloud_account_combined, icloud_account_namespace};

struct Fixture {
    state: std::path::PathBuf,
    account: Account,
    scope: Scope,
    snapshot: SecretString,
    parent: Node,
    run_dir: std::path::PathBuf,
}

async fn prepare(run: Uuid, kind: &str) -> Result<Fixture> {
    prepare_with_budget(run, kind, 64 * 1024 * 1024).await
}

async fn prepare_with_budget(run: Uuid, kind: &str, budget: u64) -> Result<Fixture> {
    ensure!(budget >= 8 * 1024 * 1024, "invalid fixture budget");
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.local-state");
    let source = base
        .join("icloud-gui-connect-validation/state")
        .canonicalize()?;
    let settings = Settings::load(&source)?;
    let accounts: Vec<_> = settings
        .accounts
        .iter()
        .filter(|a| {
            matches!(a.registration, AppRegistration::ICloud) && a.label == "iCloudGuiValidation"
        })
        .collect();
    ensure!(
        accounts.len() == 1,
        "expected exactly one isolated iCloud validation account"
    );
    let source_account = accounts[0];
    let snapshot = SealedSessionVault::new(&source, &source_account.id)?
        .load(&source_account.credential_id)
        .await?
        .context("isolated session unavailable; sign in locally")?;
    let run_dir = base.join(format!("icloud-account-{kind}-{run}"));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&run_dir)
        .context("run directory must be new; retained attempts are never replayed")?;
    private_dir(&run_dir)?;
    let state = run_dir.join("state");
    private_dir(&state)?;
    let mut account = source_account.clone();
    account.id = Uuid::new_v4().to_string();
    account.credential_id = Uuid::new_v4().to_string();
    account.label = "iCloudAccountRouterValidation".into();
    account.enabled = false;
    account.access = AccessMode::ReadWrite;
    account.root_id = ROOT_ID.into();
    account.mount_path = run_dir.join("unused-mount");
    account.cache_bytes = budget;
    record(&run_dir.join("account.json"), &account)?;
    SealedSessionVault::new(&state, &account.id)?
        .save(&account.credential_id, snapshot.clone())
        .await?;
    let mut remote =
        ICloudReadSession::from_session_snapshot(&snapshot, &account.identity.username)?;
    remote
        .list_root()
        .await
        .context("isolated session is not usable")?;
    let folder = remote
        .create_validation_folder(&format!("Cirrove Write Validation-{run}"))
        .await?;
    let parent = Node {
        id: folder.id().into(),
        parent_id: Some(ROOT_ID.into()),
        name: folder.name().into(),
        kind: NodeKind::Folder,
        size: 0,
        modified_unix: 0,
        etag: None,
        content_version: None,
        target: None,
        package: false,
    };
    record(&run_dir.join("owned-folder.json"), &parent)?;
    let scope = Scope {
        account: account.id.clone(),
        provider: "icloud".into(),
        collection: "drive".into(),
    };
    Ok(Fixture {
        state,
        account,
        scope,
        snapshot,
        parent,
        run_dir,
    })
}

/// One new zero-byte operation, never resuming a previous uncertain attempt.
pub async fn icloud_account_empty(run: Uuid) -> Result<()> {
    let f = prepare(run, "empty").await?;
    let mut remote =
        ICloudReadSession::from_session_snapshot(&f.snapshot, &f.account.identity.username)?;
    let folder = remote.validation_folder_at_root(&f.parent.id).await?;
    ensure!(folder.name() == f.parent.name, "owned root changed");
    let engine = engine(&f.state, &f.account, &f.scope, &f.snapshot).await?;
    let context = WriteContext::open(&engine, &f.state).await?;
    let row = {
        let journal = context.journal();
        let mut journal = journal
            .lock()
            .map_err(|_| anyhow::anyhow!("journal lock failed"))?;
        queue(&mut journal, &f.scope, &f.parent, None, b"")?
    };
    Store::open(context.metadata_db())?.observe_node(&f.scope, &f.parent)?;
    let provider = Arc::new(ICloudWriteProvider::new(&f.account, &context)?);
    let worker = TransferWorker::new(
        context.journal(),
        provider,
        context.checkpoints(),
        CancellationToken::new(),
    );
    let result = worker
        .run_once()
        .await?
        .context("empty operation not selected")?;
    record(
        &f.run_dir.join("result.json"),
        &serde_json::json!({"run":run,
        "state":format!("{:?}",result.state),"issue":result.issue}),
    )?;
    ensure!(
        result.id == row.id && result.state == UploadState::Uploaded,
        "empty operation unconfirmed; retained for inspection without replay"
    );
    let node = context
        .journal()
        .lock()
        .map_err(|_| anyhow::anyhow!("journal lock failed"))?
        .get(row.id)?
        .remote
        .context("empty receipt absent")?;
    ensure!(node.size == 0, "empty receipt has nonzero size");
    let children = remote.list_folder(&f.parent.id).await?;
    ensure!(
        children
            .iter()
            .filter(|entry| entry.drivewsid == node.id
                && entry.size == 0
                && entry.display_name() == NAME
                && !entry.is_folder())
            .count()
            == 1,
        "empty remote metadata not confirmed"
    );
    record(&f.run_dir.join("created.json"), &node)?;
    println!(
        "Empty upload: independently confirmed exact zero-byte item; fresh-process read validation remains open"
    );
    Ok(())
}

/// Read-only diagnosis of the retained combined-relocation setup fixture.
/// Never opens a worker or sends a mutation; names alone are not receipts.
pub async fn icloud_account_combined_inspect(run: Uuid) -> Result<()> {
    let dir = std::env::current_dir()?
        .join(".local-state")
        .join(format!("icloud-account-combined-{run}"));
    let account: Account = serde_json::from_slice(&std::fs::read(dir.join("account.json"))?)?;
    let root: Node = serde_json::from_slice(&std::fs::read(dir.join("owned-folder.json"))?)?;
    ensure!(
        root.name == format!("Cirrove Write Validation-{run}")
            && root.parent_id.as_deref() == Some(ROOT_ID)
            && root.kind == NodeKind::Folder
            && !root.package
            && root.target.is_none(),
        "not a recorded owned validation root"
    );
    let snapshot = SealedSessionVault::new(&dir.join("state"), &account.id)?
        .load(&account.credential_id)
        .await?
        .context("isolated session unavailable")?;
    let mut remote =
        ICloudReadSession::from_session_snapshot(&snapshot, &account.identity.username)?;
    let entries = remote.list_folder(&root.id).await?;
    let summary: Vec<_> = ["Destination", "Combined.txt"]
        .into_iter()
        .map(|name| {
            let matching: Vec<_> = entries
                .iter()
                .filter(|entry| entry.display_name() == name)
                .collect();
            serde_json::json!({"expected_name": name, "matches": matching.len(),
            "folders": matching.iter().filter(|entry| entry.is_folder()).count(),
            "split_extension": matching.iter().filter(|entry| !entry.extension.is_empty()).count(),
            "correct_parent": matching.iter().all(|entry| entry.parent_id == root.id)})
        })
        .collect();
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({"run":run,
        "read_only":true,"total_children":entries.len(),"observations":summary,
        "name_matches_are_not_mutation_receipts":true}))?
    );
    Ok(())
}

async fn engine(
    state: &Path,
    account: &Account,
    scope: &Scope,
    snapshot: &SecretString,
) -> Result<Arc<Engine>> {
    let provider = ICloudDrive::on_demand_from_session_snapshot(
        scope.clone(),
        &account.identity.username,
        snapshot,
    )?;
    // Deliberately not started: no recursive account scan and no filesystem mount.
    Engine::new(account.clone(), Arc::new(provider), state.to_owned()).await
}

async fn transfer(
    state: &Path,
    account: &Account,
    scope: &Scope,
    snapshot: &SecretString,
    parent: &Node,
    original: Option<&Node>,
    bytes: &[u8],
) -> Result<Node> {
    let engine = engine(state, account, scope, snapshot).await?;
    let context = WriteContext::open(&engine, state).await?;
    {
        let mut store = Store::open(context.metadata_db())?;
        store.observe_node(scope, parent)?;
        if let Some(node) = original {
            store.observe_node(scope, node)?;
        }
    }
    let row = {
        let journal = context.journal();
        let mut journal = journal
            .lock()
            .map_err(|_| anyhow::anyhow!("journal lock failed"))?;
        queue(&mut journal, scope, parent, original, bytes)?
    };
    let provider = Arc::new(ICloudWriteProvider::new(account, &context)?);
    let worker = TransferWorker::new(
        context.journal(),
        provider,
        context.checkpoints(),
        CancellationToken::new(),
    );
    let result = worker
        .run_once()
        .await?
        .context("queued operation was not selected")?;
    ensure!(
        result.id == row.id && result.state == UploadState::Uploaded,
        "account-router operation did not complete; durable state retained, no automatic replay"
    );
    let finished = context
        .journal()
        .lock()
        .map_err(|_| anyhow::anyhow!("journal lock failed"))?
        .get(row.id)?;
    finished
        .remote
        .context("completed operation has no receipt")
}

fn queue(
    journal: &mut crate::journal::UploadJournal,
    scope: &Scope,
    parent: &Node,
    original: Option<&Node>,
    bytes: &[u8],
) -> Result<crate::journal::UploadRecord> {
    if let Some(node) = original {
        ensure!(
            node.parent_id.as_ref() == Some(&parent.id)
                && node.name == NAME
                && node.kind == NodeKind::File
                && !node.package
                && node.target.is_none(),
            "replacement source is outside this run's fixture"
        );
        // Exercise the same working-file snapshot and namespace reservation used
        // by FUSE; a bare enqueue has no identity owner for a two-ID handoff.
        let working = journal.create_truncated_working(scope.clone(), node.clone())?;
        let (written, _) = journal.write_working(working.id, 0, bytes)?;
        ensure!(
            written as usize == bytes.len(),
            "short local validation write"
        );
        journal
            .seal_working(working.id)?
            .context("replacement snapshot was not queued")
    } else {
        Ok(journal.enqueue(
            scope.clone(),
            UploadIntent::Create {
                parent: parent.id.clone(),
                name: NAME.into(),
            },
            bytes,
        )?)
    }
}

async fn verify(
    snapshot: &SecretString,
    account: &Account,
    parent: &Node,
    node: &Node,
    bytes: &[u8],
) -> Result<()> {
    ensure!(
        node.parent_id.as_ref() == Some(&parent.id)
            && node.name == NAME
            && node.size == bytes.len() as u64
            && node.kind == NodeKind::File,
        "receipt is not the expected run-owned file"
    );
    let mut remote =
        ICloudReadSession::from_session_snapshot(snapshot, &account.identity.username)?;
    let digest = remote
        .hash_file_in_folder_for_revision(
            &parent.id,
            &node.id,
            node.etag.as_deref().context("receipt has no revision")?,
            node.size,
        )
        .await?;
    ensure!(
        digest == hex::encode(Sha256::digest(bytes)),
        "independent digest mismatch"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_queue_owns_the_exact_original_and_refuses_foreign_sources() {
        let temp = tempfile::tempdir().expect("fixture");
        let scope = Scope {
            account: Uuid::new_v4().to_string(),
            provider: "icloud".into(),
            collection: "drive".into(),
        };
        let mut journal = crate::journal::UploadJournal::open(
            &temp.path().join("journal"),
            &scope.account,
            1024 * 1024,
        )
        .expect("journal");
        let parent = Node {
            id: "owned-folder".into(),
            parent_id: Some(ROOT_ID.into()),
            name: "owned".into(),
            kind: NodeKind::Folder,
            size: 0,
            modified_unix: 0,
            etag: None,
            content_version: None,
            target: None,
            package: false,
        };
        let original = Node {
            id: "owned-file".into(),
            parent_id: Some(parent.id.clone()),
            name: NAME.into(),
            kind: NodeKind::File,
            size: FIRST.len() as u64,
            etag: Some("revision".into()),
            ..parent.clone()
        };
        let foreign = Node {
            parent_id: Some("other-folder".into()),
            ..original.clone()
        };
        assert!(queue(&mut journal, &scope, &parent, Some(&foreign), SECOND).is_err());
        assert!(journal.list(0, 10).expect("rows").is_empty());
        let row = queue(&mut journal, &scope, &parent, Some(&original), SECOND).expect("queue");
        let owner = journal
            .namespace_for_operation(row.id)
            .expect("namespace")
            .expect("owner");
        assert_eq!(owner.remote.as_ref(), Some(&original));
        assert_eq!(
            row.intent,
            UploadIntent::Replace {
                item: original.id,
                expected_etag: "revision".into()
            }
        );
        assert_eq!(row.sha256, hex::encode(Sha256::digest(SECOND)));
    }
}

mod trash_lookup;
pub use trash_lookup::{
    icloud_account_metadata_shapes, icloud_account_trash_lookup,
    icloud_account_trash_lookup_control,
};
