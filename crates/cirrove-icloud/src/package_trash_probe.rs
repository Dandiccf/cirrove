//! One-shot owned PACKAGE metadata-revision experiment, never a write adapter.
//! The caller preregisters a fresh sacrificial run before creating its import.
//! No API resumes a mutation after cancellation, an error, or process death.
use crate::sealed_session::SealedPackageTrashCheckpointVault;
use crate::{
    DriveEntry, ICloudReadSession, OwnedPackageCreate, OwnedPackagePlan, OwnedPackageTrashRequest,
    PackageDownload, PackageSemanticIdentity, ROOT_ID,
};
use anyhow::{Context, Result, ensure};
use cirrove_auth::CredentialVault;
use cirrove_core::{CancellationToken, ProviderError, reads::ReadWindowSink};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::io::AsyncWriteExt;
const MAX_CHECKPOINT: usize = 96 * 1024;
const MAX_ARCHIVE: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PackageTrashPhase {
    Prepared,
    RenameArmed,
    Renamed,
    StaleTrashArmed,
    StaleRefused,
    CurrentTrashArmed,
    Recovered,
}
/// Sanitized evidence from the one stale-revision request. No returned revision
/// is adopted. Item evidence was bound to the owned ID, parent and observed E1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum PackageTrashRefusal {
    HttpPrecondition { http_status: u16 },
    ItemEtagConflict { http_status: u16 },
}
impl PackageTrashRefusal {
    fn valid(self) -> bool {
        match self {
            Self::HttpPrecondition { http_status } => http_status == 412,
            Self::ItemEtagConflict { http_status } => (200..300).contains(&http_status),
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint {
    version: u8,
    plan: OwnedPackagePlan,
    account_hash: String,
    imported: DriveEntry,
    semantic: PackageSemanticIdentity,
    phase: PackageTrashPhase,
    renamed_etag: Option<String>,
    #[serde(default)]
    revision_refusal: Option<PackageTrashRefusal>,
}
impl Checkpoint {
    fn key(&self) -> String {
        SealedPackageTrashCheckpointVault::key(&self.plan.scope.account, self.plan.operation)
    }
    fn renamed_name(&self) -> String {
        format!("Cirrove Package Trash {}.pages", self.plan.operation)
    }
    fn validate(
        &self,
        session: &ICloudReadSession,
        plan: &OwnedPackagePlan,
        apple_account: &str,
    ) -> Result<()> {
        ensure!(
            matches!(self.version, 1 | 2)
                && self.plan == *plan
                && session.account_hash.as_ref() == Some(&self.account_hash)
                && self.account_hash == crate::account_hash(apple_account)?,
            "package Trash checkpoint identity mismatch"
        );
        ensure!(
            self.imported.kind == "FILE"
                && self.imported.zone == "com.apple.CloudDocs"
                && !self.imported.docwsid.is_empty()
                && self.imported.drivewsid
                    == format!("FILE::com.apple.CloudDocs::{}", self.imported.docwsid)
                && self.imported.parent_id == plan.parent
                && self.imported.display_name() == plan.destination
                && !self.imported.etag.is_empty()
                && self.imported.etag.len() <= 4096,
            "package Trash checkpoint source mismatch"
        );
        let needs_revision = matches!(
            self.phase,
            PackageTrashPhase::Renamed
                | PackageTrashPhase::StaleTrashArmed
                | PackageTrashPhase::StaleRefused
                | PackageTrashPhase::CurrentTrashArmed
                | PackageTrashPhase::Recovered
        );
        ensure!(
            !needs_revision
                || self
                    .renamed_etag
                    .as_ref()
                    .is_some_and(|e| !e.is_empty() && e != &self.imported.etag && e.len() <= 4096),
            "package Trash checkpoint has no changed revision"
        );
        let proved = matches!(
            self.phase,
            PackageTrashPhase::StaleRefused
                | PackageTrashPhase::CurrentTrashArmed
                | PackageTrashPhase::Recovered
        );
        ensure!(
            if self.version == 1 {
                self.revision_refusal.is_none()
            } else {
                self.revision_refusal.is_none_or(PackageTrashRefusal::valid)
                    && (!proved || self.revision_refusal.is_some())
                    && (self.revision_refusal.is_none()
                        || proved
                        || self.phase == PackageTrashPhase::StaleTrashArmed)
            },
            "package Trash checkpoint refusal evidence mismatch"
        );
        Ok(())
    }
}
#[derive(Debug, Serialize)]
pub enum PackageTrashLocation {
    OriginalActive,
    RenamedActive,
    RecoverableTrash,
}
/// A read-only observation is not permission to replay an armed operation.
#[derive(Debug, Serialize)]
pub struct PackageTrashInspection {
    pub phase: PackageTrashPhase,
    pub revision_refusal: Option<PackageTrashRefusal>,
    pub location: PackageTrashLocation,
    pub metadata_stale_refusal_recorded: bool,
    pub current_trash_semantic_recovery_verified: bool,
}
pub struct OwnedPackageTrashProbe {
    session: ICloudReadSession,
    apple_account: String,
    saved: Checkpoint,
    vault: Arc<dyn CredentialVault>,
    staging: PathBuf,
    _marker: File,
}
fn private_directory(path: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(path).context("package Trash state unavailable")?;
    ensure!(
        path.is_absolute()
            && meta.is_dir()
            && meta.permissions().mode() & 0o077 == 0
            && meta.uid() == std::fs::metadata("/proc/self")?.uid(),
        "package Trash state must be private"
    );
    Ok(())
}
fn separate_directories(import_state: &Path, probe_state: &Path) -> Result<()> {
    private_directory(import_state)?;
    private_directory(probe_state)?;
    let imported = std::fs::metadata(import_state)?;
    let probe = std::fs::metadata(probe_state)?;
    ensure!(
        std::fs::canonicalize(import_state)? != std::fs::canonicalize(probe_state)?
            && (imported.dev(), imported.ino()) != (probe.dev(), probe.ino()),
        "package Trash requires separate state"
    );
    Ok(())
}
fn anonymous_staging(path: &Path) -> Result<File> {
    let file = tempfile::tempfile_in(path).context("package Trash staging unavailable")?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(file)
}
struct Sink(tokio::fs::File);
#[async_trait::async_trait]
impl ReadWindowSink for Sink {
    async fn write_chunk(&mut self, bytes: &[u8]) -> std::result::Result<(), ProviderError> {
        self.0
            .write_all(bytes)
            .await
            .map_err(|_| ProviderError::Protocol("package Trash staging unavailable"))
    }
}
async fn verify_archive(
    file: File,
    receipt: PackageDownload,
    root: String,
    expected: PackageSemanticIdentity,
    cancel: &CancellationToken,
) -> Result<()> {
    let token = cancel.clone();
    let version = expected.version;
    let actual = tokio::task::spawn_blocking(move || {
        crate::package_archive_semantic_identity_versioned(&file, &receipt, &root, version, &token)
    })
    .await
    .context("package Trash verification interrupted")??;
    ensure!(actual == expected, "package Trash semantic content changed");
    ensure!(!cancel.is_cancelled(), "package Trash probe cancelled");
    Ok(())
}
impl OwnedPackageTrashProbe {
    /// `plan` must belong to the fresh preregistered sacrificial run. Its importer
    /// checkpoint, not a supplied item ID or a name search, authorizes the target.
    /// `probe_state` is a separate fresh 0700 directory. The service owns the
    /// preregistration-before-import policy; existing/public imports are excluded.
    pub async fn prepare(
        session: ICloudReadSession,
        apple_account: String,
        plan: OwnedPackagePlan,
        semantic: PackageSemanticIdentity,
        import_state: &Path,
        probe_state: &Path,
        cancel: &CancellationToken,
    ) -> Result<Self> {
        separate_directories(import_state, probe_state)?;
        let mut reader = session.read_only_fork();
        reader.account_hash = session.account_hash.clone();
        let receipt = OwnedPackageCreate::inspect(reader, &plan, import_state).await?;
        ensure!(
            receipt.registration_confirmed,
            "package import lacks confirmed registration"
        );
        let imported = receipt.observed.context("package import is not active")?;
        ensure!(
            receipt.allocated_document_id.as_deref() == Some(imported.docwsid.as_str()),
            "package import allocated identity mismatch"
        );
        let saved = Checkpoint {
            version: 2,
            account_hash: session
                .account_hash
                .clone()
                .context("package session has no account binding")?,
            plan,
            imported,
            semantic,
            phase: PackageTrashPhase::Prepared,
            renamed_etag: None,
            revision_refusal: None,
        };
        saved.validate(&session, &saved.plan, &apple_account)?;
        // A permanent create-new marker prevents re-execution even if a saved
        // checkpoint is subsequently unavailable. Never delete it automatically.
        let marker = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(probe_state.join(format!("package-trash-{}.owner", saved.plan.operation)))
            .context("package Trash operation already started or unavailable")?;
        marker.sync_all()?;
        File::open(probe_state)?.sync_all()?;
        let vault = Arc::new(SealedPackageTrashCheckpointVault::new(
            probe_state,
            &saved.plan.scope.account,
        )?);
        ensure!(
            vault.load(&saved.key()).await?.is_none(),
            "package Trash checkpoint already exists"
        );
        let mut this = Self {
            session,
            apple_account,
            saved,
            vault,
            staging: probe_state.to_owned(),
            _marker: marker,
        };
        this.active(false, cancel).await?;
        this.persist(PackageTrashPhase::Prepared).await?;
        Ok(this)
    }
    async fn persist(&mut self, phase: PackageTrashPhase) -> Result<()> {
        persist(&mut self.saved, self.vault.as_ref(), phase).await
    }
    async fn arm(&mut self, phase: PackageTrashPhase, cancel: &CancellationToken) -> Result<()> {
        arm(&mut self.saved, self.vault.as_ref(), phase, cancel).await
    }
    async fn active(&mut self, renamed: bool, cancel: &CancellationToken) -> Result<DriveEntry> {
        active(
            &mut self.session,
            &self.saved,
            &self.staging,
            renamed,
            cancel,
        )
        .await
    }
    /// Consumes the only executor. Every sent boundary is durable first; no
    /// restore/resume API exists. A failed/stale response leaves inspection only.
    pub async fn execute(mut self, cancel: &CancellationToken) -> Result<PackageTrashInspection> {
        ensure!(
            self.saved.phase == PackageTrashPhase::Prepared,
            "package Trash probe already started"
        );
        self.active(false, cancel).await?;
        self.arm(PackageTrashPhase::RenameArmed, cancel).await?;
        ensure!(
            self.session
                .send_rename(
                    &self.saved.imported.drivewsid,
                    &self.saved.imported.etag,
                    &self.saved.renamed_name()
                )
                .await?,
            "package probe rename refused"
        );
        let renamed = self.active(true, cancel).await?;
        ensure!(
            renamed.etag != self.saved.imported.etag && !renamed.etag.is_empty(),
            "package rename did not expose a new revision"
        );
        self.saved.renamed_etag = Some(renamed.etag);
        self.persist(PackageTrashPhase::Renamed).await?;
        self.arm(PackageTrashPhase::StaleTrashArmed, cancel).await?;
        let refusal = self
            .session
            .send_probe_trash(
                &self.saved.imported.drivewsid,
                &self.saved.imported.etag,
                &self.saved.plan.parent,
                self.saved
                    .renamed_etag
                    .as_deref()
                    .context("package renamed revision absent")?,
            )
            .await?;
        self.saved.revision_refusal = Some(match refusal {
            crate::write_transport::ProbeTrashResult::PreconditionFailed => {
                PackageTrashRefusal::HttpPrecondition { http_status: 412 }
            }
            crate::write_transport::ProbeTrashResult::EtagConflict { http_status } => {
                PackageTrashRefusal::ItemEtagConflict { http_status }
            }
            _ => anyhow::bail!("package stale Trash lacks explicit revision refusal: {refusal:?}"),
        });
        // Persist the response evidence before proof reads; StaleTrashArmed still
        // means no verified active-content proof and grants no replay authority.
        self.persist(PackageTrashPhase::StaleTrashArmed).await?;
        self.active(true, cancel).await?;
        self.persist(PackageTrashPhase::StaleRefused).await?;
        self.arm(PackageTrashPhase::CurrentTrashArmed, cancel)
            .await?;
        ensure!(
            self.session
                .send_trash(
                    &self.saved.imported.drivewsid,
                    self.saved
                        .renamed_etag
                        .as_deref()
                        .context("missing package revision")?
                )
                .await?,
            "package current Trash refused"
        );
        recover(
            &mut self.session,
            &self.saved,
            &self.apple_account,
            &self.staging,
            cancel,
        )
        .await?;
        self.persist(PackageTrashPhase::Recovered).await?;
        Ok(inspection(
            &self.saved,
            PackageTrashLocation::RecoverableTrash,
        ))
    }
    /// Reads exact identities and content only. Never resumes or repeats a mutation.
    pub async fn inspect(
        mut session: ICloudReadSession,
        apple_account: &str,
        plan: &OwnedPackagePlan,
        probe_state: &Path,
        cancel: &CancellationToken,
    ) -> Result<PackageTrashInspection> {
        private_directory(probe_state)?;
        let vault = SealedPackageTrashCheckpointVault::new(probe_state, &plan.scope.account)?;
        let secret = vault
            .load(&SealedPackageTrashCheckpointVault::key(
                &plan.scope.account,
                plan.operation,
            ))
            .await?
            .context("package Trash checkpoint absent")?;
        ensure!(
            secret.expose_secret().len() <= MAX_CHECKPOINT,
            "package Trash checkpoint exceeds bound"
        );
        let saved: Checkpoint = serde_json::from_str(secret.expose_secret())
            .map_err(|_| anyhow::anyhow!("invalid package Trash checkpoint"))?;
        saved.validate(&session, plan, apple_account)?;
        inspect_saved(&mut session, &saved, apple_account, probe_state, cancel).await
    }
}
async fn inspect_saved(
    session: &mut ICloudReadSession,
    saved: &Checkpoint,
    apple_account: &str,
    probe_state: &Path,
    cancel: &CancellationToken,
) -> Result<PackageTrashInspection> {
    let item = session.item_details(&saved.imported.drivewsid).await?;
    if matches!(
        item.get("parentId").and_then(serde_json::Value::as_str),
        Some("TRASH_ROOT" | crate::write_transport::TRASH_ROOT)
    ) {
        recover(session, saved, apple_account, probe_state, cancel).await?;
        return Ok(inspection(saved, PackageTrashLocation::RecoverableTrash));
    }
    let current: DriveEntry = serde_json::from_value(item)
        .map_err(|_| anyhow::anyhow!("invalid package recovery metadata"))?;
    let renamed = current.display_name() == saved.renamed_name();
    active(session, saved, probe_state, renamed, cancel).await?;
    Ok(inspection(
        saved,
        if renamed {
            PackageTrashLocation::RenamedActive
        } else {
            PackageTrashLocation::OriginalActive
        },
    ))
}

async fn persist(
    saved: &mut Checkpoint,
    vault: &dyn CredentialVault,
    phase: PackageTrashPhase,
) -> Result<()> {
    saved.phase = phase;
    let value = serde_json::to_string(saved)?;
    ensure!(
        value.len() <= MAX_CHECKPOINT,
        "package Trash checkpoint exceeds bound"
    );
    vault.save(&saved.key(), SecretString::from(value)).await
}
async fn arm(
    saved: &mut Checkpoint,
    vault: &dyn CredentialVault,
    phase: PackageTrashPhase,
    cancel: &CancellationToken,
) -> Result<()> {
    let allowed = matches!(
        (saved.phase, phase),
        (PackageTrashPhase::Prepared, PackageTrashPhase::RenameArmed)
            | (
                PackageTrashPhase::Renamed,
                PackageTrashPhase::StaleTrashArmed
            )
            | (
                PackageTrashPhase::StaleRefused,
                PackageTrashPhase::CurrentTrashArmed
            )
    );
    ensure!(
        allowed && !cancel.is_cancelled(),
        "package Trash mutation is not eligible"
    );
    persist(saved, vault, phase).await?;
    ensure!(!cancel.is_cancelled(), "package Trash probe cancelled");
    Ok(())
}
fn inspection(saved: &Checkpoint, location: PackageTrashLocation) -> PackageTrashInspection {
    let metadata_stale_refusal_recorded = matches!(
        saved.phase,
        PackageTrashPhase::StaleRefused
            | PackageTrashPhase::CurrentTrashArmed
            | PackageTrashPhase::Recovered
    );
    let current_trash_semantic_recovery_verified =
        matches!(location, PackageTrashLocation::RecoverableTrash)
            && matches!(
                saved.phase,
                PackageTrashPhase::CurrentTrashArmed | PackageTrashPhase::Recovered
            );
    PackageTrashInspection {
        phase: saved.phase,
        revision_refusal: saved.revision_refusal,
        location,
        metadata_stale_refusal_recorded,
        current_trash_semantic_recovery_verified,
    }
}
async fn active(
    session: &mut ICloudReadSession,
    saved: &Checkpoint,
    staging: &Path,
    renamed: bool,
    cancel: &CancellationToken,
) -> Result<DriveEntry> {
    ensure!(!cancel.is_cancelled(), "package Trash probe cancelled");
    let parent = session.active_folder_metadata(&saved.plan.parent).await?;
    ensure!(
        parent.kind == "FOLDER"
            && parent.zone == "com.apple.CloudDocs"
            && parent.drivewsid == saved.plan.parent
            && parent.parent_id == ROOT_ID
            && parent.display_name() == saved.plan.parent_name,
        "package Trash parent changed"
    );
    let entries = session.list_folder(&saved.plan.parent).await?;
    ensure!(
        entries.len() == 2 && entries.iter().filter(|e| *e == &saved.plan.source).count() == 1,
        "package Trash owned fixture changed"
    );
    let matches: Vec<_> = entries
        .iter()
        .filter(|e| e.drivewsid == saved.imported.drivewsid)
        .collect();
    let [entry] = matches.as_slice() else {
        anyhow::bail!("package Trash target is not unique");
    };
    check_active(entry, saved, renamed)?;
    let file = anonymous_staging(staging)?;
    let mut sink = Sink(tokio::fs::File::from_std(file.try_clone()?));
    let receipt = session
        .download_package(&saved.plan.parent, entry, MAX_ARCHIVE, &mut sink, cancel)
        .await?;
    sink.0.flush().await?;
    drop(sink);
    verify_archive(
        file,
        receipt,
        entry.display_name(),
        saved.semantic.clone(),
        cancel,
    )
    .await?;
    let after = session.item_by_id(&entry.drivewsid).await?;
    check_active(&after, saved, renamed)?;
    ensure!(
        after.etag == entry.etag && after.size == entry.size,
        "package Trash active revision changed"
    );
    Ok(after)
}
fn check_active(entry: &DriveEntry, saved: &Checkpoint, renamed: bool) -> Result<()> {
    let name = if renamed {
        saved.renamed_name()
    } else {
        saved.imported.display_name()
    };
    ensure!(
        entry.kind == "FILE"
            && entry.zone == "com.apple.CloudDocs"
            && entry.drivewsid == saved.imported.drivewsid
            && entry.docwsid == saved.imported.docwsid
            && entry.parent_id == saved.plan.parent
            && entry.display_name() == name
            && !entry.etag.is_empty(),
        "package Trash active identity changed"
    );
    if renamed {
        ensure!(
            entry.etag != saved.imported.etag
                && saved.renamed_etag.as_ref().is_none_or(|e| e == &entry.etag),
            "package Trash renamed revision changed"
        );
    } else {
        ensure!(
            entry.etag == saved.imported.etag,
            "package Trash original revision changed"
        );
    }
    Ok(())
}
async fn recover(
    session: &mut ICloudReadSession,
    saved: &Checkpoint,
    apple_account: &str,
    staging: &Path,
    cancel: &CancellationToken,
) -> Result<()> {
    let request = OwnedPackageTrashRequest {
        apple_account: apple_account.into(),
        drive_id: saved.imported.drivewsid.clone(),
        document_id: saved.imported.docwsid.clone(),
        expected_root: saved.renamed_name(),
        semantic: saved.semantic.clone(),
    };
    session
        .verify_owned_package_in_trash(request, anonymous_staging(staging)?, cancel)
        .await?;
    Ok(())
}
mod restore;
pub use restore::{OwnedPackageRestoreProbe, PackageRestoreInspection, PackageRestorePhase};
mod restore_shape;
pub use restore_shape::{OwnedPackageRestoreShape, RestorePathShape};
#[cfg(test)]
mod tests;

#[cfg(test)]
use crate::package_archive_semantic_identity;
