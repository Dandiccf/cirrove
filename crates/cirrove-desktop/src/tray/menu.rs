//! The tray's menu, as `com.canonical.dbusmenu` over D-Bus.
//!
//! StatusNotifierItem carries no menu of its own: it names an object path and a
//! shell reads the menu from there over a second interface. So a tray without
//! this has exactly one gesture, and everything milestone 5 asks a tray to offer
//! -- "tray status and actions using the daemon as the source of truth" -- has
//! nowhere to live.
//!
//! What may go in it is constrained, and the constraint is in the milestones
//! rather than in taste: *"Tray and file-manager extensions are optional
//! surfaces: no control or state may be reachable only through a tray or only
//! through one file manager."* A tray host is absent on stock GNOME and on any
//! bare window manager, so anything that lived only here would be gone for those
//! users. Every entry is therefore a shortcut to something the window also does,
//! and the menu holds no state of its own -- it is rendered from `TrayState`,
//! which holds only what the daemon said.
//!
//! Mounting and unmounting live here too, and the reason they took so long is
//! worth recording because it was wrong. The claim was that a second process
//! changing the account settings would race the window over one file, and that
//! they needed a daemon verb first. `accounts::update_enabled` takes both the
//! configuration lock and the per-account operation lock, and the window and the
//! CLI already go through it -- so there was never a race, and a third caller is
//! serialised by the same two locks. Nothing was blocking this but the belief
//! that something was.
//!
//! Acknowledgement comes free: the tray is already subscribed, so the daemon's
//! mount event redraws the row. Nothing here waits or guesses.

use super::TrayState;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use zbus::zvariant::{self, OwnedValue, Value};

/// One entry as dbusmenu sends it: id, properties, children. Named because the
/// tuple is the wire shape and it appears in a signature and two conversions.
pub type Layout = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);

/// Object path the item points a shell at.
pub const MENU_PATH: &str = "/MenuBar";

/// Ids are stable within a layout and mean nothing across one. The root is 0 by
/// specification; everything else is assigned here.
const ROOT: i32 = 0;
const SETTINGS: i32 = 1;
const QUIT: i32 = 2;
const SEPARATOR: i32 = 3;
/// Account rows start here so that adding a fixed entry above never renumbers
/// them mid-session, which would make a click land on the wrong row.
const FIRST_ACCOUNT: i32 = 100;
/// Ids per account row, so that adding an entry inside one never renumbers the
/// next account. `base` is the submenu, `base + OPEN` and `base + TOGGLE` its
/// entries.
const PER_ACCOUNT: i32 = 10;
const OPEN: i32 = 1;
const TOGGLE: i32 = 2;

pub struct DbusMenu {
    state: Arc<Mutex<TrayState>>,
    /// Where the account settings live. The menu changes a mount preference the
    /// same way the window and the CLI do -- `accounts::set_enabled_by_id`,
    /// which holds the configuration and per-account operation locks -- rather
    /// than through a control verb that does not exist.
    state_dir: std::path::PathBuf,
    /// Bumped whenever the layout changes. A shell caches a layout and refetches
    /// only when this moves, so a menu that never bumps it is a menu that shows
    /// yesterday's accounts.
    ///
    /// Shared rather than owned, because the thing that knows the layout changed
    /// is the subscription loop and it reaches the menu only through this.
    revision: Arc<AtomicU32>,
}

impl DbusMenu {
    pub fn new(
        state: Arc<Mutex<TrayState>>,
        revision: Arc<AtomicU32>,
        state_dir: std::path::PathBuf,
    ) -> Self {
        Self {
            state,
            revision,
            state_dir,
        }
    }

