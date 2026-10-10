//! Atomic non-secret account configuration. Tokens are exclusively in Secret Service.
use crate::private_dir;
use anyhow::{Context, Result, bail};
use cirrove_auth::{
    AccessMode, AppRegistration, CredentialVault, DesktopVault, Identity, PendingLogin,
    TokenBroker, save_credentials,
};
use cirrove_core::{CancellationToken, CollectionInfo as DriveInfo, ReadProvider, Scope};
use cirrove_googledrive::GoogleDrive;
use cirrove_icloud::{
    ICloudDrive, ICloudReadSession, ROOT_ID, SealedSessionVault, probe_session_key,
};
use cirrove_onedrive::{OneDrive, StaticToken};
use secrecy::SecretString;
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
            version: 2,
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
                #[derive(Deserialize)]
                struct Header {
                    version: u32,
                }
                // Preserve the typed JSON error (and source location) for a
                // malformed version; the desktop distinguishes it from a real
                // future-format version instead of displaying private input.
                let header: Header =
                    serde_json::from_slice(&bytes).context("invalid Cirrove account settings")?;
                if header.version == 2 {
                    let settings: Self = serde_json::from_slice(&bytes)
                        .context("invalid Cirrove account settings")?;
                    settings.validate()?;
                    return Ok(settings);
                }
                let mut value: serde_json::Value =
                    serde_json::from_slice(&bytes).context("invalid Cirrove account settings")?;
                match header.version {
                    1 => {
                        // Read-old is purely in memory. Merely showing accounts must
                        // neither rewrite configuration nor surprise a running daemon.
                        if let Some(accounts) =
                            value.get_mut("accounts").and_then(|v| v.as_array_mut())
                        {
                            for account in accounts {
                                if let Some(registration) = account
                                    .get_mut("registration")
                                    .and_then(|v| v.as_object_mut())
                                {
                                    if registration
                                        .get("provider")
                                        .is_some_and(|p| p != "microsoft")
                                    {
                                        bail!("version 1 settings cannot contain another provider");
                                    }
                                    registration.insert("provider".into(), "microsoft".into());
                                }
                            }
                        }
                        value["version"] = 2.into();
                    }
                    _ => bail!("unsupported account settings version"),
                }
                let settings: Self =
                    serde_json::from_value(value).context("invalid Cirrove account settings")?;
                settings.validate()?;
                Ok(settings)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }
    pub fn validate(&self) -> Result<()> {
        if self.version != 2 {
            bail!("unsupported account settings version");
        }
        let mut ids = std::collections::HashSet::new();
        let mut labels = std::collections::HashSet::new();
        let mut paths = std::collections::HashSet::new();
        for account in &self.accounts {
            account.registration.validate()?;
            if matches!(account.registration, AppRegistration::Google { .. })
                && (account.drive.id != account.root_id
                    || !matches!(
                        account.drive.drive_type.as_str(),
                        "my_drive" | "shared_drive"
                    )
                    || account.identity.subject.is_empty()
                    || !account.identity.tenant_id.is_empty())
            {
                bail!("invalid selected Google Drive");
            }
            if matches!(account.registration, AppRegistration::ICloud)
                && (account.drive.id != "drive"
                    || account.drive.drive_type != "icloud_drive"
                    || account.root_id != "FOLDER::com.apple.CloudDocs::root"
                    || account.identity.username.is_empty()
                    || !account.identity.tenant_id.is_empty()
                    || !account.identity.graph_user_id.is_empty())
            {
                bail!("invalid iCloud Drive");
            }
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
/// Exclusive settings critical section, independent of unrelated inherited FDs.
/// The guard cannot clone its descriptor or transfer unlock ownership to a child.
pub struct ConfigLock {
    file: File,
    pid: u32,
}
impl Drop for ConfigLock {
    fn drop(&mut self) {
        // flock belongs to the open file description: close alone can leave an
        // unrelated fork-before-exec child holding it. Only the acquiring
        // process may end this critical section; a forked guard must not unlock
        // the parent's still-live owner. An unlock error stays conservative.
        if self.pid == std::process::id() {
            let _ = fs2::FileExt::unlock(&self.file);
        }
    }
}
#[cfg(test)]
impl std::os::fd::AsFd for ConfigLock {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        std::os::fd::AsFd::as_fd(&self.file)
    }
}
#[cfg(test)]
impl std::os::fd::AsRawFd for ConfigLock {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        std::os::fd::AsRawFd::as_raw_fd(&self.file)
    }
}
pub fn config_lock(state: &Path) -> Result<ConfigLock> {
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
    Ok(ConfigLock {
        file,
        pid: std::process::id(),
    })
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
/// Why the browser did not start, in words the reader can act on.
///
/// "could not start the browser: No such file or directory (os error 2)" is
/// what a person saw on a fresh Arch install when they pressed Sign in with
/// Microsoft, and it names neither the program nor the package. `xdg-open` is
/// the only way this opens a browser; on Fedora and Debian a desktop pulls it
/// in and it was never missing, which is why the message had never been read by
/// anyone who needed it.
fn browser_failure(error: std::io::Error) -> anyhow::Error {
    if error.kind() == std::io::ErrorKind::NotFound {
        return anyhow::anyhow!(
            "could not open a browser: xdg-open is not installed. It is in the xdg-utils \
             package on every distribution Cirrove ships for, and signing in needs it."
        );
    }
    anyhow::Error::new(error).context("could not start the browser")
}

async fn browser_login_with_secret(
    app: AppRegistration,
    access: AccessMode,
    secret: Option<secrecy::SecretString>,
) -> Result<(Identity, cirrove_auth::Credentials)> {
    let google = matches!(app, AppRegistration::Google { .. });
    let pending = PendingLogin::with_access(app, access)
        .await?
        .with_client_secret(secret);
    if access == AccessMode::ReadWrite {
        println!(
            "Requesting write consent. An account with this grant is mounted writable, \
             so applications can change cloud files through it."
        );
    }
    if google {
        println!(
            "Google grants {} access to Drive files across your account. Cirrove mounts only the drive you choose. \
             Your name, email, file names, metadata and opened content are stored \
             locally; sign-in credentials stay in your desktop keyring. With write access, edits to ordinary files are sent directly to Google, \
             not to a Cirrove server. Removing a connection retains recoverable local data until you discard it.",
            if access == AccessMode::ReadWrite {
                "read and write"
            } else {
                "read-only"
            }
        );
    }
    println!(
        "Opening {} sign-in in your browser. Select the account you want to connect.",
        if google { "Google" } else { "Microsoft" }
    );
    let mut child = tokio::process::Command::new("xdg-open")
        .arg(pending.authorization_url().as_str())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(browser_failure)?;
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
            // xdg-open ran and gave up. On a desktop with no browser installed
            // at all -- which a minimal Arch is -- that is the whole story, and
            // "check xdg-open" sent the reader to the one thing that was fine.
            if !status.context("browser launcher failed")?.success(){bail!("xdg-open could not open a browser. Check that a web browser is installed and is the default for http and https.");}
            login.await
        }
    }
}
/// Temporarily disables just this account and waits for its owned workers to
/// release credentials before replacing a grant. Other accounts keep running.
/// Where a sign-in records the desired state it is about to overwrite.
///
/// The disable has to be durable, so the intent to undo it has to be durable too,
/// or the two disagree exactly when it matters. Without this, an interrupted run
/// left the account disabled and a *later successful* run inherited that: it read
/// the already-disabled file as the account's own wish and faithfully preserved
/// it, so the drive stayed unmounted through a sign-in that reported nothing
/// wrong. That is the failure this file prevents.
pub(crate) fn restore_marker(state: &Path, id: &str) -> PathBuf {
    state.join(format!("restore-{id}.json"))
}

/// The desired state a sign-in interrupted, if one did.
///
/// Present means some run disabled this account and never put it back, so the
/// `enabled` recorded in settings is that run's leftover rather than anything the
/// user asked for.
pub(crate) fn interrupted_desired_state(state: &Path, id: &str) -> Option<bool> {
    let bytes = std::fs::read(restore_marker(state, id)).ok()?;
    serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()?
        .get("enabled")?
        .as_bool()
}

fn write_restore_marker(state: &Path, id: &str, enabled: bool) -> Result<()> {
    let path = restore_marker(state, id);
    let temp = state.join(format!(".restore-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(&serde_json::to_vec(
            &serde_json::json!({ "enabled": enabled }),
        )?)?;
        file.sync_all()?;
        std::fs::rename(&temp, &path)?;
        File::open(state)?.sync_all()?;
        Ok(())
    })();
    if temp.exists() {
        let _ = std::fs::remove_file(temp);
    }
    result
}

/// Undo a sign-in that never put an account back, when none is running.
///
/// The daemon does this because the user cannot see what to undo: an interrupted
/// sign-in leaves the account disabled, a disabled account is not mounted, and a
/// missing mount looks like nothing at all. Gated on the per-account operation
/// lock, so it can never fight a sign-in still waiting on a browser -- that lock
/// is held for exactly as long as the disable is meant to last.
pub fn heal_interrupted_sign_ins(state: &Path) -> Result<bool> {
    heal_interrupted_accounts(state, Settings::load(state)?.accounts)
}

fn heal_interrupted_accounts(state: &Path, accounts: Vec<Account>) -> Result<bool> {
    let mut healed = false;
    for account in accounts {
        // This is only a cheap hint. A new marker can wait for the next pass;
        // any present marker is reread after ownership below.
        if !restore_marker(state, &account.id).exists() {
            continue;
        }
        let Ok(_operation) = account_operation(state, &account.id) else {
            continue; // a sign-in is in progress; its own guard owns the restore
        };
        let _lock = config_lock(state)?;
        // The outer inventory is only an identity list. A preference or sign-in
        // may have finished since it was read; neither its marker nor enabled
        // value is authoritative until both locks are held.
        let Some(enabled) = interrupted_desired_state(state, &account.id) else {
            continue;
        };
        let mut settings = Settings::load(state)?;
        let Some(stored) = settings.accounts.iter_mut().find(|a| a.id == account.id) else {
            continue;
        };
        if stored.enabled != enabled {
            stored.enabled = enabled;
            settings.save(state)?;
            healed = true;
            tracing::warn!(
                account = %account.id,
                "restored an account that an interrupted sign-in left disabled"
            );
        }
        let _ = std::fs::remove_file(restore_marker(state, &account.id));
    }
    Ok(healed)
}

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
    // The original mode used by rollback if a failed durable commit renamed
    // settings before its final directory sync failed.
    access: Option<AccessMode>,
    completed: bool,
}
impl DesiredState {
    /// Report durable completion explicitly. Drop is rollback, never success.
    fn complete(self, requested: Option<AccessMode>) -> Result<()> {
        self.complete_with(requested, Settings::save)
    }
    fn complete_with(
        mut self,
        requested: Option<AccessMode>,
        persist: impl FnOnce(&Settings, &Path) -> Result<()>,
    ) -> Result<()> {
        self.restore_with(requested, persist)?;
        self.completed = true;
        Ok(())
    }
    fn restore_with(
        &self,
        access: Option<AccessMode>,
        persist: impl FnOnce(&Settings, &Path) -> Result<()>,
    ) -> Result<()> {
        let _lock = config_lock(&self.state)?;
        let mut settings = Settings::load(&self.state)?;
        let account = settings
            .accounts
            .iter_mut()
            .find(|a| a.id == self.id)
            .context("account removed during sign-in")?;
        settle(account, self.enabled, access);
        persist(&settings, &self.state)?;
        // Only after settings are durable; otherwise restart still needs this wish.
        let _ = std::fs::remove_file(restore_marker(&self.state, &self.id));
        Ok(())
    }
}
impl Drop for DesiredState {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        if let Err(error) = self.restore_with(self.access, Settings::save) {
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
    // A marker here means an earlier run disabled this account and never put it
    // back. Its `enabled` is the account's own wish; what settings currently say
    // is that run's leftover, and adopting it would carry the damage forward.
    let was_enabled = interrupted_desired_state(&state, &original.id).unwrap_or(original.enabled);
    if was_enabled != original.enabled {
        println!(
            "{label} was left disabled by an interrupted sign-in; restoring it after this one."
        );
    }
    write_restore_marker(&state, &original.id, was_enabled)?;
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
        enabled: was_enabled,
        access: Some(original.access),
        completed: false,
    };
    if was_enabled {
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
        // The same question as `begin_connect`, for the same reason: a
        // re-sign-in is a person's minutes too, and a keyring that cannot take
        // the grant afterwards has wasted all of them.
        DesktopVault::reachable().await?;
        let secret = if matches!(original.registration, AppRegistration::Google { .. }) {
            cirrove_auth::saved_client_secret(&DesktopVault, &original.credential_id).await?
        } else {
            None
        };
        let (identity, credentials) =
            browser_login_with_secret(original.registration.clone(), requested, secret).await?;
        if identity.tenant_id != original.identity.tenant_id
            || identity.graph_user_id != original.identity.graph_user_id
        {
            bail!("different account selected; use connect with a new label to add it");
        }
        let tokens = Arc::new(StaticToken(credentials.access_token()));
        match original.registration {
            AppRegistration::Microsoft { .. } => {
                OneDrive::new(original.id.clone(), tokens)?
                    .root(&original.drive.id, &CancellationToken::new())
                    .await?;
            }
            AppRegistration::Google { .. } => {
                let root =
                    GoogleDrive::new(original.id.clone(), original.drive.id.clone(), tokens)?
                        .for_collection(&original.drive)?
                        .root(&CancellationToken::new())
                        .await?;
                if root.id != original.root_id {
                    bail!("Google root identity changed");
                }
            }
            AppRegistration::ICloud => bail!("iCloud uses native sign-in"),
        }
        save_credentials(&DesktopVault, &original.credential_id, &credentials).await?;
        Ok(())
    }
    .await;
    if result.is_ok() {
        desired.complete(Some(requested))?;
        println!("{label} is signed in again; permission is now {requested:?}.");
    } else {
        drop(desired);
    }
    result
}

