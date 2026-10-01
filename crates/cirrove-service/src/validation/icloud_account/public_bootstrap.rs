//! One preregistered, local-only bootstrap. Never starts a daemon or contacts Apple.
use super::*;
use std::{
    fs::File,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::PathBuf,
};

const RUN: &str = "ec7f82e1-3c33-4a1f-bf01-d6e3f2ca80cc";
const LABEL: &str = "iCloudPublicNativeValidation";
const DIRECTORY: &str = "/var/tmp/cirrove-public-native-ec7f82e1";

fn check_private(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir()
            && metadata.uid() == std::fs::metadata("/proc/self")?.uid()
            && metadata.permissions().mode() & 0o077 == 0,
        "bootstrap directory must be owned and private"
    );
    Ok(())
}
fn claim(path: &Path) -> Result<()> {
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(path)
        .context("bootstrap directory must be new; retain existing attempts")?;
    check_private(path)?;
    File::open(path.parent().context("missing parent")?)?.sync_all()?;
    Ok(())
}
// Match the retained owned-package experiment's explicit disk-backed storage
// contract rather than treating every unrecognized filesystem as safe.
fn checked_filesystem(success: bool, output: &[u8]) -> Result<&'static str> {
    ensure!(
        success && output == b"btrfs\n",
        "bootstrap evidence must use btrfs storage"
    );
    Ok("btrfs")
}
fn disk_filesystem(path: &Path) -> Result<&'static str> {
    let output = std::process::Command::new("findmnt")
        .args(["-n", "-o", "FSTYPE", "-T"])
        .arg(path)
        .output()?;
    checked_filesystem(output.status.success(), &output.stdout)
}
fn source_account(settings: &Settings) -> Result<&Account> {
    let mut selected = settings
        .accounts
        .iter()
        .filter(|a| a.label == "iCloudGuiValidation");
    let account = selected
        .next()
        .context("isolated GUI account unavailable")?;
    ensure!(
        selected.next().is_none()
            && matches!(account.registration, AppRegistration::ICloud)
            && account.enabled
            && account.access == AccessMode::ReadOnly
            && account.root_id == ROOT_ID,
        "expected one enabled read-only isolated iCloud GUI account"
    );
    Ok(account)
}
pub(super) fn source_identity_matches(account: &Account, retained: &Account) -> Result<()> {
    ensure!(
        matches!(retained.registration, AppRegistration::ICloud)
            && retained.label == "iCloudOwnedPackageValidation"
            && retained.access == AccessMode::ReadOnly
            && !retained.enabled
            && retained.root_id == ROOT_ID
            && retained.drive.id == "drive"
            && account.identity.username == retained.identity.username
            && account.identity.subject == retained.identity.subject
            && account.identity.tenant_id == retained.identity.tenant_id
            && account.identity.graph_user_id == retained.identity.graph_user_id,
        "GUI account differs from retained owned source identity"
    );
    Ok(())
}
fn retained_source_account() -> Result<Account> {
    use std::io::Read;
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(
        "../../.local-state/icloud-owned-package-create-ac9e5456-bd10-4b7d-9215-21bbb85dde69/account.json");
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == std::fs::metadata("/proc/self")?.uid()
            && metadata.permissions().mode() & 0o077 == 0
            && metadata.len() <= 64 * 1024,
        "invalid retained source account artifact"
    );
    let mut bytes = Vec::new();
    file.take(64 * 1024 + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 64 * 1024,
        "retained source account artifact exceeds bound"
    );
    serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("invalid retained source account artifact"))
}
fn cloned_account(source: &Account, directory: &Path) -> Account {
    let mut account = source.clone();
    account.id = Uuid::new_v4().to_string();
    account.credential_id = Uuid::new_v4().to_string();
    account.label = LABEL.into();
    account.enabled = true;
    account.access = AccessMode::ReadWrite;
    account.root_id = ROOT_ID.into();
    account.mount_path = directory.join("mount");
    account.poll_seconds = 60;
    account.cache_bytes = 64 * 1024 * 1024;
    account
}
fn publish_settings(state: &Path, account: Account) -> Result<()> {
    check_private(state)?;
    let settings = Settings {
        version: 2,
        accounts: vec![account],
    };
    let prepared = state.join("accounts.prepared.json");
    record(&prepared, &settings)?;
    // A hard link publishes the fully fsynced inode atomically and refuses an
    // existing destination. Both names are retained as preparation evidence.
    std::fs::hard_link(&prepared, state.join("accounts.json"))?;
    File::open(state)?.sync_all()?;
    let loaded = Settings::load(state)?;
    ensure!(
        serde_json::to_vec(&loaded)? == serde_json::to_vec(&settings)?,
        "prepared settings did not round trip"
    );
    Ok(())
}

/// Local keyring access only. The fixed run is single-use even after failure.
/// Failed artifacts/keys remain retained; this helper never performs cleanup.
pub async fn icloud_public_native_bootstrap(run: Uuid) -> Result<()> {
    ensure!(
        run.to_string() == RUN,
        "run is not the preregistered public native import"
    );
    let directory = PathBuf::from(DIRECTORY);
    claim(&directory)?;
    let filesystem = disk_filesystem(&directory)?;
    let state = directory.join("state");
    claim(&state)?;
    claim(&directory.join("mount"))?;
    claim(&directory.join("staging"))?;
    let mut binary = File::open(std::env::current_exe()?)?;
    let binary_sha256 = hex::encode({
        let mut hash = Sha256::new();
        std::io::copy(&mut binary, &mut HashWriter(&mut hash))?;
        hash.finalize()
    });
    record(
        &directory.join("bootstrap-started.json"),
        &serde_json::json!({
            "run": run, "pid": std::process::id(), "binary_sha256": binary_sha256,
            "filesystem": filesystem,
            "started_unix": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs(),
            "local_only": true, "starts_daemon": false, "source_label": "iCloudGuiValidation",
            "state": state, "socket": directory.join("control.sock"),
            "whole_drive_mount": true,
            "owned_parent": "Cirrove Package Validation ac9e5456-bd10-4b7d-9215-21bbb85dde69",
            "destination": format!("Cirrove Public Import {run}.pages")
        }),
    )?;
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local-state/icloud-gui-connect-validation/state")
        .canonicalize()?;
    check_private(&source)?;
    let settings = Settings::load(&source)?;
    let original = source_account(&settings)?;
    source_identity_matches(original, &retained_source_account()?)?;
    let snapshot = SealedSessionVault::new(&source, &original.id)?
        .load(&original.credential_id)
        .await
        .map_err(|_| anyhow::anyhow!("source session could not be loaded"))?
        .context("source session unavailable; sign in locally")?;
    let account = cloned_account(original, &directory);
    ensure!(
        account.id != original.id && account.credential_id != original.credential_id,
        "bootstrap identity collision"
    );
    SealedSessionVault::new(&state, &account.id)?
        .save(&account.credential_id, snapshot)
        .await
        .map_err(|_| anyhow::anyhow!("fresh session could not be saved"))?;
    publish_settings(&state, account.clone())?;
    record(
        &directory.join("bootstrap-ready.json"),
        &serde_json::json!({
            "run": run, "account": account.id, "label": LABEL,
            "state": state, "mount": account.mount_path,
            "socket": directory.join("control.sock"), "session_resealed": true,
            "source_unchanged_by_helper": true, "cloud_contacted": false
        }),
    )?;
    File::open(&directory)?.sync_all()?;
    println!("Local isolated public-import bootstrap ready; daemon not started.");
    Ok(())
}
struct HashWriter<'a>(&'a mut Sha256);
impl std::io::Write for HashWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