    /// A poisoned lock must not take the menu down; the state behind it is a
    /// render cache the daemon refills on its next event.
    fn read(&self) -> TrayState {
        match self.state.lock() {
            Ok(state) => state.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// What each row does when it is clicked, derived from the same state that
    /// drew it rather than remembered separately -- a menu that stored its own
    /// actions could act on an account the daemon has since dropped.
    fn action(&self, id: i32) -> Action {
        match id {
            SETTINGS => Action::OpenSettings,
            QUIT => Action::Quit,
            _ => {
                let offset = id - FIRST_ACCOUNT;
                if offset < 0 {
                    return Action::Nothing;
                }
                let index = usize::try_from(offset / PER_ACCOUNT).unwrap_or(usize::MAX);
                let state = self.read();
                match offset % PER_ACCOUNT {
                    OPEN => match state.mount_of(index) {
                        Some(path) => Action::OpenFolder(path),
                        None => Action::Nothing,
                    },
                    // Read the account and its current state at click time, not
                    // at draw time: a menu left open while the daemon changed
                    // its mind must not send the stale half of a toggle.
                    TOGGLE => match (state.id_of(index), state.mounted_at(index)) {
                        (Some(account), Some(mounted)) => Action::SetMounted {
                            account,
                            mounted: !mounted,
                        },
                        _ => Action::Nothing,
                    },
                    _ => Action::Nothing,
                }
            }
        }
    }
}

enum Action {
    OpenFolder(std::path::PathBuf),
    /// Change the account's desired mount state. The daemon observes the
    /// settings and answers on the event stream, which is what redraws the row;
    /// nothing here waits for it.
    SetMounted {
        account: String,
        mounted: bool,
    },
    OpenSettings,
    Quit,
    Nothing,
}

fn text(value: &str) -> OwnedValue {
    OwnedValue::from(zvariant::Str::from(value.to_owned()))
}

fn flag(value: bool) -> OwnedValue {
    OwnedValue::from(value)
}

/// One menu entry as `(ia{sv}av)`, the recursive type dbusmenu uses for layout.
fn entry(
    id: i32,
    properties: HashMap<String, OwnedValue>,
    children: Vec<Value<'static>>,
) -> Value<'static> {
    Value::from(zvariant::Structure::from((id, properties, children)))
}

fn leaf(id: i32, properties: HashMap<String, OwnedValue>) -> Value<'static> {
    entry(id, properties, Vec::new())
}

fn labelled(id: i32, label: &str, enabled: bool) -> Value<'static> {
    let mut properties = HashMap::new();
    properties.insert("label".to_string(), text(label));
    properties.insert("enabled".to_string(), flag(enabled));
    leaf(id, properties)
}

fn separator(id: i32) -> Value<'static> {
    let mut properties = HashMap::new();
    properties.insert("type".to_string(), text("separator"));
    leaf(id, properties)
}

#[zbus::interface(name = "com.canonical.dbusmenu")]
impl DbusMenu {
    #[zbus(property)]
    fn version(&self) -> u32 {
        3
    }

    #[zbus(property)]
    fn status(&self) -> &str {
        "normal"
    }

    #[zbus(property)]
    fn text_direction(&self) -> &str {
        "ltr"
    }

    #[zbus(property)]
    fn icon_theme_path(&self) -> Vec<String> {
        Vec::new()
    }

