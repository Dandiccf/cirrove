use cirrove_core::{
    CancellationToken,
    upload::{PackageSourceLayout, UploadRepresentation},
};
use cirrove_icloud::{
    PackageDownload, package_archive_semantic_identity_versioned,
    package_flat_archive_semantic_identity_v2,
};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, Metadata, OpenOptions, Permissions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Component, Path},
};

// This explicit fresh-proof policy must not be usable with a pre-v2 journal.
// Stored v1 proofs continue to be verified with v1, never rewritten.
const _: () = assert!(crate::journal::JOURNAL_SCHEMA >= 18);
const FRESH_NATIVE_SEMANTIC_VERSION: u32 = 2;
const MAX_ARCHIVE: u64 = 64 * 1024 * 1024;

/// Deliberately contains no source path, archive names, or parser/provider body.
#[derive(Debug, thiserror::Error)]
pub enum ImportAdmissionError {
    #[error("native import was cancelled")]
    Cancelled,
    #[error("native import requires a regular, unchanged local source file")]
    Source,
    #[error("native import staging must be a private local disk directory")]
    Staging,
    #[error("native archive exceeds the supported size limit")]
    Limit,
    #[error("unsupported or invalid native archive")]
    Archive,
    #[error("native import local storage is unavailable")]
    Storage,
    #[error("native import local storage is full")]
    DeviceFull,
}
type Result<T> = std::result::Result<T, ImportAdmissionError>;
fn storage(error: std::io::Error) -> ImportAdmissionError {
    match error.raw_os_error() {
        Some(libc::ENOSPC | libc::EDQUOT) => ImportAdmissionError::DeviceFull,
        _ => ImportAdmissionError::Storage,
    }
}
fn check(cancel: &CancellationToken) -> Result<()> {
    if cancel.is_cancelled() {
        Err(ImportAdmissionError::Cancelled)
    } else {
        Ok(())
    }
}

