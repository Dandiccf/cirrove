use anyhow::{Context, Result, bail};

/// Print what the daemon did, in a sentence rather than as JSON.
///
/// A refusal is an ordinary outcome here, not a crash: the exit status says the
/// request was not carried out, and the message says why in words the caller can
/// act on -- free space, unpin something, raise the budget.
fn report_pin(reply: &cirrove_service::PinReply) -> Result<()> {
    if let Some(refusal) = &reply.refusal {
        bail!("{refusal}");
    }
    let mut line = format!("pinned {}", reply.item);
    if reply.files > 1 {
        line.push_str(&format!(", {} files", reply.files));
    }
    if reply.reserved > 0 {
        line.push_str(&format!(", {} bytes reserved", reply.reserved));
    }
    if !reply.complete {
        line.push_str(
            "; part of this folder is not indexed yet and will be kept as it is discovered",
        );
    }
    println!("{line}");
    Ok(())
}
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
    /// Observe download validators for one file; GET-only, no mount or cloud writes.
    InspectOnedriveRead {
        #[arg(long)]
        label: String,
        #[arg(long)]
        item: String,
        /// The selected account drive is used unless a linked collection is specified.
        #[arg(long)]
        drive: Option<String>,
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
    /// Compare five bounded experimental-session samples with conservative reads (GET-only).
    ValidateOnedriveReadSession {
        #[arg(long)]
        label: String,
        #[arg(long)]
        item: String,
        #[arg(long)]
        drive: Option<String>,
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
    /// Developer-only application saves on an isolated, newly created cloud folder.
    ValidateOnedriveWritable {
        #[arg(long)]
        label: String,
        #[arg(long)]
        state_dir: PathBuf,
    },
    /// Measure real Graph directory updates through an isolated read-only mount.
    ValidateOnedriveFreshness {
        #[arg(long)]
        label: String,
        #[arg(long)]
        state_dir: PathBuf,
    },
    /// Measure directory listing latency on a cold mount of a real collection,
    /// while its index is still being built and while a download competes.
    ValidateOnedriveNavigation {
        #[arg(long)]
        label: String,
        #[arg(long)]
        drive: Option<String>,
        /// How long to sample. The competing download starts after a third of it.
        #[arg(long, default_value = "300")]
        seconds: u64,
        /// A large file to download against the navigation loop.
        #[arg(long)]
        item: Option<String>,
        /// Root item inside --drive. Required for a linked collection, whose root
        /// is not the account's own.
        #[arg(long)]
        root: Option<String>,
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
    /// Read one file through two cold mounts, with the experimental read path off
    /// and on, and compare the bytes and the Graph metadata requests. GET-only.
    ValidateOnedriveReadBytes {
        #[arg(long)]
        label: String,
        #[arg(long)]
        item: String,
        /// The selected account drive is used unless a linked collection is given.
        #[arg(long)]
        drive: Option<String>,
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
    /// Watch a mounted view follow remote creates, moves and deletions.
    ValidateOnedriveRemoteChanges {
        #[arg(long)]
        label: String,
        #[arg(long)]
        state_dir: PathBuf,
    },
    /// Pin a generated file on a real account and read it back through a mount
    /// without touching the provider, with an unpinned control that must.
    ValidateOnedrivePinning {
        #[arg(long)]
        label: String,
        #[arg(long)]
        state_dir: PathBuf,
    },
    /// Observe catch-up after a reconnection and the periodic recovery refresh,
    /// each with the other mechanism disabled so a discovery is attributable.
    ValidateOnedriveCatchup {
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
        /// Request write consent for this account instead of keeping its current
        /// mode. The only way to move an existing account between read-only and
        /// writable without discarding its index.
        #[arg(long, conflicts_with = "read_only")]
        write_access: bool,
        /// Return this account to read-only consent.
        #[arg(long)]
        read_only: bool,
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
    /// Keep an item available offline, reserving cache space for it.
    Pin {
        /// Account label. Omit when only one account is configured.
        #[arg(default_value = "")]
        label: String,
        /// Mount-relative path, for example `Documents/Reports`. The daemon
        /// resolves it, because only it can list a directory the index has not
        /// reached and only it knows which linked collection holds the item.
        #[arg(long)]
        path: Option<String>,
        /// Provider item id, when you already have one.
        #[arg(long)]
        item: Option<String>,
        /// Pin every file beneath a folder as well.
        #[arg(long)]
        recursive: bool,
        /// Bytes to reserve. Defaults to what the daemon can see, which is the
        /// figure that agrees with the walk.
        #[arg(long)]
        bytes: Option<u64>,
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Release a pin and free the cache space it held.
    Unpin {
        #[arg(default_value = "")]
        label: String,
        #[arg(long)]
        path: Option<String>,
        #[arg(long)]
        item: Option<String>,
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Remove an account and move its local data aside.
    ///
    /// Refuses while the account is enabled, and refuses while it still holds
    /// changes that have not reached the cloud.
    Forget {
        label: String,
        /// Remove the account even though it still holds unsent changes.
        #[arg(long)]
        discard_unsent: bool,
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
    let command = Args::parse().command;
    let validate_session = matches!(&command, Command::ValidateOnedriveReadSession { .. });
    match command {
        Command::InspectOnedriveRead {
            label,
            item,
            drive,
            state_dir: state,
        }
        | Command::ValidateOnedriveReadSession {
            label,
            item,
            drive,
            state_dir: state,
        } => {
            use cirrove_core::ReadProvider;
            let state = state.map(Ok).unwrap_or_else(state_dir)?;
            let settings = cirrove_service::accounts::Settings::load(&state)?;
            let account = settings
                .accounts
                .iter()
                .find(|a| a.label == label)
                .context("configured account not found")?;
            let provider = cirrove_service::accounts::provider(account)?;
            let scope = cirrove_core::Scope {
                account: account.id.clone(),
                provider: "onedrive".into(),
                collection: drive.unwrap_or_else(|| account.drive.id.clone()),
            };
            let cancel = CancellationToken::new();
            let node = provider.node(&scope, &item, &cancel).await?;
            if validate_session {
                let report = provider
                    .inspect_read_session(&scope, &node, &cancel)
                    .await?;
                println!("{}", serde_json::to_string_pretty(&report)?);
                if !report.all_samples_match {
                    bail!("read-session samples differ from conservative comparison reads");
                }
            } else {
                let report = provider
                    .inspect_read_validation(&scope, &node, &cancel)
                    .await?;
                println!("{}", serde_json::to_string_pretty(&report)?);
            }
        }
        Command::ValidateOnedriveWritable { label, state_dir } => {
            cirrove_service::validation::onedrive_writable(&state_dir, &label).await?;
        }
        Command::ValidateOnedriveFreshness { label, state_dir } => {
            cirrove_service::validation::onedrive_freshness(&state_dir, &label).await?;
        }
        Command::ValidateOnedriveNavigation {
            label,
            drive,
            seconds,
            item,
            root,
            state_dir: state,
        } => {
            let state = state.map(Ok).unwrap_or_else(state_dir)?;
            cirrove_service::validation::onedrive_navigation(
                &state, &label, drive, seconds, item, root,
            )
            .await?;
        }
        Command::ValidateOnedriveReadBytes {
            label,
            item,
            drive,
            state_dir: state,
        } => {
            let state = state.map(Ok).unwrap_or_else(state_dir)?;
            cirrove_service::validation::onedrive_read_bytes(&state, &label, &item, drive).await?;
        }
        Command::ValidateOnedrivePinning { label, state_dir } => {
            cirrove_service::validation::onedrive_pinning(&state_dir, &label).await?;
        }
        Command::ValidateOnedriveRemoteChanges { label, state_dir } => {
            cirrove_service::validation::onedrive_remote_changes(&state_dir, &label).await?;
        }
        Command::ValidateOnedriveCatchup { label, state_dir } => {
            cirrove_service::validation::onedrive_catchup(&state_dir, &label).await?;
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
            write_access,
            read_only,
        } => {
            let state = match state {
                Some(path) => path,
                None => state_dir()?,
            };
            let access = match (write_access, read_only) {
                (true, _) => Some(cirrove_auth::AccessMode::ReadWrite),
                (_, true) => Some(cirrove_auth::AccessMode::ReadOnly),
                _ => None,
            };
            cirrove_service::accounts::reauthenticate(state, label, access).await?;
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
        Command::Pin {
            label,
            path,
            item,
            recursive,
            bytes,
            socket,
        } => {
            let socket = match socket {
                Some(path) => path,
                None => socket_path()?,
            };
            let request = cirrove_service::PinRequest {
                label,
                item,
                path,
                recursive,
                bytes,
            };
            report_pin(&cirrove_service::pin(&socket, &request).await?)?;
        }
        Command::Unpin {
            label,
            path,
            item,
            socket,
        } => {
            let socket = match socket {
                Some(path) => path,
                None => socket_path()?,
            };
            let request = cirrove_service::PinRequest {
                label,
                item,
                path,
                ..Default::default()
            };
            report_pin(&cirrove_service::unpin(&socket, &request).await?)?;
        }
        Command::Forget {
            label,
            discard_unsent,
            state_dir: state,
        } => {
            let state = state.map(Ok).unwrap_or_else(state_dir)?;
            println!(
                "{}",
                cirrove_service::accounts::forget(&state, &label, discard_unsent)?
            );
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
