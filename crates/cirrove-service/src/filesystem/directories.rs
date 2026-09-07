//! Process-local directory listings with immutable published prefixes. Files
//! are anonymous, so closing the final handle or killing the process reclaims
//! them without an orphan scan. These are never local edits or restart state.
use std::{
    fs::File,
    io::{self, BufWriter, Write},
    os::unix::fs::FileExt,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::sync::watch;

const PAGE_BYTES: usize = 64 * 1024;
const PAGE_ENTRIES: usize = 1024;
const HEADER_BYTES: usize = 13;
const INDEX_BYTES: u64 = 8;
const MAX_SNAPSHOTS: usize = 256;
const MAX_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Default)]
struct Usage {
    bytes: u64,
    snapshots: usize,
}
#[derive(Clone, Default)]
pub(super) struct Budget(Arc<Mutex<Usage>>);
struct Reservation {
    budget: Budget,
    bytes: AtomicU64,
}
impl Budget {
    pub fn start(&self, directory: &Path) -> io::Result<Builder> {
        {
            let mut usage = self
                .0
                .lock()
                .map_err(|_| io::Error::other("snapshot budget lock failed"))?;
            if usage.snapshots >= MAX_SNAPSHOTS {
                return Err(io::Error::from_raw_os_error(libc::EMFILE));
            }
            usage.snapshots += 1;
        }
        let reservation = Arc::new(Reservation {
            budget: self.clone(),
            bytes: AtomicU64::new(0),
        });
        let data = tempfile::tempfile_in(directory)?;
        let index = tempfile::tempfile_in(directory)?;
        let (progress, receiver) = watch::channel(Progress::default());
        Ok(Builder {
            data: BufWriter::with_capacity(PAGE_BYTES, data.try_clone()?),
            index: BufWriter::with_capacity(PAGE_ENTRIES * 8, index.try_clone()?),
            snapshot: Some(Snapshot {
                data,
                index,
                progress: receiver,
                _reservation: reservation.clone(),
            }),
            progress,
            reservation,
            entries: 0,
            data_bytes: 0,
            failed: false,
        })
    }
    #[cfg(test)]
    pub fn usage(&self) -> (u64, usize) {
        let usage = self.0.lock().expect("snapshot budget lock");
        (usage.bytes, usage.snapshots)
    }
}
impl Reservation {
    fn grow(&self, bytes: u64) -> io::Result<()> {
        let mut usage = self
            .budget
            .0
            .lock()
            .map_err(|_| io::Error::other("snapshot budget lock failed"))?;
        let next = usage
            .bytes
            .checked_add(bytes)
            .filter(|n| *n <= MAX_BYTES)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOSPC))?;
        usage.bytes = next;
        // Only the builder grows this reservation; readers merely retain it.
        self.bytes.fetch_add(bytes, Ordering::Relaxed);
        Ok(())
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if let Ok(mut usage) = self.budget.0.lock() {
            usage.bytes -= self.bytes.load(Ordering::Relaxed);
            usage.snapshots -= 1;
        }
    }
}
fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid directory snapshot")
}

