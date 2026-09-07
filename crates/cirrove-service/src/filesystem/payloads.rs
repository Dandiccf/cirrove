//! Immutable projection records in an anonymous account-local file. A bounded
//! payload cache and reusable power-of-two extents bound memory and file growth.
//! File I/O runs on blocking workers outside the namespace lock.
use super::{Node, Projection, Scope};
use sha2::{Digest, Sha256};
#[cfg(test)]
use std::os::unix::fs::MetadataExt;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{self, Write},
    os::unix::fs::{FileExt, PermissionsExt},
    path::Path,
    sync::{Arc, Mutex},
};
const CACHE_BYTES: usize = 16 * 1024 * 1024;
const RECORD_BYTES: usize = 4 * 1024 * 1024;
const RECORDS: usize = 1_000_000;
const RETIRE_BATCH: usize = 4096;
fn failure() -> io::Error {
    io::Error::other("projection storage unavailable")
}
fn full() -> io::Error {
    io::Error::from_raw_os_error(libc::ENOSPC)
}
struct Encoded {
    bytes: Vec<u8>,
    full: bool,
}
impl Write for Encoded {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > RECORD_BYTES.saturating_sub(self.bytes.len()) {
            self.full = true;
            return Err(full());
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
type Stored = (
    Scope,
    Node,
    String,
    Vec<(String, String)>,
    bool,
    Option<Node>,
    Vec<(String, String)>,
);
impl Projection {
    fn encode(&self) -> io::Result<Vec<u8>> {
        let mut output = Encoded {
            bytes: Vec::new(),
            full: false,
        };
        let result = serde_json::to_writer(
            &mut output,
            &(
                self.scope.as_ref(),
                self.node.as_ref(),
                self.name.as_ref(),
                self.alias.as_ref(),
                self.reference,
                self.entry.as_deref(),
                self.ancestry.as_ref(),
            ),
        );
        if result.is_err() {
            return Err(if output.full { full() } else { failure() });
        }
        Ok(output.bytes)
    }
    fn decode(bytes: &[u8]) -> io::Result<Self> {
        let (scope, node, name, alias, reference, entry, ancestry): Stored =
            serde_json::from_slice(bytes).map_err(|_| failure())?;
        Ok(Self {
            scope: scope.into(),
            node: node.into(),
            name: name.into(),
            alias: alias.into(),
            reference,
            entry: entry.map(Arc::new),
            ancestry: ancestry.into(),
        })
    }
    /// Requested payload bytes, including Arc headers. Shared allocations are
    /// conservatively charged to every cache entry that uses them. Allocator and
    /// cache-index overhead, open/operation clones and compact headers are extra.
    fn charge(&self) -> usize {
        const ARC: usize = 2 * std::mem::size_of::<usize>();
        fn node(n: &Node) -> usize {
            std::mem::size_of::<Node>()
                + n.id.capacity()
                + n.name.capacity()
                + n.parent_id.as_ref().map_or(0, String::capacity)
                + n.etag.as_ref().map_or(0, String::capacity)
                + n.content_version.as_ref().map_or(0, String::capacity)
                + n.target
                    .as_ref()
                    .map_or(0, |t| t.collection.capacity() + t.item.capacity())
        }
        fn route(v: &Vec<(String, String)>) -> usize {
            std::mem::size_of::<Vec<(String, String)>>()
                + v.capacity() * std::mem::size_of::<(String, String)>()
                + v.iter()
                    .map(|(a, b)| a.capacity() + b.capacity())
                    .sum::<usize>()
        }
        std::mem::size_of::<Self>()
            + ARC
            + std::mem::size_of::<Scope>()
            + ARC
            + self.scope.account.capacity()
            + self.scope.provider.capacity()
            + self.scope.collection.capacity()
            + node(&self.node)
            + ARC
            + self.name.len()
            + ARC
            + route(&self.alias)
            + ARC
            + route(&self.ancestry)
            + ARC
            + self.entry.as_ref().map_or(0, |n| node(n) + ARC)
    }
}
struct Cached {
    data: Arc<Projection>,
    bytes: usize,
    age: u64,
}
struct State {
    file: File,
    free: Vec<BTreeSet<u32>>,
    arena_order: u32,
    reserved: u64,
    next_id: u64,
    cache: BTreeMap<u64, Cached>,
    order: BTreeSet<(u64, u64)>,
    bytes: usize,
    clock: u64,
    rows: usize,
    cache_limit: usize,
    row_limit: usize,
}
impl State {
    fn remove_cached(&mut self, id: u64) {
        if let Some(old) = self.cache.remove(&id) {
            self.order.remove(&(old.age, id));
            self.bytes -= old.bytes;
        }
    }
    fn cache(&mut self, id: u64, data: Arc<Projection>) {
        self.remove_cached(id);
        let bytes = data.charge();
        if bytes > self.cache_limit {
            return;
        }
        while self.bytes + bytes > self.cache_limit {
            let Some((_, old)) = self.order.first().copied() else {
                break;
            };
            self.remove_cached(old);
        }
        if self.clock == u64::MAX {
            self.cache.clear();
            self.order.clear();
            self.bytes = 0;
            self.clock = 0;
        }
        self.clock += 1;
        let age = self.clock;
        self.bytes += bytes;
        self.order.insert((age, id));
        self.cache.insert(id, Cached { data, bytes, age });
    }
}
#[derive(Clone, Copy)]
struct Slot {
    offset: u32,
    order: u32,
}
#[derive(Clone, Copy)]
struct Retired {
    id: u64,
    slot: Slot,
}
const HEADER_BYTES: usize = 36;
const MIN_ORDER: u32 = 9;

impl State {
    fn allocate(&mut self, bytes: usize) -> io::Result<Slot> {
        let capacity = bytes.checked_next_power_of_two().ok_or_else(full)?;
        let order = capacity.trailing_zeros().max(MIN_ORDER);
        if order > self.arena_order {
            return Err(full());
        }
        let available = (order..=self.arena_order)
            .find(|&n| !self.free[n as usize].is_empty())
            .ok_or_else(full)?;
        let offset = self.free[available as usize]
            .pop_first()
            .ok_or_else(failure)?;
        for split in (order..available).rev() {
            self.free[split as usize].insert(offset + (1u32 << split));
        }
        self.reserved += 1u64 << order;
        Ok(Slot { offset, order })
    }
    fn release(&mut self, slot: Slot) {
        self.reserved -= 1u64 << slot.order;
        let (mut offset, mut order) = (slot.offset, slot.order);
        while order < self.arena_order {
            let buddy = offset ^ (1u32 << order);
            if !self.free[order as usize].remove(&buddy) {
                break;
            }
            offset = offset.min(buddy);
            order += 1;
        }
        self.free[order as usize].insert(offset);
    }
}
pub(super) struct Store {
    state: Mutex<State>,
    retired: Mutex<Vec<Retired>>,
}
/// Only a live record authorizes reading its immutable extent. Its monotonic ID
/// also prevents cache reuse when a retired extent is assigned to a new record.
pub(super) struct Record {
    pub store: Arc<Store>,
    id: u64,
    slot: Slot,
}
impl Record {
    pub fn load(&self) -> io::Result<Arc<Projection>> {
        self.store.load(self)
    }
}
impl Drop for Record {
    fn drop(&mut self) {
        if let Ok(mut retired) = self.store.retired.lock() {
            retired.push(Retired {
                id: self.id,
                slot: self.slot,
            });
        }
    }
}
impl Store {
    pub fn new(directory: &Path) -> io::Result<Arc<Self>> {
        Self::with_file(
            tempfile::tempfile_in(directory)?,
            CACHE_BYTES,
            RECORDS,
            1 << 30,
        )
    }
    #[cfg(test)]
    pub(super) fn limited(
        cache_limit: usize,
        row_limit: usize,
        pages: u32,
    ) -> io::Result<Arc<Self>> {
        Self::with_file(
            tempfile::tempfile()?,
            cache_limit,
            row_limit,
            u64::from(pages) * 4096,
        )
    }
    fn with_file(
        file: File,
        cache_limit: usize,
        row_limit: usize,
        bytes: u64,
    ) -> io::Result<Arc<Self>> {
        if !bytes.is_power_of_two() || !(512..=1 << 30).contains(&bytes) {
            return Err(failure());
        }
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        let arena_order = bytes.trailing_zeros();
        let mut free = vec![BTreeSet::new(); arena_order as usize + 1];
        free[arena_order as usize].insert(0);
        Ok(Arc::new(Self {
            state: Mutex::new(State {
                file,
                free,
                arena_order,
                reserved: 0,
                next_id: 0,
                cache: BTreeMap::new(),
                order: BTreeSet::new(),
                bytes: 0,
                clock: 0,
                rows: 0,
                cache_limit,
                row_limit,
            }),
            retired: Mutex::new(Vec::new()),
        }))
    }
    fn retire(&self, state: &mut State) -> io::Result<()> {
        let records = {
            let mut pending = self.retired.lock().map_err(|_| failure())?;
            let start = pending.len().saturating_sub(RETIRE_BATCH);
            pending.split_off(start)
        };
        for record in records {
            state.remove_cached(record.id);
            state.release(record.slot);
            state.rows = state.rows.checked_sub(1).ok_or_else(failure)?;
        }
        Ok(())
    }
    pub fn collect(&self) -> io::Result<()> {
        let mut state = self.state.lock().map_err(|_| failure())?;
        self.retire(&mut state)
    }
    pub fn save(self: &Arc<Self>, data: Arc<Projection>) -> io::Result<Record> {
        let body = data.encode()?;
        let digest = Sha256::digest(&body);
        let mut header = [0u8; HEADER_BYTES];
        header[..4].copy_from_slice(&u32::try_from(body.len()).map_err(|_| full())?.to_le_bytes());
        header[4..].copy_from_slice(&digest);
        let mut state = self.state.lock().map_err(|_| failure())?;
        self.retire(&mut state)?;
        if state.rows >= state.row_limit {
            return Err(full());
        }
        let id = state.next_id.checked_add(1).ok_or_else(failure)?;
        let slot = state.allocate(body.len() + HEADER_BYTES)?;
        let offset = u64::from(slot.offset);
        let written = state
            .file
            .write_all_at(&header, offset)
            .and_then(|()| state.file.write_all_at(&body, offset + HEADER_BYTES as u64));
        if let Err(error) = written {
            state.release(slot);
            return Err(error);
        }
        state.next_id = id;
        state.rows += 1;
        state.cache(id, data);
        Ok(Record {
            store: self.clone(),
            id,
            slot,
        })
    }
    fn load(&self, record: &Record) -> io::Result<Arc<Projection>> {
        let mut state = self.state.lock().map_err(|_| failure())?;
        if let Some(data) = state.cache.get(&record.id).map(|v| v.data.clone()) {
            state.cache(record.id, data.clone());
            return Ok(data);
        }
        let mut header = [0u8; HEADER_BYTES];
        let offset = u64::from(record.slot.offset);
        state.file.read_exact_at(&mut header, offset)?;
        let bytes = u32::from_le_bytes(header[..4].try_into().map_err(|_| failure())?) as usize;
        if bytes > RECORD_BYTES || bytes + HEADER_BYTES > (1usize << record.slot.order) {
            return Err(failure());
        }
        let mut body = vec![0; bytes];
        state
            .file
            .read_exact_at(&mut body, offset + HEADER_BYTES as u64)?;
        if Sha256::digest(&body).as_slice() != &header[4..] {
            return Err(failure());
        }
        let data = Arc::new(Projection::decode(&body)?);
        state.cache(record.id, data.clone());
        Ok(data)
    }
    #[cfg(test)]
    #[allow(clippy::unwrap_used)]
    pub fn diagnostics(&self) -> serde_json::Value {
        let state = self.state.lock().unwrap();
        let file = state.file.metadata().unwrap();
        serde_json::json!({"cache_accounted_bytes":state.bytes,"cache_limit_bytes":state.cache_limit,
            "cached_payloads":state.cache.len(),"live_and_pending_rows":state.rows,"row_limit":state.row_limit,
            "reserved_extent_bytes":state.reserved,"file_limit_bytes":1u64 << state.arena_order,
            "file_length_bytes":file.len(),"file_allocated_bytes":file.blocks()*512,
            "pending_retirement":self.retired.lock().unwrap().len()})
    }
    #[cfg(test)]
    #[allow(clippy::unwrap_used)]
    pub fn usage(&self) -> (usize, usize, usize) {
        let state = self.state.lock().unwrap();
        (state.bytes, state.cache.len(), state.rows)
    }
    #[cfg(test)]
    #[allow(clippy::unwrap_used)]
    pub fn disable_cache(&self) {
        let mut state = self.state.lock().unwrap();
        state.cache_limit = 0;
        state.cache.clear();
        state.order.clear();
        state.bytes = 0;
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use cirrove_core::{NodeKind, RemoteRef};

    fn projection(name: &str) -> Arc<Projection> {
        let node = Node {
            id: "target-item".into(),
            parent_id: Some("parent".into()),
            name: name.into(),
            kind: NodeKind::File,
            size: 1024,
            modified_unix: 1,
            etag: Some("original-etag".into()),
            content_version: Some("original-content".into()),
            target: None,
        };
        let mut entry = node.clone();
        entry.id = "shortcut-item".into();
        entry.kind = NodeKind::Shortcut;
        entry.target = Some(RemoteRef {
            collection: "target-drive".into(),
            item: node.id.clone(),
            kind: Some(NodeKind::File),
        });
        Arc::new(Projection {
            scope: Arc::new(Scope {
                account: "account".into(),
                provider: "fixture".into(),
                collection: "target-drive".into(),
            }),
            node: Arc::new(node),
            name: name.into(),
            alias: Arc::new(vec![("source-drive".into(), "shortcut-item".into())]),
            ancestry: Arc::new(vec![("source-drive".into(), "root".into())]),
            reference: true,
            entry: Some(Arc::new(entry)),
        })
    }

    #[test]
    fn eviction_restores_complete_immutable_versions_and_routes() {
        let old = projection("old-name");
        let budget = old.charge();
        let store = Store::limited(budget, 4, 512).unwrap();
        let first = store.save(old.clone()).unwrap();
        let second = store.save(projection("new-name")).unwrap();
        assert_eq!(store.usage(), (budget, 1, 2));
        assert!(!store.state.lock().unwrap().cache.contains_key(&first.id));
        let restored = first.load().unwrap();
        assert!(!Arc::ptr_eq(&old, &restored));
        assert_eq!(old.encode().unwrap(), restored.encode().unwrap());
        assert_eq!(store.usage(), (budget, 1, 2));
        drop(first);
        store.collect().unwrap();
        assert_eq!(
            restored.node.content_version.as_deref(),
            Some("original-content")
        );
        assert_eq!(store.usage().2, 1);
        assert_eq!(second.load().unwrap().name.as_ref(), "new-name");
        drop(second);
        store.collect().unwrap();
        assert_eq!(store.usage(), (0, 0, 0));
    }

    #[test]
    fn live_records_enforce_quota_and_retirement_reuses_admission() {
        let store = Store::limited(0, 1, 512).unwrap();
        let first = store.save(projection("original")).unwrap();
        let error = store.save(projection("replacement")).err().unwrap();
        assert_eq!(error.raw_os_error(), Some(libc::ENOSPC));
        assert_eq!(first.load().unwrap().name.as_ref(), "original");
        let id = first.id;
        // A record drop only queues cleanup, even while the storage is locked.
        let state = store.state.lock().unwrap();
        drop(first);
        drop(state);
        let replacement = store.save(projection("replacement")).unwrap();
        assert_ne!(id, replacement.id);
        assert!(!store.state.lock().unwrap().cache.contains_key(&id));
        assert_eq!(store.usage(), (0, 0, 1));
    }

    #[test]
    fn corrupt_record_never_enters_cache() {
        let store = Store::limited(0, 4, 512).unwrap();
        let record = store.save(projection("original")).unwrap();
        store
            .state
            .lock()
            .unwrap()
            .file
            .write_all_at(b"!", u64::from(record.slot.offset) + HEADER_BYTES as u64)
            .unwrap();
        assert!(record.load().is_err());
        assert_eq!(store.usage().0, 0);
    }

    #[test]
    fn disk_and_record_limits_preserve_previously_admitted_data() {
        let store = Store::limited(0, 10, 16).unwrap();
        let first = store.save(projection("original")).unwrap();
        let error = store
            .save(projection(&"x".repeat(128 * 1024)))
            .err()
            .unwrap();
        assert_eq!(error.raw_os_error(), Some(libc::ENOSPC));
        assert_eq!(store.usage().2, 1);
        assert_eq!(first.load().unwrap().name.as_ref(), "original");
        let error = store
            .save(projection(&"x".repeat(RECORD_BYTES)))
            .err()
            .unwrap();
        assert_eq!(error.raw_os_error(), Some(libc::ENOSPC));
        assert_eq!(store.usage().2, 1);
    }
    #[test]
    fn anonymous_file_uses_requested_filesystem_and_closes_without_orphans() {
        use std::os::fd::AsRawFd;
        let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let store = Store::new(directory.path()).unwrap();
        let record = store.save(projection("original")).unwrap();
        let (path, inode) = {
            let state = store.state.lock().unwrap();
            let metadata = state.file.metadata().unwrap();
            assert_eq!(
                metadata.dev(),
                std::fs::metadata(directory.path()).unwrap().dev()
            );
            assert_eq!(metadata.mode() & 0o777, 0o600);
            assert_eq!(metadata.nlink(), 0);
            (
                format!("/proc/self/fd/{}", state.file.as_raw_fd()),
                metadata.ino(),
            )
        };
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        drop(record);
        drop(store);
        assert!(!std::fs::metadata(path).is_ok_and(|m| m.ino() == inode));
    }
    #[test]
    fn retired_small_extents_coalesce_for_larger_records() {
        let store = Store::limited(0, 32, 4).unwrap();
        let mut records = Vec::new();
        while let Ok(record) = store.save(projection("small")) {
            records.push(record);
        }
        assert!(records.len() > 4);
        let allocated = store.diagnostics()["reserved_extent_bytes"]
            .as_u64()
            .unwrap();
        assert_eq!(allocated, 16384);
        drop(records);
        store.collect().unwrap();
        assert_eq!(store.diagnostics()["reserved_extent_bytes"], 0);
        let big = store.save(projection(&"b".repeat(1500))).unwrap();
        assert_eq!(big.load().unwrap().name.len(), 1500);
        assert!(store.diagnostics()["file_length_bytes"].as_u64().unwrap() <= 16384);
    }
    #[test]
    fn repeated_size_changes_reuse_space_without_overwriting_live_records() {
        let store = Store::limited(0, 64, 256).unwrap();
        let mut live = Vec::new();
        let mut random = 12345u32;
        for round in 0..512 {
            random = random.wrapping_mul(1664525).wrapping_add(1013904223);
            let name = format!("{round}-{}", "x".repeat(random as usize % 4096));
            if live.len() == 32 {
                drop(live.remove(random as usize % live.len()));
            }
            let record = store.save(projection(&name)).unwrap();
            live.push((record, name));
            for (record, expected) in live.iter().step_by(7) {
                assert_eq!(record.load().unwrap().name.as_ref(), expected);
            }
            assert!(store.diagnostics()["file_length_bytes"].as_u64().unwrap() <= 1024 * 1024);
        }
        drop(live);
        store.collect().unwrap();
        let state = store.state.lock().unwrap();
        assert_eq!(state.reserved, 0);
        assert_eq!(state.free[state.arena_order as usize], BTreeSet::from([0]));
        assert!(
            state.free[..state.arena_order as usize]
                .iter()
                .all(BTreeSet::is_empty)
        );
    }
}
