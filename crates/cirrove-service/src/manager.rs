//! Desired account state, mount ownership and status. Observation failures never
//! count as an ejection; mount directories are checked on every mount attempt.
use crate::{
    accounts::{Account, Settings, provider},
    engine::{Engine, FeedHealth},
    filesystem::CloudFs,
};
use anyhow::{Result, bail};
use cirrove_core::{CancellationToken, ReadProvider};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::RwLock;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountStatus {
    pub label: String,
    pub account: String,
    pub tenant: String,
    pub drive: String,
    pub mount_path: PathBuf,
    pub mounted: bool,
    pub state: String,
    pub feeds: Vec<FeedHealth>,
    #[serde(default)]
    pub directory_freshness: crate::DirectoryFreshness,
    pub indexed_feeds: u64,
    pub indexed_items: u64,
}
pub type ProviderFactory = Arc<dyn Fn(&Account) -> Result<Arc<dyn ReadProvider>> + Send + Sync>;
#[derive(Default)]
pub struct Manager {
    pub status: RwLock<Vec<AccountStatus>>,
}
struct Running {
    config: Account,
    engine: Arc<Engine>,
    session: Option<fuser::BackgroundSession>,
    mount_error: Option<String>,
}
impl Running {
    async fn stop(mut self) {
        self.engine.stop().await;
        if let Some(session) = self.session.take() {
            let result = tokio::task::spawn_blocking(move || session.umount_and_join()).await;
            if !matches!(result, Ok(Ok(()))) {
                tracing::warn!("Cirrove mount shutdown did not finish cleanly");
            }
        }
    }
}
impl Manager {
    pub fn start(
        state: PathBuf,
        cancel: CancellationToken,
    ) -> (Arc<Self>, tokio::task::JoinHandle<()>) {
        Self::start_with_provider(state, cancel, Arc::new(|account| Ok(provider(account)?)))
    }
    /// The same account lifecycle is used for production and deterministic providers.
    pub fn start_with_provider(
        state: PathBuf,
        cancel: CancellationToken,
        factory: ProviderFactory,
    ) -> (Arc<Self>, tokio::task::JoinHandle<()>) {
        let manager = Arc::new(Self::default());
        let worker = manager.clone();
        let task = tokio::spawn(async move {
            worker.run(state, cancel, factory).await;
        });
        (manager, task)
    }
    async fn run(
        self: Arc<Self>,
        state: PathBuf,
        cancel: CancellationToken,
        factory: ProviderFactory,
    ) {
        let mut running: HashMap<String, Running> = HashMap::new();
        loop {
            if cancel.is_cancelled() {
                break;
            }
            let directory = state.clone();
            let settings = tokio::task::spawn_blocking(move || Settings::load(&directory)).await;
            match settings {
                Ok(Ok(settings)) => {
                    let desired: HashMap<_, _> = settings
                        .accounts
                        .iter()
                        .filter(|a| a.enabled)
                        .map(|a| (a.id.clone(), a))
                        .collect();
                    let remove: Vec<_> = running
                        .iter()
                        .filter(|(id, r)| {
                            desired.get(*id).is_none_or(|new| {
                                serde_json::to_value(new).ok()
                                    != serde_json::to_value(&r.config).ok()
                            })
                        })
                        .map(|(id, _)| id.clone())
                        .collect();
                    for id in remove {
                        if let Some(old) = running.remove(&id) {
                            old.stop().await;
                        }
                    }
                    let mut launch_errors = HashMap::new();
                    for account in settings.accounts.iter().filter(|a| a.enabled) {
                        if cancel.is_cancelled() {
                            break;
                        }
                        if running.contains_key(&account.id) {
                            continue;
                        }
                        match Self::launch(account.clone(), state.clone(), &factory).await {
                            Ok(active) => {
                                running.insert(account.id.clone(), active);
                            }
                            Err(_) => {
                                launch_errors
                                    .insert(account.id.clone(), "account_start_failed".to_string());
                            }
                        }
                    }
                    // Unknown mount state is not an empty mount list. Retain the
                    // session until a successful observation proves it was ejected.
                    let mounts = tokio::fs::read_to_string("/proc/self/mountinfo").await;
                    let mut statuses = vec![];
                    for account in &settings.accounts {
                        let mut status = AccountStatus {
                            label: account.label.clone(),
                            account: account.identity.username.clone(),
                            tenant: account.identity.tenant_id.clone(),
                            drive: account.drive.name.clone(),
                            mount_path: account.mount_path.clone(),
                            mounted: false,
                            state: launch_errors.remove(&account.id).unwrap_or_else(|| {
                                if account.enabled {
                                    "starting"
                                } else {
                                    "disabled"
                                }
                                .into()
                            }),
                            feeds: vec![],
                            directory_freshness: crate::DirectoryFreshness::default(),
                            indexed_feeds: 0,
                            indexed_items: 0,
                        };
                        if let Some(active) = running.get_mut(&account.id) {
                            match &mounts {
                                Ok(mounts) => {
                                    let source = format!("cirrove:{}", account.id);
                                    // A same-type mount can be stale or belong to
                                    // another account. Only our owned session
                                    // and matching identity count as mounted.
                                    status.mounted = active.session.is_some()
                                        && mount_record_at(mounts, &account.mount_path)
                                            == Some(("fuse.cirrove", source.as_str()));
                                    if status.mounted
                                        && active.mount_error.as_deref()
                                            == Some("mount observation unavailable")
                                    {
                                        active.mount_error = None;
                                    }
                                    if !status.mounted && !cancel.is_cancelled() {
                                        if let Some(session) = active.session.take() {
                                            let _ = tokio::task::spawn_blocking(move || {
                                                session.umount_and_join()
                                            })
                                            .await;
                                        }
                                        match mount_checked(active.engine.clone()).await {
                                            Ok(session) => {
                                                active.session = Some(session);
                                                active.mount_error = None;
                                                status.mounted = true;
                                            }
                                            Err(_) => {
                                                active.mount_error=Some("mount unavailable; directory must be empty and unmounted".into());
                                            }
                                        }
                                    }
                                }
                                Err(_) => {
                                    status.mounted = active.session.is_some();
                                    active.mount_error =
                                        Some("mount observation unavailable".into());
                                }
                            }
                            status.feeds = active.engine.health().await;
                            status.directory_freshness = active.engine.directory_freshness();
                            let db = active.engine.db.clone();
                            if let Ok(Ok((feeds, items))) = tokio::task::spawn_blocking(move || {
                                cirrove_store::Store::open(db)?.counts()
                            })
                            .await
                            {
                                status.indexed_feeds = feeds;
                                status.indexed_items = items;
                            }

                            status.state = active.mount_error.clone().unwrap_or_else(|| {
                                if status.feeds.is_empty() {
                                    "starting"
                                } else if status.feeds.iter().any(|f| f.state == "sign_in_required")
                                {
                                    "sign_in_required"
                                } else if status.feeds.iter().any(|f| f.state != "ready") {
                                    "updating_or_offline"
                                } else {
                                    "ready"
                                }
                                .into()
                            });
                        }
                        statuses.push(status);
                    }
                    *self.status.write().await = statuses;
                }
                _ => tracing::warn!(
                    "Cirrove account settings could not be loaded; retaining running accounts"
                ),
            }
            tokio::select! {biased;_=cancel.cancelled()=>break,_=tokio::time::sleep(Duration::from_secs(5))=>()}
        }
        for (_, active) in running {
            active.stop().await;
        }
        self.status.write().await.clear();
    }
    async fn launch(
        account: Account,
        state: PathBuf,
        factory: &ProviderFactory,
    ) -> Result<Running> {
        let graph = factory(&account)?;
        let engine = Engine::new(account.clone(), graph, state).await?;
        if let Err(error) = engine.start().await {
            engine.stop().await;
            return Err(error);
        }
        let (session, mount_error) = match mount_checked(engine.clone()).await {
            Ok(session) => (Some(session), None),
            Err(_) => (
                None,
                Some("mount unavailable; directory must be empty and unmounted".into()),
            ),
        };
        Ok(Running {
            config: account,
            engine,
            session,
            mount_error,
        })
    }
}
async fn mount_checked(engine: Arc<Engine>) -> Result<fuser::BackgroundSession> {
    let path = engine.account.mount_path.clone();
    recover_disconnected_mount(&engine.account).await?;
    // CloudFs captures this async runtime, while filesystem checks and the FUSE
    // handshake execute on a blocking worker.
    let fs = CloudFs::new(engine)?;
    tokio::task::spawn_blocking(move || {
        validate_mount_directory(&path)?;
        Ok(fs.mount(&path)?)
    })
    .await?
}

