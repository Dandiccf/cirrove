//! Private streamed windows. Bytes become readable only after provider validation.
use super::BLOCK_SIZE;
use cirrove_core::{ProviderError, reads::ReadWindowSink};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    os::unix::fs::FileExt,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const MAX_STAGING: u32 = 128 * 1024 * 1024;
const MAX_WINDOW: u32 = 64 * 1024 * 1024;
const CHUNK_LIMIT: usize = 64 * 1024;
#[derive(Debug, serde::Serialize)]
pub struct WindowStats {
    pub staging_budget_bytes: u32,
    pub staging_reserved_bytes: u32,
    pub staging_peak_bytes: u64,
    pub validated_windows: u64,
    pub staged_bytes: u64,
}
pub(super) struct Staging {
    path: PathBuf,
    capacity: u32,
    available: Arc<Semaphore>,
    peak: AtomicU64,
    validated: AtomicU64,
    bytes: AtomicU64,
}
impl Staging {
    pub(super) fn new(path: PathBuf, quota: u64) -> Self {
        let capacity = (quota / 4).min(MAX_STAGING as u64) as u32 / BLOCK_SIZE * BLOCK_SIZE;
        let capacity = if capacity >= 2 * BLOCK_SIZE {
            capacity
        } else {
            0
        };
        Self {
            path,
            capacity,
            available: Arc::new(Semaphore::new(capacity as usize)),
            peak: AtomicU64::new(0),
            validated: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
        }
    }
    pub(super) fn capacity(&self) -> u32 {
        self.capacity
    }
    pub(super) fn stats(&self) -> WindowStats {
        WindowStats {
            staging_budget_bytes: self.capacity,
            staging_reserved_bytes: self.capacity - self.available.available_permits() as u32,
            staging_peak_bytes: self.peak.load(Ordering::Relaxed),
            validated_windows: self.validated.load(Ordering::Relaxed),
            staged_bytes: self.bytes.load(Ordering::Relaxed),
        }
    }
    pub(super) fn reserve(&self, wanted: u32) -> Option<Reservation> {
        let length = wanted.min(MAX_WINDOW).min(self.capacity);
        if length <= BLOCK_SIZE {
            return None;
        }
        let permit = self.available.clone().try_acquire_many_owned(length).ok()?;
        self.peak.fetch_max(
            (self.capacity - self.available.available_permits() as u32) as u64,
            Ordering::Relaxed,
        );
        Some(Reservation { length, permit })
    }
    pub(super) async fn writer(&self, reservation: Reservation) -> Result<Writer, ProviderError> {
        let path = self.path.clone();
        let length = reservation.length;
        let file = tokio::task::spawn_blocking(move || {
            // Anonymous/unlinked temporary file: kernel closes it on process death.
            // An in-flight blocking operation retains the file AND byte reservation.
            tempfile::tempfile_in(path).map(|file| {
                Arc::new(ReservedFile {
                    file,
                    _permit: reservation.permit,
                })
            })
        })
        .await
        .map_err(|_| ProviderError::Unavailable)?
        .map_err(|_| ProviderError::Unavailable)?;
        Ok(Writer {
            file,
            length,
            written: 0,
            hash: Sha256::new(),
            hashes: vec![],
        })
    }
    pub(super) fn validated(&self, length: u32) {
        self.validated.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(length as u64, Ordering::Relaxed);
    }
}
pub(super) struct Reservation {
    pub length: u32,
    permit: OwnedSemaphorePermit,
}
struct ReservedFile {
    file: File,
    _permit: OwnedSemaphorePermit,
}
pub(super) struct Writer {
    file: Arc<ReservedFile>,
    length: u32,
    written: u32,
    hash: Sha256,
    hashes: Vec<[u8; 32]>,
}
#[async_trait::async_trait]
impl ReadWindowSink for Writer {
    async fn write_chunk(&mut self, bytes: &[u8]) -> Result<(), ProviderError> {
        if bytes.len() > CHUNK_LIMIT || bytes.len() > (self.length - self.written) as usize {
            return Err(ProviderError::Protocol(
                "staged window exceeds its reservation",
            ));
        }
        let file = self.file.clone();
        let data = bytes.to_vec();
        let offset = self.written;
        tokio::task::spawn_blocking(move || file.file.write_all_at(&data, offset as u64))
            .await
            .map_err(|_| ProviderError::Unavailable)?
            .map_err(|_| ProviderError::Unavailable)?;
        let mut remaining = bytes;
        while !remaining.is_empty() {
            let count = remaining
                .len()
                .min((BLOCK_SIZE - self.written % BLOCK_SIZE) as usize);
            self.hash.update(&remaining[..count]);
            self.written += count as u32;
            remaining = &remaining[count..];
            if self.written.is_multiple_of(BLOCK_SIZE) {
                self.hashes.push(self.hash.finalize_reset().into());
            }
        }
        Ok(())
    }
}
impl Writer {
    /// Called only after the adapter validated the full window.
    pub(super) fn finish(mut self, start: u64) -> Result<Window, ProviderError> {
        if self.written != self.length {
            return Err(ProviderError::Protocol("incomplete staged window"));
        }
        if !self.written.is_multiple_of(BLOCK_SIZE) {
            self.hashes.push(self.hash.finalize_reset().into());
        }
        Ok(Window {
            file: self.file,
            start,
            length: self.length,
            hashes: self.hashes,
        })
    }
}
pub(super) struct Window {
    file: Arc<ReservedFile>,
    start: u64,
    length: u32,
    hashes: Vec<[u8; 32]>,
}
impl Window {
    pub(super) async fn read(&self, offset: u64, length: u32) -> Result<Vec<u8>, ProviderError> {
        let within = offset
            .checked_sub(self.start)
            .ok_or(ProviderError::Unavailable)?;
        if !within.is_multiple_of(BLOCK_SIZE as u64) || within >= self.length as u64 {
            return Err(ProviderError::Unavailable);
        }
        let count = (self.length as u64 - within).min(BLOCK_SIZE as u64) as u32;
        if length != count {
            return Err(ProviderError::Protocol(
                "staged window read is not one complete block",
            ));
        }
        let hash = *self
            .hashes
            .get((within / BLOCK_SIZE as u64) as usize)
            .ok_or(ProviderError::Unavailable)?;
        let file = self.file.clone();
        tokio::task::spawn_blocking(move || {
            let mut bytes = vec![0; count as usize];
            file.file
                .read_exact_at(&mut bytes, within)
                .map_err(|_| ProviderError::Unavailable)?;
            if Sha256::digest(&bytes)[..] != hash {
                return Err(ProviderError::Protocol("staged window checksum mismatch"));
            }
            Ok(bytes)
        })
        .await
        .map_err(|_| ProviderError::Unavailable)?
    }
}

#[cfg(test)]
mod tests;
