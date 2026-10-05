//! Local recovery without replay: pinned immutable saves and offline working bytes.
use super::*;
#[cfg(feature = "icloud-write-probe")]
mod account_snapshot;
#[cfg(feature = "icloud-write-probe")]
pub use account_snapshot::owned_account_snapshot;
mod active;
mod working;
pub use active::{PreparedWorkingExport, VerifiedWorkingExport, WorkingExportSource};
use cirrove_core::CancellationToken;
use std::os::fd::AsRawFd;
pub use working::{WorkingExportReceipt, WorkingRecovery};

/// Exact saved generation selected while the journal owner holds its lock.
/// The open descriptor survives collection; no payload path is reopened later.
pub struct LocalExportSource {
    file: File,
    journal_root: PathBuf,
    pub(crate) operation: Uuid,
    pub(crate) size: u64,
    pub(crate) sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalExportReceipt {
    pub operation: Uuid,
    pub size: u64,
    pub sha256: String,
    pub destination: PathBuf,
}
impl UploadJournal {
    pub fn local_export_source(&self, id: Uuid) -> Result<LocalExportSource> {
        let record = self.get(id)?;
        if !(matches!(
            record.state,
            UploadState::Pending
                | UploadState::Uploading
                | UploadState::VerifyRequired
                | UploadState::Verifying
                | UploadState::Conflict
                | UploadState::Failed
        ) || super::native_abandon::exportable(&self.db, &record)?)
            || record.scope.account != self.account
            || record.sha256.len() != 64
            || !record.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(JournalError::Stale);
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.objects.join(id.to_string()))
            .map_err(|_| JournalError::Corrupt)?;
        owned_private(&file)?;
        if file.metadata()?.len() != record.size {
            return Err(JournalError::Corrupt);
        }
        Ok(LocalExportSource {
            file,
            journal_root: self
                .objects
                .parent()
                .ok_or(JournalError::Storage)?
                .canonicalize()?,
            operation: id,
            size: record.size,
            sha256: record.sha256,
        })
    }
}
fn directory(path: &Path) -> Result<File> {
    use std::path::Component;
    if !path.is_absolute() {
        return Err(JournalError::Intent);
    }
    let mut dir = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open("/")?;
    for component in path.components() {
        match component {
            Component::RootDir => (),
            Component::Normal(name) => {
                let fd = rustix::fs::openat(
                    &dir,
                    name,
                    rustix::fs::OFlags::RDONLY
                        | rustix::fs::OFlags::DIRECTORY
                        | rustix::fs::OFlags::NOFOLLOW
                        | rustix::fs::OFlags::CLOEXEC,
                    rustix::fs::Mode::empty(),
                )
                .map_err(std::io::Error::from)?;
                dir = File::from(fd);
            }
            _ => return Err(JournalError::Intent),
        }
    }
    let stats = rustix::fs::fstatfs(&dir).map_err(std::io::Error::from)?;
    // A recovery export must not create another cloud write through FUSE.
    if stats.f_type == libc::FUSE_SUPER_MAGIC {
        return Err(JournalError::Intent);
    }
    Ok(dir)
}
impl LocalExportSource {
    /// Blocking, bounded-memory copy. Progress is bytes verified/copied, not a
    /// promise of publication. Cancellation never changes the source operation.
    pub fn copy_to(
        self,
        destination: &Path,
        cancel: &CancellationToken,
        progress: impl FnMut(u64),
    ) -> Result<LocalExportReceipt> {
        let (size, sha256) = copy_local_file(
            self.file,
            &self.journal_root,
            self.size,
            Some(&self.sha256),
            destination,
            cancel,
            progress,
        )?;

        Ok(LocalExportReceipt {
            operation: self.operation,
            size,
            sha256,
            destination: destination.into(),
        })
    }
}

