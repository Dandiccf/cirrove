//! Immutable, process-local directory listings with bounded read buffers. Files
//! are anonymous, so closing the final handle or killing the process reclaims
//! them without an orphan scan. These are never local edits or restart state.
use std::{
    fs::File,
    io::{self, BufWriter, Write},
    os::unix::fs::FileExt,
    path::Path,
    sync::{Arc, Mutex},
};

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
    bytes: u64,
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
        let reservation = Reservation {
            budget: self.clone(),
            bytes: 0,
        };
        Ok(Builder {
            data: BufWriter::with_capacity(PAGE_BYTES, tempfile::tempfile_in(directory)?),
            index: BufWriter::with_capacity(PAGE_ENTRIES * 8, tempfile::tempfile_in(directory)?),
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
    fn grow(&mut self, bytes: u64) -> io::Result<()> {
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
        self.bytes += bytes;
        Ok(())
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if let Ok(mut usage) = self.budget.0.lock() {
            usage.bytes -= self.bytes;
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
    reservation: Reservation,
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
        self.failed = result.is_err();
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
    pub fn finish(mut self) -> io::Result<Snapshot> {
        if self.failed {
            return Err(invalid());
        }
        self.data.flush()?;
        self.index.flush()?;
        Ok(Snapshot {
            data: self.data.into_inner().map_err(|e| e.into_error())?,
            index: self.index.into_inner().map_err(|e| e.into_error())?,
            _reservation: self.reservation,
            entries: self.entries,
            data_bytes: self.data_bytes,
        })
    }
}
pub(super) struct Snapshot {
    data: File,
    index: File,
    _reservation: Reservation,
    entries: u64,
    data_bytes: u64,
}
impl Snapshot {
    #[cfg(test)]
    pub fn len(&self) -> u64 {
        self.entries
    }
    /// Cookies are ordinal positions. Positioned reads let independent kernel
    /// requests use one immutable snapshot without a shared seek cursor/lock.
    pub fn page(&self, offset: u64) -> io::Result<Vec<Entry>> {
        if offset >= self.entries {
            return Ok(Vec::new());
        }
        let count = (self.entries - offset).min(PAGE_ENTRIES as u64) as usize;
        let extra = usize::from(offset + (count as u64) < self.entries);
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
            positions.push(self.data_bytes);
        }
        let start = positions[0];
        let mut take = 0;
        for pair in positions.windows(2) {
            let length = pair[1].checked_sub(pair[0]).ok_or_else(invalid)?;
            if length < HEADER_BYTES as u64
                || length > PAGE_BYTES as u64
                || pair[1] > self.data_bytes
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
