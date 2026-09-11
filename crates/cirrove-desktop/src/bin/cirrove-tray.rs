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
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();
    let socket = match args.socket {
        Some(socket) => socket,
        None => cirrove_service::socket_path()?,
    };
    cirrove_desktop::tray::run(socket).await
}
