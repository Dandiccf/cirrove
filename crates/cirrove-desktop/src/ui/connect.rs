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
use cirrove_icloud::{ICloudReadSession, SignInStep};
use cirrove_service::accounts::{self, PendingConnection};
use gtk::{gio, glib};
use secrecy::SecretString;
use std::{
    cell::{Cell, RefCell},
    path::{Path, PathBuf},
    rc::Rc,
};

struct Form {
    dialog: adw::Dialog,
    stack: gtk::Stack,
    name: adw::EntryRow,
    folder: adw::ActionRow,
    folder_path: RefCell<Option<PathBuf>>,
    folder_is_custom: Cell<bool>,
    client: adw::EntryRow,
    tenant: adw::EntryRow,
    provider: adw::ComboRow,
    google_client: adw::ActionRow,
    google_client_path: RefCell<Option<PathBuf>>,
    existing_google_account: Option<String>,
    apple_id: adw::EntryRow,
    apple_password: adw::PasswordEntryRow,
    apple_code: adw::PasswordEntryRow,
    icloud_session: RefCell<Option<ICloudReadSession>>,
    writable: adw::SwitchRow,
    google_disclosure: gtk::Label,
    icloud_disclosure: gtk::Label,
    sign_in: gtk::Button,
    spinner: gtk::Spinner,
    problem: gtk::Label,
    drives: gtk::ListBox,
    drive_ids: RefCell<Vec<String>>,
    pending: RefCell<Option<PendingConnection>>,
    busy: RefCell<bool>,
}
impl Form {
    fn update_default_folder(&self) {
        if self.folder_is_custom.get() {
            self.check();
            return;
        }
        let label = self.name.text();
        let label = label.trim();
        let path = accounts::valid_label(label).then(|| {
            glib::home_dir()
                .join("Cloud")
                .join(format!("Cirrove-{label}"))
        });
        match &path {
            Some(path) => self.folder.set_subtitle(&path.to_string_lossy()),
            None => self.folder.set_subtitle(&gettext("Choose an empty folder")),
        }
        *self.folder_path.borrow_mut() = path;
        self.check();
    }
    /// Sign-in waits for a usable name and folder. Microsoft also needs an
    /// application ID; Google uses Cirrove's Desktop OAuth app by default.
    fn check(&self) {
        let provider_ready = match self.provider.selected() {
            1 => true,
            2 => {
                !self.apple_id.text().trim().is_empty()
                    && if self.icloud_session.borrow().is_some() {
                        !self.apple_code.text().trim().is_empty()
                    } else {
                        !self.apple_password.text().is_empty()
                    }
            }
            _ => !self.client.text().trim().is_empty() && !self.tenant.text().trim().is_empty(),
        };
        let ready = accounts::valid_label(self.name.text().trim())
            && self.folder_path.borrow().is_some()
            && provider_ready;
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
        self.provider.set_sensitive(!busy);
        self.google_client.set_sensitive(!busy);
        self.apple_id
            .set_sensitive(!busy && self.icloud_session.borrow().is_none());
        self.apple_password.set_sensitive(!busy);
        self.apple_code.set_sensitive(!busy);
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
                .iter()
                .find(|account| !account.authority.is_empty())
                .map(|account| (account.client_id.clone(), account.authority.clone()))
        })
        .unwrap_or_else(|| (String::new(), "common".to_owned()));
    let existing_google_account = accounts::Settings::load(state)
        .ok()
        .and_then(|settings| {
            settings
                .accounts
                .into_iter()
                .find(|account| matches!(account.registration, AppRegistration::Google { .. }))
        })
        .map(|account| account.id);

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
    let provider = adw::ComboRow::builder()
        .title(gettext("Provider"))
        .model(&gtk::StringList::new(&[
            "OneDrive",
            &gettext("Google Drive"),
            &gettext("iCloud Drive"),
        ]))
        .build();
    drive.add(&provider);
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
    let apple_id = adw::EntryRow::builder()
        .title(gettext("Apple Account email"))
        .visible(false)
        .build();
    let apple_password = adw::PasswordEntryRow::builder()
        .title(gettext("Apple Account password"))
        .visible(false)
        .build();
    let apple_code = adw::PasswordEntryRow::builder()
        .title(gettext("Trusted-device code"))
        .visible(false)
        .build();
    let writable = adw::SwitchRow::builder()
        .title(gettext("Allow changes"))
        .subtitle(gettext(
            "Files saved in this drive are uploaded. Off, the drive is read-only.",
        ))
        .build();
    let google_client = adw::ActionRow::builder()
        .title(gettext("Use another Google OAuth app"))
        .subtitle(if existing_google_account.is_some() {
            gettext("Using the Google app from an existing connection")
        } else {
            gettext("Using the Cirrove Google app (approved testers only for now)")
        })
        .activatable(true)
        .visible(false)
        .build();
    google_client.add_suffix(&gtk::Image::from_icon_name("document-open-symbolic"));
    sign.add(&google_client);
    sign.add(&client);
    sign.add(&tenant);
    sign.add(&apple_id);
    sign.add(&apple_password);
    sign.add(&apple_code);
    sign.add(&writable);
    page.add(&sign);
    let actions = adw::PreferencesGroup::new();
    let action_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
    let google_disclosure = gtk::Label::builder()
        .label(gettext(
            "Google grants account-wide Drive access: read-only, or read/write with Allow changes. Cirrove mounts only your chosen drive. Your name, email, file metadata and opened content stay local; credentials stay in your keyring. File edits go directly to Google. Removed connections keep local data until discarded.",
        ))
        .wrap(true)
        .xalign(0.0)
        .visible(false)
        .build();
    let icloud_disclosure = gtk::Label::builder()
        .label(gettext(
            "Experimental read-only iCloud Drive. Cirrove signs in with your Apple Account password and trusted-device code here, keeps only the resulting web session in your local keyring, and downloads file content when opened. Apple's Drive web protocol is undocumented; this connection is still under validation.",
        ))
        .wrap(true)
        .xalign(0.0)
        .visible(false)
        .build();
    let problem = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .visible(false)
        .build();
    problem.add_css_class("error");
    let sign_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    sign_row.set_halign(gtk::Align::End);
    let spinner = gtk::Spinner::builder().visible(false).build();
    let sign_in = gtk::Button::with_label(&gettext("Sign in with Microsoft"));
    sign_in.add_css_class("suggested-action");
    sign_in.add_css_class("pill");
    sign_in.set_sensitive(false);
    sign_row.append(&spinner);
    sign_row.append(&sign_in);
    action_box.append(&google_disclosure);
    action_box.append(&icloud_disclosure);
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
        folder_is_custom: Cell::new(false),
        client,
        tenant,
        provider,
        google_client,
        google_client_path: RefCell::new(None),
        existing_google_account,
        apple_id,
        apple_password,
        apple_code,
        icloud_session: RefCell::new(None),
        writable,
        google_disclosure,
        icloud_disclosure,
        sign_in,
        spinner,
        problem,
        drives,
        drive_ids: RefCell::new(Vec::new()),
        pending: RefCell::new(None),
        busy: RefCell::new(false),
    });
    {
        let form = form.clone();
        form.provider.clone().connect_selected_notify(move |_| {
            let google = form.provider.selected() == 1;
            let icloud = form.provider.selected() == 2;
            *form.icloud_session.borrow_mut() = None;
            form.apple_password.set_text("");
            form.apple_code.set_text("");
            form.apple_id.set_sensitive(true);
            form.dialog.set_content_height(if google || icloud { 800 } else { 660 });
            form.client.set_visible(!google && !icloud);
            form.tenant.set_visible(!google && !icloud);
            form.apple_id.set_visible(icloud);
            form.apple_password.set_visible(icloud);
            form.apple_code.set_visible(false);
            form.writable.set_visible(!icloud);
            form.google_client.set_visible(google);
            form.google_disclosure.set_visible(google);
            form.icloud_disclosure.set_visible(icloud);
            form.sign_in.set_label(&if icloud {
                gettext("Sign in to iCloud")
            } else if google {
                gettext("Sign in with Google")
            } else {
                gettext("Sign in with Microsoft")
            });
            sign.set_title(&if icloud {
                gettext("Apple sign-in")
            } else if google {
                gettext("Google sign-in")
            } else {
                gettext("Microsoft sign-in")
            });
            sign.set_description(Some(&if icloud {
                gettext("Use your regular Apple Account password. A trusted-device code may be required. This drive is read-only.")
            } else if google {
                gettext("My Drive and Shared Drives can be connected. Allow changes enables edits to ordinary files. Google Docs and Sheets appear as read-only export packages.")
            } else { gettext("The application (client) ID of your app registration; the OneDrive setup guide explains where it comes from.") }));
            form.check();
        });
    }
    {
        let form = form.clone();
        let window = window.clone();
        form.google_client.clone().connect_activated(move |_| {
            let chooser = gtk::FileDialog::builder()
                .title(gettext("Google Desktop OAuth client JSON"))
                .modal(true)
                .build();
            let form = form.clone();
            chooser.open(Some(&window), None::<&gio::Cancellable>, move |result| {
                if let Ok(file) = result
                    && let Some(path) = file.path()
                {
                    form.google_client.set_subtitle(&path.to_string_lossy());
                    *form.google_client_path.borrow_mut() = Some(path);
                    form.check();
                }
            });
        });
    }
    {
        let form = form.clone();
        form.name
            .clone()
            .connect_changed(move |_| form.update_default_folder());
    }
    for row in [&form.client, &form.tenant, &form.apple_id] {
        let form = form.clone();
        row.connect_changed(move |_| form.check());
    }
    {
        let form = form.clone();
        form.apple_password
            .clone()
            .connect_changed(move |_| form.check());
    }
    {
        let form = form.clone();
        form.apple_code
            .clone()
            .connect_changed(move |_| form.check());
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
                    form.folder_is_custom.set(true);
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
    for row in [&form.apple_password, &form.apple_code] {
        let form = form.clone();
        row.connect_entry_activated(move |_| {
            if form.provider.selected() == 2 && form.sign_in.is_sensitive() {
                form.sign_in.emit_clicked();
            }
        });
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
    if form.provider.selected() == 2 {
        begin_icloud(form, ui, runtime, state);
        return;
    }
    let label = form.name.text().trim().to_owned();
    let Some(folder) = form.folder_path.borrow().clone() else {
        return;
    };
    let google = form.provider.selected() == 1;
    let google_client = form.google_client_path.borrow().clone();
    let existing_google_account = form.existing_google_account.clone();
    let app = AppRegistration::Microsoft {
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
        &if google { gettext("Your browser opens the Google sign-in. Choose the account there; this window continues when it is done.") }
        else { gettext("Your browser opens the Microsoft sign-in. Choose the account there; this window continues when it is done.") },
    );
    let state = state.to_path_buf();
    let (send, receive) = tokio::sync::oneshot::channel();
    // block_on off the runtime's worker threads: the sign-in holds a browser
    // callback across awaits, and nothing about that needs to be Send.
    runtime.spawn_blocking(move || {
        let result = tokio::runtime::Handle::current()
            .block_on(async move {
                if google {
                    if let Some(client_file) = google_client {
                        accounts::begin_connect_google_with_access(
                            state,
                            label,
                            client_file,
                            folder,
                            access,
                        )
                        .await
                    } else if let Some(account_id) = existing_google_account {
                        accounts::begin_connect_google_with_existing_app(
                            state, label, account_id, folder, access,
                        )
                        .await
                    } else {
                        accounts::begin_connect(
                            state,
                            label,
                            AppRegistration::Google {
                                client_id: cirrove_auth::google::CIRROVE_DESKTOP_CLIENT_ID.into(),
                            },
                            folder,
                            access,
                        )
                        .await
                    }
                } else {
                    accounts::begin_connect(state, label, app, folder, access).await
                }
            })
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

enum ICloudOutcome {
    NeedsCode(Box<ICloudReadSession>),
    Connected(String, PathBuf),
}

fn begin_icloud(form: &Rc<Form>, ui: &Rc<Window>, runtime: &tokio::runtime::Handle, state: &Path) {
    let label = form.name.text().trim().to_owned();
    let Some(folder) = form.folder_path.borrow().clone() else {
        return;
    };
    let apple_id = form.apple_id.text().trim().to_owned();
    let waiting = form.icloud_session.borrow_mut().take();
    let secret = if waiting.is_some() {
        let code = SecretString::new(form.apple_code.text().to_string().into());
        form.apple_code.set_text("");
        code
    } else {
        let password = SecretString::new(form.apple_password.text().to_string().into());
        form.apple_password.set_text("");
        password
    };
    form.set_busy(true, &gettext("Signing in to iCloud…"));
    let state = state.to_path_buf();
    let (send, receive) = tokio::sync::oneshot::channel();
    runtime.spawn_blocking(move || {
        let result = tokio::runtime::Handle::current()
            .block_on(async move {
                let session = if let Some(mut session) = waiting {
                    session.verify_trusted_device_code(&secret).await?;
                    session
                } else {
                    cirrove_auth::DesktopVault::reachable().await?;
                    let mut session = ICloudReadSession::new()?;
                    match session.sign_in(&apple_id, &secret).await? {
                        SignInStep::Ready => {}
                        SignInStep::NeedsTrustedDeviceCode => {
                            session.request_trusted_device_code().await?;
                            return Ok::<_, anyhow::Error>(ICloudOutcome::NeedsCode(Box::new(
                                session,
                            )));
                        }
                    }
                    session
                };
                let account =
                    accounts::connect_icloud_with_session(state, label, folder, apple_id, session)
                        .await?;
                Ok::<_, anyhow::Error>(ICloudOutcome::Connected(account.label, account.mount_path))
            })
            .map_err(|error| format!("{error:#}"));
        let _ = send.send(result);
    });
    let form = form.clone();
    let ui = ui.clone();
    glib::spawn_future_local(async move {
        match receive.await {
            Ok(Ok(ICloudOutcome::NeedsCode(session))) => {
                *form.icloud_session.borrow_mut() = Some(*session);
                form.apple_password.set_visible(false);
                form.apple_code.set_visible(true);
                form.sign_in.set_label(&gettext("Verify code and connect"));
                form.set_busy(false, "");
                form.apple_code.grab_focus();
            }
            Ok(Ok(ICloudOutcome::Connected(label, path))) => {
                form.dialog.close();
                ui.notify(&format!(
                    "Connected {label} read-only. Its files appear in {}.",
                    path.display()
                ));
                ui.refresh();
            }
            other => {
                *form.icloud_session.borrow_mut() = None;
                form.apple_code.set_text("");
                form.apple_code.set_visible(false);
                form.apple_password.set_visible(true);
                form.sign_in.set_label(&gettext("Sign in to iCloud"));
                match other {
                    Ok(Err(error)) => form.set_busy(false, &error),
                    Err(_) => form.set_busy(false, &gettext("The iCloud sign-in did not finish.")),
                    Ok(Ok(_)) => unreachable!(),
                }
            }
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

struct ICloudReauthForm {
    dialog: adw::Dialog,
    password: adw::PasswordEntryRow,
    code: adw::PasswordEntryRow,
    button: gtk::Button,
    spinner: gtk::Spinner,
    problem: gtk::Label,
    session: RefCell<Option<ICloudReadSession>>,
    busy: Cell<bool>,
}

impl ICloudReauthForm {
    fn check(&self) {
        let ready = if self.session.borrow().is_some() {
            !self.code.text().trim().is_empty()
        } else {
            !self.password.text().is_empty()
        };
        self.button.set_sensitive(ready && !self.busy.get());
    }

    fn set_busy(&self, busy: bool, issue: &str) {
        self.busy.set(busy);
        self.password.set_sensitive(!busy);
        self.code.set_sensitive(!busy);
        self.spinner.set_visible(busy);
        self.spinner.set_spinning(busy);
        self.problem.set_label(issue);
        self.problem.set_visible(!issue.is_empty());
        if busy {
            self.problem.remove_css_class("error");
        } else {
            self.problem.add_css_class("error");
        }
        self.check();
    }
}

pub(super) fn present_icloud_reauth(ui: &Rc<Window>, id: &str) {
    let Some(window) = ui.window.upgrade() else {
        return;
    };
    let Backend::Live { runtime, state, .. } = &ui.backend else {
        return;
    };
    let Some(card) = ui.card(id) else {
        return;
    };
    let dialog = adw::Dialog::builder()
        .title(gettext("Sign in to iCloud again"))
        .content_width(480)
        .content_height(430)
        .build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::builder()
        .title(gettext("Apple Account"))
        .description(&card.username)
        .build();
    let password = adw::PasswordEntryRow::builder()
        .title(gettext("Apple Account password"))
        .build();
    let code = adw::PasswordEntryRow::builder()
        .title(gettext("Trusted-device code"))
        .visible(false)
        .build();
    group.add(&password);
    group.add(&code);
    page.add(&group);
    let note = gtk::Label::builder()
        .label(gettext("Cirrove keeps only the resulting Apple web session in your local keyring. This iCloud drive stays read-only."))
        .wrap(true)
        .xalign(0.0)
        .build();
    let actions = adw::PreferencesGroup::new();
    actions.add(&note);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.set_halign(gtk::Align::End);
    let spinner = gtk::Spinner::builder().visible(false).build();
    let button = gtk::Button::with_label(&gettext("Sign in again"));
    button.add_css_class("suggested-action");
    button.set_sensitive(false);
    row.append(&spinner);
    row.append(&button);
    let problem = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .visible(false)
        .build();
    problem.add_css_class("error");
    actions.add(&problem);
    actions.add(&row);
    page.add(&actions);
    toolbar.set_content(Some(&page));
    dialog.set_child(Some(&toolbar));
    let form = Rc::new(ICloudReauthForm {
        dialog: dialog.clone(),
        password,
        code,
        button,
        spinner,
        problem,
        session: RefCell::new(None),
        busy: Cell::new(false),
    });
    {
        let form = form.clone();
        form.password.clone().connect_changed(move |_| form.check());
    }
    {
        let form = form.clone();
        form.code.clone().connect_changed(move |_| form.check());
    }
    {
        let form = form.clone();
        let ui = ui.clone();
        let runtime = runtime.clone();
        let state = state.clone();
        let apple_id = card.username.clone();
        let label = card.label.clone();
        form.button.clone().connect_clicked(move |_| {
            start_icloud_reauth(&form, &ui, &runtime, &state, &label, &apple_id);
        });
    }
    for row in [&form.password, &form.code] {
        let form = form.clone();
        row.connect_entry_activated(move |_| {
            if form.button.is_sensitive() {
                form.button.emit_clicked();
            }
        });
    }
    dialog.present(Some(&window));
}

enum ICloudReauthOutcome {
    NeedsCode(Box<ICloudReadSession>),
    Done,
}

fn start_icloud_reauth(
    form: &Rc<ICloudReauthForm>,
    ui: &Rc<Window>,
    runtime: &tokio::runtime::Handle,
    state: &Path,
    label: &str,
    apple_id: &str,
) {
    let waiting = form.session.borrow_mut().take();
    let secret = if waiting.is_some() {
        let code = SecretString::new(form.code.text().to_string().into());
        form.code.set_text("");
        code
    } else {
        let password = SecretString::new(form.password.text().to_string().into());
        form.password.set_text("");
        password
    };
    form.set_busy(true, &gettext("Signing in to iCloud…"));
    let state = state.to_path_buf();
    let label = label.to_owned();
    let apple_id = apple_id.to_owned();
    let (send, receive) = tokio::sync::oneshot::channel();
    runtime.spawn_blocking(move || {
        let result = tokio::runtime::Handle::current()
            .block_on(async move {
                let session = if let Some(mut session) = waiting {
                    session.verify_trusted_device_code(&secret).await?;
                    session
                } else {
                    cirrove_auth::DesktopVault::reachable().await?;
                    let mut session = ICloudReadSession::new()?;
                    match session.sign_in(&apple_id, &secret).await? {
                        SignInStep::Ready => {}
                        SignInStep::NeedsTrustedDeviceCode => {
                            session.request_trusted_device_code().await?;
                            return Ok::<_, anyhow::Error>(ICloudReauthOutcome::NeedsCode(
                                Box::new(session),
                            ));
                        }
                    }
                    session
                };
                accounts::reauthenticate_icloud_with_session(state, label, session).await?;
                Ok::<_, anyhow::Error>(ICloudReauthOutcome::Done)
            })
            .map_err(|error| format!("{error:#}"));
        let _ = send.send(result);
    });
    let form = form.clone();
    let ui = ui.clone();
    glib::spawn_future_local(async move {
        match receive.await {
            Ok(Ok(ICloudReauthOutcome::NeedsCode(session))) => {
                *form.session.borrow_mut() = Some(*session);
                form.password.set_visible(false);
                form.code.set_visible(true);
                form.button.set_label(&gettext("Verify code"));
                form.set_busy(false, "");
                form.code.grab_focus();
            }
            Ok(Ok(ICloudReauthOutcome::Done)) => {
                form.dialog.close();
                ui.notify(&gettext("iCloud is signed in again with read-only access."));
                ui.refresh();
            }
            other => {
                *form.session.borrow_mut() = None;
                form.code.set_text("");
                form.code.set_visible(false);
                form.password.set_visible(true);
                form.button.set_label(&gettext("Sign in again"));
                match other {
                    Ok(Err(error)) => form.set_busy(false, &error),
                    Err(_) => form.set_busy(false, &gettext("The iCloud sign-in did not finish.")),
                    Ok(Ok(_)) => unreachable!(),
                }
            }
        }
    });
}
