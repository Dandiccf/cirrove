//! One process's shared journal lease, independent of inherited child descriptors.
use std::{fs::File, sync::Arc};

/// Share this owner instead of duplicating its file descriptor. Working sources
/// and export stages intentionally outlive the journal connection; none may
/// release its flock while another in-process holder still uses retained bytes.
pub(super) struct JournalOwner {
    file: File,
    pid: u32,
}
impl JournalOwner {
    /// Call immediately after acquiring the flock, before any fallible setup.
    pub(super) fn acquired(file: File) -> Arc<Self> {
        Arc::new(Self {
            file,
            pid: std::process::id(),
        })
    }

    pub(super) fn file(&self) -> &File {
        &self.file
    }
}
impl Drop for JournalOwner {
    fn drop(&mut self) {
        // flock is attached to the open file description: closing our last FD
        // alone can leave an unrelated forked child's inherited FD holding it.
        // Unlock only on the final in-process Arc, never from a forked child's
        // copy of this Rust owner (which would unlock the live parent's lease).
        if self.pid == std::process::id() {
            // An unlock error remains conservative: closing the FD still follows,
            // and any inherited descriptor may keep the lock rather than grant
            // a second writer prematurely. Drop must not panic during unwinding.
            let _ = fs2::FileExt::unlock(&self.file);
        }
    }
}
