//! Connecting a drive from the window: a name, a folder, the app registration,
//! a sign-in in the browser, and a choice among the drives the account can see.
//!
//! The same steps the CLI's `connect` walks, in the same order, against the
//! same service code -- `accounts::begin_connect` and
//! `PendingConnection::finish`. What the CLI asks on stdin, this asks with
//! a list. Nothing is saved until a drive is chosen; closing the dialog before
//! that forgets the grant.
use super::{Backend, Window};
use crate::i18n::gettext;
use adw::prelude::*;
use cirrove_auth::{AccessMode, AppRegistration};
use cirrove_service::accounts::{self, PendingConnection};
use gtk::{gio, glib};
use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
};

struct Form {
    dialog: adw::Dialog,
    stack: gtk::Stack,
    name: adw::EntryRow,
    folder: adw::ActionRow,
    folder_path: RefCell<Option<PathBuf>>,
    client: adw::EntryRow,
    tenant: adw::EntryRow,
    writable: adw::SwitchRow,
    sign_in: gtk::Button,
    spinner: gtk::Spinner,
    problem: gtk::Label,
    drives: gtk::ListBox,
    drive_ids: RefCell<Vec<String>>,
    pending: RefCell<Option<PendingConnection>>,
    busy: RefCell<bool>,
}
impl Form {
    /// Sign-in waits for something to sign in with: a usable name, a folder,
    /// and an application id. The button says so by staying grey.
    fn check(&self) {
        let ready = accounts::valid_label(self.name.text().trim())
            && self.folder_path.borrow().is_some()
            && !self.client.text().trim().is_empty()
            && !self.tenant.text().trim().is_empty();
        self.sign_in.set_sensitive(ready && !*self.busy.borrow());
    }
    fn set_busy(&self, busy: bool, message: &str) {
        *self.busy.borrow_mut() = busy;
        self.spinner.set_visible(busy);
        self.spinner.set_spinning(busy);
        self.problem.set_label(message);
        self.problem.set_visible(!message.is_empty());
        if busy {
            self.problem.remove_css_class("error");
        } else {
            self.problem.add_css_class("error");
        }
        for row in [&self.name, &self.client, &self.tenant] {
            row.set_sensitive(!busy);
        }
        self.folder.set_sensitive(!busy);
        self.writable.set_sensitive(!busy);
        self.check();
    }
}

