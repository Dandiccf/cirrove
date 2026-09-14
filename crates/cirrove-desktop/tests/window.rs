#![allow(clippy::unwrap_used)]
//! The window, driven against a daemon that is only a socket. Needs a display.
//!
//! Not under libtest: GTK binds to the first thread that initialises it and
//! refuses every other, and libtest runs each test on its own thread. `main`
//! below runs the scenarios in order on one thread and speaks enough of
//! libtest's dialect -- `--ignored`, filters, the summary line -- that CI's
//! invocation and a developer's `cargo test` both do what they did.
use adw::prelude::*;
use cirrove_desktop::{
    demo,
    model::{ConnectionState, ServiceFailure, SettingsFailure},
    ui::{Backend, Window},
};
use cirrove_service::Status;
use gtk::{gio, glib};
use std::{
    cell::Cell,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

fn pump_until(phase: &str, mut predicate: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(5);
    while !predicate() {
        assert!(
            Instant::now() < until,
            "native desktop condition timed out: {phase}"
        );
        while glib::MainContext::default().pending() {
            glib::MainContext::default().iteration(false);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn button(widget: &gtk::Widget, label: &str) -> Option<gtk::Button> {
    buttons(widget, label).into_iter().next()
}
/// Every mapped button with this label or tooltip, in tree order -- which for
/// per-account controls is account order.
fn buttons(widget: &gtk::Widget, label: &str) -> Vec<gtk::Button> {
    let mut found = Vec::new();
    if let Some(button) = widget.downcast_ref::<gtk::Button>()
        && button.is_mapped()
        && (button.label().as_deref() == Some(label)
            || button.tooltip_text().as_deref() == Some(label))
    {
        found.push(button.clone());
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        found.extend(buttons(&current, label));
        child = current.next_sibling();
    }
    found
}

fn displays_text(widget: &gtk::Widget, text: &str) -> bool {
    if let Some(label) = widget.downcast_ref::<gtk::Label>()
        && label.is_mapped()
        && label.text().as_str() == text
    {
        return true;
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if displays_text(&current, text) {
            return true;
        }
        child = current.next_sibling();
    }
    false
}
/// Every mapped button of ours that shows an icon and no text. GTK's own
/// window controls -- the close button it draws when there is no compositor
/// to draw one, as under Xvfb -- are not ours to name and are skipped.
/// Every mapped control that shows an icon and no words, with the icon it
/// shows, so a caller can name the ones that carry no name.
///
/// GtkButton and GtkMenuButton both, because they are not the same type and a
/// check that knew only the first quietly stopped covering a control the moment
/// one became the other.
fn icon_only_buttons(widget: &gtk::Widget) -> Vec<(gtk::Widget, String)> {
    let mut found = Vec::new();
    if widget.is::<gtk::WindowControls>() {
        return found;
    }
    let icon_and_label = if let Some(button) = widget.downcast_ref::<gtk::Button>() {
        Some((button.icon_name(), button.label()))
    } else {
        widget
            .downcast_ref::<gtk::MenuButton>()
            .map(|button| (button.icon_name(), button.label()))
    };
    if let Some((Some(icon), None)) = icon_and_label
        && widget.is_mapped()
        // libadwaita's header draws the window buttons itself under Xvfb and
        // does not put them in a GtkWindowControls; they carry GTK's
        // `titlebutton` class and a `window-*` icon, and are not ours.
        && !widget.has_css_class("titlebutton")
        && !icon.starts_with("window-")
    {
        found.push((widget.clone(), icon.to_string()));
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        found.extend(icon_only_buttons(&current));
        child = current.next_sibling();
    }
    found
}
/// Rows inside a collapsed expander are not mapped; open every account.
fn expand_all(widget: &gtk::Widget) {
    if let Some(row) = widget.downcast_ref::<adw::ExpanderRow>() {
        row.set_expanded(true);
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        expand_all(&current);
        child = current.next_sibling();
    }
}

/// A daemon that is only a socket: answers `status` from a shared value and
/// records every other request, answering `discard-stuck` the way the daemon
/// would after discarding everything -- and making the next `status` agree.
struct FakeService {
    socket: PathBuf,
    response: Arc<Mutex<Status>>,
    hold_next: Arc<AtomicBool>,
    reply_gate: Arc<tokio::sync::Semaphore>,
    requests: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}
fn fake_service(runtime: &tokio::runtime::Runtime, dir: &Path, status: Status) -> FakeService {
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt},
        net::UnixListener,
    };
    let socket = dir.join("control.sock");
    let server = {
        let _context = runtime.enter();
        UnixListener::bind(&socket).unwrap()
    };
    let response = Arc::new(Mutex::new(status));
    let replies = response.clone();
    let hold_next = Arc::new(AtomicBool::new(false));
    let hold = hold_next.clone();
    let reply_gate = Arc::new(tokio::sync::Semaphore::new(0));
    let gate = reply_gate.clone();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
    let task = runtime.spawn(async move {
        let mut clients = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = server.accept() => {
                    let (stream, _) = accepted.unwrap();
                    let replies = replies.clone();
                    let held = hold.swap(false, Ordering::SeqCst);
                    let gate = gate.clone();
                    let seen = seen.clone();
                    clients.spawn(async move {
                        let mut reader = tokio::io::BufReader::new(stream);
                        let mut line = String::new();
                        reader.read_line(&mut line).await.unwrap();
                        let mut stream = reader.into_inner();
                        let data = if line == "status\n" {
                            if held {
                                gate.acquire().await.unwrap().forget();
                            }
                            // A slow service must not block native GTK events.
                            tokio::time::sleep(Duration::from_millis(200)).await;
                            serde_json::to_vec(&*replies.lock().unwrap()).unwrap()
                        } else if line.starts_with("recent ") {
                            seen.lock().unwrap().push(line.trim_end().to_owned());
                            // One thing from the cloud and one saved here,
                            // for the "work" account only.
                            let reply = if line.contains("\"work\"") {
                                cirrove_service::RecentReply {
                                    remote: vec![cirrove_service::recent::RemoteChange {
                                        at_unix: 1,
                                        id: "r1".into(),
                                        parent_id: None,
                                        name: "Quarterly report.docx".into(),
                                        kind: "file".into(),
                                        size: 10,
                                        removed: false,
                                    }],
                                    local: vec![cirrove_service::recent::LocalChange {
                                        sequence: 1,
                                        name: "Notes.txt".into(),
                                        item: None,
                                        state: "conflict".into(),
                                        size: 3,
                                        // None on purpose: a journal written
                                        // before saves carried a time, which
                                        // must render without a time rather
                                        // than as 1970.
                                        saved_at: None,
                                    }],
                                    // Named, so the window scenario exercises
                                    // the path a person actually reads rather
                                    // than only the count.
                                    stuck: vec![cirrove_service::recent::StuckChange {
                                        what: "delete folder".into(),
                                        name: "Old invoices".into(),
                                        path: Some("Accounts/Old invoices".into()),
                                        state: "conflict".into(),
                                    }],
                                    refusal: None,
                                }
                            } else {
                                cirrove_service::RecentReply::default()
                            };
                            serde_json::to_vec(&reply).unwrap()
                        } else if line.starts_with("unpin ") || line.starts_with("pin ") {
                            seen.lock().unwrap().push(line.trim_end().to_owned());
                            let body: cirrove_service::PinRequest =
                                serde_json::from_str(line.split_once(' ').unwrap().1).unwrap();
                            let mut status = replies.lock().unwrap();
                            let mut accepted = false;
                            for account in &mut status.accounts {
                                if !body.label.is_empty() && account.label != body.label {
                                    continue;
                                }
                                if line.starts_with("unpin ") {
                                    let before = account.pins.len();
                                    account
                                        .pins
                                        .retain(|p| Some(&p.item) != body.item.as_ref());
                                    accepted = account.pins.len() < before;
                                } else {
                                    let path = body.path.clone().unwrap_or_default();
                                    account.pins.push(cirrove_service::engine::PinStatus {
                                        item: format!("id-of-{path}"),
                                        path: Some(path),
                                        recursive: body.recursive,
                                        reserved: 4096,
                                        resident: 4096,
                                        blocks: 1,
                                    });
                                    accepted = true;
                                }
                            }
                            serde_json::to_vec(&cirrove_service::PinReply {
                                accepted,
                                item: body.item.unwrap_or_default(),
                                reserved: 4096,
                                files: 0,
                                complete: true,
                                refusal: None,
                            })
                            .unwrap()
                        } else {
                            seen.lock().unwrap().push(line.trim_end().to_owned());
                            let mut status = replies.lock().unwrap();
                            let mut discarded = 0;
                            for account in &mut status.accounts {
                                discarded += account.stuck_changes;
                                account.stuck_changes = 0;
                            }
                            serde_json::to_vec(&cirrove_service::DiscardReply {
                                discarded,
                                remaining: 0,
                                refusal: None,
                            })
                            .unwrap()
                        };
                        let _ = stream.write_all(&data).await;
                    });
                },
                _ = clients.join_next(), if !clients.is_empty() => {},
            }
        }
    });
    FakeService {
        socket,
        response,
        hold_next,
        reply_gate,
        requests,
        task,
    }
}
fn write_settings(state: &Path, settings: &cirrove_service::accounts::Settings) {
    std::fs::write(
        state.join("accounts.json"),
        serde_json::to_vec(settings).unwrap(),
    )
    .unwrap();
}
/// One application per scenario, each under its own id: GApplication refuses
/// to register the same id twice in one process, and the scenarios share one.
fn application(scenario: &str) -> adw::Application {
    let app = adw::Application::builder()
        .application_id(format!("io.github.Dandiccf.Cirrove.Test.{scenario}"))
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.register(None::<&gio::Cancellable>).unwrap();
    app
}

