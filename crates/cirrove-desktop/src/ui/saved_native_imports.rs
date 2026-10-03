//! Explicit historical discovery and observer-only checks; never admission/retry.
use super::*;
use cirrove_service::{ListNativeImportsRequest, WatchNativeImportRequest};
const PAGE_LIMIT: u32 = 25;

fn page_valid(after: Option<u64>, reply: &cirrove_service::ListNativeImportsReply) -> bool {
    if reply.refusal.is_some()
        || reply.operations.len() > PAGE_LIMIT as usize
        || !serde_json::to_vec(reply).is_ok_and(|bytes| bytes.len() <= 512 * 1024)
    {
        return false;
    }
    let mut previous = after.unwrap_or(0);
    for row in &reply.operations {
        if row.sequence <= previous
            || row.name.is_empty()
            || row.name.len() > 255
            || row.name.chars().any(char::is_control)
            || !row.parent.starts_with("FOLDER::com.apple.CloudDocs::")
            || row.parent.len() > 4096
            || row.parent.chars().any(char::is_control)
            || row.completion_receipt_recorded
                != (row.state == cirrove_service::journal::UploadState::Uploaded)
            || row.completion_receipt_recorded != row.remote_item.is_some()
            || row.remote_item.as_ref().is_some_and(|id| {
                !id.starts_with("FILE::com.apple.CloudDocs::")
                    || id.len() > 4096
                    || id.chars().any(char::is_control)
            })
        {
            return false;
        }
        previous = row.sequence;
    }
    reply
        .next
        .is_none_or(|next| next > after.unwrap_or(0) && next >= previous)
}
fn watch_reply_valid(
    request: &WatchNativeImportRequest,
    reply: &cirrove_service::ImportNativePackageReply,
) -> bool {
    reply.refusal.is_none()
        && reply.job.as_ref().is_some_and(|job| {
            job.kind == cirrove_service::jobs::JobKind::ImportNativePackage
                && !job.id.is_empty()
                && job
                    .native_import
                    .as_ref()
                    .is_some_and(|progress| progress.operation == request.operation)
        })
}
fn historical_detail(row: &cirrove_service::journal::NativeImportSelection) -> String {
    if row.completion_receipt_recorded {
        gettext("Upload completion recorded. Check to confirm current availability.")
    } else {
        match row.state {
            cirrove_service::journal::UploadState::Failed
            | cirrove_service::journal::UploadState::Conflict => gettext(
                "This saved import was not confirmed. Checking does not submit another copy.",
            ),
            cirrove_service::journal::UploadState::Resolved => gettext(
                "This saved import was resolved. Its retained history does not confirm current availability.",
            ),
            _ => gettext(
                "This import remains saved. Checking observes its progress without submitting another copy.",
            ),
        }
    }
}
impl Window {
    pub fn load_saved_native_imports(self: &Rc<Self>, id: &str, after: Option<u64>) {
        let Some(selected) = self.card(id).filter(AccountCard::can_list_native_imports) else {
            return;
        };
        let Backend::Live {
            runtime, socket, ..
        } = &self.backend
        else {
            return;
        };
        if !self.begin_operation(id, n("Loading saved imports…")) {
            return;
        }
        let request = ListNativeImportsRequest {
            label: selected.label.clone(),
            expected_account_id: selected.id.clone(),
            after,
            limit: PAGE_LIMIT,
        };
        let socket = socket.clone();
        let (send, receive) = tokio::sync::oneshot::channel();
        runtime.spawn(async move {
            let _ = send.send(
                cirrove_service::list_native_imports(&socket, &request)
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
            if !ui.card(&selected.id).is_some_and(|current| {
                current.can_list_native_imports()
                    && current.same_native_import_connection(&selected)
            }) {
                ui.refresh();
                return;
            }
            match result {
                Ok(Ok(reply)) if page_valid(after, &reply) => {
                    ui.saved_native_imports_dialog(selected, reply)
                }
                _ => ui.notify(&gettext(
                    "Could not load saved imports. Nothing was submitted or retried.",
                )),
            }
            ui.refresh();
        });
    }
    fn saved_native_imports_dialog(
        self: &Rc<Self>,
        selected: AccountCard,
        reply: cirrove_service::ListNativeImportsReply,
    ) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let dialog = adw::AlertDialog::new(
            Some(&gettext("Saved iWork imports")),
            Some(&gettext(
                "These are retained import records, not a current cloud listing. Checking observes an existing operation; it never submits another copy.",
            )),
        );
        let empty_page = reply.operations.is_empty();
        let group = adw::PreferencesGroup::new();
        for saved in reply.operations {
            let row = adw::ActionRow::builder()
                .title(&saved.name)
                .subtitle(historical_detail(&saved))
                .use_markup(false)
                .subtitle_lines(0)
                .build();
            let check = gtk::Button::builder()
                .label(gettext("Check saved import"))
                .valign(gtk::Align::Center)
                .sensitive(selected.can_watch_native_import())
                .build();
            check.set_tooltip_text(Some(&gettext(
                "An active writable iCloud mount is required to check current availability.",
            )));
            let weak = Rc::downgrade(self);
            let selected = selected.clone();
            let weak_dialog = dialog.downgrade();
            check.connect_clicked(move |_| {
                let Some(ui) = weak.upgrade().filter(|ui| !ui.closed.get()) else {
                    return;
                };
                ui.watch_saved_native_import(
                    &selected,
                    WatchNativeImportRequest {
                        label: selected.label.clone(),
                        expected_account_id: selected.id.clone(),
                        operation: saved.operation,
                    },
                );
                if let Some(dialog) = weak_dialog.upgrade() {
                    dialog.close();
                }
            });
            row.add_suffix(&check);
            group.add(&row);
        }
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.append(&group);
        if empty_page {
            let empty = gtk::Label::new(Some(&gettext("No saved imports on this page.")));
            empty.set_wrap(true);
            content.append(&empty);
        }
        if !selected.can_watch_native_import() {
            let notice = gtk::Label::new(Some(&gettext(
                "History is available without uploading. Checking current availability requires an active writable iCloud mount.",
            )));
            notice.set_wrap(true);
            content.append(&notice);
        }
        dialog.set_extra_child(Some(&content));
        dialog.add_response("close", &gettext("Close"));
        dialog.set_close_response("close");
        if reply.next.is_some() {
            dialog.add_response("next", &gettext("Next page"));
        }
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            if response == "next"
                && let Some(ui) = weak.upgrade().filter(|ui| !ui.closed.get())
                && let Some(next) = reply.next
                && ui.card(&selected.id).is_some_and(|current| {
                    current.can_list_native_imports()
                        && current.same_native_import_connection(&selected)
                })
            {
                ui.load_saved_native_imports(&selected.id, Some(next));
            }
        });
        dialog.present(Some(&window));
        dialog.set_content_width(520);
    }
    /// Only attach an observer to the exact selected operation. No import/retry.
    pub fn watch_saved_native_import(
        self: &Rc<Self>,
        selected: &AccountCard,
        request: WatchNativeImportRequest,
    ) {
        if request.label != selected.label
            || request.expected_account_id != selected.id
            || !self.card(&selected.id).is_some_and(|current| {
                current.can_watch_native_import() && current.same_native_import_connection(selected)
            })
        {
            return;
        }
        // A running observer for this operation already supplies progress.
        if self.card(&selected.id).is_some_and(|current| {
            current.running.iter().any(|job| {
                job.running
                    && job
                        .import_progress
                        .as_ref()
                        .is_some_and(|progress| progress.operation == request.operation)
            })
        }) {
            return;
        }
        let Backend::Live {
            runtime, socket, ..
        } = &self.backend
        else {
            return;
        };
        if !self.begin_operation(&selected.id, n("Checking saved import…")) {
            return;
        }
        let selected = selected.clone();
        let checked = request.clone();
        let socket = socket.clone();
        let (send, receive) = tokio::sync::oneshot::channel();
        runtime.spawn(async move {
            let _ = send.send(
                cirrove_service::watch_native_import(&socket, &request)
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
            if ui.card(&selected.id).is_some_and(|current| {
                current.can_watch_native_import()
                    && current.same_native_import_connection(&selected)
            }) {
                match result {
                    Ok(Ok(reply)) if watch_reply_valid(&checked,&reply)=> {
                        // Publish the accepted observer before a delayed status refresh.
                        // A second click must already see this exact operation running.
                        if let Some(job)=&reply.job && let Some(mut view)=ui.current()
                            && let Some(card)=view.accounts.iter_mut().find(|card|card.id==selected.id)
                        {
                            card.running.retain(|old|old.import_progress.as_ref().is_none_or(|progress|progress.operation!=checked.operation));
                            card.running.push(crate::model::RunningJob::from_job(job));
                            ui.render(view);
                        }
                        ui.notify(&gettext("Checking the saved import. Progress appears with this connection's transfers."));
                    },
                    _=>ui.notify(&gettext("Could not confirm the saved import check. Its operation remains retained; no new copy was submitted.")),
                }
            }
            ui.refresh();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn selection() -> cirrove_service::journal::NativeImportSelection {
        cirrove_service::journal::NativeImportSelection {
            operation: "00000000-0000-4000-8000-000000000001"
                .parse()
                .expect("fixture UUID"),
            sequence: 2,
            state: cirrove_service::journal::UploadState::Uploaded,
            parent: "FOLDER::com.apple.CloudDocs::root".into(),
            name: "Owned.pages".into(),
            remote_item: Some("FILE::com.apple.CloudDocs::owned".into()),
            completion_receipt_recorded: true,
        }
    }
    #[test]
    fn saved_import_pages_require_bounded_advancing_historical_contract() {
        let empty = cirrove_service::ListNativeImportsReply {
            next: Some(1),
            ..Default::default()
        };
        assert!(page_valid(None, &empty));
        let valid = cirrove_service::ListNativeImportsReply {
            operations: vec![selection()],
            next: Some(2),
            refusal: None,
        };
        assert!(page_valid(Some(1), &valid));
        for arm in [
            "cursor",
            "sequence",
            "duplicates",
            "count",
            "receipt",
            "remote",
            "name",
            "parent",
            "refusal",
        ] {
            let mut changed = valid.clone();
            match arm {
                "cursor" => changed.next = Some(1),
                "sequence" => changed.operations[0].sequence = 1,
                "duplicates" => changed.operations.push(selection()),
                "count" => changed.operations = vec![selection(); 26],
                "receipt" => changed.operations[0].completion_receipt_recorded = false,
                "remote" => changed.operations[0].remote_item = None,
                "name" => changed.operations[0].name = "bad\n.pages".into(),
                "parent" => changed.operations[0].parent = "other".into(),
                _ => changed.refusal = Some("fixture refusal".into()),
            }
            assert!(!page_valid(Some(1), &changed), "{arm}");
        }
    }
    #[test]
    fn saved_import_watch_accepts_only_exact_existing_operation_and_job_kind() {
        let request = WatchNativeImportRequest {
            label: "selected".into(),
            expected_account_id: "selected-id".into(),
            operation: selection().operation,
        };
        let valid = cirrove_service::ImportNativePackageReply {
            job: Some(cirrove_service::jobs::Job {
                id: "observer".into(),
                kind: cirrove_service::jobs::JobKind::ImportNativePackage,
                native_import: Some(cirrove_service::jobs::NativeImportProgress {
                    operation: request.operation,
                    remote: None,
                }),
                ..Default::default()
            }),
            refusal: None,
        };
        assert!(watch_reply_valid(&request, &valid));
        for arm in ["operation", "kind", "progress", "job", "refusal"] {
            let mut changed = valid.clone();
            match arm {
                "operation" => {
                    changed
                        .job
                        .as_mut()
                        .expect("fixture job")
                        .native_import
                        .as_mut()
                        .expect("fixture progress")
                        .operation = "00000000-0000-4000-8000-000000000002"
                        .parse()
                        .expect("fixture UUID")
                }
                "kind" => {
                    changed.job.as_mut().expect("fixture job").kind =
                        cirrove_service::jobs::JobKind::KeepOffline
                }
                "progress" => changed.job.as_mut().expect("fixture job").native_import = None,
                "job" => changed.job = None,
                _ => changed.refusal = Some("fixture refusal".into()),
            }
            assert!(!watch_reply_valid(&request, &changed), "{arm}");
        }
    }
}
