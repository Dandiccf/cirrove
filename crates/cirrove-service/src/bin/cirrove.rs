use anyhow::{Context, Result, bail};

async fn require_abandon_capability(socket: &std::path::Path, verb: &str) -> Result<()> {
    if cirrove_service::capabilities(socket)
        .await?
        .capabilities
        .get(verb)
        != Some(&1)
    {
        bail!("this service does not support the requested native Stage recovery action");
    }
    Ok(())
}
fn show_native_abandonment(
    input: &cirrove_service::native_abandon::NativeAbandonRequest,
    receipt: &cirrove_service::native_abandon::NativeAbandonReceipt,
) -> Result<()> {
    if receipt.operation != input.operation
        || receipt.account_id != input.expected_account_id
        || receipt.original.id == receipt.retained_stage.id
    {
        bail!("abandonment receipt binding changed");
    }
    println!(
        "Local abandonment recorded [{}]. Original {:?} and staged document {:?} were active when checked. No document was deleted or moved; current cloud state may differ.",
        receipt.operation, receipt.original.id, receipt.retained_stage.id
    );
    println!(
        "Saved archive and encrypted checkpoint remain retained. Export the saved archive with export-save using this operation UUID. The retained stage is not a confirmed Trash backup."
    );
    Ok(())
}

