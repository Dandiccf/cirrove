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
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Overview {
    pub accounts: Vec<AccountCard>,
    pub service_reachable: bool,
    pub settings_available: bool,
    pub settings_error: Option<SettingsFailure>,
    pub service_error: Option<ServiceFailure>,
}
impl Overview {
    pub fn from_snapshot(snapshot: Snapshot) -> Self {
        let reachable = snapshot.status.is_ok();
        let compatible = snapshot
            .status
            .as_ref()
            .is_ok_and(|s| s.protocol_version == cirrove_service::STATUS_PROTOCOL_VERSION);
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
                    service_reachable: reachable,
                    settings_available: false,
                    settings_error: Some(error),
                    service_error,
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
                }
            })
            .collect();
        Self {
            accounts,
            service_reachable: reachable,
            settings_available: true,
            settings_error: None,
            service_error,
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
    Snapshot {
        settings: match settings {
            Ok(result) => result.map_err(SettingsFailure::from_error),
            Err(_) => Err(SettingsFailure::WorkerUnavailable),
        },
        status: status.map_err(ServiceFailure::from_error),
    }
}
