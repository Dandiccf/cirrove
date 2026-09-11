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

pub mod menu;

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
    /// Something the tray itself tried and could not do.
    ///
    /// The rest of this struct holds only what the daemon said, deliberately --
    /// an icon inferred from anything else reports something nobody knows. This
    /// is the one exception and it is not an inference: the tray asked for a
    /// mount change, the call came back an error, and the tray is the only thing
    /// that saw it. The window can put such a failure in front of the user;
    /// until this existed the tray could only write it to a log nobody reads,
    /// and the click looked like it had worked.
    ///
    /// Cleared by the next change the daemon reports, because that is the
    /// evidence the situation moved on -- a notice that outlived its cause would
    /// keep the icon shouting after the user fixed it.
    notice: Option<String>,
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
        // The daemon reporting a change is the evidence that the situation moved
        // on, so any notice about a failed action stops being current. Including
        // re-priming after a reconnection: the state was just re-read from the
        // daemon, and a local complaint that survived that would be the tray
        // keeping an icon lit for something it can no longer vouch for.
        if matches!(
            event,
            Event::Account { .. } | Event::Mount { .. } | Event::AccountRemoved { .. }
        ) {
            self.notice = None;
        }
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
            // A newer daemon said something this build has never heard of.
            // `Subscription::next` already drops these, so reaching here means a
            // caller built one directly; either way the answer is the same, and
            // it must never be to stop rendering what we do understand.
            Event::Unknown => {}
        }
    }

    pub fn set_disconnected(&mut self, disconnected: bool) {
        self.disconnected = disconnected;
    }

    /// Record something the tray tried and could not do. See [`TrayState`].
    pub fn set_notice(&mut self, notice: impl Into<String>) {
        self.notice = Some(notice.into());
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Worst-first, because a tray has one icon and the thing a user can act on
    /// has to win. An account needing sign-in is invisible if a second healthy
    /// account is allowed to paint the icon green.
    pub fn health(&self) -> Health {
        if self.disconnected {
            return Health::NeedsAttention;
        }
        // A failed action outranks a healthy account: the accounts really are
        // fine, and that is exactly why nothing else would ever show this.
        if self.notice.is_some() {
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
        // First, because it is the line the user has to read; the account states
        // below it are the ones they already expected.
        let mut lines = Vec::new();
        if let Some(notice) = &self.notice {
            lines.push(notice.clone());
        }
        if self.accounts.is_empty() {
            lines.push("No Cirrove accounts".into());
        } else {
            lines.extend(
                self.accounts
                    .values()
                    .map(|a| format!("{}: {}", a.label, summary(a))),
            );
        }
        lines.join("\n")
    }

    /// Whether the daemon is currently unreachable.
    pub fn is_disconnected(&self) -> bool {
        self.disconnected
    }

    pub fn account_count(&self) -> usize {
        self.accounts.len()
    }

    /// One row per account: label, a short state a person can read, and whether
    /// it has a folder to open. Ordered by the map, which is by account id, so
    /// the order a menu renders does not change under the user between redraws.
    pub fn rows(&self) -> Vec<(String, String, bool)> {
        self.accounts
            .values()
            .map(|a| (a.label.clone(), summary(a), a.mounted))
            .collect()
    }

    /// The mount path of the nth row, matching `rows`. Used to turn a click on a
    /// menu entry back into an account without the menu storing one.
    pub fn mount_of(&self, index: usize) -> Option<std::path::PathBuf> {
        let account = self.accounts.values().nth(index)?;
        account.mounted.then(|| account.mount_path.clone())
    }

    /// The account id of the nth row, matching `rows`. Needed to act on an
    /// account rather than only to open it, and read back from state at click
    /// time so a menu can never act on one the daemon has since dropped.
    pub fn id_of(&self, index: usize) -> Option<String> {
        self.accounts.keys().nth(index).cloned()
    }

    /// Whether the nth row is mounted, matching `rows`.
    pub fn mounted_at(&self, index: usize) -> Option<bool> {
        Some(self.accounts.values().nth(index)?.mounted)
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

/// One account's state in words, shared by the tooltip and the menu so that the
/// same account cannot be described two ways on one screen.
fn summary(account: &AccountRow) -> String {
    match (account.state.as_str(), account.mounted) {
        ("ready", true) => "ready".into(),
        ("ready", false) => "not mounted".into(),
        ("sign_in_required", _) => "sign-in required".into(),
        ("starting", _) => "starting".into(),
        ("updating_or_offline", _) => "updating or offline".into(),
        // A mount error is already a sanitized local message.
        (other, _) => other.into(),
    }
}

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

    /// Where a shell reads the menu. `ItemIsMenu` stays false so a left click
    /// still activates -- opening the folder -- and the menu is the right-click
    /// gesture, which is what a user expects of a status icon.
    #[zbus(property)]
    fn menu(&self) -> zbus::zvariant::OwnedObjectPath {
        zbus::zvariant::ObjectPath::from_static_str_unchecked(menu::MENU_PATH).into()
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
            None => open_settings(),
        }
    }

    fn secondary_activate(&self, _x: i32, _y: i32) {
        open_settings();
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

/// Open the settings window, which is a separate binary.
///
/// Spawned off `PATH` rather than an absolute path, for the reason the autostart
/// entry uses a bare command: this must work wherever a distribution installs
/// it. A failure is reported and not fatal -- a tray whose settings entry does
/// nothing is worse than one that says why.
fn open_settings() {
    if let Err(error) = std::process::Command::new("cirrove-desktop").spawn() {
        eprintln!("cirrove-tray: could not open the settings window: {error}");
    }
}

/// Publish the item on the session bus and hand back the connection.
///
/// Shared by `run` and by the integration test that checks a host is offered
/// every property it may read. Exposed rather than duplicated because a test
/// that builds its own item would assert a second implementation, and the one
/// that shipped invisible was the real one.
pub async fn publish(
    state: Arc<Mutex<TrayState>>,
    revision: Arc<std::sync::atomic::AtomicU32>,
    state_dir: PathBuf,
    redraw: Arc<tokio::sync::Notify>,
) -> Result<zbus::Connection> {
    let item = StatusNotifierItem {
        state: state.clone(),
    };
    zbus::connection::Builder::session()
        .context("no session bus; a tray needs one")?
        .serve_at("/StatusNotifierItem", item)?
        // The item only names a path; the menu is a second interface, and a
        // shell that finds nothing there shows an empty menu.
        .serve_at(
            menu::MENU_PATH,
            menu::DbusMenu::new(state, revision, state_dir, redraw),
        )?
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
    publish(
        Arc::new(Mutex::new(state)),
        Arc::new(std::sync::atomic::AtomicU32::new(1)),
        // Never written: the bus tests read the layout and do not click. A path
        // that does not exist is the safer fixture, because a test that did
        // click would fail rather than touch a real account.
        PathBuf::from("/nonexistent/cirrove-tray-test"),
        Arc::new(tokio::sync::Notify::new()),
    )
    .await
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
/// What, if anything, to tell the user about the tray host right now.
#[derive(Debug, PartialEq, Eq)]
enum HostNotice {
    /// No host is running. Say so once, with what to do about it.
    Missing,
    /// One appeared after an absence was reported, so the earlier message is
    /// now stale and the user should know it resolved itself.
    Appeared,
    Nothing,
}

/// A missing tray host is not a failure -- the item is published and waits --
/// but silence is the wrong way to tolerate it. On stock GNOME, which needs an
/// extension, the tray simply never appears and nothing anywhere says why.
///
/// Said once rather than on every change: a flaky panel that comes and goes
/// must not fill a journal, and the second message is only worth sending if the
/// first one was.
fn host_notice(reported_missing: &mut bool, present: bool) -> HostNotice {
    match (present, *reported_missing) {
        (false, false) => {
            *reported_missing = true;
            HostNotice::Missing
        }
        (true, true) => {
            *reported_missing = false;
            HostNotice::Appeared
        }
        _ => HostNotice::Nothing,
    }
}

fn report(notice: HostNotice) {
    match notice {
        HostNotice::Missing => eprintln!(
            "cirrove-tray: no tray host is running ({WATCHER}); the icon appears as soon as one \
             does. GNOME needs an AppIndicator extension; most other panels provide a host."
        ),
        HostNotice::Appeared => eprintln!("cirrove-tray: a tray host appeared; the icon is live"),
        HostNotice::Nothing => {}
    }
}

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
    let mut reported_missing = false;
    let present = dbus.name_has_owner(name).await.unwrap_or(false);
    if present {
        announce(&connection).await;
    }
    report(host_notice(&mut reported_missing, present));
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
        let present = args.new_owner.is_some();
        if present {
            announce(&connection).await;
        }
        report(host_notice(&mut reported_missing, present));
    }
}

async fn announce(connection: &zbus::Connection) {
    if let Err(error) = register_with_watcher(connection).await {
        eprintln!("cirrove-tray: {error}");
    }
}

/// Publish the item and keep it in step with the daemon until cancelled.
pub async fn run(socket: PathBuf, state_dir: PathBuf) -> Result<()> {
    let state = Arc::new(Mutex::new(TrayState::default()));
    let revision = Arc::new(std::sync::atomic::AtomicU32::new(1));
    let redraw = Arc::new(tokio::sync::Notify::new());
    let connection = publish(state.clone(), revision.clone(), state_dir, redraw.clone()).await?;

    // The menu can change what the icon should say -- a mount it could not
    // change -- and cannot publish that itself. This is the half that can.
    {
        let connection = connection.clone();
        let revision = revision.clone();
        tokio::spawn(async move {
            loop {
                redraw.notified().await;
                notify(&connection, &revision).await;
            }
        });
    }

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
                notify(&connection, &revision).await;
                while let Some(event) = subscription.next().await? {
                    apply(&state, event);
                    notify(&connection, &revision).await;
                }
            }
            Err(error) => eprintln!("cirrove-tray: {error}"),
        }
        // The daemon stopped or was never there. Show that rather than freezing
        // on the last good icon, then retry: a user restarting the service
        // should not also have to restart the tray.
        set_disconnected(&state, true);
        notify(&connection, &revision).await;
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
async fn notify(connection: &zbus::Connection, revision: &std::sync::atomic::AtomicU32) {
    // The menu renders the same state as the icon, so anything that moves one
    // moves the other. Emitting only the item's signals would leave a shell
    // showing a correct icon over a stale account list.
    menu::announce_change(connection, revision).await;
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

    /// A failure the tray itself hit has to reach the icon, not only a log.
    ///
    /// Everything else here is the daemon's answer, and this is the one thing
    /// that is not: the tray asked for a mount change, the call failed, and the
    /// tray is the only thing that saw it. Before this the click simply looked
    /// like it had worked.
    #[test]
    fn something_the_tray_could_not_do_reaches_the_icon_and_the_tooltip() {
        let mut state = TrayState::default();
        state.apply(account("one", "ready", true));
        assert_eq!(state.health(), Health::Ready, "healthy to begin with");

        state.set_notice("Could not unmount. Try again.");
        assert_eq!(
            state.health(),
            Health::NeedsAttention,
            "a failed action outranks healthy accounts; nothing else would show it"
        );
        assert_eq!(
            state.sni_status(),
            "NeedsAttention",
            "a shell must surface it"
        );
        let tooltip = state.tooltip();
        assert!(
            tooltip.starts_with("Could not unmount."),
            "the line the user has to read must come first: {tooltip:?}"
        );
        assert!(
            tooltip.contains("label-one"),
            "the accounts must still be there: {tooltip:?}"
        );
    }

    /// And it has to go away again. A notice that outlived its cause would keep
    /// the icon shouting after the user fixed the thing elsewhere -- which is
    /// worse than not having shown it, because the next real one is ignored.
    #[test]
    fn a_notice_is_cleared_by_the_daemon_reporting_a_change() {
        let mut state = TrayState::default();
        state.apply(account("one", "ready", true));
        state.set_notice("Could not unmount. Try again.");
        assert_eq!(state.health(), Health::NeedsAttention);

        // Ready is not evidence of anything moving on: it only marks the end of
        // priming, and clearing on it would drop a notice raised a moment ago.
        state.apply(Event::Ready);
        assert_eq!(
            state.health(),
            Health::NeedsAttention,
            "the priming marker is not news about the account"
        );

        state.apply(account("one", "ready", false));
        assert_eq!(
            state.health(),
            Health::Working,
            "the daemon reported a change, so the complaint is no longer current"
        );
        assert!(state.notice().is_none());
    }

    #[test]
    fn a_missing_tray_host_is_reported_once_and_its_return_only_after_that() {
        let mut reported = false;
        // Startup with no host: say so.
        assert_eq!(host_notice(&mut reported, false), HostNotice::Missing);
        // A panel that keeps failing to start must not fill the journal.
        assert_eq!(host_notice(&mut reported, false), HostNotice::Nothing);
        assert_eq!(host_notice(&mut reported, false), HostNotice::Nothing);
        // It arrived, so the earlier message is stale and worth correcting.
        assert_eq!(host_notice(&mut reported, true), HostNotice::Appeared);
        // A panel restarting is the ordinary case and needs no commentary: the
        // item re-registers and the user sees nothing missing.
        assert_eq!(host_notice(&mut reported, true), HostNotice::Nothing);
        assert_eq!(host_notice(&mut reported, false), HostNotice::Missing);
        assert_eq!(host_notice(&mut reported, true), HostNotice::Appeared);
    }

    #[test]
    fn a_host_present_from_the_start_is_not_announced() {
        // Nothing was wrong, so there is nothing to say. Announcing a working
        // tray at every start is the noise that makes a real warning invisible.
        let mut reported = false;
        assert_eq!(host_notice(&mut reported, true), HostNotice::Nothing);
        assert!(!reported);
    }

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