async fn follow_native_replacement(
    socket: &std::path::Path,
    label: &str,
    account: &str,
    initial: &cirrove_service::jobs::Job,
    expected_operation: Option<uuid::Uuid>,
    selected: Option<(String, String)>,
) -> Result<()> {
    let following = async {
        let mut operation = expected_operation;
        let mut original = initial.native_replace.as_ref().map(|p| p.original.clone());
        loop {
            let snapshot = cirrove_service::status(socket).await.context("replacement observation lost; use list-native-replacements before submitting again")?;
            let job = snapshot
                .accounts
                .iter()
                .filter(|a| a.label == label && a.account_id == account)
                .flat_map(|a| &a.jobs)
                .find(|j| j.id == initial.id)
                .context("replacement observer unavailable; use retained-operation discovery")?;
            if job.kind != cirrove_service::jobs::JobKind::ReplaceNativePackage
                || job.native_import.is_some()
                || job.native_trash.is_some()
                || job.export.is_some()
                || job.working_export.is_some()
            {
                bail!("unexpected replacement job");
            }
            if let Some(progress) = &job.native_replace {
                if operation.is_some_and(|op| op != progress.operation)
                    || original.as_ref().is_some_and(|n| n != &progress.original)
                    || selected.as_ref().is_some_and(|(id, etag)| {
                        &progress.original.id != id || progress.original.etag.as_ref() != Some(etag)
                    })
                {
                    bail!("replacement operation or selected original changed");
                }
                if operation.is_none() {
                    eprintln!("Queued replacement operation {}", progress.operation);
                }
                operation = Some(progress.operation);
                original = Some(progress.original.clone());
            }
            if job.state == cirrove_service::jobs::JobState::Succeeded {
                let receipt = job
                    .native_replace
                    .as_ref()
                    .context("replacement receipt missing")?;
                let current = receipt
                    .current
                    .as_ref()
                    .context("replacement current identity missing")?;
                let backup = receipt
                    .recovery
                    .as_ref()
                    .context("replacement recovery receipt missing")?;
                if operation != Some(receipt.operation)
                    || current.id == receipt.original.id
                    || backup.id != receipt.original.id
                    || current.parent_id != receipt.original.parent_id
                    || current.name != receipt.original.name
                    || backup.parent_id.as_deref()
                        != Some("FOLDER::com.apple.CloudDocs::TRASH_ROOT")
                    || backup.name != receipt.original.name
                    || backup.size != receipt.original.size
                    || [current, backup].iter().any(|n| {
                        n.kind != cirrove_core::NodeKind::Folder
                            || !n.package
                            || n.target.is_some()
                            || n.content_version.is_some()
                            || n.etag.as_ref().is_none_or(String::is_empty)
                    })
                {
                    bail!("replacement completion identity mismatch");
                }
                println!(
                    "Replacement completed [{}]; new document {:?}. Original Trash receipt recorded; current recovery availability may differ.",
                    receipt.operation, current.id
                );
                return Ok(());
            }
            if !job.running() {
                bail!(
                    "{}",
                    job.issue
                        .as_deref()
                        .unwrap_or("replacement unconfirmed; inspect its retained operation")
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    };
    tokio::select! {
        biased;
        _ = tokio::signal::ctrl_c() => {
            let _ = cirrove_service::stop_job(socket, &cirrove_service::StopJobRequest {label:label.into(),id:initial.id.clone()}).await;
            bail!("stopped watching; any queued replacement remains retained and may complete; use list-native-replacements if its ID was not received");
        }
        result = following => result,
    }
}

async fn follow_native_trash(
    socket: &std::path::Path,
    label: &str,
    account: &str,
    initial: &cirrove_service::jobs::Job,
    expected_operation: Option<uuid::Uuid>,
) -> Result<()> {
    let following = async {
        let mut operation = expected_operation;
        loop {
            let snapshot=cirrove_service::status(socket).await.context("native Trash observation lost; use list-native-trash with this account before submitting another removal")?;
            let current=snapshot.accounts.iter().filter(|a|a.label==label && a.account_id==account).flat_map(|a|&a.jobs).find(|job|job.id==initial.id).context("native Trash observer unavailable; use watch-native-trash with the retained operation")?;
            if current.kind != cirrove_service::jobs::JobKind::TrashNativeDocument {
                bail!("unexpected native Trash job");
            }
            if let Some(progress) = &current.native_trash {
                if progress.account_id != account
                    || operation.is_some_and(|op| op != progress.operation)
                {
                    bail!("native Trash account or operation changed");
                }
                if operation.is_none() {
                    eprintln!("Queued operation {}", progress.operation);
                    operation = Some(progress.operation);
                }
            }
            if current.state == cirrove_service::jobs::JobState::Succeeded {
                let receipt = current
                    .native_trash
                    .as_ref()
                    .context("native Trash receipt missing")?;
                if operation != Some(receipt.operation)
                    || !receipt.removal_receipt_recorded
                    || !receipt.metadata_absence_recorded
                {
                    bail!("native Trash completion is not confirmed");
                }
                println!(
                    "Recorded native Trash completion [{}]; historical receipt and metadata absence, current cloud state may differ",
                    receipt.operation
                );
                return Ok(());
            }
            if !current.running() {
                bail!(
                    "{}",
                    current
                        .issue
                        .as_deref()
                        .unwrap_or("native Trash is unconfirmed; inspect its retained operation")
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    };
    tokio::select! {biased;
        _=tokio::signal::ctrl_c()=>{
            let _=cirrove_service::stop_job(socket,&cirrove_service::StopJobRequest{label:label.into(),id:initial.id.clone()}).await;
            bail!("stopped watching; any queued native Trash operation remains retained and may complete");
        }
        result=following=>result,
    }
}

async fn follow_native_import(
    socket: &std::path::Path,
    label: &str,
    initial: &cirrove_service::jobs::Job,
    expected_name: &str,
    expected_operation: Option<uuid::Uuid>,
    expected_account_id: Option<&str>,
) -> Result<()> {
    let following = async {
        let mut operation = expected_operation;
        loop {
            let state = cirrove_service::status(socket).await.context("import observation lost; inspect Cirrove jobs and retained operations before retrying")?;
            let current = state
                .accounts
                .iter()
                .filter(|a| {
                    a.label == label && expected_account_id.is_none_or(|id| a.account_id == id)
                })
                .flat_map(|a| &a.jobs)
                .find(|job| job.id == initial.id)
                .context(
                    "import result unavailable; inspect retained operations before retrying",
                )?;
            if current.kind != cirrove_service::jobs::JobKind::ImportNativePackage {
                bail!("unexpected import job");
            }
            if let Some(progress) = &current.native_import {
                if operation.is_some_and(|id| id != progress.operation) {
                    bail!("import operation changed");
                }
                if operation.is_none() {
                    eprintln!("Queued operation {}", progress.operation);
                    operation = Some(progress.operation);
                }
            }
            if current.state == cirrove_service::jobs::JobState::Succeeded {
                let receipt = current
                    .native_import
                    .as_ref()
                    .context("import receipt missing")?;
                let remote = receipt
                    .remote
                    .as_ref()
                    .context("verified document receipt missing")?;
                if remote.name != expected_name
                    || !remote.package
                    || remote.kind != cirrove_core::NodeKind::Folder
                {
                    bail!("unexpected imported document receipt");
                }
                println!(
                    "Verified native document import [{}]: {}",
                    receipt.operation, remote.name
                );
                break;
            }
            if !current.running() {
                bail!("{}", current.issue.as_deref().unwrap_or("native import is not confirmed; inspect the retained operation before retrying"));
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        Ok(())
    };
    tokio::select! { biased;
        _=tokio::signal::ctrl_c()=>{
            let _=cirrove_service::stop_job(socket,&cirrove_service::StopJobRequest{label:label.into(),id:initial.id.clone()}).await;
            bail!("stopped watching; the retained import may still finish and was not discarded");
        }
        result=following=>result,
    }
}

/// Recover one selected working generation through the daemon, with an exact receipt.
async fn active_working_export(
    socket: &std::path::Path,
    request: cirrove_service::ExportWorkingRequest,
) -> Result<()> {
    let reply = cirrove_service::export_working(socket, &request).await?;
    if let Some(refusal) = reply.refusal {
        bail!("{refusal}");
    }
    let initial = reply.job.context("working export was not accepted")?;
    eprintln!("Working export started [{}]", initial.id);
    let interrupt = tokio::signal::ctrl_c();
    tokio::pin!(interrupt);
    let mut stopping = false;
    loop {
        let status = tokio::select! {
            result = cirrove_service::status(socket) => result?,
            _ = &mut interrupt, if !stopping => {
                let reply = cirrove_service::stop_job(socket, &cirrove_service::StopJobRequest {
                    label: request.label.clone(), id: initial.id.clone(),
                }).await?;
                stopping = true;
                if reply.already_ended {
                    bail!("export finished before cancellation; inspect the destination because its receipt was dismissed");
                }
                continue;
            }
        };
        let current = status
            .accounts
            .iter()
            .filter(|account| request.label.is_empty() || account.label == request.label)
            .flat_map(|account| &account.jobs)
            .find(|job| job.id == initial.id)
            .context(
                "working export result unavailable; inspect the destination before retrying",
            )?;
        if current.state == cirrove_service::jobs::JobState::Succeeded {
            let receipt = request.confirmed_receipt(&initial, current)
                .context("working export receipt does not match the selected version; inspect the destination")?;
            println!("{}", serde_json::to_string_pretty(receipt)?);
            return Ok(());
        }
        if !current.running() {
            bail!(
                "{}",
                current
                    .issue
                    .as_deref()
                    .unwrap_or("working export did not complete")
            );
        }
        // A short interval also bounds interrupt handling when the previous poll finished.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// Print what the daemon did, in a sentence rather than as JSON.
///
/// A refusal is an ordinary outcome here, not a crash: the exit status says the
/// request was not carried out, and the message says why in words the caller can
/// act on -- free space, unpin something, raise the budget.
fn report_pin(reply: &cirrove_service::PinReply) -> Result<()> {
    report_pin_change("pinned", reply)
}
/// Watch a job to its end, saying where it has got to while it runs.
///
/// `cirrove pin` answers a question whose answer is "it is kept offline now", so
/// the command waits even though the daemon no longer does. The waiting is the
/// caller's to skip -- Ctrl-C leaves the daemon keeping the folder, which is
/// what someone who walked away wanted.
///
/// Progress goes to standard error so a script reading standard output sees the
/// one sentence it always saw.
async fn follow_job(socket: &std::path::Path, label: &str, id: &str) -> Result<Option<String>> {
    let mut last = String::new();
    loop {
        let status = cirrove_service::status(socket).await?;
        let job = status
            .accounts
            .iter()
            .filter(|account| label.is_empty() || account.label == label)
            .flat_map(|account| account.jobs.iter())
            .find(|job| job.id == id)
            .cloned();
        let Some(job) = job else {
            // Gone from the register is the daemon saying it did what it was
            // asked: a job that ended badly stays there carrying why.
            return Ok(None);
        };
        if !job.running() {
            return Ok(Some(match (&job.state, &job.issue) {
                (cirrove_service::jobs::JobState::Stopped, _) => format!(
                    "stopped keeping {} offline after {} of {} files",
                    job.name, job.files_done, job.files_total
                ),
                (_, Some(issue)) => format!(
                    "kept {} of {} files of {} offline, then gave up: {issue}",
                    job.files_done, job.files_total, job.name
                ),
                (_, None) => format!(
                    "kept {} of {} files of {} offline",
                    job.files_done, job.files_total, job.name
                ),
            }));
        }
        let line = format!(
            "  keeping {} offline: {} of {} files, {} of {}",
            job.name,
            job.files_done,
            job.files_total,
            cirrove_service::human_bytes(job.bytes_done),
            cirrove_service::human_bytes(job.bytes_total)
        );
        if line != last {
            eprintln!("{line}");
            last = line;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
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
use cirrove_icloud::{ICloudReadSession, SignInStep};
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

fn reauth_access(write_access: bool, read_only: bool) -> Option<cirrove_auth::AccessMode> {
    match (write_access, read_only) {
        (true, _) => Some(cirrove_auth::AccessMode::ReadWrite),
        (_, true) => Some(cirrove_auth::AccessMode::ReadOnly),
        _ => None,
    }
}

fn connection_access(write_access: bool) -> cirrove_auth::AccessMode {
    if write_access {
        cirrove_auth::AccessMode::ReadWrite
    } else {
        cirrove_auth::AccessMode::ReadOnly
    }
}

fn access_name(access: cirrove_auth::AccessMode) -> &'static str {
    match access {
        cirrove_auth::AccessMode::ReadOnly => "read-only",
        cirrove_auth::AccessMode::ReadWrite => "read-write",
    }
}

async fn icloud_login(apple_id: &str) -> Result<ICloudReadSession> {
    cirrove_auth::DesktopVault::reachable().await?;
    let password =
        SecretString::new(rpassword::prompt_password("Apple account password: ")?.into());
    let mut session = ICloudReadSession::new()?;
    match session.sign_in(apple_id, &password).await? {
        SignInStep::Ready => {}
        SignInStep::NeedsTrustedDeviceCode => {
            session.request_trusted_device_code().await?;
            let code =
                SecretString::new(rpassword::prompt_password("Trusted-device code: ")?.into());
            session.verify_trusted_device_code(&code).await?;
        }
    }
    drop(password);
    Ok(session)
}

#[derive(Parser)]
#[command(
    version,
    about = "Cirrove — your clouds, one filesystem (pre-release preview)"
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Compare bounded Google adapter reads with the mount and classify shortcuts (GET-only).
    ValidateGoogleRead {
        #[arg(long)]
        label: String,
        #[arg(long, default_value = "3")]
        files: usize,
        /// Largest ordinary file to compare through both paths. The bound keeps
        /// this live check deliberate even when a drive contains large files.
        #[arg(long, default_value = "1048576")]
        max_file_bytes: u64,
        #[arg(long, default_value = "32")]
        shortcuts: usize,
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
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
        /// How long to sample the quiet, fully indexed control arm, after the
        /// collection has finished indexing. Zero skips it, which is what every
        /// run before 2026-09-15 did -- and why its figures had nothing to be a
        /// ratio of.
        #[arg(long, default_value = "0")]
        idle_seconds: u64,
        /// Samples per arm. A budget rather than a duration, because the three
        /// arms share one directory tree: the first three-arm run consumed all
        /// 1,548 directories in the collection in its two busy arms and left the
        /// control two samples, which is not a control.
        #[arg(long, default_value = "60")]
        per_arm: usize,
        /// How many readers pull that file at once. One stream moved 1.75 MiB/s
        /// through the VM's user-mode network, which did not reproduce the
        /// interference two host runs measured twice: contention needs a
        /// saturated resource, and one connection through a NAT is not one.
        #[arg(long, default_value = "1")]
        streams: usize,
        /// A file to open and read whole while the index is still being built,
        /// as a mount-relative path. What a person waits for when they open a
        /// document; see docs/benchmarks/waiting-for-a-file-during-the-first-index.json.
        #[arg(long)]
        read_first: Option<String>,
        /// A second file of comparable size, read after the index completes.
        /// A second file rather than the same one, because by then the first is
        /// in the engine's own cache and re-reading it would time the cache.
        #[arg(long)]
        read_later: Option<String>,
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
        /// Leave the fixture folder in the drive after a passing run. A failing
        /// run keeps it either way; this is for inspecting one that worked.
        #[arg(long)]
        keep_fixture: bool,
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
    /// Developer-only Google creates and stale-ETag probe in a new test folder.
    ValidateGoogleCreate {
        #[arg(long)]
        label: String,
        /// Separate account state created with connect-google --write-access.
        #[arg(long)]
        state_dir: PathBuf,
    },
    /// Developer-only Drive v2 stale-ETag probe on a retained Google test file.
    ValidateGoogleV2Etag {
        #[arg(long)]
        label: String,
        /// UUID of a completed validate-google-create run.
        #[arg(long)]
        run: uuid::Uuid,
        /// Separate account state created with connect-google --write-access.
        #[arg(long)]
        state_dir: PathBuf,
    },
    /// Developer-only Drive v2 resumable and durable-worker probe on a retained test file.
    ValidateGoogleV2Content {
        #[arg(long)]
        label: String,
        /// UUID of a completed validate-google-create run.
        #[arg(long)]
        run: uuid::Uuid,
        /// Separate account state created with connect-google --write-access.
        #[arg(long)]
        state_dir: PathBuf,
    },
    /// Sign in again to the same account, preserving its selected drive and cache.
    Reauth {
        label: String,
        #[arg(long)]
        state_dir: Option<PathBuf>,
        /// Explicitly allow changes after sign-in, preserving the drive and cache.
        /// For iCloud this is Cirrove's local policy, not narrower Apple consent.
        #[arg(long, conflicts_with = "read_only")]
        write_access: bool,
        /// Explicitly return this account to read-only after sign-in.
        #[arg(long)]
        read_only: bool,
    },
    /// Sign in through the browser and select an account/drive.
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
        /// Request write consent and mount this connection read-write.
        #[arg(long, requires = "state_dir")]
        write_access: bool,
    },
    /// Connect Google My Drive or a Shared Drive with Cirrove's Desktop OAuth app.
    ConnectGoogle {
        #[arg(long)]
        label: String,
        /// Optional private (chmod 600) JSON for a different Desktop OAuth app.
        #[arg(long)]
        client_json: Option<PathBuf>,
        #[arg(long)]
        mount_path: PathBuf,
        /// Select an offered Shared Drive by ID; defaults to My Drive.
        #[arg(long)]
        drive_id: Option<String>,
        #[arg(long)]
        state_dir: Option<PathBuf>,
        /// Request full Drive consent and mount this connection read-write.
        #[arg(long)]
        write_access: bool,
    },
    /// Experimental iCloud Drive connection; defaults to locally enforced read-only.
    ConnectIcloud {
        #[arg(long)]
        label: String,
        #[arg(long)]
        apple_id: String,
        #[arg(long)]
        mount_path: PathBuf,
        #[arg(long)]
        state_dir: Option<PathBuf>,
        /// Explicitly allow ordinary-file changes after sign-in. Locally enforced
        /// by Cirrove; does not narrow the native Apple session permission.
        #[arg(long)]
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
    /// Show long work that is running -- keeping a folder offline -- and how far
    /// it has got.
    Jobs {
        #[arg(default_value = "")]
        label: String,
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Stop a running job, or clear the record of one that ended. See `jobs`.
    Stop {
        #[arg(default_value = "")]
        label: String,
        /// The job id, as `cirrove jobs` prints it.
        #[arg(long)]
        id: String,
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
    /// What Cirrove keeps on this computer, and what of it you can get back.
    ///
    /// Removing the packages deliberately leaves your connections, index and
    /// cache alone. This says what that costs, and offers the one deletion that
    /// is always safe: the data of connections you have already removed, which
    /// `forget` sets aside rather than deletes and which nothing else ever
    /// looks at again.
    LocalData {
        /// Delete the set-aside data of connections you already removed.
        #[arg(long)]
        discard_removed: bool,
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
        /// Where to write it. Default: `cirrove-diagnostics-<time>.txt` in the
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
    /// Remove files without the recycle bin. This cannot be undone.
    ///
    /// An ordinary delete -- in any file manager, on any desktop -- puts the
    /// file in the drive's recycle bin, and nothing configures that away. This
    /// is the second gesture, and it exists because POSIX has one `unlink` with
    /// no flag in which "and skip the recycle bin" could live (ADR 0008).
    ///
    /// Folders are refused. The provider's delete on a folder is recursive, and
    /// nothing available can tell whether a child arrived a moment ago; the
    /// recycle bin is the only recovery from that, and this is the one
    /// operation that removes it.
    DeletePermanently {
        #[arg(long, default_value = "")]
        label: String,
        /// Mount-relative paths, as `paths` and `pin` take them.
        #[arg(required = true)]
        paths: Vec<String>,
        /// Skip the question. For scripts that have already asked.
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// List retained saves from a disabled account without starting its worker.
    RecoverySaves {
        #[arg(long)]
        label: String,
        #[arg(long)]
        state: Option<PathBuf>,
        #[arg(long, default_value_t = 0)]
        after: u64,
        #[arg(long, default_value_t = 200)]
        limit: u32,
    },
    /// List retained working files offline, or use --active for a running account.
    RecoveryWorking {
        #[arg(long)]
        label: String,
        #[arg(long, conflicts_with = "active")]
        state: Option<PathBuf>,
        /// Use the running daemon; the default remains offline recovery.
        #[arg(long)]
        active: bool,
        #[arg(long, requires = "active")]
        socket: Option<PathBuf>,
        #[arg(long)]
        after: Option<uuid::Uuid>,
        #[arg(long, default_value_t = 200)]
        limit: u32,
    },
    /// Recover working bytes offline or with --active; never seals or uploads.
    ExportWorking {
        #[arg(long)]
        label: String,
        #[arg(long, conflicts_with = "active")]
        state: Option<PathBuf>,
        /// Use the running daemon; the default remains offline recovery.
        #[arg(long)]
        active: bool,
        #[arg(long, requires = "active")]
        socket: Option<PathBuf>,
        #[arg(long)]
        file: uuid::Uuid,
        #[arg(long)]
        generation: u64,
        #[arg(long)]
        destination: PathBuf,
    },
    /// Replace one exact selected Pages/Numbers/Keynote PACKAGE with a validated local archive.
    /// Creates a new identity; original Trash receipt is retained. Not a normal editor save.
    ReplaceNativePackage {
        #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
        label: String,
        #[arg(long)]
        account_id: uuid::Uuid,
        #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
        path: String,
        #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
        item_id: String,
        #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
        etag: String,
        #[arg(long)]
        archive: PathBuf,
        #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
        source_root: String,
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Resolve one proven pre-handoff conflict locally. Retains cloud stage,
    /// encrypted checkpoint and saved archive; never retries or deletes anything.
    AbandonNativeStage {
        #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
        label: String,
        #[arg(long)]
        account_id: uuid::Uuid,
        #[arg(long)]
        operation: uuid::Uuid,
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Read a recorded Stage-abandonment outcome, including after restart.
    /// Historical evidence only; does not inspect current cloud state.
    NativeStageAbandonment {
        #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
        label: String,
        #[arg(long)]
        account_id: uuid::Uuid,
        #[arg(long)]
        operation: uuid::Uuid,
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Observe a saved replacement; never capture, queue or retry it.
    WatchNativeReplacement {
        #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
        label: String,
        #[arg(long)]
        account_id: uuid::Uuid,
        #[arg(long)]
        operation: uuid::Uuid,
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// List one bounded retained-replacement page; follow next even if empty.
    /// Receipts are historical evidence, not current recovery availability.
    ListNativeReplacements {
        #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
        label: String,
        #[arg(long)]
        account_id: uuid::Uuid,
        #[arg(long)]
        after: Option<u64>,
        #[arg(long, default_value_t = 100, value_parser=clap::value_parser!(u32).range(1..=100))]
        limit: u32,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// List durable native Trash operations after lost replies/restarts; evidence is historical.
    /// Reads one bounded page without enqueueing or retrying any removal.
    ListNativeTrash {
        #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
        label: String,
        #[arg(long)]
        account_id: uuid::Uuid,
        #[arg(long)]
        after: Option<u64>,
        #[arg(long, default_value_t = 100, value_parser=clap::value_parser!(u32).range(1..=100))]
        limit: u32,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Move one exact original native Pages/Numbers/Keynote PACKAGE revision to iCloud recovery; never permanent delete.
    TrashNativeDocument {
        #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
        label: String,
        #[arg(long)]
        account_id: uuid::Uuid,
        /// Mount-relative original document path (not a generated package child).
        #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
        path: String,
        #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
        item_id: String,
        #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
        etag: String,
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Observe recorded native Trash completion; historical evidence, not current cloud state.
    /// Never enqueue or replay a removal.
    WatchNativeTrash {
        #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
        label: String,
        #[arg(long)]
        account_id: uuid::Uuid,
        #[arg(long)]
        operation: uuid::Uuid,
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Import a validated native Pages/Numbers/Keynote archive as a new iCloud document; never overwrites.
    ImportNativePackage {
        #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
        label: String,
        /// Bind this import to the selected account UUID shown by status.
        #[arg(long)]
        account_id: Option<uuid::Uuid>,
        #[arg(long)]
        archive: PathBuf,
        /// Exact enclosing directory name inside the source archive.
        #[arg(long)]
        source_root: String,
        /// Visible relative destination directory; empty means the drive root.
        #[arg(long, default_value = "")]
        parent: String,
        #[arg(long)]
        name: String,
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Observe an existing native import; never submit its archive again.
    WatchNativeImport {
        #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
        label: String,
        /// Exact account UUID shown by status, preventing label reassignment.
        #[arg(long)]
        account_id: uuid::Uuid,
        #[arg(long)]
        operation: uuid::Uuid,
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Export an immutable local save without changing its cloud operation.
    ExportSave {
        #[arg(long, default_value = "")]
        label: String,
        #[arg(long)]
        operation: uuid::Uuid,
        #[arg(long)]
        destination: PathBuf,
        #[arg(long, conflicts_with = "offline")]
        socket: Option<PathBuf>,
        /// Recover a disabled, unmounted account without the daemon or credentials.
        #[arg(long)]
        offline: bool,
        #[arg(long, requires = "offline")]
        state: Option<PathBuf>,
    },
    /// Keep both copies of every save the cloud refused.
    ///
    /// A conflict means the cloud decided about the file while the person was
    /// editing it, so one of the two versions has to give way. `discard-stuck`
    /// makes that the person's: the cloud keeps its version and the local edit
    /// is gone. This makes it neither's. The bytes are still in the journal,
    /// sealed and checked against their digest, so they are queued as a new
    /// file beside the remote one -- `Report (conflicted copy 2026-09-16).docx`
    /// -- and the person compares them at their leisure.
    KeepBoth {
        #[arg(long, default_value = "")]
        label: String,
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Try the stuck changes again, where trying again is a sensible thing to do.
    ///
    /// Not all of them, and the difference is the whole of it. A change that
    /// FAILED -- a quota, a permission, a connection that went away -- is one
    /// the cloud never decided about, and trying it again is ordinary. A change
    /// in CONFLICT is one the cloud did decide about: the remote moved, and
    /// re-sending would act on whatever is there now, which is how a rename
    /// nobody made or a deletion of a version nobody saw happens. Those are
    /// counted and left alone; `discard-stuck` is what abandons them.
    RetryStuck {
        #[arg(long, default_value = "")]
        label: String,
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
        Command::ValidateGoogleRead {
            label,
            files,
            max_file_bytes,
            shortcuts,
            state_dir: state,
        } => {
            let state = state.map(Ok).unwrap_or_else(state_dir)?;
            cirrove_service::validation::google_read(
                &state,
                &label,
                files,
                max_file_bytes,
                shortcuts,
            )
            .await?;
        }
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
            let provider = cirrove_service::accounts::onedrive_provider(account)?;
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
            idle_seconds,
            per_arm,
            streams,
            read_first,
            read_later,
            item,
            root,
            state_dir: state,
        } => {
            let state = state.map(Ok).unwrap_or_else(state_dir)?;
            cirrove_service::validation::onedrive_navigation(
                &state,
                &label,
                drive,
                seconds,
                idle_seconds,
                per_arm,
                streams,
                item,
                root,
                read_first,
                read_later,
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
            keep_fixture,
        } => {
            cirrove_service::validation::onedrive_notifications(
                &state_dir,
                &label,
                check_renewal,
                keep_fixture,
            )
            .await?;
        }
        Command::ValidateOnedriveMutations { label, state_dir } => {
            cirrove_service::validation::onedrive_mutations(&state_dir, &label).await?;
        }
        Command::ValidateOnedriveUploads { label, state_dir } => {
            cirrove_service::validation::onedrive_uploads(&state_dir, &label).await?;
        }
        Command::ValidateGoogleCreate { label, state_dir } => {
            cirrove_service::validation::google_create(&state_dir, &label).await?;
        }
        Command::ValidateGoogleV2Etag {
            label,
            run,
            state_dir,
        } => {
            cirrove_service::validation::google_v2_etag(&state_dir, &label, run).await?;
        }
        Command::ValidateGoogleV2Content {
            label,
            run,
            state_dir,
        } => {
            cirrove_service::validation::google_v2_content(&state_dir, &label, run).await?;
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
            let account = cirrove_service::accounts::Settings::load(&state)?
                .accounts
                .into_iter()
                .find(|account| account.label == label)
                .context("unknown account label")?;
            let access = reauth_access(write_access, read_only);
            if matches!(account.registration, cirrove_auth::AppRegistration::ICloud) {
                let pending =
                    cirrove_service::accounts::begin_reauthenticate_icloud(state, label, access)?;
                let mode = pending.access();
                let session = icloud_login(pending.apple_id()).await?;
                pending.finish(session).await?;
                println!(
                    "iCloud is signed in again with {} access (enforced locally by Cirrove).",
                    access_name(mode)
                );
            } else {
                cirrove_service::accounts::reauthenticate(state, label, access).await?;
            }
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
                cirrove_auth::AppRegistration::Microsoft {
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
        Command::ConnectGoogle {
            label,
            client_json,
            mount_path,
            drive_id,
            state_dir: state,
            write_access,
        } => {
            let state = state.map(Ok).unwrap_or_else(state_dir)?;
            let access = if write_access {
                cirrove_auth::AccessMode::ReadWrite
            } else {
                cirrove_auth::AccessMode::ReadOnly
            };
            let pending = if let Some(client_json) = client_json {
                cirrove_service::accounts::begin_connect_google_with_access(
                    state,
                    label,
                    client_json,
                    mount_path,
                    access,
                )
                .await?
            } else {
                cirrove_service::accounts::begin_connect(
                    state,
                    label,
                    cirrove_auth::AppRegistration::Google {
                        client_id: cirrove_auth::google::CIRROVE_DESKTOP_CLIENT_ID.into(),
                    },
                    mount_path,
                    access,
                )
                .await?
            };
            println!(
                "Signed in: {} ({})",
                pending.identity().display_name,
                pending.identity().username
            );
            let id = match drive_id {
                Some(id) => id,
                None => pending
                    .drives()
                    .iter()
                    .find(|drive| drive.drive_type == "my_drive")
                    .context("Google returned no My Drive; choose an offered --drive-id")?
                    .id
                    .clone(),
            };
            let account = pending.finish(&id).await?;
            if write_access {
                println!(
                    "Connected {} writable at {}",
                    account.label,
                    account.mount_path.display()
                );
            } else {
                println!(
                    "Connected {} read-only at {}",
                    account.label,
                    account.mount_path.display()
                );
            }
        }
        Command::ConnectIcloud {
            label,
            apple_id,
            mount_path,
            state_dir: state,
            write_access,
        } => {
            let state = state.map(Ok).unwrap_or_else(state_dir)?;
            if !cirrove_service::accounts::valid_label(&label) {
                bail!("use a label of 1–48 letters, digits, hyphens or underscores");
            }
            cirrove_service::manager::validate_mount_directory(&mount_path)?;
            let canonical_mount = std::fs::canonicalize(&mount_path)?;
            if cirrove_service::accounts::Settings::load(&state)?
                .accounts
                .iter()
                .any(|account| account.label == label || account.mount_path == canonical_mount)
            {
                bail!("this label or mount path is already configured");
            }
            let session = icloud_login(&apple_id).await?;
            let account = cirrove_service::accounts::connect_icloud_with_session_and_access(
                state,
                label,
                mount_path,
                apple_id,
                session,
                connection_access(write_access),
            )
            .await?;
            println!(
                "Connected {} {} at {}. Access is enforced locally by Cirrove; this iCloud path remains experimental.",
                account.label,
                access_name(account.access),
                account.mount_path.display()
            );
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
            let label = request.label.clone();
            let reply = cirrove_service::pin(&socket, &request).await?;
            if let Some(refusal) = &reply.refusal {
                bail!("{refusal}");
            }
            // The reservation is made; the fetching is a job, and this command
            // means "it is kept offline now" -- so it waits for one, and says
            // where the fetch has got to while it does.
            if let Some(job) = &reply.job
                && let Some(ended) = follow_job(&socket, &label, job).await?
            {
                bail!("{ended}");
            }
            report_pin(&reply)?;
        }
        Command::Jobs { label, socket } => {
            let socket = match socket {
                Some(path) => path,
                None => socket_path()?,
            };
            let status = status(&socket).await?;
            let accounts: Vec<_> = status
                .accounts
                .iter()
                .filter(|account| label.is_empty() || account.label == label)
                .collect();
            if accounts.is_empty() {
                bail!("no account matches");
            }
            for account in accounts {
                println!("{}", account.label);
                if account.jobs.is_empty() {
                    println!("  nothing is running");
                }
                for job in &account.jobs {
                    let state = match job.state {
                        cirrove_service::jobs::JobState::Running => if job.kind
                            == cirrove_service::jobs::JobKind::ImportNativePackage
                        {
                            "importing native document"
                        } else if job.kind == cirrove_service::jobs::JobKind::TrashNativeDocument {
                            "observing native document Trash"
                        } else if job.kind == cirrove_service::jobs::JobKind::ExportLocal {
                            "exporting local save"
                        } else {
                            "keeping offline"
                        }
                        .to_owned(),
                        cirrove_service::jobs::JobState::Succeeded => "completed".to_owned(),
                        cirrove_service::jobs::JobState::Stopping => "stopping".to_owned(),
                        cirrove_service::jobs::JobState::Stopped => "stopped".to_owned(),
                        cirrove_service::jobs::JobState::Failed => {
                            format!(
                                "gave up: {}",
                                job.issue.as_deref().unwrap_or("no reason given")
                            )
                        }
                        cirrove_service::jobs::JobState::Unknown => "unknown".to_owned(),
                    };
                    println!(
                        "  {}  {} of {} files, {} of {}  {state}  [{}]",
                        job.name,
                        job.files_done,
                        job.files_total,
                        cirrove_service::human_bytes(job.bytes_done),
                        cirrove_service::human_bytes(job.bytes_total),
                        job.id
                    );
                    if let Some(progress) = &job.native_trash {
                        println!(
                            "    account {}  retained operation {}",
                            progress.account_id, progress.operation
                        );
                    }
                }
            }
        }
        Command::Stop { label, id, socket } => {
            let socket = match socket {
                Some(path) => path,
                None => socket_path()?,
            };
            let reply = cirrove_service::stop_job(
                &socket,
                &cirrove_service::StopJobRequest {
                    label,
                    id: id.clone(),
                },
            )
            .await?;
            if let Some(refusal) = &reply.refusal {
                bail!("{refusal}");
            }
            // Not finding it is not a failure: a person acting on a list they
            // read a moment ago is racing work that finished in between, and the
            // outcome they wanted -- it is not running -- is the one they have.
            println!(
                "{}",
                match (reply.stopped, reply.already_ended) {
                    (true, true) => "that had already ended; cleared it",
                    (true, false) => "asked it to stop",
                    (false, _) => "nothing by that name is running",
                }
            );
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
                            "  {}  {} of {} kept  {} block(s){}",
                            // The path when the index can give one, the id when
                            // it cannot; an id is unreadable but it is at least
                            // the thing `cirrove unpin --item` takes.
                            pin.path.as_deref().unwrap_or(&pin.item),
                            cirrove_service::human_bytes(pin.resident),
                            cirrove_service::human_bytes(pin.reserved),
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
        Command::LocalData {
            discard_removed,
            state_dir: state,
        } => {
            let state = state.map(Ok).unwrap_or_else(state_dir)?;
            if discard_removed {
                let (count, bytes) = cirrove_service::accounts::discard_set_aside(&state)?;
                if count == 0 {
                    println!("Nothing set aside; nothing to delete.");
                } else {
                    println!(
                        "Deleted {count} removed connection(s), freeing {}.",
                        cirrove_service::human_bytes(bytes)
                    );
                }
            }
            let data = cirrove_service::accounts::local_data(&state)?;
            println!("{}", data.state_dir.display());
            for (label, bytes) in &data.live {
                println!("  {label}  {}", cirrove_service::human_bytes(*bytes));
            }
            if !data.set_aside.is_empty() {
                println!("  set aside by an earlier removal, kept in case you want it back:");
                for aside in &data.set_aside {
                    println!(
                        "    {}  {}",
                        aside.name,
                        cirrove_service::human_bytes(aside.bytes)
                    );
                }
            }
            println!(
                "  settings, locks and shared index  {}",
                cirrove_service::human_bytes(data.other_bytes)
            );
            println!(
                "  total  {}",
                cirrove_service::human_bytes(data.total_bytes())
            );
            let reclaimable = data.reclaimable_bytes();
            if reclaimable > 0 {
                println!(
                    "\n{} belongs to connections you already removed. \
                     `cirrove local-data --discard-removed` deletes it; nothing in the cloud is touched.",
                    cirrove_service::human_bytes(reclaimable)
                );
            }
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
                println!(
                    "  {}  {}  {} bytes{}",
                    change.state,
                    change.name,
                    change.size,
                    change
                        .operation
                        .map(|id| format!("  [save {id}]"))
                        .unwrap_or_default()
                );
            }
            // The count in `status` says how many were refused; this says
            // which, which is the difference between knowing and being able to
            // act. `discard-stuck` is the verb that clears them.
            if !reply.stuck.is_empty() {
                println!("Given up on:");
                for change in reply.stuck {
                    println!(
                        "  {}  {}  {}",
                        change.state,
                        change.what,
                        change.path.as_deref().unwrap_or(&change.name)
                    );
                }
            }
            // `status` reports these as `failed_uploads`, a number beside a
            // warning sign. Which file it was about could not be learned from
            // this program at all until now.
            if !reply.failed.is_empty() {
                println!("Saves that never reached the cloud:");
                for change in reply.failed {
                    println!(
                        "  {}  {}  {}",
                        change.state,
                        change.what,
                        change.path.as_deref().unwrap_or(&change.name)
                    );
                    // What the cloud has instead. Choosing between your version
                    // and theirs without being told anything about theirs is a
                    // guess; `keep-both` is the answer that needs no choice.
                    if let Some(instead) = &change.instead {
                        println!("      the cloud has: {instead}");
                    }
                }
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
        Command::RetryStuck { label, socket } => {
            let socket = match socket {
                Some(p) => p,
                None => socket_path()?,
            };
            let reply = cirrove_service::retry_stuck(&socket, &label).await?;
            if let Some(refusal) = reply.refusal {
                bail!("{refusal}");
            }
            println!(
                "{} change(s) will be tried again{}",
                reply.queued,
                match reply.conflicts {
                    0 => String::new(),
                    1 => "; 1 is a conflict the cloud already decided about and is not re-sent \
                          (discard-stuck abandons it)"
                        .to_owned(),
                    n => format!(
                        "; {n} are conflicts the cloud already decided about and are not re-sent \
                         (discard-stuck abandons them)"
                    ),
                }
            );
        }
        Command::DeletePermanently {
            label,
            paths,
            yes,
            socket,
        } => {
            let socket = match socket {
                Some(p) => p,
                None => socket_path()?,
            };
            if !yes {
                // Asked here, in the program the person typed into, because the
                // daemon cannot see a dialogue and will not act without being
                // told this happened.
                println!(
                    "This removes {} file(s) without the recycle bin.",
                    paths.len()
                );
                for path in &paths {
                    println!("  {path}");
                }
                print!("There is no way back. Type yes to continue: ");
                use std::io::Write as _;
                std::io::stdout().flush().ok();
                let mut answer = String::new();
                std::io::stdin().read_line(&mut answer)?;
                if answer.trim() != "yes" {
                    println!("Nothing was removed.");
                    return Ok(());
                }
            }
            let reply = cirrove_service::delete_permanently(&socket, &label, paths).await?;
            if let Some(refusal) = reply.refusal {
                bail!("{refusal}");
            }
            let mut refused = 0;
            for deletion in &reply.deletions {
                match &deletion.refusal {
                    Some(why) => {
                        refused += 1;
                        println!("{}  --  {why}", deletion.path);
                    }
                    None => println!("{}  removed permanently", deletion.path),
                }
            }
            if refused > 0 {
                bail!("{refused} of {} were not removed", reply.deletions.len());
            }
        }
        Command::RecoverySaves {
            label,
            state,
            after,
            limit,
        } => {
            let state = match state {
                Some(state) => state,
                None => state_dir()?,
            };
            let rows = tokio::task::spawn_blocking(move || {
                cirrove_service::accounts::OfflineRecovery::open(&state, &label)?.list(after, limit)
            })
            .await??;
            println!("{}", serde_json::to_string_pretty(&rows)?);
        }
        Command::RecoveryWorking {
            label,
            state,
            active,
            socket,
            after,
            limit,
        } => {
            if active {
                let socket = socket.map(Ok).unwrap_or_else(socket_path)?;
                let reply = cirrove_service::recovery_working(
                    &socket,
                    &cirrove_service::RecoveryWorkingRequest {
                        label,
                        after,
                        limit,
                    },
                )
                .await?;
                if let Some(refusal) = reply.refusal {
                    bail!("{refusal}");
                }
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &serde_json::json!({"files":reply.files,"next":reply.next})
                    )?
                );
                return Ok(());
            }
            let state = match state {
                Some(state) => state,
                None => state_dir()?,
            };
            let (files, next) = tokio::task::spawn_blocking(move || {
                cirrove_service::accounts::OfflineRecovery::open(&state, &label)?
                    .working_list(after, limit)
            })
            .await??;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({"files":files,"next":next}))?
            );
        }
        Command::ExportWorking {
            label,
            state,
            active,
            socket,
            file,
            generation,
            destination,
        } => {
            if active {
                let socket = socket.map(Ok).unwrap_or_else(socket_path)?;
                let destination = if destination.is_absolute() {
                    destination
                } else {
                    std::env::current_dir()?.join(destination)
                };
                return active_working_export(
                    &socket,
                    cirrove_service::ExportWorkingRequest {
                        label,
                        file,
                        generation,
                        destination,
                    },
                )
                .await;
            }
            let state = match state {
                Some(state) => state,
                None => state_dir()?,
            };
            let destination = if destination.is_absolute() {
                destination
            } else {
                std::env::current_dir()?.join(destination)
            };
            let cancel = CancellationToken::new();
            let copy_cancel = cancel.clone();
            let mut task =
                tokio::task::spawn_blocking(move || {
                    cirrove_service::accounts::OfflineRecovery::open(&state, &label)?
                        .export_working(file, generation, &destination, &copy_cancel, |_| {})
                });
            let receipt = tokio::select! {
                result = &mut task => result??,
                _ = tokio::signal::ctrl_c() => { cancel.cancel(); task.await?? }
            };
            println!("{}", serde_json::to_string_pretty(&receipt)?);
        }
        Command::ReplaceNativePackage {
            label,
            account_id,
            path,
            item_id,
            etag,
            archive,
            source_root,
            socket,
        } => {
            let socket = match socket {
                Some(path) => path,
                None => socket_path()?,
            };
            if cirrove_service::capabilities(&socket)
                .await?
                .capabilities
                .get("replace-native-package")
                != Some(&1)
            {
                bail!("this service does not support explicit native replacement");
            }
            let account = account_id.to_string();
            let selected = (item_id.clone(), etag.clone());
            let reply=cirrove_service::replace_native_package(&socket,&cirrove_service::ReplaceNativePackageRequest{label:label.clone(),expected_account_id:account.clone(),path,item_id,etag,archive,expected_root:source_root}).await.context("replacement reply unavailable; use list-native-replacements for this account before submitting again")?;
            if let Some(refusal) = reply.refusal {
                bail!("{refusal}");
            }
            let initial = reply.job.context("replacement job was not started")?;
            follow_native_replacement(&socket, &label, &account, &initial, None, Some(selected))
                .await?;
        }
        Command::AbandonNativeStage {
            label,
            account_id,
            operation,
            socket,
        } => {
            let socket = match socket {
                Some(p) => p,
                None => socket_path()?,
            };
            let input = cirrove_service::native_abandon::NativeAbandonRequest {
                label,
                expected_account_id: account_id.to_string(),
                operation,
            };
            require_abandon_capability(&socket, "abandon-native-stage").await?;
            let reply = cirrove_service::native_abandon::abandon_native_stage(&socket, &input)
                .await
                .context(
                    "abandonment reply lost; inspect native-stage-abandonment before resubmitting",
                )?;
            if let Some(refusal) = reply.refusal {
                bail!("{refusal}");
            }
            let initial = reply.job.context("abandonment job unavailable")?;
            eprintln!(
                "Checking retained operation {operation}; no cloud retry or cleanup will be performed."
            );
            let following = async {
                loop {
                    let status = cirrove_service::status(&socket).await?;
                    let job = status
                        .accounts
                        .iter()
                        .filter(|a| {
                            a.label == input.label && a.account_id == input.expected_account_id
                        })
                        .flat_map(|a| &a.jobs)
                        .find(|j| j.id == initial.id)
                        .context("abandonment job unavailable; inspect native-stage-abandonment")?;
                    let progress = job
                        .native_abandon
                        .as_ref()
                        .context("abandonment progress missing")?;
                    if job.kind != cirrove_service::jobs::JobKind::AbandonNativeStage
                        || progress.operation != operation
                        || progress.account_id != input.expected_account_id
                    {
                        bail!("abandonment job binding changed");
                    }
                    if job.state == cirrove_service::jobs::JobState::Succeeded {
                        let receipt = progress
                            .receipt
                            .as_ref()
                            .context("abandonment receipt missing")?;
                        show_native_abandonment(&input, receipt)?;
                        return Ok::<_, anyhow::Error>(());
                    }
                    if !job.running() {
                        bail!(
                            "{}",
                            job.issue.as_deref().unwrap_or(
                                "abandonment not confirmed; inspect native-stage-abandonment"
                            )
                        );
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
            };
            tokio::select! {
                result=following=>result?,
                _=tokio::signal::ctrl_c()=>{
                    let _=cirrove_service::stop_job(&socket,&cirrove_service::StopJobRequest{label:input.label.clone(),id:initial.id.clone()}).await;
                    bail!("Stop requested. A local commit may already have completed; inspect native-stage-abandonment. No cloud cleanup requested.");
                }
            }
        }
        Command::NativeStageAbandonment {
            label,
            account_id,
            operation,
            socket,
        } => {
            let socket = match socket {
                Some(p) => p,
                None => socket_path()?,
            };
            require_abandon_capability(&socket, "native-stage-abandonment").await?;
            let input = cirrove_service::native_abandon::NativeAbandonRequest {
                label,
                expected_account_id: account_id.to_string(),
                operation,
            };
            let reply =
                cirrove_service::native_abandon::native_stage_abandonment(&socket, &input).await?;
            if let Some(refusal) = reply.refusal {
                bail!("{refusal}");
            }
            if let Some(receipt) = reply.receipt {
                show_native_abandonment(&input, &receipt)?;
            } else {
                println!(
                    "No recorded local abandonment for {operation}. No cloud action performed."
                );
            }
        }
        Command::WatchNativeReplacement {
            label,
            account_id,
            operation,
            socket,
        } => {
            let socket = match socket {
                Some(path) => path,
                None => socket_path()?,
            };
            if cirrove_service::capabilities(&socket)
                .await?
                .capabilities
                .get("watch-native-replacement")
                != Some(&1)
            {
                bail!("this service does not support native replacement observation");
            }
            let account = account_id.to_string();
            let reply = cirrove_service::watch_native_replacement(
                &socket,
                &cirrove_service::WatchNativeReplacementRequest {
                    label: label.clone(),
                    expected_account_id: account.clone(),
                    operation,
                },
            )
            .await
            .context("replacement observation could not attach; no replacement submitted")?;
            if let Some(refusal) = reply.refusal {
                bail!("{refusal}");
            }
            let initial = reply.job.context("replacement watch was not started")?;
            eprintln!("Observing retained replacement {operation}; no replacement submitted.");
            follow_native_replacement(&socket, &label, &account, &initial, Some(operation), None)
                .await?;
        }
        Command::ListNativeReplacements {
            label,
            account_id,
            after,
            limit,
            json,
            socket,
        } => {
            let socket = match socket {
                Some(path) => path,
                None => socket_path()?,
            };
            if cirrove_service::capabilities(&socket)
                .await?
                .capabilities
                .get("list-native-replacements")
                != Some(&1)
            {
                bail!("this service does not support retained replacement discovery");
            }
            let reply = cirrove_service::list_native_replacements(
                &socket,
                &cirrove_service::ListNativeReplacementsRequest {
                    label,
                    expected_account_id: account_id.to_string(),
                    after,
                    limit,
                },
            )
            .await?;
            if let Some(refusal) = &reply.refusal {
                bail!("{refusal}");
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&reply)?);
            } else {
                println!(
                    "Retained replacements for {account_id}; historical receipts, current recovery availability may differ."
                );
                for item in &reply.operations {
                    println!(
                        "{} {:?} original {:?} revision {:?} current {:?} recovery {:?} handoff receipt {}",
                        item.operation,
                        item.state,
                        item.original.item,
                        item.original.etag,
                        item.current.as_ref().map(|n| &n.item),
                        item.recovery.as_ref().map(|n| &n.item),
                        item.handoff_receipt_recorded
                    );
                }
                if let Some(next) = reply.next {
                    println!("Next page: --after {next}");
                }
            }
        }
        Command::ListNativeTrash {
            label,
            account_id,
            after,
            limit,
            json,
            socket,
        } => {
            let socket = match socket {
                Some(path) => path,
                None => socket_path()?,
            };
            if cirrove_service::capabilities(&socket)
                .await?
                .capabilities
                .get("list-native-trash")
                != Some(&1)
            {
                bail!("this service does not support discovering retained native Trash operations");
            }
            let reply = cirrove_service::list_native_trash(
                &socket,
                &cirrove_service::ListNativeTrashRequest {
                    label,
                    expected_account_id: account_id.to_string(),
                    after,
                    limit,
                },
            )
            .await?;
            if let Some(refusal) = &reply.refusal {
                bail!("{refusal}");
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&reply)?);
            } else {
                println!(
                    "Recorded native Trash operations for {account_id}; historical evidence, current cloud state may differ."
                );
                for row in &reply.operations {
                    println!(
                        "{}  {:?}  {:?}  item {:?}  original revision {:?}  removal receipt {}  metadata absence {}",
                        row.operation,
                        row.state,
                        row.name,
                        row.item_id,
                        row.etag,
                        row.removal_receipt_recorded,
                        row.metadata_absence_recorded
                    );
                }
                if let Some(next) = reply.next {
                    println!("Next page: --after {next}");
                }
            }
        }
        Command::TrashNativeDocument {
            label,
            account_id,
            path,
            item_id,
            etag,
            socket,
        } => {
            let socket = match socket {
                Some(path) => path,
                None => socket_path()?,
            };
            if cirrove_service::capabilities(&socket)
                .await?
                .capabilities
                .get("trash-native-document")
                != Some(&1)
            {
                bail!("this service does not support explicit native document Trash");
            }
            let account = account_id.to_string();
            let reply=cirrove_service::trash_native_document(&socket,&cirrove_service::TrashNativeDocumentRequest{label:label.clone(),expected_account_id:account.clone(),path,item_id,etag}).await.context("native Trash reply unavailable; use list-native-trash with this exact account before resubmitting")?;
            if let Some(refusal) = reply.refusal {
                bail!("{refusal}");
            }
            let initial = reply
                .job
                .context("native Trash admission was not started")?;
            eprintln!(
                "Native Trash admission started [{}]. Stopping observation never discards a queued removal.",
                initial.id
            );
            follow_native_trash(&socket, &label, &account, &initial, None).await?;
        }
        Command::WatchNativeTrash {
            label,
            account_id,
            operation,
            socket,
        } => {
            let socket = match socket {
                Some(path) => path,
                None => socket_path()?,
            };
            if cirrove_service::capabilities(&socket)
                .await?
                .capabilities
                .get("watch-native-trash")
                != Some(&1)
            {
                bail!("this service does not support observing retained native Trash operations");
            }
            let account = account_id.to_string();
            let reply = cirrove_service::watch_native_trash(
                &socket,
                &cirrove_service::WatchNativeTrashRequest {
                    label: label.clone(),
                    expected_account_id: account.clone(),
                    operation,
                },
            )
            .await
            .context("native Trash observer could not attach; no removal was submitted")?;
            if let Some(refusal) = reply.refusal {
                bail!("{refusal}");
            }
            let initial = reply
                .job
                .context("native Trash observation was not started")?;
            eprintln!("Checking retained native Trash [{operation}]. No removal was submitted.");
            follow_native_trash(&socket, &label, &account, &initial, Some(operation)).await?;
        }
        Command::ImportNativePackage {
            label,
            account_id,
            archive,
            source_root,
            parent,
            name,
            socket,
        } => {
            let socket = match socket {
                Some(path) => path,
                None => socket_path()?,
            };
            let capabilities = cirrove_service::capabilities(&socket).await?;
            if capabilities.capabilities.get("import-native-package") != Some(&1) {
                bail!("this service does not support native document import");
            }
            if account_id.is_some()
                && capabilities
                    .capabilities
                    .get("import-native-package-account-binding")
                    != Some(&1)
            {
                bail!("this service does not support native import account binding");
            }
            let account_id = account_id.map(|id| id.to_string());
            let archive = if archive.is_absolute() {
                archive
            } else {
                std::env::current_dir()?.join(archive)
            };
            let reply = cirrove_service::import_native_package(
                &socket,
                &cirrove_service::ImportNativePackageRequest {
                    label: label.clone(),
                    expected_account_id: account_id.clone(),
                    archive,
                    expected_root: source_root,
                    parent,
                    name: name.clone(),
                },
            )
            .await
            .context(
                "import response unavailable; inspect Cirrove jobs before submitting another copy",
            )?;
            if let Some(refusal) = reply.refusal {
                bail!("{refusal}");
            }
            let initial = reply.job.context("import was not accepted")?;
            eprintln!(
                "Native import started [{}]. Closing this command does not discard a queued upload.",
                initial.id
            );
            follow_native_import(
                &socket,
                &label,
                &initial,
                &name,
                None,
                account_id.as_deref(),
            )
            .await?;
        }
        Command::WatchNativeImport {
            label,
            account_id,
            operation,
            socket,
        } => {
            let socket = match socket {
                Some(path) => path,
                None => socket_path()?,
            };
            if cirrove_service::capabilities(&socket)
                .await?
                .capabilities
                .get("watch-native-import")
                != Some(&1)
            {
                bail!("this service does not support observing retained native imports");
            }
            let account_id = account_id.to_string();
            let reply = cirrove_service::watch_native_import(
                &socket,
                &cirrove_service::WatchNativeImportRequest {
                    label: label.clone(),
                    expected_account_id: account_id.clone(),
                    operation,
                },
            )
            .await
            .context("could not attach import observer; no archive was submitted")?;
            if let Some(refusal) = reply.refusal {
                bail!("{refusal}");
            }
            let initial = reply
                .job
                .context("native import observation was not accepted")?;
            if initial
                .native_import
                .as_ref()
                .is_none_or(|p| p.operation != operation)
            {
                bail!("native import observer bound a different saved operation");
            }
            eprintln!("Watching retained native import [{operation}]. No archive was submitted.");
            follow_native_import(
                &socket,
                &label,
                &initial,
                &initial.name,
                Some(operation),
                Some(&account_id),
            )
            .await?;
        }
        Command::ExportSave {
            label,
            operation,
            destination,
            socket,
            offline,
            state,
        } => {
            if offline {
                let state = match state {
                    Some(state) => state,
                    None => state_dir()?,
                };
                let destination = if destination.is_absolute() {
                    destination
                } else {
                    std::env::current_dir()?.join(destination)
                };
                let cancel = CancellationToken::new();
                let copy_cancel = cancel.clone();
                let mut task = tokio::task::spawn_blocking(move || {
                    cirrove_service::accounts::OfflineRecovery::open(&state, &label)?.export(
                        operation,
                        &destination,
                        &copy_cancel,
                        |_| {},
                    )
                });
                let receipt = tokio::select! {
                    result = &mut task => result??,
                    _ = tokio::signal::ctrl_c() => {
                        cancel.cancel();
                        task.await??
                    }
                };
                println!(
                    "Saved {} verified bytes to {} (SHA-256 {}). The cloud operation is unchanged.",
                    receipt.size,
                    receipt.destination.display(),
                    receipt.sha256
                );
                return Ok(());
            }
            let socket = match socket {
                Some(p) => p,
                None => socket_path()?,
            };
            let destination = if destination.is_absolute() {
                destination
            } else {
                std::env::current_dir()?.join(destination)
            };
            let reply = cirrove_service::export_save(
                &socket,
                &cirrove_service::ExportSaveRequest {
                    label: label.clone(),
                    operation,
                    destination,
                },
            )
            .await?;
            if let Some(refusal) = reply.refusal {
                bail!("{refusal}");
            }
            let job = reply.job.context("export was not accepted")?;
            println!(
                "Export started [{}]; use cirrove stop with this job ID to cancel",
                job.id
            );
            loop {
                let status = cirrove_service::status(&socket).await?;
                let current = status
                    .accounts
                    .iter()
                    .flat_map(|a| &a.jobs)
                    .find(|j| j.id == job.id)
                    .context(
                        "export result is unavailable; inspect the destination before trying again",
                    )?;
                if current.state == cirrove_service::jobs::JobState::Succeeded {
                    let receipt = current.export.as_ref().context("export receipt missing")?;
                    if receipt.operation != operation {
                        bail!("unexpected export receipt");
                    }
                    println!(
                        "Saved {} verified bytes to {} (SHA-256 {}). The cloud operation is unchanged.",
                        receipt.size,
                        receipt.destination.display(),
                        receipt.sha256
                    );
                    break;
                }
                if !current.running() {
                    bail!(
                        "{}",
                        current
                            .issue
                            .as_deref()
                            .unwrap_or("export did not complete")
                    );
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
        Command::KeepBoth { label, socket } => {
            let socket = match socket {
                Some(p) => p,
                None => socket_path()?,
            };
            let reply = cirrove_service::keep_both(&socket, &label).await?;
            if let Some(refusal) = reply.refusal {
                bail!("{refusal}");
            }
            let missed = reply.considered.saturating_sub(reply.kept);
            println!(
                "{} refused save(s) now have a copy queued beside the cloud's version{}",
                reply.kept,
                match missed {
                    0 => String::new(),
                    // Said rather than swallowed: the person is being told their
                    // work is safe, so the exceptions have to be named.
                    n => format!(
                        "; {n} could not be copied, because the file each was replacing is no                          longer in the index"
                    ),
                }
            );
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
                package: false,
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

#[cfg(test)]
mod icloud_access_tests {
    use super::*;
    #[test]
    fn native_trash_list_cli_has_exact_account_bounded_page_and_no_mutation_inputs() {
        let base = [
            "cirrove",
            "list-native-trash",
            "--label",
            "Cloud",
            "--account-id",
            "11111111-1111-4111-8111-111111111111",
        ];
        assert!(matches!(
            Args::try_parse_from(base).expect("read page").command,
            Command::ListNativeTrash {
                limit: 100,
                after: None,
                ..
            }
        ));
        for limit in ["0", "101", "4294967295"] {
            let mut args = base.to_vec();
            args.extend(["--limit", limit]);
            assert!(Args::try_parse_from(args).is_err());
        }
        for extra in ["--path", "--etag", "--item-id", "--retry", "--permanent"] {
            let mut args = base.to_vec();
            args.extend([extra, "forbidden"]);
            assert!(Args::try_parse_from(args).is_err());
        }
        let mut args = base.to_vec();
        args.extend(["--limit", "1", "--after", "42", "--json"]);
        assert!(matches!(
            Args::try_parse_from(args).expect("bounded page").command,
            Command::ListNativeTrash {
                limit: 1,
                after: Some(42),
                json: true,
                ..
            }
        ));
        assert!(Args::try_parse_from(&base[..4]).is_err());
    }
    #[test]
    fn native_trash_cli_requires_exact_binding_and_watch_cannot_accept_mutation_fields() {
        let args = [
            "cirrove",
            "trash-native-document",
            "--label",
            "Cloud",
            "--account-id",
            "11111111-1111-4111-8111-111111111111",
            "--path",
            "Folder/Own \"quoted\"; $.pages",
            "--item-id",
            "FILE::com.apple.CloudDocs::own",
            "--etag",
            "original-v1",
        ];
        assert!(
            matches!(Args::try_parse_from(args).expect("bound removal").command,Command::TrashNativeDocument{path,..} if path==args[7])
        );
        for missing in [4, 6, 8, 10] {
            let mut incomplete = args.to_vec();
            incomplete.drain(missing..missing + 2);
            assert!(Args::try_parse_from(incomplete).is_err());
        }
        let watch = [
            "cirrove",
            "watch-native-trash",
            "--label",
            "Cloud",
            "--account-id",
            "11111111-1111-4111-8111-111111111111",
            "--operation",
            "22222222-2222-4222-8222-222222222222",
        ];
        assert!(Args::try_parse_from(watch).is_ok());
        for extra in [
            "--path",
            "--item-id",
            "--etag",
            "--archive",
            "--retry",
            "--permanent",
        ] {
            let mut invalid = watch.to_vec();
            invalid.extend([extra, "forbidden"]);
            assert!(Args::try_parse_from(invalid).is_err());
        }
    }
    #[test]
    fn native_import_watch_requires_exact_account_and_operation_without_source_arguments() {
        let args = [
            "cirrove",
            "watch-native-import",
            "--label",
            "Cloud",
            "--account-id",
            "11111111-1111-4111-8111-111111111111",
            "--operation",
            "22222222-2222-4222-8222-222222222222",
        ];
        assert!(Args::try_parse_from(args).is_ok());
        assert!(Args::try_parse_from(&args[..6]).is_err());
        let mut wrong = args.to_vec();
        wrong.extend(["--archive", "/must/not/read.pages"]);
        assert!(Args::try_parse_from(wrong).is_err());
        assert!(
            Args::try_parse_from([
                "cirrove",
                "watch-native-import",
                "--label",
                "Cloud",
                "--operation",
                "22222222-2222-4222-8222-222222222222"
            ])
            .is_err()
        );
    }
    #[test]
    fn native_import_requires_explicit_source_root_and_destination_name() {
        let valid = [
            "cirrove",
            "import-native-package",
            "--label",
            "Cloud",
            "--archive",
            "/local/source.pages",
            "--source-root",
            "Source.pages",
            "--name",
            "Copy.pages",
        ];
        let mut empty_label = valid;
        empty_label[3] = "";
        assert!(Args::try_parse_from(empty_label).is_err());
        assert!(matches!(
            Args::try_parse_from(valid)
                .expect("explicit import")
                .command,
            Command::ImportNativePackage { .. }
        ));
        assert!(
            Args::try_parse_from([
                "cirrove",
                "import-native-package",
                "--label",
                "Cloud",
                "--archive",
                "/local/source.pages",
                "--name",
                "Copy.pages"
            ])
            .is_err()
        );
    }

    use cirrove_auth::AccessMode;

    #[test]
    fn icloud_connection_defaults_read_only_and_write_choice_is_explicit() {
        for requested in [false, true] {
            let mut args = vec![
                "cirrove",
                "connect-icloud",
                "--label",
                "Cloud",
                "--apple-id",
                "synthetic@example.invalid",
                "--mount-path",
                "/nonexistent/mount",
            ];
            if requested {
                args.push("--write-access");
            }
            let parsed = Args::try_parse_from(args).expect("CLI");
            let Command::ConnectIcloud { write_access, .. } = parsed.command else {
                panic!("wrong command")
            };
            assert_eq!(
                connection_access(write_access),
                if requested {
                    AccessMode::ReadWrite
                } else {
                    AccessMode::ReadOnly
                }
            );
        }
    }

    #[test]
    fn reauth_preserves_mode_unless_one_explicit_access_flag_is_given() {
        for (flag, expected) in [
            (None, None),
            (Some("--write-access"), Some(AccessMode::ReadWrite)),
            (Some("--read-only"), Some(AccessMode::ReadOnly)),
        ] {
            let mut args = vec!["cirrove", "reauth", "Cloud"];
            if let Some(flag) = flag {
                args.push(flag);
            }
            let parsed = Args::try_parse_from(args).expect("CLI");
            let Command::Reauth {
                write_access,
                read_only,
                ..
            } = parsed.command
            else {
                panic!("wrong command")
            };
            assert_eq!(reauth_access(write_access, read_only), expected);
        }
        assert!(
            Args::try_parse_from([
                "cirrove",
                "reauth",
                "Cloud",
                "--write-access",
                "--read-only"
            ])
            .is_err()
        );
    }
    #[test]
    fn native_stage_abandonment_cli_requires_exact_identity_and_accepts_no_mutation_payload() {
        for verb in ["abandon-native-stage", "native-stage-abandonment"] {
            let base = [
                "cirrove",
                verb,
                "--label",
                "Owned",
                "--account-id",
                "00000000-0000-4000-8000-000000000001",
                "--operation",
                "00000000-0000-4000-8000-000000000002",
            ];
            assert!(Args::try_parse_from(base).is_ok());
            assert!(Args::try_parse_from(&base[..6]).is_err());
            let mut forged = base.to_vec();
            forged.extend(["--archive", "/var/tmp/other.pages"]);
            assert!(Args::try_parse_from(forged).is_err());
        }
    }
    #[test]
    fn native_replacement_cli_requires_bound_original_and_keeps_observers_read_only() {
        let base = [
            "cirrove",
            "replace-native-package",
            "--label",
            "Owned",
            "--account-id",
            "00000000-0000-4000-8000-000000000001",
            "--path",
            "Folder/Owned.pages",
            "--item-id",
            "FILE::com.apple.CloudDocs::owned",
            "--etag",
            "v1",
            "--archive",
            "/var/tmp/source ; $(literal).zip",
            "--source-root",
            "Source.pages",
        ];
        assert!(matches!(
            Args::try_parse_from(base).expect("valid replacement arguments").command,
            Command::ReplaceNativePackage { archive, .. }
                if archive == std::path::Path::new(base[13])
        ));
        for option in [
            "--account-id",
            "--path",
            "--item-id",
            "--etag",
            "--archive",
            "--source-root",
        ] {
            let index = base
                .iter()
                .position(|s| *s == option)
                .expect("required option in fixture");
            let mut args = base.to_vec();
            args.drain(index..index + 2);
            assert!(Args::try_parse_from(args).is_err(), "{option}");
        }
        for verb in ["watch-native-replacement", "list-native-replacements"] {
            let mut args = vec!["cirrove", verb, "--label", "Owned", "--account-id", base[5]];
            if verb.starts_with("watch") {
                args.extend(["--operation", base[5]]);
            }
            assert!(Args::try_parse_from(args.clone()).is_ok());
            args.extend(["--archive", "/var/tmp/not-submitted.zip"]);
            assert!(Args::try_parse_from(args).is_err());
        }
        for limit in ["0", "101"] {
            assert!(
                Args::try_parse_from([
                    "cirrove",
                    "list-native-replacements",
                    "--label",
                    "Owned",
                    "--account-id",
                    base[5],
                    "--limit",
                    limit
                ])
                .is_err()
            );
        }
    }
}
