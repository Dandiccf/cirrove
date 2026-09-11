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
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)?;
    file.write_all(&serde_json::to_vec(
        &serde_json::json!({ "enabled": enabled }),
    )?)?;
    file.sync_all()?;
    File::open(state)?.sync_all()?;
    Ok(())
}

/// Undo a sign-in that never put an account back, when none is running.
///
/// The daemon does this because the user cannot see what to undo: an interrupted
/// sign-in leaves the account disabled, a disabled account is not mounted, and a
/// missing mount looks like nothing at all. Gated on the per-account operation
/// lock, so it can never fight a sign-in still waiting on a browser -- that lock
/// is held for exactly as long as the disable is meant to last.
pub fn heal_interrupted_sign_ins(state: &Path) -> Result<bool> {
    let mut healed = false;
    for account in Settings::load(state)?.accounts {
        let Some(enabled) = interrupted_desired_state(state, &account.id) else {
            continue;
        };
        let Ok(_operation) = account_operation(state, &account.id) else {
            continue; // a sign-in is in progress; its own guard owns the restore
        };
        if account.enabled != enabled {
            let _lock = config_lock(state)?;
            let mut settings = Settings::load(state)?;
            if let Some(stored) = settings.accounts.iter_mut().find(|a| a.id == account.id) {
                stored.enabled = enabled;
            }
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
            settings.save(&self.state)?;
            // Only after the settings write lands. A marker removed first would
            // lose the intent if the write failed.
            let _ = std::fs::remove_file(restore_marker(&self.state, &self.id));
            Ok(())
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
        access: None,
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
        println!("{label} is signed in again; permission is now {requested:?}.");
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
        provider: "onedrive".into(),
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
        provider: "onedrive".into(),
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
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[test]
    fn a_pin_will_not_migrate_an_index_a_running_daemon_is_reading() {
        let temp = tempfile::tempdir().expect("fixture");
        let state = temp.path().join("state");
        private_dir(&state).expect("state");
        let account = fixture_account(AccessMode::ReadOnly);
        let id = account.id.clone();
        Settings {
            version: 1,
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
            version: 1,
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
            version: 1,
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
