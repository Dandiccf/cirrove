//! A status icon that follows the daemon, as StatusNotifierItem over D-Bus.
//!
//! Milestone 5 asks for "tray status and actions using the daemon as the source
//! of truth", and specifies StatusNotifierItem rather than any toolkit's status
//! icon, because GTK4 removed the latter and Wayland shells implement the
//! former. See [ADR 0007](../../../docs/adr/0007-desktop-event-channel.md) for
//! the channel this consumes.
//!
//! The daemon is the source of truth in the strong sense: this holds no state
//! it did not receive. It renders what the subscription said and nothing it
//! inferred, so a tray that has lost its connection shows the last thing the
//! daemon actually said rather than a guess that ages silently.

use anyhow::{Context, Result};
use cirrove_service::events::Event;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// What the tray knows, which is only what the daemon told it.
#[derive(Clone, Debug, Default)]
pub struct TrayState {
    accounts: BTreeMap<String, AccountRow>,
    /// Set when the subscription dropped. Rendered, not hidden: a tray that
    /// silently keeps showing its last icon after losing the daemon is telling
    /// the user something it does not know.
    disconnected: bool,
}

#[derive(Clone, Debug, Default)]
struct AccountRow {
    label: String,
    state: String,
    mounted: bool,
    mount_path: PathBuf,
}

/// What the icon is trying to say, in priority order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    /// A user must do something, and nothing else will fix it.
    NeedsAttention,
    /// Reachable and mounted.
    Ready,
    /// Running but not settled -- starting, indexing, offline, remounting.
    Working,
    /// No accounts, or no daemon.
    Idle,
}

impl TrayState {
    pub fn apply(&mut self, event: Event) {
        match event {
            Event::Account {
                account_id,
                label,
                state,
                mounted,
                ..
            } => {
                let row = self.accounts.entry(account_id).or_default();
                row.label = label;
                row.state = state;
                row.mounted = mounted;
            }
            Event::Mount {
                account_id,
                label,
                mount_path,
                mounted,
            } => {
                let row = self.accounts.entry(account_id).or_default();
                row.label = label;
                row.mount_path = mount_path;
                row.mounted = mounted;
            }
            Event::AccountRemoved { account_id, .. } => {
                self.accounts.remove(&account_id);
            }
            // Nothing to apply: the re-priming events that follow carry the
            // state. Dropping it here is correct precisely because the channel
            // guarantees that.
            Event::Lagged { .. } => {}
            // Priming finished. Nothing to render that the priming events did
            // not already say; it exists so a client can tell an established
            // subscription from a silent one.
            Event::Ready => {}
        }
    }

    pub fn set_disconnected(&mut self, disconnected: bool) {
        self.disconnected = disconnected;
    }

    /// Worst-first, because a tray has one icon and the thing a user can act on
    /// has to win. An account needing sign-in is invisible if a second healthy
    /// account is allowed to paint the icon green.
    pub fn health(&self) -> Health {
        if self.disconnected {
            return Health::NeedsAttention;
        }
        if self.accounts.is_empty() {
            return Health::Idle;
        }
        if self
            .accounts
            .values()
            .any(|a| a.state == "sign_in_required")
        {
            return Health::NeedsAttention;
        }
        if self
            .accounts
            .values()
            .all(|a| a.state == "ready" && a.mounted)
        {
            return Health::Ready;
        }
        Health::Working
    }