pub(super) fn present(ui: &Rc<Window>) {
    let Some(window) = ui.window.upgrade() else {
        return;
    };
    let Backend::Live { runtime, state, .. } = &ui.backend else {
        ui.notify("Connecting a drive needs the running service; this is the interface preview.");
        return;
    };
    // A second drive starts from the registration the first one used: the
    // client id is the one thing on this form nobody remembers.
    let (client_id, authority) = ui
        .current()
        .and_then(|view| {
            view.accounts
                .first()
                .map(|account| (account.client_id.clone(), account.authority.clone()))
        })
        .unwrap_or_else(|| (String::new(), "common".to_owned()));

    let dialog = adw::Dialog::builder()
        .title(gettext("Connect a drive"))
        .content_width(480)
        .build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::SlideLeft)
        .vhomogeneous(false)
        .build();
    toolbar.set_content(Some(&stack));
    dialog.set_child(Some(&toolbar));

    // Page one: the form.
    let page = adw::PreferencesPage::new();
    let drive = adw::PreferencesGroup::builder()
        .title(gettext("Drive"))
        .description(gettext(
            "A name for this connection, and the folder its files appear in.",
        ))
        .build();
    let name = adw::EntryRow::builder().title(gettext("Name")).build();
    let folder = adw::ActionRow::builder()
        .title(gettext("Folder in Files"))
        .subtitle(gettext("Choose an empty folder"))
        .activatable(true)
        .build();
    folder.add_suffix(&gtk::Image::from_icon_name("folder-open-symbolic"));
    drive.add(&name);
    drive.add(&folder);
    page.add(&drive);
    let sign = adw::PreferencesGroup::builder()
        .title(gettext("Microsoft sign-in"))
        .description(gettext("The application (client) ID of your app registration; the OneDrive setup guide explains where it comes from."))
        .build();
    let client = adw::EntryRow::builder()
        .title(gettext("Application (client) ID"))
        .text(&client_id)
        .build();
    let tenant = adw::EntryRow::builder()
        .title(gettext("Tenant"))
        .text(&authority)
        .build();
    let writable = adw::SwitchRow::builder()
        .title(gettext("Allow changes"))
        .subtitle(gettext(
            "Files saved in this drive are uploaded. Off, the drive is read-only.",
        ))
        .build();
    sign.add(&client);
    sign.add(&tenant);
    sign.add(&writable);
    page.add(&sign);
    let actions = adw::PreferencesGroup::new();
    let action_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
    let problem = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .visible(false)
        .build();
    problem.add_css_class("error");
    let sign_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    sign_row.set_halign(gtk::Align::End);
    let spinner = gtk::Spinner::builder().visible(false).build();
    let sign_in = gtk::Button::with_label("Sign in with Microsoft");
    sign_in.add_css_class("suggested-action");
    sign_in.add_css_class("pill");
    sign_in.set_sensitive(false);
    sign_row.append(&spinner);
    sign_row.append(&sign_in);
    action_box.append(&problem);
    action_box.append(&sign_row);
    actions.add(&action_box);
    page.add(&actions);
    stack.add_named(&page, Some("form"));

    // Page two: the drives, once the account is known.
    let drives_page = adw::PreferencesPage::new();
    let drives_group = adw::PreferencesGroup::builder()
        .title(gettext("Choose a drive"))
        .description(
            "The drives this account can see. Files from the one you choose appear in the folder.",
        )
        .build();
    let drives = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .build();
    drives.add_css_class("boxed-list");
    drives_group.add(&drives);
    drives_page.add(&drives_group);
    stack.add_named(&drives_page, Some("drives"));

    let form = Rc::new(Form {
        dialog: dialog.clone(),
        stack,
        name,
        folder,
        folder_path: RefCell::new(None),
        client,
        tenant,
        writable,
        sign_in,
        spinner,
        problem,
        drives,
        drive_ids: RefCell::new(Vec::new()),
        pending: RefCell::new(None),
        busy: RefCell::new(false),
    });
    for row in [&form.name, &form.client, &form.tenant] {
        let form = form.clone();
        row.connect_changed(move |_| form.check());
    }
    {
        let form = form.clone();
        let window = window.clone();
        form.folder.clone().connect_activated(move |_| {
            let chooser = gtk::FileDialog::builder()
                .title(gettext("Folder for this drive"))
                .modal(true)
                .build();
            let form = form.clone();
            chooser.select_folder(Some(&window), None::<&gio::Cancellable>, move |result| {
                if let Ok(file) = result
                    && let Some(path) = file.path()
                {
                    form.folder.set_subtitle(&path.to_string_lossy());
                    *form.folder_path.borrow_mut() = Some(path);
                    form.check();
                }
            });
        });
    }
    {
        let form = form.clone();
        let ui = ui.clone();
        let runtime = runtime.clone();
        let state = state.clone();
        form.sign_in
            .clone()
            .connect_clicked(move |_| begin(&form, &ui, &runtime, &state));
    }
    {
        let form = form.clone();
        let ui = ui.clone();
        let runtime = runtime.clone();
        let state = state.clone();
        form.drives.clone().connect_row_activated(move |_, row| {
            let Some(id) = form
                .drive_ids
                .borrow()
                .get(usize::try_from(row.index()).unwrap_or(usize::MAX))
                .cloned()
            else {
                return;
            };
            finish(&form, &ui, &runtime, &state, id);
        });
    }
    form.check();
    dialog.present(Some(&window));
}