/// Native reauthentication intent captured before password/2FA entry.
/// The exact original settings are revalidated under the account operation lock
/// after same-account session validation. Dropping this pending intent changes
/// neither settings nor the mounted account.
pub struct PendingICloudReauthentication {
    state: PathBuf,
    original: Account,
    requested: Option<AccessMode>,
}

pub fn begin_reauthenticate_icloud(
    state: PathBuf,
    label: String,
    requested: Option<AccessMode>,
) -> Result<PendingICloudReauthentication> {
    let original = Settings::load(&state)?
        .accounts
        .into_iter()
        .find(|account| account.label == label)
        .context("unknown account label")?;
    if !matches!(original.registration, AppRegistration::ICloud) {
        bail!("this operation requires an iCloud connection");
    }
    Ok(PendingICloudReauthentication {
        state,
        original,
        requested,
    })
}

impl PendingICloudReauthentication {
    pub fn account_id(&self) -> &str {
        &self.original.id
    }

    pub fn apple_id(&self) -> &str {
        &self.original.identity.username
    }

    /// Apple grants a native session; this is Cirrove's local access policy,
    /// not a narrower Apple OAuth permission.
    pub fn access(&self) -> AccessMode {
        self.requested.unwrap_or(self.original.access)
    }

    pub async fn finish(self, mut session: ICloudReadSession) -> Result<()> {
        DesktopVault::reachable().await?;
        session
            .list_root()
            .await
            .context("iCloud Drive root is unavailable")?;
        let snapshot = session.session_snapshot()?;
        self.validate_snapshot(&snapshot)?;
        let Self {
            state,
            original,
            requested,
        } = self;

        let (_operation, desired) = suspend_validated_icloud_account(&state, &original)?;
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
            SealedSessionVault::new(&state, &original.id)?
                .save(&original.credential_id, snapshot)
                .await
        }
        .await;
        complete_icloud_session_save(desired, requested, result)
    }

    fn validate_snapshot(&self, snapshot: &SecretString) -> Result<()> {
        ICloudReadSession::from_session_snapshot(snapshot, &self.original.identity.username)
            .context("a different Apple account signed in")?;
        Ok(())
    }
}

fn complete_icloud_session_save(
    desired: DesiredState,
    requested: Option<AccessMode>,
    saved: Result<()>,
) -> Result<()> {
    match saved {
        Ok(()) => desired.complete(requested),
        Err(error) => {
            drop(desired);
            Err(error)
        }
    }
}

/// Compatibility wrapper for callers that already hold an authenticated session.
/// Ordinary reauthentication preserves mode. New interactive callers should
/// capture `begin_reauthenticate_icloud` before collecting password and 2FA.
pub async fn reauthenticate_icloud_with_session(
    state: PathBuf,
    label: String,
    session: ICloudReadSession,
) -> Result<()> {
    begin_reauthenticate_icloud(state, label, None)?
        .finish(session)
        .await
}
/// The Apple session was checked against `original` before this local operation.
/// Refuse a changed account before saving credentials or changing desired state.
fn suspend_validated_icloud_account(
    state: &Path,
    original: &Account,
) -> Result<(File, DesiredState)> {
    let operation = account_operation(state, &original.id)?;
    let lock = config_lock(state)?;
    let mut settings = Settings::load(state)?;
    let account = settings
        .accounts
        .iter_mut()
        .find(|account| account.id == original.id)
        .context("account removed during iCloud sign-in")?;
    if !matches!(account.registration, AppRegistration::ICloud)
        || serde_json::to_value(&*account)? != serde_json::to_value(original)?
    {
        bail!("account changed during iCloud sign-in; try again");
    }
    let enabled = interrupted_desired_state(state, &original.id).unwrap_or(account.enabled);
    write_restore_marker(state, &original.id, enabled)?;
    let desired = DesiredState {
        state: state.into(),
        id: original.id.clone(),
        enabled,
        access: Some(original.access),
        completed: false,
    };
    account.enabled = false;
    let disabled = settings.save(state);
    // Drop rollback must never try to acquire the lock still held here.
    drop(lock);
    disabled?;
    Ok((operation, desired))
}

pub fn provider(account: &Account) -> Result<Arc<dyn ReadProvider>> {
    provider_with_state(account, &crate::state_dir()?)
}

pub fn provider_with_state(account: &Account, state: &Path) -> Result<Arc<dyn ReadProvider>> {
    match account.registration {
        AppRegistration::Microsoft { .. } => Ok(onedrive_provider(account)?),
        AppRegistration::Google { .. } => Ok(google_provider(account)?),
        AppRegistration::ICloud => {
            let scope = Scope {
                account: account.id.clone(),
                provider: "icloud".into(),
                collection: account.drive.id.clone(),
            };
            Ok(Arc::new(
                ICloudDrive::on_demand_from_sealed_session(
                    scope,
                    account.identity.username.clone(),
                    account.credential_id.clone(),
                    state,
                )?
                .with_package_artifacts(state, account.cache_bytes)?,
            ))
        }
    }
}
pub(crate) fn provider_with_owned_state(
    account: &Account,
    state: &Path,
    owner: Arc<File>,
) -> Result<Arc<dyn ReadProvider>> {
    if !matches!(account.registration, AppRegistration::ICloud) {
        bail!("owned session writeback requires an iCloud account");
    }
    let scope = Scope {
        account: account.id.clone(),
        provider: "icloud".into(),
        collection: account.drive.id.clone(),
    };
    Ok(Arc::new(
        ICloudDrive::on_demand_from_owned_sealed_session(
            scope,
            account.identity.username.clone(),
            account.credential_id.clone(),
            state,
            owner,
        )?
        .with_package_artifacts(state, account.cache_bytes)?,
    ))
}

pub fn google_provider(account: &Account) -> Result<Arc<GoogleDrive>> {
    if !matches!(account.registration, AppRegistration::Google { .. }) {
        bail!("this operation requires a Google Drive connection");
    }
    let broker = TokenBroker::new(
        account.registration.clone(),
        account.identity.clone(),
        account.credential_id.clone(),
        Arc::new(DesktopVault),
    )?;
    Ok(Arc::new(
        GoogleDrive::new(
            account.id.clone(),
            account.drive.id.clone(),
            Arc::new(broker),
        )?
        .for_collection(&account.drive)?,
    ))
}
pub fn write_provider(account: &Account) -> Result<Arc<dyn crate::writable::WriteProvider>> {
    match account.registration {
        AppRegistration::Microsoft { .. } => Ok(onedrive_provider(account)?),
        AppRegistration::Google { .. } => Ok(google_provider(account)?),
        AppRegistration::ICloud => bail!("iCloud writes require the account journal context"),
    }
}
/// The iCloud router needs the journal and metadata owned by this mount.
/// Ordinary iCloud writes use this context-bound factory; the context-free
/// factory must never construct a writer without its journal and metadata.
pub fn write_provider_with_context(
    account: &Account,
    context: &crate::manager::WriteContext,
) -> Result<Arc<dyn crate::writable::WriteProvider>> {
    if matches!(account.registration, AppRegistration::ICloud) {
        return Ok(Arc::new(crate::icloud_writes::ICloudWriteProvider::new(
            account, context,
        )?));
    }
    write_provider(account)
}
/// Microsoft-only validation and write workers must refuse other accounts before
/// loading credentials or making a request.
pub fn onedrive_provider(account: &Account) -> Result<Arc<OneDrive>> {
    if !matches!(account.registration, AppRegistration::Microsoft { .. }) {
        bail!("this operation requires a OneDrive connection");
    }
    let broker = TokenBroker::new(
        account.registration.clone(),
        account.identity.clone(),
        account.credential_id.clone(),
        Arc::new(DesktopVault),
    )?;
    let graph = OneDrive::new(account.id.clone(), Arc::new(broker))?;
    // The read-session path is the default since 2026-09-10. This is the escape
    // hatch, not an opt-in: a tenant that misbehaves under sessions can be put
    // back on per-block revalidation without a rebuild, and the variable is read
    // here rather than baked in so that the fallback survives a restart.
    let graph = match std::env::var_os("CIRROVE_CONSERVATIVE_READS") {
        Some(value) if value == "1" => graph.without_read_sessions(),
        _ => graph,
    };
    Ok(Arc::new(graph))
}
/// The adapter with the read-session path selected regardless of environment.
///
/// The builder consumes the adapter, so a measurement cannot flip an existing
/// `Arc<OneDrive>`; it has to be constructed this way from the start. This and
/// its conservative twin exist so an A/B arm names itself instead of inheriting
/// whatever the default happens to be, which is what makes the comparison
/// survive a change to that default.
pub fn read_session_provider(account: &Account) -> Result<Arc<OneDrive>> {
    Ok(Arc::new(base_provider(account)?.with_read_sessions()))
}
/// The control arm: per-cache-block revalidation, environment ignored.
pub fn conservative_read_provider(account: &Account) -> Result<Arc<OneDrive>> {
    Ok(Arc::new(base_provider(account)?.without_read_sessions()))
}
fn base_provider(account: &Account) -> Result<OneDrive> {
    if !matches!(account.registration, AppRegistration::Microsoft { .. }) {
        bail!("this operation requires a OneDrive connection");
    }
    let broker = TokenBroker::new(
        account.registration.clone(),
        account.identity.clone(),
        account.credential_id.clone(),
        Arc::new(DesktopVault),
    )?;
    Ok(OneDrive::new(account.id.clone(), Arc::new(broker))?)
}
/// Refuse to migrate a store that a running daemon is using.
///
/// Opening a store migrates it, and a daemon built before that schema then
/// cannot read its own index: its feeds go offline with "local metadata
/// operation failed" while the mount stays up, which looks like a network fault
/// rather than what it is. Any tool sharing a state directory with a running
/// service has to check before it opens, not after.
///
/// This exists because it happened. A pin command run against a live state
/// directory migrated the account it was only meant to report an error about.
fn refuse_to_migrate_under_a_running_daemon(state: &Path, db: &Path) -> Result<()> {
    if daemon_lock(state).is_ok() {
        return Ok(()); // nothing is running; migrating on open is the normal path
    }
    let version = match cirrove_store::schema_version(db) {
        Ok(version) => version,
        // Absent really is nothing to migrate. Present and unreadable is a
        // different thing and must not borrow that answer: a daemon is running,
        // and a guard that opens the gate whenever it cannot see is worse than
        // no guard, because the caller believes it was checked. SQLite can fail
        // this read for reasons that pass -- a busy database, a hot journal, no
        // descriptors left -- and every one of them used to disable the refusal
        // silently.
        Err(_) if !db.exists() => return Ok(()),
        Err(error) => bail!(
            "a daemon is running and this account's index at {} could not be read \
             to check its schema ({error}). Refusing rather than guessing: opening \
             it here may migrate it and stop that daemon reading its own metadata. \
             Stop cirroved and try again.",
            db.display()
        ),
    };
    if version < cirrove_store::SCHEMA_VERSION {
        bail!(
            "this account's index is at schema {version} and this build writes {}. \
             A daemon is running and was built for {version}; opening the index here would \
             migrate it and stop that daemon reading its own metadata. \
             Install the matching build and restart cirroved first.",
            cirrove_store::SCHEMA_VERSION
        );
    }
    Ok(())
}
/// Record or release a pin from outside the daemon.
///
/// Writes the registry directly rather than commanding the running service. The
/// daemon re-reads reservations on its status cycle, so a pin made here takes
/// effect within seconds without a control verb, and one made while no daemon
/// runs is already in place when the next mount reads pins at construction.
///
/// The account operation lock is held for the write, so this cannot interleave
/// with a sign-in that is changing the same account.
pub fn set_pin(
    state: &Path,
    label: &str,
    item: &str,
    recursive: bool,
    bytes: Option<u64>,
) -> Result<String> {
    let account = Settings::load(state)?
        .accounts
        .into_iter()
        .find(|a| a.label == label)
        .context("unknown account")?;
    let _operation = account_operation(state, &account.id)?;
    let directory = state.join("accounts").join(&account.id);
    let scope = cirrove_core::Scope {
        account: account.id.clone(),
        provider: account.registration.provider_id().into(),
        collection: account.drive.id.clone(),
    };
    let key = serde_json::to_string(&scope)?;
    let db = directory.join("metadata.db");
    refuse_to_migrate_under_a_running_daemon(state, &db)?;
    let mut store = cirrove_store::Store::open(db)?;
    let reserved = match bytes {
        Some(bytes) => bytes,
        // Taken from the local index rather than the provider: pinning should not
        // need the network, and an item nobody has indexed is one whose size this
        // command cannot honestly guess.
        None => {
            store
                .node(&scope, item)?
                .context("item is not in the local index; pass --bytes to pin it anyway")?
                .size
        }
    };
    let headroom = (account.cache_bytes / 10)
        .max(8 * 4 * 1024 * 1024)
        .min(account.cache_bytes);
    let budget = account.cache_bytes.saturating_sub(headroom);
    match store.pin(&key, item, recursive, reserved, budget)? {
        Ok(pin) => Ok(format!(
            "pinned {} reserving {} bytes{}",
            pin.item,
            pin.reserved,
            if pin.recursive { ", recursively" } else { "" }
        )),
        Err(refusal) => bail!("{refusal}"),
    }
}
pub fn clear_pin(state: &Path, label: &str, item: &str) -> Result<String> {
    let account = Settings::load(state)?
        .accounts
        .into_iter()
        .find(|a| a.label == label)
        .context("unknown account")?;
    let _operation = account_operation(state, &account.id)?;
    let scope = cirrove_core::Scope {
        account: account.id.clone(),
        provider: account.registration.provider_id().into(),
        collection: account.drive.id.clone(),
    };
    let key = serde_json::to_string(&scope)?;
    let db = state.join("accounts").join(&account.id).join("metadata.db");
    refuse_to_migrate_under_a_running_daemon(state, &db)?;
    let mut store = cirrove_store::Store::open(db)?;
    Ok(if store.unpin(&key, item)? {
        format!("released {item}")
    } else {
        format!("{item} was not pinned")
    })
}
/// Complete browser sign-in, display verified identity and let the caller choose a
/// drive. Persistence happens only after the selected drive's root is verified.
/// A sign-in that has happened and a drive that has not yet been chosen.
///
/// `connect` used to do both in one call and ask for the drive on stdin, which
/// a window cannot answer. Split, the window shows the drives and finishes with
/// the one the user picks; the CLI does the same with a prompt. Nothing is saved
/// until `finish`: dropping this forgets the grant, which is the right outcome
/// for a sign-in the user walked away from.
pub struct PendingConnection {
    state: PathBuf,
    label: String,
    app: AppRegistration,
    mount_path: PathBuf,
    access: AccessMode,
    id: String,
    identity: Identity,
    credentials: cirrove_auth::Credentials,
    provider: ConnectionProvider,
    drives: Vec<DriveInfo>,
}
enum ConnectionProvider {
    Microsoft(OneDrive),
    Google(GoogleDrive),
}
impl PendingConnection {
    /// Who signed in.
    pub fn identity(&self) -> &Identity {
        &self.identity
    }
    /// The drives this account can mount, the default Documents drive first.
    pub fn drives(&self) -> &[DriveInfo] {
        &self.drives
    }
    /// Save the account with this drive. A drive not in the list is looked up,
    /// so a caller who knows an id can name one the listing missed.
    pub async fn finish(self, drive_id: &str) -> Result<Account> {
        let cancel = CancellationToken::new();
        let (drive, root) = match &self.provider {
            ConnectionProvider::Microsoft(graph) => {
                let drive = match self.drives.iter().find(|d| d.id == drive_id) {
                    Some(drive) => drive.clone(),
                    None => graph.drive(drive_id, &cancel).await?,
                };
                let root = graph.root(&drive.id, &cancel).await?;
                (drive, root)
            }
            ConnectionProvider::Google(google) => {
                let drive = self
                    .drives
                    .iter()
                    .find(|d| d.id == drive_id)
                    .cloned()
                    .context("Google drive was not offered at sign-in")?;
                let root = google.clone().for_collection(&drive)?.root(&cancel).await?;
                if root.id != drive.id {
                    bail!("Google root identity changed");
                }
                (drive, root)
            }
        };
        if self.mount_path.exists() && std::fs::read_dir(&self.mount_path)?.next().is_some() {
            bail!("mount directory must be empty");
        }
        let account = Account {
            id: self.id,
            label: self.label,
            registration: self.app,
            identity: self.identity,
            credential_id: uuid::Uuid::new_v4().to_string(),
            access: self.access,
            drive,
            root_id: root.id,
            mount_path: self.mount_path,
            enabled: true,
            poll_seconds: 30,
            cache_bytes: 5 * 1024 * 1024 * 1024,
        };
        // Store the grant before acquiring the settings lock. Keyring awaits
        // and cleanup never span a shared filesystem configuration lock.
        save_credentials(&DesktopVault, &account.credential_id, &self.credentials).await?;
        let saved = (|| -> Result<()> {
            let _lock = config_lock(&self.state)?;
            let mut settings = Settings::load(&self.state)?;
            if settings
                .accounts
                .iter()
                .any(|a| a.label == account.label || a.mount_path == account.mount_path)
            {
                bail!("account settings changed during sign-in; try another label or path");
            }
            settings.accounts.push(account.clone());
            settings.save(&self.state)
        })();
        if let Err(error) = saved {
            let _ = DesktopVault.remove(&account.credential_id).await;
            return Err(error);
        }
        Ok(account)
    }
}
/// Persist an already authenticated native Apple session as a read-only drive.
/// The caller collects the password and trusted-device code locally; neither
/// is passed here. Apple requests and keyring awaits finish before the shared
/// settings lock is acquired.
pub async fn connect_icloud_with_session(
    state: PathBuf,
    label: String,
    mount_path: PathBuf,
    apple_id: String,
    session: ICloudReadSession,
) -> Result<Account> {
    connect_icloud_with_session_and_access(
        state,
        label,
        mount_path,
        apple_id,
        session,
        AccessMode::ReadOnly,
    )
    .await
}

