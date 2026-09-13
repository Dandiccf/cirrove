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
/// Every mapped button that shows an icon and no text.
fn icon_only_buttons(widget: &gtk::Widget) -> Vec<gtk::Button> {
    let mut found = Vec::new();
    if let Some(button) = widget.downcast_ref::<gtk::Button>()
        && button.is_mapped()
        && button.icon_name().is_some()
        && button.label().is_none()
    {
        found.push(button.clone());
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
    let settings = sample.settings.unwrap();
    write_settings(&state, &settings);
    // The mounted account has lost its grant and holds two refused changes; the
    // second account is saved but unmounted.
    let mut status = sample.status.unwrap();
    {
        let work = &mut status.accounts[0];
        work.feeds[0].state = "sign_in_required".into();
        work.stuck_changes = 2;
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

    // Every button that is only an icon has a name: the tooltip a pointer
    // shows and, set from the same words, the label a screen reader says.
    let nameless: Vec<String> = icon_only_buttons(window.upcast_ref())
        .into_iter()
        .filter(|b| b.tooltip_text().is_none_or(|t| t.is_empty()))
        .map(|b| b.icon_name().map(|n| n.to_string()).unwrap_or_default())
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

const SCENARIOS: &[(&str, fn())] = &[
    (
        "native_window_keeps_focus_and_waits_for_service_mount_acknowledgement",
        native_window_keeps_focus_and_waits_for_service_mount_acknowledgement,
    ),
    (
        "every_account_action_is_offered_from_the_window_and_only_where_it_applies",
        every_account_action_is_offered_from_the_window_and_only_where_it_applies,
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
