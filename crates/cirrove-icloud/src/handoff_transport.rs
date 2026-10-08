//! Exact-identity staged replacement inspection shared by the normal adapter
//! and isolated probes. No fixture creation or uncontrolled update helpers live here.
use super::*;
use crate::write_transport::TRASH_ROOT;
// Version-2 checkpoint compatibility; new plans do not rely on fixture names.
const PROBE_PREFIX: &str = "Cirrove Write Validation-";
const PROBE_FILE: &str = "created-by-cirrove.txt";

// Generated transport names are not document identities. Keep a safe,
// bounded original suffix so a binary replacement is not allocated as text.
// Extensionless or unsafe/oversized suffixes use the binary .tmp fallback.
pub(crate) fn ordinary_replacement_suffix(name: &str) -> String {
    const MAX_SUFFIX: usize = 255 - "recovery-by-cirrove-".len() - 36;
    std::path::Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| {
            !extension.is_empty()
                && extension.len() < MAX_SUFFIX
                && !extension
                    .chars()
                    .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
        })
        .map_or_else(|| ".tmp".into(), |extension| format!(".{extension}"))
}

fn valid_etag(etag: &str) -> bool {
    !etag.is_empty() && etag.len() <= 4096 && !etag.contains(['\0', '\r', '\n'])
}
fn check_trash_identity_ids(
    item: &serde_json::Value,
    original_id: &str,
    original_doc_id: &str,
) -> Result<()> {
    if item.get("drivewsid").and_then(serde_json::Value::as_str) != Some(original_id)
        || item.get("docwsid").and_then(serde_json::Value::as_str) != Some(original_doc_id)
        || item.get("type").and_then(serde_json::Value::as_str) != Some("FILE")
        || !matches!(
            item.get("parentId").and_then(serde_json::Value::as_str),
            Some("TRASH_ROOT" | TRASH_ROOT)
        )
        || item
            .get("restorePath")
            .is_none_or(serde_json::Value::is_null)
    {
        bail!("iCloud Trash backup lacks its exact recovery identity");
    }
    Ok(())
}

/// Non-secret exact identities and content hashes needed to reconcile a
/// staged handoff after process death. The owning account is bound separately
/// by the private validation journal.
#[derive(Clone, Serialize, Deserialize)]
pub struct HandoffPlan {
    pub(crate) version: u8,
    #[serde(default = "default_handoff_parent_id")]
    pub(crate) folder_parent_id: String,
    pub(crate) folder_id: String,
    pub(crate) folder_name: String,
    pub(crate) original_id: String,
    pub(crate) original_doc_id: String,
    pub(crate) original_etag: String,
    pub(crate) staged_id: String,
    pub(crate) staged_doc_id: String,
    pub(crate) staged_etag: String,
    pub(crate) staged_name: String,
    pub(crate) recovery_name: String,
    #[serde(default = "default_handoff_target_name")]
    pub(crate) target_name: String,
    pub(crate) original_sha256: String,
    pub(crate) staged_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) package: Option<packages::NativePackageProof>,
}

fn default_handoff_target_name() -> String {
    PROBE_FILE.into()
}