fn native_window_keeps_focus_and_waits_for_service_mount_acknowledgement() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    cirrove_service::private_dir(&state).unwrap();
    let sample = demo::snapshot().unwrap();
    let settings = sample.settings.unwrap();
    let first = settings.accounts[0].id.clone();
    write_settings(&state, &settings);
    let service = fake_service(&runtime, temp.path(), sample.status.unwrap());
    let response = service.response.clone();
    let hold_next = service.hold_next.clone();
    let reply_gate = service.reply_gate.clone();
    let socket = service.socket.clone();
    let app = application("Focus");
    let pulse = Rc::new(Cell::new(0));
    let count = pulse.clone();
    let timer = glib::timeout_add_local(Duration::from_millis(10), move || {
        count.set(count.get() + 1);
        glib::ControlFlow::Continue
    });
    let ui = Window::new(
        &app,
        Backend::Live {
            runtime: runtime.handle().clone(),
            state: state.clone(),
            socket: socket.clone(),
        },
    );
    pump_until("initial snapshot", || {
        ui.current()
            .is_some_and(|view| view.accounts[0].state == ConnectionState::Connected)
    });
    // Window creation/rendering may outlast a fixed server delay on software
    // rendering. Hold a later response explicitly so GTK must make progress
    // while service I/O is still outstanding, independent of startup speed.
    hold_next.store(true, Ordering::SeqCst);
    ui.refresh();
    pump_until("service accepted held refresh", || {
        !hold_next.load(Ordering::SeqCst)
    });
    let before = pulse.get();
    pump_until("GTK events while service reply is held", || {
        pulse.get() >= before + 5
    });
    assert!(
        ui.current().unwrap().service_reachable,
        "held status request timed out before GTK could run"
    );
    reply_gate.add_permits(1);
    let window = ui.window.upgrade().unwrap();
    let refresh = button(window.upcast_ref(), "Refresh connection status").unwrap();
    assert!(refresh.grab_focus());
    let before = pulse.get();
    ui.refresh();
    pump_until("refresh keeps keyboard focus", || {
        pulse.get() >= before + 25
    });
    assert_eq!(
        gtk::prelude::RootExt::focus(&window),
        Some(refresh.upcast())
    );
    let unmount = button(window.upcast_ref(), "Unmount").unwrap();
    assert!(unmount.grab_focus());
    unmount.emit_clicked();
    pump_until("saved preference awaiting service", || {
        ui.current()
            .is_some_and(|v| v.accounts[0].state == ConnectionState::Unmounting)
    });
    assert!(
        !cirrove_service::accounts::Settings::load(&state)
            .unwrap()
            .accounts[0]
            .enabled
    );
    assert!(
        ui.current().unwrap().accounts[0].mounted,
        "saved preference falsely acknowledged unmount"
    );
    assert_eq!(
        gtk::prelude::RootExt::focus(&window),
        Some(unmount.clone().upcast())
    );
    {
        let mut status = response.lock().unwrap();
        let account = status
            .accounts
            .iter_mut()
            .find(|a| a.account_id == first)
            .unwrap();
        account.enabled = false;
        account.mounted = false;
        account.state = "disabled".into();
        account.feeds.clear();
    }
    ui.refresh();
    pump_until("service unmount acknowledgement", || {
        ui.current()
            .is_some_and(|v| v.accounts[0].state == ConnectionState::Unmounted)
    });
    assert_eq!(
        gtk::prelude::RootExt::focus(&window),
        Some(unmount.clone().upcast())
    );
    assert_eq!(unmount.label().as_deref(), Some("Mount"));
    // Moving focus while an operation is pending must not be undone on refresh.
    unmount.emit_clicked();
    let help = button(window.upcast_ref(), "OneDrive setup guide").unwrap();
    assert!(help.grab_focus());
    pump_until("new mount preference awaiting service", || {
        ui.current()
            .is_some_and(|v| v.accounts[0].state == ConnectionState::Mounting)
    });
    assert_eq!(gtk::prelude::RootExt::focus(&window), Some(help.upcast()));
    // Actual rendered failures must preserve their causes, and a recovered
    // snapshot must clear both warnings without restarting the window.
    let saved = std::fs::read(state.join("accounts.json")).unwrap();
    response.lock().unwrap().protocol_version = 0;
    ui.refresh();
    let incompatible = ServiceFailure::Incompatible {
        expected: cirrove_service::STATUS_PROTOCOL_VERSION,
        actual: 0,
    };
    pump_until("incompatible service banner", || {
        displays_text(window.upcast_ref(), incompatible.description())
    });
    assert!(
        !button(window.upcast_ref(), "Cancel mount")
            .unwrap()
            .is_sensitive()
    );
    std::fs::write(
        state.join("accounts.json"),
        br#"{"version":"PRIVATE CONTENT"}"#,
    )
    .unwrap();
    ui.refresh();
    pump_until("invalid settings description", || {
        ui.current()
            .and_then(|view| view.settings_error)
            .is_some_and(|error| {
                matches!(error, SettingsFailure::InvalidFormat { .. })
                    && displays_text(window.upcast_ref(), &error.description())
            })
    });
    assert!(displays_text(
        window.upcast_ref(),
        incompatible.description()
    ));
    response.lock().unwrap().protocol_version = cirrove_service::STATUS_PROTOCOL_VERSION;
    ui.refresh();
    pump_until("service recovered while settings remain invalid", || {
        ui.current()
            .is_some_and(|view| view.service_error.is_none() && view.settings_error.is_some())
            && !displays_text(window.upcast_ref(), incompatible.description())
    });
    std::fs::write(state.join("accounts.json"), saved).unwrap();
    // The settings page has its own Retry even without a service-error banner.
    button(window.upcast_ref(), "Retry").unwrap().emit_clicked();
    pump_until("recovered settings and service", || {
        ui.current().is_some_and(|view| {
            view.settings_error.is_none()
                && view.service_error.is_none()
                && view.accounts.len() == 2
        }) && !displays_text(window.upcast_ref(), incompatible.description())
    });
    assert!(!displays_text(
        window.upcast_ref(),
        incompatible.description()
    ));
    window.close();
    // The window owns no daemon lifetime: the same fake service still answers.
    assert!(runtime.block_on(cirrove_service::status(&socket)).is_ok());
    timer.remove();
    service.task.abort();
    runtime.shutdown_timeout(Duration::from_secs(1));
}

