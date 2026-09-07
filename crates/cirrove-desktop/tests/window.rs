#![allow(clippy::unwrap_used)]
use adw::prelude::*;
use cirrove_desktop::{
    demo,
    model::ConnectionState,
    ui::{Backend, Window},
};
use gtk::{gio, glib};
use std::{
    cell::Cell,
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
    if let Some(button) = widget.downcast_ref::<gtk::Button>()
        && (button.label().as_deref() == Some(label)
            || button.tooltip_text().as_deref() == Some(label))
    {
        return Some(button.clone());
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(found) = button(&current, label) {
            return Some(found);
        }
        child = current.next_sibling();
    }
    None
}

#[test]
#[ignore = "requires a graphical display; synthetic local socket/accounts only"]
fn native_window_keeps_focus_and_waits_for_service_mount_acknowledgement() {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::UnixListener,
    };
    adw::init().unwrap();
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
    std::fs::write(
        state.join("accounts.json"),
        serde_json::to_vec(&settings).unwrap(),
    )
    .unwrap();
    let response = Arc::new(Mutex::new(sample.status.unwrap()));
    let socket = temp.path().join("control.sock");
    let server = {
        let _context = runtime.enter();
        UnixListener::bind(&socket).unwrap()
    };
    let replies = response.clone();
    let hold_next = Arc::new(AtomicBool::new(false));
    let hold = hold_next.clone();
    let reply_gate = Arc::new(tokio::sync::Semaphore::new(0));
    let gate = reply_gate.clone();
    let server = runtime.spawn(async move {
        let mut clients = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = server.accept() => {
                    let (mut stream, _) = accepted.unwrap();
                    let replies = replies.clone();
                    let held = hold.swap(false, Ordering::SeqCst);
                    let gate = gate.clone();
                    clients.spawn(async move {
                        let mut command = [0; 7];
                        stream.read_exact(&mut command).await.unwrap();
                        assert_eq!(&command, b"status\n");
                        if held {
                            gate.acquire().await.unwrap().forget();
                        }
                        // A slow service must not block native GTK events.
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        let data = serde_json::to_vec(&*replies.lock().unwrap()).unwrap();
                        let _ = stream.write_all(&data).await;
                    });
                },
                _ = clients.join_next(), if !clients.is_empty() => {},
            }
        }
    });
    let app = adw::Application::builder()
        .application_id("io.github.Dandiccf.Cirrove.Test")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.register(None::<&gio::Cancellable>).unwrap();
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
    window.close();
    // The window owns no daemon lifetime: the same fake service still answers.
    assert!(runtime.block_on(cirrove_service::status(&socket)).is_ok());
    timer.remove();
    server.abort();
    runtime.shutdown_timeout(Duration::from_secs(1));
}
