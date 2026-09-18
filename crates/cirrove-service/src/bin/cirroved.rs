use anyhow::Result;
use cirrove_core::CancellationToken;
use cirrove_service::{private_dir, serve_managed, socket_path, state_dir};
use cirrove_store::Store;
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about = "Cirrove user service — read-only preview")]
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
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into())
                // Transport trace logs can contain signed URLs and frames.
                .add_directive("tungstenite=off".parse()?)
                .add_directive("tokio_tungstenite=off".parse()?),
        )
        .init();
    let args = Args::parse();
    let state = match args.state_dir {
        Some(p) => p,
        None => state_dir()?,
    };
    private_dir(&state)?;
    let _owner = cirrove_service::accounts::daemon_lock(&state)?;
    let db = state.join("metadata.db");
    let path = db.clone();
    tokio::task::spawn_blocking(move || Store::open(path)).await??;
    let socket = match args.socket {
        Some(p) => p,
        None => socket_path()?,
    };
    cirrove_service::recover_control_socket(&socket).await?;
    let cancel = CancellationToken::new();
    let shutdown = cancel.clone();
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::spawn(async move {
        tokio::select! {_=tokio::signal::ctrl_c()=>{},_=terminate.recv()=>{}}
        shutdown.cancel();
    });
    // Not "read-only": `Manager::start` supplies a write factory, so an account
    // carrying a write grant gets a writable mount. The log said otherwise long
    // after that stopped being true, and an operator reading it had no way to
    // tell whether their own mount could be written to.
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "starting Cirrove account service"
    );
    let (manager, worker) = cirrove_service::manager::Manager::start(state, cancel.clone());
    let result = serve_managed(db, socket, cancel.clone(), Some(manager)).await;
    cancel.cancel();
    worker.await?;
    result
}