/// Every failure kind reaches the rendered window with its own words.
///
/// Two of them were driven through the window before -- an incompatible service
/// and an invalid settings file -- and the other seven were asserted only as
/// strings on the model, where a wrong widget, a hidden banner or a truncated
/// label would not have shown. Rendering each one and reading the text back out
/// of the widget tree is the difference between "the sentence exists" and "a
/// person sees it".
///
/// It also holds them apart. A failure that shares another's words sends
/// someone to the wrong remedy, and these differ: a settings file that cannot
/// be read is a permissions problem, one that is invalid is a restore, an
/// unreachable service is a start, and an incompatible one is an update.
fn every_failure_kind_reaches_the_window_with_its_own_words() {
    use cirrove_desktop::model::{Overview, ServiceFailure, SettingsFailure};
    use std::io::ErrorKind;

    let app = application("failure kinds");
    let ui = Window::new(&app, Backend::Demo);
    let window = ui.window.upgrade().unwrap();
    window.present();

    let settings = [
        SettingsFailure::Read(ErrorKind::PermissionDenied),
        SettingsFailure::Read(ErrorKind::IsADirectory),
        SettingsFailure::Read(ErrorKind::Other),
        SettingsFailure::InvalidFormat {
            line: 3,
            column: 11,
        },
        SettingsFailure::InvalidConfiguration,
        SettingsFailure::WorkerUnavailable,
    ];
    let service = [
        ServiceFailure::Io(ErrorKind::NotFound),
        ServiceFailure::Io(ErrorKind::PermissionDenied),
        ServiceFailure::Io(ErrorKind::ConnectionRefused),
        ServiceFailure::TimedOut,
        ServiceFailure::InvalidResponse,
        ServiceFailure::Incompatible {
            expected: 1,
            actual: 0,
        },
    ];

    // Variant label -> the sentence the window actually displayed.
    let mut words: Vec<(String, String)> = Vec::new();
    for failure in &settings {
        let mut view = Overview::from_snapshot(demo::snapshot().unwrap());
        view.settings_available = false;
        view.settings_error = Some(failure.clone());
        view.accounts.clear();
        ui.render(view);
        let text = failure.description();
        pump_until(&format!("settings failure rendered: {text}"), || {
            displays_text(window.upcast_ref(), &text)
        });
        words.push((format!("{failure:?}"), text));
    }
    for failure in &service {
        let mut view = Overview::from_snapshot(demo::snapshot().unwrap());
        view.service_error = Some(failure.clone());
        ui.render(view);
        let text = failure.description().to_owned();
        pump_until(&format!("service failure rendered: {text}"), || {
            displays_text(window.upcast_ref(), &text)
        });
        words.push((format!("{failure:?}"), text));
    }

    assert_eq!(words.len(), 12, "every variant must have been rendered");

    // Sharing words is allowed only where two variants are the same problem
    // with the same remedy. There is exactly one such pair: a socket that is
    // not there and one that refuses both mean no daemon is listening, and the
    // answer to both is to start it. Anything else sharing words would send
    // someone to the wrong remedy, so the grouping is stated here rather than
    // left for a count to imply.
    let mut grouped: std::collections::BTreeMap<&str, Vec<&str>> = Default::default();
    for (variant, sentence) in &words {
        grouped.entry(sentence).or_default().push(variant);
    }
    let shared: Vec<Vec<&str>> = grouped
        .values()
        .filter(|variants| variants.len() > 1)
        .cloned()
        .collect();
    assert_eq!(
        shared,
        vec![vec!["Io(NotFound)", "Io(ConnectionRefused)"]],
        "the only failures allowed to share words are the two that mean no daemon is listening"
    );
    window.close();
}

