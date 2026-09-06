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
    about = "Cirrove — your clouds, one filesystem (read-only preview)"
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Measure real Graph directory updates through an isolated read-only mount.
    ValidateOnedriveFreshness {
        #[arg(long)]
        label: String,
        #[arg(long)]
        state_dir: PathBuf,
    },
    /// Verify Graph notifications using one new isolated synthetic cloud folder.
    ValidateOnedriveNotifications {
        #[arg(long)]
        label: String,
        #[arg(long)]
        state_dir: PathBuf,
        /// Also wait for the real 50-minute renewal and verify a fresh notification.
        #[arg(long)]
        check_renewal: bool,
    },
    /// Developer-only rename, move and file deletion inside a new synthetic folder.
    ValidateOnedriveMutations {
        #[arg(long)]
        label: String,
        #[arg(long)]
        state_dir: PathBuf,
    },
    /// Developer-only cloud writes in a newly created synthetic test folder.
    ValidateOnedriveUploads {
        #[arg(long)]
        label: String,
        /// Separate account state created with connect --write-access.
        #[arg(long)]
        state_dir: PathBuf,
    },
    /// Sign in again to the same account, preserving its selected drive and cache.
    Reauth {
        label: String,
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
    /// Sign in through the browser and select an account/drive (read-only).
    Connect {
        #[arg(long)]
        label: String,
        #[arg(long)]
        client_id: String,
        #[arg(long, default_value = "common")]
        tenant: String,
        #[arg(long)]
        mount_path: PathBuf,
        #[arg(long)]
        drive_id: Option<String>,
        #[arg(long)]
        state_dir: Option<PathBuf>,
        /// Opt in to write consent for isolated developer tests; mounts stay read-only.
        #[arg(long, requires = "state_dir")]
        write_access: bool,
    },
    /// List configured account identities and drive selections; no secrets.
    Accounts {
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
    /// Reversibly enable or disable the desired mount state.
    Enable {
        label: String,
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
    Disable {
        label: String,
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
    /// Verify desktop credential storage using an isolated synthetic entry.
    KeyringCheck,

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
        Command::ValidateOnedriveFreshness { label, state_dir } => {
            cirrove_service::validation::onedrive_freshness(&state_dir, &label).await?;
        }
        Command::ValidateOnedriveNotifications {
            label,
            state_dir,
            check_renewal,
        } => {
            cirrove_service::validation::onedrive_notifications(&state_dir, &label, check_renewal)
                .await?;
        }
        Command::ValidateOnedriveMutations { label, state_dir } => {
            cirrove_service::validation::onedrive_mutations(&state_dir, &label).await?;
        }
        Command::ValidateOnedriveUploads { label, state_dir } => {
            cirrove_service::validation::onedrive_uploads(&state_dir, &label).await?;
        }
        Command::Reauth {
            label,
            state_dir: state,
        } => {
            let state = match state {
                Some(path) => path,
                None => state_dir()?,
            };
            cirrove_service::accounts::reauthenticate(state, label).await?;
        }
        Command::Connect {
            label,
            client_id,
            tenant,
            mount_path,
            drive_id,
            state_dir: state,
            write_access,
        } => {
            let state = match state {
                Some(p) => p,
                None => state_dir()?,
            };
            cirrove_service::accounts::connect(
                state,
                label,
                cirrove_auth::AppRegistration {
                    client_id,
                    authority: tenant,
                },
                mount_path,
                drive_id,
                if write_access {
                    cirrove_auth::AccessMode::ReadWrite
                } else {
                    cirrove_auth::AccessMode::ReadOnly
                },
            )
            .await?;
        }
        Command::Accounts { state_dir: state } => {
            let state = match state {
                Some(p) => p,
                None => state_dir()?,
            };
            for a in cirrove_service::accounts::Settings::load(&state)?.accounts {
                println!(
                    "{} · {} · {}\n  tenant {} · drive {} ({})\n  {} · {}",
                    a.label,
                    a.identity.display_name,
                    a.identity.username,
                    a.identity.tenant_id,
                    a.drive.name,
                    a.drive.drive_type,
                    a.mount_path.display(),
                    if a.enabled { "enabled" } else { "disabled" }
                );
            }
        }
        Command::Enable {
            label,
            state_dir: state,
        } => {
            let state = match state {
                Some(p) => p,
                None => state_dir()?,
            };
            cirrove_service::accounts::set_enabled(&state, &label, true)?;
        }
        Command::Disable {
            label,
            state_dir: state,
        } => {
            let state = match state {
                Some(p) => p,
                None => state_dir()?,
            };
            cirrove_service::accounts::set_enabled(&state, &label, false)?;
        }
        Command::KeyringCheck => cirrove_service::accounts::keyring_check().await?,

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
                modified_unix: 0,
                etag: Some("v1".into()),
                content_version: None,
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
