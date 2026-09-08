//! Process-local directory listings with immutable published prefixes. A small
//! listing stays resident; larger ones spill to anonymous files. Those files are
//! unlinked, so closing the final handle or killing the process reclaims them
//! without an orphan scan. These are never local edits or restart state.
use std::{
    fs::File,
    io::{self},
    os::unix::fs::FileExt,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, RwLock,
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
/// Creating a snapshot file is a filesystem metadata operation, so it queues
/// behind whatever else is writing to that filesystem. An ordinary listing must
/// not pay that on the navigation path, so it stays resident until this many
/// data and index bytes; the mount can hold at most MAX_SNAPSHOTS of them.
const SPILL_BYTES: u64 = 64 * 1024;

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
    /// Starting a listing performs no filesystem operation at all. Files appear
    /// only if the listing outgrows its resident allowance.
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
        let backing = Arc::new(RwLock::new(Backing::Resident {
            data: Vec::new(),
            index: Vec::new(),
        }));
        let (progress, receiver) = watch::channel(Progress::default());
        Ok(Builder {
            directory: directory.to_owned(),
            backing: backing.clone(),
            pending_data: Vec::new(),
            pending_index: Vec::new(),
            stored_data: 0,
            stored_entries: 0,
            writers: None,
            snapshot: Some(Snapshot {
                backing,
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
/// Where a listing's bytes live. The same value is shared by the builder and
/// every reader, so a spill is visible to readers that already hold the
/// snapshot. Reads take the shared lock; only appending and spilling exclude.
enum Backing {
    Resident { data: Vec<u8>, index: Vec<u8> },
    Files { data: File, index: File },
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Part {
    Data,
    Index,
}
impl Backing {
    fn resident_bytes(&self) -> u64 {
        match self {
            Self::Resident { data, index } => (data.len() + index.len()) as u64,
            Self::Files { .. } => 0,
        }
    }
    /// Move an oversized listing to anonymous files, preserving the bytes that
    /// readers may already hold published positions for, and hand the caller
    /// independent writer handles so producing never blocks a reader.
    fn spill(&mut self, directory: &Path) -> io::Result<(File, File)> {
        let Self::Resident { data, index } = self else {
            return Err(invalid());
        };
        let data_file = tempfile::tempfile_in(directory)?;
        let index_file = tempfile::tempfile_in(directory)?;
        data_file.write_all_at(data, 0)?;
        index_file.write_all_at(index, 0)?;
        let writers = (data_file.try_clone()?, index_file.try_clone()?);
        *self = Self::Files {
            data: data_file,
            index: index_file,
        };
        Ok(writers)
    }
    fn append(&mut self, data_at: u64, data: &[u8], index_at: u64, index: &[u8]) -> io::Result<()> {
        let Self::Resident {
            data: resident_data,
            index: resident_index,
        } = self
        else {
            return Err(invalid());
        };
        if data_at != resident_data.len() as u64 || index_at != resident_index.len() as u64 {
            return Err(invalid());
        }
        resident_data.extend_from_slice(data);
        resident_index.extend_from_slice(index);
        Ok(())
    }
    fn read_at(&self, part: Part, buffer: &mut [u8], offset: u64) -> io::Result<()> {
        match self {
            Self::Resident { data, index } => {
                let source = if part == Part::Data { data } else { index };
                let start = usize::try_from(offset).map_err(|_| invalid())?;
                let end = start.checked_add(buffer.len()).ok_or_else(invalid)?;
                let bytes = source
                    .get(start..end)
                    .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
                buffer.copy_from_slice(bytes);
                Ok(())
            }
            Self::Files { data, index } => {
                (if part == Part::Data { data } else { index }).read_exact_at(buffer, offset)
            }
        }
    }
}
fn poisoned() -> io::Error {
    io::Error::other("directory snapshot lock failed")
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
    directory: PathBuf,
    backing: Arc<RwLock<Backing>>,
    // Bytes accepted but not yet stored, so a batch reaches the backing whole.
    pending_data: Vec<u8>,
    pending_index: Vec<u8>,
    stored_data: u64,
    stored_entries: u64,
    // Writer handles exist only after a spill, independent of the reader's.
    writers: Option<(File, File)>,
    snapshot: Option<Snapshot>,
    progress: watch::Sender<Progress>,
    // Last field owning the reservation, after the shared backing.
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
        self.pending_index
            .extend_from_slice(&self.data_bytes.to_le_bytes());
        self.pending_data.extend_from_slice(&inode.to_le_bytes());
        self.pending_data.push(u8::from(directory));
        self.pending_data
            .extend_from_slice(&(name.len() as u32).to_le_bytes());
        self.pending_data.extend_from_slice(name.as_bytes());
        self.entries += 1;
        self.data_bytes += bytes;
        // Bound what an unpublished batch holds, exactly as the previous
        // buffered writer did. Storing never advances the reader frontier.
        if self.pending_data.len() >= PAGE_BYTES {
            self.store()?;
        }
        Ok(())
    }
    /// Move accepted bytes into the shared backing, spilling to files first if
    /// the listing has outgrown its resident allowance.
    fn store(&mut self) -> io::Result<()> {
        if self.pending_data.is_empty() && self.pending_index.is_empty() {
            return Ok(());
        }
        let index_at = self.stored_entries * INDEX_BYTES;
        if self.writers.is_none() {
            let mut backing = self.backing.write().map_err(|_| poisoned())?;
            let pending = (self.pending_data.len() + self.pending_index.len()) as u64;
            if backing.resident_bytes() + pending > SPILL_BYTES {
                self.writers = Some(backing.spill(&self.directory)?);
            } else {
                backing.append(
                    self.stored_data,
                    &self.pending_data,
                    index_at,
                    &self.pending_index,
                )?;
            }
        }
        if let Some((data, index)) = &self.writers {
            data.write_all_at(&self.pending_data, self.stored_data)?;
            index.write_all_at(&self.pending_index, index_at)?;
        }
        self.stored_data += self.pending_data.len() as u64;
        self.stored_entries += self.pending_index.len() as u64 / INDEX_BYTES;
        self.pending_data.clear();
        self.pending_index.clear();
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
        // Publish the frontier only after BOTH streams hold the complete batch.
        if let Err(error) = self.store() {
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
    backing: Arc<RwLock<Backing>>,
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
        let backing = self.backing.read().map_err(|_| poisoned())?;
        backing.read_at(Part::Index, &mut index, offset * INDEX_BYTES)?;
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
        backing.read_at(Part::Data, &mut bytes, start)?;
        drop(backing);
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
impl Builder {
    /// Spill first, so an already published prefix stays readable, then point
    /// one stream at a device that always reports ENOSPC.
    pub fn fail_writes(&mut self, part: Part) -> io::Result<()> {
        if self.writers.is_none() {
            let mut backing = self.backing.write().map_err(|_| poisoned())?;
            self.writers = Some(backing.spill(&self.directory)?);
        }
        let full = std::fs::OpenOptions::new().write(true).open("/dev/full")?;
        let writers = self.writers.as_mut().ok_or_else(invalid)?;
        if part == Part::Data {
            writers.0 = full;
        } else {
            writers.1 = full;
        }
        Ok(())
    }
}
#[cfg(test)]
impl Snapshot {
    pub fn resident(&self) -> bool {
        matches!(
            &*self.backing.read().expect("snapshot backing"),
            Backing::Resident { .. }
        )
    }
    /// Anonymous files have no directory entry; a resident listing has no file
    /// at all. Both satisfy the requirement that nothing survives a crash.
    pub fn unlinked(&self) -> bool {
        match &*self.backing.read().expect("snapshot backing") {
            Backing::Resident { .. } => true,
            Backing::Files { data, index } => [data, index].iter().all(|file| {
                use std::os::unix::fs::MetadataExt;
                file.metadata().expect("snapshot file").nlink() == 0
            }),
        }
    }
    pub fn damage(&self, part: Part, at: u64, bytes: &[u8]) {
        let mut backing = self.backing.write().expect("snapshot backing");
        match &mut *backing {
            Backing::Resident { data, index } => {
                let target = if part == Part::Data { data } else { index };
                let at = at as usize;
                target.resize(target.len().max(at + bytes.len()), 0);
                target[at..at + bytes.len()].copy_from_slice(bytes);
            }
            Backing::Files { data, index } => {
                let target = if part == Part::Data { data } else { index };
                target.write_all_at(bytes, at).expect("damage snapshot");
            }
        }
    }
    pub fn truncate(&self, part: Part, length: u64) {
        let mut backing = self.backing.write().expect("snapshot backing");
        match &mut *backing {
            Backing::Resident { data, index } => {
                let target = if part == Part::Data { data } else { index };
                target.truncate(length as usize);
            }
            Backing::Files { data, index } => {
                let target = if part == Part::Data { data } else { index };
                target.set_len(length).expect("truncate snapshot");
            }
        }
    }
}

#[cfg(test)]
mod tests;