/// The actions a terminal used to be needed for -- connecting a drive, signing
/// in again, discarding changes the cloud refused, removing a connection -- are
/// in the window, and each is offered exactly where it applies: sign-in on the
/// account that needs it, discard where something was refused, removal only on
/// an unmounted account.
fn every_account_action_is_offered_from_the_window_and_only_where_it_applies() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    cirrove_service::private_dir(&state).unwrap();
    let sample = demo::snapshot().unwrap();
    let mut settings = sample.settings.unwrap();
    // One of each access level, so the consent button can be seen to offer the
    // other direction rather than one fixed word.
    settings.accounts[0].access = cirrove_auth::AccessMode::ReadWrite;
    write_settings(&state, &settings);
    // The mounted account has lost its grant and holds two refused changes; the
    // second account is saved but unmounted.
    let mut status = sample.status.unwrap();
    {
        let work = &mut status.accounts[0];
        work.feeds[0].state = "sign_in_required".into();
        work.stuck_changes = 2;
        work.failed_uploads = 1;
    }
    let service = fake_service(&runtime, temp.path(), status);
    let app = application("Actions");
    let ui = Window::new(
        &app,
        Backend::Live {
            runtime: runtime.handle().clone(),
            state: state.clone(),
            socket: service.socket.clone(),
        },
    );
    pump_until("initial snapshot", || {
        ui.current().is_some_and(|view| {
            view.accounts[0].state == ConnectionState::SignInRequired
                && view.accounts[1].state == ConnectionState::Unmounted
        })
    });
    let window = ui.window.upgrade().unwrap();
    expand_all(window.upcast_ref());
    pump_until("accounts expanded", || {
        buttons(window.upcast_ref(), "Remove").len() == 2
    });

    // Sign-in is offered on the account that needs it and nowhere else.
    let sign_in = buttons(window.upcast_ref(), "Sign in again");
    assert_eq!(sign_in.len(), 1, "one account needs a sign-in");
    assert!(sign_in[0].is_sensitive());

    // Removal is offered on both, but a mounted account must be unmounted first.
    let remove = buttons(window.upcast_ref(), "Remove");
    assert!(
        !remove[0].is_sensitive(),
        "a mounted account must not be removable under a running mount"
    );
    assert!(remove[1].is_sensitive());

    // A save that did not reach the cloud is shown as its own warning,
    // distinct from a refused namespace change, and without a discard button:
    // its remedy is re-saving. This is the surfacing a power cut's failed save
    // was missing.
    assert!(displays_text(
        window.upcast_ref(),
        "Saves that did not reach the cloud"
    ));
    assert!(
        ui.current()
            .is_some_and(|v| v.accounts[0].failed_uploads == 1),
        "the failed save reached the window"
    );

    // Consent is changed from the window, in whichever direction the account is
    // not already in. This was the last ordinary flow that needed a terminal.
    // The click is deliberately not exercised: it opens the provider's sign-in
    // in a browser, which is the point -- the consent screen is what grants
    // anything, not this button.
    let allow = buttons(window.upcast_ref(), "Allow changes");
    let restrict = buttons(window.upcast_ref(), "Make read-only");
    assert_eq!(
        allow.len(),
        1,
        "the read-only account offers to allow changes"
    );
    assert_eq!(
        restrict.len(),
        1,
        "the writable account offers to go back to read-only"
    );
    assert!(
        restrict[0]
            .tooltip_text()
            .is_some_and(|t| t.contains("only to read")),
        "the button says what the sign-in will ask for"
    );
    // And does not overstate it. Asking for the narrower scope stops Cirrove
    // writing; it does not take the permission away, which only the person can
    // do in their Microsoft account. A tooltip promising the latter would be a
    // promise this button cannot keep.
    assert!(
        restrict[0]
            .tooltip_text()
            .is_some_and(|t| t.contains("Microsoft account")),
        "the read-only button must say where the permission itself is withdrawn"
    );
    assert!(
        allow[0].has_css_class("suggested-action"),
        "asking for write is the direction that grants something"
    );
    assert!(
        !restrict[0].has_css_class("suggested-action"),
        "asking to read less is ordinary"
    );

    // Refused changes are shown, and discarding them goes to the daemon.
    assert!(displays_text(
        window.upcast_ref(),
        "Changes the cloud refused"
    ));
    let discard = buttons(window.upcast_ref(), "Discard");
    assert_eq!(
        discard.len(),
        1,
        "only the account with refused changes offers a discard"
    );
    discard[0].emit_clicked();
    pump_until("discard reached the daemon", || {
        service
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.starts_with("discard-stuck ") && line.contains("\"work\""))
    });
    pump_until("the refused changes are gone from the window", || {
        ui.current().is_some_and(|v| v.accounts[0].stuck == 0)
            && !displays_text(window.upcast_ref(), "Changes the cloud refused")
    });

    // What changed lately is shown: the cloud's change and the local save,
    // the refused one marked.
    pump_until("recent activity rendered", || {
        displays_text(window.upcast_ref(), "Quarterly report.docx")
            && displays_text(
                window.upcast_ref(),
                "work · saved here · the cloud refused it",
            )
    });
    assert!(
        ui.current().is_some_and(|v| v
            .activity
            .iter()
            .any(|e| e.name == "Notes.txt" && e.warning)),
        "a refused save is an entry that warns"
    );

    // The tray notice must match what the session bus actually says, not what
    // the machine running the test happens to have. A developer's desktop has a
    // tray host and CI's bare X server does not, and asserting either one made
    // this a test of the environment: written assuming a host, it failed on CI
    // for being right. So the expectation is asked for rather than assumed, the
    // same way the window asks.
    const NOTICE: &str = "No tray icon: this desktop has no tray. On GNOME, install the \
                          AppIndicator extension; most other desktops provide one.";
    let host = runtime.block_on(cirrove_desktop::tray::host_present());
    let expected = cirrove_desktop::tray::notice_when(host);
    pump_until("the tray notice settles", || {
        displays_text(window.upcast_ref(), NOTICE) == expected
    });
    assert_eq!(
        displays_text(window.upcast_ref(), NOTICE),
        expected,
        "the bus says host_present={host:?}, so the notice should be shown={expected}"
    );

    // Every button that is only an icon has a name: the tooltip a pointer
    // shows and, set from the same words, the label a screen reader says.
    let nameless: Vec<String> = icon_only_buttons(window.upcast_ref())
        .into_iter()
        .filter(|(w, _)| w.tooltip_text().is_none_or(|t| t.is_empty()))
        .map(|(_, icon)| icon)
        .collect();
    assert!(
        nameless.is_empty(),
        "icon-only buttons without a name: {nameless:?}"
    );
    assert!(
        !icon_only_buttons(window.upcast_ref()).is_empty(),
        "the check saw the icon buttons at all"
    );

    // A drive can be connected from the window; with nothing filled in there
    // is nothing to sign in with yet.
    button(window.upcast_ref(), "Connect a drive")
        .unwrap()
        .emit_clicked();
    pump_until("connect dialog", || {
        button(window.upcast_ref(), "Sign in with Microsoft").is_some()
    });
    assert!(
        !button(window.upcast_ref(), "Sign in with Microsoft")
            .unwrap()
            .is_sensitive(),
        "sign-in must wait for a name, a folder and an application id"
    );

    window.close();
    service.task.abort();
    runtime.shutdown_timeout(Duration::from_secs(1));
}

