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
//! Mount and unmount are deliberately absent. The window changes them by writing
//! the account settings, and a second process doing the same is a race over one
//! file rather than a feature. They belong here once the daemon has a verb for
//! them, which is the same argument that put pinning behind the control socket.

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

pub struct DbusMenu {
    state: Arc<Mutex<TrayState>>,
    /// Bumped whenever the layout changes. A shell caches a layout and refetches
    /// only when this moves, so a menu that never bumps it is a menu that shows
    /// yesterday's accounts.
    ///
    /// Shared rather than owned, because the thing that knows the layout changed
    /// is the subscription loop and it reaches the menu only through this.
    revision: Arc<AtomicU32>,
}

impl DbusMenu {
    pub fn new(state: Arc<Mutex<TrayState>>, revision: Arc<AtomicU32>) -> Self {
        Self { state, revision }
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
                let index = usize::try_from(id - FIRST_ACCOUNT).unwrap_or(usize::MAX);
                match self.read().mount_of(index) {
                    Some(path) => Action::OpenFolder(path),
                    None => Action::Nothing,
                }
            }
        }
    }
}

enum Action {
    OpenFolder(std::path::PathBuf),
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
fn entry(id: i32, properties: HashMap<String, OwnedValue>) -> Value<'static> {
    // Leaves only: this menu is two levels deep, so the recursive child list is
    // always empty and the recursion in the type costs nothing here.
    let children: Vec<Value<'static>> = Vec::new();
    Value::from(zvariant::Structure::from((id, properties, children)))
}

fn separator(id: i32) -> Value<'static> {
    let mut properties = HashMap::new();
    properties.insert("type".to_string(), text("separator"));
    entry(id, properties)
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
            let mut properties = HashMap::new();
            properties.insert("label".to_string(), text("Service is not reachable"));
            // Shown and not hidden, and not clickable: a menu that silently drops
            // its rows when the daemon goes away looks like a broken tray rather
            // than a reachable service that is down.
            properties.insert("enabled".to_string(), flag(false));
            children.push(entry(FIRST_ACCOUNT, properties));
        } else if state.account_count() == 0 {
            let mut properties = HashMap::new();
            properties.insert("label".to_string(), text("No accounts configured"));
            properties.insert("enabled".to_string(), flag(false));
            children.push(entry(FIRST_ACCOUNT, properties));
        } else {
            for (index, (label, summary, mounted)) in state.rows().into_iter().enumerate() {
                let mut properties = HashMap::new();
                properties.insert("label".to_string(), text(&format!("{label} — {summary}")));
                // Only a mounted account has a folder to open, so only that row
                // is clickable. The row is still shown, because its state is the
                // thing a user opened the menu to see.
                properties.insert("enabled".to_string(), flag(mounted));
                let id = FIRST_ACCOUNT + i32::try_from(index).unwrap_or(0);
                children.push(entry(id, properties));
            }
        }

        children.push(separator(SEPARATOR));

        let mut settings = HashMap::new();
        settings.insert("label".to_string(), text("Cirrove settings…"));
        children.push(entry(SETTINGS, settings));

        let mut quit = HashMap::new();
        quit.insert("label".to_string(), text("Quit tray"));
        children.push(entry(QUIT, quit));

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