fn prepare_local_copy(
    mut file: File,
    journal_root: &Path,
    size: u64,
    expected_hash: Option<&str>,
    destination: &Path,
    cancel: &CancellationToken,
    mut progress: impl FnMut(u64),
) -> Result<PreparedLocalCopy> {
    let stamp = |file: &File| -> std::io::Result<_> {
        let meta = file.metadata()?;
        Ok((
            meta.len(),
            meta.mtime(),
            meta.mtime_nsec(),
            meta.ctime(),
            meta.ctime_nsec(),
        ))
    };
    let before = expected_hash.is_none().then(|| stamp(&file)).transpose()?;
    if cancel.is_cancelled() {
        return Err(JournalError::Stale);
    }
    let parent = destination.parent().ok_or(JournalError::Intent)?;
    let name = destination.file_name().ok_or(JournalError::Intent)?;
    let dir = directory(parent)?;
    let anchored = PathBuf::from(format!("/proc/self/fd/{}", dir.as_raw_fd()));
    let resolved = std::fs::read_link(&anchored)?;
    if resolved.starts_with(journal_root) {
        return Err(JournalError::Intent);
    }
    let target = anchored.join(name);
    // Refuse even a dangling symlink. persist_noclobber below also closes
    // the race where a destination appears during the copy.
    match std::fs::symlink_metadata(&target) {
        Ok(_) => return Err(JournalError::Intent),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(e.into()),
    }
    let mut temporary = tempfile::Builder::new()
        .prefix(".cirrove-export-")
        .tempfile_in(&anchored)?;
    let mut hash = Sha256::new();
    let mut copied = 0u64;
    let mut buffer = [0u8; 128 * 1024];
    loop {
        if cancel.is_cancelled() {
            return Err(JournalError::Stale);
        }
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        copied = copied
            .checked_add(count as u64)
            .ok_or(JournalError::Corrupt)?;
        if copied > size {
            return Err(JournalError::Corrupt);
        }
        hash.update(&buffer[..count]);
        temporary.write_all(&buffer[..count])?;
        progress(copied);
    }
    let sha256 = hex::encode(hash.finalize());
    if copied != size || expected_hash.is_some_and(|expected| expected != sha256) {
        return Err(JournalError::Corrupt);
    }
    temporary.as_file().sync_all()?;
    if let Some(before) = before
        && stamp(&file)? != before
    {
        return Err(JournalError::Stale);
    }
    Ok(PreparedLocalCopy {
        temporary,
        directory: dir,
        resolved_parent: resolved,
        destination: destination.into(),
        size: copied,
        sha256,
    })
}

struct PreparedLocalCopy {
    temporary: tempfile::NamedTempFile,
    directory: File,
    resolved_parent: PathBuf,
    destination: PathBuf,
    size: u64,
    sha256: String,
}
impl PreparedLocalCopy {
    fn publish(self, cancel: &CancellationToken) -> Result<(u64, String)> {
        if cancel.is_cancelled() {
            return Err(JournalError::Stale);
        }
        let anchored = PathBuf::from(format!("/proc/self/fd/{}", self.directory.as_raw_fd()));
        if std::fs::read_link(&anchored)? != self.resolved_parent {
            return Err(JournalError::Stale);
        }
        let name = self.destination.file_name().ok_or(JournalError::Intent)?;
        self.temporary
            .persist_noclobber(anchored.join(name))
            .map_err(|error| JournalError::from(error.error))?;
        self.directory.sync_all()?;
        Ok((self.size, self.sha256))
    }
}
fn copy_local_file(
    file: File,
    journal_root: &Path,
    size: u64,
    expected_hash: Option<&str>,
    destination: &Path,
    cancel: &CancellationToken,
    progress: impl FnMut(u64),
) -> Result<(u64, String)> {
    prepare_local_copy(
        file,
        journal_root,
        size,
        expected_hash,
        destination,
        cancel,
        progress,
    )?
    .publish(cancel)
}

