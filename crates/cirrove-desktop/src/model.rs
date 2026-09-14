use cirrove_service::{Status, accounts::Settings, manager::AccountStatus};
use std::path::PathBuf;
mod failures;
pub use failures::{ServiceFailure, SettingsFailure};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    Connected,
    Updating,
    Limited,
    SignInRequired,
    Mounting,
    Unmounting,
    Unmounted,
    Unavailable,
    ServiceUnavailable,
    IncompatibleService,
    WaitingForService,
}
impl ConnectionState {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Connected => "Connected",
            Self::Updating => "Updating or offline",
            Self::Limited => "Provider is limiting requests",
            Self::SignInRequired => "Sign in again",
            Self::Mounting => "Mounting…",
            Self::Unmounting => "Unmounting…",
            Self::Unmounted => "Unmounted",
            Self::Unavailable => "Connection needs attention",
            Self::ServiceUnavailable => "Service unavailable",
            Self::IncompatibleService => "Service update required",
            Self::WaitingForService => "Waiting for service",
        }
    }
    pub fn description(&self) -> &'static str {
        match self {
            Self::Connected => {
                "File contents download when opened. Cirrove checks for folder changes in the background."
            }
            Self::Updating => {
                "Cirrove is checking for changes. Previously listed files may be out of date."
            }
            Self::Limited => {
                "The cloud provider has asked Cirrove to wait. It will retry automatically."
            }
            Self::SignInRequired => {
                "This account needs a new sign-in before cloud access can resume."
            }
            Self::Mounting => "Waiting for the service to confirm the mount.",
            Self::Unmounting => "Waiting for the service to release this mount.",
            Self::Unmounted => "This connection is saved and can be mounted again.",
            Self::Unavailable => {
                "The mount or a cloud library is unavailable. Check the connection and mount location."
            }
            Self::ServiceUnavailable => {
                "The Cirrove service could not be reached. This is saved configuration, not a current mount status."
            }
            Self::IncompatibleService => {
                "The running service cannot confirm this account's identity. Update the service before using mount controls."
            }
            Self::WaitingForService => {
                "Waiting for the service to observe the saved account settings."
            }
        }
    }
    pub fn busy(&self) -> bool {
        matches!(
            self,
            Self::Mounting | Self::Unmounting | Self::WaitingForService
        )
    }
    pub fn warning(&self) -> bool {
        !matches!(
            self,
            Self::Connected
                | Self::Unmounted
                | Self::Mounting
                | Self::Unmounting
                | Self::WaitingForService
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountCard {
    pub id: String,
    /// The name the CLI and the daemon know the account by; every verb takes it.
    pub label: String,
    pub title: String,
    pub username: String,
    pub tenant: String,
    pub mount_path: PathBuf,
    pub enabled: bool,
    pub mounted: bool,
    pub controls_available: bool,
    pub state: ConnectionState,
    pub cache_bytes: u64,
    pub library_count: usize,
    /// Changes the daemon has stopped retrying because the cloud refused them.
    pub stuck: u64,
    /// Saves that did not reach the cloud -- uploads the provider refused or
    /// that failed. The file is here; the cloud has an older version or none.
    pub failed_uploads: u64,
    /// Whether the grant allows changes; a read-only drive shows as such.
    pub writable: bool,
    /// The app registration this account signed in through, so connecting a
    /// second drive can start from it rather than from an empty field.
    pub client_id: String,
    pub authority: String,
    /// What this account keeps offline, ready to show.
    pub kept_offline: Vec<KeptOffline>,
    /// One sentence about how much of the cache pinning has claimed, or `None`
    /// when the daemon did not say (an older one, or an account not running).
    pub pin_budget: Option<String>,
}

/// One pinned item, in the words the window shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeptOffline {
    /// The provider item id. Not shown; it is what `unpin` takes, and it is the
    /// only handle that stays right when a file is renamed in the cloud.
    pub item: String,
    /// What to show: the item's path in the drive, or the id when the daemon
    /// could not resolve one. An id is unreadable, but it is honest, and it is
    /// still the thing the button acts on.
    pub name: String,
    /// How much of it is actually on this computer, in words.
    pub detail: String,
    /// A folder pinned with everything under it.
    pub recursive: bool,
}
impl KeptOffline {
    pub fn from_status(pin: &cirrove_service::engine::PinStatus) -> Self {
        // Reserved and resident are different questions and the row answers the
        // one a person is asking: is it here? A pin that reserved space and
        // fetched nothing keeps nothing, and saying "66 KB" of it would promise
        // an offline read that cannot be served.
        let detail = if pin.blocks == 0 || pin.resident == 0 {
            format!(
                "{} reserved, nothing fetched yet",
                cirrove_service::human_bytes(pin.reserved)
            )
        } else if pin.resident >= pin.reserved {
            format!(
                "{} on this computer",
                cirrove_service::human_bytes(pin.resident)
            )
        } else {
            format!(
                "{} of {} on this computer",
                cirrove_service::human_bytes(pin.resident),
                cirrove_service::human_bytes(pin.reserved)
            )
        };
        Self {
            item: pin.item.clone(),
            name: pin.path.clone().unwrap_or_else(|| pin.item.clone()),
            detail,
            recursive: pin.recursive,
        }
    }
}
impl AccountCard {
    pub fn action_label(&self) -> &'static str {
        if !self.enabled {
            "Mount"
        } else if self.mounted {
            "Unmount"
        } else {
            "Cancel mount"
        }
    }
}