/// Explicit native connection policy. Defaults remain in the compatibility
/// wrapper; password/2FA callers carry their selected mode to this final step.
/// Writes require explicit selection; the compatibility wrapper stays read-only.
pub async fn connect_icloud_with_session_and_access(
    state: PathBuf,
    label: String,
    mount_path: PathBuf,
    apple_id: String,
    mut session: ICloudReadSession,
    access: AccessMode,
) -> Result<Account> {
    if !valid_label(&label) {
        bail!("use a label of 1–48 letters, digits, hyphens or underscores");
    }
    let apple_id = apple_id.trim().to_lowercase();
    let subject = probe_session_key(&apple_id)?;
    crate::manager::validate_mount_directory(&mount_path)?;
    let mount_path = std::fs::canonicalize(mount_path)?;
    {
        let _lock = config_lock(&state)?;
        let settings = Settings::load(&state)?;
        if settings
            .accounts
            .iter()
            .any(|account| account.label == label || account.mount_path == mount_path)
        {
            bail!("this label or mount path is already configured");
        }
    }
    DesktopVault::reachable().await?;
    session
        .list_root()
        .await
        .context("iCloud Drive root is unavailable")?;
    let snapshot = session.session_snapshot()?;
    ICloudReadSession::from_session_snapshot(&snapshot, &apple_id)
        .context("signed-in Apple account does not match the selected identifier")?;
    let account = Account {
        id: uuid::Uuid::new_v4().to_string(),
        label,
        registration: AppRegistration::ICloud,
        identity: Identity {
            tenant_id: String::new(),
            subject,
            username: apple_id,
            graph_user_id: String::new(),
            display_name: "iCloud Drive".into(),
        },
        credential_id: uuid::Uuid::new_v4().to_string(),
        access,
        drive: DriveInfo {
            id: "drive".into(),
            name: "iCloud Drive".into(),
            drive_type: "icloud_drive".into(),
            web_url: "https://www.icloud.com/iclouddrive".into(),
        },
        root_id: ROOT_ID.into(),
        mount_path,
        enabled: true,
        poll_seconds: 60,
        cache_bytes: 5 * 1024 * 1024 * 1024,
    };
    let sealed = SealedSessionVault::new(&state, &account.id)?;
    save_icloud_connection_owned(state, account.clone(), snapshot, Arc::new(sealed)).await?;
    Ok(account)
}

// Persistence owns its inputs independently of the interactive waiter. Dropping
// that waiter before credential save finishes cancels publication, but does not
// interrupt credential readback or cleanup. Once the synchronous settings commit
// starts, it completes and preserves uncertain-publication retention semantics.
async fn save_icloud_connection_owned(
    state: PathBuf,
    account: Account,
    snapshot: SecretString,
    vault: Arc<dyn CredentialVault>,
) -> Result<()> {
    let (send, receive) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let result = save_icloud_connection_with(
            &state,
            &account,
            snapshot,
            vault.as_ref(),
            |settings, location| {
                if send.is_closed() {
                    bail!("iCloud connection cancelled before settings publication");
                }
                settings.save(location)
            },
        )
        .await;
        let _ = send.send(result);
    });
    receive
        .await
        .context("iCloud connection persistence task did not finish")?
}

async fn save_icloud_connection_with(
    state: &Path,
    account: &Account,
    snapshot: SecretString,
    vault: &dyn CredentialVault,
    persist: impl FnOnce(&Settings, &Path) -> Result<()>,
) -> Result<()> {
    // Validate the candidate identity before any
    // credential write. The final locked check still detects concurrent edits.
    Settings {
        version: 2,
        accounts: vec![account.clone()],
    }
    .validate()?;
    let credential_saved = vault.save(&account.credential_id, snapshot).await;
    let mut safe_to_remove = false;
    let saved = (|| -> Result<()> {
        let _lock = config_lock(state)?;
        let mut settings = Settings::load(state)?;
        let references_session = |existing: &Account| {
            existing.id == account.id || existing.credential_id == account.credential_id
        };
        safe_to_remove = !settings.accounts.iter().any(references_session);
        credential_saved?;
        if settings.accounts.iter().any(|existing| {
            existing.label == account.label || existing.mount_path == account.mount_path
        }) {
            bail!("account settings changed during sign-in; try another label or path");
        }
        settings.accounts.push(account.clone());
        let result = persist(&settings, state);
        if result.is_err() {
            // An error after atomic rename can leave the enabled account
            // published. Keep its session; an uncertain readback is not proof
            // that removing credentials is safe. Check under the same lock.
            safe_to_remove = Settings::load(state)
                .map(|published| !published.accounts.iter().any(references_session))
                .unwrap_or(false);
        }
        result
    })();
    if let Err(error) = saved {
        if safe_to_remove {
            let _ = vault.remove(&account.credential_id).await;
        }
        return Err(error);
    }
    Ok(())
}
/// Check the label and mount path, sign in through the browser, and list the
/// drives. The account is not saved until `PendingConnection::finish`.
pub async fn begin_connect(
    state: PathBuf,
    label: String,
    app: AppRegistration,
    mount_path: PathBuf,
    access: AccessMode,
) -> Result<PendingConnection> {
    begin_connect_with_secret(state, label, app, mount_path, access, None).await
}
pub async fn begin_connect_google(
    state: PathBuf,
    label: String,
    client_file: PathBuf,
    mount_path: PathBuf,
) -> Result<PendingConnection> {
    begin_connect_google_with_access(state, label, client_file, mount_path, AccessMode::ReadOnly)
        .await
}
pub async fn begin_connect_google_with_access(
    state: PathBuf,
    label: String,
    client_file: PathBuf,
    mount_path: PathBuf,
    access: AccessMode,
) -> Result<PendingConnection> {
    let client = cirrove_auth::google::DesktopClient::load(&client_file)?;
    begin_connect_with_secret(
        state,
        label,
        client.registration,
        mount_path,
        access,
        client.secret,
    )
    .await
}
/// Reuse only the OAuth application registration from another local Google
/// connection. The new account still completes its own browser consent and
/// receives a separate credential entry.
pub async fn begin_connect_google_with_existing_app(
    state: PathBuf,
    label: String,
    existing_account_id: String,
    mount_path: PathBuf,
    access: AccessMode,
) -> Result<PendingConnection> {
    let existing = Settings::load(&state)?
        .accounts
        .into_iter()
        .find(|account| {
            account.id == existing_account_id
                && matches!(account.registration, AppRegistration::Google { .. })
        })
        .context("existing Google connection is no longer configured")?;
    let secret = cirrove_auth::saved_client_secret(&DesktopVault, &existing.credential_id).await?;
    begin_connect_with_secret(
        state,
        label,
        existing.registration,
        mount_path,
        access,
        secret,
    )
    .await
}
async fn begin_connect_with_secret(
    state: PathBuf,
    label: String,
    app: AppRegistration,
    mount_path: PathBuf,
    access: AccessMode,
    secret: Option<secrecy::SecretString>,
) -> Result<PendingConnection> {
    app.validate()?;
    let secret = if matches!(
        &app,
        AppRegistration::Google { client_id }
            if client_id == cirrove_auth::google::CIRROVE_DESKTOP_CLIENT_ID
    ) {
        Some(
            secret
                .or_else(cirrove_auth::google::bundled_desktop_client_secret)
                .context(
                    "this build lacks Cirrove's Google Desktop OAuth client_secret; use an \
                     official build or choose a custom Desktop OAuth client JSON",
                )?,
        )
    } else {
        secret
    };
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
    // Before the browser, not after. A sign-in is minutes of a person's
    // attention and it cannot be handed back: discovering afterwards that there
    // is nowhere to keep the grant throws all of it away, which is what
    // happened on a clean Arch machine on 2026-09-15.
    DesktopVault::reachable().await?;
    let (identity, credentials) = browser_login_with_secret(app.clone(), access, secret).await?;
    let id = uuid::Uuid::new_v4().to_string();
    let tokens = Arc::new(StaticToken(credentials.access_token()));
    let (provider, drives) = match app {
        AppRegistration::Microsoft { .. } => {
            let graph = OneDrive::new(id.clone(), tokens)?;
            let drives = graph.drives(&CancellationToken::new()).await?;
            (ConnectionProvider::Microsoft(graph), drives)
        }
        AppRegistration::Google { .. } => {
            let google = GoogleDrive::new(id.clone(), "root".into(), tokens)?;
            let drives = google.collections(&CancellationToken::new()).await?;
            (ConnectionProvider::Google(google), drives)
        }
        AppRegistration::ICloud => bail!("iCloud uses native sign-in"),
    };
    Ok(PendingConnection {
        state,
        label,
        app,
        mount_path,
        access,
        id,
        identity,
        credentials,
        provider,
        drives,
    })
}
pub async fn connect(
    state: PathBuf,
    label: String,
    app: AppRegistration,
    mount_path: PathBuf,
    drive_id: Option<String>,
    access: AccessMode,
) -> Result<()> {
    let pending = begin_connect(state, label, app, mount_path, access).await?;
    let identity = pending.identity();
    println!(
        "Signed in: {} ({})\nTenant: {}",
        identity.display_name, identity.username, identity.tenant_id
    );
    let drive = match drive_id {
        Some(drive) => drive,
        None => {
            let drives = pending.drives();
            for (index, drive) in drives.iter().enumerate() {
                println!(
                    "{}. {} · {}\n   {}",
                    index + 1,
                    drive.name,
                    drive.drive_type,
                    drive.web_url
                );
            }
            // The ids, for an error that can actually be acted on. By the time
            // this question is asked the browser half has already succeeded, so
            // failing here throws away a sign-in the person has just done.
            let listing = drives
                .iter()
                .map(|drive| format!("  --drive-id {}   ({})", drive.id, drive.name))
                .collect::<Vec<_>>()
                .join("\n");
            let selected = tokio::task::spawn_blocking(move || -> Result<usize> {
                print!("Drive number to mount (Enter cancels): ");
                std::io::stdout().flush()?;
                let mut line = String::new();
                let read = std::io::stdin().read_line(&mut line)?;
                drive_choice(read, &line, &listing)
            })
            .await??;
            drives
                .get(selected.checked_sub(1).context("invalid drive number")?)
                .context("invalid drive number")?
                .id
                .clone()
        }
    };
    let account = pending.finish(&drive).await?;
    println!(
        "Connected {}. Mount location: {}",
        account.label,
        account.mount_path.display()
    );
    Ok(())
}
pub fn set_enabled(state: &Path, label: &str, enabled: bool) -> Result<()> {
    // The command line wants a sentence; the window wants the kind, and gets it
    // from set_enabled_by_id. PreferenceRefusal implements Error, so `?` here
    // turns the kind into that sentence without either side losing anything.
    Ok(update_enabled(
        state,
        |account| account.label == label,
        enabled,
    )?)
}
/// Desktop actions address the persisted account identity, never a reusable label.
/// Why a mount-preference change did not happen.
///
/// A kind rather than a message. The window has to say something different for
/// each of these -- they have different remedies -- and it must not show a raw
/// error. Until now all four collapsed into one sentence guessing that another
/// operation might be running, which was the wrong guess three times in four.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreferenceRefusal {
    /// Another window, the tray or the command line holds the settings lock,
    /// or this account already has an operation in flight.
    Busy,
    /// The account is not in the settings any more: removed somewhere else
    /// while this window was showing it.
    NotConfigured,
    /// The settings could not be read.
    Unreadable,
    /// The settings could not be written back.
    Unwritable,
}

