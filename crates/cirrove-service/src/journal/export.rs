//! A pinned immutable save, copied without holding the journal lock or replaying it.
use super::*;
use cirrove_core::CancellationToken;
use std::os::fd::AsRawFd;

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
        mut self,
        destination: &Path,
        cancel: &CancellationToken,
        mut progress: impl FnMut(u64),
    ) -> Result<LocalExportReceipt> {
        if cancel.is_cancelled() {
            return Err(JournalError::Stale);
        }
        let parent = destination.parent().ok_or(JournalError::Intent)?;
        let name = destination.file_name().ok_or(JournalError::Intent)?;
        let dir = directory(parent)?;
        let anchored = PathBuf::from(format!("/proc/self/fd/{}", dir.as_raw_fd()));
        let resolved = std::fs::read_link(&anchored)?;
        if resolved.starts_with(&self.journal_root) {
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
            let count = self.file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            copied = copied
                .checked_add(count as u64)
                .ok_or(JournalError::Corrupt)?;
            if copied > self.size {
                return Err(JournalError::Corrupt);
            }
            hash.update(&buffer[..count]);
            temporary.write_all(&buffer[..count])?;
            progress(copied);
        }
        if copied != self.size || hex::encode(hash.finalize()) != self.sha256 {
            return Err(JournalError::Corrupt);
        }
        temporary.as_file().sync_all()?;
        if cancel.is_cancelled() {
            return Err(JournalError::Stale);
        }
        if std::fs::read_link(&anchored)? != resolved {
            return Err(JournalError::Stale);
        }
        temporary
            .persist_noclobber(&target)
            .map_err(|error| JournalError::from(error.error))?;
        dir.sync_all()?;
        Ok(LocalExportReceipt {
            operation: self.operation,
            size: self.size,
            sha256: self.sha256,
            destination: destination.into(),
        })
    }
}