    /// The whole menu. `parent_id` and `depth` are honoured loosely on purpose:
    /// this layout is two levels deep, so a shell asking for a subtree of a leaf
    /// gets an empty one and a shell asking for everything gets everything.
    fn get_layout(
        &self,
        parent_id: i32,
        _depth: i32,
        _property_names: Vec<String>,
    ) -> (u32, Layout) {
        let revision = self.revision.load(Ordering::Relaxed);
        if parent_id != ROOT {
            return (revision, (parent_id, HashMap::new(), Vec::new()));
        }

        let state = self.read();
        let mut children: Vec<Value<'static>> = Vec::new();

        if state.is_disconnected() {
            // Shown and not hidden, and not clickable: a menu that silently drops
            // its rows when the daemon goes away looks like a broken tray rather
            // than a reachable service that is down.
            children.push(labelled(FIRST_ACCOUNT, "Service is not reachable", false));
        } else if state.account_count() == 0 {
            children.push(labelled(FIRST_ACCOUNT, "No accounts configured", false));
        } else {
            for (index, (label, summary, mounted)) in state.rows().into_iter().enumerate() {
                let base = FIRST_ACCOUNT + i32::try_from(index).unwrap_or(0) * PER_ACCOUNT;
                let mut properties = HashMap::new();
                properties.insert("label".to_string(), text(&format!("{label} — {summary}")));
                properties.insert("children-display".to_string(), text("submenu"));
                children.push(entry(
                    base,
                    properties,
                    vec![
                        // Only a mounted account has a folder to open. The entry
                        // stays visible when it is not, because its absence
                        // would read as the account having fewer capabilities
                        // rather than as it being unmounted right now.
                        labelled(base + OPEN, "Open folder", mounted),
                        labelled(
                            base + TOGGLE,
                            if mounted { "Unmount" } else { "Mount" },
                            true,
                        ),
                    ],
                ));
            }
        }

        children.push(separator(SEPARATOR));

        children.push(labelled(SETTINGS, "Cirrove settings…", true));
        children.push(labelled(QUIT, "Quit tray", true));

        let mut root = HashMap::new();
        root.insert("children-display".to_string(), text("submenu"));
        let children: Vec<OwnedValue> = children
            .into_iter()
            .filter_map(|child| OwnedValue::try_from(child).ok())
            .collect();
        (revision, (ROOT, root, children))
    }

    /// Properties for a set of ids. Rendered from the same layout rather than a
    /// second description of it, so the two cannot disagree.
    fn get_group_properties(
        &self,
        ids: Vec<i32>,
        _property_names: Vec<String>,
    ) -> Vec<(i32, HashMap<String, OwnedValue>)> {
        let (_, (_, _, children)) = self.get_layout(ROOT, -1, Vec::new());
        children
            .into_iter()
            .filter_map(|child| {
                let structure = zvariant::Structure::try_from(child).ok()?;
                let fields = structure.fields();
                let id = i32::try_from(fields.first()?.try_clone().ok()?).ok()?;
                if !ids.is_empty() && !ids.contains(&id) {
                    return None;
                }
                let properties =
                    HashMap::<String, OwnedValue>::try_from(fields.get(1)?.try_clone().ok()?)
                        .ok()?;
                Some((id, properties))
            })
            .collect()
    }

    fn get_property(&self, id: i32, name: String) -> OwnedValue {
        self.get_group_properties(vec![id], Vec::new())
            .into_iter()
            .next()
            .and_then(|(_, mut properties)| properties.remove(&name))
            .unwrap_or_else(|| text(""))
    }

    /// A click. Anything other than `clicked` is ignored rather than guessed at.
    fn event(&self, id: i32, event_id: String, _data: Value<'_>, _timestamp: u32) {
        if event_id != "clicked" {
            return;
        }
        match self.action(id) {
            Action::OpenFolder(path) => super::open_path(&path),
            Action::OpenSettings => super::open_settings(),
            // The tray is the only thing this process does, so quitting it is
            // exiting. The daemon and the mount are untouched, which is the
            // distinction a user has to be able to rely on.
            Action::Quit => std::process::exit(0),
            Action::SetMounted { account, mounted } => {
                // Blocking filesystem work under two locks; it must not run on
                // the bus task, which a shell is waiting on for this reply.
                let state_dir = self.state_dir.clone();
                tokio::task::spawn_blocking(move || {
                    if let Err(error) =
                        cirrove_service::accounts::set_enabled_by_id(&state_dir, &account, mounted)
                    {
                        // The window can put this in front of the user and a
                        // tray cannot; the honest thing is to say so where an
                        // operator will find it rather than to fail silently.
                        eprintln!("cirrove-tray: could not change the mount preference: {error:#}");
                    }
                });
            }
            Action::Nothing => {}
        }
    }

