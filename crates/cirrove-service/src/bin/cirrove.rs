use anyhow::{Context, Result, bail};

/// Print what the daemon did, in a sentence rather than as JSON.
///
/// A refusal is an ordinary outcome here, not a crash: the exit status says the
/// request was not carried out, and the message says why in words the caller can
/// act on -- free space, unpin something, raise the budget.
fn report_pin(reply: &cirrove_service::PinReply) -> Result<()> {
    report_pin_change("pinned", reply)
}
/// `unpin` used to report through `report_pin` and say "pinned", which is the
/// one word an unpin must not say.
fn report_pin_change(verb: &str, reply: &cirrove_service::PinReply) -> Result<()> {
    if let Some(refusal) = &reply.refusal {
        bail!("{refusal}");
    }
    let mut line = format!("{verb} {}", reply.item);
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
    /// Mount this account again, and keep mounting it at every start.
    Enable {
        label: String,
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
    /// Unmount this account and stop mounting it, without removing anything.
    ///
    /// The account, its credentials, its index and its cache all stay. `enable`
    /// brings it back.
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
    /// Show what is pinned and how much of the cache budget it has claimed.
    Pins {
        #[arg(default_value = "")]
        label: String,
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
    /// Write a diagnostics bundle -- versions, status, the service's journal --
    /// with account names, folders, file names and ids replaced, for sharing.
    Diagnose {
        /// Where to write it. Default: cirrove-diagnostics-<time>.txt in the
        /// current directory.
        #[arg(long)]
        out: Option<PathBuf>,
        /// How far back the journal excerpt reaches, in journalctl's words.
        #[arg(long, default_value = "2h")]
        since: String,
        /// Print to standard output instead of a file.
        #[arg(long)]
        stdout: bool,
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// What changed lately: remote changes from the cloud and local saves.
    Recent {
        #[arg(default_value = "")]
        label: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// The state of mount-relative paths: kind, pin cover, bytes on disk.
    Paths {
        #[arg(long, default_value = "")]
        label: String,
        #[arg(required = true)]
        paths: Vec<String>,
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Abandon the changes the daemon gave up on, so the mount shows what the
    /// cloud actually has.
    ///
    /// `status` reports these as `stuck_changes`: a delete the provider refused
    /// leaves the item hidden locally and present in the account, and nothing
    /// retries it. This drops the local intent -- it never re-sends anything,
    /// because the conflict means the remote moved and a stale retry would
    /// destroy whatever is there now. The item comes back into view and you can
    /// decide again.
    DiscardStuck {
        #[arg(long, default_value = "")]
        label: String,
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
                provider: cirrove_onedrive::PROVIDER_ID.into(),
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
        Command::Pins { label, socket } => {
            let socket = match socket {
                Some(path) => path,
                None => socket_path()?,
            };
            let status = status(&socket).await?;
            let accounts: Vec<_> = status
                .accounts
                .iter()
                .filter(|a| label.is_empty() || a.label == label)
                .collect();
            if accounts.is_empty() {
                bail!("no account matches that label");
            }
            for account in accounts {
                println!("{}", account.label);
                if account.pins.is_empty() {
                    println!("  nothing pinned");
                } else {
                    for pin in &account.pins {
                        // resident against reserved is the difference between a
                        // pin that is keeping content and one that is only an
                        // accounting entry, so it leads.
                        println!(
                            "  {}  {:.0}/{:.0} MiB kept  {} block(s){}",
                            pin.item,
                            pin.resident as f64 / (1024.0 * 1024.0),
                            pin.reserved as f64 / (1024.0 * 1024.0),
                            pin.blocks,
                            if pin.recursive { "  recursive" } else { "" }
                        );
                    }
                }
                println!("  {}", account.pin_budget.explain());
            }
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
            report_pin_change(
                "unpinned",
                &cirrove_service::unpin(&socket, &request).await?,
            )?;
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
        Command::Diagnose {
            out,
            since,
            stdout,
            socket,
        } => {
            let socket = match socket {
                Some(p) => p,
                None => socket_path()?,
            };
            let (status_value, status_error) = match status(&socket).await {
                Ok(status) => (Some(serde_json::to_value(&status)?), None),
                Err(error) => (None, Some(format!("{error:#}"))),
            };
            let kernel = std::process::Command::new("uname")
                .arg("-r")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
                .unwrap_or_else(|| "?".into());
            let journal = std::process::Command::new("journalctl")
                .args([
                    "--user",
                    "-u",
                    "cirroved.service",
                    "--no-pager",
                    "-o",
                    "short-iso",
                    "--since",
                    &format!("-{since}"),
                ])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default();
            let hostname = std::process::Command::new("uname")
                .arg("-n")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
                .unwrap_or_default();
            let text = cirrove_service::diagnostics::bundle(
                env!("CARGO_PKG_VERSION"),
                &kernel,
                &hostname,
                status_value.as_ref(),
                status_error.as_deref(),
                &journal,
                &since,
            );
            if stdout {
                print!("{text}");
            } else {
                let path = out.unwrap_or_else(|| {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    PathBuf::from(format!("cirrove-diagnostics-{now}.txt"))
                });
                use std::os::unix::fs::OpenOptionsExt;
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&path)
                    .with_context(|| format!("cannot write {}", path.display()))?;
                std::io::Write::write_all(&mut file, text.as_bytes())?;
                println!(
                    "wrote {}; read it before sharing it -- names and paths are replaced, but the replacement works on shapes, not meaning",
                    path.display()
                );
            }
        }
        Command::Recent {
            label,
            limit,
            socket,
        } => {
            let socket = match socket {
                Some(p) => p,
                None => socket_path()?,
            };
            let reply =
                cirrove_service::recent(&socket, &cirrove_service::RecentRequest { label, limit })
                    .await?;
            if let Some(refusal) = reply.refusal {
                bail!("{refusal}");
            }
            println!("From the cloud:");
            if reply.remote.is_empty() {
                println!("  nothing since this service started");
            }
            for change in reply.remote {
                println!(
                    "  {}  {}  {}{}",
                    change.at_unix,
                    if change.removed { "removed" } else { "changed" },
                    change.name,
                    if change.kind == "folder" { "/" } else { "" }
                );
            }
            println!("Saved here:");
            if reply.local.is_empty() {
                println!("  nothing in the journal");
            }
            for change in reply.local {
                println!("  {}  {}  {} bytes", change.state, change.name, change.size);
            }
        }
        Command::Paths {
            label,
            paths,
            socket,
        } => {
            let socket = match socket {
                Some(p) => p,
                None => socket_path()?,
            };
            let reply =
                cirrove_service::paths(&socket, &cirrove_service::PathsRequest { label, paths })
                    .await?;
            if let Some(refusal) = reply.refusal {
                bail!("{refusal}");
            }
            for state in reply.states {
                let cover = match state.pinned.as_deref() {
                    Some("direct") => "pinned",
                    Some("inherited") => "pinned via folder",
                    _ => "not pinned",
                };
                match (state.refusal, state.kind.as_str()) {
                    (Some(refusal), _) => println!("{}  --  {refusal}", state.path),
                    // A folder's size is its subtree's, and nothing of a folder
                    // is "on disk"; the number would only invite the comparison.
                    (None, "folder") => println!("{}  folder  {cover}", state.path),
                    (None, _) => println!(
                        "{}  file  {cover}  {}/{} bytes on disk",
                        state.path, state.resident, state.size
                    ),
                }
            }
        }
        Command::DiscardStuck { label, socket } => {
            let socket = match socket {
                Some(p) => p,
                None => socket_path()?,
            };
            let reply = cirrove_service::discard_stuck(&socket, &label).await?;
            if let Some(refusal) = reply.refusal {
                bail!("{refusal}");
            }
            println!(
                "abandoned {} change(s); {} still stuck",
                reply.discarded, reply.remaining
            );
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
                provider: cirrove_onedrive::PROVIDER_ID.into(),
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
                None,
            )
            .await?;
            println!(
                "Metadata refresh complete ({pages} pages). No file content downloaded or cloud files modified."
            );
        }
    }
    Ok(())
}
