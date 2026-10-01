//! Deliberately narrow live-write experiment. Only a new, probe-named folder
//! and a new file inside that folder can be created. This is not WriteProvider.
use super::*;
pub(crate) use crate::upload_transport::{UploadSlot, Uploaded, UploadedFile};

const PROBE_PREFIX: &str = "Cirrove Write Validation-";
pub(crate) const PROBE_FILE: &str = "created-by-cirrove.txt";
pub(crate) const MAX_OWNED_UPLOAD: usize = 4 * 1024 * 1024;

fn content_type_for_name(name: &str) -> &'static str {
    if name.ends_with(".txt") {
        "text/plain"
    } else {
        "application/octet-stream"
    }
}

pub struct TrashListingProbe {
    pub entries: usize,
    pub complete: bool,
    pub entries_with_restore_path: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub enum TrashRestoreOutcome {
    RestoredSameIdAndBytes,
    RestoredNewIdAndBytes,
    RestoredNewDocumentIdAndBytes,
    RestoredDifferentNameAndBytes,
    RestoredDifferentDocumentIdAndNameWithBytes,
    Indeterminate(&'static str),
}

/// Unforgeable by callers outside this module; obtained only after creating
/// and listing a fresh fixture through this session.
#[derive(Clone)]
pub struct ValidationFolder {
    pub(crate) id: String,
    pub(crate) name: String,
}

impl ValidationFolder {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

pub struct ValidationFile {
    pub(crate) id: String,
    pub(crate) document_id: String,
    pub(crate) etag: String,
    pub(crate) name: String,
}

impl ValidationFile {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn document_id(&self) -> &str {
        &self.document_id
    }

    pub fn etag(&self) -> &str {
        &self.etag
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum SameIdUpdateOutcome {
    Updated,
    RejectedUnchanged,
    Indeterminate,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RenameProbeOutcome {
    StaleAccepted,
    StaleRejectedFreshAccepted,
    StaleRejectedFreshRejected,
    Indeterminate,
}

#[derive(Debug, PartialEq, Eq)]
pub enum MoveProbeOutcome {
    StaleAccepted,
    StaleRejectedCurrentIntact,
    Indeterminate,
}

#[derive(Debug, PartialEq, Eq)]
pub enum EmptyFolderMoveOutcome {
    MovedExactId,
    RejectedIntact,
    Indeterminate,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PopulatedFolderMoveOutcome {
    MovedExactTree,
    RejectedIntact,
    Indeterminate,
}

#[derive(Debug, PartialEq, Eq)]
pub enum MoveCollisionOutcome {
    RejectedBothIntact,
    DuplicateNameAfterMove,
    RenamedOnCollision,
    DestinationDisplaced,
    Indeterminate,
}

#[derive(Debug, PartialEq, Eq)]
pub enum TrashProbeOutcome {
    StaleAcceptedAbsentFromParent,
    StaleRejectedCurrentIntact,
    StaleRejectedFreshAcceptedAbsentFromParent,
    StaleRejectedFreshRejectedCurrentIntact,
    Indeterminate,
}

#[derive(Debug, PartialEq, Eq)]
pub enum OccupiedNameOutcome {
    RejectedBothIntact,
    AcceptedDuplicateName,
    ConflictRenamedBothIntact,
    OriginalNoLongerListed,
    Indeterminate,
}

#[derive(Debug, PartialEq, Eq)]
pub enum HandoffOutcome {
    BothPreservedAtTargetAndRecovery,
    RecoveryMovedStagingUnchanged,
    Indeterminate,
}

/// A pre-request identity reservation for one unique staged upload. The
/// remote file ID is discovered only after Apple's registration request.
#[derive(Serialize, Deserialize)]
pub struct StagedRegistrationPlan {
    version: u8,
    folder_id: String,
    folder_name: String,
    original_id: String,
    original_doc_id: String,
    original_etag: String,
    original_sha256: String,
    staged_name: String,
    staged_sha256: String,
    staged_id: Option<String>,
    staged_doc_id: Option<String>,
    staged_etag: Option<String>,
}

#[derive(Deserialize)]
struct TrashReply {
    items: Vec<TrashResult>,
}

#[derive(Deserialize)]
struct TrashResult {
    status: String,
}

pub(crate) struct OwnedRegistration<'a> {
    pub(crate) folder: &'a ValidationFolder,
    pub(crate) name: &'a str,
    pub(crate) slot: &'a UploadSlot,
    pub(crate) data: UploadedFile,
    pub(crate) size: u64,
    pub(crate) expected_bytes: Option<&'a [u8]>,
    pub(crate) discard_receipt: bool,
}

#[derive(Deserialize)]
struct UpdateReply {
    status: UpdateStatus,
    results: Vec<UpdateResult>,
}

#[derive(Deserialize)]
struct UpdateStatus {
    status_code: i64,
}

#[derive(Deserialize)]
struct UpdateResult {
    status: UpdateStatus,
    document: Option<UpdatedDocument>,
}

#[derive(Deserialize)]
struct UpdatedDocument {
    document_id: String,
    name: String,
    deleted: bool,
}

fn exactly_one<T>(mut values: Vec<T>, stage: &'static str) -> Result<T> {
    if values.len() != 1 {
        bail!("iCloud {stage} returned an unexpected result count");
    }
    Ok(values.remove(0))
}

impl ICloudReadSession {
    /// Bounded read-only inspection of Apple's special Trash root. It may
    /// lack the ordinary folder `type` field, so this deliberately avoids
    /// treating it as a normal mounted directory.
    pub async fn inspect_trash_listing(&mut self) -> Result<TrashListingProbe> {
        let (items, complete) = self.read_trash_items().await?;
        Ok(TrashListingProbe {
            entries: items.len(),
            complete,
            entries_with_restore_path: items
                .iter()
                .filter(|item| item.get("restorePath").is_some_and(|path| !path.is_null()))
                .count(),
        })
    }

    /// Independent validation of one exact Trash identity, without exposing
    /// provider response bodies to the caller.
    pub async fn exact_item_in_trash(&mut self, id: &str) -> Result<bool> {
        if id.is_empty() || !id.starts_with("FILE::com.apple.CloudDocs::") {
            bail!("invalid iCloud validation Trash identity");
        }
        let (items, complete) = self.read_trash_items().await?;
        if !complete {
            bail!("iCloud validation Trash listing is incomplete");
        }
        let matches: Vec<_> = items
            .iter()
            .filter(|item| item.get("drivewsid").and_then(|value| value.as_str()) == Some(id))
            .collect();
        if matches.len() != 1 {
            bail!(
                "iCloud validation Trash has {} matches for the exact file ID",
                matches.len()
            );
        }
        if matches[0]
            .get("restorePath")
            .is_none_or(|path| path.is_null())
        {
            bail!("iCloud validation Trash item lacks a restore path");
        }
        Ok(true)
    }

    async fn upload_probe_bytes(
        &mut self,
        name: &str,
        bytes: &[u8],
    ) -> Result<(UploadSlot, UploadedFile)> {
        let slot = self.allocate_upload_slot(name, bytes.len() as u64).await?;
        let data = self.upload_to_slot(&slot, name, bytes).await?;
        Ok((slot, data))
    }

    async fn upload_to_slot(
        &mut self,
        slot: &UploadSlot,
        name: &str,
        bytes: &[u8],
    ) -> Result<UploadedFile> {
        let upload_url = checked_content_url(&slot.url)?;
        let response = self
            .http
            .post(upload_url)
            .header("content-type", content_type_for_name(name))
            .body(bytes.to_vec())
            .send()
            .await
            .map_err(|_| anyhow!("iCloud validation content upload failed"))?;
        if !response.status().is_success() {
            return Err(drive_request_failure(
                response.status(),
                "iCloud validation content upload",
            ));
        }
        let uploaded: Uploaded = read_json(response, "iCloud validation content upload").await?;
        let data = uploaded.file;
        if data.size != bytes.len() as u64 || data.receipt.is_empty() || data.signature.is_empty() {
            bail!("iCloud upload receipt is incomplete");
        }
        Ok(data)
    }

    /// Create a unique fixture at the root. The caller must keep its returned ID;
    /// no retry is safe if the response is lost after Apple commits the request.
    pub async fn create_validation_folder(&mut self, name: &str) -> Result<ValidationFolder> {
        if !name.starts_with(PROBE_PREFIX) || Uuid::parse_str(&name[PROBE_PREFIX.len()..]).is_err()
        {
            bail!("write probe requires a fresh Cirrove validation folder name");
        }
        let root = self.list_root().await?;
        if root.iter().any(|entry| entry.display_name() == name) {
            bail!("iCloud validation folder already exists");
        }
        let created = self.create_folder_request(ROOT_ID, name).await?;
        let listed = self.list_root().await?;
        if !listed.iter().any(|entry| {
            entry.drivewsid == created && entry.display_name() == name && entry.is_folder()
        }) {
            bail!("iCloud did not list the created validation folder");
        }
        Ok(ValidationFolder {
            id: created,
            name: name.into(),
        })
    }

    /// Rebuild an owned fixture handle after a process restart, requiring the
    /// exact ID to remain a UUID-named folder directly under this account root.
    pub async fn validation_folder_at_root(&mut self, id: &str) -> Result<ValidationFolder> {
        let root = self.list_root().await?;
        let mut matches = root.iter().filter(|entry| {
            entry.drivewsid == id
                && entry.is_folder()
                && entry
                    .display_name()
                    .strip_prefix(PROBE_PREFIX)
                    .is_some_and(|suffix| Uuid::parse_str(suffix).is_ok())
        });
        let entry = matches.next().context("validation folder is absent")?;
        if matches.next().is_some() {
            bail!("validation folder identity is ambiguous");
        }
        Ok(ValidationFolder {
            id: entry.drivewsid.clone(),
            name: entry.display_name(),
        })
    }

    /// Create one empty nested folder in a fresh UUID-named validation parent.
    /// The returned exact ID and ETag must be persisted before a later move.
    pub async fn create_empty_nested_move_fixture(
        &mut self,
        parent: &ValidationFolder,
        name: &str,
    ) -> Result<(ValidationFolder, String)> {
        const PREFIX: &str = "Cirrove Nested Move-";
        if parent
            .name
            .strip_prefix(PROBE_PREFIX)
            .is_none_or(|suffix| Uuid::parse_str(suffix).is_err())
            || name
                .strip_prefix(PREFIX)
                .is_none_or(|suffix| Uuid::parse_str(suffix).is_err())
            || self.validation_folder_at_root(&parent.id).await?.name != parent.name
            || !self.list_folder(&parent.id).await?.is_empty()
        {
            bail!("invalid nested iCloud move fixture parent");
        }
        let id = self.create_folder_request(&parent.id, name).await?;
        let listed = self.list_folder(&parent.id).await?;
        if listed.len() != 1
            || listed[0].drivewsid != id
            || listed[0].display_name() != name
            || !listed[0].is_folder()
            || listed[0].etag.is_empty()
            || !self.list_folder(&id).await?.is_empty()
        {
            bail!("nested iCloud move fixture was not listed under its exact ID");
        }
        Ok((
            ValidationFolder {
                id,
                name: name.to_owned(),
            },
            listed[0].etag.clone(),
        ))
    }

    /// Send one conditional move for a newly created empty nested folder.
    /// A follow-up process must independently check both exact parents.
    pub async fn probe_empty_nested_folder_move(
        &mut self,
        source: &ValidationFolder,
        destination: &ValidationFolder,
        nested: &ValidationFolder,
        etag: &str,
    ) -> Result<EmptyFolderMoveOutcome> {
        if source.id == destination.id
            || nested.id == source.id
            || nested.id == destination.id
            || etag.is_empty()
        {
            bail!("invalid empty-folder move fixture");
        }
        for folder in [source, destination] {
            if self.validation_folder_at_root(&folder.id).await?.name != folder.name {
                bail!("empty-folder move parent identity changed");
            }
        }
        let before = self.list_folder(&source.id).await?;
        if before.len() != 1
            || before[0].drivewsid != nested.id
            || before[0].display_name() != nested.name
            || before[0].etag != etag
            || !before[0].is_folder()
            || !self.list_folder(&nested.id).await?.is_empty()
            || !self.list_folder(&destination.id).await?.is_empty()
        {
            bail!("empty-folder move preflight changed");
        }
        let accepted = self.send_move(&nested.id, etag, &destination.id).await?;
        let source_after = self.list_folder(&source.id).await?;
        let destination_after = self.list_folder(&destination.id).await?;
        let nested_empty = self.list_folder(&nested.id).await?.is_empty();
        if accepted
            && source_after.is_empty()
            && destination_after.len() == 1
            && destination_after[0].drivewsid == nested.id
            && destination_after[0].display_name() == nested.name
            && destination_after[0].is_folder()
            && nested_empty
        {
            return Ok(EmptyFolderMoveOutcome::MovedExactId);
        }
        if !accepted
            && source_after.len() == 1
            && source_after[0].drivewsid == nested.id
            && source_after[0].display_name() == nested.name
            && destination_after.is_empty()
            && nested_empty
        {
            return Ok(EmptyFolderMoveOutcome::RejectedIntact);
        }
        Ok(EmptyFolderMoveOutcome::Indeterminate)
    }

    /// Register one small file in the newly created, still-empty nested move
    /// fixture. This is deliberately separate from the root-only file helper.
    pub async fn create_file_in_nested_move_fixture(
        &mut self,
        source: &ValidationFolder,
        nested: &ValidationFolder,
        bytes: &[u8],
    ) -> Result<ValidationFile> {
        if bytes.is_empty()
            || bytes.len() > 4096
            || nested
                .name
                .strip_prefix("Cirrove Nested Move-")
                .is_none_or(|suffix| Uuid::parse_str(suffix).is_err())
            || self.validation_folder_at_root(&source.id).await?.name != source.name
        {
            bail!("invalid populated-folder move fixture");
        }
        let source_items = self.list_folder(&source.id).await?;
        if source_items.len() != 1
            || source_items[0].drivewsid != nested.id
            || source_items[0].display_name() != nested.name
            || !source_items[0].is_folder()
            || !self.list_folder(&nested.id).await?.is_empty()
        {
            bail!("nested move fixture is not empty under its exact parent");
        }
        let (slot, data) = self.upload_probe_bytes(PROBE_FILE, bytes).await?;
        self.register_owned_file(OwnedRegistration {
            folder: nested,
            name: PROBE_FILE,
            slot: &slot,
            data,
            size: bytes.len() as u64,
            expected_bytes: Some(bytes),
            discard_receipt: false,
        })
        .await?
        .context("populated-folder fixture registration was not confirmed")
    }

    /// Create one second-level folder and file, exclusively within a fresh
    /// Cirrove-owned move fixture. A caller must persist all returned exact
    /// identities before attempting to move the outer folder.
    pub async fn create_grandchild_move_fixture(
        &mut self,
        source: &ValidationFolder,
        outer: &ValidationFolder,
        name: &str,
        bytes: &[u8],
    ) -> Result<(ValidationFolder, ValidationFile)> {
        if bytes.is_empty()
            || bytes.len() > 4096
            || name
                .strip_prefix("Cirrove Nested Move-")
                .is_none_or(|suffix| Uuid::parse_str(suffix).is_err())
            || self.validation_folder_at_root(&source.id).await?.name != source.name
        {
            bail!("invalid two-level move fixture");
        }
        let source_items = self.list_folder(&source.id).await?;
        if source_items.len() != 1
            || source_items[0].drivewsid != outer.id
            || source_items[0].display_name() != outer.name
            || source_items[0].parent_id != source.id
            || !source_items[0].is_folder()
            || !self.list_folder(&outer.id).await?.is_empty()
        {
            bail!("outer move fixture is not empty under its exact parent");
        }
        let inner_id = self.create_folder_request(&outer.id, name).await?;
        let outer_items = self.list_folder(&outer.id).await?;
        if outer_items.len() != 1
            || outer_items[0].drivewsid != inner_id
            || outer_items[0].display_name() != name
            || outer_items[0].parent_id != outer.id
            || !outer_items[0].is_folder()
            || !self.list_folder(&inner_id).await?.is_empty()
        {
            bail!("inner move fixture was not listed under its exact ID");
        }
        let inner = ValidationFolder {
            id: inner_id,
            name: name.to_owned(),
        };
        let (slot, data) = self.upload_probe_bytes(PROBE_FILE, bytes).await?;
        let file = self
            .register_owned_file(OwnedRegistration {
                folder: &inner,
                name: PROBE_FILE,
                slot: &slot,
                data,
                size: bytes.len() as u64,
                expected_bytes: Some(bytes),
                discard_receipt: false,
            })
            .await?
            .context("two-level fixture registration was not confirmed")?;
        Ok((inner, file))
    }

    /// Probe one conditional move of a two-level, Cirrove-owned subtree.
    #[allow(clippy::too_many_arguments)]
    pub async fn probe_two_level_folder_move(
        &mut self,
        source: &ValidationFolder,
        destination: &ValidationFolder,
        outer: &ValidationFolder,
        outer_etag: &str,
        inner: &ValidationFolder,
        file: &ValidationFile,
        bytes: &[u8],
    ) -> Result<PopulatedFolderMoveOutcome> {
        if source.id == destination.id
            || outer_etag.is_empty()
            || file.name != PROBE_FILE
            || bytes.is_empty()
            || bytes.len() > 4096
        {
            bail!("invalid two-level folder move request");
        }
        for parent in [source, destination] {
            if self.validation_folder_at_root(&parent.id).await?.name != parent.name {
                bail!("two-level move parent identity changed");
            }
        }
        let source_before = self.list_folder(&source.id).await?;
        let outer_before = self.list_folder(&outer.id).await?;
        let inner_before = self.list_folder(&inner.id).await?;
        if source_before.len() != 1
            || source_before[0].drivewsid != outer.id
            || source_before[0].display_name() != outer.name
            || source_before[0].parent_id != source.id
            || source_before[0].etag != outer_etag
            || !source_before[0].is_folder()
            || outer_before.len() != 1
            || outer_before[0].drivewsid != inner.id
            || outer_before[0].display_name() != inner.name
            || outer_before[0].parent_id != outer.id
            || !outer_before[0].is_folder()
            || inner_before.len() != 1
            || inner_before[0].drivewsid != file.id
            || inner_before[0].docwsid != file.document_id
            || inner_before[0].display_name() != file.name
            || inner_before[0].parent_id != inner.id
            || inner_before[0].etag != file.etag
            || inner_before[0].size != bytes.len() as u64
            || self.read_small_file_in_folder(&inner.id, &file.id).await? != bytes
            || !self.list_folder(&destination.id).await?.is_empty()
        {
            bail!("two-level folder move preflight changed");
        }
        let accepted = self
            .send_move(&outer.id, outer_etag, &destination.id)
            .await?;
        let source_after = self.list_folder(&source.id).await?;
        let destination_after = self.list_folder(&destination.id).await?;
        let outer_after = self.list_folder(&outer.id).await?;
        let inner_after = self.list_folder(&inner.id).await?;
        let tree_intact = outer_after.len() == 1
            && outer_after[0].drivewsid == inner.id
            && outer_after[0].display_name() == inner.name
            && outer_after[0].parent_id == outer.id
            && outer_after[0].is_folder()
            && inner_after.len() == 1
            && inner_after[0].drivewsid == file.id
            && inner_after[0].docwsid == file.document_id
            && inner_after[0].display_name() == file.name
            && inner_after[0].parent_id == inner.id
            && inner_after[0].size == bytes.len() as u64
            && self.read_small_file_in_folder(&inner.id, &file.id).await? == bytes;
        if accepted
            && source_after.is_empty()
            && destination_after.len() == 1
            && destination_after[0].drivewsid == outer.id
            && destination_after[0].display_name() == outer.name
            && destination_after[0].parent_id == destination.id
            && destination_after[0].is_folder()
            && tree_intact
        {
            return Ok(PopulatedFolderMoveOutcome::MovedExactTree);
        }
        if !accepted
            && source_after.len() == 1
            && source_after[0].drivewsid == outer.id
            && source_after[0].etag == outer_etag
            && source_after[0].parent_id == source.id
            && destination_after.is_empty()
            && tree_intact
        {
            return Ok(PopulatedFolderMoveOutcome::RejectedIntact);
        }
        Ok(PopulatedFolderMoveOutcome::Indeterminate)
    }

    /// Move one newly created nested folder containing exactly its own file.
    /// The exact identities and expected bytes must be saved before this call.
    pub async fn probe_populated_nested_folder_move(
        &mut self,
        source: &ValidationFolder,
        destination: &ValidationFolder,
        nested: &ValidationFolder,
        nested_etag: &str,
        file: &ValidationFile,
        bytes: &[u8],
    ) -> Result<PopulatedFolderMoveOutcome> {
        if source.id == destination.id
            || nested_etag.is_empty()
            || file.name != PROBE_FILE
            || file.etag.is_empty()
            || bytes.is_empty()
            || bytes.len() > 4096
        {
            bail!("invalid populated-folder move request");
        }
        for folder in [source, destination] {
            if self.validation_folder_at_root(&folder.id).await?.name != folder.name {
                bail!("populated-folder move parent identity changed");
            }
        }
        let source_before = self.list_folder(&source.id).await?;
        let nested_before = self.list_folder(&nested.id).await?;
        if source_before.len() != 1
            || source_before[0].drivewsid != nested.id
            || source_before[0].display_name() != nested.name
            || source_before[0].etag != nested_etag
            || !source_before[0].is_folder()
            || nested_before.len() != 1
            || nested_before[0].drivewsid != file.id
            || nested_before[0].docwsid != file.document_id
            || nested_before[0].display_name() != file.name
            || nested_before[0].etag != file.etag
            || nested_before[0].size != bytes.len() as u64
            || self.read_small_file_in_folder(&nested.id, &file.id).await? != bytes
            || !self.list_folder(&destination.id).await?.is_empty()
        {
            bail!("populated-folder move preflight changed");
        }
        let accepted = self
            .send_move(&nested.id, nested_etag, &destination.id)
            .await?;
        let source_after = self.list_folder(&source.id).await?;
        let destination_after = self.list_folder(&destination.id).await?;
        let nested_after = self.list_folder(&nested.id).await?;
        let file_intact = nested_after.len() == 1
            && nested_after[0].drivewsid == file.id
            && nested_after[0].docwsid == file.document_id
            && nested_after[0].display_name() == file.name
            && nested_after[0].size == bytes.len() as u64
            && self.read_small_file_in_folder(&nested.id, &file.id).await? == bytes;
        if accepted
            && source_after.is_empty()
            && destination_after.len() == 1
            && destination_after[0].drivewsid == nested.id
            && destination_after[0].display_name() == nested.name
            && destination_after[0].is_folder()
            && file_intact
        {
            return Ok(PopulatedFolderMoveOutcome::MovedExactTree);
        }
        if !accepted
            && source_after.len() == 1
            && source_after[0].drivewsid == nested.id
            && source_after[0].display_name() == nested.name
            && destination_after.is_empty()
            && file_intact
        {
            return Ok(PopulatedFolderMoveOutcome::RejectedIntact);
        }
        Ok(PopulatedFolderMoveOutcome::Indeterminate)
    }

    /// Upload at most 4 KiB into a folder just created by this process. No
    /// overwrite, rename, trash, or general path-based write is exposed.
    pub async fn create_validation_file(
        &mut self,
        folder: &ValidationFolder,
        bytes: &[u8],
    ) -> Result<ValidationFile> {
        self.create_owned_file(folder, PROBE_FILE, bytes, true, false)
            .await
            .and_then(|file| file.context("iCloud validation file receipt was discarded"))
    }

    /// A second, uniquely named upload, only beside this process's original
    /// fixture. It cannot target an existing folder or overwrite a name.
    pub async fn create_staged_file(
        &mut self,
        folder: &ValidationFolder,
        original: &ValidationFile,
        bytes: &[u8],
    ) -> Result<ValidationFile> {
        let children = self.list_folder(&folder.id).await?;
        if children.len() != 1
            || children[0].drivewsid != original.id
            || children[0].display_name() != PROBE_FILE
            || original.name != PROBE_FILE
        {
            bail!("iCloud validation folder does not contain only the original fixture");
        }
        let name = format!("staged-by-cirrove-{}.txt", Uuid::new_v4());
        self.create_owned_file(folder, &name, bytes, false, false)
            .await
            .and_then(|file| file.context("iCloud staged file receipt was discarded"))
    }

    pub async fn prepare_staged_registration(
        &mut self,
        folder: &ValidationFolder,
        original: &ValidationFile,
        original_bytes: &[u8],
        staged_bytes: &[u8],
    ) -> Result<StagedRegistrationPlan> {
        if original.name != PROBE_FILE
            || original_bytes.is_empty()
            || staged_bytes.is_empty()
            || original_bytes.len() > 4096
            || staged_bytes.len() > 4096
        {
            bail!("invalid staged iCloud registration fixture");
        }
        let mut plan = StagedRegistrationPlan {
            version: 2,
            folder_id: folder.id.clone(),
            folder_name: folder.name.clone(),
            original_id: original.id.clone(),
            original_doc_id: original.document_id.clone(),
            original_etag: original.etag.clone(),
            original_sha256: hex::encode(Sha256::digest(original_bytes)),
            staged_name: format!("staged-by-cirrove-{}.txt", Uuid::new_v4()),
            staged_sha256: hex::encode(Sha256::digest(staged_bytes)),
            staged_id: None,
            staged_doc_id: None,
            staged_etag: None,
        };
        if self.inspect_staged_registration(&mut plan).await? {
            bail!("staged iCloud registration name was already present");
        }
        Ok(plan)
    }

    /// Deliberately discard the registration response after the request was
    /// sent. The plan must have been durably saved before this call.
    pub async fn send_staged_registration_discard_receipt(
        &mut self,
        plan: &mut StagedRegistrationPlan,
        bytes: &[u8],
    ) -> Result<()> {
        plan.validate()?;
        if plan.staged_id.is_some()
            || hex::encode(Sha256::digest(bytes)) != plan.staged_sha256
            || self.inspect_staged_registration(plan).await?
        {
            bail!("staged iCloud registration is no longer prepared");
        }
        let folder = ValidationFolder {
            id: plan.folder_id.clone(),
            name: plan.folder_name.clone(),
        };
        let receipt = self
            .create_owned_file(&folder, &plan.staged_name, bytes, false, true)
            .await?;
        if receipt.is_some() {
            bail!("staged registration receipt was unexpectedly retained");
        }
        Ok(())
    }

    /// Probe a safer replacement sequence on one new two-file fixture:
    /// reject a stale old-file ETag, then Trash the current old revision and
    /// install the staged file. This is not an atomic or mounted write path.
    pub async fn probe_conditional_trash_handoff(
        &mut self,
        folder: &ValidationFolder,
        original: &ValidationFile,
        staged: &ValidationFile,
        original_bytes: &[u8],
        revised_bytes: &[u8],
        staged_bytes: &[u8],
    ) -> Result<()> {
        if original.name != PROBE_FILE
            || !staged.name.starts_with("staged-by-cirrove-")
            || original.id == staged.id
            || original_bytes.is_empty()
            || revised_bytes.is_empty()
            || staged_bytes.is_empty()
            || original_bytes.len() > 4096
            || revised_bytes.len() > 4096
            || staged_bytes.len() > 4096
            || original_bytes == revised_bytes
            || folder
                .name
                .strip_prefix(PROBE_PREFIX)
                .is_none_or(|suffix| Uuid::parse_str(suffix).is_err())
        {
            bail!("invalid conditional Trash handoff fixture");
        }
        let before = self.list_folder(&folder.id).await?;
        if before.len() != 2
            || !before.iter().any(|entry| {
                entry.drivewsid == original.id
                    && entry.docwsid == original.document_id
                    && entry.etag == original.etag
                    && entry.display_name() == original.name
            })
            || !before.iter().any(|entry| {
                entry.drivewsid == staged.id
                    && entry.docwsid == staged.document_id
                    && entry.etag == staged.etag
                    && entry.display_name() == staged.name
            })
            || self
                .read_small_file_in_folder(&folder.id, &original.id)
                .await?
                != original_bytes
            || self
                .read_small_file_in_folder(&folder.id, &staged.id)
                .await?
                != staged_bytes
        {
            bail!("conditional Trash handoff pair changed before update");
        }
        if self
            .probe_same_id_update(folder, original, original_bytes, revised_bytes)
            .await?
            != SameIdUpdateOutcome::Updated
        {
            bail!("owned old-file revision did not advance exactly");
        }
        let revised = self.list_folder(&folder.id).await?;
        let current = exactly_one(
            revised
                .iter()
                .filter(|entry| entry.drivewsid == original.id)
                .collect(),
            "revised old file",
        )?;
        let current_etag = current.etag.clone();
        if revised.len() != 2
            || current_etag.is_empty()
            || current_etag == original.etag
            || !revised.iter().any(|entry| {
                entry.drivewsid == staged.id
                    && entry.etag == staged.etag
                    && entry.display_name() == staged.name
            })
            || self
                .read_small_file_in_folder(&folder.id, &original.id)
                .await?
                != revised_bytes
            || self
                .read_small_file_in_folder(&folder.id, &staged.id)
                .await?
                != staged_bytes
        {
            bail!("owned revision or staged file changed before conditional Trash");
        }
        if self.send_trash(&original.id, &original.etag).await? {
            bail!("stale ETag unexpectedly moved the newer old-file revision");
        }
        let after_stale = self.list_folder(&folder.id).await?;
        if after_stale.len() != 2
            || !after_stale
                .iter()
                .any(|entry| entry.drivewsid == original.id && entry.etag == current_etag)
            || !after_stale
                .iter()
                .any(|entry| entry.drivewsid == staged.id && entry.etag == staged.etag)
            || self
                .read_small_file_in_folder(&folder.id, &original.id)
                .await?
                != revised_bytes
            || self
                .read_small_file_in_folder(&folder.id, &staged.id)
                .await?
                != staged_bytes
        {
            bail!("stale Trash request did not preserve both exact files");
        }
        if !self.send_trash(&original.id, &current_etag).await? {
            bail!("current ETag did not move the exact old file to Trash");
        }
        let after_trash = self.list_folder(&folder.id).await?;
        if after_trash.len() != 1
            || after_trash[0].drivewsid != staged.id
            || after_trash[0].etag != staged.etag
            || after_trash[0].display_name() != staged.name
        {
            bail!("conditional Trash did not leave the staged ID intact");
        }
        self.verify_owned_trash_bytes(original, revised_bytes)
            .await?;
        if !self
            .send_rename(&staged.id, &staged.etag, PROBE_FILE)
            .await?
        {
            bail!("staged ID was not installed after conditional Trash");
        }
        let final_items = self.list_folder(&folder.id).await?;
        if final_items.len() != 1
            || final_items[0].drivewsid != staged.id
            || final_items[0].docwsid != staged.document_id
            || final_items[0].display_name() != PROBE_FILE
            || self
                .read_small_file_in_folder(&folder.id, &staged.id)
                .await?
                != staged_bytes
        {
            bail!("conditional Trash handoff did not install the staged bytes");
        }
        self.verify_owned_trash_bytes(original, revised_bytes)
            .await?;
        Ok(())
    }

    /// Reconcile by an exact parent ID and a UUID-reserved name, then bind
    /// the discovered remote ID only after full-byte verification. Absence is
    /// uncertain after a pending request and must not trigger a replay.
    pub async fn inspect_staged_registration(
        &mut self,
        plan: &mut StagedRegistrationPlan,
    ) -> Result<bool> {
        plan.validate()?;
        if !self.list_root().await?.iter().any(|entry| {
            entry.drivewsid == plan.folder_id
                && entry.display_name() == plan.folder_name
                && entry.is_folder()
        }) {
            bail!("staged iCloud parent identity changed");
        }
        let children = self.list_folder(&plan.folder_id).await?;
        if children.is_empty() || children.len() > 2 {
            bail!("staged iCloud parent has an unexpected item count");
        }
        let old = exactly_one(
            children
                .iter()
                .filter(|entry| entry.drivewsid == plan.original_id)
                .collect(),
            "staged registration original",
        )?;
        if old.is_folder()
            || old.docwsid != plan.original_doc_id
            || old.etag != plan.original_etag
            || old.display_name() != PROBE_FILE
            || old.size > 4096
            || hex::encode(Sha256::digest(
                self.read_small_file_in_folder(&plan.folder_id, &plan.original_id)
                    .await?,
            )) != plan.original_sha256
        {
            bail!("staged iCloud original changed");
        }
        if children.len() == 1 {
            if plan.staged_id.is_some() {
                bail!("previously bound staged iCloud ID disappeared");
            }
            return Ok(false);
        }
        let stage = exactly_one(
            children
                .iter()
                .filter(|entry| entry.display_name() == plan.staged_name)
                .collect(),
            "staged registration candidate",
        )?;
        if stage.is_folder()
            || stage.drivewsid == plan.original_id
            || stage.size > 4096
            || plan
                .staged_id
                .as_ref()
                .is_some_and(|id| id != &stage.drivewsid)
            || plan
                .staged_doc_id
                .as_ref()
                .is_some_and(|id| id != &stage.docwsid)
            || plan
                .staged_etag
                .as_ref()
                .is_some_and(|etag| etag != &stage.etag)
            || hex::encode(Sha256::digest(
                self.read_small_file_in_folder(&plan.folder_id, &stage.drivewsid)
                    .await?,
            )) != plan.staged_sha256
        {
            bail!("staged iCloud candidate identity or bytes differ");
        }
        plan.staged_id = Some(stage.drivewsid.clone());
        plan.staged_doc_id = Some(stage.docwsid.clone());
        plan.staged_etag = Some(stage.etag.clone());
        Ok(true)
    }

    /// Convert a verified, bound registration checkpoint into the existing
    /// two-ID handoff plan. No mutation occurs until the handoff plan is
    /// separately fsynced by its journal.
    pub async fn prepare_handoff_from_registration(
        &mut self,
        registration: &mut StagedRegistrationPlan,
    ) -> Result<HandoffPlan> {
        if !self.inspect_staged_registration(registration).await? {
            bail!("staged registration is not visible for handoff");
        }
        let plan = HandoffPlan {
            version: 2,
            folder_parent_id: ROOT_ID.into(),
            folder_id: registration.folder_id.clone(),
            folder_name: registration.folder_name.clone(),
            original_id: registration.original_id.clone(),
            original_doc_id: registration.original_doc_id.clone(),
            original_etag: registration.original_etag.clone(),
            staged_id: registration
                .staged_id
                .clone()
                .context("staged registration has no item identity")?,
            staged_doc_id: registration
                .staged_doc_id
                .clone()
                .context("staged registration has no document identity")?,
            staged_etag: registration
                .staged_etag
                .clone()
                .context("staged registration has no item revision")?,
            staged_name: registration.staged_name.clone(),
            recovery_name: format!("recovery-by-cirrove-{}.txt", Uuid::new_v4()),
            target_name: PROBE_FILE.into(),
            original_sha256: registration.original_sha256.clone(),
            staged_sha256: registration.staged_sha256.clone(),
            package: None,
        };
        if self.inspect_durable_handoff(&plan).await? != HandoffObserved::Prepared {
            bail!("registered staged file changed before handoff");
        }
        Ok(plan)
    }

    pub(crate) async fn create_owned_file(
        &mut self,
        folder: &ValidationFolder,
        name: &str,
        bytes: &[u8],
        require_empty: bool,
        discard_registration_receipt: bool,
    ) -> Result<Option<ValidationFile>> {
        self.create_owned_file_with_slot(
            folder,
            name,
            bytes,
            require_empty,
            discard_registration_receipt,
            None,
        )
        .await
    }

    pub(crate) async fn create_owned_file_with_slot(
        &mut self,
        folder: &ValidationFolder,
        name: &str,
        bytes: &[u8],
        require_empty: bool,
        discard_registration_receipt: bool,
        reserved_slot: Option<&UploadSlot>,
    ) -> Result<Option<ValidationFile>> {
        let folder_id = folder.id.as_str();
        if bytes.is_empty()
            || bytes.len() > reserved_slot.map_or(4096, |_| MAX_OWNED_UPLOAD)
            || !folder_id.starts_with("FOLDER::com.apple.CloudDocs::")
        {
            bail!("invalid iCloud validation upload");
        }
        if !self.list_root().await?.iter().any(|entry| {
            entry.drivewsid == folder.id && entry.display_name() == folder.name && entry.is_folder()
        }) {
            bail!("iCloud validation folder is no longer at the root");
        }
        let before = self.list_folder(folder_id).await?;
        if (require_empty && !before.is_empty())
            || before.iter().any(|entry| entry.display_name() == name)
        {
            bail!("iCloud validation destination is occupied");
        }
        let (slot, data) = match reserved_slot {
            Some(slot) => (slot.clone(), self.upload_to_slot(slot, name, bytes).await?),
            None => self.upload_probe_bytes(name, bytes).await?,
        };
        self.register_owned_file(OwnedRegistration {
            folder,
            name,
            slot: &slot,
            data,
            size: bytes.len() as u64,
            expected_bytes: Some(bytes),
            discard_receipt: discard_registration_receipt,
        })
        .await
    }

    pub(crate) async fn register_owned_file(
        &mut self,
        registration: OwnedRegistration<'_>,
    ) -> Result<Option<ValidationFile>> {
        let OwnedRegistration {
            folder,
            name,
            slot,
            data,
            size,
            expected_bytes,
            discard_receipt,
        } = registration;
        let folder_id = folder.id.as_str();
        let starting_document_id = folder_id
            .rsplit("::")
            .next()
            .context("invalid folder identity")?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis() as u64;
        let update_url = self
            .docs_endpoint
            .as_ref()
            .context("iCloud sign-in is not complete")?
            .join("ws/com.apple.CloudDocs/update/documents")?;
        let response = self
            .http
            .post(update_url)
            .header("origin", ICLOUD_ORIGIN)
            .header("referer", format!("{ICLOUD_ORIGIN}/"))
            .json(&json!({
                "allow_conflict": false,
                "btime": now,
                "command": "add_file",
                "create_short_guid": true,
                "data": {
                    "receipt": data.receipt,
                    "reference_signature": data.reference_signature,
                    "signature": data.signature,
                    "size": data.size,
                    "wrapping_key": data.wrapping_key
                },
                "document_id": slot.document_id,
                "file_flags": {"is_executable": false, "is_hidden": false, "is_writable": true},
                "mtime": now,
                "path": {"path": name, "starting_document_id": starting_document_id}
            }))
            .send()
            .await
            .map_err(|_| anyhow!("iCloud validation file registration failed"))?;
        if discard_receipt {
            // The response exists, but this process intentionally never parses
            // it or learns the resulting remote item ID.
            drop(response);
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(drive_request_failure(
                response.status(),
                "iCloud validation file registration",
            ));
        }
        let reply: UpdateReply = read_json(response, "iCloud validation file registration").await?;
        let result = exactly_one(reply.results, "file registration")?;
        if reply.status.status_code != 0 || result.status.status_code != 0 {
            bail!("iCloud rejected validation file registration");
        }
        let document = result
            .document
            .context("iCloud did not return the registered file")?;
        if document.deleted || document.name != name || document.document_id != slot.document_id {
            bail!("iCloud returned a different validation file");
        }
        let matches: Vec<_> = self
            .list_folder(folder_id)
            .await?
            .into_iter()
            .filter(|entry| entry.display_name() == name)
            .collect();
        let entry = exactly_one(matches, "validation file listing")?;
        if entry.is_folder() || entry.docwsid != document.document_id || entry.size != size {
            bail!("iCloud validation file listing differs from the upload");
        }
        if let Some(bytes) = expected_bytes {
            let readback = self
                .read_small_file_in_folder(folder_id, &entry.drivewsid)
                .await?;
            if readback != bytes {
                bail!("iCloud validation file readback differs from the upload");
            }
        }
        Ok(Some(ValidationFile {
            id: entry.drivewsid,
            document_id: entry.docwsid,
            etag: entry.etag,
            name: name.into(),
        }))
    }

    /// One intentionally narrow experiment against the file created above.
    /// The caller supplies its original bytes so a rejected request can be
    /// distinguished from a response whose effect is unknown. No retry occurs.
    pub async fn probe_same_id_update(
        &mut self,
        folder: &ValidationFolder,
        file: &ValidationFile,
        old_bytes: &[u8],
        new_bytes: &[u8],
    ) -> Result<SameIdUpdateOutcome> {
        if file.name != PROBE_FILE {
            bail!("invalid iCloud validation filename");
        }
        self.probe_named_same_id_update(folder, file, old_bytes, new_bytes)
            .await
    }

    async fn probe_named_same_id_update(
        &mut self,
        folder: &ValidationFolder,
        file: &ValidationFile,
        old_bytes: &[u8],
        new_bytes: &[u8],
    ) -> Result<SameIdUpdateOutcome> {
        if old_bytes.is_empty()
            || new_bytes.is_empty()
            || new_bytes.len() > 4096
            || old_bytes == new_bytes
            || file.etag.is_empty()
        {
            bail!("invalid iCloud validation update");
        }
        let items = self.list_folder(&folder.id).await?;
        let entry = exactly_one(
            items
                .iter()
                .filter(|entry| entry.drivewsid == file.id)
                .collect(),
            "validation base lookup",
        )?;
        if entry.docwsid != file.document_id
            || entry.etag != file.etag
            || entry.display_name() != file.name
            || self.read_small_file_in_folder(&folder.id, &file.id).await? != old_bytes
        {
            bail!("iCloud validation base has changed");
        }
        let accepted = self
            .send_named_same_id_update(
                &folder.id,
                &file.document_id,
                &file.etag,
                new_bytes,
                false,
                &file.name,
            )
            .await?;
        let after = self.list_folder(&folder.id).await?;
        let original: Vec<_> = after
            .iter()
            .filter(|entry| entry.drivewsid == file.id)
            .collect();
        if original.len() != 1
            || after
                .iter()
                .filter(|entry| entry.display_name() == file.name)
                .count()
                != 1
        {
            return Ok(SameIdUpdateOutcome::Indeterminate);
        }
        let original = original[0];
        if original.docwsid != file.document_id || original.is_folder() {
            return Ok(SameIdUpdateOutcome::Indeterminate);
        }
        let bytes = self.read_small_file_in_folder(&folder.id, &file.id).await?;
        if accepted && bytes == new_bytes {
            Ok(SameIdUpdateOutcome::Updated)
        } else if !accepted && original.etag == file.etag && bytes == old_bytes {
            Ok(SameIdUpdateOutcome::RejectedUnchanged)
        } else {
            Ok(SameIdUpdateOutcome::Indeterminate)
        }
    }

    /// Exercise the exact race between the worker's prepared observation and
    /// its old-ID Trash request. Only the fresh, named two-file fixture is
    /// eligible. A rejected stale request must leave both exact IDs readable.
    pub(crate) async fn probe_intervening_edit_rejects_trash(
        &mut self,
        plan: &HandoffPlan,
    ) -> Result<()> {
        plan.validate()?;
        let original = self
            .read_small_file_in_folder(&plan.folder_id, &plan.original_id)
            .await?;
        if original.is_empty()
            || original.len() > 4096
            || hex::encode(Sha256::digest(&original)) != plan.original_sha256
        {
            bail!("iCloud concurrent-edit fixture differs from saved bytes");
        }
        let mut revised = original.clone();
        revised[0] ^= 1;
        let folder = ValidationFolder {
            id: plan.folder_id.clone(),
            name: plan.folder_name.clone(),
        };
        let file = ValidationFile {
            id: plan.original_id.clone(),
            document_id: plan.original_doc_id.clone(),
            etag: plan.original_etag.clone(),
            name: PROBE_FILE.into(),
        };
        if self
            .probe_same_id_update(&folder, &file, &original, &revised)
            .await?
            != SameIdUpdateOutcome::Updated
        {
            bail!("iCloud intervening update was not confirmed");
        }
        // Send exactly the stale ETag captured by the worker. Never retry with
        // the newly observed ETag, even when Apple rejects this request.
        let accepted = self
            .send_trash(&plan.original_id, &plan.original_etag)
            .await?;
        let items = self.list_folder(&plan.folder_id).await?;
        if accepted || items.len() != 2 {
            bail!("iCloud stale Trash did not preserve the two-file fixture");
        }
        let old = exactly_one(
            items
                .iter()
                .filter(|item| item.drivewsid == plan.original_id)
                .collect(),
            "concurrent-edit old ID",
        )?;
        let staged = exactly_one(
            items
                .iter()
                .filter(|item| item.drivewsid == plan.staged_id)
                .collect(),
            "concurrent-edit staged ID",
        )?;
        if old.is_folder()
            || old.docwsid != plan.original_doc_id
            || old.display_name() != PROBE_FILE
            || old.etag == plan.original_etag
            || staged.is_folder()
            || staged.docwsid != plan.staged_doc_id
            || staged.etag != plan.staged_etag
            || staged.display_name() != plan.staged_name
            || self
                .read_small_file_in_folder(&plan.folder_id, &plan.original_id)
                .await?
                != revised
            || hex::encode(Sha256::digest(
                self.read_small_file_in_folder(&plan.folder_id, &plan.staged_id)
                    .await?,
            )) != plan.staged_sha256
        {
            bail!("iCloud concurrent-edit fixture identities or bytes changed");
        }
        let (trash, complete) = self.read_trash_items().await?;
        if !complete
            || trash.iter().any(|item| {
                item.get("drivewsid").and_then(|id| id.as_str()) == Some(plan.original_id.as_str())
            })
        {
            bail!("iCloud stale Trash left the old ID in an uncertain location");
        }
        Ok(())
    }

    async fn send_same_id_update(
        &mut self,
        folder_id: &str,
        document_id: &str,
        etag: &str,
        bytes: &[u8],
        http_if_match: bool,
    ) -> Result<bool> {
        self.send_named_same_id_update(
            folder_id,
            document_id,
            etag,
            bytes,
            http_if_match,
            PROBE_FILE,
        )
        .await
    }

    async fn send_named_same_id_update(
        &mut self,
        folder_id: &str,
        document_id: &str,
        etag: &str,
        bytes: &[u8],
        http_if_match: bool,
        name: &str,
    ) -> Result<bool> {
        let (_slot, data) = self.upload_probe_bytes(name, bytes).await?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis() as u64;
        let starting_document_id = folder_id
            .rsplit("::")
            .next()
            .context("invalid folder identity")?;
        let endpoint = self
            .docs_endpoint
            .as_ref()
            .context("iCloud sign-in is not complete")?
            .join("ws/com.apple.CloudDocs/update/documents")?;
        let mut request = self
            .http
            .post(endpoint)
            .header("origin", ICLOUD_ORIGIN)
            .header("referer", format!("{ICLOUD_ORIGIN}/"))
            .json(&json!({
                "allow_conflict": false,
                "btime": now,
                "command": "add_file",
                "create_short_guid": false,
                "data": {
                    "receipt": data.receipt,
                    "reference_signature": data.reference_signature,
                    "signature": data.signature,
                    "size": data.size,
                    "wrapping_key": data.wrapping_key
                },
                "document_id": document_id,
                "etag": etag,
                "file_flags": {"is_executable": false, "is_hidden": false, "is_writable": true},
                "mtime": now,
                "path": {"path": name, "starting_document_id": starting_document_id}
            }));
        if http_if_match {
            let tag = etag.trim_matches('"');
            if tag.is_empty() || tag.contains('"') || tag.contains('\n') || tag.contains('\r') {
                bail!("iCloud ETag cannot be sent as an HTTP precondition");
            }
            request = request.header(reqwest::header::IF_MATCH, format!("\"{tag}\""));
        }
        let response = request
            .send()
            .await
            .map_err(|_| anyhow!("iCloud same-ID update request failed"))?;
        if response.status().is_success() {
            let reply: UpdateReply = read_json(response, "iCloud same-ID update").await?;
            Ok(reply.status.status_code == 0
                && reply.results.len() == 1
                && reply.results[0].status.status_code == 0
                && reply.results[0].document.as_ref().is_some_and(|document| {
                    document.document_id == document_id
                        && document.name == name
                        && !document.deleted
                }))
        } else {
            Ok(false)
        }
    }

    /// Reuse the *original* ETag after one confirmed same-ID update. All bytes
    /// and identities belong to the newly created validation fixture.
    pub async fn probe_stale_etag_update(
        &mut self,
        folder: &ValidationFolder,
        file: &ValidationFile,
        current_bytes: &[u8],
        candidate_bytes: &[u8],
        http_if_match: bool,
    ) -> Result<SameIdUpdateOutcome> {
        if current_bytes.is_empty()
            || current_bytes.len() != candidate_bytes.len()
            || current_bytes == candidate_bytes
        {
            bail!("stale-revision probe requires different, same-size bytes");
        }
        let before = self.list_folder(&folder.id).await?;
        let entry = exactly_one(
            before
                .iter()
                .filter(|entry| entry.drivewsid == file.id)
                .collect(),
            "stale-revision base lookup",
        )?;
        if entry.docwsid != file.document_id
            || entry.display_name() != PROBE_FILE
            || entry.etag.is_empty()
            || entry.etag == file.etag
            || self.read_small_file_in_folder(&folder.id, &file.id).await? != current_bytes
        {
            bail!("iCloud did not expose the expected newer validation revision");
        }
        let current_etag = entry.etag.clone();
        let accepted = self
            .send_same_id_update(
                &folder.id,
                &file.document_id,
                &file.etag,
                candidate_bytes,
                http_if_match,
            )
            .await?;
        let after = self.list_folder(&folder.id).await?;
        let same_name: Vec<_> = after
            .iter()
            .filter(|entry| entry.display_name() == PROBE_FILE)
            .collect();
        if same_name.len() != 1
            || same_name[0].drivewsid != file.id
            || same_name[0].docwsid != file.document_id
        {
            return Ok(SameIdUpdateOutcome::Indeterminate);
        }
        let bytes = self.read_small_file_in_folder(&folder.id, &file.id).await?;
        if accepted && bytes == candidate_bytes {
            Ok(SameIdUpdateOutcome::Updated)
        } else if !accepted && same_name[0].etag == current_etag && bytes == current_bytes {
            Ok(SameIdUpdateOutcome::RejectedUnchanged)
        } else {
            Ok(SameIdUpdateOutcome::Indeterminate)
        }
    }

    /// On a fresh fixture only, ask whether a stale ETag prevents moving an
    /// exact item ID out of its parent. The request is sent once and never
    /// retried; absence from the parent does not by itself prove Trash state.
    pub async fn probe_stale_etag_trash(
        &mut self,
        folder: &ValidationFolder,
        file: &ValidationFile,
        current_bytes: &[u8],
        fresh_followup: bool,
    ) -> Result<TrashProbeOutcome> {
        if current_bytes.is_empty() || current_bytes.len() > 4096 || file.name != PROBE_FILE {
            bail!("invalid iCloud trash validation fixture");
        }
        let before = self.list_folder(&folder.id).await?;
        let entry = exactly_one(
            before
                .iter()
                .filter(|entry| entry.drivewsid == file.id)
                .collect(),
            "trash base lookup",
        )?;
        if before.len() != 1
            || entry.is_folder()
            || entry.docwsid != file.document_id
            || entry.display_name() != PROBE_FILE
            || entry.etag.is_empty()
            || entry.etag == file.etag
            || self.read_small_file_in_folder(&folder.id, &file.id).await? != current_bytes
        {
            bail!("iCloud trash base is not the expected newer fixture");
        }
        let current_etag = entry.etag.clone();
        let accepted = self.send_trash(&file.id, &file.etag).await?;
        let after = self.list_folder(&folder.id).await?;
        if accepted && after.iter().all(|entry| entry.drivewsid != file.id) {
            return Ok(TrashProbeOutcome::StaleAcceptedAbsentFromParent);
        }
        if !accepted
            && after.len() == 1
            && after[0].drivewsid == file.id
            && after[0].docwsid == file.document_id
            && after[0].etag == current_etag
            && self.read_small_file_in_folder(&folder.id, &file.id).await? == current_bytes
        {
            if !fresh_followup {
                return Ok(TrashProbeOutcome::StaleRejectedCurrentIntact);
            }
            let fresh_accepted = self.send_trash(&file.id, &current_etag).await?;
            let after_fresh = self.list_folder(&folder.id).await?;
            if fresh_accepted && after_fresh.iter().all(|entry| entry.drivewsid != file.id) {
                return Ok(TrashProbeOutcome::StaleRejectedFreshAcceptedAbsentFromParent);
            }
            if !fresh_accepted
                && after_fresh.len() == 1
                && after_fresh[0].drivewsid == file.id
                && after_fresh[0].docwsid == file.document_id
                && after_fresh[0].etag == current_etag
                && self.read_small_file_in_folder(&folder.id, &file.id).await? == current_bytes
            {
                return Ok(TrashProbeOutcome::StaleRejectedFreshRejectedCurrentIntact);
            }
        }
        Ok(TrashProbeOutcome::Indeterminate)
    }

    /// Exercise recoverable deletion on one newly created fixture. Every
    /// remote request is sent once; an uncertain result stops for inspection.
    pub async fn probe_trash_restore_cycle(
        &mut self,
        folder: &ValidationFolder,
        file: &ValidationFile,
        bytes: &[u8],
    ) -> Result<TrashRestoreOutcome> {
        if bytes.is_empty() || bytes.len() > 4096 || file.name != PROBE_FILE {
            bail!("invalid iCloud Trash restore validation fixture");
        }
        let before = self.list_folder(&folder.id).await?;
        if before.len() != 1
            || before[0].drivewsid != file.id
            || before[0].docwsid != file.document_id
            || before[0].display_name() != PROBE_FILE
            || before[0].etag != file.etag
            || self.read_small_file_in_folder(&folder.id, &file.id).await? != bytes
        {
            bail!("iCloud Trash restore base changed");
        }
        let accepted = self.send_trash(&file.id, &file.etag).await?;
        if !accepted
            || self
                .list_folder(&folder.id)
                .await?
                .iter()
                .any(|entry| entry.drivewsid == file.id)
        {
            return Ok(TrashRestoreOutcome::Indeterminate("trash_not_confirmed"));
        }
        let (trash, complete) = self.read_trash_items().await?;
        if !complete {
            return Ok(TrashRestoreOutcome::Indeterminate(
                "trash_listing_incomplete",
            ));
        }
        let candidate = exactly_one(
            trash
                .iter()
                .filter(|item| {
                    item.get("drivewsid").and_then(|id| id.as_str()) == Some(file.id.as_str())
                })
                .collect(),
            "validation item in Trash",
        )?;
        let etag = candidate
            .get("etag")
            .and_then(|etag| etag.as_str())
            .filter(|etag| !etag.is_empty())
            .context("iCloud Trash item has no ETag")?;
        if candidate
            .get("restorePath")
            .is_none_or(|path| path.is_null())
            || candidate
                .get("docwsid")
                .and_then(|doc| doc.as_str())
                .is_some_and(|doc| !doc.is_empty() && doc != file.document_id)
        {
            bail!("iCloud Trash item lacks expected recovery identity");
        }
        let endpoint = self
            .drive_endpoint
            .as_ref()
            .context("iCloud sign-in is not complete")?
            .join("putBackItemsFromTrash")?;
        let response = self
            .http
            .post(endpoint)
            .header("origin", ICLOUD_ORIGIN)
            .header("referer", format!("{ICLOUD_ORIGIN}/"))
            .json(&json!({"items": [{"drivewsid": file.id, "etag": etag}]}))
            .send()
            .await
            .map_err(|_| anyhow!("iCloud validation Trash restore request failed"))?;
        let restored = if response.status().is_success() {
            let reply: TrashReply = read_json(response, "iCloud validation Trash restore").await?;
            exactly_one(reply.items, "Trash restore")?.status == "OK"
        } else {
            false
        };
        let after = self.list_folder(&folder.id).await?;
        if !restored {
            return Ok(TrashRestoreOutcome::Indeterminate("restore_not_accepted"));
        }
        if after.len() != 1 {
            return Ok(TrashRestoreOutcome::Indeterminate("restore_parent_count"));
        }
        let item = &after[0];
        if item.is_folder() {
            return Ok(TrashRestoreOutcome::Indeterminate("restore_kind"));
        }
        if self
            .read_small_file_in_folder(&folder.id, &item.drivewsid)
            .await?
            != bytes
        {
            return Ok(TrashRestoreOutcome::Indeterminate("restore_bytes"));
        }
        let same_document = item.docwsid == file.document_id;
        let same_name = item.display_name() == PROBE_FILE;
        if !same_document && !same_name {
            return Ok(TrashRestoreOutcome::RestoredDifferentDocumentIdAndNameWithBytes);
        }
        if !same_document {
            return Ok(TrashRestoreOutcome::RestoredNewDocumentIdAndBytes);
        }
        if !same_name {
            return Ok(TrashRestoreOutcome::RestoredDifferentNameAndBytes);
        }
        Ok(if item.drivewsid == file.id {
            TrashRestoreOutcome::RestoredSameIdAndBytes
        } else {
            TrashRestoreOutcome::RestoredNewIdAndBytes
        })
    }

    /// Check whether a recoverable old version can still be read by its
    /// exact provider ID while it is in Trash. Only a new owned fixture is
    /// eligible; neither restore nor permanent deletion is attempted.
    pub async fn probe_owned_trash_download(
        &mut self,
        folder: &ValidationFolder,
        file: &ValidationFile,
        bytes: &[u8],
    ) -> Result<()> {
        if bytes.is_empty()
            || bytes.len() > 4096
            || file.name != PROBE_FILE
            || folder
                .name
                .strip_prefix(PROBE_PREFIX)
                .is_none_or(|suffix| Uuid::parse_str(suffix).is_err())
        {
            bail!("invalid iCloud Trash download fixture");
        }
        let before = self.list_folder(&folder.id).await?;
        if before.len() != 1
            || before[0].drivewsid != file.id
            || before[0].docwsid != file.document_id
            || before[0].display_name() != PROBE_FILE
            || before[0].etag != file.etag
            || self.read_small_file_in_folder(&folder.id, &file.id).await? != bytes
        {
            bail!("iCloud Trash download base changed");
        }
        if !self.send_trash(&file.id, &file.etag).await?
            || self
                .list_folder(&folder.id)
                .await?
                .iter()
                .any(|entry| entry.drivewsid == file.id)
        {
            bail!("iCloud did not confirm the exact fixture in Trash");
        }
        self.verify_owned_trash_bytes(file, bytes).await?;
        Ok(())
    }

    /// The result of a completed conditional Trash step remains bound to the
    /// exact item ID, full bytes and complete recovery metadata.
    async fn verify_owned_trash_bytes(
        &mut self,
        file: &ValidationFile,
        bytes: &[u8],
    ) -> Result<()> {
        let (items, complete) = self.read_trash_items().await?;
        if !complete {
            bail!("iCloud Trash listing is incomplete");
        }
        let item = exactly_one(
            items
                .iter()
                .filter(|item| item.get("drivewsid").and_then(|id| id.as_str()) == Some(&file.id))
                .collect(),
            "owned file in Trash",
        )?;
        let etag = item
            .get("etag")
            .and_then(|etag| etag.as_str())
            .filter(|etag| !etag.is_empty())
            .context("iCloud Trash item has no ETag")?
            .to_owned();
        if item.get("size").and_then(|size| size.as_u64()) != Some(bytes.len() as u64)
            || item.get("restorePath").is_none_or(|path| path.is_null())
            || item
                .get("docwsid")
                .and_then(|id| id.as_str())
                .is_some_and(|id| !id.is_empty() && id != file.document_id)
        {
            bail!("iCloud Trash item lacks the expected recoverable identity");
        }
        let signed_url = self.ordinary_download_url(&file.id).await?;
        let mut response = self
            .http
            .get(signed_url)
            .send()
            .await
            .map_err(|_| anyhow!("iCloud Trash content request failed"))?;
        if response.status() != StatusCode::OK {
            bail!(
                "iCloud Trash content request failed ({})",
                response.status().as_u16()
            );
        }
        let mut received = Vec::with_capacity(bytes.len());
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow!("iCloud Trash content response was interrupted"))?
        {
            if received.len().saturating_add(chunk.len()) > 4096 {
                bail!("iCloud Trash file exceeds the bounded fixture limit");
            }
            received.extend_from_slice(&chunk);
        }
        if received != bytes {
            bail!("iCloud Trash bytes differ from the original fixture");
        }
        let (after, complete) = self.read_trash_items().await?;
        if !complete {
            bail!("iCloud Trash listing became incomplete after the read");
        }
        let unchanged = exactly_one(
            after
                .iter()
                .filter(|item| item.get("drivewsid").and_then(|id| id.as_str()) == Some(&file.id))
                .collect(),
            "owned file after Trash read",
        )?;
        if unchanged.get("etag").and_then(|value| value.as_str()) != Some(etag.as_str())
            || unchanged.get("size").and_then(|value| value.as_u64()) != Some(bytes.len() as u64)
            || unchanged
                .get("restorePath")
                .is_none_or(|path| path.is_null())
        {
            bail!("iCloud Trash item changed during the exact-ID read");
        }
        Ok(())
    }

    /// Test a current-ETag move into a distinct owned file's occupied name.
    /// Only exact IDs and complete bytes establish the disposition of each
    /// item; an HTTP response or a name alone cannot do so.
    pub async fn probe_occupied_move_name(
        &mut self,
        source: &ValidationFolder,
        destination: &ValidationFolder,
        moving: &ValidationFile,
        moving_bytes: &[u8],
        occupant: &ValidationFile,
        occupant_bytes: &[u8],
    ) -> Result<MoveCollisionOutcome> {
        if source.id == destination.id
            || moving.id == occupant.id
            || moving.name != occupant.name
            || moving_bytes.is_empty()
            || occupant_bytes.is_empty()
            || moving_bytes.len() > 4096
            || occupant_bytes.len() > 4096
        {
            bail!("invalid occupied move fixture");
        }
        let root = self.list_root().await?;
        for folder in [source, destination] {
            if root
                .iter()
                .filter(|entry| {
                    entry.drivewsid == folder.id
                        && entry.display_name() == folder.name
                        && entry.is_folder()
                })
                .count()
                != 1
            {
                bail!("occupied move folder identity changed");
            }
        }
        let source_before = self.list_folder(&source.id).await?;
        let destination_before = self.list_folder(&destination.id).await?;
        if source_before.len() != 1
            || destination_before.len() != 1
            || source_before[0].drivewsid != moving.id
            || source_before[0].docwsid != moving.document_id
            || source_before[0].display_name() != moving.name
            || source_before[0].etag != moving.etag
            || destination_before[0].drivewsid != occupant.id
            || destination_before[0].docwsid != occupant.document_id
            || destination_before[0].display_name() != occupant.name
            || destination_before[0].etag != occupant.etag
            || self
                .read_small_file_in_folder(&source.id, &moving.id)
                .await?
                != moving_bytes
            || self
                .read_small_file_in_folder(&destination.id, &occupant.id)
                .await?
                != occupant_bytes
        {
            bail!("occupied move fixture changed before request");
        }
        let accepted = self
            .send_move(&moving.id, &moving.etag, &destination.id)
            .await?;
        let source_after = self.list_folder(&source.id).await?;
        let destination_after = self.list_folder(&destination.id).await?;
        let moving_at_source = source_after.iter().find(|item| item.drivewsid == moving.id);
        let moving_at_destination = destination_after
            .iter()
            .find(|item| item.drivewsid == moving.id);
        let occupant_at_destination = destination_after
            .iter()
            .find(|item| item.drivewsid == occupant.id);
        if !accepted
            && source_after.len() == 1
            && destination_after.len() == 1
            && moving_at_source.is_some_and(|item| {
                item.docwsid == moving.document_id
                    && item.display_name() == moving.name
                    && item.etag == moving.etag
            })
            && occupant_at_destination.is_some_and(|item| {
                item.docwsid == occupant.document_id
                    && item.display_name() == occupant.name
                    && item.etag == occupant.etag
            })
            && self
                .read_small_file_in_folder(&source.id, &moving.id)
                .await?
                == moving_bytes
            && self
                .read_small_file_in_folder(&destination.id, &occupant.id)
                .await?
                == occupant_bytes
        {
            return Ok(MoveCollisionOutcome::RejectedBothIntact);
        }
        if source_after.is_empty()
            && moving_at_destination.is_some_and(|item| item.docwsid == moving.document_id)
            && self
                .read_small_file_in_folder(&destination.id, &moving.id)
                .await?
                == moving_bytes
        {
            if destination_after.len() == 2
                && occupant_at_destination.is_some_and(|item| {
                    item.docwsid == occupant.document_id && item.display_name() == occupant.name
                })
                && self
                    .read_small_file_in_folder(&destination.id, &occupant.id)
                    .await?
                    == occupant_bytes
            {
                if moving_at_destination.is_some_and(|item| item.display_name() == moving.name) {
                    return Ok(MoveCollisionOutcome::DuplicateNameAfterMove);
                }
                return Ok(MoveCollisionOutcome::RenamedOnCollision);
            }
            if accepted && occupant_at_destination.is_none() {
                return Ok(MoveCollisionOutcome::DestinationDisplaced);
            }
        }
        Ok(MoveCollisionOutcome::Indeterminate)
    }

    /// A single stale-ETag move between two new Cirrove-owned root folders.
    /// The caller has already established a newer same-ID content revision.
    /// Never retry a possibly accepted move, even when its response is lost.
    pub async fn probe_stale_etag_move(
        &mut self,
        source: &ValidationFolder,
        destination: &ValidationFolder,
        file: &ValidationFile,
        revised: &[u8],
    ) -> Result<MoveProbeOutcome> {
        if source.id == destination.id || revised.is_empty() || revised.len() > 4096 {
            bail!("invalid owned move fixture");
        }
        let root = self.list_root().await?;
        for folder in [source, destination] {
            let matches: Vec<_> = root
                .iter()
                .filter(|entry| {
                    entry.drivewsid == folder.id
                        && entry.display_name() == folder.name
                        && entry.is_folder()
                })
                .collect();
            if matches.len() != 1 {
                bail!("owned move folder identity changed");
            }
        }
        let before_source = self.list_folder(&source.id).await?;
        let before_destination = self.list_folder(&destination.id).await?;
        let current = exactly_one(
            before_source
                .iter()
                .filter(|entry| entry.drivewsid == file.id)
                .collect(),
            "owned move source",
        )?;
        if before_source.len() != 1
            || !before_destination.is_empty()
            || current.is_folder()
            || current.docwsid != file.document_id
            || current.display_name() != file.name
            || current.etag.is_empty()
            || current.etag == file.etag
            || self.read_small_file_in_folder(&source.id, &file.id).await? != revised
        {
            bail!("owned move fixture changed before stale request");
        }
        let current_etag = current.etag.clone();
        let accepted = self
            .send_move(&file.id, &file.etag, &destination.id)
            .await?;
        let after_source = self.list_folder(&source.id).await?;
        let after_destination = self.list_folder(&destination.id).await?;
        if accepted {
            if !after_source.is_empty() || after_destination.len() != 1 {
                return Ok(MoveProbeOutcome::Indeterminate);
            }
            let moved = &after_destination[0];
            if moved.drivewsid == file.id
                && moved.docwsid == file.document_id
                && moved.display_name() == file.name
                && !moved.is_folder()
                && self
                    .read_small_file_in_folder(&destination.id, &file.id)
                    .await?
                    == revised
            {
                return Ok(MoveProbeOutcome::StaleAccepted);
            }
        } else if after_source.len() == 1 && after_destination.is_empty() {
            let intact = &after_source[0];
            if intact.drivewsid == file.id
                && intact.docwsid == file.document_id
                && intact.display_name() == file.name
                && intact.etag == current_etag
                && self.read_small_file_in_folder(&source.id, &file.id).await? == revised
            {
                return Ok(MoveProbeOutcome::StaleRejectedCurrentIntact);
            }
        }
        Ok(MoveProbeOutcome::Indeterminate)
    }

    /// Fresh-ETag control in a different new fixture. Return `false` only
    /// when both exact IDs and full bytes prove the rejected move did nothing.
    pub async fn probe_fresh_etag_move(
        &mut self,
        source: &ValidationFolder,
        destination: &ValidationFolder,
        file: &ValidationFile,
        content: &[u8],
    ) -> Result<bool> {
        if source.id == destination.id || content.is_empty() || content.len() > 4096 {
            bail!("invalid owned move control");
        }
        let root = self.list_root().await?;
        for folder in [source, destination] {
            if root
                .iter()
                .filter(|entry| {
                    entry.drivewsid == folder.id
                        && entry.display_name() == folder.name
                        && entry.is_folder()
                })
                .count()
                != 1
            {
                bail!("owned move control folder identity changed");
            }
        }
        let before_source = self.list_folder(&source.id).await?;
        let before_destination = self.list_folder(&destination.id).await?;
        if before_source.len() != 1
            || !before_destination.is_empty()
            || before_source[0].drivewsid != file.id
            || before_source[0].docwsid != file.document_id
            || before_source[0].display_name() != file.name
            || before_source[0].etag != file.etag
            || self.read_small_file_in_folder(&source.id, &file.id).await? != content
        {
            bail!("owned move control changed before request");
        }
        let accepted = self
            .send_move(&file.id, &file.etag, &destination.id)
            .await?;
        let after_source = self.list_folder(&source.id).await?;
        let after_destination = self.list_folder(&destination.id).await?;
        if accepted {
            if after_source.is_empty()
                && after_destination.len() == 1
                && after_destination[0].drivewsid == file.id
                && after_destination[0].docwsid == file.document_id
                && after_destination[0].display_name() == file.name
                && !after_destination[0].etag.is_empty()
                && self
                    .read_small_file_in_folder(&destination.id, &file.id)
                    .await?
                    == content
            {
                return Ok(true);
            }
        } else if after_source.len() == 1
            && after_destination.is_empty()
            && after_source[0].drivewsid == file.id
            && after_source[0].docwsid == file.document_id
            && after_source[0].display_name() == file.name
            && after_source[0].etag == file.etag
            && self.read_small_file_in_folder(&source.id, &file.id).await? == content
        {
            return Ok(false);
        }
        bail!("owned move control ended indeterminate")
    }

    /// Make the source ETag stale by an acknowledged metadata-only rename,
    /// then send exactly one move with the original ETag. This tests the
    /// namespace conflict that a content-only stale trial cannot establish.
    pub async fn probe_metadata_stale_move(
        &mut self,
        source: &ValidationFolder,
        destination: &ValidationFolder,
        file: &ValidationFile,
        content: &[u8],
    ) -> Result<MoveProbeOutcome> {
        const RENAMED: &str = "renamed-before-move.txt";
        if source.id == destination.id || content.is_empty() || content.len() > 4096 {
            bail!("invalid metadata move fixture");
        }
        let root = self.list_root().await?;
        for folder in [source, destination] {
            if root
                .iter()
                .filter(|entry| {
                    entry.drivewsid == folder.id
                        && entry.display_name() == folder.name
                        && entry.is_folder()
                })
                .count()
                != 1
            {
                bail!("metadata move folder identity changed");
            }
        }
        let before_source = self.list_folder(&source.id).await?;
        if before_source.len() != 1
            || !self.list_folder(&destination.id).await?.is_empty()
            || before_source[0].drivewsid != file.id
            || before_source[0].docwsid != file.document_id
            || before_source[0].display_name() != file.name
            || before_source[0].etag != file.etag
            || self.read_small_file_in_folder(&source.id, &file.id).await? != content
        {
            bail!("metadata move fixture changed before rename");
        }
        if !self.send_rename(&file.id, &file.etag, RENAMED).await? {
            bail!("metadata move setup rename was rejected");
        }
        let renamed = self.list_folder(&source.id).await?;
        if renamed.len() != 1
            || renamed[0].drivewsid != file.id
            || renamed[0].docwsid != file.document_id
            || renamed[0].display_name() != RENAMED
            || renamed[0].etag.is_empty()
            || renamed[0].etag == file.etag
            || !self.list_folder(&destination.id).await?.is_empty()
            || self.read_small_file_in_folder(&source.id, &file.id).await? != content
        {
            bail!("metadata move setup did not establish a newer ETag");
        }
        let current_etag = renamed[0].etag.clone();
        let accepted = self
            .send_move(&file.id, &file.etag, &destination.id)
            .await?;
        let after_source = self.list_folder(&source.id).await?;
        let after_destination = self.list_folder(&destination.id).await?;
        if accepted {
            if after_source.is_empty()
                && after_destination.len() == 1
                && after_destination[0].drivewsid == file.id
                && after_destination[0].docwsid == file.document_id
                && after_destination[0].display_name() == RENAMED
                && self
                    .read_small_file_in_folder(&destination.id, &file.id)
                    .await?
                    == content
            {
                return Ok(MoveProbeOutcome::StaleAccepted);
            }
        } else if after_source.len() == 1
            && after_destination.is_empty()
            && after_source[0].drivewsid == file.id
            && after_source[0].docwsid == file.document_id
            && after_source[0].display_name() == RENAMED
            && after_source[0].etag == current_etag
            && self.read_small_file_in_folder(&source.id, &file.id).await? == content
        {
            return Ok(MoveProbeOutcome::StaleRejectedCurrentIntact);
        }
        Ok(MoveProbeOutcome::Indeterminate)
    }

    /// First submit the original stale ETag for a rename, then the newly
    /// observed ETag if and only if the stale request left the fixture intact.
    pub async fn probe_conditional_rename(
        &mut self,
        folder: &ValidationFolder,
        file: &ValidationFile,
        current_bytes: &[u8],
    ) -> Result<RenameProbeOutcome> {
        const NEW_NAME: &str = "renamed-by-cirrove.txt";
        let before = self.list_folder(&folder.id).await?;
        let entry = exactly_one(
            before
                .iter()
                .filter(|entry| entry.drivewsid == file.id)
                .collect(),
            "rename base lookup",
        )?;
        if entry.docwsid != file.document_id
            || entry.display_name() != PROBE_FILE
            || entry.etag.is_empty()
            || entry.etag == file.etag
            || self.read_small_file_in_folder(&folder.id, &file.id).await? != current_bytes
        {
            bail!("iCloud rename base is not the expected newer fixture");
        }
        let current_etag = entry.etag.clone();
        let stale_accepted = self.send_rename(&file.id, &file.etag, NEW_NAME).await?;
        let after_stale = self.list_folder(&folder.id).await?;
        let listed = exactly_one(
            after_stale
                .iter()
                .filter(|entry| entry.drivewsid == file.id)
                .collect(),
            "stale rename observation",
        )?;
        if self.read_small_file_in_folder(&folder.id, &file.id).await? != current_bytes {
            return Ok(RenameProbeOutcome::Indeterminate);
        }
        if stale_accepted {
            return Ok(
                if listed.display_name() == NEW_NAME && listed.docwsid == file.document_id {
                    RenameProbeOutcome::StaleAccepted
                } else {
                    RenameProbeOutcome::Indeterminate
                },
            );
        }
        if listed.display_name() != PROBE_FILE
            || listed.docwsid != file.document_id
            || listed.etag != current_etag
        {
            return Ok(RenameProbeOutcome::Indeterminate);
        }
        let fresh_accepted = self.send_rename(&file.id, &current_etag, NEW_NAME).await?;
        let after_fresh = self.list_folder(&folder.id).await?;
        let listed = exactly_one(
            after_fresh
                .iter()
                .filter(|entry| entry.drivewsid == file.id)
                .collect(),
            "fresh rename observation",
        )?;
        if self.read_small_file_in_folder(&folder.id, &file.id).await? != current_bytes {
            return Ok(RenameProbeOutcome::Indeterminate);
        }
        if fresh_accepted && listed.display_name() == NEW_NAME && listed.docwsid == file.document_id
        {
            Ok(RenameProbeOutcome::StaleRejectedFreshAccepted)
        } else if !fresh_accepted
            && listed.display_name() == PROBE_FILE
            && listed.etag == current_etag
        {
            Ok(RenameProbeOutcome::StaleRejectedFreshRejected)
        } else {
            Ok(RenameProbeOutcome::Indeterminate)
        }
    }

    /// A metadata-only stale ETag test: rename once, then reuse the ETag from
    /// before that rename. This distinguishes metadata from content revisions.
    pub async fn probe_metadata_rename_revision(
        &mut self,
        folder: &ValidationFolder,
        file: &ValidationFile,
        content: &[u8],
    ) -> Result<RenameProbeOutcome> {
        const FIRST: &str = "renamed-once.txt";
        const SECOND: &str = "renamed-twice.txt";
        let before = self.list_folder(&folder.id).await?;
        let entry = exactly_one(
            before
                .iter()
                .filter(|entry| entry.drivewsid == file.id)
                .collect(),
            "metadata rename base lookup",
        )?;
        if entry.docwsid != file.document_id
            || entry.display_name() != PROBE_FILE
            || entry.etag != file.etag
            || self.read_small_file_in_folder(&folder.id, &file.id).await? != content
        {
            bail!("iCloud metadata rename base changed");
        }
        if !self.send_rename(&file.id, &file.etag, FIRST).await? {
            bail!("first metadata rename was rejected");
        }
        let first = self.list_folder(&folder.id).await?;
        let entry = exactly_one(
            first
                .iter()
                .filter(|entry| entry.drivewsid == file.id)
                .collect(),
            "first metadata rename observation",
        )?;
        if entry.display_name() != FIRST
            || entry.docwsid != file.document_id
            || self.read_small_file_in_folder(&folder.id, &file.id).await? != content
        {
            return Ok(RenameProbeOutcome::Indeterminate);
        }
        if entry.etag.is_empty() || entry.etag == file.etag {
            bail!("first metadata rename did not expose a new ETag");
        }
        let current_etag = entry.etag.clone();
        let stale_accepted = self.send_rename(&file.id, &file.etag, SECOND).await?;
        let after_stale = self.list_folder(&folder.id).await?;
        let entry = exactly_one(
            after_stale
                .iter()
                .filter(|entry| entry.drivewsid == file.id)
                .collect(),
            "stale metadata rename observation",
        )?;
        if self.read_small_file_in_folder(&folder.id, &file.id).await? != content {
            return Ok(RenameProbeOutcome::Indeterminate);
        }
        if stale_accepted {
            return Ok(
                if entry.display_name() == SECOND && entry.docwsid == file.document_id {
                    RenameProbeOutcome::StaleAccepted
                } else {
                    RenameProbeOutcome::Indeterminate
                },
            );
        }
        if entry.display_name() != FIRST || entry.etag != current_etag {
            return Ok(RenameProbeOutcome::Indeterminate);
        }
        let fresh_accepted = self.send_rename(&file.id, &current_etag, SECOND).await?;
        let after_fresh = self.list_folder(&folder.id).await?;
        let entry = exactly_one(
            after_fresh
                .iter()
                .filter(|entry| entry.drivewsid == file.id)
                .collect(),
            "fresh metadata rename observation",
        )?;
        if self.read_small_file_in_folder(&folder.id, &file.id).await? != content {
            return Ok(RenameProbeOutcome::Indeterminate);
        }
        if fresh_accepted && entry.display_name() == SECOND && entry.docwsid == file.document_id {
            Ok(RenameProbeOutcome::StaleRejectedFreshAccepted)
        } else if !fresh_accepted && entry.display_name() == FIRST && entry.etag == current_etag {
            Ok(RenameProbeOutcome::StaleRejectedFreshRejected)
        } else {
            Ok(RenameProbeOutcome::Indeterminate)
        }
    }

    /// Test the exact occupied-name handoff that a staged replacement would
    /// need. Both items were created in this process and are independently
    /// verified before the single rename request.
    pub async fn probe_occupied_name_rename(
        &mut self,
        folder: &ValidationFolder,
        original: &ValidationFile,
        staged: &ValidationFile,
        original_bytes: &[u8],
        staged_bytes: &[u8],
    ) -> Result<OccupiedNameOutcome> {
        if original.id == staged.id
            || original.name != PROBE_FILE
            || !staged.name.starts_with("staged-by-cirrove-")
            || original_bytes == staged_bytes
        {
            bail!("invalid staged validation pair");
        }
        let before = self.list_folder(&folder.id).await?;
        if before.len() != 2
            || !before.iter().any(|entry| {
                entry.drivewsid == original.id && entry.display_name() == original.name
            })
            || !before
                .iter()
                .any(|entry| entry.drivewsid == staged.id && entry.display_name() == staged.name)
            || self
                .read_small_file_in_folder(&folder.id, &original.id)
                .await?
                != original_bytes
            || self
                .read_small_file_in_folder(&folder.id, &staged.id)
                .await?
                != staged_bytes
        {
            bail!("iCloud staged validation pair changed before the rename");
        }
        let accepted = self
            .send_rename(&staged.id, &staged.etag, PROBE_FILE)
            .await?;
        let after = self.list_folder(&folder.id).await?;
        let old = after.iter().find(|entry| entry.drivewsid == original.id);
        let new = after.iter().find(|entry| entry.drivewsid == staged.id);
        let Some(old) = old else {
            return Ok(OccupiedNameOutcome::OriginalNoLongerListed);
        };
        let Some(new) = new else {
            return Ok(OccupiedNameOutcome::Indeterminate);
        };
        if old.docwsid != original.document_id
            || new.docwsid != staged.document_id
            || self
                .read_small_file_in_folder(&folder.id, &original.id)
                .await?
                != original_bytes
            || self
                .read_small_file_in_folder(&folder.id, &staged.id)
                .await?
                != staged_bytes
        {
            return Ok(OccupiedNameOutcome::Indeterminate);
        }
        if !accepted && old.display_name() == original.name && new.display_name() == staged.name {
            Ok(OccupiedNameOutcome::RejectedBothIntact)
        } else if accepted && old.display_name() == PROBE_FILE && new.display_name() == PROBE_FILE {
            Ok(OccupiedNameOutcome::AcceptedDuplicateName)
        } else if old.display_name() == PROBE_FILE
            && new.display_name().starts_with("created-by-cirrove ")
            && new.display_name().ends_with(".txt")
        {
            Ok(OccupiedNameOutcome::ConflictRenamedBothIntact)
        } else {
            Ok(OccupiedNameOutcome::Indeterminate)
        }
    }

    /// Stage -> recovery -> target, but only for two new fixtures. No remote
    /// item is deleted. This tests protocol observability, not atomicity.
    pub async fn probe_staged_handoff(
        &mut self,
        folder: &ValidationFolder,
        original: &ValidationFile,
        staged: &ValidationFile,
        original_bytes: &[u8],
        staged_bytes: &[u8],
    ) -> Result<HandoffOutcome> {
        if original.id == staged.id
            || original.name != PROBE_FILE
            || !staged.name.starts_with("staged-by-cirrove-")
            || original_bytes == staged_bytes
        {
            bail!("invalid staged handoff pair");
        }
        let before = self.list_folder(&folder.id).await?;
        if before.len() != 2
            || !before.iter().any(|entry| {
                entry.drivewsid == original.id && entry.display_name() == original.name
            })
            || !before
                .iter()
                .any(|entry| entry.drivewsid == staged.id && entry.display_name() == staged.name)
            || self
                .read_small_file_in_folder(&folder.id, &original.id)
                .await?
                != original_bytes
            || self
                .read_small_file_in_folder(&folder.id, &staged.id)
                .await?
                != staged_bytes
        {
            bail!("iCloud staged handoff pair changed before the first rename");
        }
        let recovery_name = format!("recovery-by-cirrove-{}.txt", Uuid::new_v4());
        let first_accepted = self
            .send_rename(&original.id, &original.etag, &recovery_name)
            .await?;
        let after_first = self.list_folder(&folder.id).await?;
        let old = after_first
            .iter()
            .find(|entry| entry.drivewsid == original.id);
        let new = after_first
            .iter()
            .find(|entry| entry.drivewsid == staged.id);
        let (Some(old), Some(new)) = (old, new) else {
            return Ok(HandoffOutcome::Indeterminate);
        };
        if !first_accepted
            || old.display_name() != recovery_name
            || new.display_name() != staged.name
            || old.docwsid != original.document_id
            || new.docwsid != staged.document_id
            || self
                .read_small_file_in_folder(&folder.id, &original.id)
                .await?
                != original_bytes
            || self
                .read_small_file_in_folder(&folder.id, &staged.id)
                .await?
                != staged_bytes
        {
            return Ok(HandoffOutcome::Indeterminate);
        }
        let second_accepted = self
            .send_rename(&staged.id, &staged.etag, PROBE_FILE)
            .await?;
        let after_second = self.list_folder(&folder.id).await?;
        let old = after_second
            .iter()
            .find(|entry| entry.drivewsid == original.id);
        let new = after_second
            .iter()
            .find(|entry| entry.drivewsid == staged.id);
        let (Some(old), Some(new)) = (old, new) else {
            return Ok(HandoffOutcome::Indeterminate);
        };
        if old.display_name() != recovery_name
            || old.docwsid != original.document_id
            || new.docwsid != staged.document_id
            || self
                .read_small_file_in_folder(&folder.id, &original.id)
                .await?
                != original_bytes
            || self
                .read_small_file_in_folder(&folder.id, &staged.id)
                .await?
                != staged_bytes
        {
            return Ok(HandoffOutcome::Indeterminate);
        }
        if second_accepted && new.display_name() == PROBE_FILE {
            Ok(HandoffOutcome::BothPreservedAtTargetAndRecovery)
        } else if !second_accepted && new.display_name() == staged.name {
            Ok(HandoffOutcome::RecoveryMovedStagingUnchanged)
        } else {
            Ok(HandoffOutcome::Indeterminate)
        }
    }

    pub async fn prepare_durable_handoff(
        &mut self,
        folder: &ValidationFolder,
        original: &ValidationFile,
        staged: &ValidationFile,
        original_bytes: &[u8],
        staged_bytes: &[u8],
    ) -> Result<HandoffPlan> {
        self.prepare_durable_handoff_named(
            folder,
            original,
            staged,
            original_bytes,
            staged_bytes,
            format!("recovery-by-cirrove-{}.txt", Uuid::new_v4()),
        )
        .await
    }

    /// The shared transfer worker reserves this exact name in its journal
    /// before the provider may submit either rename.
    pub async fn prepare_durable_handoff_named(
        &mut self,
        folder: &ValidationFolder,
        original: &ValidationFile,
        staged: &ValidationFile,
        original_bytes: &[u8],
        staged_bytes: &[u8],
        recovery_name: String,
    ) -> Result<HandoffPlan> {
        if !staged.name.starts_with("staged-by-cirrove-")
            || original.id == staged.id
            || original_bytes.is_empty()
            || staged_bytes.is_empty()
            || original_bytes.len() > 4096
            || staged_bytes.len() > 4096
        {
            bail!("invalid durable iCloud validation pair");
        }
        let plan = HandoffPlan {
            version: 2,
            folder_parent_id: ROOT_ID.into(),
            folder_id: folder.id.clone(),
            folder_name: folder.name.clone(),
            original_id: original.id.clone(),
            original_doc_id: original.document_id.clone(),
            original_etag: original.etag.clone(),
            staged_id: staged.id.clone(),
            staged_doc_id: staged.document_id.clone(),
            staged_etag: staged.etag.clone(),
            staged_name: staged.name.clone(),
            recovery_name,
            target_name: original.name.clone(),
            original_sha256: hex::encode(Sha256::digest(original_bytes)),
            staged_sha256: hex::encode(Sha256::digest(staged_bytes)),
            package: None,
        };
        if self.inspect_durable_handoff(&plan).await? != HandoffObserved::Prepared {
            bail!("durable iCloud validation pair changed during preparation");
        }
        Ok(plan)
    }
}

fn valid_etag(etag: &str) -> bool {
    !etag.is_empty() && etag.len() <= 4096 && !etag.contains(['\0', '\r', '\n'])
}

impl StagedRegistrationPlan {
    pub fn validate(&self) -> Result<()> {
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
        let bound = match (&self.staged_id, &self.staged_doc_id, &self.staged_etag) {
            (None, None, None) => true,
            (Some(id), Some(document), Some(etag)) => {
                let candidate = split_file_id(id)?;
                candidate.0 == "com.apple.CloudDocs"
                    && candidate.1 == document
                    && id != &self.original_id
                    && valid_etag(etag)
            }
            _ => false,
        };
        if self.version != 2
            || !uuid_name(&self.folder_name, PROBE_PREFIX, "")
            || !self.folder_id.starts_with("FOLDER::com.apple.CloudDocs::")
            || !uuid_name(&self.staged_name, "staged-by-cirrove-", ".txt")
            || old.0 != "com.apple.CloudDocs"
            || old.1 != self.original_doc_id
            || !digest(&self.original_sha256)
            || !digest(&self.staged_sha256)
            || !valid_etag(&self.original_etag)
            || !bound
        {
            bail!("invalid staged iCloud registration plan");
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod handoff_tests {
    use super::*;

    #[test]
    fn staged_registration_reservation_requires_a_matching_bound_identity() {
        let mut plan = StagedRegistrationPlan {
            version: 2,
            folder_id: "FOLDER::com.apple.CloudDocs::folder-1".into(),
            folder_name: format!("{PROBE_PREFIX}{}", Uuid::new_v4()),
            original_id: "FILE::com.apple.CloudDocs::old-1".into(),
            original_doc_id: "old-1".into(),
            original_etag: "old-revision".into(),
            original_sha256: "a".repeat(64),
            staged_name: format!("staged-by-cirrove-{}.txt", Uuid::new_v4()),
            staged_sha256: "b".repeat(64),
            staged_id: None,
            staged_doc_id: None,
            staged_etag: None,
        };
        plan.validate().unwrap();
        plan.staged_id = Some("FILE::com.apple.CloudDocs::new-1".into());
        assert!(plan.validate().is_err());
        plan.staged_doc_id = Some("new-1".into());
        assert!(plan.validate().is_err());
        plan.staged_etag = Some("staged-revision".into());
        plan.validate().unwrap();
        plan.staged_doc_id = Some("unrelated".into());
        assert!(plan.validate().is_err());
        plan.staged_id = Some("FILE::other.zone::unrelated".into());
        assert!(plan.validate().is_err());
    }
}

mod competing;