impl std::fmt::Display for PreferenceRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Busy => "another Cirrove operation is running; try again in a moment",
            Self::NotConfigured => "that connection is no longer configured",
            Self::Unreadable => "the saved connections could not be read",
            Self::Unwritable => "the saved connections could not be written",
        })
    }
}
impl std::error::Error for PreferenceRefusal {}

pub fn set_enabled_by_id(
    state: &Path,
    id: &str,
    enabled: bool,
) -> std::result::Result<(), PreferenceRefusal> {
    update_enabled(state, |account| account.id == id, enabled)
}
fn update_enabled(
    state: &Path,
    select: impl Fn(&Account) -> bool,
    enabled: bool,
) -> std::result::Result<(), PreferenceRefusal> {
    update_enabled_with(state, select, enabled, Settings::save)
}

fn update_enabled_with(
    state: &Path,
    select: impl Fn(&Account) -> bool,
    enabled: bool,
    persist: impl FnOnce(&Settings, &Path) -> Result<()>,
) -> std::result::Result<(), PreferenceRefusal> {
    let _lock = config_lock(state).map_err(|_| PreferenceRefusal::Busy)?;
    let mut settings = Settings::load(state).map_err(|_| PreferenceRefusal::Unreadable)?;
    let account = settings
        .accounts
        .iter_mut()
        .find(|a| select(a))
        .ok_or(PreferenceRefusal::NotConfigured)?;
    // Held per account, so this is "this drive is busy" and not "Cirrove is".
    let _operation = account_operation(state, &account.id).map_err(|_| PreferenceRefusal::Busy)?;
    let marker = restore_marker(state, &account.id);
    if marker.exists() {
        // Supersede an interrupted sign-in's old wish before changing settings.
        // A later persistence error can still leave this explicit intent for
        // healing (including an error after marker rename); it is not rollback.
        write_restore_marker(state, &account.id, enabled)
            .map_err(|_| PreferenceRefusal::Unwritable)?;
    }
    account.enabled = enabled;
    persist(&settings, state).map_err(|_| PreferenceRefusal::Unwritable)?;
    // Failure to remove is harmless: any retained marker now holds the latest
    // explicit wish, and future preference changes supersede it again.
    let _ = std::fs::remove_file(marker);
    Ok(())
}
/// Remove an account, refusing while it still holds work nobody has sent.
///
/// Built because milestone 3 asks that unsent changes survive "account disable
/// and removal" and there was no removal path at all -- so a test of the clause
/// would have asserted that a thing which does not exist does not delete a
/// journal, and would have passed before and after any change for the same
/// reason.
///
/// Three deliberate narrownesses. It refuses while the account is enabled, so
/// removal never races a live mount and reuses machinery that already exists.
/// It refuses when the journal still holds unsent records, naming how many,
/// unless the caller says explicitly to discard them -- that refusal is the
/// clause. And it MOVES the account directory to `removed/` rather than
/// deleting it, which is what AGENTS.md asks of anything that would otherwise
/// be a recursive delete near a mount; a human empties that directory.
///
/// The mount directory itself is never touched. A stray local file there is
/// already asserted to survive a remount, and removal must not become the
/// exception.
pub fn forget(state: &Path, label: &str, discard_unsent: bool) -> Result<String> {
    let _lock = config_lock(state)?;
    let mut settings = Settings::load(state)?;
    let index = settings
        .accounts
        .iter()
        .position(|a| a.label == label)
        .context("no account carries that label")?;
    let account = settings.accounts[index].clone();
    let _operation = account_operation(state, &account.id)?;
    if account.enabled {
        bail!("{label} is still enabled; disable it first so removal cannot race a running mount");
    }
    let directory = state.join("accounts").join(&account.id);
    let unsent = unsent_uploads(&directory, &account.id)?;
    if unsent > 0 && !discard_unsent {
        bail!(
            "{label} still holds {unsent} change(s) that have not reached the cloud. \
Enable it and let them upload, or pass --discard-unsent to remove them with it."
        );
    }
    let removed = state.join("removed");
    private_dir(&removed)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let target = removed.join(format!("{}-{stamp}", account.id));
    if directory.exists() {
        std::fs::rename(&directory, &target)
            .with_context(|| format!("could not move {} aside", directory.display()))?;
    }
    settings.accounts.remove(index);
    settings.save(state)?;
    Ok(format!(
        "removed {label}; its local data was moved to {} rather than deleted{}",
        target.display(),
        if unsent > 0 {
            format!(", including {unsent} unsent change(s)")
        } else {
            String::new()
        }
    ))
}
/// One connection's data, set aside by `forget` and still on the disk.
#[derive(Clone, Debug)]
pub struct SetAside {
    /// The directory name under `removed/`: the account id and when it went.
    pub name: String,
    pub bytes: u64,
    pub removed_at: u64,
}

/// What Cirrove is keeping on this computer, and which of it can be reclaimed.
#[derive(Clone, Debug)]
pub struct LocalData {
    pub state_dir: PathBuf,
    /// Connections that still exist, with what each one's cache and index cost.
    pub live: Vec<(String, u64)>,
    /// Connections already removed, whose data `forget` moved aside rather than
    /// deleted. Nothing else ever looks at these, so without this they sit
    /// there for good -- which is how an uninstall leaves gigabytes behind that
    /// the person believed they had removed.
    pub set_aside: Vec<SetAside>,
    /// Everything else under the state directory: the settings file, locks,
    /// the shared index.
    pub other_bytes: u64,
}

impl LocalData {
    pub fn total_bytes(&self) -> u64 {
        self.live.iter().map(|(_, b)| b).sum::<u64>()
            + self.set_aside.iter().map(|a| a.bytes).sum::<u64>()
            + self.other_bytes
    }
    pub fn reclaimable_bytes(&self) -> u64 {
        self.set_aside.iter().map(|a| a.bytes).sum()
    }
}

