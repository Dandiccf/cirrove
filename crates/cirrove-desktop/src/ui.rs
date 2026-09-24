//! GTK widgets and asynchronous desktop controller.
use crate::i18n::{fill, gettext, n};
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
    /// Offered beside Discard, and insensitive when every refusal is a conflict:
    /// those are the ones the cloud already decided about, and re-sending one
    /// would act on whatever is there now.
    retry: gtk::Button,
    keep_both: gtk::Button,
    destroy: gtk::Button,
    wastebasket: adw::ActionRow,
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
    empty: gtk::Box,
    empty_icon: gtk::Image,
    empty_title: gtk::Label,
    empty_description: gtk::Label,
    settings_retry: gtk::Button,
    empty_connect: gtk::Button,
    banner: adw::Banner,
    /// Shown only when the session bus says no tray host is running. The
    /// tray already reports this on its own stderr, which is where an
    /// operator looks and not where the person whose icon never appeared
    /// does; they read this window.
    tray_notice: adw::Banner,
    restart_notice: adw::Banner,
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
/// Now, in unix seconds, for the relative times in the activity list.
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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
    /// The identity a shell pairs the window with, set here rather than at any
    /// one entry point.
    ///
    /// On X11 a shell matches a window to its desktop entry by WM_CLASS, GTK
    /// builds WM_CLASS from the program name, and the program name defaults to
    /// the binary -- so the window announced itself as "cirrove-desktop"
    /// against an entry named io.github.Dandiccf.Cirrove.desktop, and nothing
    /// could pair them. Found by hand in a Plasma X11 session.
    ///
    /// It sat in main.rs first, which was wrong in a way the test caught
    /// immediately: anything that builds the window another way -- a test, a
    /// future entry point -- got the binary's name, and under the harness the
    /// class came out empty. An identity that depends on which function started
    /// the process is not the application's identity.
    pub const APP_ID: &'static str = "io.github.Dandiccf.Cirrove";

    pub fn new(app: &adw::Application, backend: Backend) -> Rc<Self> {
        // Before any surface is realized: GTK reads the program name when it
        // creates the first one, and setting it afterwards would silently do
        // nothing.
        glib::set_prgname(Some(Self::APP_ID));
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title(gettext("Cirrove"))
            .default_width(660)
            .default_height(540)
            .build();
        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&adw::WindowTitle::new(
            &gettext("Cirrove"),
            &gettext("Cloud drives"),
        )));
        let connect_button = icon_button("list-add-symbolic", "Connect a drive");
        header.pack_start(&connect_button);
        let refresh_button = icon_button("view-refresh-symbolic", "Refresh connection status");
        header.pack_end(&refresh_button);
        toolbar.add_top_bar(&header);
        let banner = adw::Banner::builder()
            .title(gettext("Cirrove service is unavailable"))
            .button_label(gettext("Retry"))
            .revealed(false)
            .build();
        toolbar.add_top_bar(&banner);
        let tray_notice = adw::Banner::builder()
            // One line, no `\` continuation: Rust strips the newline and the
            // indentation, C does not, so xgettext would extract a message id
            // with eighteen spaces in it that the running program never looks
            // up. scripts/test-translations.py refuses that now.
            .title(gettext("No tray icon: this desktop has no tray. On GNOME, install the AppIndicator extension; most other desktops provide one."))
            .revealed(false)
            .build();
        toolbar.add_top_bar(&tray_notice);
        // An upgrade replaces the files and leaves the running processes on the
        // old ones, so a person can sit on a version they have already replaced
        // with nothing anywhere saying so. Measured on Fedora: after dnf
        // upgrade the daemon's pid was unchanged and its /proc/<pid>/exe read
        // "(deleted)". No button, because restarting the service from inside
        // the thing that would be restarted is a trick, and logging out is the
        // instruction that always works.
        let restart_notice = adw::Banner::builder()
            .title(gettext("A newer version of Cirrove is installed. Log out and back in, or restart the Cirrove service, to start using it."))
            .revealed(false)
            .build();
        toolbar.add_top_bar(&restart_notice);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 18);
        body.set_margin_top(24);
        body.set_margin_bottom(18);
        body.set_margin_start(24);
        body.set_margin_end(24);
        let heading = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let title = gtk::Label::builder()
            .label(gettext("Your clouds, in Files"))
            .xalign(0.0)
            .wrap(true)
            .build();
        title.add_css_class("title-1");
        let description = gtk::Label::builder()
            .label(gettext("Manage your saved cloud connections."))
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
            .title(gettext("Recent activity"))
            .visible(false)
            .build();
        body.append(&activity);
        // A compact card belongs inside this settings page. AdwStatusPage is a
        // whole-page widget: here it expanded between the heading and footer,
        // and a newly installed icon theme could leave its oversized icon as a
        // blurry missing-image placeholder until the desktop reloaded caches.
        let empty = gtk::Box::new(gtk::Orientation::Vertical, 0);
        empty.add_css_class("card");
        let empty_content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        empty_content.set_margin_top(24);
        empty_content.set_margin_bottom(24);
        empty_content.set_margin_start(28);
        empty_content.set_margin_end(28);
        let empty_icon = gtk::Image::builder()
            .icon_name("folder-remote-symbolic")
            .pixel_size(48)
            .halign(gtk::Align::Center)
            .build();
        empty_icon.add_css_class("dim-label");
        let empty_title = gtk::Label::builder()
            .label(gettext("Loading connections…"))
            .xalign(0.5)
            .wrap(true)
            .build();
        empty_title.add_css_class("title-2");
        let empty_description = gtk::Label::builder()
            .label(gettext("Reading your saved accounts and service status."))
            .xalign(0.5)
            .justify(gtk::Justification::Center)
            .max_width_chars(54)
            .wrap(true)
            .build();
        empty_description.add_css_class("dim-label");
        empty_content.append(&empty_icon);
        empty_content.append(&empty_title);
        empty_content.append(&empty_description);
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
        empty_content.append(&empty_actions);
        empty.append(&empty_content);
        body.append(&empty);
        let help = gtk::Box::new(gtk::Orientation::Horizontal, 16);
        let onedrive_help = gtk::LinkButton::with_label(
            "https://github.com/Dandiccf/cirrove/blob/main/docs/onedrive-setup.md",
            &gettext("OneDrive setup guide"),
        );
        let google_help = gtk::LinkButton::with_label(
            "https://github.com/Dandiccf/cirrove/blob/main/docs/google-drive.md#connect",
            &gettext("Google Drive setup guide"),
        );
        help.set_halign(gtk::Align::Start);
        if matches!(backend, Backend::Demo) {
            onedrive_help.set_sensitive(false);
            google_help.set_sensitive(false);
        }
        help.append(&onedrive_help);
        help.append(&google_help);
        body.append(&help);
        let footer = gtk::Label::builder()
            .label(gettext(if matches!(backend, Backend::Demo) {
                n("Interface preview · sample accounts")
            } else {
                n("Files download when opened · changes upload in the background")
            }))
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
            empty_icon,
            empty_title,
            empty_description,
            settings_retry,
            empty_connect,
            banner,
            tray_notice,
            restart_notice,
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
    /// Ask the session bus whether a tray host is running, and show or hide the
    /// notice. A bus that cannot be asked says nothing: not knowing is not the
    /// same as knowing there is none.
    fn check_tray_host(self: &Rc<Self>) {
        let Backend::Live { runtime, .. } = &self.backend else {
            return;
        };
        let (send, receive) = tokio::sync::oneshot::channel();
        runtime.spawn(async move {
            let _ = send.send(crate::tray::host_present().await);
        });
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(answer) = receive.await else { return };
            let Some(ui) = weak.upgrade().filter(|ui| !ui.closed.get()) else {
                return;
            };
            ui.tray_notice
                .set_revealed(crate::tray::notice_when(answer));
        });
    }
    pub fn refresh(self: &Rc<Self>) {
        if self.closed.get() || self.operation.borrow().is_some() || self.refreshing.replace(true) {
            return;
        }
        self.check_tray_host();
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
                        ui.notify(&gettext("Could not read connection status. Try again."));
                    }
                });
            }
        }
    }
    pub fn render(self: &Rc<Self>, overview: Overview) {
        self.restart_notice.set_revealed(overview.restart_required);
        self.banner.set_revealed(overview.service_error.is_some());
        if let Some(error) = &overview.service_error {
            self.banner.set_title(&gettext(error.description()));
        }
        self.empty
            .set_visible(!overview.settings_available || overview.accounts.is_empty());
        self.group
            .set_visible(overview.settings_available && !overview.accounts.is_empty());
        self.empty_title
            .set_label(&gettext(if overview.settings_available {
                n("No drives connected")
            } else {
                n("Account settings unavailable")
            }));
        self.empty_icon
            .set_icon_name(Some(if overview.settings_available {
                "folder-remote-symbolic"
            } else {
                "dialog-warning-symbolic"
            }));
        let description = overview.settings_error.as_ref().map_or_else(
            || gettext("Connect a cloud drive and its files appear in Files, downloaded as you open them."),
            |error| gettext(error.description()),
        );
        self.empty_description.set_label(&description);
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
                .subtitle(
                    match entry
                        .at_unix
                        .and_then(|at| crate::model::how_long_ago(now_unix(), at))
                    {
                        Some(when) => fill(
                            &gettext("{} · {} · {}"),
                            &[&entry.account, &gettext(&entry.what), &when],
                        ),
                        None => fill(
                            &gettext("{} · {}"),
                            &[&entry.account, &gettext(&entry.what)],
                        ),
                    },
                )
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
            .label(gettext("Sign in again"))
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        sign_in.add_css_class("suggested-action");
        row.add_suffix(&sign_in);
        let mount = gtk::Button::builder()
            .label(gettext("Mount"))
            .valign(gtk::Align::Center)
            .build();
        row.add_suffix(&mount);
        let status = adw::ActionRow::builder()
            .title(gettext("Connection status"))
            .use_markup(false)
            .subtitle_lines(0)
            .build();
        let location = adw::ActionRow::builder()
            .title(gettext("Location in Files"))
            .use_markup(false)
            .subtitle_selectable(true)
            .subtitle_lines(2)
            .build();
        let identity = adw::ActionRow::builder()
            .title(gettext("Cloud account"))
            .use_markup(false)
            .subtitle_selectable(true)
            .subtitle_lines(2)
            .build();
        let access = adw::ActionRow::builder()
            .title(gettext("Access"))
            .use_markup(false)
            .subtitle_lines(0)
            .build();
        // Changing an account's consent was the last ordinary flow that needed a
        // terminal (`cirrove reauth --write-access`). It is a re-sign-in either
        // way, so the provider's own consent screen is what actually grants or
        // narrows the access; this button only asks for it.
        let consent = gtk::Button::builder()
            .label(gettext("Allow changes"))
            .valign(gtk::Align::Center)
            .build();
        access.add_suffix(&consent);
        let storage = adw::ActionRow::builder()
            .title(gettext("Local cache"))
            .use_markup(false)
            .build();
        // Shown only while there is something to discard. The daemon has
        // stopped retrying these; the user decides what happens to the copies.
        let refused = adw::ActionRow::builder()
            .title(gettext("Changes the cloud refused"))
            .use_markup(false)
            .subtitle_lines(0)
            .visible(false)
            .build();
        refused.add_css_class("warning");
        // Two answers to a refused change, and they are not the same answer.
        // Trying again is what a failure deserves; discarding is what a
        // conflict leaves. The order puts the recoverable one first.
        let retry = gtk::Button::builder()
            .label(gettext("Try again"))
            .valign(gtk::Align::Center)
            .build();
        let discard = gtk::Button::builder()
            .label(gettext("Discard"))
            .valign(gtk::Align::Center)
            .build();
        discard.add_css_class("destructive-action");
        refused.add_suffix(&retry);
        refused.add_suffix(&discard);
        let kept = adw::ExpanderRow::builder()
            .title(gettext("Kept offline"))
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
            .label(gettext("File…"))
            .halign(gtk::Align::Fill)
            .build();
        keep_file.add_css_class("flat");
        let keep_folder = gtk::Button::builder()
            .label(gettext("Folder…"))
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
            .tooltip_text(gettext("Keep a file or folder offline"))
            .valign(gtk::Align::Center)
            .popover(&popover)
            .build();
        keep_add.add_css_class("flat");
        keep_add.update_property(&[gtk::accessible::Property::Label(
            "Keep a file or folder offline",
        )]);
        kept.add_suffix(&keep_add);

        // Not a warning and not an offer to fix it. A folder in somebody's drive
        // is theirs; the mount stopped anything being put in this one and will
        // not take it away. Saying it is there is what was missing.
        let wastebasket = adw::ActionRow::builder()
            .title(gettext("A wastebasket folder is in this drive"))
            .use_markup(false)
            .subtitle_lines(0)
            .visible(false)
            .build();
        // Deliberately not beside "Keep offline": a surface that puts keeping
        // and destroying next to each other invites the wrong click. It sits
        // with the other destructive thing instead, and asks twice.
        let destruction = adw::ActionRow::builder()
            .title(gettext("Delete a file permanently"))
            .subtitle(gettext(
                "Skips the drive's recycle bin, so there is no way back. An ordinary delete in any file manager is recoverable and stays that way.",
            ))
            .use_markup(false)
            .subtitle_lines(0)
            .build();
        let destroy = gtk::Button::builder()
            .label(gettext("Choose a file…"))
            .valign(gtk::Align::Center)
            .build();
        destroy.add_css_class("destructive-action");
        destruction.add_suffix(&destroy);
        let removal = adw::ActionRow::builder()
            .title(gettext("Remove this connection"))
            .subtitle(gettext(
                "Its sign-in and local index are set aside; nothing in the cloud is touched.",
            ))
            .use_markup(false)
            .subtitle_lines(0)
            .build();
        let remove = gtk::Button::builder()
            .label(gettext("Remove"))
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
        // did not reach the cloud. Its remedy is different too, and so is its
        // one button: discarding a refused folder removal loses nothing, and
        // discarding a failed save loses what the person wrote, so this row
        // offers to keep both copies rather than to throw one away.
        let unsent = adw::ActionRow::builder()
            .title(gettext("Saves that did not reach the cloud"))
            .use_markup(false)
            .subtitle_lines(0)
            .visible(false)
            .build();
        unsent.add_css_class("warning");
        let keep_both = gtk::Button::builder()
            .label(gettext("Keep both copies"))
            .valign(gtk::Align::Center)
            .build();
        unsent.add_suffix(&keep_both);
        row.add_row(&refused);
        row.add_row(&unsent);
        row.add_row(&wastebasket);
        row.add_row(&destruction);
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
        retry.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.retry_refused(&key);
            }
        });
        let weak = Rc::downgrade(self);
        let key = id.to_owned();
        keep_both.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.keep_both_saves(&key);
            }
        });
        let weak = Rc::downgrade(self);
        let key = id.to_owned();
        destroy.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.choose_file_to_destroy(&key);
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
            retry,
            unsent,
            keep_both,
            destroy,
            wastebasket,
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
        let state = gettext(if let Some(operation) = writing {
            operation.text
        } else if late {
            n("Still waiting for service")
        } else {
            card.state.label()
        });
        row.row
            .set_subtitle(&fill(&gettext("{} · {}"), &[&card.username, &state]));
        row.status.set_subtitle(&if late {
            gettext(
                "The request is saved, but the service has not confirmed it. You can change the mount preference or check the service.",
            )
        } else {
            gettext(card.state.description())
        });
        if card.state.warning() || late {
            row.status.add_css_class("warning");
        } else {
            row.status.remove_css_class("warning");
        }
        row.mount.set_label(&gettext(card.action_label()));
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
        row.identity
            .set_title(&if card.provider_id == "googledrive" {
                gettext("Google account")
            } else {
                gettext("Microsoft account")
            });
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
        row.consent.set_tooltip_text(Some(if card.writable && card.provider_id == "googledrive" {
            "Sign in again asking only to read. Cirrove stops making changes; withdraw the earlier permission separately in your Google Account"
        } else if card.writable {
            // Deliberately about what Cirrove will do, not about what the token
            // can do. Asking for the narrower scope does not take the wider one
            // away: where the account has already consented to it, the provider
            // may return it again, and only the person can withdraw it in their
            // provider account. Saying the permission is gone would be a
            // stronger promise than this button keeps.
            "Sign in again asking only to read. Cirrove stops making changes; withdraw the earlier permission separately in your Microsoft account"
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
        row.consent.set_visible(card.supports_writes);
        row.consent
            .set_sensitive(idle && card.controls_available && card.supports_writes);
        row.storage.set_subtitle(&fill(
            &gettext("Up to {} GiB · downloaded as needed"),
            &[&format!(
                "{:.1}",
                card.cache_bytes as f64 / 1024_f64.powi(3)
            )],
        ));
        row.refused.set_visible(card.stuck > 0);
        // The count, then which ones. A person told that two changes were
        // refused and not which cannot do anything about either -- the fourteen
        // folder removals that went missing on a live drive were a number on a
        // screen and nothing else.
        let counted = if card.stuck == 1 {
            gettext("1 change")
        } else {
            fill(&gettext("{} changes"), &[&card.stuck.to_string()])
        };
        // "They will not be retried" was true of all of them and is now true of
        // only some: a change that failed is one the cloud never decided about.
        let mut refused = if card.retryable == 0 {
            fill(
                &gettext(
                    "{} that the cloud would not accept. The cloud has decided about these, so they are not re-sent; discarding removes the local copies and the cloud keeps its version.",
                ),
                &[&counted],
            )
        } else if card.retryable == card.stuck {
            fill(
                &gettext(
                    "{} that did not reach the cloud. Try again sends them once more; discarding removes the local copies and the cloud keeps its version.",
                ),
                &[&counted],
            )
        } else {
            fill(
                &gettext(
                    "{} that the cloud would not accept, {} of which are worth trying again. The rest the cloud has decided about and are not re-sent; discarding removes the local copies and the cloud keeps its version.",
                ),
                &[&counted, &card.retryable.to_string()],
            )
        };
        if !card.refused_paths.is_empty() {
            let shown: Vec<&str> = card
                .refused_paths
                .iter()
                .take(3)
                .map(String::as_str)
                .collect();
            refused.push('\n');
            refused.push_str(&shown.join(", "));
            // An older daemon names none of them, and a very long list would
            // push the row off the window; either way the count above still
            // says how many there are.
            if card.refused_paths.len() > shown.len() {
                refused.push_str(&fill(
                    &gettext(", and {} more"),
                    &[&(card.refused_paths.len() - shown.len()).to_string()],
                ));
            }
        }
        row.refused.set_subtitle(&refused);
        row.discard.set_sensitive(idle);
        row.retry.set_sensitive(idle && card.retryable > 0);
        row.retry.set_tooltip_text(Some(if card.retryable > 0 {
            "Send these to the cloud again"
        } else {
            "The cloud has decided about these; sending them again would act on what is there now"
        }));
        row.unsent.set_visible(card.failed_uploads > 0);
        let mut unsent = fill(
            &gettext(
                "{} did not reach the cloud. The file is on this computer; the cloud has an older version or none. Open the file and save it again to try once more.",
            ),
            &[&if card.failed_uploads == 1 {
                gettext("1 save")
            } else {
                fill(&gettext("{} saves"), &[&card.failed_uploads.to_string()])
            }],
        );
        // Which file, not just how many. Without this the row is a warning sign
        // a person cannot act on: the owner met exactly that on 2026-09-16 and
        // asked what the triangle meant, and answering it needed the journal
        // copied off the machine and an item id resolved by hand.
        if !card.failed_paths.is_empty() {
            let shown: Vec<&str> = card
                .failed_paths
                .iter()
                .take(3)
                .map(String::as_str)
                .collect();
            unsent.push('\n');
            unsent.push_str(&shown.join(", "));
            if card.failed_paths.len() > shown.len() {
                unsent.push_str(&fill(
                    &gettext(", and {} more"),
                    &[&(card.failed_paths.len() - shown.len()).to_string()],
                ));
            }
        }
        row.unsent.set_subtitle(&unsent);
        row.wastebasket.set_visible(card.wastebasket.is_some());
        if let Some(name) = &card.wastebasket {
            row.wastebasket.set_subtitle(&fill(
                &gettext(
                    "{} was made by a file manager before Cirrove refused to hold one, and whatever is in it is still in your cloud drive. Nothing can be added to it now. Removing it is yours to decide; Cirrove will not.",
                ),
                &[name],
            ));
        }
        row.destroy
            .set_sensitive(idle && card.mounted && card.writable);
        row.keep_both.set_sensitive(idle && card.failed_uploads > 0);
        row.keep_both.set_tooltip_text(Some(&gettext(
            "Put your version beside the cloud's, under a new name, instead of losing one of them",
        )));
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
        // What is arriving comes before what has arrived: it is the part that
        // is still happening, and the part with a button that means something.
        for job in &card.running {
            let entry = adw::ActionRow::builder()
                .title(&job.name)
                .subtitle(&job.detail)
                .use_markup(false)
                .subtitle_lines(0)
                .build();
            if job.failed {
                entry.add_css_class("warning");
            }
            if job.running {
                let bar = gtk::ProgressBar::builder()
                    .valign(gtk::Align::Center)
                    .width_request(120)
                    .show_text(false)
                    .build();
                match job.percent {
                    Some(percent) => bar.set_fraction(f64::from(percent) / 100.0),
                    // A total nobody knows is a bar that must move rather than
                    // sit at zero, which reads as "nothing is happening".
                    None => bar.pulse(),
                }
                bar.update_property(&[gtk::accessible::Property::Label(&job.detail)]);
                entry.add_suffix(&bar);
            }
            let stop = gtk::Button::builder()
                // The same button whether the work is running or over: one asks
                // it to stop, the other clears a record nobody needs any more.
                .label(if job.running {
                    gettext("Stop")
                } else {
                    gettext("Dismiss")
                })
                .valign(gtk::Align::Center)
                .sensitive(idle)
                .build();
            stop.add_css_class("flat");
            let weak = Rc::downgrade(self);
            let key = card.id.clone();
            let id = job.id.clone();
            let name = job.name.clone();
            let running = job.running;
            stop.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.stop_job(&key, &id, &name, running);
                }
            });
            entry.add_suffix(&stop);
            row.kept.add_row(&entry);
            rows.push(entry);
        }
        for pin in &card.kept_offline {
            let entry = adw::ActionRow::builder()
                .title(&pin.name)
                .subtitle(if pin.recursive {
                    fill(&gettext("{} · everything inside it"), &[&pin.detail])
                } else {
                    pin.detail.clone()
                })
                .use_markup(false)
                .subtitle_lines(0)
                .build();
            let stop = gtk::Button::builder()
                .label(gettext("Stop keeping"))
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
        // The summary of what is running belongs where it can be read without
        // opening anything: a progress bar inside a collapsed expander is a
        // progress bar nobody sees.
        let summary = match card.running.iter().find(|job| job.running) {
            Some(job) => fill(
                &gettext("Keeping {} offline · {}"),
                &[&job.name, &job.detail],
            ),
            None => match (&card.pin_budget, card.kept_offline.len()) {
                // The budget sentence is the useful one, because the question a
                // person opens this for is how much room is left.
                (Some(budget), _) => budget.clone(),
                (None, 0) => "Nothing is kept offline yet.".to_owned(),
                (None, 1) => "1 item kept offline.".to_owned(),
                (None, n) => fill(&gettext("{} items kept offline."), &[&n.to_string()]),
            },
        };
        row.kept.set_subtitle(&summary);
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
        if !self.begin_operation(id, n("Saving mount preference…")) {
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
                    let _ = send.send(accounts::set_enabled_by_id(&state, &id, enabled));
                });
                let weak = Rc::downgrade(self);
                glib::spawn_future_local(async move {
                    let result = receive.await;
                    let Some(ui) = weak.upgrade().filter(|ui| !ui.closed.get()) else {
                        return;
                    };
                    ui.end_operation();
                    // Four causes with four remedies, where there used to be
                    // one sentence guessing at the least likely of them.
                    match result {
                        Ok(Ok(())) => {}
                        Ok(Err(refusal)) => ui.notify(&gettext(match refusal {
                            accounts::PreferenceRefusal::Busy => n(
                                "Cirrove is busy with this drive. Wait a moment and try again.",
                            ),
                            accounts::PreferenceRefusal::NotConfigured => n(
                                "This connection is no longer configured. It may have been removed in another window.",
                            ),
                            accounts::PreferenceRefusal::Unreadable => n(
                                "Your saved connections could not be read, so the change was not made. Check the settings location, then try again.",
                            ),
                            accounts::PreferenceRefusal::Unwritable => n(
                                "The change could not be saved. Check that the settings location is writable and has space, then try again.",
                            ),
                        })),
                        // The worker died before answering: not a refusal, and
                        // saying it was would send someone looking in the wrong
                        // place.
                        Err(_) => ui.notify(&gettext(
                            "The task saving this change stopped unexpectedly. Retry or reopen Cirrove.",
                        )),
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
    /// current one kept. Google's in-app data notice immediately precedes its
    /// browser consent; Microsoft still goes straight to its consent screen.
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
        if card.provider_id == "googledrive" {
            let Some(window) = self.window.upgrade() else {
                return;
            };
            let mode = access.unwrap_or(if card.writable {
                cirrove_auth::AccessMode::ReadWrite
            } else {
                cirrove_auth::AccessMode::ReadOnly
            });
            let grant = if mode == cirrove_auth::AccessMode::ReadWrite {
                gettext("read and write")
            } else {
                gettext("read-only")
            };
            let dialog = adw::AlertDialog::new(
                Some(&gettext("Continue to Google sign-in?")),
                Some(&fill(
                    &gettext(
                        "Google grants {} access to Drive files across your account. Cirrove mounts only the drive you chose. Your name, email, file names, metadata and opened content are stored locally; credentials stay in your desktop keyring. With write access, ordinary-file edits go directly to Google, not to a Cirrove server. Removing this connection retains recoverable local data until you discard it.",
                    ),
                    &[&grant],
                )),
            );
            dialog.add_response("cancel", &gettext("Cancel"));
            dialog.add_response("continue", &gettext("Continue to Google"));
            dialog.set_default_response(Some("cancel"));
            dialog.set_close_response("cancel");
            let weak = Rc::downgrade(self);
            let id = id.to_owned();
            dialog.choose(Some(&window), gtk::gio::Cancellable::NONE, move |answer| {
                if answer == "continue"
                    && let Some(ui) = weak.upgrade()
                {
                    ui.start_reauthentication(&id, access);
                }
            });
            return;
        }
        self.start_reauthentication(id, access);
    }
    fn start_reauthentication(self: &Rc<Self>, id: &str, access: Option<cirrove_auth::AccessMode>) {
        let Some(card) = self.card(id) else {
            return;
        };
        let Backend::Live { runtime, state, .. } = &self.backend else {
            return;
        };
        if !self.begin_operation(id, n("Signing in in your browser…")) {
            return;
        }
        let state = state.clone();
        let label = card.label.clone();
        let google = card.provider_id == "googledrive";
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
                        if google {
                            "Signed in. Cirrove will not change anything in this drive. To withdraw the earlier permission, remove Cirrove's access in your Google Account."
                        } else {
                            "Signed in. Cirrove will not change anything in this drive. To withdraw the earlier permission, remove Cirrove's access in your Microsoft account."
                        }
                    }
                    None => "Signed in again.",
                }),
                Ok(Err(error)) => ui.notify(&fill(&gettext("The sign-in did not finish: {}"), &[&error.to_string()])),
                Err(_) => ui.notify(&gettext("The sign-in did not finish.")),
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
        if !self.begin_operation(id, n("Releasing…")) {
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
                    Some(refusal) => ui.notify(&fill(
                        &gettext("{} is still kept offline: {}"),
                        &[&shown, &refusal],
                    )),
                    None if reply.accepted => {
                        ui.notify(&fill(&gettext("{} is no longer kept offline."), &[&shown]));
                    }
                    None => ui.notify(&fill(&gettext("{} was not kept offline."), &[&shown])),
                },
                Ok(Err(error)) => ui.notify(&fill(
                    &gettext("Could not release {}: {}"),
                    &[&shown, &error.to_string()],
                )),
                Err(_) => ui.notify(&gettext("The service did not answer.")),
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
    /// Pick a file to remove without the recycle bin, then ask again (ADR 0008).
    ///
    /// Files only. The daemon refuses a folder, and offering a folder chooser
    /// here would be an invitation to be told no.
    pub fn choose_file_to_destroy(self: &Rc<Self>, id: &str) {
        let Some(card) = self.card(id).filter(|c| c.mounted && c.writable) else {
            return;
        };
        let Backend::Live { .. } = &self.backend else {
            return;
        };
        let chooser = gtk::FileDialog::builder()
            .title(gettext("Delete a file permanently"))
            .accept_label(gettext("Choose"))
            .initial_folder(&gtk::gio::File::for_path(&card.mount_path))
            .modal(true)
            .build();
        let weak = Rc::downgrade(self);
        let key = id.to_owned();
        let window = self.window.upgrade();
        chooser.open(
            window.as_ref(),
            gtk::gio::Cancellable::NONE,
            move |result| {
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
                ui.confirm_destruction(&key, &path);
            },
        );
    }

    /// The second question. Choosing a file in a chooser is not consent to
    /// destroy it, and the daemon will not act until a client says it asked.
    fn confirm_destruction(self: &Rc<Self>, id: &str, path: &std::path::Path) {
        let Some(card) = self.card(id) else {
            return;
        };
        let Some(window) = self.window.upgrade() else {
            return;
        };
        if path.is_dir() {
            self.notify(&gettext(
                "A folder cannot be deleted permanently: the cloud's delete is recursive and nothing can tell whether a file arrived in it a moment ago.",
            ));
            return;
        }
        let Ok(relative) = path.strip_prefix(&card.mount_path) else {
            self.notify(&gettext("Choose a file inside this drive's folder."));
            return;
        };
        let relative = relative.to_string_lossy().into_owned();
        let dialog = adw::AlertDialog::new(
            Some(&fill(&gettext("Delete {} permanently?"), &[&relative])),
            Some(&gettext(
                "It does not go to the drive's recycle bin, and it cannot be recovered from this computer or from the cloud. Deleting it the ordinary way would be recoverable.",
            )),
        );
        dialog.add_response("cancel", &gettext("Cancel"));
        dialog.add_response("delete", &gettext("Delete permanently"));
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        let key = id.to_owned();
        dialog.choose(Some(&window), gtk::gio::Cancellable::NONE, move |answer| {
            if answer != "delete" {
                return;
            }
            if let Some(ui) = weak.upgrade() {
                ui.destroy_path(&key, &relative);
            }
        });
    }

    /// The half that needs no dialogue, so a test can reach it.
    pub fn destroy_path(self: &Rc<Self>, id: &str, relative: &str) {
        let Some(card) = self.card(id) else {
            return;
        };
        let Backend::Live {
            runtime, socket, ..
        } = &self.backend
        else {
            return;
        };
        if !self.begin_operation(id, n("Deleting permanently…")) {
            return;
        }
        let socket = socket.clone();
        let label = card.label.clone();
        let paths = vec![relative.to_owned()];
        let (send, receive) = tokio::sync::oneshot::channel();
        runtime.spawn(async move {
            let result = cirrove_service::delete_permanently(&socket, &label, paths)
                .await
                .map_err(|error| format!("{error:#}"));
            let _ = send.send(result);
        });
        let weak = Rc::downgrade(self);
        let shown = relative.to_owned();
        glib::spawn_future_local(async move {
            let result = receive.await;
            let Some(ui) = weak.upgrade().filter(|ui| !ui.closed.get()) else {
                return;
            };
            ui.end_operation();
            match result {
                Ok(Ok(reply)) => match (&reply.refusal, reply.deletions.first()) {
                    (Some(refusal), _) => ui.notify(refusal),
                    (None, Some(one)) => match &one.refusal {
                        Some(why) => ui.notify(why),
                        None => ui.notify(&fill(
                            &gettext("{} is gone. It was not put in the recycle bin."),
                            &[&shown],
                        )),
                    },
                    (None, None) => ui.notify(&gettext("Nothing was removed.")),
                },
                Ok(Err(error)) => ui.notify(&fill(
                    &gettext("Could not delete permanently: {}"),
                    &[&error.to_string()],
                )),
                Err(_) => ui.notify(&gettext("The service did not answer.")),
            }
            ui.refresh();
        });
    }
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
            self.notify(&gettext("Choose a file inside this drive's folder."));
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
        if !self.begin_operation(id, n("Keeping offline…")) {
            return;
        }
        let key = id.to_owned();
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
                    None if !reply.accepted => ui.notify(&fill(&gettext("{} was not kept offline."), &[&relative])),
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
                Err(_) => ui.notify(&gettext("The service did not answer.")),
            }
            // Open the section the fetch will report into. Expanding a section
            // on a timer would fight whoever collapsed it; this is the one
            // moment it is the person's own doing.
            if let Some(row) = ui.rows.borrow().get(&key) {
                row.kept.set_expanded(true);
            }
            ui.refresh();
        });
    }
    /// Stop work that is running, or clear the record of work that ended.
    ///
    /// Stopping a fetch releases the pin it was filling: someone who stopped it
    /// did not ask to keep part of a folder, and a pin reserving the whole of
    /// one while holding part misreports both. The daemon does that; this says
    /// so, because a button whose effect is larger than its label is a trap.
    pub fn stop_job(self: &Rc<Self>, id: &str, job: &str, name: &str, running: bool) {
        let Some(card) = self.card(id) else {
            return;
        };
        let Backend::Live {
            runtime, socket, ..
        } = &self.backend
        else {
            return;
        };
        if !self.begin_operation(id, n("Stopping…")) {
            return;
        }
        let socket = socket.clone();
        let request = cirrove_service::StopJobRequest {
            label: card.label.clone(),
            id: job.to_owned(),
        };
        let shown = name.to_owned();
        let (send, receive) = tokio::sync::oneshot::channel();
        runtime.spawn(async move {
            let result = cirrove_service::stop_job(&socket, &request)
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
                Ok(Ok(reply)) => match (&reply.refusal, reply.stopped, running) {
                    (Some(refusal), _, _) => ui.notify(refusal),
                    (None, true, true) => ui.notify(&fill(
                        &gettext("Stopping. {} will not be kept offline."),
                        &[&shown],
                    )),
                    // Not finding it is the outcome the person wanted: it is not
                    // running. Saying "nothing by that name" would read as a
                    // failure of the button rather than as work that finished.
                    (None, _, _) => ui.refresh(),
                },
                Ok(Err(error)) => ui.notify(&fill(
                    &gettext("Could not stop {}: {}"),
                    &[&shown, &error.to_string()],
                )),
                Err(_) => ui.notify(&gettext("The service did not answer.")),
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
        if !self.begin_operation(id, n("Discarding refused changes…")) {
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
                Err(_) => ui.notify(&gettext("Could not discard the refused changes.")),
            }
            ui.refresh();
        });
    }
    /// Ask the daemon to try the refused changes again.
    ///
    /// Only the ones worth trying: a change that failed is one the cloud never
    /// decided about, and a change in conflict is one it did. The button is
    /// insensitive when every refusal is the second kind, and the answer says
    /// how many were left alone, because a person who pressed "Try again" and
    /// saw nothing move deserves to know why.
    /// Keep both copies of every save the cloud refused.
    ///
    /// The other two answers to a refusal each cost something: trying again
    /// sends the person's version over whatever the cloud has now, and
    /// discarding throws the person's version away. This one costs nothing --
    /// their bytes are still in the journal, sealed and checked against their
    /// digest, so they go beside the cloud's version under a new name and the
    /// person decides afterwards, with both in front of them.
    pub fn keep_both_saves(self: &Rc<Self>, id: &str) {
        let Some(card) = self.card(id).filter(|c| c.failed_uploads > 0) else {
            return;
        };
        let Backend::Live {
            runtime, socket, ..
        } = &self.backend
        else {
            return;
        };
        if !self.begin_operation(id, n("Keeping both copies…")) {
            return;
        }
        let socket = socket.clone();
        let label = card.label.clone();
        let (send, receive) = tokio::sync::oneshot::channel();
        runtime.spawn(async move {
            let result = cirrove_service::keep_both(&socket, &label)
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
                Ok(Ok(reply)) => {
                    let missed = reply.considered.saturating_sub(reply.kept);
                    match (&reply.refusal, reply.kept, missed) {
                        (Some(refusal), _, _) => ui.notify(refusal),
                        (None, kept, 0) => ui.notify(&fill(
                            &gettext("Copying {} save(s) to the cloud beside the version already there."),
                            &[&kept.to_string()],
                        )),
                        // The exceptions are named rather than swallowed: the
                        // person is being told their work is safe.
                        (None, kept, missed) => ui.notify(&fill(
                            &gettext("Copying {} save(s) beside the version already there. {} could not be copied, because the file each was replacing is no longer in the drive."),
                            &[&kept.to_string(), &missed.to_string()],
                        )),
                    }
                }
                Ok(Err(error)) => ui.notify(&fill(
                    &gettext("Could not keep both copies: {}"),
                    &[&error.to_string()],
                )),
                Err(_) => ui.notify(&gettext("The service did not answer.")),
            }
            ui.refresh();
        });
    }
    pub fn retry_refused(self: &Rc<Self>, id: &str) {
        let Some(card) = self.card(id).filter(|c| c.retryable > 0) else {
            return;
        };
        let Backend::Live {
            runtime, socket, ..
        } = &self.backend
        else {
            return;
        };
        if !self.begin_operation(id, n("Trying the refused changes again…")) {
            return;
        }
        let socket = socket.clone();
        let label = card.label.clone();
        let (send, receive) = tokio::sync::oneshot::channel();
        runtime.spawn(async move {
            let result = cirrove_service::retry_stuck(&socket, &label)
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
                Ok(Ok(reply)) => match (&reply.refusal, reply.queued, reply.conflicts) {
                    (Some(refusal), _, _) => ui.notify(refusal),
                    (None, queued, 0) => ui.notify(&fill(
                        &gettext("Trying {} change(s) again."),
                        &[&queued.to_string()],
                    )),
                    // One line, no `\` continuation: xgettext reads these as C,
                    // where a spliced line keeps the indentation that follows it,
                    // and the message id would carry eighteen spaces no
                    // translation could ever match.
                    (None, queued, conflicts) => ui.notify(&fill(
                        &gettext("Trying {} change(s) again. {} the cloud already decided about; those are not re-sent, because the file has moved on since. Discard them to let the drive show what the cloud has."),
                        &[&queued.to_string(), &conflicts.to_string()],
                    )),
                },
                Ok(Err(error)) => ui.notify(&fill(
                    &gettext("Could not try again: {}"),
                    &[&error.to_string()],
                )),
                Err(_) => ui.notify(&gettext("The service did not answer.")),
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
                Err(_) => ui.notify(&gettext("Could not check this connection.")),
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
        if !self.begin_operation(id, n("Removing…")) {
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
                Err(_) => ui.notify(&gettext("Could not remove the connection.")),
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
            let Err(error) = launcher.launch_future(Some(&window)).await else {
                return;
            };
            let Some(ui) = weak.upgrade() else { return };
            // A person dismissing the application chooser is not a failure and
            // must not be reported as one; it was, until now. The other two
            // cases have different remedies, and "check the connection" was
            // the wrong advice for both -- the mount is local.
            if error.matches(gtk::gio::IOErrorEnum::Cancelled) {
                return;
            }
            ui.notify(&gettext(if error.matches(gtk::gio::IOErrorEnum::NotFound) {
                n("This drive's folder is not there. It may have been unmounted; refresh and try again.")
            } else {
                n("No application is set up to open a folder on this desktop. Install a file manager, or open the folder path yourself.")
            }));
        });
    }
    fn notify(&self, message: &str) {
        self.toast.add_toast(adw::Toast::new(message));
    }
}
