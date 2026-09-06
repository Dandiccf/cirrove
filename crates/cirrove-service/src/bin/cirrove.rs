use anyhow::{Context, Result, bail};
use cirrove_core::{
    CancellationToken, Change, ChangePage, Checkpoint, Cursor, Node, NodeKind, Scope,
};
use cirrove_onedrive::{OneDrive, StaticToken};
use cirrove_service::{private_dir, refresh, socket_path, state_dir, status};
use cirrove_store::Store;
use clap::{Parser, Subcommand};
use secrecy::SecretString;
use std::{
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::PathBuf,
    sync::Arc,
};

#[derive(Parser)]
#[command(
    version,
    about = "Cirrove — your clouds, one filesystem (development foundation)"
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Query the local daemon; no cloud requests.
    Status {
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Exercise atomic metadata staging with synthetic data, without cloud access.
    Demo {
        #[arg(long)]
        state_dir: PathBuf,
    },
    /// Developer-only metadata indexing; does not download or modify cloud files.
    IndexOnedrive {
        #[arg(long)]
        account: String,
        #[arg(long)]
        drive: String,
        /// Private mode-0600 regular file containing a short-lived Graph access token.
        #[arg(long)]
        token_file: PathBuf,
        #[arg(long)]
        state_dir: Option<PathBuf>,
        /// Stage a new baseline after an expired cursor; preserve old index until complete.
        #[arg(long)]
        reset: bool,
    },
}
#[tokio::main]
async fn main() -> Result<()> {
    match Args::parse().command {
        Command::Status { socket } => {
            let socket = match socket {
                Some(p) => p,
                None => socket_path()?,
            };
            println!("{}", serde_json::to_string_pretty(&status(&socket).await?)?);
        }
        Command::Demo { state_dir } => {
            private_dir(&state_dir)?;
            let path = state_dir.join("demo.db");
            let mut store = Store::open(&path)?;
            let scope = Scope {
                account: "demo".into(),
                provider: "fixture".into(),
                collection: "sample-drive".into(),
            };
            store.begin(&scope, true)?;
            let node = Node {
                id: "sample-file".into(),
                parent_id: None,
                name: "Welcome.txt".into(),
                kind: NodeKind::File,
                size: 42,
                etag: Some("v1".into()),
                target: None,
            };
            store.stage(
                &scope,
                None,
                &ChangePage {
                    changes: vec![Change::Upsert(node)],
                    checkpoint: Checkpoint::Continue(Cursor("fixture-page-2".into())),
                },
            )?;
            drop(store);
            let mut store = Store::open(&path)?;
            let cursor = store.begin(&scope, false)?;
            store.stage(
                &scope,
                cursor.as_ref(),
                &ChangePage {
                    changes: vec![],
                    checkpoint: Checkpoint::Complete(Cursor("fixture-delta-1".into())),
                },
            )?;
            println!(
                "Demo complete: resumed a staged page and atomically published {} synthetic file. No cloud access.",
                store.nodes(&scope)?.len()
            );
        }
        Command::IndexOnedrive {
            account,
            drive,
            token_file,
            state_dir: state,
            reset,
        } => {
            if account.is_empty() || drive.is_empty() {
                bail!("account and drive must not be empty");
            }
            let meta = std::fs::symlink_metadata(&token_file)
                .context("cannot inspect access-token file")?;
            if !meta.is_file()
                || meta.file_type().is_symlink()
                || meta.permissions().mode() & 0o077 != 0
                || meta.uid() != std::fs::metadata("/proc/self")?.uid()
                || meta.len() > 65536
            {
                bail!("token file must be an owned private regular file, at most 64 KiB");
            }
            let token =
                std::fs::read_to_string(&token_file).context("cannot read access-token file")?;
            if token.trim().is_empty() {
                bail!("access-token file is empty");
            }
            let provider = OneDrive::new(
                account.clone(),
                Arc::new(StaticToken(SecretString::from(token.trim().to_owned()))),
            )?;
            let state = match state {
                Some(p) => p,
                None => state_dir()?,
            };
            private_dir(&state)?;
            let scope = Scope {
                account,
                provider: "onedrive".into(),
                collection: drive,
            };
            let cancel = CancellationToken::new();
            let shutdown = cancel.clone();
            tokio::spawn(async move {
                let _ = tokio::signal::ctrl_c().await;
                shutdown.cancel();
            });
            let pages = refresh(
                &provider,
                &scope,
                &state.join("metadata.db"),
                reset,
                &cancel,
            )
            .await?;
            println!(
                "Metadata refresh complete ({pages} pages). No file content downloaded or cloud files modified."
            );
        }
    }
    Ok(())
}