/// Capability produced only by bounded capture and explicit-layout semantic parsing.
/// The anonymous snapshot has no public path and only a read-only descriptor is
/// retained. A filename extension is never admission authority.
/// Call capture from a bounded blocking task, outside the journal lock.
pub struct ValidatedPackageArchive {
    file: File,
    representation: UploadRepresentation,
    size: u64,
    sha256: String,
}
impl ValidatedPackageArchive {
    /// Explicit source contract; a missing wrapper is never inferred from bytes.
    pub fn capture_with_source_layout(
        source: &Path,
        staging_directory: &Path,
        source_layout: PackageSourceLayout,
        expected_root: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<Self> {
        Self::capture_excluding_with_source_layout(
            source,
            staging_directory,
            source_layout,
            expected_root,
            cancel,
            &[],
        )
    }
    pub fn capture(
        source: &Path,
        staging_directory: &Path,
        expected_root: &str,
        cancel: &CancellationToken,
    ) -> Result<Self> {
        Self::capture_with_source_layout(
            source,
            staging_directory,
            PackageSourceLayout::Wrapped,
            Some(expected_root),
            cancel,
        )
    }
    #[cfg(test)]
    pub(crate) fn capture_excluding(
        source: &Path,
        staging_directory: &Path,
        expected_root: &str,
        cancel: &CancellationToken,
        excluded: &[std::path::PathBuf],
    ) -> Result<Self> {
        Self::capture_observed(
            source,
            staging_directory,
            expected_root,
            cancel,
            excluded,
            |_| {},
        )
    }
    #[cfg(test)]
    fn capture_observed(
        source: &Path,
        staging_directory: &Path,
        expected_root: &str,
        cancel: &CancellationToken,
        excluded: &[std::path::PathBuf],
        mut copied: impl FnMut(u64),
    ) -> Result<Self> {
        Self::capture_observed_with_source_layout(
            source,
            staging_directory,
            PackageSourceLayout::Wrapped,
            Some(expected_root),
            cancel,
            excluded,
            &mut copied,
        )
    }
    pub(crate) fn capture_excluding_with_source_layout(
        source: &Path,
        staging_directory: &Path,
        source_layout: PackageSourceLayout,
        expected_root: Option<&str>,
        cancel: &CancellationToken,
        excluded: &[std::path::PathBuf],
    ) -> Result<Self> {
        Self::capture_observed_with_source_layout(
            source,
            staging_directory,
            source_layout,
            expected_root,
            cancel,
            excluded,
            |_| {},
        )
    }
    fn capture_observed_with_source_layout(
        source: &Path,
        staging_directory: &Path,
        source_layout: PackageSourceLayout,
        expected_root: Option<&str>,
        cancel: &CancellationToken,
        excluded: &[std::path::PathBuf],
        mut copied: impl FnMut(u64),
    ) -> Result<Self> {
        check(cancel)?;
        if !matches!(
            (source_layout, expected_root),
            (PackageSourceLayout::Wrapped, Some(_))
                | (
                    PackageSourceLayout::FlatNumbers | PackageSourceLayout::FlatPages,
                    None
                )
        ) {
            return Err(ImportAdmissionError::Archive);
        }
        let directory = private_directory(staging_directory)?;
        if !source.is_absolute() {
            return Err(ImportAdmissionError::Source);
        }
        // O_PATH pins identity without opening a FUSE content handle or FIFO.
        let source_path = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_NOFOLLOW)
            .open(source)
            .map_err(|_| ImportAdmissionError::Source)?;
        let before = source_path
            .metadata()
            .map_err(|_| ImportAdmissionError::Source)?;
        if !before.is_file() {
            return Err(ImportAdmissionError::Source);
        }
        if before.len() == 0 || before.len() > MAX_ARCHIVE {
            return Err(ImportAdmissionError::Limit);
        }
        let fs = rustix::fs::fstatfs(&source_path).map_err(|_| ImportAdmissionError::Source)?;
        if fs.f_type == libc::FUSE_SUPER_MAGIC {
            return Err(ImportAdmissionError::Source);
        }
        let pinned_path = std::fs::read_link(format!("/proc/self/fd/{}", source_path.as_raw_fd()))
            .map_err(|_| ImportAdmissionError::Source)?;
        if excluded.iter().any(|root| pinned_path.starts_with(root)) {
            return Err(ImportAdmissionError::Source);
        }
        let mut source = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(format!("/proc/self/fd/{}", source_path.as_raw_fd()))
            .map_err(|_| ImportAdmissionError::Source)?;
        if !same_source(
            &before,
            &source
                .metadata()
                .map_err(|_| ImportAdmissionError::Source)?,
        ) {
            return Err(ImportAdmissionError::Source);
        }
        // Descriptor-relative directory identity survives path substitution.
        let directory_path = format!("/proc/self/fd/{}", directory.as_raw_fd());
        let mut snapshot = tempfile::tempfile_in(directory_path).map_err(storage)?;
        snapshot
            .set_permissions(Permissions::from_mode(0o600))
            .map_err(storage)?;
        let mut hash = Sha256::new();
        let mut size = 0u64;
        let mut buffer = [0u8; 128 * 1024];
        loop {
            check(cancel)?;
            let count = source
                .read(&mut buffer)
                .map_err(|_| ImportAdmissionError::Source)?;
            if count == 0 {
                break;
            }
            size = size
                .checked_add(count as u64)
                .ok_or(ImportAdmissionError::Limit)?;
            if size > MAX_ARCHIVE || size > before.len() {
                return Err(ImportAdmissionError::Source);
            }
            #[cfg(test)]
            if let Some(errno) = WRITE_FAILURE.with(|fault| fault.take()) {
                return Err(storage(std::io::Error::from_raw_os_error(errno)));
            }
            snapshot.write_all(&buffer[..count]).map_err(storage)?;
            hash.update(&buffer[..count]);
            copied(size);
        }
        check(cancel)?;
        let after = source
            .metadata()
            .map_err(|_| ImportAdmissionError::Source)?;
        if size != before.len() || !same_source(&before, &after) {
            return Err(ImportAdmissionError::Source);
        }
        snapshot
            .set_permissions(Permissions::from_mode(0o400))
            .map_err(storage)?;
        snapshot.sync_all().map_err(storage)?;
        let receipt = PackageDownload {
            size,
            sha256: hex::encode(hash.finalize()),
        };
        // Close the sole writable handle BEFORE validation and capability return.
        let file =
            File::open(format!("/proc/self/fd/{}", snapshot.as_raw_fd())).map_err(storage)?;
        drop(snapshot);
        let semantic = match (source_layout, expected_root) {
            (PackageSourceLayout::Wrapped, Some(root)) => {
                package_archive_semantic_identity_versioned(
                    &file,
                    &receipt,
                    root,
                    FRESH_NATIVE_SEMANTIC_VERSION,
                    cancel,
                )
            }
            (PackageSourceLayout::FlatNumbers | PackageSourceLayout::FlatPages, None) => {
                package_flat_archive_semantic_identity_v2(&file, &receipt, cancel)
            }
            _ => return Err(ImportAdmissionError::Archive),
        }
        .map_err(|_| {
            if cancel.is_cancelled() {
                ImportAdmissionError::Cancelled
            } else {
                ImportAdmissionError::Archive
            }
        })?;
        check(cancel)?;
        Ok(Self {
            file,
            representation: match (source_layout, expected_root) {
                (PackageSourceLayout::Wrapped, Some(root)) => {
                    UploadRepresentation::PackageArchive {
                        expected_root: root.to_owned(),
                        semantic,
                    }
                }
                (PackageSourceLayout::FlatNumbers, None) => {
                    UploadRepresentation::FlatNumbersArchive { semantic }
                }
                (PackageSourceLayout::FlatPages, None) => {
                    UploadRepresentation::FlatPagesArchive { semantic }
                }
                _ => return Err(ImportAdmissionError::Archive),
            },
            size,
            sha256: receipt.sha256,
        })
    }
    pub(crate) fn into_parts(self) -> (File, UploadRepresentation, u64, String) {
        (self.file, self.representation, self.size, self.sha256)
    }
}
fn same_source(before: &Metadata, after: &Metadata) -> bool {
    before.dev() == after.dev()
        && before.ino() == after.ino()
        && before.len() == after.len()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
        && before.ctime() == after.ctime()
        && before.ctime_nsec() == after.ctime_nsec()
}
fn private_directory(path: &Path) -> Result<File> {
    if !path.is_absolute() {
        return Err(ImportAdmissionError::Staging);
    }
    let mut directory = File::open("/").map_err(storage)?;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                directory = File::from(
                    rustix::fs::openat(
                        &directory,
                        name,
                        rustix::fs::OFlags::RDONLY
                            | rustix::fs::OFlags::DIRECTORY
                            | rustix::fs::OFlags::NOFOLLOW
                            | rustix::fs::OFlags::CLOEXEC,
                        rustix::fs::Mode::empty(),
                    )
                    .map_err(|_| ImportAdmissionError::Staging)?,
                );
            }
            _ => return Err(ImportAdmissionError::Staging),
        }
    }
    let metadata = directory.metadata().map_err(storage)?;
    let owner = std::fs::metadata("/proc/self").map_err(storage)?.uid();
    let fs = rustix::fs::fstatfs(&directory).map_err(|_| ImportAdmissionError::Staging)?;
    if metadata.uid() != owner
        || metadata.mode() & 0o777 != 0o700
        || fs.f_type == libc::FUSE_SUPER_MAGIC
        || fs.f_type == libc::TMPFS_MAGIC
    {
        return Err(ImportAdmissionError::Staging);
    }
    Ok(directory)
}

#[cfg(test)]
thread_local! {
    // Fail at the actual staged-write boundary without filling a developer disk.
    static WRITE_FAILURE: std::cell::Cell<Option<i32>> = const { std::cell::Cell::new(None) };
}
#[cfg(test)]
pub(crate) mod tests;
