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
        // This used to promise that mounts stayed read-only. That was true while
        // the only writable path was a validator mounting a disabled account by
        // hand; it stopped being true when the daemon learned to mount writable
        // from the grant, and a consent screen is the wrong place to be wrong.
        println!(
            "Requesting write consent. An account with this grant is mounted writable, \
             so applications can change cloud files through it."
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
/// Puts an account's desired state back after a sign-in attempt.
///
/// Sign-in has to disable the account durably, because that is how the daemon is
/// asked to release the mount, and it then waits on a human in a browser. So the
/// window between disabling and restoring is long and easy to interrupt, and an
/// interrupted run used to leave the account disabled with nothing said: the
/// drive simply stopped being mounted, across reboots, until somebody noticed.
///
/// Restoring from `Drop` closes that for a panic or an early return. It cannot
/// close it for process death, which is why `reauthenticate` says out loud what
/// to run if the attempt does not finish.
struct DesiredState {
    state: PathBuf,
    id: String,
    enabled: bool,
    access: Option<AccessMode>,
}
impl DesiredState {
    /// Adopt the requested permission mode, then restore. Consumes the guard so
    /// the `Drop` path cannot also run.
    fn settle(mut self, requested: AccessMode) {
        self.access = Some(requested);
        drop(self);
    }
}
impl Drop for DesiredState {
    fn drop(&mut self) {
        let restore = || -> Result<()> {
            let _lock = config_lock(&self.state)?;
            let mut settings = Settings::load(&self.state)?;
            let account = settings
                .accounts
                .iter_mut()
                .find(|a| a.id == self.id)
                .context("account removed during sign-in")?;
            settle(account, self.enabled, self.access);
            settings.save(&self.state)
        };
        if let Err(error) = restore() {
            tracing::error!(
                %error,
                account = %self.id,
                "could not restore this account's desired state after sign-in; \
                 re-enable it with `cirrove enable <label>`"
            );
        }
    }
}

/// Restore an account after a sign-in attempt, adopting the requested permission
/// mode only if that attempt actually succeeded.
///
/// The recorded mode is what `Writeback::new` consults before allowing writes, so
/// recording a mode the credential does not have is worse than refusing the
/// upgrade: writes would be admitted locally and then refused by the provider,
/// after an application believed its save had been accepted. A refused consent, a
/// different account chosen in the browser and an unreadable drive root all land
/// here as `succeeded == false`.
fn settle(account: &mut Account, enabled: bool, granted: Option<AccessMode>) {
    account.enabled = enabled;
    if let Some(access) = granted {
        account.access = access;
    }
}

/// Sign in again for an existing account, optionally changing its permission mode.
///
/// `access: None` preserves what the account already has, which is the ordinary
/// case: a grant that expired or was revoked. `Some(mode)` requests a different
/// one, and this is the only way to move an account between read-only and
/// writable. Without it the alternatives were both destructive -- `connect`
/// refuses an existing label or mount path, so upgrading meant deleting the
/// account and re-indexing the drive from nothing.
///
/// The stored mode changes only after the browser grant is validated and the
/// selected drive's root is read back, so a refused or abandoned consent leaves
/// the account exactly as it was.
pub async fn reauthenticate(
    state: PathBuf,
    label: String,
    access: Option<AccessMode>,
) -> Result<()> {
    let original = Settings::load(&state)?
        .accounts
        .into_iter()
        .find(|a| a.label == label)
        .context("unknown account label")?;
    let requested = access.unwrap_or(original.access);
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
    // Held from here on. Disabling is durable because it is how the daemon is
    // asked to release the mount; restoring it is this guard's only job.
    let desired = DesiredState {
        state: state.clone(),
        id: original.id.clone(),
        enabled: original.enabled,
        access: None,
    };
    if original.enabled {
        println!(
            "{label} is disabled while you sign in. If this does not finish, run \
             `cirrove enable {label}` to put it back."
        );
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
            browser_login(original.registration.clone(), requested).await?;
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
    if result.is_ok() {
        desired.settle(requested);
    } else {
        drop(desired);
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
    update_enabled(state, |account| account.label == label, enabled)
}
/// Desktop actions address the persisted account identity, never a reusable label.
pub fn set_enabled_by_id(state: &Path, id: &str, enabled: bool) -> Result<()> {
    update_enabled(state, |account| account.id == id, enabled)
}
fn update_enabled(state: &Path, select: impl Fn(&Account) -> bool, enabled: bool) -> Result<()> {
    let _lock = config_lock(state)?;
    let mut settings = Settings::load(state)?;
    let account = settings
        .accounts
        .iter_mut()
        .find(|a| select(a))
        .context("account is no longer configured")?;
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

    /// A permission mode is recorded only when the sign-in that granted it worked.
    ///
    /// `Writeback::new` admits writes on the strength of this field, so recording
    /// a mode the credential does not have is worse than refusing the upgrade: an
    /// application's save would be accepted locally and then refused by the
    /// provider. Adopting `requested` unconditionally makes the read-only rows
    /// below fail.
    #[test]
    fn a_failed_sign_in_never_records_the_permission_it_asked_for() {
        for (had, granted, expected) in [
            (
                AccessMode::ReadOnly,
                Some(AccessMode::ReadWrite),
                AccessMode::ReadWrite,
            ),
            (AccessMode::ReadOnly, None, AccessMode::ReadOnly),
            (
                AccessMode::ReadWrite,
                Some(AccessMode::ReadOnly),
                AccessMode::ReadOnly,
            ),
            (AccessMode::ReadWrite, None, AccessMode::ReadWrite),
        ] {
            for enabled in [false, true] {
                let mut account = fixture_account(had);
                super::settle(&mut account, enabled, granted);
                assert_eq!(account.access, expected, "{had:?}->{granted:?}");
                // The mount's desired state is restored either way; a sign-in
                // attempt is not a way to disable an account by failing.
                assert_eq!(account.enabled, enabled);
            }
        }
    }

    /// An abandoned sign-in puts the account back where it found it.
    ///
    /// Disabling is durable, because it is how the daemon is asked to release the
    /// mount, and what follows is a person in a browser. Without this restore an
    /// interrupted run left the drive unmounted, across reboots, with nothing
    /// said and nothing to read. Deleting the `Drop` impl makes this fail.
    #[test]
    fn an_abandoned_sign_in_restores_the_account_it_disabled() {
        let temp = tempfile::tempdir().expect("fixture");
        let state = temp.path().join("state");
        crate::private_dir(&state).expect("state directory");
        let mut account = fixture_account(AccessMode::ReadOnly);
        account.enabled = true;
        let id = account.id.clone();
        Settings {
            version: 1,
            accounts: vec![account],
        }
        .save(&state)
        .expect("seed");

        // Exactly what reauthenticate does before it waits on the browser.
        let mut disabled = Settings::load(&state).expect("load");
        disabled.accounts[0].enabled = false;
        disabled.save(&state).expect("disable");
        assert!(!Settings::load(&state).expect("load").accounts[0].enabled);

        // The sign-in never finishes: no granted access, guard dropped.
        drop(super::DesiredState {
            state: state.clone(),
            id,
            enabled: true,
            access: None,
        });

        let after = Settings::load(&state).expect("load");
        assert!(
            after.accounts[0].enabled,
            "an abandoned sign-in left the account disabled and the drive unmounted"
        );
        assert_eq!(
            after.accounts[0].access,
            AccessMode::ReadOnly,
            "and it must not have adopted a permission nobody granted"
        );
    }

    fn fixture_account(access: AccessMode) -> Account {
        Account {
            id: "00000000-0000-4000-8000-000000000007".into(),
            label: "fixture".into(),
            registration: cirrove_auth::AppRegistration {
                client_id: "00000000-0000-4000-8000-000000000001".into(),
                authority: "common".into(),
            },
            identity: cirrove_auth::Identity {
                tenant_id: "00000000-0000-4000-8000-000000000002".into(),
                subject: "fixture".into(),
                username: "fixture@example.invalid".into(),
                graph_user_id: "fixture".into(),
                display_name: "fixture".into(),
            },
            credential_id: "00000000-0000-4000-8000-000000000008".into(),
            access,
            drive: cirrove_onedrive::DriveInfo {
                id: "drive".into(),
                name: "fixture".into(),
                drive_type: "business".into(),
                web_url: "https://example.invalid".into(),
            },
            root_id: "root".into(),
            mount_path: "/nonexistent/reauth-fixture".into(),
            enabled: false,
            poll_seconds: 3600,
            cache_bytes: 8 * 1024 * 1024,
        }
    }
}
