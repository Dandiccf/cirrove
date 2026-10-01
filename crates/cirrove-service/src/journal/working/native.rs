//! Journal-local native working bytes. No mounted projection or write admission.
use super::*;
use cirrove_core::{CancellationToken, reads::NativeArchiveBinding};
use std::os::fd::AsRawFd;

pub(crate) mod atomic;
pub(crate) mod backup;
mod edit;
pub(crate) mod projection;
pub(crate) mod retirement;
pub(crate) mod successors;
mod temporary;

const MAX_ARCHIVE: u64 = 64 * 1024 * 1024;
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Binding {
    scope: Scope,
    archive: Node,
    source: Node,
    semantic: PackageSemanticIdentity,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArchiveRevision {
    source_etag: String,
    source_size: u64,
    source_parent: String,
    sha256: String,
}
impl Binding {
    fn checked(value: NativeArchiveBinding) -> Result<Self> {
        let b = Self {
            scope: value.scope,
            archive: value.archive,
            source: value.source,
            semantic: value.semantic,
        };
        b.validate()?;
        Ok(b)
    }
    fn validate(&self) -> Result<ArchiveRevision> {
        let a = &self.archive;
        let s = &self.source;
        let intent = UploadIntent::Replace {
            item: s.id.clone(),
            expected_etag: s.etag.clone().ok_or(JournalError::Intent)?,
        };
        let representation = UploadRepresentation::PackageReplacementArchive {
            expected_root: a.name.clone(),
            semantic: self.semantic.clone(),
            original: Box::new(s.clone()),
            original_semantic: self.semantic.clone(),
        };
        package_replacement::validate(&self.scope, &intent, &representation)?;
        if a.id != format!("icloud-artifact:{}", s.id)
            || a.parent_id.as_ref() != Some(&s.id)
            || a.kind != NodeKind::File
            || a.package
            || a.target.is_some()
            || a.etag.is_some()
            || a.name != s.name
            || a.size == 0
            || a.size > MAX_ARCHIVE
        {
            return Err(JournalError::Intent);
        }
        let revision = a
            .content_version
            .as_deref()
            .filter(|v| v.len() <= 8192)
            .and_then(|v| v.strip_prefix("icloud-artifact-v2:"))
            .ok_or(JournalError::Intent)?;
        let r: ArchiveRevision =
            serde_json::from_str(revision).map_err(|_| JournalError::Intent)?;
        if Some(&r.source_etag) != s.etag.as_ref()
            || Some(&r.source_parent) != s.parent_id.as_ref()
            || r.source_size != s.size
            || r.sha256.len() != 64
            || !r
                .sha256
                .bytes()
                .all(|v| v.is_ascii_hexdigit() && !v.is_ascii_uppercase())
        {
            return Err(JournalError::Intent);
        }
        Ok(r)
    }
}
pub(crate) fn migrate(db: &mut Connection) -> Result<()> {
    let tx = db.transaction()?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS native_working_bindings(working TEXT PRIMARY KEY, source TEXT NOT NULL UNIQUE, body TEXT NOT NULL);")?;
    successors::migrate(&tx)?;
    atomic::migrate(&tx)?;
    backup::migrate(&tx)?;
    retirement::migrate(&tx)?;
    tx.pragma_update(None, "user_version", JOURNAL_SCHEMA)?;
    tx.commit()?;
    Ok(())
}
/// Reserved hydration; callers fill only exact selected immutable archive bytes.
pub struct NativeWorkingHydration {
    binding: Binding,
    source: WorkingSource,
    retired: Option<retirement::Reactivation>,
}
/// Validated hydration, produced outside the journal mutex.
pub struct ValidatedNativeWorking {
    binding: Binding,
    source: WorkingSource,
    retired: Option<retirement::Reactivation>,
}
impl NativeWorkingHydration {
    pub fn write_chunk(&mut self, bytes: &[u8]) -> Result<()> {
        self.source.write_chunk(bytes)
    }
    pub fn validate(self, cancel: &CancellationToken) -> Result<ValidatedNativeWorking> {
        let r = self.binding.validate()?;
        if self.source.written != self.source.size || cancel.is_cancelled() {
            return Err(JournalError::Stale);
        }
        self.source.temporary.as_file().sync_all()?;
        let receipt = cirrove_icloud::PackageDownload {
            size: self.source.size,
            sha256: r.sha256,
        };
        let actual = cirrove_icloud::package_archive_semantic_identity_versioned(
            self.source.temporary.as_file(),
            &receipt,
            &self.binding.archive.name,
            self.binding.semantic.version,
            cancel,
        )
        .map_err(|_| JournalError::Corrupt)?;
        if actual != self.binding.semantic || cancel.is_cancelled() {
            return Err(JournalError::Stale);
        }
        Ok(ValidatedNativeWorking {
            binding: self.binding,
            source: self.source,
            retired: self.retired,
        })
    }
}
/// Pins source descriptor and reserved staging while copying outside the mutex.
pub struct NativeWorkingCapture {
    binding: Binding,
    record: WorkingFile,
    file: File,
    staging: WorkingSource,
}
pub struct CapturedNativeWorking {
    binding: Binding,
    record: WorkingFile,
    file: File,
    semantic: PackageSemanticIdentity,
    sha256: String,
    size: u64,
    _reservation: tempfile::TempPath,
    _owner: Arc<JournalOwner>,
}
/// An immutable, validated capture with its existing quota reservation. Only the
/// native capture path can construct this; adoption never rereads archive bytes.
pub(in crate::journal) struct PreparedNativeObject {
    file: File,
    path: tempfile::TempPath,
    owner: Arc<JournalOwner>,
    size: u64,
    sha256: String,
}
impl PreparedNativeObject {
    pub(in crate::journal) fn adopt(
        self,
        journal: &UploadJournal,
        expected: &representation::BoundReceipt,
    ) -> Result<tempfile::NamedTempFile> {
        if !Arc::ptr_eq(&self.owner, &journal._owner)
            || self.size != expected.size
            || self.sha256 != expected.sha256
            || expected.cancel.is_cancelled()
            || self.path.parent() != Some(journal.working.as_path())
        {
            return Err(JournalError::Stale);
        }
        let descriptor = self.file.metadata()?;
        let path = std::fs::symlink_metadata(&self.path)?;
        if !descriptor.is_file()
            || !path.is_file()
            || descriptor.dev() != path.dev()
            || descriptor.ino() != path.ino()
            || descriptor.nlink() != 1
            || descriptor.len() != self.size
            || descriptor.mode() & 0o777 != 0o400
        {
            return Err(JournalError::Corrupt);
        }
        Ok(tempfile::NamedTempFile::from_parts(self.file, self.path))
    }
}
impl NativeWorkingCapture {
    pub fn capture(self, cancel: &CancellationToken) -> Result<CapturedNativeWorking> {
        self.capture_observed(cancel, |_| {})
    }
    fn capture_observed(
        mut self,
        cancel: &CancellationToken,
        mut copied: impl FnMut(u64),
    ) -> Result<CapturedNativeWorking> {
        let mut offset = 0;
        let mut hash = Sha256::new();
        let mut buffer = [0; 128 * 1024];
        while offset < self.staging.size {
            if cancel.is_cancelled() {
                return Err(JournalError::Stale);
            }
            let limit = (self.staging.size - offset).min(buffer.len() as u64) as usize;
            let count = self.file.read_at(&mut buffer[..limit], offset)?;
            if count == 0 {
                return Err(JournalError::Stale);
            }
            self.staging.write_chunk(&buffer[..count])?;
            hash.update(&buffer[..count]);
            offset += count as u64;
            copied(offset);
        }
        if self.file.metadata()?.len() != offset || cancel.is_cancelled() {
            return Err(JournalError::Stale);
        }
        self.staging
            .temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o400))?;
        self.staging.temporary.as_file().sync_all()?;
        let file = File::open(format!(
            "/proc/self/fd/{}",
            self.staging.temporary.as_file().as_raw_fd()
        ))?;
        let reservation = self.staging.temporary.into_temp_path(); // closes the writable handle
        let sha256 = hex::encode(hash.finalize());
        let receipt = cirrove_icloud::PackageDownload {
            size: offset,
            sha256: sha256.clone(),
        };
        let semantic = cirrove_icloud::package_archive_semantic_identity_versioned(
            &file,
            &receipt,
            &self.binding.archive.name,
            self.binding.semantic.version,
            cancel,
        )
        .map_err(|_| JournalError::Corrupt)?;
        if cancel.is_cancelled() {
            return Err(JournalError::Stale);
        }
        Ok(CapturedNativeWorking {
            binding: self.binding,
            record: self.record,
            file,
            semantic,
            sha256,
            size: offset,
            _reservation: reservation,
            _owner: self.staging._owner,
        })
    }
}
pub(crate) struct NativeCommit {
    transfer: Option<atomic::Transfer>,
    record: WorkingFile,
    binding: Binding,
    intent: UploadIntent,
    representation: UploadRepresentation,
}
impl NativeCommit {
    pub(crate) fn working_id(&self) -> Uuid {
        self.record.id
    }
}
impl UploadJournal {
    pub fn reserve_native_working(
        &mut self,
        value: NativeArchiveBinding,
    ) -> Result<NativeWorkingHydration> {
        let binding = Binding::checked(value)?;
        if binding.scope.account != self.account {
            return Err(JournalError::Account);
        }
        let retired = retirement::select_slot(self, &binding)?;
        let source = self.reserve_working(binding.archive.size)?;
        Ok(NativeWorkingHydration {
            binding,
            source,
            retired,
        })
    }
    pub fn publish_native_working(
        &mut self,
        validated: ValidatedNativeWorking,
    ) -> Result<WorkingFile> {
        let ValidatedNativeWorking {
            binding,
            source,
            retired,
        } = validated;
        binding.validate()?;
        if binding.scope.account != self.account {
            return Err(JournalError::Account);
        }
        source.complete_descriptor(&self.working)?;
        retirement::recheck_slot(self, &binding, retired.as_ref())?;
        let id = retired
            .as_ref()
            .map_or_else(Uuid::new_v4, |selected| selected.slot.working);
        let mut node = binding.archive.clone();
        node.id = format!("local-native-archive-{id}");
        node.content_version = Some(format!("working-{id}"));
        let mut record = WorkingFile {
            id,
            scope: binding.scope.clone(),
            node,
            intent: UploadIntent::Replace {
                item: binding.source.id.clone(),
                expected_etag: binding.source.etag.clone().ok_or(JournalError::Intent)?,
            },
            latest: None,
            dirty: false,
            generation: retired.as_ref().map_or(Ok(0), |slot| {
                slot.slot
                    .generation
                    .checked_add(1)
                    .ok_or(JournalError::Quota)
            })?,
            initial_remote: None,
            unlinked: false,
            native: true,
        };
        let source_identity = serde_json::to_string(&(&binding.scope, &binding.source.id))?;
        let exists: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM native_working_bindings WHERE source=?1)",
            [&source_identity],
            |r| r.get(0),
        )?;
        if exists {
            return Err(JournalError::Stale);
        }
        source
            .temporary
            .persist_noclobber(self.working.join(id.to_string()))
            .map_err(|_| JournalError::Storage)?;
        File::open(&self.working)?.sync_all()?;
        let tx = self.db.transaction()?;
        if let Some(selected) = &retired {
            retirement::prepare_reactivation(&tx, &binding, selected)?;
        }
        // Hydration publishes one source owner and its derived byte stream
        // together; the child never acquires an independent cloud operation.
        tx.execute(
            "INSERT INTO working_files(id,identity,slot,body) VALUES(?1,?2,?3,?4)",
            params![
                id.to_string(),
                serde_json::to_string(&(&record.scope, &record.node.id))?,
                format!("native-working-{id}"),
                serde_json::to_string(&record)?
            ],
        )?;
        tx.execute(
            "INSERT INTO native_working_bindings VALUES(?1,?2,?3)",
            params![
                id.to_string(),
                source_identity,
                serde_json::to_string(&binding)?
            ],
        )?;
        projection::attach(&tx, &mut record, &binding)?;
        if let Some(slot) = retired
            && tx.execute(
                "DELETE FROM native_retired_slots WHERE working=?1 AND body=?2",
                params![
                    slot.slot.working.to_string(),
                    serde_json::to_string(&slot.slot)?
                ],
            )? != 1
        {
            return Err(JournalError::Stale);
        }
        tx.commit()?;
        Ok(record)
    }
    fn native_binding(&self, id: Uuid) -> Result<Binding> {
        let body: Option<String> = self
            .db
            .query_row(
                "SELECT body FROM native_working_bindings WHERE working=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        let b: Binding = serde_json::from_str(&body.ok_or(JournalError::Corrupt)?)?;
        b.validate()?;
        Ok(b)
    }
    pub fn capture_native_working(&mut self, id: Uuid) -> Result<NativeWorkingCapture> {
        let record = self.working_file(id)?;
        if !record.native || !record.dirty || record.unlinked {
            return Err(JournalError::Stale);
        }
        if backup::gap(&self.db, id)?.is_some() {
            return Err(JournalError::Stale);
        }
        let binding = self.native_binding(id)?;
        if binding.scope != record.scope {
            return Err(JournalError::Corrupt);
        }
        let file = self.working_descriptor(id, false)?;
        let size = file.metadata()?.len();
        if size == 0 || size > MAX_ARCHIVE {
            return Err(JournalError::Intent);
        }
        let staging = self.reserve_working_named(size, ".native-capture-")?;
        Ok(NativeWorkingCapture {
            binding,
            record,
            file,
            staging,
        })
    }
    pub fn seal_captured_native_working(
        &mut self,
        captured: CapturedNativeWorking,
        cancel: &CancellationToken,
    ) -> Result<UploadRecord> {
        let current = self.working_file(captured.record.id)?;
        validate_current(&current, &captured.record)?;
        if self.native_binding(current.id)? != captured.binding || cancel.is_cancelled() {
            return Err(JournalError::Stale);
        }
        let (original, original_semantic) =
            successors::provisional(&self.db, &current, &captured.binding)?;
        let intent = UploadIntent::Replace {
            item: original.id.clone(),
            expected_etag: original.etag.clone().ok_or(JournalError::Corrupt)?,
        };
        let representation = UploadRepresentation::PackageReplacementArchive {
            expected_root: captured.binding.archive.name.clone(),
            semantic: captured.semantic,
            original: Box::new(original),
            original_semantic,
        };
        let order = WriteOrder {
            base: current.latest.map(|predecessor| WriteBase {
                predecessor,
                resolved: false,
            }),
            prerequisites: Vec::new(),
        };
        self.enqueue_admitted_source(
            current.scope.clone(),
            intent.clone(),
            order,
            Some(GenerationCommit::Native(Box::new(NativeCommit {
                transfer: None,
                record: current,
                binding: captured.binding,
                intent,
                representation: representation.clone(),
            }))),
            representation::Admission {
                representation,
                receipt: Some(representation::BoundReceipt {
                    size: captured.size,
                    sha256: captured.sha256.clone(),
                    cancel: cancel.clone(),
                }),
            },
            std::io::empty(),
            Some(PreparedNativeObject {
                file: captured.file,
                path: captured._reservation,
                owner: captured._owner,
                size: captured.size,
                sha256: captured.sha256,
            }),
        )
    }
}
fn validate_current(current: &WorkingFile, selected: &WorkingFile) -> Result<()> {
    if !current.native
        || !current.dirty
        || current.unlinked
        || current.latest != selected.latest
        || current.id != selected.id
        || current.scope != selected.scope
        || current.generation != selected.generation
        || current.intent != selected.intent
        || current.node != selected.node
    {
        return Err(JournalError::Stale);
    }
    Ok(())
}
pub(crate) fn commit(
    tx: &rusqlite::Transaction<'_>,
    commit: &NativeCommit,
    upload: &UploadRecord,
) -> Result<()> {
    let body: String = tx.query_row(
        "SELECT body FROM working_files WHERE id=?1",
        [commit.record.id.to_string()],
        |r| r.get(0),
    )?;
    let mut current: WorkingFile = serde_json::from_str(&body)?;
    validate_current(&current, &commit.record)?;
    let body: String = tx.query_row(
        "SELECT body FROM native_working_bindings WHERE working=?1",
        [current.id.to_string()],
        |r| r.get(0),
    )?;
    let binding: Binding = serde_json::from_str(&body)?;
    if binding != commit.binding
        || upload.scope != binding.scope
        || upload.intent != commit.intent
        || upload.representation != commit.representation
    {
        return Err(JournalError::Stale);
    }
    successors::record(tx, &current, upload, &binding)?;
    current.latest = Some(upload.id);
    current.dirty = false;
    current.node.size = upload.size;
    tx.execute(
        "UPDATE working_files SET body=?2 WHERE id=?1",
        params![current.id.to_string(), serde_json::to_string(&current)?],
    )?;
    namespace::update_working(tx, &current)?;
    Ok(())
}
#[cfg(test)]
mod tests;

#[cfg(test)]
mod prepared_tests;
