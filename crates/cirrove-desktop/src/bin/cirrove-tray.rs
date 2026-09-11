//! A Cirrove status icon, separate from the settings window.
//!
//! Separate binary rather than a flag on `cirrove-desktop`, because a tray is
//! the thing a user leaves running from login and a settings window is not.
//! Whether it stays a separate binary once there is packaging is open; see
//! docs/adr/0007-desktop-event-channel.md.

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "Cirrove tray status icon")]
struct Args {
    /// Control socket of the daemon to follow. Defaults to the usual one.
    #[arg(long)]
    socket: Option<PathBuf>,
    /// Account settings directory. Defaults to the usual one; pass it alongside
    /// `--socket` when following an isolated development daemon, so that a
    /// mount action changes that daemon's accounts and not the real ones.
    #[arg(long)]
    state_dir: Option<PathBuf>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();
    let socket = match args.socket {
        Some(socket) => socket,
        None => cirrove_service::socket_path()?,
    };
    let state_dir = match args.state_dir {
        Some(state_dir) => state_dir,
        None => cirrove_service::state_dir()?,
    };
    cirrove_desktop::tray::run(socket, state_dir).await
}
