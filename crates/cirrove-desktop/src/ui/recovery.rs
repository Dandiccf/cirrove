//! Local-only recovery. The daemon owns generation selection and integrity checks.
use super::*;
use cirrove_service::{jobs::Job, recent::LocalChange};

impl Window {
    pub fn choose_recovery_save(self: &Rc<Self>, id: &str) {
        if self.card(id).is_some_and(|c| !c.enabled && !c.mounted) {
            self.load_offline_recovery(id, crate::recovery::offline::Cursor::default());
            return;
        }
        let Some(card) = self.card(id).filter(|c| c.mounted && c.writable) else {
            return;
        };
        let Backend::Live {
            runtime, socket, ..
        } = &self.backend
        else {
            return;
        };
        if !self.begin_operation(id, n("Loading saved versions…")) {
            return;
        }
        let socket = socket.clone();
        let label = card.label.clone();
        let (send, receive) = tokio::sync::oneshot::channel();
        runtime.spawn(async move {
            let _ = send.send(
                cirrove_service::recent(
                    &socket,
                    &cirrove_service::RecentRequest { label, limit: 200 },
                )
                .await,
            );
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
            let saves = match result {
                Ok(Ok(reply)) if reply.refusal.is_none() => reply
                    .local
                    .into_iter()
                    .filter(crate::recovery::eligible)
                    .collect::<Vec<_>>(),
                _ => {
                    ui.notify(&gettext(
                        "Could not load saved versions. The service may be unavailable.",
                    ));
                    return;
                }
            };
            if saves.is_empty() {
                ui.notify(&gettext("No sealed local version is available to export. Unsaved editor changes are not included."));
                return;
            }
            ui.recovery_picker(card, saves);
        });
    }

    fn recovery_picker(self: &Rc<Self>, card: AccountCard, saves: Vec<LocalChange>) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let dialog = adw::AlertDialog::new(
            Some(&gettext("Recover a saved version")),
            Some(&gettext(
                "Choose a version from the latest 200 saves. The cloud operation stays unchanged.",
            )),
        );
        let names: Vec<String> = saves
            .iter()
            .map(|save| {
                fill(
                    &gettext("{} — version {}, {} bytes"),
                    &[
                        &save.name,
                        &save.sequence.to_string(),
                        &save.size.to_string(),
                    ],
                )
            })
            .collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let choice = gtk::DropDown::from_strings(&names);
        dialog.set_extra_child(Some(&choice));
        dialog.add_response("cancel", &gettext("Cancel"));
        dialog.add_response("save", &gettext("Save a local copy…"));
        dialog.set_default_response(Some("save"));
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            if response != "save" {
                return;
            }
            let Some(ui) = weak.upgrade().filter(|ui| !ui.closed.get()) else {
                return;
            };
            let Some(save) = saves.get(choice.selected() as usize) else {
                return;
            };
            ui.recovery_destination(card.clone(), save.clone());
        });
        dialog.present(Some(&window));
    }

    fn recovery_destination(self: &Rc<Self>, card: AccountCard, save: LocalChange) {
        let chooser = gtk::FileDialog::builder()
            .title(gettext("Save a local copy outside your cloud drives"))
            .initial_name("Recovered copy")
            .modal(true)
            .build();
        let weak = Rc::downgrade(self);
        chooser.save(
            self.window.upgrade().as_ref(),
            gio::Cancellable::NONE,
            move |result| {
                let Some(ui) = weak.upgrade().filter(|ui| !ui.closed.get()) else {
                    return;
                };
                let Ok(file) = result else { return };
                let Some(path) = file.path() else {
                    ui.notify(&gettext("Choose a local folder outside your cloud drives."));
                    return;
                };
                ui.export_saved_version(&card.id, save, path);
            },
        );
    }

    /// Start the selected immutable generation after the local destination chooser.
    pub fn export_saved_version(
        self: &Rc<Self>,
        id: &str,
        save: LocalChange,
        destination: PathBuf,
    ) {
        let Some(current) = self.card(id).filter(|c| c.mounted && c.writable) else {
            return;
        };
        let Some(operation) = save.operation else {
            return;
        };
        let Backend::Live {
            runtime, socket, ..
        } = &self.backend
        else {
            return;
        };
        let Some(window) = self.window.upgrade() else {
            return;
        };
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
        let message = gtk::Label::builder().label(gettext("The saved version stays in Cirrove. Existing destination files will not be replaced."))
            .wrap(true).build();
        let progress = gtk::ProgressBar::new();
        let stop = gtk::Button::with_label(&gettext("Stop"));
        stop.set_sensitive(false);
        content.append(&message);
        content.append(&progress);
        content.append(&stop);
        toolbar.set_content(Some(&content));
        dialog.set_child(Some(&toolbar));
        dialog.present(Some(&window));
        let job_id = Rc::new(RefCell::new(None::<String>));
        let socket_stop = socket.clone();
        let runtime_stop = runtime.clone();
        let label_stop = current.label.clone();
        let stop_id = job_id.clone();
        stop.connect_clicked(move |button| {
            let Some(id) = stop_id.borrow().clone() else {
                return;
            };
            button.set_sensitive(false);
            let socket = socket_stop.clone();
            let label = label_stop.clone();
            runtime_stop.spawn(async move {
                let _ = cirrove_service::stop_job(
                    &socket,
                    &cirrove_service::StopJobRequest { label, id },
                )
                .await;
            });
        });
        let socket = socket.clone();
        let request = cirrove_service::ExportSaveRequest {
            label: current.label,
            operation,
            destination: destination.clone(),
        };
        let account_id = current.id;
        let (send, mut receive) = tokio::sync::mpsc::channel::<Result<Job, ()>>(2);
        runtime.spawn(async move {
            let first = match cirrove_service::export_save(&socket, &request).await {
                Ok(reply) if reply.refusal.is_none() => reply.job,
                _ => None,
            };
            let Some(mut job) = first else {
                let _ = send.send(Err(())).await;
                return;
            };
            let id = job.id.clone();
            loop {
                let running = job.running();
                if send.send(Ok(job)).await.is_err() || !running {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
                job = match cirrove_service::status(&socket).await {
                    Ok(status) => match status
                        .accounts
                        .into_iter()
                        .find(|a| a.account_id == account_id)
                        .and_then(|a| a.jobs.into_iter().find(|j| j.id == id))
                    {
                        Some(job) => job,
                        None => {
                            let _ = send.send(Err(())).await;
                            break;
                        }
                    },
                    Err(_) => {
                        let _ = send.send(Err(())).await;
                        break;
                    }
                };
            }
        });
        glib::spawn_future_local(async move {
            let mut success = false;
            while let Some(update) = receive.recv().await {
                let Ok(job) = update else { break };
                *job_id.borrow_mut() = Some(job.id.clone());
                progress.set_fraction(if job.bytes_total == 0 {
                    0.0
                } else {
                    (job.bytes_done as f64 / job.bytes_total as f64).clamp(0.0, 1.0)
                });
                message.set_text(&fill(
                    &gettext("{} of {} bytes copied"),
                    &[&job.bytes_done.to_string(), &job.bytes_total.to_string()],
                ));
                stop.set_sensitive(job.state == cirrove_service::jobs::JobState::Running);
                if !job.running() {
                    success = crate::recovery::confirmed(&job, &save, &destination);
                    break;
                }
            }
            stop.set_visible(false);
            dialog.set_can_close(true);
            if success {
                progress.set_fraction(1.0);
                message.set_text(&fill(
                    &gettext("Verified copy saved to {}. The cloud operation is unchanged."),
                    &[&destination.to_string_lossy()],
                ));
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
