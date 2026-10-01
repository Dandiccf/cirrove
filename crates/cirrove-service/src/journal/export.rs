//! Local recovery without replay: pinned immutable saves and offline working bytes.
use super::*;
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
        if !matches!(
            record.state,
            UploadState::Pending
                | UploadState::Uploading
                | UploadState::VerifyRequired
                | UploadState::Verifying
                | UploadState::Conflict
                | UploadState::Failed
        ) || record.scope.account != self.account
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
    pub fn local_export_source(&self, id: Uuid) -> Result<LocalExportSource> {
        self.journal.local_export_source(id)
    }
}