fn default_handoff_parent_id() -> String {
    String::new()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoffObserved {
    Prepared,
    OldAtRecovery,
    Complete,
    Diverged,
}

impl ICloudReadSession {
    async fn handoff_items(&mut self, plan: &HandoffPlan) -> Result<Option<Vec<DriveEntry>>> {
        plan.validate()?;
        if matches!(plan.version, 6 | 7) {
            let folder = self.active_folder_metadata(&plan.folder_id).await?;
            if folder.drivewsid != plan.folder_id
                || folder.kind != "FOLDER"
                || (plan.version == 6
                    && (folder.parent_id != plan.folder_parent()
                        || folder.display_name() != plan.folder_name))
            {
                return Ok(None);
            }
            return Ok(Some(folder.items));
        }
        if plan.version == 5 {
            // New non-root plans address the captured folder directly. The
            // complete envelope proves its identity and supplies its children;
            // unrelated same-name siblings cannot redirect an ID-based handoff.
            // Persisted v2/v3 plans retain their parent-listing contract below.
            let folder = self.folder_metadata(&plan.folder_id).await?;
            if folder.drivewsid != plan.folder_id
                || folder.parent_id != plan.folder_parent()
                || folder.display_name() != plan.folder_name
                || folder.kind != "FOLDER"
            {
                return Ok(None);
            }
            return Ok(Some(folder.items));
        }
        if plan.folder_id != ROOT_ID
            && !plan.folder_present(&self.list_folder(plan.folder_parent()).await?)
        {
            return Ok(None);
        }
        Ok(Some(self.list_folder(&plan.folder_id).await?))
    }

    pub async fn inspect_durable_handoff(&mut self, plan: &HandoffPlan) -> Result<HandoffObserved> {
        if plan.package.is_some() {
            bail!("native handoff requires semantic transport");
        }
        let Some(items) = self.handoff_items(plan).await? else {
            return Ok(HandoffObserved::Diverged);
        };
        let Some((Some(old), new)) = plan.select_active(&items) else {
            return Ok(HandoffObserved::Diverged);
        };
        if old.is_folder()
            || new.is_folder()
            || old.docwsid != plan.original_doc_id
            || new.docwsid != plan.staged_doc_id
            || old.size > crate::MAX_WRITE_FILE_SIZE
            || new.size > crate::MAX_WRITE_FILE_SIZE
        {
            return Ok(HandoffObserved::Diverged);
        }
        let old_hash = self
            .hash_file_in_folder_for_revision(
                &plan.folder_id,
                &plan.original_id,
                &old.etag,
                old.size,
            )
            .await?;
        let new_hash = self
            .hash_file_in_folder_for_revision(&plan.folder_id, &plan.staged_id, &new.etag, new.size)
            .await?;
        if old_hash != plan.original_sha256 || new_hash != plan.staged_sha256 {
            return Ok(HandoffObserved::Diverged);
        }
        let (old_name, new_name) = (old.display_name(), new.display_name());
        Ok(
            if old_name == plan.target_name && new_name == plan.staged_name {
                if plan.revisions_match_prepared(&old.etag, &new.etag) {
                    HandoffObserved::Prepared
                } else {
                    HandoffObserved::Diverged
                }
            } else if old_name == plan.recovery_name && new_name == plan.staged_name {
                if plan.staged_etag == new.etag {
                    HandoffObserved::OldAtRecovery
                } else {
                    HandoffObserved::Diverged
                }
            } else if old_name == plan.recovery_name && new_name == plan.target_name {
                HandoffObserved::Complete
            } else {
                HandoffObserved::Diverged
            },
        )
    }

    /// Construct the two journal receipts only after independently verifying
    /// both exact IDs, names and complete bytes in the finished fixture.
    pub async fn verified_handoff_nodes(
        &mut self,
        plan: &HandoffPlan,
    ) -> Result<(cirrove_core::Node, cirrove_core::Node)> {
        use cirrove_core::{Node, NodeKind};
        if self.inspect_durable_handoff(plan).await? != HandoffObserved::Complete {
            bail!("iCloud handoff has no complete two-ID receipt");
        }
        let entries = self.list_folder(&plan.folder_id).await?;
        let Some((Some(old), current)) = plan.select_active(&entries) else {
            bail!("iCloud handoff receipt has ambiguous owned identities or names");
        };
        if old.is_folder()
            || current.is_folder()
            || old.display_name() != plan.recovery_name
            || current.display_name() != plan.target_name
            || old.docwsid != plan.original_doc_id
            || current.docwsid != plan.staged_doc_id
            || old.etag.is_empty()
            || current.etag.is_empty()
        {
            bail!("iCloud handoff receipt changed or differs from the saved bytes");
        }
        if self
            .hash_file_in_folder_for_revision(&plan.folder_id, &old.drivewsid, &old.etag, old.size)
            .await?
            != plan.original_sha256
            || self
                .hash_file_in_folder_for_revision(
                    &plan.folder_id,
                    &current.drivewsid,
                    &current.etag,
                    current.size,
                )
                .await?
                != plan.staged_sha256
        {
            bail!("iCloud handoff receipt changed or differs from the saved bytes");
        }
        let receipt = |entry: &DriveEntry, name: String| Node {
            id: entry.drivewsid.clone(),
            parent_id: Some(plan.folder_id.clone()),
            name,
            kind: NodeKind::File,
            size: entry.size,
            modified_unix: 0,
            etag: Some(entry.etag.clone()),
            content_version: Some(entry.etag.clone()),
            target: None,
            package: false,
        };
        Ok((
            receipt(current, plan.target_name.clone()),
            receipt(old, plan.recovery_name.clone()),
        ))
    }

    /// Exact-ID, full-byte observation of the former item in Trash. Both direct
    /// observations must explicitly identify a recoverable FILE in the Trash
    /// parent; changed ETag/size/name/restore metadata yields no receipt. This
    /// proves presence only and is never evidence of a complete Trash inventory.
    async fn verified_trash_backup_node(
        &mut self,
        plan: &HandoffPlan,
    ) -> Result<cirrove_core::Node> {
        self.verified_trash_backup_identity(
            &plan.original_id,
            &plan.original_doc_id,
            &plan.original_sha256,
        )
        .await
    }

    /// Bounded, read-only presence and raw-content proof for an owned ordinary
    /// DATA replacement. The caller must bind this session and both historical
    /// receipts to the registered account before calling. This does not inspect
    /// a complete Trash inventory or establish a restoration destination.
    #[cfg(feature = "write-probe")]
    pub async fn verify_owned_data_in_trash(
        &mut self,
        original: &cirrove_core::Node,
        backup: &cirrove_core::Node,
        source_sha256: &str,
    ) -> Result<cirrove_core::Node> {
        use cirrove_core::NodeKind;
        let ordinary = |node: &cirrove_core::Node| {
            node.kind == NodeKind::File
                && !node.package
                && node.target.is_none()
                && node.etag.as_deref().is_some_and(valid_etag)
                && node.content_revision().is_some()
        };
        let valid_name = |name: &str| {
            !name.is_empty()
                && name.len() <= 255
                && !matches!(name, "." | "..")
                && !name.contains(['/', '\\', '\0', '\r', '\n'])
        };
        if !ordinary(original)
            || !ordinary(backup)
            || original.id != backup.id
            || original.parent_id.as_deref().is_none_or(|parent| {
                parent
                    .strip_prefix("FOLDER::com.apple.CloudDocs::")
                    .is_none_or(|suffix| {
                        suffix.is_empty()
                            || suffix == "TRASH_ROOT"
                            || suffix.len() > 4096
                            || suffix.contains(['/', '\0', ':'])
                    })
            })
            || backup.parent_id.as_deref() != Some(TRASH_ROOT)
            || original.name != backup.name
            || !valid_name(&original.name)
            || original.size != backup.size
            || original.size > crate::MAX_WRITE_FILE_SIZE
            || source_sha256.len() != 64
            || !source_sha256
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            bail!("invalid owned ordinary DATA Trash binding");
        }
        let (zone, document) = split_file_id(&original.id)?;
        if zone != "com.apple.CloudDocs" {
            bail!("invalid owned ordinary DATA Trash binding");
        }
        let result = tokio::time::timeout(
            VERIFICATION_TRANSFER_TIMEOUT,
            self.verified_trash_backup_identity(&original.id, document, source_sha256),
        )
        .await
        .map_err(|_| anyhow!("owned ordinary DATA Trash verification timed out"))??;
        if &result != backup {
            bail!("owned ordinary DATA Trash receipt differs from the completed backup");
        }
        Ok(result)
    }

    async fn verified_trash_backup_identity(
        &mut self,
        original_id: &str,
        original_doc_id: &str,
        source_sha256: &str,
    ) -> Result<cirrove_core::Node> {
        use cirrove_core::{Node, NodeKind};
        let item = self.item_details(original_id).await?;
        check_trash_identity_ids(&item, original_id, original_doc_id)?;
        let etag = item
            .get("etag")
            .and_then(|value| value.as_str())
            .filter(|value| valid_etag(value))
            .context("iCloud Trash backup lacks an ETag")?
            .to_owned();
        let size = item
            .get("size")
            .and_then(|value| value.as_u64())
            .filter(|size| *size <= crate::MAX_WRITE_FILE_SIZE)
            .context("iCloud Trash backup exceeds the fixture limit")?;
        let base_name = item
            .get("name")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .context("iCloud Trash backup lacks a name")?;
        let extension = item
            .get("extension")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let actual_name = if extension.is_empty() {
            base_name.to_owned()
        } else {
            format!("{base_name}.{extension}")
        };
        if actual_name.len() > 255 || actual_name.contains(['/', '\\', '\0', '\r', '\n']) {
            bail!("iCloud Trash backup has an invalid name");
        }
        let mut received = 0u64;
        let mut hash = Sha256::new();
        // A zero-byte item has no content to download. It still needs the
        // exact-ID, ETag, size, recovery-path and digest checks on both sides.
        if size > 0 {
            let signed_url = self.ordinary_download_url(original_id).await?;
            let headers = crate::probe_timing::Timing::start("trash download headers");
            let mut response = self
                .http
                .get(signed_url)
                .timeout(VERIFICATION_TRANSFER_TIMEOUT)
                .send()
                .await
                .map_err(|_| anyhow!("iCloud Trash backup download failed"))?;
            backup_download_status(response.status())?;
            headers.finish();
            let _body = crate::probe_timing::Timing::start("trash download body");
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| anyhow!("iCloud Trash backup download interrupted"))?
            {
                received = received.saturating_add(chunk.len() as u64);
                if received > size {
                    bail!("iCloud Trash backup exceeds the fixture limit");
                }
                hash.update(&chunk);
            }
        }
        if received != size || hex::encode(hash.finalize()) != source_sha256 {
            bail!("iCloud Trash backup differs from the saved fixture bytes");
        }
        let unchanged = self.item_details(original_id).await?;
        check_trash_identity_ids(&unchanged, original_id, original_doc_id)?;
        if unchanged.get("etag").and_then(|value| value.as_str()) != Some(&etag)
            || unchanged.get("size").and_then(|value| value.as_u64()) != Some(size)
            || unchanged.get("name").and_then(|value| value.as_str()) != Some(base_name)
            || unchanged
                .get("extension")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                != extension
            || unchanged.get("restorePath") != item.get("restorePath")
        {
            bail!("iCloud Trash backup changed during exact-ID read");
        }
        Ok(Node {
            id: original_id.into(),
            parent_id: Some(TRASH_ROOT.into()),
            name: actual_name,
            kind: NodeKind::File,
            size,
            modified_unix: 0,
            etag: Some(etag.clone()),
            content_version: Some(etag),
            target: None,
            package: false,
        })
    }

    /// Read-only oracle for a registered interrupted-write validation arm.
    #[cfg(feature = "write-probe")]
    pub async fn probe_inspect_trash_handoff(
        &mut self,
        plan: &HandoffPlan,
    ) -> Result<HandoffObserved> {
        self.inspect_durable_trash_handoff(plan).await
    }

    pub(crate) async fn inspect_durable_trash_handoff(
        &mut self,
        plan: &HandoffPlan,
    ) -> Result<HandoffObserved> {
        Ok(self.inspect_trash_handoff_with_receipt(plan).await?.0)
    }

    /// One bounded observation supplies both the phase and, when complete,
    /// its verified receipt. The shared worker must not repeat full Trash
    /// downloads three times inside one provider deadline.
    pub(crate) async fn inspect_trash_handoff_with_receipt(
        &mut self,
        plan: &HandoffPlan,
    ) -> Result<(
        HandoffObserved,
        Option<(cirrove_core::Node, cirrove_core::Node)>,
    )> {
        if plan.package.is_some() {
            bail!("native handoff requires semantic transport");
        }
        use cirrove_core::{Node, NodeKind};
        let Some(items) = self.handoff_items(plan).await? else {
            return Ok((HandoffObserved::Diverged, None));
        };
        let Some((old, staged)) = plan.select_active(&items) else {
            return Ok((HandoffObserved::Diverged, None));
        };
        if old.is_some() {
            let state = self.inspect_durable_handoff(plan).await?;
            return Ok((
                if state == HandoffObserved::Prepared {
                    HandoffObserved::Prepared
                } else {
                    HandoffObserved::Diverged
                },
                None,
            ));
        }
        if staged.is_folder()
            || staged.drivewsid != plan.staged_id
            || staged.docwsid != plan.staged_doc_id
            || staged.size > crate::MAX_WRITE_FILE_SIZE
        {
            return Ok((HandoffObserved::Diverged, None));
        }
        if self
            .hash_file_in_folder_for_revision(
                &plan.folder_id,
                &plan.staged_id,
                &staged.etag,
                staged.size,
            )
            .await?
            != plan.staged_sha256
        {
            return Ok((HandoffObserved::Diverged, None));
        }
        let backup = self.verified_trash_backup_node(plan).await?;
        let later = self.list_folder(&plan.folder_id).await?;
        let Some((None, still_staged)) = plan.select_active(&later) else {
            return Ok((HandoffObserved::Diverged, None));
        };
        if still_staged.drivewsid != staged.drivewsid
            || still_staged.docwsid != staged.docwsid
            || still_staged.etag != staged.etag
            || still_staged.size != staged.size
            || still_staged.display_name() != staged.display_name()
        {
            return Ok((HandoffObserved::Diverged, None));
        }
        if staged.display_name() == plan.staged_name && staged.etag == plan.staged_etag {
            return Ok((HandoffObserved::OldAtRecovery, None));
        }
        if staged.display_name() != plan.target_name || !valid_etag(&staged.etag) {
            return Ok((HandoffObserved::Diverged, None));
        }
        let current = Node {
            id: staged.drivewsid.clone(),
            parent_id: Some(plan.folder_id.clone()),
            name: staged.display_name(),
            kind: NodeKind::File,
            size: staged.size,
            modified_unix: 0,
            etag: Some(staged.etag.clone()),
            content_version: Some(staged.etag.clone()),
            target: None,
            package: false,
        };
        Ok((HandoffObserved::Complete, Some((current, backup))))
    }

    pub async fn verified_trash_handoff_nodes(
        &mut self,
        plan: &HandoffPlan,
    ) -> Result<(cirrove_core::Node, cirrove_core::Node)> {
        self.inspect_trash_handoff_with_receipt(plan)
            .await?
            .1
            .context("iCloud Trash handoff has no complete receipt")
    }

    pub async fn move_old_to_recovery(&mut self, plan: &HandoffPlan) -> Result<bool> {
        if self.inspect_durable_handoff(plan).await? != HandoffObserved::Prepared {
            bail!("old iCloud validation item is not in the prepared state");
        }
        // Never refresh the precondition to a newer remote revision. Apple's
        // web rename may ignore a stale ETag, but a known intervening edit must
        // stop this research operation before its first mutating request.
        self.send_rename(&plan.original_id, &plan.original_etag, &plan.recovery_name)
            .await
    }

    pub async fn move_staged_to_target(&mut self, plan: &HandoffPlan) -> Result<bool> {
        if self.inspect_durable_handoff(plan).await? != HandoffObserved::OldAtRecovery {
            bail!("staged iCloud validation item is not ready for handoff");
        }
        self.send_rename(&plan.staged_id, &plan.staged_etag, &plan.target_name)
            .await
    }
}
impl HandoffPlan {
    pub(crate) fn folder_parent(&self) -> &str {
        if self.folder_parent_id.is_empty() && self.version == 2 {
            ROOT_ID
        } else {
            &self.folder_parent_id
        }
    }

