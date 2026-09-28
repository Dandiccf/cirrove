//! A real FUSE create check, confined to a new Cirrove-owned iCloud folder.
//! Neither account settings nor the installed daemon are changed.
#[path = "cirrove-icloud-mounted-write-probe/fixture.rs"]
mod fixture;

use anyhow::{Context, Result, ensure};
use cirrove_auth::{AccessMode, AppRegistration, CredentialVault, DesktopVault};
use cirrove_core::mutation::MutationReceipt;
use cirrove_core::{Node, NodeKind, Scope};
use cirrove_icloud::{
    ICloudDrive, ICloudOwnedFixtureFolderCreate, ICloudOwnedFixtureUpload, ICloudReadSession,
    SealedSessionVault,
};
use cirrove_service::{
    accounts::Settings,
    engine::Engine,
    journal::{MutationState, UploadJournal, UploadState},
    private_dir,
    writable::WritableSession,
};
use fixture::Fixture;
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;

const APP: &str = r#"
import os, sys
mount = sys.argv[1]
name = sys.argv[2]
path = os.path.join(mount, name)
fd = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
try:
    os.write(fd, b'Cirrove isolated mounted iCloud validation\n')
    os.fsync(fd)
finally:
    os.close(fd)
os.mkdir(os.path.join(mount, 'Mounted Folder'))
"#;