pub(super) struct Entry {
    pub inode: u64,
    pub directory: bool,
    pub name: String,
}
pub(super) struct Builder {
    data: BufWriter<File>,
    index: BufWriter<File>,
    snapshot: Option<Snapshot>,
    progress: watch::Sender<Progress>,
    // Last field owning the reservation, after both writer descriptors.
    reservation: Arc<Reservation>,
    entries: u64,
    data_bytes: u64,
    failed: bool,
}
impl Builder {
    pub fn push(&mut self, inode: u64, directory: bool, name: &str) -> io::Result<()> {
        if self.failed {
            return Err(invalid());
        }
        let result = self.append(inode, directory, name);
        if let Err(error) = &result {
            self.fail(error.raw_os_error().unwrap_or(libc::EIO));
        }
        result
    }
    fn append(&mut self, inode: u64, directory: bool, name: &str) -> io::Result<()> {
        if name.len() > PAGE_BYTES - HEADER_BYTES {
            return Err(io::Error::from_raw_os_error(libc::ENAMETOOLONG));
        }
        if inode == 0 || name.is_empty() || name.contains('/') || name.contains('\0') {
            return Err(invalid());
        }
        let bytes = (HEADER_BYTES + name.len()) as u64;
        self.reservation.grow(bytes + INDEX_BYTES)?;
        self.index.write_all(&self.data_bytes.to_le_bytes())?;
        self.data.write_all(&inode.to_le_bytes())?;
        self.data.write_all(&[u8::from(directory)])?;
        self.data.write_all(&(name.len() as u32).to_le_bytes())?;
        self.data.write_all(name.as_bytes())?;
        self.entries += 1;
        self.data_bytes += bytes;
        Ok(())
    }
    pub fn cancelled(&self) -> bool {
        self.progress.is_closed()
    }
    pub fn fail(&mut self, error: i32) {
        self.failed = true;
        self.progress.send_modify(|state| state.error = Some(error));
    }
    fn flush(&mut self, complete: bool) -> io::Result<()> {
        if self.failed {
            return Err(invalid());
        }
        if self.cancelled() {
            return Err(io::Error::from_raw_os_error(libc::ECANCELED));
        }
        // Publish the frontier only after BOTH files contain the complete batch.
        if let Err(error) = self.data.flush().and_then(|_| self.index.flush()) {
            self.fail(error.raw_os_error().unwrap_or(libc::EIO));
            return Err(error);
        }
        self.progress.send_replace(Progress {
            entries: self.entries,
            data_bytes: self.data_bytes,
            complete,
            error: None,
        });
        Ok(())
    }
    /// Transfer the sole snapshot after its first flushed batch; later calls
    /// advance its immutable prefix without creating another handle or cache.
    pub fn publish(&mut self) -> io::Result<Option<Snapshot>> {
        self.flush(false)?;
        Ok(self.snapshot.take())
    }
    pub fn complete(mut self) -> io::Result<Option<Snapshot>> {
        self.flush(true)?;
        Ok(self.snapshot.take())
    }
    pub fn finish(self) -> io::Result<Snapshot> {
        self.complete()?.ok_or_else(invalid)
    }
}
#[derive(Clone, Copy, Default)]
struct Progress {
    entries: u64,
    data_bytes: u64,
    complete: bool,
    error: Option<i32>,
}
pub(super) struct Snapshot {
    data: File,
    index: File,
    progress: watch::Receiver<Progress>,
    _reservation: Arc<Reservation>,
}
impl Snapshot {
    #[cfg(test)]
    pub fn len(&self) -> u64 {
        self.progress.borrow().entries
    }
    pub async fn ready(&self, offset: u64) -> io::Result<()> {
        let mut progress = self.progress.clone();
        loop {
            let state = *progress.borrow_and_update();
            if offset < state.entries {
                return Ok(());
            }
            if let Some(error) = state.error {
                return Err(io::Error::from_raw_os_error(error));
            }
            if state.complete {
                return Ok(());
            }
            if progress.changed().await.is_err() {
                // Recheck the final value: a successful producer may close its
                // sender immediately after publishing Complete.
                let state = *progress.borrow();
                if offset < state.entries || state.complete && state.error.is_none() {
                    return Ok(());
                }
                return Err(io::Error::from_raw_os_error(
                    state.error.unwrap_or(libc::EIO),
                ));
            }
        }
    }
    /// Cookies are ordinal positions. Positioned reads let independent kernel
    /// requests use one immutable snapshot without a shared seek cursor/lock.
    pub fn page(&self, offset: u64) -> io::Result<Vec<Entry>> {
        // Copy the frontier and release the watch borrow before any disk I/O.
        let state = *self.progress.borrow();
        if offset >= state.entries {
            if let Some(error) = state.error {
                return Err(io::Error::from_raw_os_error(error));
            }
            return if state.complete {
                Ok(Vec::new())
            } else if self.progress.has_changed().is_err() {
                Err(io::Error::from_raw_os_error(libc::EIO))
            } else {
                Err(io::ErrorKind::WouldBlock.into())
            };
        }
        let count = (state.entries - offset).min(PAGE_ENTRIES as u64) as usize;
        let extra = usize::from(offset + (count as u64) < state.entries);
        let mut index = vec![0u8; (count + extra) * 8];
        self.index.read_exact_at(&mut index, offset * INDEX_BYTES)?;
        let mut positions = index
            .as_chunks::<8>()
            .0
            .iter()
            .copied()
            .map(u64::from_le_bytes)
            .collect::<Vec<_>>();
        if extra == 0 {
            positions.push(state.data_bytes);
        }
        let start = positions[0];
        let mut take = 0;
        for pair in positions.windows(2) {
            let length = pair[1].checked_sub(pair[0]).ok_or_else(invalid)?;
            if length < HEADER_BYTES as u64
                || length > PAGE_BYTES as u64
                || pair[1] > state.data_bytes
            {
                return Err(invalid());
            }
            if pair[1].checked_sub(start).ok_or_else(invalid)? > PAGE_BYTES as u64 {
                break;
            }
            take += 1;
        }
        if take == 0 {
            return Err(invalid());
        }
        let mut bytes = vec![0; (positions[take] - start) as usize];
        self.data.read_exact_at(&mut bytes, start)?;
        let mut entries = Vec::with_capacity(take);
        for pair in positions[..=take].windows(2) {
            let record = &bytes[(pair[0] - start) as usize..(pair[1] - start) as usize];
            let inode = u64::from_le_bytes(record[..8].try_into().map_err(|_| invalid())?);
            let directory = match record[8] {
                0 => false,
                1 => true,
                _ => return Err(invalid()),
            };
            let size =
                u32::from_le_bytes(record[9..13].try_into().map_err(|_| invalid())?) as usize;
            let name = std::str::from_utf8(&record[13..]).map_err(|_| invalid())?;
            if size != name.len()
                || inode == 0
                || name.is_empty()
                || name.contains('/')
                || name.contains('\0')
            {
                return Err(invalid());
            }
            entries.push(Entry {
                inode,
                directory,
                name: name.into(),
            });
        }
        Ok(entries)
    }
}

#[cfg(test)]
mod tests;