    pub(crate) fn folder_present(&self, entries: &[DriveEntry]) -> bool {
        entries
            .iter()
            .filter(|entry| entry.drivewsid == self.folder_id)
            .count()
            == 1
            && entries
                .iter()
                .filter(|entry| entry.display_name() == self.folder_name)
                .count()
                == 1
            && entries.iter().any(|entry| {
                entry.drivewsid == self.folder_id
                    && entry.parent_id == self.folder_parent()
                    && entry.display_name() == self.folder_name
                    && entry.kind == "FOLDER"
            })
    }

    /// Keep unrelated siblings visible while refusing duplicate identities or
    /// another item occupying any name used by this two-ID handoff.
    pub(crate) fn select_active<'a>(
        &self,
        items: &'a [DriveEntry],
    ) -> Option<(Option<&'a DriveEntry>, &'a DriveEntry)> {
        let mut old = items
            .iter()
            .filter(|entry| entry.drivewsid == self.original_id);
        let original = old.next();
        if old.next().is_some() {
            return None;
        }
        let mut staged = items
            .iter()
            .filter(|entry| entry.drivewsid == self.staged_id);
        let new = staged.next()?;
        if staged.next().is_some()
            || items.iter().any(|entry| {
                entry.drivewsid != self.original_id
                    && entry.drivewsid != self.staged_id
                    && [
                        self.target_name.as_str(),
                        self.staged_name.as_str(),
                        self.recovery_name.as_str(),
                    ]
                    .contains(&entry.display_name().as_str())
            })
        {
            return None;
        }
        Some((original, new))
    }

    pub(crate) fn revisions_match_prepared(&self, old: &str, staged: &str) -> bool {
        old == self.original_etag && staged == self.staged_etag
    }

    pub(crate) fn validate(&self) -> Result<()> {
        let uuid_name = |name: &str, prefix: &str, suffix: &str| {
            name.strip_prefix(prefix)
                .and_then(|middle| middle.strip_suffix(suffix))
                .is_some_and(|middle| Uuid::parse_str(middle).is_ok())
        };
        let digest = |value: &str| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        };
        let old = split_file_id(&self.original_id)?;
        let new = split_file_id(&self.staged_id)?;
        let valid_parent = self
            .folder_parent_id
            .starts_with("FOLDER::com.apple.CloudDocs::")
            && self
                .folder_parent_id
                .rsplit("::")
                .next()
                .is_some_and(|id| !id.is_empty());
        let valid_folder_name = !self.folder_name.is_empty()
            && self.folder_name.len() <= 255
            && !matches!(self.folder_name.as_str(), "." | "..")
            && !self.folder_name.contains(['/', '\0', '\r', '\n']);
        let valid_version = match self.version {
            2 => self.folder_parent() == ROOT_ID && uuid_name(&self.folder_name, PROBE_PREFIX, ""),
            3 | 5 | 6 => {
                valid_parent && valid_folder_name && self.folder_parent_id != self.folder_id
            }
            4 | 7 => {
                self.folder_id == ROOT_ID && self.folder_parent_id.is_empty() && valid_folder_name
            }
            _ => false,
        };
        let native = matches!(self.version, 6 | 7);
        let ordinary_suffix = ordinary_replacement_suffix(&self.target_name);
        let suffix = if native {
            cirrove_core::upload::native_package_suffix(&self.target_name)
                .context("invalid native package format")?
        } else {
            ordinary_suffix.as_str()
        };
        let matching_names = |suffix: &str| {
            uuid_name(&self.staged_name, "staged-by-cirrove-", suffix)
                && uuid_name(&self.recovery_name, "recovery-by-cirrove-", suffix)
        };
        // Persisted ordinary checkpoints used .txt for both names, irrespective
        // of target. Admit that complete legacy pair, never a mixed pair.
        let names_valid = matching_names(suffix) || (!native && matching_names(".txt"));
        let content_valid = match &self.package {
            Some(proof) if native => {
                proof.validate().is_ok()
                    && self.original_sha256.is_empty()
                    && self.staged_sha256.is_empty()
                    && cirrove_core::upload::native_package_suffix(&self.target_name).is_some()
                    && !self.original_etag.contains('*')
                    && !self.staged_etag.contains('*')
            }
            None if !native => digest(&self.original_sha256) && digest(&self.staged_sha256),
            _ => false,
        };
        if !valid_version
            || (self.folder_id == ROOT_ID && !matches!(self.version, 4 | 7))
            || !self.folder_id.starts_with("FOLDER::com.apple.CloudDocs::")
            || self.folder_id.rsplit("::").next().is_none_or(str::is_empty)
            || !names_valid
            || self.staged_name.len() > 255
            || self.recovery_name.len() > 255
            || self.target_name.is_empty()
            || self.target_name.len() > 255
            || matches!(self.target_name.as_str(), "." | "..")
            || self.target_name.contains(['/', '\0', '\r', '\n'])
            || self.target_name == self.staged_name
            || self.target_name == self.recovery_name
            || self.original_id == self.staged_id
            || old.0 != "com.apple.CloudDocs"
            || new.0 != "com.apple.CloudDocs"
            || old.1 != self.original_doc_id
            || new.1 != self.staged_doc_id
            || !content_valid
            || !valid_etag(&self.original_etag)
            || !valid_etag(&self.staged_etag)
        {
            bail!("invalid durable iCloud handoff plan");
        }
        Ok(())
    }
}

