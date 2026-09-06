//! Atomic non-secret account configuration. Tokens are exclusively in Secret Service.
use crate::private_dir;
use anyhow::{Context, Result, bail};
use cirrove_auth::{
    AccessMode, AppRegistration, CredentialVault, DesktopVault, Identity, PendingLogin,
    TokenBroker, save_credentials,
};
use cirrove_core::CancellationToken;
use cirrove_onedrive::{DriveInfo, OneDrive, StaticToken};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub label: String,
    pub registration: AppRegistration,
    pub identity: Identity,
    pub credential_id: String,
    #[serde(default)]
    pub access: AccessMode,
    pub drive: DriveInfo,
    pub root_id: String,
    pub mount_path: PathBuf,
    pub enabled: bool,
    pub poll_seconds: u64,
    pub cache_bytes: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    pub version: u32,
    pub accounts: Vec<Account>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            version: 1,
            accounts: vec![],
        }
    }
}
impl Settings {
    pub fn load(state: &Path) -> Result<Self> {
        match std::fs::read(state.join("accounts.json")) {
            Ok(bytes) => {
                if bytes.len() > 1024 * 1024 {
                    bail!("account settings exceed limit");
                }
                let settings: Self =
                    serde_json::from_slice(&bytes).context("invalid Cirrove account settings")?;
                settings.validate()?;
                Ok(settings)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }
    pub fn validate(&self) -> Result<()> {
        if self.version != 1 {
            bail!("unsupported account settings version");
        }
        let mut ids = std::collections::HashSet::new();
        let mut labels = std::collections::HashSet::new();
        let mut paths = std::collections::HashSet::new();
        for account in &self.accounts {
            account.registration.validate()?;
            if !valid_label(&account.label)
                || uuid::Uuid::parse_str(&account.id).is_err()
                || uuid::Uuid::parse_str(&account.credential_id).is_err()
                || account.drive.id.is_empty()
                || account.root_id.is_empty()
                || !account.mount_path.is_absolute()
                || account
                    .mount_path
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
                || account
                    .mount_path
                    .components()
                    .collect::<PathBuf>()
                    .as_os_str()
                    != account.mount_path.as_os_str()
                || account.poll_seconds < 10
                || account.cache_bytes < 8 * 1024 * 1024
            {
                bail!("invalid account configuration");
            }
            if !ids.insert(&account.id)
                || !labels.insert(&account.label)
                || !paths.insert(&account.mount_path)
            {
                bail!("duplicate account or mount path");
            }
        }
        Ok(())
    }
    fn save(&self, state: &Path) -> Result<()> {
        self.validate()?;
        let tmp = state.join(format!(".accounts-{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&tmp)?;
            file.write_all(&serde_json::to_vec_pretty(self)?)?;
            file.sync_all()?;
            std::fs::rename(&tmp, state.join("accounts.json"))?;
            File::open(state)?.sync_all()?;
            Ok(())
        })();
        if tmp.exists() {
            let _ = std::fs::remove_file(tmp);
        }
        result
    }
}
pub fn valid_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 48
        && label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
pub fn config_lock(state: &Path) -> Result<File> {
    private_dir(state)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(state.join("settings.lock"))?;
    fs2::FileExt::try_lock_exclusive(&file)
        .context("another Cirrove settings operation is running")?;
    Ok(file)
}
pub fn daemon_lock(state: &Path) -> Result<File> {
    private_dir(state)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(state.join("daemon.lock"))?;
    fs2::FileExt::try_lock_exclusive(&file)
        .context("another Cirrove daemon already owns these accounts")?;
    Ok(file)
}
pub fn account_lock(directory: &Path) -> Result<File> {
    private_dir(directory)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(directory.join("owner.lock"))?;
    fs2::FileExt::try_lock_exclusive(&file).context("Cirrove account is still stopping")?;
    Ok(file)
}
pub(crate) fn account_operation(state: &Path, id: &str) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(state.join(format!("operation-{id}.lock")))?;
    fs2::FileExt::try_lock_exclusive(&file)
        .context("another operation is changing this account")?;
    Ok(file)
}
async fn browser_login(
    app: AppRegistration,
    access: AccessMode,
) -> Result<(Identity, cirrove_auth::Credentials)> {
    let pending = PendingLogin::with_access(app, access).await?;
    if access == AccessMode::ReadWrite {
        println!(
            "Requesting write consent for developer validation. Filesystem mounts remain read-only."
        );
    }
    println!("Opening Microsoft sign-in in your browser. Select the account you want to connect.");
    let mut child = tokio::process::Command::new("xdg-open")
        .arg(pending.authorization_url().as_str())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("could not start the browser")?;
    // Some launchers remain alive as long as a newly started browser. Receiving
    // the callback must not depend on the launcher exiting, or close that browser.
    await_browser_login(&mut child, pending.finish()).await
}
async fn await_browser_login<T>(
    child: &mut tokio::process::Child,
    login: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    tokio::pin!(login);
    tokio::select! {biased;
        result=&mut login=>result,
        status=child.wait()=>{
            if !status.context("browser launcher failed")?.success(){bail!("browser could not be opened; check xdg-open");}
            login.await
        }
    }
}
/// Temporarily disables just this account and waits for its owned workers to
/// release credentials before replacing a grant. Other accounts keep running.
pub async fn reauthenticate(state: PathBuf, label: String) -> Result<()> {
    let original = Settings::load(&state)?
        .accounts
        .into_iter()
        .find(|a| a.label == label)
        .context("unknown account label")?;
    let _operation = account_operation(&state, &original.id)?;
    {
        let _lock = config_lock(&state)?;
        let mut settings = Settings::load(&state)?;
        let account = settings
            .accounts
            .iter_mut()
            .find(|a| a.id == original.id)
            .context("account removed")?;
        account.enabled = false;
        settings.save(&state)?;
    }
    let result = async {
        let directory = state.join("accounts").join(&original.id);
        let _owner = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                if let Ok(owner) = account_lock(&directory) {
                    break owner;
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        })
        .await
        .context("account did not stop; close files in this mount and try again")?;
        let (identity, credentials) =
            browser_login(original.registration.clone(), original.access).await?;
        if identity.tenant_id != original.identity.tenant_id
            || identity.graph_user_id != original.identity.graph_user_id
        {
            bail!("different account selected; use connect with a new label to add it");
        }
        let graph = OneDrive::new(
            original.id.clone(),
            Arc::new(StaticToken(credentials.access_token())),
        )?;
        graph
            .root(&original.drive.id, &CancellationToken::new())
            .await?;
        save_credentials(&DesktopVault, &original.credential_id, &credentials).await?;
        Ok(())
    }
    .await;
    {
        let _lock = config_lock(&state)?;
        let mut settings = Settings::load(&state)?;
        let account = settings
            .accounts
            .iter_mut()
            .find(|a| a.id == original.id)
            .context("account removed during sign-in")?;
        account.enabled = original.enabled;
        settings.save(&state)?;
    }
    result
}
pub fn provider(account: &Account) -> Result<Arc<OneDrive>> {
    let broker = TokenBroker::new(
        account.registration.clone(),
        account.identity.clone(),
        account.credential_id.clone(),
        Arc::new(DesktopVault),
    )?;
    Ok(Arc::new(OneDrive::new(
        account.id.clone(),
        Arc::new(broker),
    )?))
}
/// Complete browser sign-in, display verified identity and let the caller choose a
/// drive. Persistence happens only after the selected drive's root is verified.
pub async fn connect(
    state: PathBuf,
    label: String,
    app: AppRegistration,
    mount_path: PathBuf,
    drive_id: Option<String>,
    access: AccessMode,
) -> Result<()> {
    if !valid_label(&label) {
        bail!("use a label of 1–48 letters, digits, hyphens or underscores");
    }
    if !mount_path.is_absolute() {
        bail!("mount path must be absolute");
    }
    crate::manager::validate_mount_directory(&mount_path)?;
    let mount_path = std::fs::canonicalize(mount_path)?;
    {
        let _lock = config_lock(&state)?;
        let settings = Settings::load(&state)?;
        if settings
            .accounts
            .iter()
            .any(|a| a.label == label || a.mount_path == mount_path)
        {
            bail!("this label or mount path is already configured");
        }
    }
    let (identity, credentials) = browser_login(app.clone(), access).await?;
    println!(
        "Signed in: {} ({})\nTenant: {}",
        identity.display_name, identity.username, identity.tenant_id
    );
    let id = uuid::Uuid::new_v4().to_string();
    let graph = OneDrive::new(
        id.clone(),
        Arc::new(StaticToken(credentials.access_token())),
    )?;
    let cancel = CancellationToken::new();
    let drive = if let Some(drive) = drive_id {
        graph.drive(&drive, &cancel).await?
    } else {
        let drives = graph.drives(&cancel).await?;
        for (index, drive) in drives.iter().enumerate() {
            println!(
                "{}. {} · {}\n   {}",
                index + 1,
                drive.name,
                drive.drive_type,
                drive.web_url
            );
        }
        let selected = tokio::task::spawn_blocking(move || -> Result<usize> {
            print!("Drive number to mount (Enter cancels): ");
            std::io::stdout().flush()?;
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            line.trim()
                .parse::<usize>()
                .context("drive selection cancelled or invalid")
        })
        .await??;
        drives
            .get(selected.checked_sub(1).context("invalid drive number")?)
            .context("invalid drive number")?
            .clone()
    };
    let root = graph.root(&drive.id, &cancel).await?;
    if mount_path.exists() && std::fs::read_dir(&mount_path)?.next().is_some() {
        bail!("mount directory must be empty");
    }
    let account = Account {
        id,
        label,
        registration: app,
        identity,
        credential_id: uuid::Uuid::new_v4().to_string(),
        access,
        drive,
        root_id: root.id,
        mount_path,
        enabled: access == AccessMode::ReadOnly,
        poll_seconds: 30,
        cache_bytes: 5 * 1024 * 1024 * 1024,
    };
    let _lock = config_lock(&state)?;
    let mut settings = Settings::load(&state)?;
    if settings
        .accounts
        .iter()
        .any(|a| a.label == account.label || a.mount_path == account.mount_path)
    {
        bail!("account settings changed during sign-in; try another label or path");
    }
    save_credentials(&DesktopVault, &account.credential_id, &credentials).await?;
    settings.accounts.push(account.clone());
    if let Err(error) = settings.save(&state) {
        let _ = DesktopVault.remove(&account.credential_id).await;
        return Err(error);
    }
    println!(
        "Connected {}. Mount location: {}",
        account.label,
        account.mount_path.display()
    );
    Ok(())
}
pub fn set_enabled(state: &Path, label: &str, enabled: bool) -> Result<()> {
    let _lock = config_lock(state)?;
    let mut settings = Settings::load(state)?;
    let account = settings
        .accounts
        .iter_mut()
        .find(|a| a.label == label)
        .context("unknown account label")?;
    let _operation = account_operation(state, &account.id)?;
    account.enabled = enabled;
    settings.save(state)
}
pub async fn keyring_check() -> Result<()> {
    let key = format!("selftest-{}", uuid::Uuid::new_v4());
    let value = secrecy::SecretString::from("cirrove-synthetic-keyring-check");
    let vault = DesktopVault;
    vault.save(&key, value).await?;
    let result = vault.load(&key).await;
    vault.remove(&key).await?;
    use secrecy::ExposeSecret;
    if result?.as_ref().map(|v| v.expose_secret()) != Some("cirrove-synthetic-keyring-check") {
        bail!("desktop keyring verification failed");
    }
    println!("Desktop keyring write/read/removal passed using a synthetic Cirrove credential.");
    Ok(())
}
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[test]
    fn config_writes_are_atomic_and_daemon_ownership_is_exclusive() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        private_dir(&state).unwrap();
        let lock = daemon_lock(&state).unwrap();
        assert!(daemon_lock(&state).is_err());
        drop(lock);
        // Parallel tests launch child processes. Between fork and exec a child
        // can retain the open description underlying flock, even with CLOEXEC.
        // Require prompt eventual release; do not mistake that short interval
        // for a leaked owner or ignore errors other than lock contention.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            match daemon_lock(&state) {
                Ok(reacquired) => {
                    drop(reacquired);
                    break;
                }
                Err(error) => {
                    assert!(
                        error
                            .downcast_ref::<std::io::Error>()
                            .is_some_and(|e| e.kind() == std::io::ErrorKind::WouldBlock),
                        "{error:#}"
                    );
                    assert!(
                        std::time::Instant::now() < deadline,
                        "owner was not released: {error:#}"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
        }
        Settings::default().save(&state).unwrap();
        assert!(Settings::load(&state).unwrap().accounts.is_empty());
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(state.join("accounts.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    #[test]
    fn labels_cannot_escape_mount_names() {
        for label in ["../escape", "a/b", "", "spaces here"] {
            assert!(!valid_label(label));
        }
        assert!(valid_label("work-onedrive"));
    }
    #[tokio::test]
    async fn login_completion_does_not_wait_for_or_kill_the_browser_launcher() {
        let mut child = tokio::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            await_browser_login(&mut child, async { Ok(7) }),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result, 7);
        assert!(child.try_wait().unwrap().is_none());
        child.kill().await.unwrap();
    }
}