/// What an account keeps offline, in the window, and taking one back.
///
/// Pinning existed only in the CLI and the Files context menu, so the question
/// "what is my cache actually spent on" had no answer anywhere a person looks.
/// The row has to name the file rather than the provider id it is keyed on --
/// a list of 016WYNLZ... is a list nobody can act on -- and it has to separate
/// what a pin reserved from what is really on disk, because a pin that fetched
/// nothing keeps nothing.
fn the_window_shows_what_is_kept_offline_and_can_release_it() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    cirrove_service::private_dir(&state).unwrap();
    let sample = demo::snapshot().unwrap();
    let settings = sample.settings.unwrap();
    write_settings(&state, &settings);

    let mut status = sample.status.unwrap();
    {
        let work = &mut status.accounts[0];
        work.pins = vec![
            cirrove_service::engine::PinStatus {
                item: "016WYNLZUU5GZ4UDRX2JFJDE3W7US5P7FK".into(),
                path: Some("Dokumente/Report.docx".into()),
                recursive: false,
                reserved: 67_826,
                resident: 67_826,
                blocks: 1,
            },
            // Reserved and never fetched: the row must not claim it is here.
            cirrove_service::engine::PinStatus {
                item: "016WYNLZQGNQJQG5PLTRB2AKFOQ55TUAMV".into(),
                path: Some("Anlagen".into()),
                recursive: true,
                reserved: 5 * 1024 * 1024,
                resident: 0,
                blocks: 0,
            },
            // No path: the index cannot place it, so the id is all there is.
            cirrove_service::engine::PinStatus {
                item: "016WYNLZORPHANORPHANORPHANORPHAN".into(),
                path: None,
                recursive: false,
                reserved: 1024,
                resident: 1024,
                blocks: 1,
            },
        ];
        work.pin_budget = cirrove_service::engine::PinBudget {
            cache_bytes: 5 * 1024 * 1024 * 1024,
            pinnable_bytes: 4 * 1024 * 1024 * 1024,
            reserved_bytes: 5 * 1024 * 1024 + 68_850,
            free_bytes: 4 * 1024 * 1024 * 1024 - (5 * 1024 * 1024 + 68_850),
        };
    }
    let service = fake_service(&runtime, temp.path(), status);
    let app = application("KeptOffline");
    let ui = Window::new(
        &app,
        Backend::Live {
            runtime: runtime.handle().clone(),
            state: state.clone(),
            socket: service.socket.clone(),
        },
    );
    pump_until("initial snapshot", || {
        ui.current()
            .is_some_and(|view| view.accounts[0].kept_offline.len() == 3)
    });
    let window = ui.window.upgrade().unwrap();
    expand_all(window.upcast_ref());
    pump_until("the kept-offline section is open", || {
        displays_text(window.upcast_ref(), "Kept offline")
    });

    // Named by path, not by provider id.
    assert!(
        displays_text(window.upcast_ref(), "Dokumente/Report.docx"),
        "a pin is named by where it is"
    );
    // What is really on disk, separately from what was reserved.
    assert!(
        displays_text(window.upcast_ref(), "66 KB on this computer"),
        "a fetched pin says how much is here"
    );
    assert!(
        displays_text(
            window.upcast_ref(),
            "5.0 MB reserved, nothing fetched yet · everything inside it"
        ),
        "a folder pin that fetched nothing must not claim the space is filled"
    );
    // And where there is no path, the id, because half a path would name a
    // different file.
    assert!(
        displays_text(window.upcast_ref(), "016WYNLZORPHANORPHANORPHANORPHAN"),
        "a pin the index cannot place still appears"
    );

    // Releasing one goes to the daemon by item id, and the row goes away.
    let release = buttons(window.upcast_ref(), "Stop keeping");
    assert_eq!(release.len(), 3, "every kept item can be released");
    release[0].emit_clicked();
    pump_until("the release reached the daemon", || {
        service.requests.lock().unwrap().iter().any(|line| {
            line.starts_with("unpin ") && line.contains("016WYNLZUU5GZ4UDRX2JFJDE3W7US5P7FK")
        })
    });
    pump_until("the released item left the window", || {
        ui.current()
            .is_some_and(|v| v.accounts[0].kept_offline.len() == 2)
            && !displays_text(window.upcast_ref(), "Dokumente/Report.docx")
    });

    // Keeping something new: a path inside the mount is sent mount-relative.
    let mount = ui.current().unwrap().accounts[0].mount_path.clone();
    ui.keep_path(
        &ui.current().unwrap().accounts[0].id.clone(),
        &mount.join("Anlagen/Neu.txt"),
    );
    pump_until("the pin reached the daemon, mount-relative", || {
        service
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.starts_with("pin ") && line.contains("\"Anlagen/Neu.txt\""))
    });

    // A path outside the drive is refused here rather than sent.
    let before = service.requests.lock().unwrap().len();
    ui.keep_path(
        &ui.current().unwrap().accounts[0].id.clone(),
        std::path::Path::new("/etc/hosts"),
    );
    assert_eq!(
        service.requests.lock().unwrap().len(),
        before,
        "a path outside the drive must not be sent to the daemon"
    );

    window.close();
    service.task.abort();
    runtime.shutdown_timeout(Duration::from_secs(1));
}

