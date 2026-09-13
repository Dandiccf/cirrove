//! GTK widgets and asynchronous desktop controller.
use crate::model::{AccountCard, ConnectionState, Overview};
use adw::prelude::*;
use cirrove_service::accounts;
use gtk::{gio, glib};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

mod connect;

#[derive(Clone)]
pub enum Backend {
    Live {
        runtime: tokio::runtime::Handle,
        state: PathBuf,
        socket: PathBuf,
    },
    Demo,
}
/// An account operation in flight. One at a time: each ends in a settings
/// write or a daemon request, and the row it belongs to says which.
struct Operation {
    id: String,
    text: &'static str,
}
struct AccountRow {
    row: adw::ExpanderRow,
    mount: gtk::Button,
    open: gtk::Button,
    sign_in: gtk::Button,
    spinner: gtk::Spinner,
    status: adw::ActionRow,
    location: adw::ActionRow,
    storage: adw::ActionRow,
    identity: adw::ActionRow,
    access: adw::ActionRow,
    consent: gtk::Button,
    refused: adw::ActionRow,
    discard: gtk::Button,
    unsent: adw::ActionRow,
    remove: gtk::Button,
    /// What this account keeps offline. An expander rather than a flat list
    /// because it grows without bound and is not what most people open the
    /// window for; its subtitle carries the budget, so the size of the answer
    /// is readable without opening it.
    kept: adw::ExpanderRow,
    kept_rows: RefCell<Vec<adw::ActionRow>>,
    keep_add: gtk::MenuButton,
}
pub struct Window {
    pub window: glib::WeakRef<adw::ApplicationWindow>,
    backend: Backend,
    group: adw::PreferencesGroup,
    /// What changed lately, across accounts; hidden while there is nothing.
    activity: adw::PreferencesGroup,
    activity_rows: RefCell<Vec<adw::ActionRow>>,
    empty: adw::StatusPage,
    settings_retry: gtk::Button,
    empty_connect: gtk::Button,
    banner: adw::Banner,
    refresh_button: gtk::Button,
    connect_button: gtk::Button,
    toast: adw::ToastOverlay,
    rows: RefCell<HashMap<String, AccountRow>>,
    overview: RefCell<Option<Overview>>,
    refreshing: Cell<bool>,
    operation: RefCell<Option<Operation>>,
    operation_focus: RefCell<Option<glib::WeakRef<gtk::Widget>>>,
    generation: Cell<u64>,
    waiting: RefCell<HashMap<String, Instant>>,
    closed: Cell<bool>,
}
/// A button that is only an icon, with the name a screen reader says and the
/// tooltip a pointer shows: the same words, set once, so neither can lag the
/// other. GTK does not derive the accessible name from the tooltip.
fn icon_button(icon: &str, name: &str) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(name)
        .valign(gtk::Align::Center)
        .build();
    button.update_property(&[gtk::accessible::Property::Label(name)]);
    button
}
impl Window {
    pub fn new(app: &adw::Application, backend: Backend) -> Rc<Self> {
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Cirrove")
            .default_width(660)
            .default_height(620)
            .build();
        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&adw::WindowTitle::new("Cirrove", "Cloud drives")));
        let connect_button = icon_button("list-add-symbolic", "Connect a drive");
        header.pack_start(&connect_button);
        let refresh_button = icon_button("view-refresh-symbolic", "Refresh connection status");
        header.pack_end(&refresh_button);
        toolbar.add_top_bar(&header);
        let banner = adw::Banner::builder()
            .title("Cirrove service is unavailable")
            .button_label("Retry")
            .revealed(false)
            .build();
        toolbar.add_top_bar(&banner);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 24);
        body.set_margin_top(28);
        body.set_margin_bottom(24);
        body.set_margin_start(24);
        body.set_margin_end(24);
        let heading = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let title = gtk::Label::builder()
            .label("Your clouds, in Files")
            .xalign(0.0)
            .wrap(true)
            .build();
        title.add_css_class("title-1");
        let description = gtk::Label::builder()
            .label("Manage your saved cloud connections.")
            .xalign(0.0)
            .wrap(true)
            .build();
        description.add_css_class("dim-label");
        heading.append(&title);
        heading.append(&description);
        body.append(&heading);
        let group = adw::PreferencesGroup::new();
        body.append(&group);
        // Changes to content, not files merely opened: what arrived, changed
        // or went away in the cloud, and what was saved here and where it is
        // on its way. Present only when there is something to say.
        let activity = adw::PreferencesGroup::builder()
            .title("Recent activity")
            .visible(false)
            .build();
        body.append(&activity);
        let empty = adw::StatusPage::builder()
            .icon_name("io.github.Dandiccf.Cirrove-symbolic")
            .title("Loading connections…")
            .description("Reading your saved accounts and service status.")
            .build();
        let empty_actions = gtk::Box::new(gtk::Orientation::Vertical, 12);
        empty_actions.set_halign(gtk::Align::Center);
        let empty_connect = gtk::Button::with_label("Connect a drive");
        empty_connect.add_css_class("pill");
        empty_connect.add_css_class("suggested-action");
        empty_connect.set_visible(false);
        empty_actions.append(&empty_connect);
        let settings_retry = gtk::Button::with_label("Retry");
        settings_retry.add_css_class("pill");
        settings_retry.set_visible(false);
        empty_actions.append(&settings_retry);
        empty.set_child(Some(&empty_actions));
        body.append(&empty);
        let help = gtk::LinkButton::with_label(
            "https://github.com/Dandiccf/cirrove/blob/main/docs/onedrive-setup.md",
            "OneDrive setup guide",
        );
        help.set_halign(gtk::Align::Start);
        if matches!(backend, Backend::Demo) {
            help.set_sensitive(false);
        }
        body.append(&help);
        let footer = gtk::Label::builder()
            .label(if matches!(backend, Backend::Demo) {
                "Interface preview · sample accounts"
            } else {
                "Files download when opened · changes upload in the background"
            })
            .xalign(0.0)
            .wrap(true)
            .build();
        footer.add_css_class("dim-label");
        footer.add_css_class("caption");
        body.append(&footer);
        let clamp = adw::Clamp::builder()
            .maximum_size(700)
            .tightening_threshold(560)
            .child(&body)
            .build();
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&clamp)
            .vexpand(true)
            .build();
        toolbar.set_content(Some(&scroll));
        let toast = adw::ToastOverlay::new();
        toast.set_child(Some(&toolbar));
        window.set_content(Some(&toast));
        let ui = Rc::new(Self {
            window: window.downgrade(),
            backend,
            group,
            activity,
            activity_rows: RefCell::new(Vec::new()),
            empty,
            settings_retry,
            empty_connect,
            banner,
            refresh_button,
            connect_button,
            toast,
            rows: RefCell::new(HashMap::new()),
            overview: RefCell::new(None),
            refreshing: Cell::new(false),
            operation: RefCell::new(None),
            operation_focus: RefCell::new(None),
            generation: Cell::new(0),
            waiting: RefCell::new(HashMap::new()),
            closed: Cell::new(false),
        });
        let weak = Rc::downgrade(&ui);
        ui.refresh_button.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.refresh();
            }
        });
        for button in [&ui.connect_button, &ui.empty_connect] {
            let weak = Rc::downgrade(&ui);
            button.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    connect::present(&ui);
                }
            });
        }
        let weak = Rc::downgrade(&ui);
        ui.banner.connect_button_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.refresh();
            }
        });
        let weak = Rc::downgrade(&ui);
        ui.settings_retry.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.refresh();
            }
        });
        let weak = Rc::downgrade(&ui);
        window.connect_close_request(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.closed.set(true);
            }
            glib::Propagation::Proceed
        });
        let weak = Rc::downgrade(&ui);
        glib::timeout_add_local(Duration::from_secs(2), move || {
            let Some(ui) = weak.upgrade().filter(|ui| !ui.closed.get()) else {
                return glib::ControlFlow::Break;
            };
            ui.refresh();
            glib::ControlFlow::Continue
        });
        ui.refresh();
        window.present();
        ui
    }
    pub fn refresh(self: &Rc<Self>) {
        if self.closed.get() || self.operation.borrow().is_some() || self.refreshing.replace(true) {
            return;
        }
        match &self.backend {
            Backend::Demo => {
                let overview = self
                    .overview
                    .borrow()
                    .clone()
                    .or_else(|| crate::demo::overview().ok());
                if let Some(overview) = overview {
                    self.render(overview);
                }
                self.refreshing.set(false);
            }
            Backend::Live {
                runtime,
                state,
                socket,
            } => {
                let state = state.clone();
                let socket = socket.clone();
                let (send, receive) = tokio::sync::oneshot::channel();
                runtime.spawn(async move {
                    let result = crate::model::snapshot(state, socket).await;
                    let _ = send.send(Overview::from_snapshot(result));
                });
                let weak = Rc::downgrade(self);
                let generation = self.generation.get();
                glib::spawn_future_local(async move {
                    let result = receive.await;
                    let Some(ui) = weak.upgrade().filter(|ui| !ui.closed.get()) else {
                        return;
                    };
                    ui.refreshing.set(false);
                    if ui.generation.get() != generation {
                        ui.refresh();
                        return;
                    }
                    if let Ok(overview) = result {
                        ui.render(overview);
                    } else {
                        ui.notify("Could not read connection status. Try again.");
                    }
                });
            }
        }
    }
    pub fn render(self: &Rc<Self>, overview: Overview) {
        self.banner.set_revealed(overview.service_error.is_some());
        if let Some(error) = &overview.service_error {
            self.banner.set_title(error.description());
        }
        self.empty
            .set_visible(!overview.settings_available || overview.accounts.is_empty());
        self.group
            .set_visible(overview.settings_available && !overview.accounts.is_empty());
        self.empty.set_title(if overview.settings_available {
            "No drives connected"
        } else {
            "Account settings unavailable"
        });
        let description = overview.settings_error.as_ref().map_or_else(
            || {
                "Connect a OneDrive and its files appear in Files, downloaded as you open them."
                    .into()
            },
            |error| error.description(),
        );
        self.empty.set_description(Some(&description));
        self.settings_retry
            .set_visible(overview.settings_error.is_some());
        self.empty_connect
            .set_visible(overview.settings_error.is_none());
        let mut rows = self.rows.borrow_mut();
        rows.retain(|id, row| {
            let keep = overview.accounts.iter().any(|a| &a.id == id);
            if !keep {
                self.group.remove(&row.row);
            }
            keep
        });
        let mut waiting = self.waiting.borrow_mut();
        waiting.retain(|id, _| {
            overview
                .accounts
                .iter()
                .any(|a| &a.id == id && a.state.busy())
        });
        for card in &overview.accounts {
            if !rows.contains_key(&card.id) {
                let row = self.account_row(&card.id);
                self.group.add(&row.row);
                rows.insert(card.id.clone(), row);
            }
            if let Some(row) = rows.get(&card.id) {
                let late = card.state.busy()
                    && waiting
                        .entry(card.id.clone())
                        .or_insert_with(Instant::now)
                        .elapsed()
                        >= Duration::from_secs(30);
                self.update_row(row, card, late);
            }
        }
        self.render_activity(&overview.activity);
        *self.overview.borrow_mut() = Some(overview);
        // Disabling the active button can clear GTK's keyboard focus. Restore
        // it after saving, but preserve any focus the user moved meanwhile.
        if self.operation.borrow().is_none()
            && let Some(focus) = self.operation_focus.borrow_mut().take()
            && let Some(window) = self.window.upgrade()
            && gtk::prelude::RootExt::focus(&window).is_none()
            && let Some(widget) = focus.upgrade()
            && widget.is_sensitive()
            && widget.root().as_ref() == Some(window.upcast_ref())
        {
            widget.grab_focus();
        }
    }
    /// Rebuilt on every render: at most a few rows, and a diff would be more
    /// code than the rows.
    fn render_activity(&self, entries: &[crate::model::ActivityEntry]) {
        let mut rows = self.activity_rows.borrow_mut();
        for row in rows.drain(..) {
            self.activity.remove(&row);
        }
        for entry in entries.iter().take(12) {
            let row = adw::ActionRow::builder()
                .title(&entry.name)
                .subtitle(format!("{} · {}", entry.account, entry.what))
                .use_markup(false)
                .subtitle_lines(1)
                .build();
            if entry.warning {
                row.add_css_class("warning");
            }
            self.activity.add(&row);
            rows.push(row);
        }
        self.activity.set_visible(!entries.is_empty());
    }
    fn account_row(self: &Rc<Self>, id: &str) -> AccountRow {
        let row = adw::ExpanderRow::builder().use_markup(false).build();
        let icon = gtk::Image::from_icon_name("io.github.Dandiccf.Cirrove-symbolic");
        icon.set_pixel_size(32);
        icon.add_css_class("accent");
        row.add_prefix(&icon);
        let spinner = gtk::Spinner::new();
        row.add_suffix(&spinner);
        let open = icon_button("folder-open-symbolic", "Open in Files");
        open.add_css_class("flat");
        row.add_suffix(&open);
        // Where the state says sign in, the button to do it is right there.
        let sign_in = gtk::Button::builder()
            .label("Sign in again")
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        sign_in.add_css_class("suggested-action");
        row.add_suffix(&sign_in);
        let mount = gtk::Button::builder()
            .label("Mount")
            .valign(gtk::Align::Center)
            .build();
        row.add_suffix(&mount);
        let status = adw::ActionRow::builder()
            .title("Connection status")
            .use_markup(false)
            .subtitle_lines(0)
            .build();
        let location = adw::ActionRow::builder()
            .title("Location in Files")
            .use_markup(false)
            .subtitle_selectable(true)
            .subtitle_lines(2)
            .build();
        let identity = adw::ActionRow::builder()
            .title("Microsoft account")
            .use_markup(false)
            .subtitle_selectable(true)
            .subtitle_lines(2)
            .build();
        let access = adw::ActionRow::builder()
            .title("Access")
            .use_markup(false)
            .subtitle_lines(0)
            .build();
        // Changing an account's consent was the last ordinary flow that needed a
        // terminal (`cirrove reauth --write-access`). It is a re-sign-in either
        // way, so the provider's own consent screen is what actually grants or
        // narrows the access; this button only asks for it.
        let consent = gtk::Button::builder()
            .label("Allow changes")
            .valign(gtk::Align::Center)
            .build();
        access.add_suffix(&consent);
        let storage = adw::ActionRow::builder()
            .title("Local cache")
            .use_markup(false)
            .build();
        // Shown only while there is something to discard. The daemon has
        // stopped retrying these; the user decides what happens to the copies.
        let refused = adw::ActionRow::builder()
            .title("Changes the cloud refused")
            .use_markup(false)
            .subtitle_lines(0)
            .visible(false)
            .build();
        refused.add_css_class("warning");
        let discard = gtk::Button::builder()
            .label("Discard")
            .valign(gtk::Align::Center)
            .build();
        discard.add_css_class("destructive-action");
        refused.add_suffix(&discard);
        let kept = adw::ExpanderRow::builder()
            .title("Kept offline")
            .use_markup(false)
            .subtitle_lines(0)
            .build();
        // Adding a pin needs a file to point at, and the file chooser is the
        // only honest way to pick one: the daemon takes a mount-relative path,
        // and a text field would invite paths that are not in the drive.
        // Two entries behind one control: a file chooser picks a file or a
        // folder and never both, and two icon buttons in a row this narrow read
        // as clutter.
        let keep_file = gtk::Button::builder()
            .label("File…")
            .halign(gtk::Align::Fill)
            .build();
        keep_file.add_css_class("flat");
        let keep_folder = gtk::Button::builder()
            .label("Folder…")
            .halign(gtk::Align::Fill)
            .build();
        keep_folder.add_css_class("flat");
        let choices = gtk::Box::new(gtk::Orientation::Vertical, 0);
        choices.append(&keep_file);
        choices.append(&keep_folder);
        let popover = gtk::Popover::builder().child(&choices).build();
        let keep_add = gtk::MenuButton::builder()
            .icon_name("list-add-symbolic")
            // A name from the start. render_kept_offline replaces it with one
            // that says why it is insensitive when the account is unmounted,
            // but a control that is only an icon must never exist unnamed.
            .tooltip_text("Keep a file or folder offline")
            .valign(gtk::Align::Center)
            .popover(&popover)
            .build();
        keep_add.add_css_class("flat");
        keep_add.update_property(&[gtk::accessible::Property::Label(
            "Keep a file or folder offline",
        )]);
        kept.add_suffix(&keep_add);

        let removal = adw::ActionRow::builder()
            .title("Remove this connection")
            .subtitle("Its sign-in and local index are set aside; nothing in the cloud is touched.")
            .use_markup(false)
            .subtitle_lines(0)
            .build();
        let remove = gtk::Button::builder()
            .label("Remove")
            .valign(gtk::Align::Center)
            .build();
        remove.add_css_class("destructive-action");
        removal.add_suffix(&remove);
        row.add_row(&status);
        row.add_row(&identity);
        row.add_row(&access);
        row.add_row(&location);
        row.add_row(&storage);
        row.add_row(&kept);
        // Distinct from the refused row above: those are changes to the
        // namespace the cloud would not take; this is a file's content that
        // did not reach the cloud. Its remedy is different too -- open the
        // file and save it again -- so it carries no discard button.
        let unsent = adw::ActionRow::builder()
            .title("Saves that did not reach the cloud")
            .use_markup(false)
            .subtitle_lines(0)
            .visible(false)
            .build();
        unsent.add_css_class("warning");
        row.add_row(&refused);
        row.add_row(&unsent);
        row.add_row(&removal);
        let weak = Rc::downgrade(self);
        let key = id.to_owned();
        mount.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.toggle(&key);
            }
        });
        let weak = Rc::downgrade(self);
        let key = id.to_owned();
        open.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.open(&key);
            }
        });
        let weak = Rc::downgrade(self);
        let key = id.to_owned();
        sign_in.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.sign_in(&key);
            }
        });
        let weak = Rc::downgrade(self);
        let key = id.to_owned();
        discard.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.discard(&key);
            }
        });
        let weak = Rc::downgrade(self);
        let key = id.to_owned();
        remove.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.remove(&key);
            }
        });
        let weak = Rc::downgrade(self);
        let key = id.to_owned();
        consent.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.change_access(&key);
            }
        });
        for (button, folder) in [(&keep_file, false), (&keep_folder, true)] {
            let weak = Rc::downgrade(self);
            let key = id.to_owned();
            let popover = popover.clone();
            button.connect_clicked(move |_| {
                popover.popdown();
                if let Some(ui) = weak.upgrade() {
                    ui.keep_offline(&key, folder);
                }
            });
        }
        AccountRow {
            row,
            mount,
            open,
            sign_in,
            spinner,
            status,
            location,
            identity,
            access,
            consent,
            storage,
            refused,
            discard,
            unsent,
            remove,
            kept,
            kept_rows: RefCell::new(Vec::new()),
            keep_add,
        }
    }
    fn update_row(self: &Rc<Self>, row: &AccountRow, card: &AccountCard, late: bool) {
        row.row.set_title(&card.title);
        let live = matches!(self.backend, Backend::Live { .. });
        let operation = self.operation.borrow();
        let idle = operation.is_none();
        let writing = operation.as_ref().filter(|op| op.id == card.id);
        let state = if let Some(operation) = writing {
            operation.text
        } else if late {
            "Still waiting for service"
        } else {
            card.state.label()
        };
        row.row
            .set_subtitle(&format!("{} · {state}", card.username));
        row.status.set_subtitle(if late { "The request is saved, but the service has not confirmed it. You can change the mount preference or check the service." } else { card.state.description() });
        if card.state.warning() || late {
            row.status.add_css_class("warning");
        } else {
            row.status.remove_css_class("warning");
        }
        row.mount.set_label(card.action_label());
        row.mount.set_sensitive(card.controls_available && idle);
        row.open.set_sensitive(card.mounted && live);
        // Offered wherever the state says so, in the preview too: the preview
        // shows what the window does, and the actions themselves are what
        // check for a live service.
        row.sign_in
            .set_visible(card.state == ConnectionState::SignInRequired);
        row.sign_in.set_sensitive(idle);
        row.spinner
            .set_spinning(writing.is_some() || card.state.busy());
        row.spinner
            .set_visible(writing.is_some() || card.state.busy());
        row.location
            .set_subtitle(&card.mount_path.to_string_lossy());
        row.identity.set_subtitle(&card.username);
        row.access.set_subtitle(if card.writable {
            "Changes made in this drive are uploaded to the cloud."
        } else {
            "Read-only: files can be opened but not changed."
        });
        row.consent.set_label(if card.writable {
            "Make read-only"
        } else {
            "Allow changes"
        });
        row.consent.set_tooltip_text(Some(if card.writable {
            "Sign in again asking only to read, so this drive stops accepting changes"
        } else {
            "Sign in again asking to make changes, so files in this drive can be saved"
        }));
        // Asking to write is the direction that grants something, so it is the
        // one marked; asking to read less is ordinary.
        if card.writable {
            row.consent.remove_css_class("suggested-action");
        } else {
            row.consent.add_css_class("suggested-action");
        }
        row.consent.set_sensitive(idle && card.controls_available);
        row.storage.set_subtitle(&format!(
            "Up to {:.1} GiB · downloaded as needed",
            card.cache_bytes as f64 / 1024_f64.powi(3)
        ));
        row.refused.set_visible(card.stuck > 0);
        row.refused.set_subtitle(&format!(
            "{} that the cloud would not accept. They will not be retried. Discarding removes the local copies; the cloud keeps its version.",
            if card.stuck == 1 {
                "1 change".to_owned()
            } else {
                format!("{} changes", card.stuck)
            }
        ));
        row.discard.set_sensitive(idle);
        row.unsent.set_visible(card.failed_uploads > 0);
        row.unsent.set_subtitle(&format!(
            "{} did not reach the cloud. The file is on this computer; the cloud has an older version or none. Open the file and save it again to try once more.",
            if card.failed_uploads == 1 {
                "1 save".to_owned()
            } else {
                format!("{} saves", card.failed_uploads)
            },
        ));
        self.render_kept_offline(row, card, idle);
        // Removal under a running mount would race it; the daemon refuses, and
        // the button says so before the user gets that far.
        let removable = !card.enabled && !card.mounted;
        row.remove.set_sensitive(removable && idle);
        row.remove.set_tooltip_text(Some(if removable {
            "Remove this connection"
        } else {
            "Unmount before removing"
        }));
    }
    /// Rebuild one account's list of kept-offline items.
    ///
    /// Rebuilt rather than diffed: the list is short, it changes only when
    /// someone pins or unpins, and a diff would have to key on an item id that
    /// is also the thing whose row moved. The rows are tracked so they can be
    /// removed again; an ExpanderRow has no way to ask what is in it.
    fn render_kept_offline(self: &Rc<Self>, row: &AccountRow, card: &AccountCard, idle: bool) {
        let mut rows = row.kept_rows.borrow_mut();
        for old in rows.drain(..) {
            row.kept.remove(&old);
        }
        for pin in &card.kept_offline {
            let entry = adw::ActionRow::builder()
                .title(&pin.name)
                .subtitle(if pin.recursive {
                    format!("{} · everything inside it", pin.detail)
                } else {
                    pin.detail.clone()
                })
                .use_markup(false)
                .subtitle_lines(0)
                .build();
            let stop = gtk::Button::builder()
                .label("Stop keeping")
                .valign(gtk::Align::Center)
                .sensitive(idle && card.controls_available)
                .build();
            stop.add_css_class("flat");
            let weak = Rc::downgrade(self);
            let key = card.id.clone();
            let item = pin.item.clone();
            let name = pin.name.clone();
            stop.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.stop_keeping(&key, &item, &name);
                }
            });
            entry.add_suffix(&stop);
            row.kept.add_row(&entry);
            rows.push(entry);
        }
        row.kept
            .set_subtitle(&match (&card.pin_budget, card.kept_offline.len()) {
                // The budget sentence is the useful one, because the question a
                // person opens this for is how much room is left.
                (Some(budget), _) => budget.clone(),
                (None, 0) => "Nothing is kept offline yet.".to_owned(),
                (None, 1) => "1 item kept offline.".to_owned(),
                (None, n) => format!("{n} items kept offline."),
            });
        row.keep_add
            .set_sensitive(idle && card.controls_available && card.mounted);
        row.keep_add.set_tooltip_text(Some(if card.mounted {
            "Keep a file or folder offline"
        } else {
            "Mount this connection to choose a file"
        }));
    }
    fn card(&self, id: &str) -> Option<AccountCard> {
        self.overview
            .borrow()
            .as_ref()?
            .accounts
            .iter()
            .find(|a| a.id == id)
            .cloned()
    }
    pub fn current(&self) -> Option<Overview> {
        self.overview.borrow().clone()
    }
    /// Claim the one operation slot and redraw with it. False if another is
    /// running or the window is closing.
    fn begin_operation(self: &Rc<Self>, id: &str, text: &'static str) -> bool {
        if self.operation.borrow().is_some() || self.closed.get() {
            return false;
        }
        self.generation.set(self.generation.get().wrapping_add(1));
        *self.operation_focus.borrow_mut() = self
            .window
            .upgrade()
            .and_then(|window| gtk::prelude::RootExt::focus(&window))
            .map(|widget| widget.downgrade());
        *self.operation.borrow_mut() = Some(Operation {
            id: id.to_owned(),
            text,
        });
        let view = self.overview.borrow().clone();
        if let Some(view) = view {
            self.render(view);
        }
        true
    }
    fn end_operation(&self) {
        *self.operation.borrow_mut() = None;
    }
    pub fn toggle(self: &Rc<Self>, id: &str) {
        let Some(card) = self.card(id).filter(|a| a.controls_available) else {
            return;
        };
        if !self.begin_operation(id, "Saving mount preference…") {
            return;
        }
        let enabled = !card.enabled;
        match &self.backend {
            Backend::Demo => {
                let weak = Rc::downgrade(self);
                let id = id.to_owned();
                glib::timeout_add_local_once(Duration::from_millis(400), move || {
                    let Some(ui) = weak.upgrade().filter(|ui| !ui.closed.get()) else {
                        return;
                    };
                    ui.end_operation();
                    let mut view = ui.overview.borrow().clone();
                    if let Some(view) = view.as_mut() {
                        if let Some(card) = view.accounts.iter_mut().find(|a| a.id == id) {
                            card.enabled = enabled;
                            card.mounted = enabled;
                            card.state = if enabled {
                                ConnectionState::Connected
                            } else {
                                ConnectionState::Unmounted
                            };
                        }
                        ui.render(view.clone());
                    }
                });
            }
            Backend::Live { runtime, state, .. } => {
                let state = state.clone();
                let id = id.to_owned();
                let (send, receive) = tokio::sync::oneshot::channel();
                runtime.spawn_blocking(move || {
                    let result = accounts::set_enabled_by_id(&state, &id, enabled).map_err(|_| ());
                    let _ = send.send(result);
                });
                let weak = Rc::downgrade(self);
                glib::spawn_future_local(async move {
                    let result = receive.await;
                    let Some(ui) = weak.upgrade().filter(|ui| !ui.closed.get()) else {
                        return;
                    };
                    ui.end_operation();
                    if !matches!(result, Ok(Ok(()))) {
                        ui.notify("Could not save the mount preference. Another account operation may be running; refresh and try again.");
                    }
                    ui.refresh();
                });
            }
        }
    }
    /// Ask the provider for the other access level: write where the account is
    /// read-only, read-only where it can write.
    ///
    /// The same re-sign-in as `sign_in`, with a level asked for rather than the
    /// current one kept. It is the provider's consent screen that actually
    /// grants or narrows anything -- this only asks -- which is why no
    /// confirmation is put in front of it: the browser already shows exactly
    /// what is being granted, and a dialog here would be a second, vaguer copy
    /// of that.
    pub fn change_access(self: &Rc<Self>, id: &str) {
        let Some(card) = self.card(id) else {
            return;
        };
        let wanted = if card.writable {
            cirrove_auth::AccessMode::ReadOnly
        } else {
            cirrove_auth::AccessMode::ReadWrite
        };
        self.reauthenticate(id, Some(wanted));
    }
    /// A new grant for an account whose old one stopped working. The browser
    /// does the asking; the daemon releases the account meanwhile and takes it
    /// back after, which the row shows as it happens.
    pub fn sign_in(self: &Rc<Self>, id: &str) {
        self.reauthenticate(id, None);
    }
    /// The sign-in both of the above are: `access` of `None` keeps whatever the
    /// account has.
    fn reauthenticate(self: &Rc<Self>, id: &str, access: Option<cirrove_auth::AccessMode>) {
        let Some(card) = self.card(id) else {
            return;
        };
        let Backend::Live { runtime, state, .. } = &self.backend else {
            return;
        };
        if !self.begin_operation(id, "Signing in in your browser…") {
            return;
        }
        let state = state.clone();
        let label = card.label.clone();
        let (send, receive) = tokio::sync::oneshot::channel();
        // block_on off the runtime's worker threads: the sign-in holds a
        // browser callback and the keyring open across awaits, and nothing
        // about that needs to be Send.
        runtime.spawn_blocking(move || {
            let result = tokio::runtime::Handle::current()
                .block_on(accounts::reauthenticate(state, label, access))
                .map_err(|error| format!("{error:#}"));
            let _ = send.send(result);
        });
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = receive.await;
            let Some(ui) = weak.upgrade().filter(|ui| !ui.closed.get()) else {
                return;
            };
            ui.end_operation();
            match result {
                Ok(Ok(())) => ui.notify(match access {
                    Some(cirrove_auth::AccessMode::ReadWrite) => {
                        "Signed in. Changes in this drive are uploaded to the cloud."
                    }
                    Some(cirrove_auth::AccessMode::ReadOnly) => {
                        "Signed in. This drive is read-only again."
                    }
                    None => "Signed in again.",
                }),
                Ok(Err(error)) => ui.notify(&format!("The sign-in did not finish: {error}")),
                Err(_) => ui.notify("The sign-in did not finish."),
            }
            ui.refresh();
        });
    }
    /// Release one pin. Named by the path so the message is readable, acted on
    /// by the item id so a file renamed in the cloud since the list was drawn
    /// still unpins the right thing.
    pub fn stop_keeping(self: &Rc<Self>, id: &str, item: &str, name: &str) {
        let Some(card) = self.card(id) else {
            return;
        };
        let Backend::Live {
            runtime, socket, ..
        } = &self.backend
        else {
            return;
        };
        if !self.begin_operation(id, "Releasing…") {
            return;
        }
        let socket = socket.clone();
        let request = cirrove_service::PinRequest {
            label: card.label.clone(),
            item: Some(item.to_owned()),
            path: None,
            recursive: false,
            bytes: None,
        };
        let shown = name.to_owned();
        let (send, receive) = tokio::sync::oneshot::channel();
        runtime.spawn(async move {
            let result = cirrove_service::unpin(&socket, &request)
                .await
                .map_err(|error| format!("{error:#}"));
            let _ = send.send(result);
        });
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = receive.await;
            let Some(ui) = weak.upgrade().filter(|ui| !ui.closed.get()) else {
                return;
            };
            ui.end_operation();
            match result {
                Ok(Ok(reply)) => match reply.refusal {
                    Some(refusal) => {
                        ui.notify(&format!("{shown} is still kept offline: {refusal}"))
                    }
                    None if reply.accepted => {
                        ui.notify(&format!("{shown} is no longer kept offline."));
                    }
                    None => ui.notify(&format!("{shown} was not kept offline.")),
                },
                Ok(Err(error)) => ui.notify(&format!("Could not release {shown}: {error}")),
                Err(_) => ui.notify("The service did not answer."),
            }
            ui.refresh();
        });
    }
    /// Choose a file or folder in this account's mount and keep it offline.
    ///
    /// The chooser starts at the mount and the chosen path is made relative to
    /// it, because the daemon takes a mount-relative path. Anything outside the
    /// mount is refused here with a sentence rather than sent and refused with
    /// an error, since the user cannot tell from the dialog which is which.
    pub fn keep_offline(self: &Rc<Self>, id: &str, folder: bool) {
        let Some(card) = self.card(id).filter(|c| c.mounted) else {
            return;
        };
        let Backend::Live { .. } = &self.backend else {
            return;
        };
        let chooser = gtk::FileDialog::builder()
            .title(if folder {
                "Keep a folder offline"
            } else {
                "Keep a file offline"
            })
            .accept_label("Keep offline")
            .initial_folder(&gtk::gio::File::for_path(&card.mount_path))
            .modal(true)
            .build();
        let weak = Rc::downgrade(self);
        let key = id.to_owned();
        let window = self.window.upgrade();
        // Two calls, not one dialog: GTK's file chooser picks a file or a
        // folder, never either. A single "file or folder" button over `open`
        // would offer folders in its title and refuse to return one.
        let chosen = move |result: Result<gtk::gio::File, glib::Error>| {
            let Some(ui) = weak.upgrade().filter(|ui| !ui.closed.get()) else {
                return;
            };
            // A cancelled dialog is not a failure and says nothing.
            let Ok(file) = result else {
                return;
            };
            let Some(path) = file.path() else {
                return;
            };
            ui.keep_path(&key, &path);
        };
        if folder {
            chooser.select_folder(window.as_ref(), gtk::gio::Cancellable::NONE, chosen);
        } else {
            chooser.open(window.as_ref(), gtk::gio::Cancellable::NONE, chosen);
        }
    }
    /// The half of `keep_offline` that does not need a dialog, so a test can
    /// reach it: turn a filesystem path into a mount-relative one and pin it.
    pub fn keep_path(self: &Rc<Self>, id: &str, path: &std::path::Path) {
        let Some(card) = self.card(id) else {
            return;
        };
        let Backend::Live {
            runtime, socket, ..
        } = &self.backend
        else {
            return;
        };
        let Ok(relative) = path.strip_prefix(&card.mount_path) else {
            self.notify("Choose a file inside this drive's folder.");
            return;
        };
        let relative = relative.to_string_lossy().into_owned();
        if relative.is_empty() {
            self.notify(
                "Keeping the whole drive offline is not offered; choose a folder inside it.",
            );
            return;
        }
        let recursive = path.is_dir();
        if !self.begin_operation(id, "Keeping offline…") {
            return;
        }
        let socket = socket.clone();
        let request = cirrove_service::PinRequest {
            label: card.label.clone(),
            item: None,
            path: Some(relative.clone()),
            recursive,
            bytes: None,
        };
        let (send, receive) = tokio::sync::oneshot::channel();
        runtime.spawn(async move {
            let result = cirrove_service::pin(&socket, &request)
                .await
                .map_err(|error| format!("{error:#}"));
            let _ = send.send(result);
        });
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = receive.await;
            let Some(ui) = weak.upgrade().filter(|ui| !ui.closed.get()) else {
                return;
            };
            ui.end_operation();
            match result {
                Ok(Ok(reply)) => match reply.refusal {
                    Some(refusal) => ui.notify(&refusal),
                    None if !reply.accepted => ui.notify(&format!("{relative} was not kept offline.")),
                    // An incomplete walk is not an error: the pin is recorded
                    // and honoured for what is indexed, and fills in as the
                    // index does. Saying so beats silence when the number of
                    // files looks too small.
                    None if recursive && !reply.complete => ui.notify(&format!(
                        "Keeping {relative} offline. {} file(s) so far; the rest follow as Cirrove finishes listing the folder.",
                        reply.files
                    )),
                    None => ui.notify(&format!(
                        "Keeping {relative} offline, {}.",
                        cirrove_service::human_bytes(reply.reserved)
                    )),
                },
                Ok(Err(error)) => ui.notify(&format!("Could not keep {relative} offline: {error}")),
                Err(_) => ui.notify("The service did not answer."),
            }
            ui.refresh();
        });
    }
    /// Abandon the changes the cloud refused. The daemon does the unwinding
    /// and says how many it could; the row disappears with the last one.
    pub fn discard(self: &Rc<Self>, id: &str) {
        let Some(card) = self.card(id).filter(|c| c.stuck > 0) else {
            return;
        };
        let Backend::Live {
            runtime, socket, ..
        } = &self.backend
        else {
            return;
        };
        if !self.begin_operation(id, "Discarding refused changes…") {
            return;
        }
        let socket = socket.clone();
        let label = card.label.clone();
        let (send, receive) = tokio::sync::oneshot::channel();
        runtime.spawn(async move {
            let result = cirrove_service::discard_stuck(&socket, &label)
                .await
                .map_err(|error| format!("{error:#}"));
            let _ = send.send(result);
        });
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = receive.await;
            let Some(ui) = weak.upgrade().filter(|ui| !ui.closed.get()) else {
                return;
            };
            ui.end_operation();
            match result {
                Ok(Ok(reply)) => match reply.refusal {
                    Some(refusal) => ui.notify(&format!("Nothing discarded: {refusal}")),
                    None if reply.remaining == 0 => {
                        ui.notify(&format!("Discarded {} change(s).", reply.discarded));
                    }
                    None => ui.notify(&format!(
                        "Discarded {} change(s); {} could not be unwound and remain.",
                        reply.discarded, reply.remaining
                    )),
                },
                Ok(Err(error)) => ui.notify(&format!("Could not discard: {error}")),
                Err(_) => ui.notify("Could not discard the refused changes."),
            }
            ui.refresh();
        });
    }
    /// Ask before removing, and ask differently when the account still holds
    /// changes the cloud has not received: those are the user's, and removing
    /// the connection is the one way to lose them.
    pub fn remove(self: &Rc<Self>, id: &str) {
        let Some(card) = self.card(id).filter(|c| !c.enabled && !c.mounted) else {
            return;
        };
        let Backend::Live { runtime, state, .. } = &self.backend else {
            return;
        };
        if self.operation.borrow().is_some() || self.closed.get() {
            return;
        }
        let state = state.clone();
        let label = card.label.clone();
        let (send, receive) = tokio::sync::oneshot::channel();
        runtime.spawn_blocking(move || {
            let result =
                accounts::unsent_changes(&state, &label).map_err(|error| format!("{error:#}"));
            let _ = send.send(result);
        });
        let weak = Rc::downgrade(self);
        let id = id.to_owned();
        glib::spawn_future_local(async move {
            let result = receive.await;
            let Some(ui) = weak.upgrade().filter(|ui| !ui.closed.get()) else {
                return;
            };
            match result {
                Ok(Ok(unsent)) => ui.confirm_removal(&id, unsent),
                Ok(Err(error)) => ui.notify(&format!("Could not check this connection: {error}")),
                Err(_) => ui.notify("Could not check this connection."),
            }
        });
    }
    fn confirm_removal(self: &Rc<Self>, id: &str, unsent: usize) {
        let Some(card) = self.card(id) else {
            return;
        };
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let dialog = adw::AlertDialog::new(Some(&format!("Remove {}?", card.title)), None);
        dialog.add_response("cancel", "Cancel");
        if unsent > 0 {
            dialog.set_body(&format!(
                "{unsent} change(s) made in this drive have not reached the cloud. Removing the connection discards them. To keep them, mount the drive again and let them upload first. Nothing already in the cloud is touched."
            ));
            dialog.add_response("remove", "Remove and discard");
        } else {
            dialog.set_body(
                "Its sign-in and local index are set aside rather than deleted, and nothing in the cloud is touched.",
            );
            dialog.add_response("remove", "Remove");
        }
        dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        let id = id.to_owned();
        let discard = unsent > 0;
        dialog.connect_response(None, move |_, response| {
            if response == "remove"
                && let Some(ui) = weak.upgrade()
            {
                ui.forget(&id, discard);
            }
        });
        dialog.present(Some(&window));
    }
    fn forget(self: &Rc<Self>, id: &str, discard_unsent: bool) {
        let Some(card) = self.card(id) else {
            return;
        };
        let Backend::Live { runtime, state, .. } = &self.backend else {
            return;
        };
        if !self.begin_operation(id, "Removing…") {
            return;
        }
        let state = state.clone();
        let label = card.label.clone();
        let (send, receive) = tokio::sync::oneshot::channel();
        runtime.spawn_blocking(move || {
            let result = accounts::forget(&state, &label, discard_unsent)
                .map_err(|error| format!("{error:#}"));
            let _ = send.send(result);
        });
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = receive.await;
            let Some(ui) = weak.upgrade().filter(|ui| !ui.closed.get()) else {
                return;
            };
            ui.end_operation();
            match result {
                Ok(Ok(message)) => ui.notify(&message),
                Ok(Err(error)) => ui.notify(&format!("Could not remove: {error}")),
                Err(_) => ui.notify("Could not remove the connection."),
            }
            ui.refresh();
        });
    }
    fn open(self: &Rc<Self>, id: &str) {
        if !matches!(self.backend, Backend::Live { .. }) {
            return;
        }
        let Some(card) = self.card(id).filter(|a| a.mounted) else {
            return;
        };
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let file = gio::File::for_path(card.mount_path);
        let launcher = gtk::FileLauncher::new(Some(&file));
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            if launcher.launch_future(Some(&window)).await.is_err()
                && let Some(ui) = weak.upgrade()
            {
                ui.notify("Files could not open this mount. Check the connection and try again.");
            }
        });
    }
    fn notify(&self, message: &str) {
        self.toast.add_toast(adw::Toast::new(message));
    }
}