/// Exclusive, read-only access to retained saves without a write owner.
/// This deliberately does not call `UploadJournal::open`: no migrations,
/// reconciliation transitions, sealing or collection may run during recovery.
pub struct RecoveryJournal {
    journal: UploadJournal,
    _directory: std::sync::Arc<File>,
}
impl RecoveryJournal {
    pub fn open(root: &Path, account: &str) -> Result<Self> {
        if account.is_empty() {
            return Err(JournalError::Intent);
        }
        let dir = directory(root)?;
        let meta = dir.metadata()?;
        if meta.uid() != std::fs::metadata("/proc/self")?.uid()
            || meta.permissions().mode() & 0o077 != 0
        {
            return Err(JournalError::Storage);
        }
        let anchored = PathBuf::from(format!("/proc/self/fd/{}", dir.as_raw_fd()));
        let open_existing = |name: &str| -> Result<File> {
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(anchored.join(name))?;
            owned_private(&file)?;
            Ok(file)
        };
        let owner = open_existing("owner.lock")?;
        fs2::FileExt::try_lock_exclusive(&owner).map_err(|e| {
            if e.kind() == std::io::ErrorKind::WouldBlock {
                JournalError::Busy
            } else {
                JournalError::Storage
            }
        })?;
        let owner = JournalOwner::acquired(owner);
        let _database = open_existing("uploads.db")?;
        let db = Connection::open_with_flags(
            root.join("uploads.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        // SQLite rejects /proc descriptor aliases with NOFOLLOW. Check that
        // its ordinary pathname still identifies our held private directory
        // and database before accepting any records from the connection.
        let current_dir = std::fs::symlink_metadata(root)?;
        let current_db = std::fs::symlink_metadata(root.join("uploads.db"))?;
        let held_db = _database.metadata()?;
        if current_dir.dev() != meta.dev()
            || current_dir.ino() != meta.ino()
            || current_db.dev() != held_db.dev()
            || current_db.ino() != held_db.ino()
        {
            return Err(JournalError::Stale);
        }
        db.busy_timeout(std::time::Duration::from_secs(3))?;
        let version: u32 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if !(14..=super::JOURNAL_SCHEMA).contains(&version) {
            return Err(JournalError::Schema);
        }
        let stored: String =
            db.query_row("SELECT account FROM identity WHERE singleton=1", [], |r| {
                r.get(0)
            })?;
        if stored != account {
            return Err(JournalError::Account);
        }
        let objects_meta = std::fs::symlink_metadata(anchored.join("objects"))?;
        if !objects_meta.is_dir()
            || objects_meta.uid() != meta.uid()
            || objects_meta.permissions().mode() & 0o077 != 0
        {
            return Err(JournalError::Storage);
        }
        Ok(Self {
            journal: UploadJournal {
                db,
                objects: anchored.join("objects"),
                working: anchored.join("working"),
                account: account.into(),
                quota: 1,
                _owner: owner,
            },
            _directory: std::sync::Arc::new(dir),
        })
    }
    /// Bounded sequence pagination, including historical terminal states so the
    /// caller can advance even when a page contains no exportable versions.
    pub fn list(&self, after: u64, limit: u32) -> Result<Vec<UploadRecord>> {
        self.journal.list(after, limit.clamp(1, 200))
    }
    /// Indexed identity lookups only; never scan the retained working set.
    pub(crate) fn retained_name(&self, scope: &Scope, item: &str) -> Result<Option<String>> {
        if scope.account != self.journal.account {
            return Err(JournalError::Intent);
        }
        let object = super::namespace::by_local(&self.journal.db, scope, item)?
            .or(super::namespace::by_remote(&self.journal.db, scope, item)?);
        if let Some(object) = object {
            if object.scope != *scope {
                return Err(JournalError::Corrupt);
            }
            if !object.node.name.is_empty() {
                return Ok(Some(object.node.name));
            }
        }
        Ok(self
            .journal
            .working_by_identity(scope, item)?
            .map(|file| file.node.name)
            .filter(|name| !name.is_empty()))
    }
    pub fn recent_uploads(&self, limit: u32) -> Result<Vec<UploadRecord>> {
        self.journal.recent_uploads(limit.min(200))
    }
    /// Inspect retained outcomes without opening a writer or changing records.
    /// Counts use the writer's exact state predicates, independently of paging.
    pub(crate) fn retained_outcome_counts(&self) -> Result<(u64, u64, u64)> {
        Ok((
            self.journal.stuck_mutations()?,
            self.journal.unconfirmed_changes()?,
            self.journal.failed_uploads()?,
        ))
    }
    pub(crate) fn retained_failed_uploads(&self, limit: usize) -> Result<Vec<UploadRecord>> {
        self.journal.failed_upload_list(limit.clamp(1, 200))
    }
    pub(crate) fn retained_stuck_mutations(&self, limit: usize) -> Result<Vec<MutationRecord>> {
        self.journal.stuck_mutation_list(limit.clamp(1, 200))
    }
    pub fn local_export_source(&self, id: Uuid) -> Result<LocalExportSource> {
        self.journal.local_export_source(id)
    }
}

impl RecoveryJournal {
    pub(crate) fn native_trash_list(
        &self,
        scope: &Scope,
        after: Option<u64>,
        limit: u32,
    ) -> Result<crate::native_trash::NativeTrashListing> {
        self.journal.native_trash_list(scope, after, limit)
    }
}

#[cfg(feature = "icloud-write-probe")]
impl RecoveryJournal {
    /// Feature-only bounded inventories under the existing read-only owner lease.
    pub(crate) fn native_validation_mutations(
        &self,
        after: u64,
        limit: u32,
    ) -> Result<Vec<MutationRecord>> {
        self.journal.list_mutations(after, limit.clamp(1, 200))
    }
    pub(crate) fn native_validation_namespace_by_remote(
        &self,
        scope: &Scope,
        item: &str,
    ) -> Result<Option<NamespaceObject>> {
        self.journal.namespace_by_remote(scope, item)
    }
    pub(crate) fn native_validation_incomplete_queue(&self) -> Result<i64> {
        Ok(self.journal.db.query_row(
            "SELECT count(*) FROM write_queue WHERE complete=0",
            [],
            |row| row.get(0),
        )?)
    }
    pub(crate) fn native_validation_working_inventory(&self) -> Result<(i64, i64)> {
        Ok(self.journal.db.query_row(
            "SELECT count(*),coalesce(sum(CASE WHEN json_extract(body,'$.dirty')=1 OR json_extract(body,'$.unlinked')=1 THEN 1 ELSE 0 END),0) FROM working_files",
            [], |row| Ok((row.get(0)?, row.get(1)?)),
        )?)
    }
    /// Import-only validation: one owned folder and no native-save association.
    pub(crate) fn native_validation_import_auxiliary_inventory(&self) -> Result<(i64, i64, i64)> {
        Ok(self.journal.db.query_row(
            "SELECT (SELECT count(*) FROM namespace_objects),(SELECT count(*) FROM native_working_operations),(SELECT count(*) FROM write_queue)",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?)
    }
    /// Ordinary DATA validation only: bounded exact namespace/working frontier.
    /// No writer, checkpoint vault or provider is opened by these observations.
    #[allow(clippy::type_complexity)]
    pub(crate) fn ordinary_validation_inventory(
        &self,
    ) -> Result<(Vec<NamespaceObject>, Vec<WorkingFile>, (i64, i64))> {
        let mut query = self
            .journal
            .db
            .prepare("SELECT body FROM namespace_objects ORDER BY id LIMIT 4")?;
        let objects = query
            .query_map([], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect::<Result<Vec<_>>>()?;
        let mut query = self
            .journal
            .db
            .prepare("SELECT body FROM working_files ORDER BY id LIMIT 2")?;
        let working = query
            .query_map([], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect::<Result<Vec<_>>>()?;
        let other = self.journal.db.query_row(
            "SELECT (SELECT count(*) FROM package_metadata_publication),(SELECT count(*) FROM file_replacements)",
            [], |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok((objects, working, other))
    }
    pub(crate) fn ordinary_validation_namespace_for_operation(
        &self,
        operation: Uuid,
    ) -> Result<Option<NamespaceObject>> {
        self.journal.namespace_for_operation(operation)
    }
    /// The owned DATA arm has at most three namespace operation pairs.
    /// A fourth pair is a refusal witness, including unknown operations.
    pub(crate) fn ordinary_validation_namespace_operations(&self) -> Result<Vec<(String, String)>> {
        let mut query = self.journal.db.prepare(
            "SELECT operation,object FROM namespace_operations ORDER BY operation LIMIT 4",
        )?;
        let pairs = query
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(pairs)
    }
    /// Bind SQL identities/state to typed bodies and exact completed queue rows.
    /// The owned arm has at most three operations; a fourth is a refusal witness.
    #[allow(clippy::type_complexity)]
    pub(crate) fn ordinary_validation_operation_columns(
        &self,
    ) -> Result<Vec<(String, String, u64, String, Option<u64>, Option<bool>)>> {
        let mut query = self.journal.db.prepare(
            "SELECT 'upload',u.id,u.sequence,u.state,q.sequence,q.complete FROM uploads u
             LEFT JOIN write_queue q ON q.id=u.id
             UNION ALL
             SELECT 'mutation',m.id,m.sequence,m.state,q.sequence,q.complete FROM mutations m
             LEFT JOIN write_queue q ON q.id=m.id
             ORDER BY 3 LIMIT 4",
        )?;
        let rows = query
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                    r.get::<_, Option<bool>>(5)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(kind, id, sequence, state, queue, complete)| {
                Ok((
                    kind,
                    id,
                    u64::try_from(sequence).map_err(|_| JournalError::Corrupt)?,
                    state,
                    queue
                        .map(u64::try_from)
                        .transpose()
                        .map_err(|_| JournalError::Corrupt)?,
                    complete,
                ))
            })
            .collect()
    }
    pub(crate) fn ordinary_validation_working_digest(&self, id: Uuid) -> Result<(u64, String)> {
        use sha2::{Digest, Sha256};
        use std::os::unix::fs::MetadataExt;
        let mut file = self.journal.working_descriptor(id, false)?;
        let before = file.metadata()?;
        let size = before.len();
        if size > 64 * 1024 * 1024 {
            return Err(JournalError::Quota);
        }
        let mut hash = Sha256::new();
        let mut buffer = [0; 64 * 1024];
        let mut bounded = (&mut file).take(size + 1);
        let mut received = 0u64;
        loop {
            let count = bounded.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            received += count as u64;
            if received > size {
                return Err(JournalError::Stale);
            }
            hash.update(&buffer[..count]);
        }
        let after = file.metadata()?;
        if received != size
            || after.len() != size
            || after.dev() != before.dev()
            || after.ino() != before.ino()
            || after.mtime() != before.mtime()
            || after.mtime_nsec() != before.mtime_nsec()
            || after.ctime() != before.ctime()
            || after.ctime_nsec() != before.ctime_nsec()
        {
            return Err(JournalError::Stale);
        }
        Ok((size, hex::encode(hash.finalize())))
    }
    /// Indexed read-only native source/byte-stream association. Existing shape
    /// validation binds either live working bytes or an exact dormant slot.
    #[allow(clippy::type_complexity)]
    pub(crate) fn native_validation_fuse_association(
        &self,
        operation: Uuid,
    ) -> Result<
        Option<(
            Uuid,
            Uuid,
            NamespaceObject,
            NamespaceObject,
            Option<WorkingFile>,
        )>,
    > {
        let association: Option<(String, String)> = self
            .journal
            .db
            .query_row(
                "SELECT working,owner FROM native_working_operations WHERE operation=?1",
                [operation.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((working, owner)) = association else {
            return Ok(None);
        };
        let working = Uuid::parse_str(&working).map_err(|_| JournalError::Corrupt)?;
        let owner = Uuid::parse_str(&owner).map_err(|_| JournalError::Corrupt)?;
        let source = self.journal.namespace_object(owner)?;
        let child = self.journal.namespace_object(working)?;
        super::working::native::projection::validate_child(&self.journal.db, &child)?;
        if source.scope.account != self.journal.account
            || source.scope != child.scope
            || self
                .journal
                .namespace_for_operation(operation)?
                .is_none_or(|object| object.id != owner)
        {
            return Err(JournalError::Corrupt);
        }
        let body: Option<String> = self
            .journal
            .db
            .query_row(
                "SELECT body FROM working_files WHERE id=?1",
                [working.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        let file: Option<WorkingFile> = body.map(|body| serde_json::from_str(&body)).transpose()?;
        let row = self.journal.get(operation)?;
        let (original, current, _) = row
            .native_replacement_receipt()
            .ok_or(JournalError::Corrupt)?;
        let UploadRepresentation::PackageReplacementArchive {
            original_semantic,
            semantic,
            ..
        } = &row.representation
        else {
            return Err(JournalError::Corrupt);
        };
        if source.remote.as_ref() != Some(current) || source.remote_sequence != row.sequence {
            return Err(JournalError::Corrupt);
        }
        if file.is_some() {
            let body: String = self.journal.db.query_row(
                "SELECT body FROM native_working_bindings WHERE working=?1",
                [working.to_string()],
                |r| r.get(0),
            )?;
            let binding: serde_json::Value = serde_json::from_str(&body)?;
            let body: String = self.journal.db.query_row(
                "SELECT body FROM native_working_heads WHERE working=?1",
                [working.to_string()],
                |r| r.get(0),
            )?;
            let head: serde_json::Value = serde_json::from_str(&body)?;
            if binding["scope"] != serde_json::to_value(&row.scope)?
                || binding["source"] != serde_json::to_value(original)?
                || binding["semantic"] != serde_json::to_value(original_semantic)?
                || head["semantic"] != serde_json::to_value(semantic)?
            {
                return Err(JournalError::Corrupt);
            }
        } else {
            let body: String = self.journal.db.query_row(
                "SELECT body FROM native_retired_slots WHERE working=?1 AND owner=?2",
                params![working.to_string(), owner.to_string()],
                |r| r.get(0),
            )?;
            let slot: serde_json::Value = serde_json::from_str(&body)?;
            if slot["current"] != serde_json::to_value(current)?
                || slot["scope"] != serde_json::to_value(&row.scope)?
            {
                return Err(JournalError::Corrupt);
            }
        }
        Ok(Some((working, owner, source, child, file)))
    }
    pub(crate) fn native_validation_upload(&self, id: Uuid) -> Result<UploadRecord> {
        self.journal.get(id)
    }
    pub(crate) fn native_validation_package_publication(
        &self,
        id: Uuid,
    ) -> Result<PackagePublicationStatus> {
        self.journal.package_publication_status(id)
    }
    pub(crate) fn native_validation_absence(&self, id: Uuid) -> Result<bool> {
        Ok(self.journal.native_trash_publication_status(id)? == PackagePublicationStatus::Absent)
    }
    pub(crate) fn native_validation_mutation(&self, id: Uuid) -> Result<MutationRecord> {
        self.journal.mutation(id)
    }
}

impl RecoveryJournal {
    #[allow(dead_code)] // Read-only local observer contract; service wiring follows.
    pub(crate) fn native_replacement_list(
        &self,
        scope: &Scope,
        after: Option<u64>,
        limit: u32,
    ) -> Result<NativeReplacementListing> {
        self.journal.native_replacement_list(scope, after, limit)
    }
}

impl RecoveryJournal {
    pub fn native_stage_abandonment(
        &self,
        id: Uuid,
    ) -> Result<Option<cirrove_icloud::NativeReplacementAbandonRecord>> {
        self.journal.native_stage_abandonment(id)
    }
}

impl RecoveryJournal {
    pub(crate) fn native_import_list(
        &self,
        scope: &Scope,
        after: Option<u64>,
        limit: u32,
    ) -> Result<NativeImportListing> {
        self.journal.native_import_list(scope, after, limit)
    }
}