#[tokio::main]
async fn main() -> Result<()> {
    let state = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local-state/icloud-gui-connect-validation/state")
        .canonicalize()
        .context("isolated iCloud validation state unavailable")?;
    let settings = Settings::load(&state)?;
    let accounts: Vec<_> = settings
        .accounts
        .iter()
        .filter(|account| {
            matches!(account.registration, AppRegistration::ICloud)
                && account.label == "iCloudGuiValidation"
        })
        .collect();
    ensure!(
        accounts.len() == 1,
        "expected exactly one isolated iCloud validation account"
    );
    let account = accounts[0];
    let vault = SealedSessionVault::new(&state, &account.id)?;
    let snapshot = vault
        .load(&account.credential_id)
        .await?
        .context("isolated iCloud session missing; sign in locally")?;
    let apple_id = &account.identity.username;
    let mut creator = ICloudReadSession::from_session_snapshot(&snapshot, apple_id)?;
    creator
        .list_root()
        .await
        .context("isolated iCloud session is not usable")?;

    let run = Uuid::new_v4();
    let run_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../.local-state/icloud-mounted-write-{run}"));
    private_dir(&run_dir)?;
    let folder = creator
        .create_validation_folder(&format!("Cirrove Write Validation-{run}"))
        .await
        .context("creating the dedicated iCloud test folder")?;
    let scope = Scope {
        account: account.id.clone(),
        provider: "icloud".into(),
        collection: "drive".into(),
    };
    let root = Node {
        id: folder.id().into(),
        parent_id: None,
        name: folder.name().into(),
        kind: NodeKind::Folder,
        size: 0,
        modified_unix: 0,
        etag: None,
        content_version: None,
        target: None,
        package: false,
    };
    let provider = Arc::new(Fixture::new(
        scope.clone(),
        root,
        ICloudDrive::on_demand_from_session_snapshot(scope.clone(), apple_id, &snapshot)?,
        ICloudOwnedFixtureUpload::new(
            scope.clone(),
            ICloudReadSession::from_session_snapshot(&snapshot, apple_id)?,
            folder.clone(),
        )?,
        ICloudOwnedFixtureFolderCreate::new(
            scope,
            ICloudReadSession::from_session_snapshot(&snapshot, apple_id)?,
            folder.clone(),
            Arc::new(DesktopVault),
        )?,
    ));
    let mut config = account.clone();
    config.enabled = false;
    config.access = AccessMode::ReadWrite;
    config.root_id = folder.id().into();
    config.mount_path = run_dir.join("mount");
    config.poll_seconds = 3600;
    config.cache_bytes = 64 * 1024 * 1024;
    private_dir(&config.mount_path)?;
    let engine = Engine::new(config, provider.clone(), run_dir.join("engine")).await?;
    let journal = Arc::new(Mutex::new(UploadJournal::open(
        &run_dir.join("journal"),
        &account.id,
        64 * 1024 * 1024,
    )?));
    let session =
        WritableSession::mount(engine.clone(), journal, provider, Arc::new(DesktopVault)).await?;
    println!(
        "Isolated iCloud test mount active: {}",
        engine.account.mount_path.display()
    );
    let check = async {
        let filename = "Mounted Create.txt";
        let output = tokio::process::Command::new("python3")
            .args(["-c", APP])
            .arg(&engine.account.mount_path)
            .arg(filename)
            .kill_on_drop(true)
            .output()
            .await
            .context("running separate FUSE write process")?;
        ensure!(
            output.status.success(),
            "FUSE write process failed; evidence retained"
        );
        let uploads = loop {
            let rows = session.uploads(0, 16).await?;
            ensure!(
                !rows
                    .iter()
                    .any(|r| matches!(r.state, UploadState::Failed | UploadState::Conflict)),
                "mounted upload requires review"
            );
            if rows.len() == 1 && rows[0].state == UploadState::Uploaded {
                break rows;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        };
        let mutations = loop {
            let rows = session.mutations(0, 16).await?;
            ensure!(
                !rows
                    .iter()
                    .any(|r| matches!(r.state, MutationState::Failed | MutationState::Conflict)),
                "mounted folder create requires review"
            );
            if rows.len() == 1 && rows[0].state == MutationState::Applied {
                break rows;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        };
        let mut independent = ICloudReadSession::from_session_snapshot(&snapshot, apple_id)?;
        let children = independent.list_folder(folder.id()).await?;
        let file = children
            .iter()
            .find(|entry| entry.display_name() == filename && !entry.is_folder())
            .context("independent iCloud listing does not contain mounted file")?;
        let created_folder = children
            .iter()
            .find(|entry| entry.display_name() == "Mounted Folder" && entry.is_folder())
            .context("independent iCloud listing does not contain mounted folder")?;
        let uploaded = uploads[0]
            .remote
            .as_ref()
            .context("uploaded journal record lacks remote identity")?;
        ensure!(
            uploaded.id == file.drivewsid && uploaded.parent_id.as_deref() == Some(folder.id()),
            "independent file identity differs from uploaded receipt"
        );
        let Some(MutationReceipt::Upsert(created)) = mutations[0].receipt.as_ref() else {
            anyhow::bail!("folder mutation lacks a remote upsert receipt");
        };
        ensure!(
            created.id == created_folder.drivewsid
                && created.parent_id.as_deref() == Some(folder.id()),
            "independent folder identity differs from mutation receipt"
        );
        let bytes = independent
            .read_small_file_in_folder(folder.id(), &file.drivewsid)
            .await?;
        ensure!(
            bytes == b"Cirrove isolated mounted iCloud validation\n",
            "remote file bytes differ from mounted write"
        );
        ensure!(
            uploads[0].sha256 == hex::encode(Sha256::digest(&bytes)),
            "journal digest differs from independent read"
        );
        ensure!(
            uploads[0].size == bytes.len() as u64,
            "journal size differs from independent read"
        );
        println!(
            "Mounted file and folder create verified by independent iCloud listing and full-byte read."
        );
        Ok::<(), anyhow::Error>(())
    };
    let result = tokio::select! {
        result = tokio::time::timeout(Duration::from_secs(360), check) =>
            result.map_err(|_| anyhow::anyhow!("mounted iCloud validation timed out; fixture and journal retained"))?,
        _ = tokio::signal::ctrl_c() => Err(anyhow::anyhow!("mounted iCloud validation interrupted; fixture and journal retained")),
    };
    let shutdown = session.shutdown().await;
    result?;
    shutdown?;
    println!(
        "Isolated fixture and journal retained at {}",
        run_dir.display()
    );
    Ok(())
}