    fn event_group(&self, events: Vec<(i32, String, Value<'_>, u32)>) -> Vec<i32> {
        for (id, event_id, data, timestamp) in events {
            self.event(id, event_id, data, timestamp);
        }
        Vec::new()
    }

    /// No lazily built submenus, so nothing needs updating before a menu opens.
    fn about_to_show(&self, _id: i32) -> bool {
        false
    }

    fn about_to_show_group(&self, _ids: Vec<i32>) -> (Vec<i32>, Vec<i32>) {
        (Vec::new(), Vec::new())
    }
}

/// Tell a shell the layout changed, so it refetches instead of keeping the one it
/// first read. Without this an account that appeared, went away or lost consent
/// stays invisible in the menu until the tray is restarted.
pub async fn announce_change(connection: &zbus::Connection, revision: &AtomicU32) {
    let next = revision.fetch_add(1, Ordering::Relaxed) + 1;
    let _ = connection
        .emit_signal(
            None::<&str>,
            MENU_PATH,
            "com.canonical.dbusmenu",
            "LayoutUpdated",
            &(next, ROOT),
        )
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use cirrove_service::events::Event;

    fn menu(rows: &[(&str, bool)]) -> DbusMenu {
        let mut state = TrayState::default();
        for (id, mounted) in rows {
            state.apply(Event::Account {
                account_id: (*id).into(),
                label: format!("label-{id}"),
                state: "ready".into(),
                enabled: true,
                mounted: *mounted,
            });
        }
        DbusMenu::new(
            Arc::new(Mutex::new(state)),
            Arc::new(AtomicU32::new(1)),
            std::path::PathBuf::from("/nonexistent"),
        )
    }

    /// A click has to reach the account it was drawn for. Ids are the only thing
    /// carrying that, so the arithmetic is the thing worth pinning down: with a
    /// flat `FIRST_ACCOUNT + index`, the second account's row and the first
    /// account's second entry are the same number.
    #[test]
    fn a_click_reaches_the_account_it_was_drawn_for() {
        let menu = menu(&[("first", true), ("second", false)]);
        let second = FIRST_ACCOUNT + PER_ACCOUNT;
        assert!(matches!(
            menu.action(second + TOGGLE),
            Action::SetMounted { ref account, mounted: true } if account == "second"
        ));
        assert!(matches!(
            menu.action(FIRST_ACCOUNT + TOGGLE),
            Action::SetMounted { ref account, mounted: false } if account == "first"
        ));
    }

    /// The toggle sends the opposite of what the daemon currently says, read at
    /// click time. A menu that remembered what it drew would send the stale half
    /// of a toggle after the daemon changed its mind -- unmounting something the
    /// user had already unmounted elsewhere, which reads as the tray undoing it.
    #[test]
    fn the_toggle_is_decided_from_current_state_and_not_from_what_was_drawn() {
        let menu = menu(&[("only", true)]);
        assert!(matches!(
            menu.action(FIRST_ACCOUNT + TOGGLE),
            Action::SetMounted { mounted: false, .. }
        ));
        menu.state
            .lock()
            .expect("tray state lock")
            .apply(Event::Mount {
                account_id: "only".into(),
                label: "label-only".into(),
                mount_path: std::path::PathBuf::from("/tmp/x"),
                mounted: false,
            });
        assert!(matches!(
            menu.action(FIRST_ACCOUNT + TOGGLE),
            Action::SetMounted { mounted: true, .. }
        ));
    }

    /// An id no account answers to must do nothing, not act on whatever is
    /// nearest. A menu a shell cached across an account being removed sends
    /// exactly these.
    #[test]
    fn an_id_no_account_answers_to_does_nothing() {
        let menu = menu(&[("only", true)]);
        for id in [
            FIRST_ACCOUNT - 1,
            FIRST_ACCOUNT + PER_ACCOUNT + TOGGLE,
            FIRST_ACCOUNT + PER_ACCOUNT * 9,
            FIRST_ACCOUNT + 7,
        ] {
            assert!(
                matches!(menu.action(id), Action::Nothing),
                "id {id} acted on something"
            );
        }
    }
}
