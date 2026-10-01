//! One-shot developer-only PACKAGE import. No production routing or replacement.
//! A retained marker forbids replay even when a process loses its checkpoint.
use crate::{DriveEntry, ICloudReadSession, ROOT_ID, SealedUploadCheckpointVault};
use anyhow::{Context, Result, ensure};
use cirrove_auth::CredentialVault;
use cirrove_core::{CancellationToken, Scope};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};
use uuid::Uuid;
mod transport;
pub use transport::PackageAllocationRefusal;
use transport::Slot;
const MAX_BODY: u64 = 64 * 1024 * 1024;
const MAX_CHECKPOINT: usize = 96 * 1024;

/// Caller must create this synthetic source and fresh, exclusively owned folder.
/// No arbitrary source selection or user-document sampling is performed here.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OwnedPackagePlan {
    pub scope: Scope,
    pub operation: Uuid,
    pub parent: String,
    pub parent_name: String,
    pub source: DriveEntry,
    pub destination: String,
    pub archive_size: u64,
    pub archive_sha256: String,
}
impl OwnedPackagePlan {
    fn validate(&self) -> Result<()> {
        Uuid::parse_str(&self.scope.account).context("invalid package account")?;
        ensure!(
            self.scope.provider == "icloud" && self.scope.collection == "drive",
            "invalid package scope"
        );
        ensure!(
            self.parent.starts_with("FOLDER::com.apple.CloudDocs::")
                && self.parent != ROOT_ID
                && self.parent.len() <= 512
                && !self.parent.contains(['/', '\\', '\0']),
            "invalid package parent"
        );
        ensure!(
            self.parent_name == format!("Cirrove Package Validation {}", self.operation),
            "package parent is not the owned fixture"
        );
        ensure!(
            self.destination == format!("Cirrove Package Import {}.pages", self.operation),
            "invalid package destination"
        );
        ensure!(
            self.source.kind == "FILE"
                && self.source.zone == "com.apple.CloudDocs"
                && self.source.parent_id == self.parent
                && self.source.extension == "pages"
                && self.source.name == format!("Cirrove Package Source {}", self.operation),
            "invalid package source"
        );
        ensure!(
            !self.source.docwsid.is_empty()
                && self.source.docwsid.len() <= 256
                && self.source.drivewsid
                    == format!("FILE::com.apple.CloudDocs::{}", self.source.docwsid)
                && !self.source.etag.is_empty()
                && self.source.etag.len() <= 1024
                && self.source.items.is_empty(),
            "invalid package source identity"
        );
        ensure!(
            self.archive_size > 0
                && self.archive_size <= MAX_BODY
                && self.archive_sha256.len() == 64
                && self
                    .archive_sha256
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            "invalid package archive receipt"
        );
        Ok(())
    }
}
#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
enum Phase {
    Prepared,
    AllocationStarted,
    Allocated,
    BodyStarted,
    BodyComplete,
    RegistrationStarted,
    Registered,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint {
    version: u8,
    plan: OwnedPackagePlan,
    account_hash: String,
    phase: Phase,
    slot: Option<Slot>,
    // Secret registration fragment; never Debug or public JSON output.
    registration: Option<String>,
}
/// This is a registration observation, not a native-content verification receipt.
pub struct PackageCreateInspection {
    pub allocated_document_id: Option<String>,
    pub observed: Option<DriveEntry>,
    pub registration_confirmed: bool,
}
pub struct OwnedPackageCreate {
    session: ICloudReadSession,
    saved: Checkpoint,
    vault: Arc<dyn CredentialVault>,
    staging: PathBuf,
    // Retained permanent create-new marker prevents concurrent executors/replay.
    _marker: File,
}
impl OwnedPackageCreate {
    /// `state` must be a fresh caller-owned 0700 directory on persistent storage.
    /// This rejects any previous operation marker, even if no checkpoint remains.
    pub async fn prepare(
        session: ICloudReadSession,
        plan: OwnedPackagePlan,
        state: &Path,
    ) -> Result<Self> {
        plan.validate()?;
        ensure!(state.is_absolute(), "invalid package state");
        let meta = std::fs::symlink_metadata(state).context("package state unavailable")?;
        ensure!(
            meta.is_dir() && meta.permissions().mode() & 0o077 == 0,
            "package state must be private"
        );
        let hash = session
            .account_hash
            .clone()
            .context("package session has no account binding")?;
        let marker = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(state.join(format!("package-create-{}.owner", plan.operation)))
            .context("package operation already started or unavailable")?;
        marker.sync_all()?;
        File::open(state)?.sync_all()?;
        let vault = SealedUploadCheckpointVault::new(state, &plan.scope.account)?;
        let saved = Checkpoint {
            version: 1,
            plan,
            account_hash: hash,
            phase: Phase::Prepared,
            slot: None,
            registration: None,
        };
        let mut this = Self {
            session,
            saved,
            vault: Arc::new(vault),
            staging: state.to_owned(),
            _marker: marker,
        };
        ensure!(
            this.vault.load(&this.key()).await?.is_none(),
            "package checkpoint already exists"
        );
        this.persist(Phase::Prepared).await?;
        Ok(this)
    }
    fn key(&self) -> String {
        format!("upload/{}", self.saved.plan.operation)
    }
    async fn persist(&mut self, phase: Phase) -> Result<()> {
        // Update memory first: a failed save cannot authorize re-entry.
        self.saved.phase = phase;
        let encoded = serde_json::to_string(&self.saved)?;
        ensure!(
            encoded.len() <= MAX_CHECKPOINT,
            "package checkpoint exceeds bound"
        );
        self.vault
            .save(&self.key(), SecretString::from(encoded))
            .await
    }
    async fn arm(&mut self, phase: Phase, cancel: &CancellationToken) -> Result<()> {
        ensure!(
            matches!(
                phase,
                Phase::AllocationStarted | Phase::BodyStarted | Phase::RegistrationStarted
            ),
            "invalid package mutation phase"
        );
        ensure!(!cancel.is_cancelled(), "package import cancelled");
        self.persist(phase).await?;
        // Saving can await a locked desktop vault. Cancellation during that wait
        // must not authorize the immediately following cloud mutation.
        ensure!(!cancel.is_cancelled(), "package import cancelled");
        Ok(())
    }
    async fn preflight(&mut self) -> Result<()> {
        let p = &self.saved.plan;
        let folder = self.session.folder_metadata(&p.parent).await?;
        ensure!(
            folder.kind == "FOLDER"
                && folder.zone == "com.apple.CloudDocs"
                && folder.drivewsid == p.parent
                && folder.parent_id == ROOT_ID
                && folder.display_name() == p.parent_name,
            "owned package folder changed"
        );
        let entries = self.session.list_folder(&p.parent).await?;
        ensure!(
            entries.len() == 1 && entries[0] == p.source,
            "owned package folder or source changed"
        );
        ensure!(
            matches!(
                self.session
                    .download_representation(&p.source.drivewsid)
                    .await?,
                crate::ContentRepresentation::Package(_)
            ),
            "source is not a native package"
        );
        Ok(())
    }
    /// Consumes the executor. Cancellation/error leaves the checkpoint and marker;
    /// neither this API nor inspection may retry any network mutation.
    pub async fn execute(
        mut self,
        source: File,
        cancel: &CancellationToken,
    ) -> Result<PackageCreateInspection> {
        ensure!(
            self.saved.phase == Phase::Prepared,
            "package operation already started"
        );
        let size = self.saved.plan.archive_size;
        let expected = self.saved.plan.archive_sha256.clone();
        // Copy into a private anonymous descriptor: callers cannot replace bytes
        // between hashing and HTTP streaming by changing the supplied pathname.
        let token = cancel.clone();
        let staging = self.staging.clone();
        let file = tokio::task::spawn_blocking(move || -> Result<File> {
            let mut source = source;
            ensure!(
                source.metadata()?.is_file() && source.metadata()?.len() == size,
                "package source size changed"
            );
            source.seek(SeekFrom::Start(0))?;
            let mut private = tempfile::tempfile_in(staging)?;
            let mut hash = Sha256::new();
            let mut count = 0u64;
            let mut buffer = [0; 64 * 1024];
            loop {
                ensure!(!token.is_cancelled(), "package import cancelled");
                let n = source.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                count = count
                    .checked_add(n as u64)
                    .context("package size overflow")?;
                ensure!(count <= size, "package source grew");
                hash.update(&buffer[..n]);
                private.write_all(&buffer[..n])?;
            }
            ensure!(
                count == size && hex::encode(hash.finalize()) == expected,
                "package source digest changed"
            );
            let receipt = crate::PackageDownload {
                size,
                sha256: expected,
            };
            // Enforce the archive contract here too: callers cannot bypass ZIP
            // safety/expansion validation by supplying a correct hash of junk.
            crate::compare_package_archives(&private, &receipt, &private, &receipt, &token)?;
            private.seek(SeekFrom::Start(0))?;
            Ok(private)
        })
        .await
        .context("package source staging interrupted")??;
        self.preflight().await?;
        ensure!(!cancel.is_cancelled(), "package import cancelled");
        self.arm(Phase::AllocationStarted, cancel).await?;
        let slot = transport::allocate(&mut self.session, &self.saved.plan).await?;
        ensure!(
            slot.document_id != self.saved.plan.source.docwsid,
            "package allocation reused source identity"
        );
        self.saved.slot = Some(slot);
        self.persist(Phase::Allocated).await?;
        self.preflight().await?;
        ensure!(!cancel.is_cancelled(), "package import cancelled");
        self.arm(Phase::BodyStarted, cancel).await?;
        let slot = self.saved.slot.as_ref().context("missing package slot")?;
        let registration = transport::upload(&mut self.session, slot, file, size).await?;
        self.saved.registration = Some(registration.expose_secret().to_owned());
        self.persist(Phase::BodyComplete).await?;
        self.preflight().await?;
        ensure!(!cancel.is_cancelled(), "package import cancelled");
        self.arm(Phase::RegistrationStarted, cancel).await?;
        transport::register(&mut self.session, &self.saved).await?;
        self.persist(Phase::Registered).await?;
        observe(&mut self.session, &self.saved).await
    }
    /// Read-only recovery, including after ambiguous HTTP responses. Never retries.
    pub async fn inspect(
        mut session: ICloudReadSession,
        plan: &OwnedPackagePlan,
        state: &Path,
    ) -> Result<PackageCreateInspection> {
        plan.validate()?;
        let vault = SealedUploadCheckpointVault::new(state, &plan.scope.account)?;
        let secret = vault
            .load(&format!("upload/{}", plan.operation))
            .await?
            .context("package checkpoint absent")?;
        ensure!(
            secret.expose_secret().len() <= MAX_CHECKPOINT,
            "package checkpoint exceeds bound"
        );
        let saved: Checkpoint = serde_json::from_str(secret.expose_secret())
            .map_err(|_| anyhow::anyhow!("invalid package checkpoint"))?;
        ensure!(
            saved.version == 1
                && saved.plan == *plan
                && session.account_hash.as_ref() == Some(&saved.account_hash),
            "package checkpoint identity mismatch"
        );
        if let Some(slot) = &saved.slot {
            slot.validate()?;
        }
        observe(&mut session, &saved).await
    }
}
async fn observe(
    session: &mut ICloudReadSession,
    saved: &Checkpoint,
) -> Result<PackageCreateInspection> {
    let Some(slot) = &saved.slot else {
        return Ok(PackageCreateInspection {
            allocated_document_id: None,
            observed: None,
            registration_confirmed: false,
        });
    };
    let entries = session.list_folder(&saved.plan.parent).await?;
    let observed = unique_observed(entries, &slot.document_id)?;
    if let Some(item) = &observed {
        ensure!(
            item.kind == "FILE"
                && item.zone == "com.apple.CloudDocs"
                && item.drivewsid == format!("FILE::com.apple.CloudDocs::{}", slot.document_id)
                && item.parent_id == saved.plan.parent
                && item.display_name() == saved.plan.destination
                && !item.etag.is_empty(),
            "package readback identity mismatch"
        );
    }
    Ok(PackageCreateInspection {
        allocated_document_id: Some(slot.document_id.clone()),
        observed,
        registration_confirmed: saved.phase == Phase::Registered,
    })
}
#[cfg(test)]
mod tests;

fn unique_observed(entries: Vec<DriveEntry>, document: &str) -> Result<Option<DriveEntry>> {
    let mut matches = entries.into_iter().filter(|item| item.docwsid == document);
    let observed = matches.next();
    ensure!(
        matches.next().is_none(),
        "duplicate package readback identity"
    );
    Ok(observed)
}