    /// A themed icon name.
    ///
    /// Generic names on purpose: Cirrove has no installed icon yet -- that is
    /// its own milestone 5 box -- and inventing a name now would show a blank
    /// square on every theme until that box is done. These exist in Adwaita and
    /// in every icon theme that follows the freedesktop naming spec.
    pub fn icon_name(&self) -> &'static str {
        match self.health() {
            Health::NeedsAttention => "dialog-warning-symbolic",
            Health::Ready => "folder-remote-symbolic",
            Health::Working => "emblem-synchronizing-symbolic",
            Health::Idle => "folder-remote-symbolic",
        }
    }

    /// The StatusNotifierItem `Status` property, which decides whether a shell
    /// may hide the icon. `NeedsAttention` is what makes a shell surface it.
    pub fn sni_status(&self) -> &'static str {
        match self.health() {
            Health::NeedsAttention => "NeedsAttention",
            Health::Idle => "Passive",
            _ => "Active",
        }
    }

    pub fn title(&self) -> String {
        "Cirrove".into()
    }

    /// One line per account. No paths in the summary line, because a tooltip is
    /// visible to anyone looking at the screen and a mount path carries the
    /// user's folder names.
    pub fn tooltip(&self) -> String {
        if self.disconnected {
            return "Cirrove service is not reachable".into();
        }
        if self.accounts.is_empty() {
            return "No Cirrove accounts".into();
        }
        self.accounts
            .values()
            .map(|a| {
                let state = match (a.state.as_str(), a.mounted) {
                    ("ready", true) => "ready".to_string(),
                    ("ready", false) => "not mounted".to_string(),
                    ("sign_in_required", _) => "sign-in required".to_string(),
                    ("starting", _) => "starting".to_string(),
                    ("updating_or_offline", _) => "updating or offline".to_string(),
                    // A mount error is already a sanitized local message.
                    (other, _) => other.to_string(),
                };
                format!("{}: {state}", a.label)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The folder a click should open, if exactly one is mounted.
    pub fn single_mount(&self) -> Option<PathBuf> {
        let mut mounted = self.accounts.values().filter(|a| a.mounted);
        let first = mounted.next()?;
        mounted.next().is_none().then(|| first.mount_path.clone())
    }
}

/// The StatusNotifierItem `ToolTip` signature, `(sa(iiay)ss)`: themed icon
/// name, pixmaps, title, description. Named because the tuple is the wire
/// shape and spelling it inline twice is how the two copies drift apart.
type ToolTip = (String, Vec<(i32, i32, Vec<u8>)>, String, String);

/// The D-Bus object a shell reads.
pub struct StatusNotifierItem {
    state: Arc<Mutex<TrayState>>,
}

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl StatusNotifierItem {
    #[zbus(property)]
    fn category(&self) -> &str {
        "SystemServices"
    }

    #[zbus(property)]
    fn id(&self) -> &str {
        "cirrove"
    }

    #[zbus(property)]
    fn title(&self) -> String {
        self.read().title()
    }

    #[zbus(property)]
    fn status(&self) -> String {
        self.read().sni_status().to_string()
    }

    #[zbus(property)]
    fn icon_name(&self) -> String {
        self.read().icon_name().to_string()
    }

    /// What a host draws while `Status` is `NeedsAttention`.
    ///
    /// The spec switches icons with the status, and leaving this out is why the
    /// first version of this tray drew nothing at all: it announced
    /// NeedsAttention and then offered the host only an `IconName`, which the
    /// host does not use in that state. Passive hid it and NeedsAttention had
    /// nothing to show, so both states an account-less daemon can produce were
    /// invisible.
    #[zbus(property)]
    fn attention_icon_name(&self) -> String {
        self.read().icon_name().to_string()
    }

    /// Empty: the icons are themed names. Present because hosts read the whole
    /// property set and a missing one is not always treated as "no pixmap".
    #[zbus(property)]
    fn icon_pixmap(&self) -> Vec<(i32, i32, Vec<u8>)> {
        Vec::new()
    }

    #[zbus(property)]
    fn attention_icon_pixmap(&self) -> Vec<(i32, i32, Vec<u8>)> {
        Vec::new()
    }

    #[zbus(property)]
    fn overlay_icon_name(&self) -> String {
        String::new()
    }

    #[zbus(property)]
    fn overlay_icon_pixmap(&self) -> Vec<(i32, i32, Vec<u8>)> {
        Vec::new()
    }

    #[zbus(property)]
    fn attention_movie_name(&self) -> String {
        String::new()
    }

    /// No window to raise: this is a daemon's indicator, not an application.
    #[zbus(property)]
    fn window_id(&self) -> i32 {
        0
    }

    /// There is no dbusmenu yet. The path is answered rather than omitted
    /// because a host that reads the whole set and finds nothing here can drop
    /// the item; `ItemIsMenu` false is what tells it to activate instead.
    #[zbus(property)]
    fn menu(&self) -> zbus::zvariant::OwnedObjectPath {
        zbus::zvariant::ObjectPath::from_static_str_unchecked("/NoDbusmenu").into()
    }

    /// `(icon name, pixmaps, title, description)`. Pixmaps stay empty: the icon
    /// is themed, and shipping our own would want the installed icon set that
    /// milestone 5 lists as separate work.
    #[zbus(property)]
    fn tool_tip(&self) -> ToolTip {
        let state = self.read();
        (
            state.icon_name().to_string(),
            Vec::new(),
            "Cirrove".into(),
            state.tooltip(),
        )
    }

    /// False: there is no dbusmenu yet, so a click should activate rather than
    /// open a menu that does not exist. Saying true here makes a shell open an
    /// empty menu, which looks like a broken tray rather than a plain one.
    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        false
    }

    /// Left click. Opens the mount when exactly one is mounted; ambiguity is
    /// left alone rather than resolved by guessing which account was meant.
    fn activate(&self, _x: i32, _y: i32) {
        let path = self.read().single_mount();
        match path {
            Some(path) => open_path(&path),
            None => tracing_open_window(),
        }
    }

    fn secondary_activate(&self, _x: i32, _y: i32) {
        tracing_open_window();
    }

    fn scroll(&self, _delta: i32, _orientation: &str) {}
}

impl StatusNotifierItem {
    /// A poisoned lock must not take the tray down: the state behind it is a
    /// render cache the daemon refills on the next event.
    fn read(&self) -> TrayState {
        match self.state.lock() {
            Ok(state) => state.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

fn open_path(path: &std::path::Path) {
    if let Err(error) = std::process::Command::new("xdg-open").arg(path).spawn() {
        eprintln!("could not open the mount: {error}");
    }
}

fn tracing_open_window() {
    // The settings window is a separate binary and launching it is the desktop
    // milestone's business, not this file's. Saying so beats a silent no-op.
    eprintln!("cirrove-tray: no window action is wired up yet");
}

/// Publish the item on the session bus and hand back the connection.
///
/// Shared by `run` and by the integration test that checks a host is offered
/// every property it may read. Exposed rather than duplicated because a test
/// that builds its own item would assert a second implementation, and the one
/// that shipped invisible was the real one.
pub async fn publish(state: Arc<Mutex<TrayState>>) -> Result<zbus::Connection> {
    let item = StatusNotifierItem { state };
    zbus::connection::Builder::session()
        .context("no session bus; a tray needs one")?
        .serve_at("/StatusNotifierItem", item)?
        .build()
        .await
        .context("could not publish the tray item on the session bus")
}

/// Publish an item carrying a state a host would have to draw.
///
/// Tests only. The state is deliberately not empty: an account-less tray is
/// Passive, and Passive is the one status a host is allowed to ignore.
pub async fn publish_for_test() -> Result<zbus::Connection> {
    let mut state = TrayState::default();
    state.apply(Event::Account {
        account_id: "id".into(),
        label: "test".into(),
        state: "sign_in_required".into(),
        enabled: true,
        mounted: true,
    });
    publish(Arc::new(Mutex::new(state))).await
}

/// The well-known name this tray owns, and the lock that keeps one of it.
///
/// Autostart mechanisms overlap: an XDG autostart entry, a systemd user unit and
/// a window manager's own `exec-once` can all be in play on one machine, and a
/// package cannot know which. Rather than make the packaging clever -- which
/// docs/distribution.md forbids anyway, since package scripts may not assume a
/// desktop or reach a user's session bus -- the binary refuses to be the second
/// copy. The bus connection it needs regardless is the lock.
pub const TRAY_BUS_NAME: &str = "io.github.Dandiccf.Cirrove.Tray";

/// The interface a shell must provide before a tray item means anything.
const WATCHER: &str = "org.kde.StatusNotifierWatcher";

/// Take the name, or report that someone else has it.
pub async fn claim_single_instance(connection: &zbus::Connection) -> Result<bool> {
    use zbus::fdo::{RequestNameFlags, RequestNameReply};
    let reply = connection
        .request_name_with_flags(
            zbus::names::WellKnownName::try_from(TRAY_BUS_NAME).context("bad tray bus name")?,
            // DoNotQueue: a second copy should exit now, not wait for the first
            // to die and then silently become a tray nobody asked for.
            RequestNameFlags::DoNotQueue.into(),
        )
        .await;
    match reply {
        Ok(RequestNameReply::PrimaryOwner) => Ok(true),
        Ok(_) => Ok(false),
        Err(zbus::Error::NameTaken) => Ok(false),
        Err(error) => Err(error).context("could not claim the tray name"),
    }
}

/// Register with the shell's watcher every time one appears.
///
/// Not once at startup. A tray is started by an autostart mechanism whose
/// ordering against the panel is not guaranteed, so the watcher may not exist
/// yet; and panels get restarted and reloaded, taking every registration with
/// them. Both cases look the same on the bus -- the name gains an owner -- so
/// both are handled by watching for that rather than by assuming startup order.
async fn follow_watcher(connection: zbus::Connection) {
    use futures_util::stream::StreamExt;
    let dbus = match zbus::fdo::DBusProxy::new(&connection).await {
        Ok(dbus) => dbus,
        Err(error) => {
            eprintln!("cirrove-tray: cannot watch the session bus: {error}");
            return;
        }
    };
    let Ok(name) = zbus::names::BusName::try_from(WATCHER) else {
        return;
    };
    if dbus.name_has_owner(name).await.unwrap_or(false) {
        announce(&connection).await;
    }
    let Ok(mut changes) = dbus.receive_name_owner_changed().await else {
        eprintln!("cirrove-tray: cannot follow the tray host; it will not reappear");
        return;
    };
    while let Some(change) = changes.next().await {
        let Ok(args) = change.args() else { continue };
        if args.name.as_str() != WATCHER {
            continue;
        }
        // A panel started or restarted: re-register, because the old
        // registration died with it. A panel going away needs nothing done --
        // the item stays published and is picked up when one returns.
        if args.new_owner.is_some() {
            announce(&connection).await;
        }
    }
}

async fn announce(connection: &zbus::Connection) {
    if let Err(error) = register_with_watcher(connection).await {
        eprintln!("cirrove-tray: {error}");
    }
}

/// Publish the item and keep it in step with the daemon until cancelled.
pub async fn run(socket: PathBuf) -> Result<()> {
    let state = Arc::new(Mutex::new(TrayState::default()));
    let connection = publish(state.clone()).await?;

    if !claim_single_instance(&connection).await? {
        // Not an error: on a machine where two autostart mechanisms both fire,
        // this is the ordinary outcome and the other copy is doing the job.
        eprintln!("cirrove-tray: another instance already owns {TRAY_BUS_NAME}; exiting");
        return Ok(());
    }

    // No tray host is not a failure to start. A tray that exits because a panel
    // has not come up yet is a tray that never runs on half the desktops it is
    // installed on, so it waits instead and registers when one appears.
    tokio::spawn(follow_watcher(connection.clone()));

    loop {
        match cirrove_service::Subscription::open(&socket).await {
            Ok(mut subscription) => {
                set_disconnected(&state, false);
                notify(&connection).await;
                while let Some(event) = subscription.next().await? {
                    apply(&state, event);
                    notify(&connection).await;
                }
            }
            Err(error) => eprintln!("cirrove-tray: {error}"),
        }
        // The daemon stopped or was never there. Show that rather than freezing
        // on the last good icon, then retry: a user restarting the service
        // should not also have to restart the tray.
        set_disconnected(&state, true);
        notify(&connection).await;
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

async fn register_with_watcher(connection: &zbus::Connection) -> Result<()> {
    let watcher = zbus::Proxy::new(connection, WATCHER, "/StatusNotifierWatcher", WATCHER)
        .await
        .context("no StatusNotifierWatcher on the session bus")?;
    let name = connection
        .unique_name()
        .context("the session bus gave this connection no name")?
        .to_string();
    watcher
        .call_method("RegisterStatusNotifierItem", &(name.as_str()))
        .await
        .context("the shell refused to register the tray item")?;
    Ok(())
}

fn apply(state: &Arc<Mutex<TrayState>>, event: Event) {
    match state.lock() {
        Ok(mut state) => state.apply(event),
        Err(poisoned) => poisoned.into_inner().apply(event),
    }
}

fn set_disconnected(state: &Arc<Mutex<TrayState>>, disconnected: bool) {
    match state.lock() {
        Ok(mut state) => state.set_disconnected(disconnected),
        Err(poisoned) => poisoned.into_inner().set_disconnected(disconnected),
    }
}

/// Tell the shell to re-read. Properties are pull-based in this interface, so
/// without these signals a shell keeps the first values forever.
async fn notify(connection: &zbus::Connection) {
    for signal in ["NewIcon", "NewAttentionIcon", "NewStatus", "NewToolTip"] {
        let _ = connection
            .emit_signal(
                None::<&str>,
                "/StatusNotifierItem",
                "org.kde.StatusNotifierItem",
                signal,
                &(),
            )
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(id: &str, state: &str, mounted: bool) -> Event {
        Event::Account {
            account_id: id.into(),
            label: format!("label-{id}"),
            state: state.into(),
            enabled: true,
            mounted,
        }
    }

    fn mount(id: &str, path: &str, mounted: bool) -> Event {
        Event::Mount {
            account_id: id.into(),
            label: format!("label-{id}"),
            mount_path: PathBuf::from(path),
            mounted,
        }
    }

    #[test]
    fn no_accounts_is_passive_so_a_shell_may_hide_it() {
        let state = TrayState::default();
        assert_eq!(state.health(), Health::Idle);
        assert_eq!(state.sni_status(), "Passive");
    }

    #[test]
    fn one_account_needing_sign_in_beats_another_that_is_healthy() {
        // The reason health is worst-first. A user with two accounts, one of
        // which has lost consent, must see the one they can act on.
        let mut state = TrayState::default();
        state.apply(account("a", "ready", true));
        state.apply(account("b", "sign_in_required", true));
        assert_eq!(state.health(), Health::NeedsAttention);
        assert_eq!(state.sni_status(), "NeedsAttention");
        assert_eq!(state.icon_name(), "dialog-warning-symbolic");
    }

    #[test]
    fn a_ready_but_unmounted_account_is_working_rather_than_ready() {
        let mut state = TrayState::default();
        state.apply(account("a", "ready", false));
        assert_eq!(state.health(), Health::Working);
    }

    #[test]
    fn losing_the_daemon_is_shown_rather_than_frozen_on_the_last_icon() {
        let mut state = TrayState::default();
        state.apply(account("a", "ready", true));
        assert_eq!(state.health(), Health::Ready);
        state.set_disconnected(true);
        assert_eq!(state.health(), Health::NeedsAttention);
        assert_eq!(state.tooltip(), "Cirrove service is not reachable");
    }

    #[test]
    fn a_removed_account_leaves_no_row_behind() {
        let mut state = TrayState::default();
        state.apply(account("a", "ready", true));
        state.apply(Event::AccountRemoved {
            account_id: "a".into(),
            label: "label-a".into(),
        });
        assert_eq!(state.health(), Health::Idle);
    }

    #[test]
    fn lagging_changes_nothing_because_repriming_follows() {
        let mut state = TrayState::default();
        state.apply(account("a", "sign_in_required", true));
        let before = state.health();
        state.apply(Event::Lagged { dropped: 12 });
        assert_eq!(state.health(), before);
    }

    #[test]
    fn a_click_opens_a_mount_only_when_there_is_exactly_one() {
        let mut state = TrayState::default();
        state.apply(mount("a", "/mnt/a", true));
        assert_eq!(state.single_mount(), Some(PathBuf::from("/mnt/a")));
        state.apply(mount("b", "/mnt/b", true));
        assert_eq!(state.single_mount(), None, "two mounts is ambiguous");
    }

    #[test]
    fn the_tooltip_names_accounts_and_not_their_paths() {
        // A tooltip is visible to anyone looking at the screen; a mount path
        // carries the user's own folder names.
        let mut state = TrayState::default();
        state.apply(account("a", "sign_in_required", true));
        state.apply(mount("a", "/home/someone/Cloud/Private-Sales", true));
        let tooltip = state.tooltip();
        assert!(tooltip.contains("label-a"), "{tooltip}");
        assert!(tooltip.contains("sign-in required"), "{tooltip}");
        assert!(!tooltip.contains("Private-Sales"), "{tooltip}");
    }
}