fn begin(form: &Rc<Form>, ui: &Rc<Window>, runtime: &tokio::runtime::Handle, state: &Path) {
    let label = form.name.text().trim().to_owned();
    let Some(folder) = form.folder_path.borrow().clone() else {
        return;
    };
    let app = AppRegistration {
        client_id: form.client.text().trim().to_owned(),
        authority: form.tenant.text().trim().to_owned(),
    };
    let access = if form.writable.is_active() {
        AccessMode::ReadWrite
    } else {
        AccessMode::ReadOnly
    };
    form.set_busy(
        true,
        "Your browser opens the Microsoft sign-in. Choose the account there; this window continues when it is done.",
    );
    let state = state.to_path_buf();
    let (send, receive) = tokio::sync::oneshot::channel();
    // block_on off the runtime's worker threads: the sign-in holds a browser
    // callback across awaits, and nothing about that needs to be Send.
    runtime.spawn_blocking(move || {
        let result = tokio::runtime::Handle::current()
            .block_on(accounts::begin_connect(state, label, app, folder, access))
            .map_err(|error| format!("{error:#}"));
        let _ = send.send(result);
    });
    let form = form.clone();
    let _ui = ui.clone();
    glib::spawn_future_local(async move {
        match receive.await {
            Ok(Ok(pending)) => {
                show_drives(&form, &pending);
                *form.pending.borrow_mut() = Some(pending);
                form.set_busy(false, "");
                form.stack.set_visible_child_name("drives");
            }
            Ok(Err(error)) => form.set_busy(false, &error),
            Err(_) => form.set_busy(false, "The sign-in did not finish."),
        }
    });
}

fn show_drives(form: &Rc<Form>, pending: &PendingConnection) {
    while let Some(child) = form.drives.first_child() {
        form.drives.remove(&child);
    }
    let mut ids = form.drive_ids.borrow_mut();
    ids.clear();
    for drive in pending.drives() {
        let row = adw::ActionRow::builder()
            .title(&drive.name)
            .subtitle(format!("{} · {}", drive.drive_type, drive.web_url))
            .use_markup(false)
            .activatable(true)
            .build();
        row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        form.drives.append(&row);
        ids.push(drive.id.clone());
    }
}

fn finish(
    form: &Rc<Form>,
    ui: &Rc<Window>,
    runtime: &tokio::runtime::Handle,
    state: &Path,
    drive_id: String,
) {
    let Some(pending) = form.pending.borrow_mut().take() else {
        return;
    };
    form.stack.set_visible_child_name("form");
    form.set_busy(true, "Saving the connection…");
    let state = state.to_path_buf();
    let (send, receive) = tokio::sync::oneshot::channel();
    runtime.spawn_blocking(move || {
        let result = tokio::runtime::Handle::current()
            .block_on(async {
                let account = pending.finish(&drive_id).await?;
                // The CLI saves a writable account unmounted, a leftover from
                // when only a validator asked for write access. From the
                // window, connecting a drive means seeing it in Files.
                if !account.enabled {
                    accounts::set_enabled_by_id(&state, &account.id, true)?;
                }
                Ok::<_, anyhow::Error>((account.label, account.mount_path))
            })
            .map_err(|error| format!("{error:#}"));
        let _ = send.send(result);
    });
    let form = form.clone();
    let ui = ui.clone();
    glib::spawn_future_local(async move {
        match receive.await {
            Ok(Ok((label, path))) => {
                form.dialog.close();
                ui.notify(&format!(
                    "Connected {label}. Its files appear in {}.",
                    path.display()
                ));
                ui.refresh();
            }
            // The grant went with the attempt; the next try signs in again.
            Ok(Err(error)) => form.set_busy(false, &error),
            Err(_) => form.set_busy(false, "The connection could not be saved."),
        }
    });
}
