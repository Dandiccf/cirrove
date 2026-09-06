use anyhow::Result;
use cirrove_core::CancellationToken;
use cirrove_service::{private_dir, serve, socket_path, state_dir};
use cirrove_store::Store;
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about = "Cirrove user service — metadata foundation")]
struct Args {
    #[arg(long)]
    state_dir: Option<PathBuf>,
    #[arg(long)]
    socket: Option<PathBuf>,
}
#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = Args::parse();
    let state = match args.state_dir {
        Some(p) => p,
        None => state_dir()?,
    };
    private_dir(&state)?;
    let db = state.join("metadata.db");
    let path = db.clone();
    tokio::task::spawn_blocking(move || Store::open(path)).await??;
    let socket = match args.socket {
        Some(p) => p,
        None => socket_path()?,
    };
    let cancel = CancellationToken::new();
    let shutdown = cancel.clone();
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::spawn(async move {
        tokio::select! {_=tokio::signal::ctrl_c()=>{},_=terminate.recv()=>{}}
        shutdown.cancel();
    });
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "starting Cirrove metadata service; no cloud mounts configured"
    );
    serve(db, socket, cancel).await
}
