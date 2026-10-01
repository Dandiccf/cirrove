//! Explicit archive import; validation and all cloud operations belong to the daemon.
use super::*;
use cirrove_service::ImportNativePackageRequest;

impl Window {
    pub fn choose_native_import(self: &Rc<Self>, id: &str) {
        let Some(card) = self.card(id).filter(AccountCard::can_import_native_package) else {
            return;
        };
        if !matches!(self.backend, Backend::Live { .. }) || self.operation.borrow().is_some() {
            return;
        }
        let chooser = gtk::FileDialog::builder()
            .title(gettext("Choose a local Pages archive"))
            .modal(true)
            .build();
        let filter = gtk::FileFilter::new();
        filter.set_name(Some(&gettext("Pages and ZIP archives")));
        filter.add_pattern("*.zip");
        filter.add_pattern("*.ZIP");
        filter.add_pattern("*.pages");
        filter.add_pattern("*.PAGES");
        chooser.set_default_filter(Some(&filter));
        let window = self.window.upgrade();
        let weak = Rc::downgrade(self);
        chooser.open(window.as_ref(), gio::Cancellable::NONE, move |result| {
            let Some(ui) = weak.upgrade().filter(|ui| !ui.closed.get()) else {
                return;
            };
            let Ok(file) = result else {
                return;
            };
            if let Some(path) = file.path() {
                ui.native_import_dialog(card.clone(), path);
            } else {
                ui.notify(&gettext("Choose an archive stored on this computer."));
            }
        });
    }

    /// Separated from the portal chooser for synthetic window validation.
    pub fn native_import_dialog(self: &Rc<Self>, selected: AccountCard, archive: PathBuf) {
        if !self
            .card(&selected.id)
            .is_some_and(|current| current.same_native_import_target(&selected))
        {
            return;
        }
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let dialog = adw::AlertDialog::new(
            Some(&gettext("Import a Pages document")),
            Some(&gettext(
                "Choose a ZIP archive containing one Pages document folder. Import creates a new document, up to 64 MiB. It does not replace or edit an existing document.",
            )),
        );
        dialog.set_content_width(520);
        let group = adw::PreferencesGroup::new();
        let source = adw::ActionRow::builder()
            .title(gettext("Local archive"))
            .subtitle(archive.file_name().and_then(|s| s.to_str()).unwrap_or(""))
            .use_markup(false)
            .subtitle_lines(2)
            .build();
        let root = adw::EntryRow::builder()
            .title(gettext("Document folder"))
            .build();
        let parent = adw::EntryRow::builder()
            .title(gettext("Destination folder"))
            .build();
        let name = adw::EntryRow::builder()
            .title(gettext("New document name"))
            .build();
        // An archive filename is only a helpful starting value, never authority.
        if let Some(guess) = archive
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| {
                if s.to_ascii_lowercase().ends_with(".zip") {
                    &s[..s.len() - 4]
                } else {
                    s
                }
            })
            .filter(|s| s.to_ascii_lowercase().ends_with(".pages"))
        {
            root.set_text(guess);
            name.set_text(guess);
        }
        group.add(&source);
        group.add(&root);
        group.add(&parent);
        group.add(&name);
        let help = gtk::Label::builder()
            .label(gettext("Use the exact .pages folder name inside the archive, and include .pages in the new name. Leave the destination blank for the top level, or enter a folder such as Documents/Reports."))
            .wrap(true)
            .xalign(0.0)
            .build();
        help.add_css_class("dim-label");
        let fields = gtk::Box::new(gtk::Orientation::Vertical, 12);
        fields.append(&group);
        fields.append(&help);
        dialog.set_extra_child(Some(&fields));
        dialog.add_response("cancel", &gettext("Cancel"));
        dialog.add_response("import", &gettext("Import"));
        dialog.set_response_appearance("import", adw::ResponseAppearance::Suggested);
        dialog.set_close_response("cancel");
        dialog.set_default_response(Some("import"));
        let valid = {
            let archive = archive.clone();
            let root = root.downgrade();
            let parent = parent.downgrade();
            let name = name.downgrade();
            move || {
                let (Some(root), Some(parent), Some(name)) =
                    (root.upgrade(), parent.upgrade(), name.upgrade())
                else {
                    return false;
                };
                crate::model::native_import_fields_valid(
                    &archive,
                    &root.text(),
                    &parent.text(),
                    &name.text(),
                )
            }
        };
        dialog.set_response_enabled("import", valid());
        let valid = Rc::new(valid);
        for entry in [&root, &parent, &name] {
            let weak_dialog = dialog.downgrade();
            let changed_valid = valid.clone();
            entry.connect_changed(move |_| {
                if let Some(dialog) = weak_dialog.upgrade() {
                    dialog.set_response_enabled("import", changed_valid());
                }
            });
            let weak_dialog = dialog.downgrade();
            let valid = valid.clone();
            entry.connect_entry_activated(move |_| {
                if valid()
                    && let Some(dialog) = weak_dialog.upgrade()
                    && let Some(button) = dialog.default_widget()
                {
                    button.activate();
                }
            });
        }
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            if response != "import" {
                return;
            }
            if let Some(ui) = weak.upgrade().filter(|ui| !ui.closed.get()) {
                ui.submit_native_import(
                    &selected,
                    archive.clone(),
                    root.text().to_string(),
                    parent.text().to_string(),
                    name.text().to_string(),
                );
            }
        });
        dialog.present(Some(&window));
    }

    pub fn submit_native_import(
        self: &Rc<Self>,
        selected: &AccountCard,
        archive: PathBuf,
        expected_root: String,
        parent: String,
        name: String,
    ) {
        let Some(card) = self
            .card(&selected.id)
            .filter(|current| current.same_native_import_target(selected))
        else {
            return;
        };
        if !crate::model::native_import_fields_valid(&archive, &expected_root, &parent, &name) {
            return;
        }
        let Backend::Live {
            runtime, socket, ..
        } = &self.backend
        else {
            return;
        };
        if !self.begin_operation(&card.id, n("Starting document import…")) {
            return;
        }
        let request = ImportNativePackageRequest {
            expected_account_id: Some(card.id.clone()),
            label: card.label,
            archive,
            expected_root,
            parent,
            name,
        };
        let socket = socket.clone();
        let (send, receive) = tokio::sync::oneshot::channel();
        runtime.spawn(async move {
            let _ = send.send(
                cirrove_service::import_native_package(&socket, &request)
                    .await
                    .map_err(|_| ()),
            );
        });
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = receive.await;
            let Some(ui) = weak.upgrade().filter(|ui| !ui.closed.get()) else {
                return;
            };
            ui.end_operation();
            match result {
                Ok(Ok(reply)) if reply.refusal.is_none() && reply.job.is_some() => ui.notify(&gettext("Import started. Progress appears with this connection's transfers.")),
                Ok(Ok(reply)) => ui.notify(reply.refusal.as_deref().unwrap_or(&gettext("The service did not start the import."))),
                _ => ui.notify(&gettext("Could not confirm the import. Check this connection's transfers before trying again.")),
            }
            ui.refresh();
        });
    }
}