/// An account's owner lock is held by Engine before this is called. A crashed
/// FUSE session can remain mounted with ENOTCONN after its process has exited.
/// Detach only that exact account's disconnected mount; never a live/foreign one.
async fn recover_disconnected_mount(account: &Account) -> Result<()> {
    let mounts = tokio::fs::read_to_string("/proc/self/mountinfo").await?;
    let Some((kind, source)) = mount_record_at(&mounts, &account.mount_path) else {
        return Ok(());
    };
    if kind != "fuse.cirrove" || source != format!("cirrove:{}", account.id) {
        bail!("mount path belongs to another filesystem or account");
    }
    let path = account.mount_path.clone();
    let disconnected = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::task::spawn_blocking(move || {
            std::fs::symlink_metadata(path)
                .is_err_and(|error| error.raw_os_error() == Some(libc::ENOTCONN))
        }),
    )
    .await??;
    if !disconnected {
        bail!("existing Cirrove mount is live or cannot be verified as disconnected");
    }
    let mut command = tokio::process::Command::new("fusermount3");
    command
        .args(["-u", "-z", "--"])
        .arg(&account.mount_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let status = tokio::time::timeout(Duration::from_secs(5), command.status()).await??;
    if !status.success() {
        bail!("could not detach disconnected Cirrove mount");
    }
    tracing::info!("recovered disconnected Cirrove mount");
    Ok(())
}
pub fn validate_mount_directory(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
    {
        bail!("mount path must be absolute and normalized");
    }
    let mounts = std::fs::read_to_string("/proc/self/mountinfo")?;
    // Reject mounts at or below this path, including other cloud clients. No
    // filesystem probes into a potentially stalled foreign FUSE filesystem.
    if mount_at(&mounts, path).is_some() {
        bail!("mount path is already mounted");
    }
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        if current != path
            && mount_at(&mounts, &current).is_some_and(|kind| kind.starts_with("fuse"))
        {
            bail!("mount path cannot be inside another FUSE mount");
        }
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if !meta.is_dir() || meta.file_type().is_symlink() => {
                bail!("mount path must use real directories")
            }
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::create_dir(&current)?,
            Err(e) => return Err(e.into()),
        }
    }
    if std::fs::read_dir(path)?.next().is_some() {
        bail!("mount directory must be empty");
    }
    Ok(())
}
fn mount_at<'a>(mounts: &'a str, path: &Path) -> Option<&'a str> {
    mount_record_at(mounts, path).map(|(kind, _)| kind)
}
fn mount_record_at<'a>(mounts: &'a str, path: &Path) -> Option<(&'a str, &'a str)> {
    let encoded = path
        .to_string_lossy()
        .replace('\\', "\\134")
        .replace(' ', "\\040")
        .replace('\t', "\\011")
        .replace('\n', "\\012");
    mounts.lines().find_map(|line| {
        let (fields, filesystem) = line.split_once(" - ")?;
        if fields.split_whitespace().nth(4) != Some(encoded.as_str()) {
            return None;
        }
        let mut parts = filesystem.split_whitespace();
        Some((parts.next()?, parts.next()?))
    })
}
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[test]
    fn recognizes_foreign_mounts_and_escaped_paths() {
        let data = "1 0 0:1 / /tmp/Cloud\\040drive rw - fuse.cirrove cirrove:x rw\n2 0 0:2 / /tmp/foreign rw - fuse.rclone rclone rw";
        assert_eq!(
            mount_at(data, Path::new("/tmp/Cloud drive")),
            Some("fuse.cirrove")
        );
        assert_eq!(
            mount_at(data, Path::new("/tmp/foreign")),
            Some("fuse.rclone")
        );
        assert_eq!(mount_at(data, Path::new("/tmp/missing")), None);
    }
    #[test]
    fn remount_refuses_new_local_files_and_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("mount");
        validate_mount_directory(&path).unwrap();
        std::fs::write(path.join("local.txt"), "preserve").unwrap();
        assert!(validate_mount_directory(&path).is_err());
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        assert!(validate_mount_directory(&alias).is_err());
        assert_eq!(
            std::fs::read_to_string(path.join("local.txt")).unwrap(),
            "preserve"
        );
    }
}
