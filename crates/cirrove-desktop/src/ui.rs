//! GTK widgets and asynchronous desktop controller.
use crate::model::{AccountCard, ConnectionState, Overview};
use adw::prelude::*;
use gtk::{gio, glib};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

#[derive(Clone)]
pub enum Backend {
    Live {
        runtime: tokio::runtime::Handle,
        state: PathBuf,
        socket: PathBuf,
    },
    Demo,
}
struct AccountRow {
    row: adw::ExpanderRow,
    mount: gtk::Button,
    open: gtk::Button,
    spinner: gtk::Spinner,
    status: adw::ActionRow,
    location: adw::ActionRow,
    storage: adw::ActionRow,
    identity: adw::ActionRow,
}
pub struct Window {
    pub window: glib::WeakRef<adw::ApplicationWindow>,
    backend: Backend,
    group: adw::PreferencesGroup,
    empty: adw::StatusPage,
    banner: adw::Banner,
    refresh_button: gtk::Button,
    toast: adw::ToastOverlay,
    rows: RefCell<HashMap<String, AccountRow>>,
    overview: RefCell<Option<Overview>>,
    refreshing: Cell<bool>,
    operation: RefCell<Option<String>>,
    operation_focus: RefCell<Option<glib::WeakRef<gtk::Widget>>>,
    generation: Cell<u64>,
    waiting: RefCell<HashMap<String, Instant>>,
    closed: Cell<bool>,
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
        let refresh_button = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Refresh connection status")
            .build();
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
        let empty = adw::StatusPage::builder()
            .icon_name("folder-remote-symbolic")
            .title("Loading connections…")
            .description("Reading your saved accounts and service status.")
            .build();
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
                "Development preview · mounted files are read-only"
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
            empty,
            banner,
            refresh_button,
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
        let weak = Rc::downgrade(&ui);
        ui.banner.connect_button_clicked(move |_| {
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
        self.banner.set_revealed(!overview.service_reachable);
        self.empty
            .set_visible(!overview.settings_available || overview.accounts.is_empty());
        self.group
            .set_visible(overview.settings_available && !overview.accounts.is_empty());
        self.empty.set_title(if overview.settings_available {
            "No drives connected"
        } else {
            "Account settings unavailable"
        });
        self.empty.set_description(Some(if overview.settings_available {
            "Use the OneDrive setup guide to connect your first account."
        } else { "Cirrove could not read your saved connections. Check the settings directory and retry." }));
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
    fn account_row(self: &Rc<Self>, id: &str) -> AccountRow {
        let row = adw::ExpanderRow::builder().use_markup(false).build();
        let icon = gtk::Image::from_icon_name("folder-remote-symbolic");
        icon.set_pixel_size(32);
        icon.add_css_class("accent");
        row.add_prefix(&icon);
        let spinner = gtk::Spinner::new();
        row.add_suffix(&spinner);
        let open = gtk::Button::builder()
            .icon_name("folder-open-symbolic")
            .valign(gtk::Align::Center)
            .tooltip_text("Open in Files")
            .build();
        open.add_css_class("flat");
        row.add_suffix(&open);
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
        let storage = adw::ActionRow::builder()
            .title("Local cache")
            .use_markup(false)
            .build();
        row.add_row(&status);
        row.add_row(&identity);
        row.add_row(&location);
        row.add_row(&storage);
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
        AccountRow {
            row,
            mount,
            open,
            spinner,
            status,
            location,
            identity,
            storage,
        }
    }
    fn update_row(&self, row: &AccountRow, card: &AccountCard, late: bool) {
        row.row.set_title(&card.title);
        let writing = self.operation.borrow().as_deref() == Some(&card.id);
        let state = if writing {
            "Saving mount preference…"
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
        row.mount
            .set_sensitive(card.controls_available && self.operation.borrow().is_none());
        row.open
            .set_sensitive(card.mounted && matches!(self.backend, Backend::Live { .. }));
        row.spinner.set_spinning(writing || card.state.busy());
        row.spinner.set_visible(writing || card.state.busy());
        row.location
            .set_subtitle(&card.mount_path.to_string_lossy());
        row.identity.set_subtitle(&card.username);
        row.storage.set_subtitle(&format!(
            "Up to {:.1} GiB · downloaded as needed",
            card.cache_bytes as f64 / 1024_f64.powi(3)
        ));
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
    pub fn toggle(self: &Rc<Self>, id: &str) {
        let Some(card) = self.card(id).filter(|a| a.controls_available) else {
            return;
        };
        if self.operation.borrow().is_some() || self.closed.get() {
            return;
        }
        self.generation.set(self.generation.get().wrapping_add(1));
        *self.operation_focus.borrow_mut() = self
            .window
            .upgrade()
            .and_then(|window| gtk::prelude::RootExt::focus(&window))
            .map(|widget| widget.downgrade());
        *self.operation.borrow_mut() = Some(id.to_owned());
        let view = self.overview.borrow().clone();
        if let Some(view) = view {
            self.render(view);
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
                    *ui.operation.borrow_mut() = None;
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
                    let result = cirrove_service::accounts::set_enabled_by_id(&state, &id, enabled)
                        .map_err(|_| ());
                    let _ = send.send(result);
                });
                let weak = Rc::downgrade(self);
                glib::spawn_future_local(async move {
                    let result = receive.await;
                    let Some(ui) = weak.upgrade().filter(|ui| !ui.closed.get()) else {
                        return;
                    };
                    *ui.operation.borrow_mut() = None;
                    if !matches!(result, Ok(Ok(()))) {
                        ui.notify("Could not save the mount preference. Another account operation may be running; refresh and try again.");
                    }
                    ui.refresh();
                });
            }
        }
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
    pub fn current(&self) -> Option<Overview> {
        self.overview.borrow().clone()
    }
}