/// Bytes under a directory, following nothing and failing on nothing.
///
/// A size that cannot be read is reported as zero rather than as an error: this
/// exists to tell a person roughly what their disk is being used for, and one
/// unreadable file is not a reason to refuse to answer at all.
fn directory_bytes(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => directory_bytes(&entry.path()),
            Ok(kind) if kind.is_file() => entry.metadata().map(|m| m.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

/// Report what is on the disk, so the choice to keep or remove it can be made
/// on numbers rather than on a path from the documentation.
pub fn local_data(state: &Path) -> Result<LocalData> {
    let settings = Settings::load(state).unwrap_or_default();
    let live: Vec<(String, u64)> = settings
        .accounts
        .iter()
        .map(|account| {
            (
                account.label.clone(),
                directory_bytes(&state.join("accounts").join(&account.id)),
            )
        })
        .collect();
    let mut set_aside = Vec::new();
    if let Ok(entries) = std::fs::read_dir(state.join("removed")) {
        for entry in entries.flatten() {
            if !entry.file_type().map(|k| k.is_dir()).unwrap_or(false) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            // forget names these "<account id>-<unix seconds>".
            let removed_at = name
                .rsplit('-')
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            set_aside.push(SetAside {
                bytes: directory_bytes(&entry.path()),
                name,
                removed_at,
            });
        }
    }
    set_aside.sort_by_key(|a| a.removed_at);
    let accounts_total = directory_bytes(&state.join("accounts"));
    let live_total: u64 = live.iter().map(|(_, b)| *b).sum();
    Ok(LocalData {
        state_dir: state.to_path_buf(),
        live,
        set_aside,
        // Everything under the state directory that is neither a live account
        // nor a set-aside one.
        other_bytes: directory_bytes(state)
            .saturating_sub(accounts_total)
            .saturating_sub(directory_bytes(&state.join("removed")))
            .saturating_add(accounts_total.saturating_sub(live_total)),
    })
}

/// Delete the data of connections that were already removed.
///
/// Touches `removed/` and nothing else, ever. A live account's data is not
/// reachable from here by any argument, which is the point: the dangerous
/// version of this command would be one that could be talked into deleting a
/// drive someone is still using.
pub fn discard_set_aside(state: &Path) -> Result<(usize, u64)> {
    let _lock = config_lock(state)?;
    let removed = state.join("removed");
    let mut count = 0;
    let mut bytes = 0;
    let Ok(entries) = std::fs::read_dir(&removed) else {
        return Ok((0, 0));
    };
    for entry in entries.flatten() {
        if !entry.file_type().map(|k| k.is_dir()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        // Refuse anything that is not directly inside removed/, which a
        // symlink or a crafted name could otherwise make it follow.
        if path.parent() != Some(removed.as_path()) {
            continue;
        }
        let size = directory_bytes(&path);
        std::fs::remove_dir_all(&path)
            .with_context(|| format!("could not delete {}", path.display()))?;
        count += 1;
        bytes += size;
    }
    Ok((count, bytes))
}

/// How many changes an account still holds that have not reached the cloud.
///
/// For a window deciding what to ask before removing an account: with zero the
/// question is "remove?", with more it is "remove and discard these?". Takes the
/// same locks `forget` takes and refuses an enabled account for the same reason.
pub fn unsent_changes(state: &Path, label: &str) -> Result<usize> {
    let _lock = config_lock(state)?;
    let account = Settings::load(state)?
        .accounts
        .into_iter()
        .find(|a| a.label == label)
        .context("no account carries that label")?;
    let _operation = account_operation(state, &account.id)?;
    if account.enabled {
        bail!("{label} is still enabled; disable it first");
    }
    unsent_uploads(&state.join("accounts").join(&account.id), &account.id)
}
/// How many uploads are still waiting, without disturbing them.
///
/// Opening the journal takes its lock, so this runs only under
/// `account_operation` and only for an account nothing is running.
fn unsent_uploads(directory: &Path, owner: &str) -> Result<usize> {
    let journal = directory.join("journal");
    if !journal.exists() {
        return Ok(0);
    }
    let open = crate::journal::UploadJournal::open(&journal, owner, u64::MAX)
        .context("could not read the account's pending uploads")?;
    let rows = open.list(0, 10_000)?;
    Ok(rows
        .iter()
        .filter(|r| {
            !matches!(
                r.state,
                crate::journal::UploadState::Uploaded | crate::journal::UploadState::Failed
            )
        })
        .count())
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
/// Which drive the person picked, or an error that says what to do instead.
///
/// `read` is what `read_line` returned: zero means end of input, which is what
/// a command with nothing connected to its input sees. That is not the same as
/// someone pressing Enter to cancel, and it is the case that matters -- a
/// desktop sign-in reaches `connect` with no terminal, gets all the way through
/// the browser, and would otherwise be told that an empty string is not an
/// integer. The grant is already spent by then, so the message has to name the
/// flag that avoids the question rather than describe a parse failure.
fn drive_choice(read: usize, line: &str, listing: &str) -> Result<usize> {
    let answer = line.trim();
    if read == 0 {
        bail!(
            "no drive was chosen: this command asks which drive to mount and \
             nothing is connected to its input. The sign-in itself succeeded. \
             Run it again naming the drive, which skips the question:\n{listing}"
        );
    }
    if answer.is_empty() {
        bail!("cancelled: no drive was chosen. To connect without being asked:\n{listing}");
    }
    answer
        .parse::<usize>()
        .with_context(|| format!("{answer:?} is not a drive number"))
}

/// An offline account held against mount, settings changes and journal workers.
/// No credentials, provider, migration or recovery worker is constructed.
pub struct OfflineRecovery {
    account_id: String,
    journal: crate::journal::RecoveryJournal,
    state: PathBuf,
    mounts: Vec<PathBuf>,
    _operation: File,
    _owner: File,
}
impl OfflineRecovery {
    pub fn open(state: &Path, label: &str) -> Result<Self> {
        let resolved = state.canonicalize()?;
        let state = resolved.as_path();
        let _settings = config_lock(state)?;
        let settings = Settings::load(state)?;
        let account = settings
            .accounts
            .iter()
            .find(|a| a.label == label)
            .context("no account carries that label")?;
        let operation = account_operation(state, &account.id)?;
        if account.enabled {
            bail!(
                "disable this account before offline recovery; use export-save for an active drive"
            );
        }
        let directory = state.join("accounts").join(&account.id);
        if !directory.is_dir() {
            bail!("this account has no retained local data");
        }
        let owner = account_lock(&directory)?;
        let journal =
            crate::journal::RecoveryJournal::open(&directory.join("journal"), &account.id)
                .context("could not open the retained journal for read-only recovery")?;
        Ok(Self {
            account_id: account.id.clone(),
            journal,
            state: state.canonicalize()?,
            mounts: settings
                .accounts
                .iter()
                .map(|a| a.mount_path.clone())
                .collect(),
            _operation: operation,
            _owner: owner,
        })
    }
    pub fn account_id(&self) -> &str {
        &self.account_id
    }
    pub fn list(&self, after: u64, limit: u32) -> Result<Vec<crate::recent::LocalChange>> {
        self.journal
            .list(after, limit)?
            .into_iter()
            .map(|record| {
                let name = match &record.intent {
                    crate::journal::UploadIntent::Create { name, .. } => name.clone(),
                    crate::journal::UploadIntent::Replace { item, .. } => record
                        .remote
                        .as_ref()
                        .map(|node| node.name.clone())
                        .unwrap_or_else(|| item.clone()),
                };
                let state = serde_json::to_value(record.state)?
                    .as_str()
                    .context("invalid save state")?
                    .to_owned();
                Ok(crate::recent::LocalChange {
                    operation: Some(record.id),
                    sequence: record.sequence,
                    name,
                    state,
                    size: record.size,
                    item: None,
                    saved_at: (record.saved_at > 0).then_some(record.saved_at),
                    transferred: record.transferred_bytes,
                })
            })
            .collect()
    }
    fn check_export_destination(&self, destination: &Path) -> Result<()> {
        if !destination.is_absolute()
            || destination.starts_with(&self.state)
            || self
                .mounts
                .iter()
                .any(|mount| destination.starts_with(mount))
        {
            bail!("choose a destination outside Cirrove mounts and local state");
        }
        Ok(())
    }
    pub fn working_list(
        &self,
        after: Option<uuid::Uuid>,
        limit: u32,
    ) -> Result<(Vec<crate::journal::WorkingRecovery>, Option<uuid::Uuid>)> {
        Ok(self.journal.working_list(after, limit)?)
    }
    pub fn export_working(
        &self,
        id: uuid::Uuid,
        generation: u64,
        destination: &Path,
        cancel: &CancellationToken,
        progress: impl FnMut(u64),
    ) -> Result<crate::journal::WorkingExportReceipt> {
        self.check_export_destination(destination)?;
        Ok(self
            .journal
            .export_working(id, generation, destination, cancel, progress)?)
    }
    pub fn export(
        &self,
        id: uuid::Uuid,
        destination: &Path,
        cancel: &CancellationToken,
        progress: impl FnMut(u64),
    ) -> Result<crate::journal::LocalExportReceipt> {
        self.check_export_destination(destination)?;
        Ok(self
            .journal
            .local_export_source(id)?
            .copy_to(destination, cancel, progress)?)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const DRIVE_LISTING: &str = "  --drive-id b!abc   (Dokumente)";

    #[test]
    fn no_terminal_names_the_flag_rather_than_blaming_a_parse() {
        // read_line returning zero is end of input: a sign-in driven from a
        // desktop, a script or a service reaches the question this way, after
        // the browser half has already succeeded.
        let error = format!("{:#}", drive_choice(0, "", DRIVE_LISTING).unwrap_err());
        assert!(
            error.contains("--drive-id b!abc"),
            "must name the flag and the id: {error}"
        );
        assert!(
            error.contains("sign-in itself succeeded"),
            "must say the sign-in was not wasted: {error}"
        );
        assert!(
            !error.contains("parse"),
            "must not blame an integer parse: {error}"
        );
    }

    #[test]
    fn pressing_enter_is_a_cancellation_and_not_an_absent_terminal() {
        let error = format!("{:#}", drive_choice(1, "\n", DRIVE_LISTING).unwrap_err());
        assert!(error.contains("cancelled"), "{error}");
        assert!(error.contains("--drive-id b!abc"), "{error}");
    }

    #[test]
    fn a_number_is_the_number() {
        assert_eq!(drive_choice(2, "2\n", DRIVE_LISTING).expect("a number"), 2);
    }

    #[test]
    fn a_word_is_reported_as_the_word_it_was() {
        let error = format!("{:#}", drive_choice(5, "two\n", DRIVE_LISTING).unwrap_err());
        assert!(
            error.contains("\"two\""),
            "must quote what was typed: {error}"
        );
    }
    #[test]
    fn a_pin_will_not_migrate_an_index_a_running_daemon_is_reading() {
        let temp = tempfile::tempdir().expect("fixture");
        let state = temp.path().join("state");
        private_dir(&state).expect("state");
        let account = fixture_account(AccessMode::ReadOnly);
        let id = account.id.clone();
        Settings {
            version: 2,
            accounts: vec![account],
        }
        .save(&state)
        .expect("seed");
        let directory = state.join("accounts").join(&id);
        private_dir(&directory).expect("account directory");
        // An index written by an older build.
        let db = directory.join("metadata.db");
        let old = rusqlite::Connection::open(&db).expect("db");
        old.pragma_update(None, "user_version", cirrove_store::SCHEMA_VERSION - 1)
            .expect("older schema");
        drop(old);

        // With no daemon holding the state, migrating on open is the normal path.
        super::refuse_to_migrate_under_a_running_daemon(&state, &db)
            .expect("no daemon, no refusal");

        // With one running, the refusal is the whole point: migrating here leaves
        // that daemon unable to read its own metadata while its mount stays up,
        // which reads as a network fault rather than as what it is.
        let held = daemon_lock(&state).expect("daemon lock");
        let refusal = super::refuse_to_migrate_under_a_running_daemon(&state, &db)
            .expect_err("a running daemon must block the migration");
        let message = format!("{refusal}");
        assert!(message.contains("Install the matching build"), "{message}");

        // An index that is there but cannot be read right now is not the same as
        // no index, and the guard must not treat it as one. SQLite fails this
        // read for reasons that pass -- a busy database, a hot journal, no
        // descriptors left -- and treating any of them as "nothing to migrate"
        // opens the gate at exactly the moment the guard cannot see.
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&db).expect("db metadata").permissions();
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o000))
            .expect("make the index unreadable");
        let blind = super::refuse_to_migrate_under_a_running_daemon(&state, &db);
        std::fs::set_permissions(&db, mode).expect("restore");
        let blind = format!(
            "{}",
            blind.expect_err("an unreadable index under a running daemon must be refused")
        );
        assert!(blind.contains("could not be read"), "{blind}");

        // Absent really is nothing to migrate, and must stay allowed -- a guard
        // that refused a first run would make a new account unusable.
        std::fs::remove_file(&db).expect("remove the index");
        super::refuse_to_migrate_under_a_running_daemon(&state, &db)
            .expect("no index at all is nothing to migrate");
        drop(held);
    }
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
    /// A message that names the package, rather than the errno.
    ///
    /// "could not start the browser: No such file or directory (os error 2)" is
    /// what a person saw on a fresh Arch install, and it named neither the
    /// program nor the package. Every distribution Cirrove ships for calls it
    /// xdg-utils, so the message can say so.
    #[test]
    fn a_missing_xdg_open_names_the_package_and_not_the_errno() {
        let said = browser_failure(std::io::Error::from(std::io::ErrorKind::NotFound)).to_string();
        assert!(said.contains("xdg-open"), "{said}");
        assert!(said.contains("xdg-utils"), "{said}");
        assert!(
            !said.contains("os error"),
            "an errno is not something a person can act on: {said}"
        );
        // Anything else keeps the context it had; only the missing-program case
        // has a remedy worth naming.
        let other =
            browser_failure(std::io::Error::from(std::io::ErrorKind::PermissionDenied)).to_string();
        assert!(other.contains("could not start the browser"), "{other}");
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
            version: 2,
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
            completed: false,
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

    #[test]
    fn completion_reports_failed_persistence_and_rolls_back_the_original_mode() {
        for wrote_before_failure in [false, true] {
            let temp = tempfile::tempdir().expect("fixture");
            let state = temp.path().join("state");
            crate::private_dir(&state).expect("state");
            let mut original = fixture_account(AccessMode::ReadOnly);
            original.enabled = false;
            Settings {
                version: 2,
                accounts: vec![original.clone()],
            }
            .save(&state)
            .expect("seed disabled");
            super::write_restore_marker(&state, &original.id, true).expect("marker");
            let desired = super::DesiredState {
                state: state.clone(),
                id: original.id.clone(),
                enabled: true,
                access: Some(original.access),
                completed: false,
            };
            let result =
                desired.complete_with(Some(AccessMode::ReadWrite), |candidate, location| {
                    assert_eq!(candidate.accounts[0].access, AccessMode::ReadWrite);
                    assert!(candidate.accounts[0].enabled);
                    if wrote_before_failure {
                        // Model an error after atomic rename, e.g. directory fsync.
                        candidate.save(location)?;
                    }
                    anyhow::bail!("injected settings persistence failure")
                });
            assert!(
                result.is_err(),
                "a failed settings commit cannot report a successful sign-in"
            );
            let restored = Settings::load(&state).expect("load");
            assert_eq!(restored.accounts[0].access, AccessMode::ReadOnly);
            assert!(restored.accounts[0].enabled);
            assert!(
                !super::restore_marker(&state, &original.id).exists(),
                "successful rollback clears its marker"
            );
        }
    }

    #[test]
    fn ordinary_completion_preserves_access_without_a_second_settings_write() {
        use std::os::unix::fs::MetadataExt;
        for mode in [AccessMode::ReadOnly, AccessMode::ReadWrite] {
            for enabled in [false, true] {
                let temp = tempfile::tempdir().expect("fixture");
                let state = temp.path().join("state");
                crate::private_dir(&state).expect("state");
                let mut original = fixture_account(mode);
                original.enabled = false;
                Settings {
                    version: 2,
                    accounts: vec![original.clone()],
                }
                .save(&state)
                .expect("seed");
                super::write_restore_marker(&state, &original.id, enabled).expect("marker");
                let saved_file = std::cell::RefCell::new(None::<File>);
                super::DesiredState {
                    state: state.clone(),
                    id: original.id.clone(),
                    enabled,
                    access: Some(mode),
                    completed: false,
                }
                .complete_with(None, |candidate, location| {
                    assert_eq!(candidate.accounts[0].access, mode);
                    candidate.save(location)?;
                    // Hold the committed inode so a second settings rename cannot reuse it.
                    *saved_file.borrow_mut() = Some(File::open(location.join("accounts.json"))?);
                    Ok(())
                })
                .expect("completion");
                let held = saved_file
                    .borrow()
                    .as_ref()
                    .expect("committed file")
                    .metadata()
                    .expect("metadata");
                let current =
                    std::fs::metadata(state.join("accounts.json")).expect("current metadata");
                assert_eq!(
                    (held.dev(), held.ino()),
                    (current.dev(), current.ino()),
                    "Drop wrote settings after successful completion"
                );
                let restored = Settings::load(&state).expect("load");
                assert_eq!(restored.accounts[0].access, mode);
                assert_eq!(restored.accounts[0].enabled, enabled);
                assert!(!super::restore_marker(&state, &original.id).exists());
            }
        }
    }

    #[test]
    fn pending_icloud_access_captures_before_login_and_cancellation_keeps_settings() {
        for requested in [
            None,
            Some(AccessMode::ReadOnly),
            Some(AccessMode::ReadWrite),
        ] {
            let temp = tempfile::tempdir().expect("fixture");
            let state = temp.path().join("state");
            crate::private_dir(&state).expect("state");
            let original = icloud_reauth_fixture();
            Settings {
                version: 2,
                accounts: vec![original.clone()],
            }
            .save(&state)
            .expect("seed");
            let before = std::fs::read(state.join("accounts.json")).expect("bytes");
            let pending = super::begin_reauthenticate_icloud(
                state.clone(),
                original.label.clone(),
                requested,
            )
            .expect("pending");
            assert_eq!(pending.access(), requested.unwrap_or(AccessMode::ReadOnly));
            assert_eq!(pending.apple_id(), original.identity.username);
            assert_eq!(pending.account_id(), original.id);
            drop(pending);
            assert_eq!(
                std::fs::read(state.join("accounts.json")).expect("bytes"),
                before
            );
            assert!(!super::restore_marker(&state, &original.id).exists());
        }
    }

    #[test]
    fn pending_icloud_access_refuses_settings_changed_while_password_is_entered() {
        let temp = tempfile::tempdir().expect("fixture");
        let state = temp.path().join("state");
        crate::private_dir(&state).expect("state");
        let original = icloud_reauth_fixture();
        let mut settings = Settings {
            version: 2,
            accounts: vec![original.clone()],
        };
        settings.save(&state).expect("seed");
        let pending = super::begin_reauthenticate_icloud(
            state.clone(),
            original.label.clone(),
            Some(AccessMode::ReadWrite),
        )
        .expect("pending");
        settings.accounts[0].cache_bytes += 4096;
        settings.save(&state).expect("changed during sign-in");
        let before = std::fs::read(state.join("accounts.json")).expect("bytes");
        assert!(
            super::suspend_validated_icloud_account(&pending.state, &pending.original).is_err()
        );
        assert_eq!(
            std::fs::read(state.join("accounts.json")).expect("bytes"),
            before
        );
        assert!(!super::restore_marker(&state, &original.id).exists());
    }

    #[test]
    fn pending_icloud_access_rejects_foreign_session_identity_without_settings_changes() {
        let temp = tempfile::tempdir().expect("fixture");
        let state = temp.path().join("state");
        crate::private_dir(&state).expect("state");
        let original = icloud_reauth_fixture();
        Settings {
            version: 2,
            accounts: vec![original.clone()],
        }
        .save(&state)
        .expect("seed");
        let pending = super::begin_reauthenticate_icloud(
            state.clone(),
            original.label.clone(),
            Some(AccessMode::ReadWrite),
        )
        .expect("pending");
        let snapshot = |apple_id: &str| {
            let key = probe_session_key(apple_id).expect("synthetic account hash");
            SecretString::from(serde_json::json!({
                "version": 1, "account_hash": key.strip_prefix("icloud-probe-").expect("hash"),
                "headers": { "scnt":"", "session_id":"", "session_token":"synthetic-not-a-credential", "trust_token":"", "account_country":"", "auth_attributes":"" },
                "drive_endpoint":"https://synthetic.icloud.com/", "docs_endpoint":"https://synthetic.icloud.com/", "cookies":[]
            }).to_string())
        };
        assert!(
            pending
                .validate_snapshot(&snapshot(&original.identity.username))
                .is_ok()
        );
        assert!(
            pending
                .validate_snapshot(&snapshot("foreign@example.invalid"))
                .is_err()
        );
        assert!(Settings::load(&state).expect("settings").accounts[0].enabled);
        assert!(!super::restore_marker(&state, &original.id).exists());
    }

    #[test]
    fn pending_icloud_requested_mode_reaches_durable_completion_preserves_explicit_policy() {
        for initial_access in [AccessMode::ReadOnly, AccessMode::ReadWrite] {
            for enabled in [false, true] {
                for requested in [
                    None,
                    Some(AccessMode::ReadOnly),
                    Some(AccessMode::ReadWrite),
                ] {
                    for session_saved in [false, true] {
                        let temp = tempfile::tempdir().expect("fixture");
                        let state = temp.path().join("state");
                        crate::private_dir(&state).expect("state");
                        let mut original = icloud_reauth_fixture();
                        original.enabled = enabled;
                        original.access = initial_access;
                        Settings {
                            version: 2,
                            accounts: vec![original.clone()],
                        }
                        .save(&state)
                        .expect("seed");
                        // Retained journal bytes must not be touched by mode or session persistence.
                        let journal = state.join("accounts").join(&original.id).join("journal");
                        std::fs::create_dir_all(&journal).expect("fixture retained directory");
                        std::fs::write(journal.join("retained-fixture"), b"retained bytes")
                            .expect("fixture bytes");
                        let pending = super::begin_reauthenticate_icloud(
                            state.clone(),
                            original.label.clone(),
                            requested,
                        )
                        .expect("pending");
                        let (_operation, desired) = super::suspend_validated_icloud_account(
                            &pending.state,
                            &pending.original,
                        )
                        .expect("suspend");
                        let saved = if session_saved {
                            Ok(())
                        } else {
                            Err(anyhow::anyhow!("synthetic session save failure"))
                        };
                        let result =
                            super::complete_icloud_session_save(desired, pending.requested, saved);
                        assert_eq!(result.is_ok(), session_saved);
                        let mut expected = original.clone();
                        if session_saved {
                            expected.access = requested.unwrap_or(initial_access);
                        }
                        let after = Settings::load(&state).expect("settings");
                        assert_eq!(
                            serde_json::to_value(&after.accounts[0]).expect("account"),
                            serde_json::to_value(&expected).expect("expected policy")
                        );
                        assert_eq!(
                            std::fs::read(journal.join("retained-fixture"))
                                .expect("retained bytes"),
                            b"retained bytes"
                        );
                        assert!(!super::restore_marker(&state, &original.id).exists());
                    }
                }
            }
        }
    }

    #[test]
    fn pending_icloud_policy_commit_failure_restores_original_mode_and_enabled_state() {
        for initial_access in [AccessMode::ReadOnly, AccessMode::ReadWrite] {
            for enabled in [false, true] {
                for published_before_error in [false, true] {
                    let temp = tempfile::tempdir().expect("fixture");
                    let state = temp.path().join("state");
                    crate::private_dir(&state).expect("state");
                    let mut original = icloud_reauth_fixture();
                    original.access = initial_access;
                    original.enabled = enabled;
                    Settings {
                        version: 2,
                        accounts: vec![original.clone()],
                    }
                    .save(&state)
                    .expect("seed");
                    let requested = match initial_access {
                        AccessMode::ReadOnly => AccessMode::ReadWrite,
                        AccessMode::ReadWrite => AccessMode::ReadOnly,
                    };
                    let pending = super::begin_reauthenticate_icloud(
                        state.clone(),
                        original.label.clone(),
                        Some(requested),
                    )
                    .expect("pending");
                    let (_operation, desired) =
                        super::suspend_validated_icloud_account(&pending.state, &pending.original)
                            .expect("suspend");
                    let result = desired.complete_with(Some(requested), |candidate, location| {
                        if published_before_error {
                            candidate.save(location)?;
                        }
                        anyhow::bail!("synthetic policy commit failure");
                    });
                    assert!(result.is_err());
                    let after = Settings::load(&state).expect("rollback");
                    assert_eq!(
                        serde_json::to_value(&after.accounts[0]).expect("after"),
                        serde_json::to_value(&original).expect("original")
                    );
                    assert!(!super::restore_marker(&state, &original.id).exists());
                }
            }
        }
    }

    #[derive(Default)]
    struct ConnectionVault {
        saves: std::sync::atomic::AtomicUsize,
        removes: std::sync::atomic::AtomicUsize,
    }
    #[async_trait::async_trait]
    impl CredentialVault for ConnectionVault {
        async fn load(&self, _: &str) -> Result<Option<SecretString>> {
            panic!("connection persistence does not read credentials")
        }
        async fn save(&self, _: &str, _: SecretString) -> Result<()> {
            self.saves.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
        async fn remove(&self, _: &str) -> Result<()> {
            self.removes
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn icloud_connection_invalid_identity_refuses_before_any_credential_write() {
        use std::sync::atomic::Ordering;
        for access in [AccessMode::ReadOnly, AccessMode::ReadWrite] {
            let temp = tempfile::tempdir().expect("fixture");
            let state = temp.path().join("state");
            let mut account = icloud_reauth_fixture();
            account.access = access;
            account.root_id = "foreign-root".into();
            let vault = ConnectionVault::default();
            assert!(
                super::save_icloud_connection_with(
                    &state,
                    &account,
                    SecretString::from("synthetic-session"),
                    &vault,
                    |_, _| panic!("identity must be rejected before settings persistence")
                )
                .await
                .is_err()
            );
            assert_eq!(vault.saves.load(Ordering::SeqCst), 0);
            assert_eq!(vault.removes.load(Ordering::SeqCst), 0);
            assert!(!state.exists());
        }
    }

    #[tokio::test]
    async fn icloud_connection_failed_commit_keeps_session_if_settings_were_published() {
        use std::sync::atomic::Ordering;
        for published_before_error in [false, true] {
            let temp = tempfile::tempdir().expect("fixture");
            let state = temp.path().join("state");
            crate::private_dir(&state).expect("state");
            let account = icloud_reauth_fixture();
            let vault = ConnectionVault::default();
            assert!(
                super::save_icloud_connection_with(
                    &state,
                    &account,
                    SecretString::from("synthetic-session"),
                    &vault,
                    |candidate, location| {
                        if published_before_error {
                            candidate.save(location)?;
                        }
                        anyhow::bail!("synthetic error at settings commit")
                    }
                )
                .await
                .is_err()
            );
            assert_eq!(vault.saves.load(Ordering::SeqCst), 1);
            assert_eq!(
                vault.removes.load(Ordering::SeqCst),
                usize::from(!published_before_error)
            );
            let settings = Settings::load(&state).expect("published settings");
            assert_eq!(settings.accounts.len(), usize::from(published_before_error));
            if published_before_error {
                assert_eq!(settings.accounts[0].credential_id, account.credential_id);
                assert!(settings.accounts[0].enabled);
            }
        }
    }

    #[tokio::test]
    async fn icloud_connection_owned_save_reports_published_account() {
        use std::sync::atomic::Ordering;
        for access in [AccessMode::ReadOnly, AccessMode::ReadWrite] {
            let temp = tempfile::tempdir().expect("fixture");
            let state = temp.path().join("state");
            crate::private_dir(&state).expect("state");
            let mut account = icloud_reauth_fixture();
            account.access = access;
            let vault = Arc::new(ConnectionVault::default());
            super::save_icloud_connection_owned(
                state.clone(),
                account.clone(),
                SecretString::from("synthetic-session"),
                vault.clone(),
            )
            .await
            .expect("durable connection");
            let settings = Settings::load(&state).expect("settings");
            assert_eq!(settings.accounts.len(), 1);
            assert_eq!(settings.accounts[0].access, access);
            assert_eq!(settings.accounts[0].credential_id, account.credential_id);
            assert_eq!(vault.saves.load(Ordering::SeqCst), 1);
            assert_eq!(vault.removes.load(Ordering::SeqCst), 0);
        }
    }

    struct PausedConnectionVault {
        stored: std::sync::atomic::AtomicBool,
        started: tokio::sync::Notify,
        resume: tokio::sync::Notify,
        removed: tokio::sync::Notify,
        fail_readback: bool,
    }
    #[async_trait::async_trait]
    impl CredentialVault for PausedConnectionVault {
        async fn load(&self, _: &str) -> Result<Option<SecretString>> {
            unreachable!("not used by connection persistence")
        }
        async fn save(&self, _: &str, _: SecretString) -> Result<()> {
            self.stored.store(true, std::sync::atomic::Ordering::SeqCst);
            self.started.notify_one();
            self.resume.notified().await;
            if self.fail_readback {
                anyhow::bail!("synthetic credential readback failure");
            }
            Ok(())
        }
        async fn remove(&self, _: &str) -> Result<()> {
            self.stored
                .store(false, std::sync::atomic::Ordering::SeqCst);
            self.removed.notify_one();
            Ok(())
        }
    }

    #[tokio::test]
    async fn icloud_connection_cancelled_save_finishes_cleanup_without_publishing() {
        use std::sync::atomic::{AtomicBool, Ordering};
        for initial_access in [AccessMode::ReadOnly, AccessMode::ReadWrite] {
            for fail_readback in [false, true] {
                let temp = tempfile::tempdir().expect("fixture");
                let state = temp.path().join("state");
                crate::private_dir(&state).expect("state");
                let vault = Arc::new(PausedConnectionVault {
                    stored: AtomicBool::new(false),
                    started: tokio::sync::Notify::new(),
                    resume: tokio::sync::Notify::new(),
                    removed: tokio::sync::Notify::new(),
                    fail_readback,
                });
                let mut account = icloud_reauth_fixture();
                account.access = initial_access;
                let task = tokio::spawn(super::save_icloud_connection_owned(
                    state.clone(),
                    account,
                    SecretString::from("synthetic-session"),
                    vault.clone(),
                ));
                tokio::time::timeout(std::time::Duration::from_secs(2), vault.started.notified())
                    .await
                    .expect("credential save reached deterministic boundary");
                assert!(vault.stored.load(Ordering::SeqCst));
                task.abort();
                assert!(task.await.expect_err("caller cancelled").is_cancelled());
                vault.resume.notify_one();
                tokio::time::timeout(std::time::Duration::from_secs(2), vault.removed.notified())
                    .await
                    .expect("owned persistence must finish cancelled cleanup");
                assert!(!vault.stored.load(Ordering::SeqCst));
                assert!(
                    Settings::load(&state)
                        .expect("settings")
                        .accounts
                        .is_empty()
                );
            }
        }
    }

    fn icloud_reauth_fixture() -> Account {
        let mut account = fixture_account(AccessMode::ReadOnly);
        account.registration = AppRegistration::ICloud;
        account.identity.tenant_id.clear();
        account.identity.graph_user_id.clear();
        account.drive.drive_type = "icloud_drive".into();
        account.root_id = "FOLDER::com.apple.CloudDocs::root".into();
        account.enabled = true;
        account
    }

    #[test]
    fn icloud_reauthentication_refuses_stale_account_before_disabling_or_marking() {
        for change in [
            "label",
            "identity",
            "credential",
            "enabled",
            "mount",
            "removed",
        ] {
            let temp = tempfile::tempdir().expect("fixture");
            let state = temp.path().join("state");
            crate::private_dir(&state).expect("state");
            let original = icloud_reauth_fixture();
            let mut settings = Settings {
                version: 2,
                accounts: vec![original.clone()],
            };
            match change {
                "label" => settings.accounts[0].label = "renamed".into(),
                "identity" => {
                    settings.accounts[0].identity.username = "different@example.test".into()
                }
                "credential" => {
                    settings.accounts[0].credential_id = uuid::Uuid::new_v4().to_string()
                }
                "enabled" => settings.accounts[0].enabled = false,
                "mount" => settings.accounts[0].mount_path = temp.path().join("different-mount"),
                _ => settings.accounts.clear(),
            }
            settings.save(&state).expect("changed settings");
            let before = std::fs::read(state.join("accounts.json")).expect("settings bytes");
            assert!(
                super::suspend_validated_icloud_account(&state, &original).is_err(),
                "{change}"
            );
            assert_eq!(
                std::fs::read(state.join("accounts.json")).expect("settings bytes"),
                before
            );
            assert!(!super::restore_marker(&state, &original.id).exists());
        }
    }

    #[test]
    fn config_guard_drop_releases_inherited_description_and_allows_icloud_restore() {
        use std::{
            io::Read,
            os::fd::{AsFd, AsRawFd},
            process::{Child, Command, Stdio},
            sync::mpsc,
            time::Duration,
        };
        // The real pre-exec interval has this same flock lifetime. A controlled
        // CLOEXEC seam retains it after exec without unsafe code or global fork.
        // This is mechanism coverage, not proof of CI's unique cause.
        #[derive(Debug)]
        struct Inheritor(Child);
        impl Drop for Inheritor {
            fn drop(&mut self) {
                // Kill/reap only this exact synthetic child, never a pattern.
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        fn inherit(owner: &(impl AsFd + AsRawFd)) -> Inheritor {
            let identity = rustix::fs::fstat(owner).expect("owner identity");
            let flags = rustix::io::fcntl_getfd(owner).expect("owner flags");
            rustix::io::fcntl_setfd(owner, flags & !rustix::io::FdFlags::CLOEXEC)
                .expect("controlled descriptor inheritance");
            let child = Command::new("python3")
                .args([
                    "-I", "-S", "-c",
                    "import os,signal,sys; signal.alarm(10); held=os.fstat(int(sys.argv[1])); assert (held.st_dev,held.st_ino)==(int(sys.argv[2]),int(sys.argv[3])); sys.stdout.buffer.write(b'ready'); sys.stdout.buffer.flush(); sys.stdin.buffer.read(1)",
                ])
                .arg(owner.as_raw_fd().to_string())
                .arg(identity.st_dev.to_string())
                .arg(identity.st_ino.to_string())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map(Inheritor);
            // Install child cleanup before this fallible flag restoration.
            rustix::io::fcntl_setfd(owner, flags).expect("restore original descriptor flags");
            let mut child = child.expect("controlled descriptor holder");
            let mut stdout = child.0.stdout.take().expect("readiness pipe");
            let (send, receive) = mpsc::channel();
            let reader = std::thread::spawn(move || {
                let mut ready = [0; 5];
                let _ = send.send(stdout.read_exact(&mut ready).is_ok() && ready == *b"ready");
            });
            assert!(
                receive
                    .recv_timeout(Duration::from_secs(5))
                    .expect("bounded readiness")
            );
            reader.join().expect("readiness reader");
            child
        }
        for originally_enabled in [false, true] {
            let temp = tempfile::Builder::new()
                .prefix("cirrove-config-inheritance-")
                .tempdir_in("/var/tmp")
                .expect("private fixture");
            let state = temp.path().join("state");
            crate::private_dir(&state).expect("state");
            let mut original = icloud_reauth_fixture();
            original.enabled = originally_enabled;
            Settings {
                version: 2,
                accounts: vec![original.clone()],
            }
            .save(&state)
            .expect("seed");
            let held = super::config_lock(&state).expect("parent settings owner");
            let mut child = inherit(&held);
            assert!(
                super::config_lock(&state).is_err(),
                "live parent remains exclusive"
            );
            assert!(
                super::config_lock(&state).is_err(),
                "a failed contender must not unlock its owner"
            );
            drop(held);
            assert!(child.0.try_wait().expect("holder status").is_none());
            let next = super::config_lock(&state).expect(
                "dropping logical owner must release flock while inherited descriptor stays alive",
            );
            assert!(
                super::config_lock(&state).is_err(),
                "successor owner remains exclusive"
            );
            drop(next);
            let (_operation, desired) = super::suspend_validated_icloud_account(&state, &original)
                .expect("validated suspend after inherited description release");
            assert!(!Settings::load(&state).expect("suspended settings").accounts[0].enabled);
            desired
                .complete(None)
                .expect("completed desired-state restore");
            assert_eq!(
                serde_json::to_value(
                    Settings::load(&state).expect("restored settings").accounts[0].clone()
                )
                .expect("account"),
                serde_json::to_value(&original).expect("original")
            );
            assert!(!super::restore_marker(&state, &original.id).exists());
            assert!(child.0.try_wait().expect("holder still alive").is_none());
        }
    }

    #[test]
    fn icloud_reauthentication_keeps_validated_read_only_access_and_desired_state() {
        for originally_enabled in [false, true] {
            let temp = tempfile::tempdir().expect("fixture");
            let state = temp.path().join("state");
            crate::private_dir(&state).expect("state");
            let mut original = icloud_reauth_fixture();
            original.enabled = originally_enabled;
            Settings {
                version: 2,
                accounts: vec![original.clone()],
            }
            .save(&state)
            .expect("seed");
            let (_operation, desired) = super::suspend_validated_icloud_account(&state, &original)
                .expect("validated account");
            assert!(!Settings::load(&state).expect("load").accounts[0].enabled);
            desired.complete(None).expect("restore");
            let after = Settings::load(&state).expect("load");
            assert_eq!(
                serde_json::to_value(&after.accounts[0]).expect("account"),
                serde_json::to_value(original).expect("original")
            );
        }
    }

    #[test]
    fn explicit_mount_preference_supersedes_interrupted_sign_in() {
        for (old_wish, requested) in [(true, false), (false, true)] {
            let temp = tempfile::tempdir().expect("fixture");
            let state = temp.path().join("state");
            crate::private_dir(&state).expect("state");
            let mut account = fixture_account(AccessMode::ReadWrite);
            account.enabled = false; // an interrupted sign-in's temporary disable
            Settings {
                version: 2,
                accounts: vec![account.clone()],
            }
            .save(&state)
            .expect("seed");
            super::write_restore_marker(&state, &account.id, old_wish).expect("old wish");

            super::set_enabled(&state, &account.label, requested).expect("explicit preference");
            super::heal_interrupted_sign_ins(&state).expect("heal");
            super::heal_interrupted_sign_ins(&state).expect("heal again");
            account.enabled = requested;
            assert_eq!(
                serde_json::to_value(&Settings::load(&state).expect("load").accounts[0])
                    .expect("actual account"),
                serde_json::to_value(&account).expect("expected account"),
                "an interrupted sign-in overrode the later explicit mount preference"
            );
            assert!(!super::restore_marker(&state, &account.id).exists());
        }
    }

    mod interrupted_preference_tests {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        fn seed(state: &Path, enabled: bool, marker: Option<bool>) -> Account {
            crate::private_dir(state).expect("state");
            let mut account = icloud_reauth_fixture();
            account.enabled = enabled;
            Settings {
                version: 2,
                accounts: vec![account.clone()],
            }
            .save(state)
            .expect("seed");
            if let Some(wish) = marker {
                super::super::write_restore_marker(state, &account.id, wish).expect("marker");
            }
            account
        }

        // Parallel process tests can inherit an operation's CLOEXEC open
        // description between fork and exec. Wait only for that bounded lock
        // contention after the logical owner has returned; never retry writes.
        fn wait_for_operation_release(state: &Path, id: &str) {
            wait_for_operation_release_with(state, id, || {});
        }
        fn wait_for_operation_release_with(state: &Path, id: &str, mut contended: impl FnMut()) {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
            loop {
                match super::super::account_operation(state, id) {
                    Ok(lease) => {
                        // Release inherited copies of this probe's description too.
                        fs2::FileExt::unlock(&lease).expect("release test operation lease");
                        drop(lease);
                        assert!(
                            std::time::Instant::now() <= deadline,
                            "operation release exceeded its bound"
                        );
                        return;
                    }
                    Err(error) => {
                        assert!(
                            error
                                .downcast_ref::<std::io::Error>()
                                .is_some_and(|e| e.kind() == std::io::ErrorKind::WouldBlock),
                            "unexpected operation release error: {error:#}"
                        );
                        assert!(
                            std::time::Instant::now() < deadline,
                            "operation owner was not released: {error:#}"
                        );
                        contended();
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                }
            }
        }

        #[test]
        fn bounded_operation_release_wait_preserves_busy_until_owner_releases() {
            let temp = tempfile::tempdir().expect("fixture");
            let state = temp.path().join("state");
            let account = seed(&state, false, Some(true));
            let before = std::fs::read(state.join("accounts.json")).expect("settings");
            let marker =
                std::fs::read(super::super::restore_marker(&state, &account.id)).expect("marker");
            let held =
                super::super::account_operation(&state, &account.id).expect("controlled owner");
            assert_eq!(
                set_enabled_by_id(&state, &account.id, true),
                Err(PreferenceRefusal::Busy)
            );
            assert_eq!(
                std::fs::read(state.join("accounts.json")).expect("settings"),
                before
            );
            assert_eq!(
                std::fs::read(super::super::restore_marker(&state, &account.id)).expect("marker"),
                marker
            );
            let (send, receive) = std::sync::mpsc::channel::<()>();
            let release = std::thread::spawn(move || {
                receive
                    .recv_timeout(std::time::Duration::from_secs(1))
                    .expect("bounded release signal");
                drop(held);
            });
            let mut send = Some(send);
            // CONTROL_WAIT_BEGIN: omission must expose the old immediate-success assumption.
            wait_for_operation_release_with(&state, &account.id, || {
                if let Some(send) = send.take() {
                    send.send(()).expect("release controlled owner");
                }
            });
            // CONTROL_WAIT_END
            set_enabled_by_id(&state, &account.id, true)
                .expect("preference after controlled owner release");
            assert!(send.is_none(), "the wait must observe actual contention");
            release.join().expect("controlled release thread closed");
            heal_interrupted_sign_ins(&state).expect("heal latest wish");
            assert!(Settings::load(&state).expect("load").accounts[0].enabled);
        }

        #[test]
        fn persistence_failure_retains_explicit_durable_intent() {
            for requested in [false, true] {
                for published_settings in [false, true] {
                    let temp = tempfile::tempdir().expect("fixture");
                    let state = temp.path().join("state");
                    let mut account = seed(&state, !requested, Some(!requested));
                    let result = super::super::update_enabled_with(
                        &state,
                        |a| a.id == account.id,
                        requested,
                        |candidate, directory| {
                            if published_settings {
                                candidate.save(directory)?;
                            }
                            bail!("injected settings persistence failure")
                        },
                    );
                    assert_eq!(result, Err(PreferenceRefusal::Unwritable));
                    assert_eq!(
                        super::super::interrupted_desired_state(&state, &account.id),
                        Some(requested),
                        "a reported persistence error must not revive the old wish"
                    );
                    wait_for_operation_release(&state, &account.id);
                    heal_interrupted_sign_ins(&state).expect("finish durable intent");
                    account.enabled = requested;
                    assert_eq!(
                        serde_json::to_value(&Settings::load(&state).expect("load").accounts[0])
                            .expect("actual"),
                        serde_json::to_value(&account).expect("expected")
                    );
                }
            }
        }

        #[test]
        fn cleanup_failure_retains_only_latest_wish() {
            for requested in [false, true] {
                let temp = tempfile::tempdir().expect("fixture");
                let state = temp.path().join("state");
                let account = seed(&state, !requested, Some(!requested));
                let result = super::super::update_enabled_with(
                    &state,
                    |a| a.id == account.id,
                    requested,
                    |candidate, directory| {
                        candidate.save(directory)?;
                        std::fs::set_permissions(
                            directory,
                            std::fs::Permissions::from_mode(0o500),
                        )?;
                        Ok(())
                    },
                );
                std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))
                    .expect("restore directory mode");
                result.expect("settings durable despite failed marker unlink");
                assert_eq!(
                    super::super::interrupted_desired_state(&state, &account.id),
                    Some(requested),
                    "failed cleanup must leave the new complete marker"
                );
                wait_for_operation_release(&state, &account.id);
                assert!(!heal_interrupted_sign_ins(&state).expect("same wish"));
                wait_for_operation_release(&state, &account.id);
                set_enabled_by_id(&state, &account.id, !requested).expect("later preference");
                heal_interrupted_sign_ins(&state).expect("heal again");
                assert_eq!(
                    Settings::load(&state).expect("load").accounts[0].enabled,
                    !requested
                );
            }
        }

        #[test]
        fn live_sign_in_refuses_both_public_preference_routes() {
            let temp = tempfile::tempdir().expect("fixture");
            let state = temp.path().join("state");
            let account = seed(&state, false, Some(true));
            let before = std::fs::read(state.join("accounts.json")).expect("settings");
            let marker =
                std::fs::read(super::super::restore_marker(&state, &account.id)).expect("marker");
            let _operation =
                account_operation(&state, &account.id).expect("sign-in owns operation");
            assert!(set_enabled(&state, &account.label, true).is_err());
            assert_eq!(
                set_enabled_by_id(&state, &account.id, true),
                Err(PreferenceRefusal::Busy)
            );
            assert_eq!(
                std::fs::read(state.join("accounts.json")).expect("settings"),
                before
            );
            assert_eq!(
                std::fs::read(super::super::restore_marker(&state, &account.id)).expect("marker"),
                marker
            );
        }

        #[test]
        fn marker_failure_before_rename_preserves_previous_file() {
            use std::os::unix::fs::MetadataExt;
            let temp = tempfile::tempdir().expect("fixture");
            let state = temp.path().join("state");
            let account = seed(&state, false, Some(true));
            let marker = super::super::restore_marker(&state, &account.id);
            let held = File::open(&marker).expect("held original marker");
            let bytes = std::fs::read(&marker).expect("marker");
            std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o500))
                .expect("deny new temporary marker");
            let result = super::super::write_restore_marker(&state, &account.id, false);
            std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))
                .expect("restore directory mode");
            assert!(result.is_err());
            assert_eq!(std::fs::read(&marker).expect("marker preserved"), bytes);
            assert_eq!(
                std::fs::metadata(&marker).expect("metadata").ino(),
                held.metadata().expect("held").ino()
            );
            assert_eq!(std::fs::read_dir(&state).expect("inventory").count(), 2);
        }

        #[test]
        fn healer_uses_current_account_not_initial_inventory() {
            let temp = tempfile::tempdir().expect("fixture");
            let state = temp.path().join("state");
            let account = seed(&state, true, None);
            let initial_inventory = Settings::load(&state).expect("initial inventory").accounts;
            // A sign-in starts and finishes abandoning its temporary disable
            // after the healer's inventory, but before ownership is acquired.
            let mut current = Settings::load(&state).expect("current");
            current.accounts[0].enabled = false;
            current.save(&state).expect("temporary disable");
            super::super::write_restore_marker(&state, &account.id, true).expect("new marker");
            assert!(
                super::super::heal_interrupted_accounts(&state, initial_inventory)
                    .expect("heal fresh state")
            );
            assert!(Settings::load(&state).expect("load").accounts[0].enabled);
        }

        #[test]
        fn id_preference_preserves_other_accounts_and_no_marker_behavior() {
            for marker in [None, Some(false), Some(true)] {
                for requested in [false, true] {
                    let temp = tempfile::tempdir().expect("fixture");
                    let state = temp.path().join("state");
                    let mut account = seed(&state, !requested, marker);
                    let mut other = fixture_account(AccessMode::ReadWrite);
                    other.id = "00000000-0000-4000-8000-000000000009".into();
                    other.label = "other".into();
                    other.mount_path = "/nonexistent/other-fixture".into();
                    let mut settings = Settings::load(&state).expect("load");
                    settings.accounts.push(other.clone());
                    settings.save(&state).expect("other account");
                    set_enabled_by_id(&state, &account.id, requested)
                        .expect("explicit ID preference");
                    heal_interrupted_sign_ins(&state).expect("heal");
                    account.enabled = requested;
                    let expected = Settings {
                        version: 2,
                        accounts: vec![account.clone(), other],
                    };
                    assert_eq!(
                        serde_json::to_value(Settings::load(&state).expect("load"))
                            .expect("actual"),
                        serde_json::to_value(expected).expect("expected")
                    );
                    assert!(!super::super::restore_marker(&state, &account.id).exists());
                }
            }
        }
    }

    /// A later sign-in must not inherit what an interrupted one left behind.
    ///
    /// This is the failure that actually happened. An earlier attempt disabled the
    /// account and died; a following attempt then read that file, believed
    /// `enabled: false` was the account's own wish, upgraded the permission and
    /// preserved the disable. Settings said `read_write` and the drive was not
    /// mounted at all, with nothing reported wrong. Only the durable marker can
    /// tell the two apart, because the disable itself has to be durable.
    #[test]
    fn a_later_sign_in_does_not_inherit_an_interrupted_ones_disable() {
        let temp = tempfile::tempdir().expect("fixture");
        let state = temp.path().join("state");
        crate::private_dir(&state).expect("state directory");
        let mut account = fixture_account(AccessMode::ReadOnly);
        account.enabled = true;
        let id = account.id.clone();

        // An attempt records its intent, disables, and is killed.
        Settings {
            version: 2,
            accounts: vec![account],
        }
        .save(&state)
        .expect("seed");
        super::write_restore_marker(&state, &id, true).expect("marker");
        let mut settings = Settings::load(&state).expect("load");
        settings.accounts[0].enabled = false;
        settings.save(&state).expect("disable");

        // What the next run reads is the leftover, not the wish.
        assert!(!Settings::load(&state).expect("load").accounts[0].enabled);
        assert_eq!(
            super::interrupted_desired_state(&state, &id),
            Some(true),
            "the marker is what remembers the account was enabled"
        );

        // The daemon puts it back and clears the marker.
        assert!(super::heal_interrupted_sign_ins(&state).expect("heal"));
        assert!(
            Settings::load(&state).expect("load").accounts[0].enabled,
            "an interrupted sign-in kept the drive unmounted"
        );
        assert_eq!(super::interrupted_desired_state(&state, &id), None);
        assert!(
            !super::heal_interrupted_sign_ins(&state).expect("heal"),
            "healing twice must not report a second repair"
        );
    }

    /// A sign-in that is still running owns its own restore.
    #[test]
    fn healing_leaves_an_account_alone_while_its_sign_in_holds_the_operation_lock() {
        let temp = tempfile::tempdir().expect("fixture");
        let state = temp.path().join("state");
        crate::private_dir(&state).expect("state directory");
        let mut account = fixture_account(AccessMode::ReadOnly);
        account.enabled = false;
        let id = account.id.clone();
        Settings {
            version: 2,
            accounts: vec![account],
        }
        .save(&state)
        .expect("seed");
        super::write_restore_marker(&state, &id, true).expect("marker");

        let held = super::account_operation(&state, &id).expect("operation lock");
        assert!(!super::heal_interrupted_sign_ins(&state).expect("heal"));
        assert!(
            !Settings::load(&state).expect("load").accounts[0].enabled,
            "re-enabling mid sign-in would fight the run that disabled it"
        );
        assert_eq!(super::interrupted_desired_state(&state, &id), Some(true));
        drop(held);
        // Parallel tests launch child processes. Between fork and exec a child can
        // retain the open description underlying flock, even with CLOEXEC, so the
        // operation lock this test just dropped can stay held for a short interval.
        // `heal_interrupted_sign_ins` reads that as a sign-in still running and
        // declines, which is correct of it and is what this assertion would
        // otherwise mistake for a failure to heal. Require prompt eventual healing
        // rather than instantaneous healing; the assertion above still proves that
        // a held lock suppresses it.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if super::heal_interrupted_sign_ins(&state).expect("heal") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "an account left disabled by an interrupted sign-in was never restored"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// An older package over a newer settings file must stop, not read what it
    /// does not understand: the version guard is what a rollback relies on.
    #[test]
    fn a_settings_file_from_a_newer_version_is_refused_rather_than_read() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        crate::private_dir(&state).unwrap();
        std::fs::write(
            state.join("accounts.json"),
            br#"{"version":99,"accounts":[]}"#,
        )
        .unwrap();
        let error = Settings::load(&state).unwrap_err().to_string();
        assert!(
            error.contains("unsupported account settings version"),
            "a newer settings file must be refused by name: {error}"
        );
    }

    #[test]
    fn a_google_write_grant_can_enable_a_my_drive_connection() {
        let mut account = fixture_account(AccessMode::ReadWrite);
        account.registration = AppRegistration::Google {
            client_id: "123-fixture.apps.googleusercontent.com".into(),
        };
        account.identity.tenant_id.clear();
        account.drive.id = "root-id".into();
        account.drive.drive_type = "my_drive".into();
        account.root_id = "root-id".into();
        let mut settings = Settings {
            version: 2,
            accounts: vec![account],
        };
        settings
            .validate()
            .expect("a disabled Google write grant is valid");
        settings.accounts[0].enabled = true;
        settings
            .validate()
            .expect("an enabled Google My Drive write connection is valid");
    }

    #[test]
    fn a_shared_drive_write_grant_can_enable_a_scoped_connection() {
        let mut account = fixture_account(AccessMode::ReadWrite);
        account.registration = AppRegistration::Google {
            client_id: "123-fixture.apps.googleusercontent.com".into(),
        };
        account.identity.tenant_id.clear();
        account.drive.id = "shared-drive-id".into();
        account.drive.drive_type = "shared_drive".into();
        account.root_id = "shared-drive-id".into();
        let mut settings = Settings {
            version: 2,
            accounts: vec![account],
        };
        settings
            .validate()
            .expect("a disabled Shared Drive account may hold a write grant");
        settings.accounts[0].enabled = true;
        settings
            .validate()
            .expect("an enabled Shared Drive account may use its write grant");
        assert!(write_provider(&settings.accounts[0]).is_ok());
    }

    #[test]
    fn icloud_settings_preserve_identity_checks_for_explicit_read_and_write_modes() {
        for access in [AccessMode::ReadOnly, AccessMode::ReadWrite] {
            let mut account = icloud_reauth_fixture();
            account.access = access;
            let settings = Settings {
                version: 2,
                accounts: vec![account.clone()],
            };
            settings.validate().expect("explicit local access policy");
            assert!(
                write_provider(&account).is_err(),
                "context-free factory must still refuse"
            );
            for field in 0..6 {
                let mut invalid = settings.clone();
                let account = &mut invalid.accounts[0];
                match field {
                    0 => account.drive.id = "foreign-drive".into(),
                    1 => account.drive.drive_type = "foreign-type".into(),
                    2 => account.root_id = "foreign-root".into(),
                    3 => account.identity.username.clear(),
                    4 => account.identity.tenant_id = "foreign-tenant".into(),
                    5 => account.identity.graph_user_id = "foreign-subject".into(),
                    _ => unreachable!(),
                }
                assert!(
                    invalid.validate().is_err(),
                    "identity field {field} must remain checked"
                );
            }
            let mut encoded = serde_json::to_value(&settings).expect("settings");
            encoded["accounts"][0]
                .as_object_mut()
                .expect("account")
                .remove("access");
            let defaulted: Settings = serde_json::from_value(encoded).expect("older settings");
            assert_eq!(defaulted.accounts[0].access, AccessMode::ReadOnly);
            defaulted
                .validate()
                .expect("missing access retains safe default");
        }
    }

    fn assert_account_owner_released(directory: &Path) {
        // Parallel tests may fork while a CLOEXEC descriptor is still open.
        // Its inherited open description can outlive the last logical owner
        // until exec. Only lock contention gets this bounded release allowance.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            match account_lock(directory) {
                Ok(reacquired) => {
                    drop(reacquired);
                    assert!(
                        std::time::Instant::now() <= deadline,
                        "account release exceeded its bound"
                    );
                    return;
                }
                Err(error) => {
                    assert!(
                        error
                            .downcast_ref::<std::io::Error>()
                            .is_some_and(|e| e.kind() == std::io::ErrorKind::WouldBlock),
                        "unexpected account release error: {error:#}"
                    );
                    assert!(
                        std::time::Instant::now() < deadline,
                        "last account owner was not released: {error:#}"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
        }
    }

    #[tokio::test]
    async fn owned_poll_account_lease_outlives_stopped_engine_until_provider_drop() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let account = icloud_reauth_fixture();
        let directory = temp.path().join("accounts").join(&account.id);
        let owner = Arc::new(account_lock(&directory)?);
        let lifetime = Arc::downgrade(&owner);
        let provider = provider_with_owned_state(&account, temp.path(), owner.clone())?;
        let engine = crate::engine::Engine::new_with_owner(
            account,
            provider.clone(),
            temp.path().into(),
            owner,
        )
        .await?;
        engine.stop().await;
        drop(engine);
        assert!(
            account_lock(&directory).is_err(),
            "outliving owned provider must retain account lease"
        );
        drop(provider);
        assert!(
            lifetime.upgrade().is_none(),
            "last owned provider drop must release every logical account owner"
        );
        assert_account_owner_released(&directory);
        Ok(())
    }

    #[tokio::test]
    async fn owned_poll_failed_engine_construction_releases_last_account_lease() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let account = icloud_reauth_fixture();
        let directory = temp.path().join("accounts").join(&account.id);
        let owner = Arc::new(account_lock(&directory)?);
        let lifetime = Arc::downgrade(&owner);
        let provider = provider_with_owned_state(&account, temp.path(), owner.clone())?;
        std::fs::write(
            directory.join("metadata.db"),
            b"synthetic invalid SQLite header",
        )?;
        assert!(
            crate::engine::Engine::new_with_owner(account, provider, temp.path().into(), owner)
                .await
                .is_err()
        );
        assert!(
            lifetime.upgrade().is_none(),
            "failed Engine construction must release every logical account owner"
        );
        assert_account_owner_released(&directory);
        Ok(())
    }

    fn fixture_account(access: AccessMode) -> Account {
        Account {
            id: "00000000-0000-4000-8000-000000000007".into(),
            label: "fixture".into(),
            registration: cirrove_auth::AppRegistration::Microsoft {
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
    mod reauth_manager_retained_coupling {
        include!("accounts/reauth_manager_retained_coupling.rs");
    }
}