const SCENARIOS: &[(&str, fn())] = &[
    (
        "every_failure_kind_reaches_the_window_with_its_own_words",
        every_failure_kind_reaches_the_window_with_its_own_words,
    ),
    (
        "native_window_keeps_focus_and_waits_for_service_mount_acknowledgement",
        native_window_keeps_focus_and_waits_for_service_mount_acknowledgement,
    ),
    (
        "every_account_action_is_offered_from_the_window_and_only_where_it_applies",
        every_account_action_is_offered_from_the_window_and_only_where_it_applies,
    ),
    (
        "the_window_shows_what_is_kept_offline_and_can_release_it",
        the_window_shows_what_is_kept_offline_and_can_release_it,
    ),
];
const NEEDS: &str = "requires a graphical display; synthetic local socket/accounts only";

fn main() {
    let mut args = std::env::args().skip(1);
    let mut run_ignored = false;
    let mut filters = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--ignored" | "--include-ignored" => run_ignored = true,
            // Flags that take a value we do not use.
            "--test-threads" | "--skip" | "--color" | "--format" | "--logfile" | "-Z" => {
                args.next();
            }
            flag if flag.starts_with('-') => {}
            _ => filters.push(arg),
        }
    }
    let selected: Vec<_> = SCENARIOS
        .iter()
        .filter(|(name, _)| filters.is_empty() || filters.iter().any(|f| name.contains(f.as_str())))
        .collect();
    println!("\nrunning {} tests", selected.len());
    if !run_ignored {
        for (name, _) in &selected {
            println!("test {name} ... ignored, {NEEDS}");
        }
        println!(
            "\ntest result: ok. 0 passed; 0 failed; {} ignored; 0 measured; {} filtered out\n",
            selected.len(),
            SCENARIOS.len() - selected.len()
        );
        return;
    }
    adw::init().unwrap();
    let mut failed = 0;
    for (name, run) in &selected {
        match std::panic::catch_unwind(run) {
            Ok(()) => println!("test {name} ... ok"),
            Err(_) => {
                failed += 1;
                println!("test {name} ... FAILED");
            }
        }
    }
    println!(
        "\ntest result: {}. {} passed; {failed} failed; 0 ignored; 0 measured; {} filtered out\n",
        if failed == 0 { "ok" } else { "FAILED" },
        selected.len() - failed,
        SCENARIOS.len() - selected.len()
    );
    if failed > 0 {
        std::process::exit(101);
    }
}
