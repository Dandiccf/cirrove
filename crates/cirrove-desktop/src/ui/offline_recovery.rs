//! Offline local recovery; no daemon or provider is instantiated.
use super::*;
use crate::recovery::offline::{self, Cursor, Page, Selection};
use cirrove_core::CancellationToken;

enum Update {
    Progress(u64),
    Finished(bool),
}
impl Window {
    pub(super) fn load_offline_recovery(self: &Rc<Self>, id: &str, cursor: Cursor) {
        let Some(card) = self.card(id).filter(|c| !c.enabled && !c.mounted) else {
            return;
        };
        let Backend::Live { runtime, state, .. } = &self.backend else {
            return;
        };
        if !self.begin_operation(id, n("Loading saved versions…")) {
            return;
        }
        let (state, label, key) = (state.clone(), card.label.clone(), card.id.clone());
        let (send, receive) = tokio::sync::oneshot::channel();
        runtime.spawn_blocking(move || {
            let _ = send.send(offline::load(&state, &label, &key, cursor));
        });
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = receive.await;
            let Some(ui) = weak.upgrade().filter(|ui| !ui.closed.get()) else {
                return;
            };
            ui.end_operation();
            if let Some(view) = ui.current() {
                ui.render(view);
            }
            if !ui
                .card(&card.id)
                .is_some_and(|c| !c.enabled && !c.mounted && c.label == card.label)
            {
                return;
            }
            match result {
                Ok(Ok(page))=>ui.offline_recovery_picker(card,page),
                _=>ui.notify(&gettext("Could not read local recovery data. Keep this connection unmounted and try again.")),
            }
        });
    }
    fn offline_recovery_picker(self: &Rc<Self>, card: AccountCard, page: Page) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let dialog = adw::AlertDialog::new(
            Some(&gettext("Recover local changes")),
            Some(&gettext(
                "Saved versions are complete local saves. Working files may contain an interrupted write. Exporting does not upload or discard anything.",
            )),
        );
        let names: Vec<String> = page
            .selections
            .iter()
            .map(|s| match s {
                Selection::Saved(save) => fill(
                    &gettext("{} — saved version {}, {} bytes"),
                    &[
                        &save.name,
                        &save.sequence.to_string(),
                        &save.size.to_string(),
                    ],
                ),
                Selection::Working(working) => fill(
                    &gettext("{} — working file, {} bytes"),
                    &[&working.name, &working.size.to_string()],
                ),
            })
            .collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let choice = gtk::DropDown::from_strings(&names);
        if names.is_empty() {
            dialog.set_extra_child(Some(&gtk::Label::new(Some(&gettext(
                "No recoverable versions on this page.",
            )))));
        } else {
            dialog.set_extra_child(Some(&choice));
        }
        dialog.add_response("cancel", &gettext("Cancel"));
        if page.next.is_some() {
            dialog.add_response("next", &gettext("Next page"));
        }
        dialog.add_response("save", &gettext("Save a local copy…"));
        dialog.set_response_enabled("save", !names.is_empty());
        dialog.set_default_response(Some(if names.is_empty() { "cancel" } else { "save" }));
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            let Some(ui) = weak.upgrade().filter(|u| !u.closed.get()) else {
                return;
            };
            if response == "next" {
                if let Some(cursor) = page.next.clone() {
                    ui.load_offline_recovery(&card.id, cursor);
                }
            } else if response == "save" {
                let Some(selection) = page.selections.get(choice.selected() as usize).cloned()
                else {
                    return;
                };
                let chooser = gtk::FileDialog::builder()
                    .title(gettext("Save a local copy outside your cloud drives"))
                    .initial_name("Recovered copy")
                    .modal(true)
                    .build();
                let key = card.id.clone();
                let weak = Rc::downgrade(&ui);
                chooser.save(
                    ui.window.upgrade().as_ref(),
                    gio::Cancellable::NONE,
                    move |result| {
                        let Some(ui) = weak.upgrade().filter(|u| !u.closed.get()) else {
                            return;
                        };
                        let Ok(file) = result else { return };
                        let Some(path) = file.path() else {
                            ui.notify(&gettext("Choose a local folder outside your cloud drives."));
                            return;
                        };
                        ui.export_offline_version(&key, selection, path);
                    },
                );
            }
        });
        dialog.present(Some(&window));
    }
    pub fn export_offline_version(
        self: &Rc<Self>,
        id: &str,
        selection: Selection,
        destination: PathBuf,
    ) {
        let Some(card) = self.card(id).filter(|c| !c.enabled && !c.mounted) else {
            return;
        };
        let Backend::Live { runtime, state, .. } = &self.backend else {
            return;
        };
        let Some(window) = self.window.upgrade() else {
            return;
        };
        if !self.begin_operation(id, n("Saving a local copy")) {
            return;
        }
        let dialog = adw::Dialog::builder()
            .title(gettext("Saving a local copy"))
            .content_width(460)
            .can_close(false)
            .build();
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(16)
            .margin_top(24)
            .margin_bottom(24)
            .margin_start(24)
            .margin_end(24)
            .build();
        let message=gtk::Label::builder().label(gettext("Local recovery data stays in Cirrove. Existing destination files will not be replaced.")).wrap(true).build();
        let progress = gtk::ProgressBar::new();
        let stop = gtk::Button::with_label(&gettext("Stop"));
        content.append(&message);
        content.append(&progress);
        content.append(&stop);
        toolbar.set_content(Some(&content));
        dialog.set_child(Some(&toolbar));
        dialog.present(Some(&window));
        let cancel = CancellationToken::new();
        let stop_cancel = cancel.clone();
        stop.connect_clicked(move |button| {
            stop_cancel.cancel();
            button.set_sensitive(false);
        });
        let (send, mut receive) = tokio::sync::mpsc::channel(2);
        let (state, target, total) = (state.clone(), destination.clone(), selection.size());
        let working = matches!(&selection, Selection::Working(_));
        let copy_cancel = cancel.clone();
        runtime.spawn_blocking(move || {
            let result = offline::copy(
                &state,
                &card.label,
                &card.id,
                &selection,
                &target,
                &copy_cancel,
                |bytes| {
                    let _ = send.try_send(Update::Progress(bytes));
                },
            );
            let _ = send.blocking_send(Update::Finished(result.is_ok()));
        });
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let mut success = false;
            while let Some(update) = receive.recv().await {
                if weak.upgrade().is_none_or(|u| u.closed.get()) {
                    cancel.cancel();
                    return;
                }
                match update {
                    Update::Progress(bytes) => {
                        progress.set_fraction(if total == 0 {
                            0.0
                        } else {
                            (bytes as f64 / total as f64).clamp(0.0, 1.0)
                        });
                        message.set_text(&fill(
                            &gettext("{} of {} bytes copied"),
                            &[&bytes.to_string(), &total.to_string()],
                        ));
                    }
                    Update::Finished(done) => {
                        success = done;
                        break;
                    }
                }
            }
            if let Some(ui) = weak.upgrade() {
                ui.end_operation();
                if let Some(view) = ui.current() {
                    ui.render(view);
                }
            }
            stop.set_visible(false);
            dialog.set_can_close(true);
            if success {
                progress.set_fraction(1.0);
                message.set_text(&fill(&if working {gettext("Working-file copy saved to {}. An interrupted write may be incomplete; the original local data is unchanged.")} else {gettext("Verified copy saved to {}. The cloud operation is unchanged.")},&[&destination.to_string_lossy()]));
            } else {
                message.set_text(&gettext("Export was not confirmed. Inspect the destination before trying again. The original saved version was not discarded."));
            }
            let close = gtk::Button::with_label(&gettext("Close"));
            let weak_dialog = dialog.downgrade();
            close.connect_clicked(move |_| {
                if let Some(dialog) = weak_dialog.upgrade() {
                    dialog.close();
                }
            });
            content.append(&close);
        });
    }
}