pub struct Snapshot {
    pub settings: Result<Settings, SettingsFailure>,
    pub status: Result<Status, ServiceFailure>,
    /// What changed lately, per account label, latest first. Empty when the
    /// daemon does not answer `recent` (an older one) or has nothing.
    pub activity: Vec<(String, cirrove_service::RecentReply)>,
}
/// One line of recent activity, ready to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivityEntry {
    /// The account's label.
    pub account: String,
    /// The file or folder's name.
    pub name: String,
    /// What happened, in words: "changed in the cloud", "removed in the
    /// cloud", "saved here · uploading", ...
    pub what: String,
    /// True for a local save the cloud refused, so a row can warn.
    pub warning: bool,
}
impl ActivityEntry {
    /// The lines for one account's answer, cloud changes first, latest first.
    pub fn from_reply(account: &str, reply: &cirrove_service::RecentReply) -> Vec<Self> {
        let remote = reply.remote.iter().map(|change| Self {
            account: account.to_owned(),
            name: if change.kind == "folder" {
                format!("{}/", change.name)
            } else {
                change.name.clone()
            },
            what: if change.removed {
                "removed in the cloud".to_owned()
            } else {
                "changed in the cloud".to_owned()
            },
            warning: false,
        });
        let local = reply.local.iter().map(|change| {
            let (what, warning) = match change.state.as_str() {
                "uploaded" => ("saved here · in the cloud", false),
                "pending" | "preparing" => ("saved here · waiting to upload", false),
                "uploading" | "verifying" | "verifyrequired" => ("saved here · uploading", false),
                "conflict" => ("saved here · the cloud refused it", true),
                "failed" => ("saved here · upload failed", true),
                other => (other, false),
            };
            Self {
                account: account.to_owned(),
                name: change.name.clone(),
                what: what.to_owned(),
                warning,
            }
        });
        remote.chain(local).collect()
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Overview {
    pub accounts: Vec<AccountCard>,
    /// Recent activity across accounts, latest first within each account.
    pub activity: Vec<ActivityEntry>,
    pub service_reachable: bool,
    pub settings_available: bool,
    pub settings_error: Option<SettingsFailure>,
    pub service_error: Option<ServiceFailure>,
    /// The daemon is running a version that is no longer the one on disk,
    /// because a package upgrade replaced its binary underneath it. Nothing
    /// else tells the person, and the symptom is subtle -- the upgrade appears
    /// to have done nothing at all.
    pub restart_required: bool,
}
impl Overview {
    pub fn from_snapshot(snapshot: Snapshot) -> Self {
        let reachable = snapshot.status.is_ok();
        let compatible = snapshot
            .status
            .as_ref()
            .is_ok_and(|s| s.protocol_version == cirrove_service::STATUS_PROTOCOL_VERSION);
        // Only from a compatible daemon: an older one always answers false,
        // which would be indistinguishable from "no upgrade is pending".
        let restart_required = snapshot
            .status
            .as_ref()
            .is_ok_and(|s| compatible && s.restart_required);
        let service_error = match &snapshot.status {
            Err(error) => Some(error.clone()),
            Ok(status) if !compatible => Some(ServiceFailure::Incompatible {
                expected: cirrove_service::STATUS_PROTOCOL_VERSION,
                actual: status.protocol_version,
            }),
            Ok(_) => None,
        };
        let settings = match snapshot.settings {
            Ok(settings) => settings,
            Err(error) => {
                return Self {
                    accounts: vec![],
                    activity: vec![],
                    service_reachable: reachable,
                    settings_available: false,
                    settings_error: Some(error),
                    service_error,
                    restart_required,
                };
            }
        };
        let accounts = settings
            .accounts
            .iter()
            .map(|account| {
                // UUID is necessary, and the remaining identity/location fields must
                // agree too. A stale service snapshot cannot describe a new location.
                let status = snapshot
                    .status
                    .as_ref()
                    .ok()
                    .filter(|_| compatible)
                    .and_then(|s| {
                        s.accounts.iter().find(|s| {
                            s.account_id == account.id
                                && s.drive_id == account.drive.id
                                && s.root_id == account.root_id
                                && s.mount_path == account.mount_path
                                && s.account == account.identity.username
                                && s.tenant == account.identity.tenant_id
                        })
                    });
                let state = match status {
                    Some(s)
                        if s.enabled != account.enabled
                            || s.mounted != account.enabled
                                && matches!(
                                    s.state.as_str(),
                                    "ready" | "disabled" | "starting"
                                ) =>
                    {
                        if account.enabled {
                            ConnectionState::Mounting
                        } else {
                            ConnectionState::Unmounting
                        }
                    }
                    Some(s) => connection_state(s),
                    None if !reachable => ConnectionState::ServiceUnavailable,
                    None if compatible => ConnectionState::WaitingForService,
                    None => ConnectionState::IncompatibleService,
                };
                AccountCard {
                    id: account.id.clone(),
                    label: account.label.clone(),
                    title: format!("OneDrive · {}", account.drive.name),
                    username: account.identity.username.clone(),
                    tenant: account.identity.tenant_id.clone(),
                    mount_path: account.mount_path.clone(),
                    enabled: account.enabled,
                    mounted: status.is_some_and(|s| s.mounted),
                    controls_available: compatible,
                    state,
                    cache_bytes: account.cache_bytes,
                    library_count: status.map_or(0, |s| s.feeds.len()),
                    stuck: status.map_or(0, |s| s.stuck_changes),
                    failed_uploads: status.map_or(0, |s| s.failed_uploads),
                    writable: account.access == cirrove_auth::AccessMode::ReadWrite,
                    client_id: account.registration.client_id.clone(),
                    authority: account.registration.authority.clone(),
                    kept_offline: status
                        .map(|s| s.pins.iter().map(KeptOffline::from_status).collect())
                        .unwrap_or_default(),
                    pin_budget: status.map(|s| s.pin_budget.explain()),
                }
            })
            .collect();
        let activity = snapshot
            .activity
            .iter()
            .flat_map(|(label, reply)| ActivityEntry::from_reply(label, reply))
            .collect();
        Self {
            accounts,
            activity,
            service_reachable: reachable,
            settings_available: true,
            settings_error: None,
            service_error,
            restart_required,
        }
    }
}
fn connection_state(s: &AccountStatus) -> ConnectionState {
    if s.feeds.iter().any(|f| f.state == "sign_in_required") || s.state == "sign_in_required" {
        return ConnectionState::SignInRequired;
    }
    if s.feeds.iter().any(|f| f.state == "throttled") {
        return ConnectionState::Limited;
    }
    match s.state.as_str() {
        "disabled" if !s.mounted => ConnectionState::Unmounted,
        "starting" => ConnectionState::Mounting,
        "ready"
            if s.mounted && !s.feeds.is_empty() && s.feeds.iter().all(|f| f.state == "ready") =>
        {
            ConnectionState::Connected
        }
        "ready" | "indexing" | "updating_or_offline" if s.mounted => ConnectionState::Updating,
        _ => ConnectionState::Unavailable,
    }
}

/// Only local settings and the bounded local status socket are read. The desktop
/// does not create a provider, refresh tokens or list cloud files to poll status.
pub async fn snapshot(state: PathBuf, socket: PathBuf) -> Snapshot {
    let settings = tokio::task::spawn_blocking(move || Settings::load(&state));
    let status = cirrove_service::status(&socket);
    let (settings, status) = tokio::join!(settings, status);
    // One `recent` per mounted account. An older daemon answers with a
    // refusal, which is an empty list here, not an error: activity is an
    // extra, and a window must not go blank for want of it.
    let mut activity = Vec::new();
    if let Ok(status) = &status {
        for account in status.accounts.iter().filter(|a| a.mounted) {
            let request = cirrove_service::RecentRequest {
                label: account.label.clone(),
                limit: 8,
            };
            if let Ok(reply) = cirrove_service::recent(&socket, &request).await
                && reply.refusal.is_none()
            {
                activity.push((account.label.clone(), reply));
            }
        }
    }
    Snapshot {
        settings: match settings {
            Ok(result) => result.map_err(SettingsFailure::from_error),
            Err(_) => Err(SettingsFailure::WorkerUnavailable),
        },
        status: status.map_err(ServiceFailure::from_error),
        activity,
    }
}