fn backup_download_status(status: StatusCode) -> Result<()> {
    if status == StatusCode::OK {
        Ok(())
    } else {
        Err(crate::content_request_failure(
            status,
            "iCloud Trash backup download",
        ))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[test]
    fn signed_verification_download_distinguishes_storage_from_url_expiry() {
        for status in [200, 507, 401, 403, 503, 509] {
            let result = backup_download_status(StatusCode::from_u16(status).unwrap())
                .map_err(crate::file_create::map_session_error);
            match status {
                200 => assert!(result.is_ok()),
                507 => assert!(matches!(
                    result,
                    Err(cirrove_core::upload::UploadError::InsufficientStorage)
                )),
                _ => assert!(matches!(
                    result,
                    Err(cirrove_core::upload::UploadError::Uncertain)
                )),
            }
        }
    }

    fn plan() -> HandoffPlan {
        HandoffPlan {
            version: 2,
            folder_parent_id: ROOT_ID.into(),
            folder_id: "FOLDER::com.apple.CloudDocs::folder-1".into(),
            folder_name: format!("{PROBE_PREFIX}{}", Uuid::new_v4()),
            original_id: "FILE::com.apple.CloudDocs::old-1".into(),
            original_doc_id: "old-1".into(),
            original_etag: "old-revision".into(),
            staged_id: "FILE::com.apple.CloudDocs::new-1".into(),
            staged_doc_id: "new-1".into(),
            staged_etag: "staged-revision".into(),
            staged_name: format!("staged-by-cirrove-{}.txt", Uuid::new_v4()),
            recovery_name: format!("recovery-by-cirrove-{}.txt", Uuid::new_v4()),
            target_name: PROBE_FILE.into(),
            original_sha256: "a".repeat(64),
            staged_sha256: "b".repeat(64),
            package: None,
        }
    }

    async fn exact_trash_server(
        first: serde_json::Value,
        second: serde_json::Value,
    ) -> Result<(ICloudReadSession, tokio::task::JoinHandle<()>)> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move {
            for item in [first, second] {
                let (mut socket, _) = listener.accept().await.expect("accept");
                let mut bytes = Vec::new();
                let end = loop {
                    let mut block = [0; 1024];
                    let count = socket.read(&mut block).await.expect("read");
                    assert!(count > 0 && bytes.len() < 8192);
                    bytes.extend_from_slice(&block[..count]);
                    if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                        break end + 4;
                    }
                };
                let header = std::str::from_utf8(&bytes[..end])
                    .expect("headers")
                    .to_lowercase();
                let length: usize = header
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length: "))
                    .expect("length")
                    .parse()
                    .expect("number");
                while bytes.len() - end < length {
                    let mut block = [0; 1024];
                    let n = socket.read(&mut block).await.expect("body");
                    assert!(n > 0);
                    bytes.extend_from_slice(&block[..n]);
                }
                let request: serde_json::Value =
                    serde_json::from_slice(&bytes[end..end + length]).expect("json");
                let valid = header.starts_with("post /retrieveitemdetails ")
                    && request
                        == json!({"items":[{
                    "drivewsid":"FILE::com.apple.CloudDocs::old-1", "partialData":false,"includeHierarchy":false}]});
                let status = if valid { "200 OK" } else { "400 Bad Request" };
                let data = serde_json::to_vec(&json!({"items":[item]})).expect("body");
                let header = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    data.len()
                );
                socket.write_all(header.as_bytes()).await.expect("header");
                socket.write_all(&data).await.expect("body");
            }
        });
        let mut session = ICloudReadSession::new()?;
        session.drive_endpoint = Some(format!("http://{address}/").parse()?);
        Ok((session, server))
    }

    #[tokio::test]
    async fn direct_trash_backup_checks_membership_and_revision_on_both_observations() -> Result<()>
    {
        let mut plan = plan();
        plan.original_sha256 = hex::encode(Sha256::digest(b""));
        let metadata = json!({"drivewsid":plan.original_id,"docwsid":plan.original_doc_id,
            "parentId":"TRASH_ROOT","restorePath":["owned-parent"],"type":"FILE",
            "etag":"trash-revision","name":"Original","extension":"txt","size":0});
        let (mut session, server) = exact_trash_server(metadata.clone(), metadata.clone()).await?;
        let result = session.verified_trash_backup_node(&plan).await;
        server.abort();
        let node = result?;
        assert_eq!(node.id, plan.original_id);
        assert_eq!(node.parent_id.as_deref(), Some(TRASH_ROOT));
        assert_eq!(node.size, 0);
        assert_eq!(node.name, "Original.txt");
        for key in [
            "drivewsid",
            "docwsid",
            "parentId",
            "restorePath",
            "type",
            "etag",
            "size",
            "name",
        ] {
            let mut changed = metadata.clone();
            changed[key] = serde_json::Value::Null;
            for first_changed in [false, true] {
                let (first, second) = if first_changed {
                    (changed.clone(), metadata.clone())
                } else {
                    (metadata.clone(), changed.clone())
                };
                let (mut session, server) = exact_trash_server(first, second).await?;
                let result = session.verified_trash_backup_node(&plan).await;
                server.abort();
                assert!(
                    result.is_err(),
                    "accepted absent {key} (first={first_changed})"
                );
            }
        }
        let mut changed = metadata.clone();
        changed["etag"] = json!("other-revision");
        let (mut session, server) = exact_trash_server(metadata, changed).await?;
        let result = session.verified_trash_backup_node(&plan).await;
        server.abort();
        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn persisted_handoff_cannot_retarget_another_item_or_unowned_name() {
        let value = plan();
        value.validate().unwrap();
        let bytes = serde_json::to_vec(&value).unwrap();
        let restored: HandoffPlan = serde_json::from_slice(&bytes).unwrap();
        restored.validate().unwrap();

        let mut foreign = plan();
        foreign.original_id = "FILE::other.zone::old-1".into();
        assert!(foreign.validate().is_err());
        let mut swapped = plan();
        swapped.staged_doc_id = "unrelated".into();
        assert!(swapped.validate().is_err());
        let mut unsafe_name = plan();
        unsafe_name.recovery_name = "important.txt".into();
        assert!(unsafe_name.validate().is_err());
        let mut legacy = plan();
        legacy.version = 1;
        assert!(legacy.validate().is_err());
        let mut invalid_revision = plan();
        invalid_revision.original_etag = "new\nline".into();
        assert!(invalid_revision.validate().is_err());
    }

    #[test]
    fn nested_handoff_binds_its_actual_parent_and_rejects_legacy_retargeting() {
        let mut stored = serde_json::to_value(plan()).unwrap();
        stored["version"] = serde_json::json!(3);
        stored["folder_name"] = serde_json::json!("Correspondence");
        stored["folder_parent_id"] = serde_json::json!("FOLDER::com.apple.CloudDocs::parent-1");
        let nested: HandoffPlan = serde_json::from_value(stored.clone()).unwrap();
        nested.validate().unwrap();
        let folder = |id: &str, parent: &str, name: &str| -> DriveEntry {
            serde_json::from_value(serde_json::json!({
                "drivewsid": id,
                "parentId": parent,
                "name": name,
                "type": "FOLDER"
            }))
            .unwrap()
        };
        let matching = folder(
            &nested.folder_id,
            nested.folder_parent(),
            &nested.folder_name,
        );
        assert!(nested.folder_present(std::slice::from_ref(&matching)));
        assert!(
            !nested.folder_present(&[folder(&nested.folder_id, ROOT_ID, &nested.folder_name,)])
        );
        assert!(!nested.folder_present(&[
            matching,
            folder(
                "FOLDER::com.apple.CloudDocs::foreign",
                nested.folder_parent(),
                &nested.folder_name,
            ),
        ]));

        let mut missing_parent = stored.clone();
        missing_parent
            .as_object_mut()
            .unwrap()
            .remove("folder_parent_id");
        let missing_parent: HandoffPlan = serde_json::from_value(missing_parent).unwrap();
        assert!(missing_parent.validate().is_err());

        stored["version"] = serde_json::json!(2);
        let retargeted_legacy: HandoffPlan = serde_json::from_value(stored).unwrap();
        assert!(retargeted_legacy.validate().is_err());
    }

    #[test]
    fn handoff_checkpoint_keeps_its_actual_target_name() {
        let mut stored = serde_json::to_value(plan()).unwrap();
        stored["target_name"] = serde_json::json!("Mounted Create.txt");
        let restored: HandoffPlan = serde_json::from_value(stored).unwrap();
        restored.validate().unwrap();
        let again = serde_json::to_value(restored).unwrap();
        assert_eq!(again["target_name"], "Mounted Create.txt");

        let mut legacy = again.clone();
        legacy.as_object_mut().unwrap().remove("target_name");
        let legacy: HandoffPlan = serde_json::from_value(legacy).unwrap();
        assert_eq!(legacy.target_name, PROBE_FILE);

        let mut unsafe_name: HandoffPlan = serde_json::from_value(again).unwrap();
        unsafe_name.target_name = "../other-account.txt".into();
        assert!(unsafe_name.validate().is_err());
    }

    #[test]
    fn unrelated_siblings_are_allowed_but_name_collisions_are_not() {
        let plan = plan();
        let entry = |id: &str, name: &str| -> DriveEntry {
            serde_json::from_value(serde_json::json!({
                "drivewsid": id,
                "docwsid": id.rsplit("::").next().unwrap(),
                "name": name,
                "type": "FILE",
                "etag": "revision",
                "size": 3
            }))
            .unwrap()
        };
        let old = entry(&plan.original_id, &plan.target_name);
        let staged = entry(&plan.staged_id, &plan.staged_name);
        let other = entry("FILE::com.apple.CloudDocs::other", "Unrelated.txt");
        assert!(
            plan.select_active(&[old.clone(), staged.clone(), other])
                .is_some()
        );
        let collision = entry("FILE::com.apple.CloudDocs::other", &plan.target_name);
        assert!(
            plan.select_active(&[old.clone(), staged.clone(), collision])
                .is_none()
        );
        assert!(plan.select_active(&[old.clone(), old, staged]).is_none());
    }

    #[test]
    fn durable_handoff_refuses_changed_revisions_even_when_bytes_match() {
        let plan = plan();
        assert!(plan.revisions_match_prepared("old-revision", "staged-revision"));
        assert!(!plan.revisions_match_prepared("foreign-revision", "staged-revision"));
        assert!(!plan.revisions_match_prepared("old-revision", "foreign-revision"));
    }
}

#[cfg(test)]
mod folder_identity_tests;

pub(crate) mod packages;

#[cfg(all(test, feature = "write-probe"))]
mod owned_data_tests;
