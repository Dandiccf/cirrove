//! Two-identity native replacement transport. This does not edit a document in
//! place, preserve its sharing link/history, or own a replay journal. The caller
//! must durably arm each mutation before calling it and only inspect thereafter
//! if the outcome is uncertain. Ordinary HandoffComplete receipts remain closed.
use super::*;
use crate::package_trash::OwnedPackageTrashRequest;
use crate::write_staging::WriteStagingFile;
use cirrove_core::{CancellationToken, Node, NodeKind, ProviderError, reads::ReadWindowSink};
use std::{path::Path, sync::Arc};
const ARCHIVE_LIMIT: u64 = 64 * 1024 * 1024;

/// Explicit developer-only pause at the native install race boundary. Normal
/// session and replacement entrypoints never construct or invoke this probe.
#[cfg(feature = "write-probe")]
#[async_trait::async_trait]
pub trait NativeInstallPreflightProbe: Send + Sync {
    async fn after_final_preflight(
        &self,
        original_id: &str,
        staged_id: &str,
        parent_id: &str,
        cancel: &CancellationToken,
    ) -> Result<()>;
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativePackageProof {
    pub(crate) version: u8,
    pub(crate) account_hash: String,
    pub(crate) original: PackageSemanticIdentity,
    pub(crate) staged: PackageSemanticIdentity,
    pub(crate) original_size: u64,
    pub(crate) staged_size: u64,
}
impl NativePackageProof {
    pub(super) fn validate(&self) -> Result<()> {
        if self.version != 1
            || self.account_hash.len() != 64
            || !self
                .account_hash
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            || self.original_size > crate::MAX_WRITE_FILE_SIZE
            || self.staged_size > crate::MAX_WRITE_FILE_SIZE
            || self.original.validate().is_err()
            || self.staged.validate().is_err()
        {
            bail!("invalid native handoff content proof");
        }
        Ok(())
    }
}
fn proof<'a>(session: &ICloudReadSession, plan: &'a HandoffPlan) -> Result<&'a NativePackageProof> {
    plan.validate()?;
    let proof = plan
        .package
        .as_ref()
        .context("native handoff proof missing")?;
    if session.account_hash.as_deref() != Some(proof.account_hash.as_str()) {
        bail!("native handoff account mismatch");
    }
    Ok(proof)
}
async fn staging(session: &ICloudReadSession, directory: &Path) -> Result<Arc<WriteStagingFile>> {
    session
        .write_staging_budget
        .create(directory)
        .await
        .map_err(|_| anyhow!("native handoff staging unavailable"))
}
struct Sink {
    file: Arc<WriteStagingFile>,
    offset: u64,
}
#[async_trait::async_trait]
impl ReadWindowSink for Sink {
    async fn write_chunk(&mut self, bytes: &[u8]) -> std::result::Result<(), ProviderError> {
        if bytes.len() > 64 * 1024 {
            return Err(ProviderError::Protocol(
                "native handoff chunk exceeds bound",
            ));
        }
        self.file
            .write_at(self.offset, bytes.to_vec())
            .await
            .map_err(|_| ProviderError::Protocol("native handoff staging unavailable"))?;
        self.offset += bytes.len() as u64;
        Ok(())
    }
}
fn active(entry: &DriveEntry, plan: &HandoffPlan, original: bool, complete: bool) -> bool {
    let p = plan.package.as_ref().expect("validated proof");
    entry.kind == "FILE"
        && entry.zone == "com.apple.CloudDocs"
        && entry.parent_id == plan.folder_id
        && entry.drivewsid
            == if original {
                plan.original_id.as_str()
            } else {
                plan.staged_id.as_str()
            }
        && entry.docwsid
            == if original {
                plan.original_doc_id.as_str()
            } else {
                plan.staged_doc_id.as_str()
            }
        && entry.size
            == if original {
                p.original_size
            } else {
                p.staged_size
            }
        && entry.display_name()
            == if original || complete {
                plan.target_name.as_str()
            } else {
                plan.staged_name.as_str()
            }
        && valid_etag(&entry.etag)
        && !entry.etag.contains('*')
        && (complete
            || entry.etag
                == if original {
                    plan.original_etag.as_str()
                } else {
                    plan.staged_etag.as_str()
                })
}
fn same_entry(left: &DriveEntry, right: &DriveEntry) -> bool {
    left.drivewsid == right.drivewsid
        && left.docwsid == right.docwsid
        && left.kind == right.kind
        && left.zone == right.zone
        && left.parent_id == right.parent_id
        && left.display_name() == right.display_name()
        && left.size == right.size
        && left.etag == right.etag
}
fn node(entry: &DriveEntry) -> Node {
    Node {
        id: entry.drivewsid.clone(),
        parent_id: Some(if entry.parent_id == "TRASH_ROOT" {
            TRASH_ROOT.into()
        } else {
            entry.parent_id.clone()
        }),
        name: entry.display_name(),
        kind: NodeKind::Folder,
        size: entry.size,
        modified_unix: 0,
        etag: Some(entry.etag.clone()),
        content_version: None,
        target: None,
        package: true,
    }
}
impl ICloudReadSession {
    pub(crate) async fn verify_native_replacement_original(
        &mut self,
        parent: &Node,
        original: &Node,
        semantic: &PackageSemanticIdentity,
        directory: &Path,
        cancel: &CancellationToken,
    ) -> Result<()> {
        drop(staging(self, directory).await?);
        let folder = self.active_folder_metadata(&parent.id).await?;
        if folder.drivewsid != parent.id
            || folder.kind != "FOLDER"
            || (parent.id != ROOT_ID
                && (Some(folder.parent_id.as_str()) != parent.parent_id.as_deref()
                    || folder.display_name() != parent.name))
        {
            return Err(StaleRead.into());
        }
        let matches: Vec<_> = folder
            .items
            .iter()
            .filter(|e| e.drivewsid == original.id || e.display_name() == original.name)
            .collect();
        if matches.len() != 1 {
            return Err(StaleRead.into());
        }
        let entry = matches[0];
        if entry.drivewsid != original.id
            || entry.docwsid != original.id.rsplit("::").next().unwrap_or_default()
            || entry.kind != "FILE"
            || entry.zone != "com.apple.CloudDocs"
            || entry.parent_id != parent.id
            || entry.display_name() != original.name
            || Some(entry.etag.as_str()) != original.etag.as_deref()
            || entry.size != original.size
        {
            return Err(StaleRead.into());
        }
        self.verify_active_handoff_package(entry, semantic, directory, cancel)
            .await
    }
    async fn verify_active_handoff_package(
        &mut self,
        entry: &DriveEntry,
        expected: &PackageSemanticIdentity,
        directory: &Path,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let before = self.item_details(&entry.drivewsid).await?;
        let observed: DriveEntry = serde_json::from_value(before.clone())
            .map_err(|_| anyhow!("native handoff metadata invalid"))?;
        if !same_entry(entry, &observed) || before.get("restorePath").is_some_and(|v| !v.is_null())
        {
            return Err(StaleRead.into());
        }
        let file = staging(self, directory).await?;
        let mut sink = Sink {
            file: file.clone(),
            offset: 0,
        };
        let archive = self
            .download_package(&entry.parent_id, entry, ARCHIVE_LIMIT, &mut sink, cancel)
            .await?;
        drop(sink);
        let after = self.item_details(&entry.drivewsid).await?;
        let observed: DriveEntry = serde_json::from_value(after.clone())
            .map_err(|_| anyhow!("native handoff metadata invalid"))?;
        if !same_entry(entry, &observed) || after.get("restorePath").is_some_and(|v| !v.is_null()) {
            return Err(StaleRead.into());
        }
        let root = entry.display_name();
        let token = cancel.clone();
        let expected = expected.clone();
        tokio::task::spawn_blocking(move || {
            let actual = crate::package_archive_semantic_identity_versioned(
                file.borrowed_file(),
                &archive,
                &root,
                expected.version,
                &token,
            )?;
            if actual != expected {
                return Err(ProviderError::Protocol(
                    "native handoff package content mismatch",
                ));
            }
            Ok::<_, ProviderError>(())
        })
        .await
        .map_err(|_| anyhow!("native handoff verification interrupted"))??;
        Ok(())
    }
    pub(crate) async fn inspect_native_handoff_inner(
        &mut self,
        plan: &HandoffPlan,
        directory: &Path,
        cancel: &CancellationToken,
    ) -> Result<(HandoffObserved, Option<(Node, Node)>)> {
        let p = proof(self, plan)?.clone();
        // Validate disk destination before any provider request.
        drop(staging(self, directory).await?);
        let Some(items) = self.handoff_items(plan).await? else {
            return Ok((HandoffObserved::Diverged, None));
        };
        let Some((old, staged)) = plan.select_active(&items) else {
            return Ok((HandoffObserved::Diverged, None));
        };
        let complete = staged.display_name() == plan.target_name;
        if !active(staged, plan, false, complete) {
            return Ok((HandoffObserved::Diverged, None));
        }
        if let Some(old) = old {
            if complete || !active(old, plan, true, false) {
                return Ok((HandoffObserved::Diverged, None));
            }
            self.verify_active_handoff_package(old, &p.original, directory, cancel)
                .await?;
            self.verify_active_handoff_package(staged, &p.staged, directory, cancel)
                .await?;
            let Some(after) = self.handoff_items(plan).await? else {
                return Ok((HandoffObserved::Diverged, None));
            };
            let Some((Some(new_old), new_staged)) = plan.select_active(&after) else {
                return Ok((HandoffObserved::Diverged, None));
            };
            return Ok((
                if same_entry(old, new_old) && same_entry(staged, new_staged) {
                    HandoffObserved::Prepared
                } else {
                    HandoffObserved::Diverged
                },
                None,
            ));
        }
        let request = || OwnedPackageTrashRequest {
            apple_account: String::new(),
            drive_id: plan.original_id.clone(),
            document_id: plan.original_doc_id.clone(),
            expected_root: plan.target_name.clone(),
            semantic: p.original.clone(),
        };
        let before = self.item_details(&plan.original_id).await?;
        let binding = crate::package_trash::observation(&before, &request())?;
        let old: DriveEntry = serde_json::from_value(before)
            .map_err(|_| anyhow!("native handoff recovery metadata invalid"))?;
        if old.display_name() != plan.target_name || old.size != p.original_size {
            return Ok((HandoffObserved::Diverged, None));
        }
        self.verify_package_in_trash_for_account_hash(
            request(),
            &p.account_hash,
            staging(self, directory).await?,
            cancel,
        )
        .await?;
        self.verify_active_handoff_package(staged, &p.staged, directory, cancel)
            .await?;
        if crate::package_trash::observation(
            &self.item_details(&plan.original_id).await?,
            &request(),
        )? != binding
        {
            return Err(StaleRead.into());
        }
        let Some(after) = self.handoff_items(plan).await? else {
            return Ok((HandoffObserved::Diverged, None));
        };
        let Some((None, new_staged)) = plan.select_active(&after) else {
            return Ok((HandoffObserved::Diverged, None));
        };
        if !same_entry(staged, new_staged) {
            return Ok((HandoffObserved::Diverged, None));
        }
        Ok((
            if complete {
                HandoffObserved::Complete
            } else {
                HandoffObserved::OldAtRecovery
            },
            if complete {
                Some((node(staged), node(&old)))
            } else {
                None
            },
        ))
    }
    /// Read-only reconciliation for a captured two-ID package handoff. Missing
    /// original identity alone never proves recovery or permits an install.
    pub async fn inspect_native_package_handoff(
        &mut self,
        plan: &HandoffPlan,
        directory: &Path,
        cancel: &CancellationToken,
    ) -> Result<HandoffObserved> {
        tokio::select! { biased; _ = cancel.cancelled() => Err(ProviderError::Cancelled.into()),
        result = self.inspect_native_handoff_inner(plan,directory,cancel) => result.map(|v|v.0) }
    }
    /// Caller must persist the possibly-sent phase first. No retry is performed,
    /// and no returned revision is substituted for the captured original ETag.
    pub async fn trash_native_handoff_original(
        &mut self,
        plan: &HandoffPlan,
        directory: &Path,
        cancel: &CancellationToken,
    ) -> Result<bool> {
        if self
            .inspect_native_package_handoff(plan, directory, cancel)
            .await?
            != HandoffObserved::Prepared
        {
            return Ok(false);
        }
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled.into());
        }
        tokio::select! { biased; _ = cancel.cancelled() => Err(ProviderError::Cancelled.into()),
        result = self.send_trash(&plan.original_id,&plan.original_etag) => result }
    }
    /// Caller must persist the install possibly-sent phase first. Apple's rename
    /// is not proven revision-conditional; preflight plus postflight can detect
    /// divergence but cannot promise cross-client isolation or preserve links.
    pub async fn install_native_handoff_stage(
        &mut self,
        plan: &HandoffPlan,
        directory: &Path,
        cancel: &CancellationToken,
    ) -> Result<bool> {
        if self
            .inspect_native_package_handoff(plan, directory, cancel)
            .await?
            != HandoffObserved::OldAtRecovery
        {
            return Ok(false);
        }
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled.into());
        }
        tokio::select! { biased; _ = cancel.cancelled() => Err(ProviderError::Cancelled.into()),
        result = self.send_rename(&plan.staged_id,&plan.staged_etag,&plan.target_name) => result }
    }
    /// Feature-only same-operation install. The probe runs AFTER the final full
    /// OldAtRecovery observation and BEFORE the one rename. Do not add a second
    /// preflight after release: that would test the older preflight-refusal arm.
    #[cfg(feature = "write-probe")]
    pub async fn install_native_handoff_stage_with_preflight_probe(
        &mut self,
        plan: &HandoffPlan,
        directory: &Path,
        cancel: &CancellationToken,
        probe: &dyn NativeInstallPreflightProbe,
    ) -> Result<bool> {
        if self
            .inspect_native_package_handoff(plan, directory, cancel)
            .await?
            != HandoffObserved::OldAtRecovery
        {
            return Ok(false);
        }
        tokio::select! { biased;
            _ = cancel.cancelled() => return Err(ProviderError::Cancelled.into()),
            result = probe.after_final_preflight(&plan.original_id, &plan.staged_id, &plan.folder_id, cancel) => result?,
        }
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled.into());
        }
        tokio::select! { biased; _ = cancel.cancelled() => Err(ProviderError::Cancelled.into()),
        result = self.send_rename(&plan.staged_id,&plan.staged_etag,&plan.target_name) => result }
    }
    pub async fn verified_native_handoff_nodes(
        &mut self,
        plan: &HandoffPlan,
        directory: &Path,
        cancel: &CancellationToken,
    ) -> Result<Option<(Node, Node)>> {
        tokio::select! { biased; _ = cancel.cancelled() => Err(ProviderError::Cancelled.into()),
        result = self.inspect_native_handoff_inner(plan,directory,cancel) => result.map(|v|v.1) }
    }
}
#[cfg(test)]
mod tests;
