use adw::prelude::*;
use anyhow::{Context, Result, ensure};
use cirrove_desktop::{
    model::{ConnectionState, ServiceFailure, SettingsFailure},
    ui::{Backend, Window},
};
use clap::{Parser, ValueEnum};
use gtk::{gio, glib};
use std::{
    cell::{Cell, RefCell},
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

#[derive(Clone, Copy, ValueEnum)]
enum DemoState {
    Connected,
    ServiceOffline,
    ServiceTimeout,
    IncompatibleService,
    InvalidSettings,
    SignInRequired,
    Empty,
    LongNames,
}
#[derive(Parser)]
#[command(
    version,
    about = "Cirrove cloud account settings (development preview)"
)]
struct Args {
    #[arg(long, requires = "socket", conflicts_with = "demo")]
    state_dir: Option<PathBuf>,
    #[arg(long, requires = "state_dir", conflicts_with = "demo")]
    socket: Option<PathBuf>,
    /// Show synthetic accounts without reading or changing real settings.
    #[arg(long)]
    demo: bool,
    #[arg(long, value_enum, requires = "demo")]
    demo_state: Option<DemoState>,
    /// Save this synthetic window to a PNG and exit; requires a graphical display.
    #[arg(long, requires = "demo")]
    snapshot: Option<PathBuf>,
    #[arg(long, requires = "demo")]
    dark: bool,
    #[arg(long, requires = "demo", conflicts_with = "dark")]
    light: bool,
    /// Expand the first sample account for interface inspection.
    #[arg(long, requires = "demo")]
    demo_details: bool,
}
fn main() -> Result<()> {
    let args = Args::parse();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let backend = if args.demo {
        Backend::Demo
    } else {
        let state = match args.state_dir {
            Some(p) => p,
            None => cirrove_service::state_dir()?,
        };
        let socket = match args.socket {
            Some(p) => p,
            None => cirrove_service::socket_path()?,
        };
        ensure!(
            state.is_absolute() && socket.is_absolute(),
            "state and socket paths must be absolute"
        );
        Backend::Live {
            runtime: runtime.handle().clone(),
            state,
            socket,
        }
    };
    // Isolated developer paths/demos must not activate a production instance.
    let isolated = args.demo
        || matches!(&backend,Backend::Live{state,..} if cirrove_service::state_dir().ok().as_ref()!=Some(state));
    let app = adw::Application::builder()
        .application_id("io.github.Dandiccf.Cirrove")
        .flags(if isolated {
            gio::ApplicationFlags::NON_UNIQUE
        } else {
            gio::ApplicationFlags::FLAGS_NONE
        })
        .build();
    let current = Rc::new(RefCell::new(None::<Rc<Window>>));
    let owner = current.clone();
    let failed = Rc::new(Cell::new(false));
    let result = failed.clone();
    app.connect_activate(move |app| {
        if let Some(window) = owner
            .borrow()
            .as_ref()
            .and_then(|ui| ui.window.upgrade())
            .filter(|w| w.is_visible())
        {
            window.present();
            return;
        }
        if args.dark || args.light {
            adw::StyleManager::default().set_color_scheme(if args.dark {
                adw::ColorScheme::ForceDark
            } else {
                adw::ColorScheme::ForceLight
            });
        }
        let ui = Window::new(app, backend.clone());
        if args.demo && let Some(mut view) = ui.current() {
            match args.demo_state.unwrap_or(DemoState::Connected) {
                DemoState::Connected => (),
                DemoState::Empty => view.accounts.clear(),
                DemoState::ServiceOffline => {
                    view.service_reachable = false;
                    view.service_error = Some(ServiceFailure::Io(std::io::ErrorKind::NotFound));
                    for account in &mut view.accounts {
                        account.mounted = false;
                        account.controls_available = false;
                        account.state = ConnectionState::ServiceUnavailable;
                    }
                },
                DemoState::ServiceTimeout | DemoState::IncompatibleService => {
                    let incompatible = matches!(args.demo_state, Some(DemoState::IncompatibleService));
                    view.service_reachable = incompatible;
                    view.service_error = Some(if incompatible {
                        ServiceFailure::Incompatible { expected: cirrove_service::STATUS_PROTOCOL_VERSION, actual: 0 }
                    } else { ServiceFailure::TimedOut });
                    for account in &mut view.accounts {
                        account.mounted = false;
                        account.controls_available = false;
                        account.state = if incompatible { ConnectionState::IncompatibleService } else { ConnectionState::ServiceUnavailable };
                    }
                },
                DemoState::InvalidSettings => {
                    view.accounts.clear();
                    view.settings_available = false;
                    view.settings_error = Some(SettingsFailure::InvalidFormat { line: 12, column: 8 });
                },
                DemoState::SignInRequired => {
                    if let Some(account) = view.accounts.first_mut() {
                        account.state = ConnectionState::SignInRequired;
                    }
                },
                DemoState::LongNames => {
                    if let Some(account) = view.accounts.first_mut() {
                        account.title = "OneDrive · Gemeinsame Dokumente – Geschäftsentwicklung & internationale Zusammenarbeit".into();
                        account.username = "alexander.langer.name@example.invalid".into();
                    }
                }
            }
            ui.render(view);
        }
        if args.demo_details && let Some(window) = ui.window.upgrade() {
            expand_first_account(window.upcast_ref());
        }
        if let Some(path) = args.snapshot.clone() {
            let window = ui.window.clone();
            if let Some(window) = window.upgrade() {
                window.set_size_request(660, 620);
            }
            let app = app.clone();
            let failed = result.clone();
            glib::timeout_add_local_once(Duration::from_secs(1), move || {
                let result = window
                    .upgrade()
                    .context("preview window closed")
                    .and_then(|window| snapshot(&window, &path));
                if let Err(error) = result {
                    eprintln!("Could not capture synthetic preview: {error}");
                    failed.set(true);
                }
                app.quit();
            });
        }
        *owner.borrow_mut() = Some(ui);
    });
    app.connect_shutdown(move |_| {
        current.borrow_mut().take();
    });
    let exit = app.run_with_args(&["cirrove-desktop"]);
    runtime.shutdown_timeout(Duration::from_secs(2));
    ensure!(
        exit == glib::ExitCode::SUCCESS && !failed.get(),
        "desktop application failed"
    );
    Ok(())
}
fn expand_first_account(widget: &gtk::Widget) -> bool {
    if let Some(row) = widget.downcast_ref::<adw::ExpanderRow>() {
        row.set_expanded(true);
        return true;
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if expand_first_account(&current) {
            return true;
        }
        child = current.next_sibling();
    }
    false
}
fn snapshot(window: &adw::ApplicationWindow, path: &Path) -> Result<()> {
    let paintable = gtk::WidgetPaintable::new(Some(window));
    let snapshot = gtk::Snapshot::new();
    let (width, height) = (window.width(), window.height());
    ensure!(width > 0 && height > 0, "preview has not been allocated");
    paintable.snapshot(&snapshot, width as f64, height as f64);
    let node = snapshot.to_node().context("preview has no render node")?;
    let renderer = window.renderer().context("preview renderer unavailable")?;
    let texture = renderer.render_texture(
        &node,
        Some(&gtk::graphene::Rect::new(
            0.0,
            0.0,
            width as f32,
            height as f32,
        )),
    );
    texture
        .save_to_png(path)
        .context("could not write preview PNG")?;
    Ok(())
}
