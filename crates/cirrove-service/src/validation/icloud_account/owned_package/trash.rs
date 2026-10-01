//! Fixed, preregistered sacrificial package experiment. Never selects an existing
//! remote document for mutation. Restart is inspection only, including bootstrap.
use super::*;
use crate::{
    journal::{MutationState, UploadJournal},
    mutations::MutationWorker,
    native_import::ValidatedPackageArchive,
};
use cirrove_core::{
    ReadProvider,
    mutation::{MutationIntent, MutationReceipt, MutationRequest},
    upload::UploadRepresentation,
};
use cirrove_icloud::{
    ICloudFolderCreate, ICloudPackageCreate, OwnedPackageTrashProbe, PackageSemanticIdentity,
    SealedFolderCheckpointVault, SealedUploadCheckpointVault, package_archive_semantic_identity,
};
use serde::{Deserialize, Serialize};
use std::{os::unix::fs::MetadataExt, sync::Mutex};
const RUN: &str = "e6ec6113-dcce-42da-9836-4afce35e0d28";
const SOURCE: &str = "ac9e5456-bd10-4b7d-9215-21bbb85dde69";
const PURPOSE: &str = "owned-native-package-metadata-trash-v1";
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Preregistration {
    version: u8,
    purpose: String,
    run: Uuid,
    source_run: Uuid,
    account: String,
    source_account: String,
    apple_account: String,
    source_drive: String,
    source_document: String,
    source_revision: String,
    source_root: String,
    source_size: u64,
    source_sha256: String,
    semantic: PackageSemanticIdentity,
    parent_name: String,
    source_name: String,
    target_name: String,
}
impl Preregistration {
    fn validate(&self, account: &Account, source: &Account, plan: &OwnedPackagePlan) -> Result<()> {
        ensure!(
            self.version == 1
                && self.purpose == PURPOSE
                && self.run == Uuid::parse_str(RUN)?
                && self.source_run == Uuid::parse_str(SOURCE)?
                && self.run != self.source_run
                && plan.operation == self.source_run
                && plan.scope == scope(source)
                && self.account == account.id
                && self.source_account == source.id
                && account.id != source.id
                && account.credential_id != source.credential_id
                && self.apple_account == account.identity.username
                && account.identity == source.identity
                && account.drive.id == "drive"
                && account.drive.drive_type == source.drive.drive_type
                && self.source_drive == plan.source.drivewsid
                && self.source_document == plan.source.docwsid
                && self.source_revision == plan.source.etag
                && self.source_root == plan.source.display_name()
                && self.source_size == plan.archive_size
                && self.source_sha256 == plan.archive_sha256
                && self.parent_name == format!("Cirrove Package Validation {}", self.run)
                && self.source_name == format!("Cirrove Package Source {}.pages", self.run)
                && self.target_name == format!("Cirrove Package Import {}.pages", self.run)
                && account.label == "iCloudOwnedPackageValidation"
                && !account.enabled
                && account.access == AccessMode::ReadOnly
                && account.root_id == ROOT_ID
                && matches!(account.registration, AppRegistration::ICloud),
            "package Trash preregistration identity mismatch"
        );
        self.semantic.validate()?;
        Ok(())
    }
}
#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
enum Phase {
    Preregistered,
    FolderArmed,
    FolderCreated,
    SourceArmed,
    SourceCreated,
    ImportArmed,
    ImportVerified,
    TrashArmed,
    Complete,
}
impl Phase {
    fn name(self) -> &'static str {
        match self {
            Self::Preregistered => "00-preregistered",
            Self::FolderArmed => "01-folder-armed",
            Self::FolderCreated => "02-folder-created",
            Self::SourceArmed => "03-source-armed",
            Self::SourceCreated => "04-source-created",
            Self::ImportArmed => "05-import-armed",
            Self::ImportVerified => "06-import-verified",
            Self::TrashArmed => "07-trash-armed",
            Self::Complete => "08-complete",
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PhaseReceipt {
    run: Uuid,
    account: String,
    phase: Phase,
}
fn advance(dir: &Path, prereg: &Preregistration, previous: Phase, next: Phase) -> Result<()> {
    let allowed = matches!(
        (previous, next),
        (Phase::Preregistered, Phase::FolderArmed)
            | (Phase::FolderArmed, Phase::FolderCreated)
            | (Phase::FolderCreated, Phase::SourceArmed)
            | (Phase::SourceArmed, Phase::SourceCreated)
            | (Phase::SourceCreated, Phase::ImportArmed)
            | (Phase::ImportArmed, Phase::ImportVerified)
            | (Phase::ImportVerified, Phase::TrashArmed)
            | (Phase::TrashArmed, Phase::Complete)
    );
    ensure!(
        allowed,
        "package Trash phase cannot resume or skip a boundary"
    );
    let before: PhaseReceipt = read_json(&dir.join(format!("{}.json", previous.name())), 4096)?;
    ensure!(
        before.run == prereg.run && before.account == prereg.account && before.phase == previous,
        "package Trash phase binding mismatch"
    );
    record(
        &dir.join(format!("{}.json", next.name())),
        &PhaseReceipt {
            run: prereg.run,
            account: prereg.account.clone(),
            phase: next,
        },
    )?;
    File::open(dir)?.sync_all()?;
    Ok(())
}
fn private_owned_dir(path: &Path) -> Result<()> {
    check_directory(path)?;
    ensure!(
        std::fs::symlink_metadata(path)?.uid() == std::fs::metadata("/proc/self")?.uid(),
        "package state owner mismatch"
    );
    Ok(())
}
// Accept lexical repository `..` components only after walking the original
// path through directory descriptors without following symlinks. Canonicalizing
// first would hide an otherwise-forbidden symlink component from capture.
fn canonical_private_directory(path: &Path) -> Result<PathBuf> {
    private_owned_dir(path)?;
    ensure!(path.is_absolute(), "package directory must be absolute");
    let mut pinned = File::open("/")?;
    for component in path.components() {
        let name = match component {
            std::path::Component::RootDir | std::path::Component::CurDir => continue,
            std::path::Component::Normal(name) => name,
            std::path::Component::ParentDir => std::ffi::OsStr::new(".."),
            _ => anyhow::bail!("unsupported package directory component"),
        };
        pinned = File::from(
            rustix::fs::openat(
                &pinned,
                name,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::DIRECTORY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .context("package directory contains an unsafe component")?,
        );
    }
    let before = pinned.metadata()?;
    let canonical = path.canonicalize()?;
    private_owned_dir(&canonical)?;
    let after = std::fs::symlink_metadata(&canonical)?;
    ensure!(
        (before.dev(), before.ino(), before.uid()) == (after.dev(), after.ino(), after.uid()),
        "package directory changed while resolving its path"
    );
    Ok(canonical)
}
fn capture_local_source(
    source_dir: &Path,
    run_dir: &Path,
    root: &str,
    cancel: &CancellationToken,
) -> Result<ValidatedPackageArchive> {
    let source = canonical_private_directory(source_dir)?;
    let run = canonical_private_directory(run_dir)?;
    Ok(ValidatedPackageArchive::capture(
        &source.join("source.zip"),
        &run.join("capture"),
        root,
        cancel,
    )?)
}
async fn semantic(
    path: PathBuf,
    receipt: PackageDownload,
    root: String,
) -> Result<PackageSemanticIdentity> {
    tokio::task::spawn_blocking(move || {
        let file = private_file(&path, LIMIT)?;
        Ok(package_archive_semantic_identity(
            &file,
            &receipt,
            &root,
            &CancellationToken::new(),
        )?)
    })
    .await?
}
fn check_run(run: Uuid) -> Result<()> {
    ensure!(
        run == Uuid::parse_str(RUN)? && run != Uuid::parse_str(SOURCE)?,
        "package Trash requires the fresh preregistered UUID"
    );
    Ok(())
}
/// Mutating one-shot developer experiment. Fixed source is a retained own archive,
/// not a remote sample. A pre-existing run directory refuses every second call.
pub async fn icloud_owned_package_trash(run: Uuid) -> Result<()> {
    check_run(run)?;
    let (source_dir, source_account, source_plan) = retained(Uuid::parse_str(SOURCE)?)?;
    let source_dir = canonical_private_directory(&source_dir)?;
    let expected = semantic(
        source_dir.join("source.zip"),
        PackageDownload {
            size: source_plan.archive_size,
            sha256: source_plan.archive_sha256.clone(),
        },
        source_plan.source.display_name(),
    )
    .await?;
    let dir = directory(run);
    std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
    let dir = canonical_private_directory(&dir)?;
    private_dir(&dir.join("state"))?;
    private_dir(&dir.join("trash-state"))?;
    private_dir(&dir.join("capture"))?;
    manifest(&dir, run, "import")?;
    let temporary = std::env::temp_dir();
    let sqlite_temporary = std::env::var_os("SQLITE_TMPDIR")
        .map(PathBuf::from)
        .context("set a disk-backed SQLITE_TMPDIR for the probe")?;
    for path in [&temporary, &sqlite_temporary] {
        let output = std::process::Command::new("findmnt")
            .args(["-n", "-o", "FSTYPE", "-T"])
            .arg(path)
            .output()?;
        ensure!(
            output.status.success() && output.stdout == b"btrfs\n",
            "package Trash temporary storage must be btrfs"
        );
    }
    record(
        &dir.join("trash-environment.json"),
        &serde_json::json!({"TMPDIR": temporary, "SQLITE_TMPDIR": sqlite_temporary, "filesystem": "btrfs"}),
    )?;
    let mut account = source_account.clone();
    account.id = Uuid::new_v4().to_string();
    account.credential_id = Uuid::new_v4().to_string();
    account.enabled = false;
    account.access = AccessMode::ReadOnly;
    account.mount_path = dir.join("unused-mount");
    let prereg = Preregistration {
        version: 1,
        purpose: PURPOSE.into(),
        run,
        source_run: Uuid::parse_str(SOURCE)?,
        account: account.id.clone(),
        source_account: source_account.id.clone(),
        apple_account: account.identity.username.clone(),
        source_drive: source_plan.source.drivewsid.clone(),
        source_document: source_plan.source.docwsid.clone(),
        source_revision: source_plan.source.etag.clone(),
        source_root: source_plan.source.display_name(),
        source_size: source_plan.archive_size,
        source_sha256: source_plan.archive_sha256.clone(),
        semantic: expected.clone(),
        parent_name: format!("Cirrove Package Validation {run}"),
        source_name: format!("Cirrove Package Source {run}.pages"),
        target_name: format!("Cirrove Package Import {run}.pages"),
    };
    prereg.validate(&account, &source_account, &source_plan)?;
    record(&dir.join("account.json"), &account)?;
    record(&dir.join("trash-preregistered.json"), &prereg)?;
    record(
        &dir.join(format!("{}.json", Phase::Preregistered.name())),
        &PhaseReceipt {
            run,
            account: account.id.clone(),
            phase: Phase::Preregistered,
        },
    )?;
    File::open(&dir)?.sync_all()?;
    File::open(dir.parent().context("run parent")?)?.sync_all()?;
    // Retained source is opened read-only; current cloud package bytes are never
    // used to infer mutation authority. No existing account settings are changed.
    let snapshot = SealedSessionVault::new(&source_dir.join("state"), &source_account.id)?
        .load(&source_account.credential_id)
        .await?
        .context("owned source session unavailable")?;
    SealedSessionVault::new(&dir.join("state"), &account.id)?
        .save(&account.credential_id, snapshot.clone())
        .await?;
    let cancel = CancellationToken::new();
    // Real local capture is preflighted before FolderArmed or any cloud write.
    // Keep the resulting immutable snapshot through bootstrap; do not recapture
    // the original pathname after creating the folder.
    let capture_source = source_dir.clone();
    let capture_run = dir.clone();
    let expected_root = prereg.source_root.clone();
    let token = cancel.clone();
    let capture = tokio::task::spawn_blocking(move || {
        capture_local_source(&capture_source, &capture_run, &expected_root, &token)
    })
    .await??;
    let read = ICloudDrive::on_demand_from_session_snapshot(
        scope(&account),
        &account.identity.username,
        &snapshot,
    )?;
    let root = read.node(&scope(&account), ROOT_ID, &cancel).await?;
    ensure!(
        root.id == ROOT_ID
            && root.kind == NodeKind::Folder
            && root.parent_id.is_none()
            && !root.package
            && root.target.is_none(),
        "invalid native bootstrap root"
    );
    let journal = Arc::new(Mutex::new(UploadJournal::open(
        &dir.join("bootstrap-journal"),
        &account.id,
        256 * 1024 * 1024,
    )?));
    advance(&dir, &prereg, Phase::Preregistered, Phase::FolderArmed)?;
    let folder_request = MutationRequest {
        scope: scope(&account),
        intent: MutationIntent::CreateFolder {
            parent: ROOT_ID.into(),
            name: prereg.parent_name.clone(),
        },
    };
    let row = journal
        .lock()
        .map_err(|_| anyhow::anyhow!("journal unavailable"))?
        .enqueue_mutation(folder_request)?;
    record(
        &dir.join("folder-operation.json"),
        &serde_json::json!({"run":run,"operation":row.id}),
    )?;
    File::open(&dir)?.sync_all()?;
    let folder_provider = ICloudFolderCreate::from_session_snapshot(
        scope(&account),
        &account.identity.username,
        &snapshot,
        root,
        Arc::new(SealedFolderCheckpointVault::new(
            &dir.join("state"),
            &account.id,
        )?),
    )?;
    let result = MutationWorker::new(journal.clone(), Arc::new(folder_provider), cancel.clone())
        .run_once()
        .await?
        .context("folder operation missing")?;
    ensure!(
        result.state == MutationState::Applied,
        "folder creation uncertain; inspection only"
    );
    let saved = journal
        .lock()
        .map_err(|_| anyhow::anyhow!("journal unavailable"))?
        .mutation(row.id)?;
    let Some(MutationReceipt::Upsert(parent)) = saved.receipt else {
        anyhow::bail!("folder creation receipt unavailable");
    };
    ensure!(
        parent.parent_id.as_deref() == Some(ROOT_ID)
            && parent.name == prereg.parent_name
            && parent.kind == NodeKind::Folder
            && !parent.package
            && parent.target.is_none(),
        "created folder identity mismatch"
    );
    record(&dir.join("owned-folder-node.json"), &parent)?;
    advance(&dir, &prereg, Phase::FolderArmed, Phase::FolderCreated)?;
    advance(&dir, &prereg, Phase::FolderCreated, Phase::SourceArmed)?;
    let upload = journal
        .lock()
        .map_err(|_| anyhow::anyhow!("journal unavailable"))?
        .enqueue_validated_package_archive(
            scope(&account),
            UploadIntent::Create {
                parent: parent.id.clone(),
                name: prereg.source_name.clone(),
            },
            capture,
            &cancel,
        )?;
    ensure!(
        upload.id != run
            && upload.size == prereg.source_size
            && upload.sha256 == prereg.source_sha256
            && upload.representation
                == UploadRepresentation::PackageArchive {
                    expected_root: prereg.source_root.clone(),
                    semantic: expected.clone()
                },
        "captured source provenance changed"
    );
    record(
        &dir.join("source-operation.json"),
        &serde_json::json!({"run":run,"operation":upload.id}),
    )?;
    File::open(&dir)?.sync_all()?;
    let adapter = ICloudPackageCreate::from_session_snapshot(
        scope(&account),
        &account.identity.username,
        &snapshot,
        parent.clone(),
        &dir.join("capture"),
    )?;
    let result = TransferWorker::new(
        journal.clone(),
        Arc::new(adapter),
        Arc::new(SealedUploadCheckpointVault::new(
            &dir.join("state"),
            &account.id,
        )?),
        cancel.clone(),
    )
    .run_once()
    .await?
    .context("source import missing")?;
    ensure!(
        result.id == upload.id && result.state == UploadState::Uploaded,
        "source creation uncertain; inspection only"
    );
    let saved = journal
        .lock()
        .map_err(|_| anyhow::anyhow!("journal unavailable"))?
        .get(upload.id)?;
    ensure!(
        saved.package_completion.as_ref() == Some(&expected),
        "source semantic receipt mismatch"
    );
    let node = saved.remote.context("source receipt absent")?;
    ensure!(
        node.parent_id.as_deref() == Some(parent.id.as_str())
            && node.name == prereg.source_name
            && node.package
            && node.kind == NodeKind::Folder,
        "source destination receipt mismatch"
    );
    let mut remote = session(&dir, &account).await?;
    let entries = remote.list_folder(&parent.id).await?;
    ensure!(
        entries.len() == 1
            && entries[0].drivewsid == node.id
            && entries[0].display_name() == prereg.source_name,
        "source creation inventory mismatch"
    );
    let source = entries.into_iter().next().context("source absent")?;
    let source_receipt =
        download(&mut remote, &parent.id, &source, &dir.join("source.zip")).await?;
    let actual = semantic(
        dir.join("source.zip"),
        PackageDownload {
            size: source_receipt.size,
            sha256: source_receipt.sha256.clone(),
        },
        prereg.source_name.clone(),
    )
    .await?;
    ensure!(
        actual == expected,
        "fresh source differs from retained own archive"
    );
    let plan = OwnedPackagePlan {
        scope: scope(&account),
        operation: run,
        parent: parent.id,
        parent_name: prereg.parent_name.clone(),
        source,
        destination: prereg.target_name.clone(),
        archive_size: source_receipt.size,
        archive_sha256: source_receipt.sha256,
    };
    checked_plan(&account, &plan, run)?;
    record(&dir.join("plan.json"), &plan)?;
    record(
        &dir.join("source-ready.json"),
        &serde_json::json!({"run":run,"archive_size":plan.archive_size,"source_sha256":plan.archive_sha256,"semantic_self_verified":true,"semantic":expected}),
    )?;
    advance(&dir, &prereg, Phase::SourceArmed, Phase::SourceCreated)?;
    advance(&dir, &prereg, Phase::SourceCreated, Phase::ImportArmed)?;
    let importer = OwnedPackageCreate::prepare(
        session(&dir, &account).await?,
        plan.clone(),
        &dir.join("state"),
    )
    .await?;
    let created = importer
        .execute(private_file(&dir.join("source.zip"), LIMIT)?, &cancel)
        .await?;
    ensure!(
        created.registration_confirmed && created.observed.is_some(),
        "sacrificial import uncertain; inspection only"
    );
    icloud_owned_package_verify(run).await?;
    advance(&dir, &prereg, Phase::ImportArmed, Phase::ImportVerified)?;
    advance(&dir, &prereg, Phase::ImportVerified, Phase::TrashArmed)?;
    let probe = OwnedPackageTrashProbe::prepare(
        session(&dir, &account).await?,
        account.identity.username.clone(),
        plan,
        expected,
        &dir.join("state"),
        &dir.join("trash-state"),
        &cancel,
    )
    .await?;
    let report = probe.execute(&cancel).await?;
    ensure!(
        report.metadata_stale_refusal_recorded && report.current_trash_semantic_recovery_verified,
        "native Trash experiment incomplete"
    );
    record(&dir.join("trash-result.json"), &report)?;
    advance(&dir, &prereg, Phase::TrashArmed, Phase::Complete)?;
    println!(
        "Fresh owned package: metadata stale precondition refused and current revision recoverable with verified semantics. Native replacement remains unproven."
    );
    Ok(())
}
/// Never resumes a queue or mutation. Earlier bootstrap uncertainty is retained
/// for review; only an existing core Trash checkpoint permits provider inspection.
pub async fn icloud_owned_package_trash_inspect(run: Uuid) -> Result<()> {
    check_run(run)?;
    let dir = directory(run);
    private_owned_dir(&dir)?;
    let prereg: Preregistration = read_json(&dir.join("trash-preregistered.json"), 16 * 1024)?;
    let account: Account = read_json(&dir.join("account.json"), 64 * 1024)?;
    let (_, source_account, source_plan) = retained(Uuid::parse_str(SOURCE)?)?;
    prereg.validate(&account, &source_account, &source_plan)?;
    let armed: PhaseReceipt = read_json(
        &dir.join(format!("{}.json", Phase::TrashArmed.name())),
        4096,
    )
    .context(
        "bootstrap stopped before Trash; retained receipts require review, no mutation resumed",
    )?;
    ensure!(
        armed.run == run && armed.account == account.id && armed.phase == Phase::TrashArmed,
        "Trash inspection phase mismatch"
    );
    let (_, _, plan) = retained(run)?;
    let report = OwnedPackageTrashProbe::inspect(
        session(&dir, &account).await?,
        &account.identity.username,
        &plan,
        &dir.join("trash-state"),
        &CancellationToken::new(),
    )
    .await?;
    let output = verification_directory(&dir)?;
    record(&output.join("trash-inspection.json"), &report)?;
    println!("Owned package Trash checkpoint inspected read-only; no mutation resumed.");
    Ok(())
}
#[cfg(test)]
mod tests;
