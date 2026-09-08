//! Version-keyed, bounded on-demand block cache. A cache block is published only
//! after the provider proves the requested version and the file is durable.
#[cfg(test)]
mod durability;
mod sessions;
mod windows;
use crate::private_dir;
use cirrove_core::{CancellationToken, Node, ProviderError, ReadProvider, Scope};
use cirrove_store::Store;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex as StdMutex, Weak},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{Mutex, Semaphore},
};
pub use windows::WindowStats;

pub const BLOCK_SIZE: u32 = 4 * 1024 * 1024;
const MEMORY_BLOCKS: usize = 8;
const RANGE_TIMEOUT: Duration = Duration::from_secs(30);
type MemoryBlocks = HashMap<String, (Arc<Vec<u8>>, Instant)>;
pub struct ContentCache {
    path: PathBuf,
    db: PathBuf,
    quota: u64,
    gates: StdMutex<HashMap<String, Weak<Mutex<()>>>>,
    memory: StdMutex<MemoryBlocks>,
    failures: StdMutex<HashMap<String, (ProviderError, Instant)>>,
    loaders: Semaphore,
    publish: Mutex<()>,
    sessions: sessions::Sessions,
    staging: Arc<windows::Staging>,
}
impl ContentCache {
    pub fn new(path: PathBuf, db: PathBuf, quota: u64) -> anyhow::Result<Self> {
        private_dir(&path)?;
        if quota < BLOCK_SIZE as u64 + 32 {
            anyhow::bail!("cache quota must fit one block");
        }
        let staging = Arc::new(windows::Staging::new(path.clone(), quota));
        let quota = quota - staging.capacity() as u64;
        reconcile(&path, &db, quota)?;
        Ok(Self {
            path,
            db,
            quota,
            gates: StdMutex::new(HashMap::new()),
            memory: StdMutex::new(HashMap::new()),
            failures: StdMutex::new(HashMap::new()),
            loaders: Semaphore::new(4),
            publish: Mutex::new(()),
            sessions: sessions::Sessions::with_staging(staging.clone()),
            staging,
        })
    }
    pub fn window_stats(&self) -> WindowStats {
        self.staging.stats()
    }
    fn gate(&self, key: &str) -> Result<Arc<Mutex<()>>, ProviderError> {
        let mut gates = self.gates.lock().map_err(|_| ProviderError::Unavailable)?;
        if let Some(g) = gates.get(key).and_then(Weak::upgrade) {
            return Ok(g);
        }
        gates.retain(|_, v| v.strong_count() > 0);
        let gate = Arc::new(Mutex::new(()));
        gates.insert(key.into(), Arc::downgrade(&gate));
        Ok(gate)
    }
    fn remember(&self, key: String, bytes: Arc<Vec<u8>>) -> Result<(), ProviderError> {
        let mut memory = self.memory.lock().map_err(|_| ProviderError::Unavailable)?;
        if memory.len() >= MEMORY_BLOCKS
            && !memory.contains_key(&key)
            && let Some(oldest) = memory
                .iter()
                .min_by_key(|(_, (_, t))| *t)
                .map(|(k, _)| k.clone())
        {
            memory.remove(&oldest);
        }
        memory.insert(key, (bytes, Instant::now()));
        Ok(())
    }
    fn recalled(&self, key: &str) -> Result<Option<Arc<Vec<u8>>>, ProviderError> {
        let mut memory = self.memory.lock().map_err(|_| ProviderError::Unavailable)?;
        Ok(memory.get_mut(key).map(|(bytes, time)| {
            *time = Instant::now();
            bytes.clone()
        }))
    }
    pub async fn read(
        &self,
        provider: &dyn ReadProvider,
        scope: &Scope,
        node: &Node,
        offset: u64,
        length: u32,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>, ProviderError> {
        if length > 8 * 1024 * 1024 {
            return Err(ProviderError::Protocol("read exceeds memory budget"));
        }
        if offset >= node.size {
            return Ok(vec![]);
        }
        let length = (node.size - offset).min(length as u64);
        let mut output = Vec::with_capacity(length as usize);
        let end = offset + length;
        let mut position = offset;
        while position < end {
            if cancel.is_cancelled() {
                return Err(ProviderError::Cancelled);
            }
            let start = position / BLOCK_SIZE as u64 * BLOCK_SIZE as u64;
            let bytes = self.block(provider, scope, node, start, cancel).await?;
            let within = (position - start) as usize;
            let amount = (end - position).min(bytes.len().saturating_sub(within) as u64) as usize;
            if amount == 0 {
                return Err(ProviderError::Unavailable);
            }
            output.extend_from_slice(&bytes[within..within + amount]);
            position += amount as u64;
        }
        Ok(output)
    }
    async fn block(
        &self,
        provider: &dyn ReadProvider,
        scope: &Scope,
        node: &Node,
        start: u64,
        cancel: &CancellationToken,
    ) -> Result<Arc<Vec<u8>>, ProviderError> {
        let version = node
            .content_revision()
            .ok_or(ProviderError::Protocol("file has no version tag"))?;
        let identity = serde_json::to_vec(&(scope, &node.id, version, node.size, start))
            .map_err(|_| ProviderError::Unavailable)?;
        let key = hex::encode(Sha256::digest(&identity));
        if let Some(bytes) = self.recalled(&key)? {
            return Ok(bytes);
        }
        let gate = self.gate(&key)?;
        let _guard = tokio::select! {biased;_=cancel.cancelled()=>return Err(ProviderError::Cancelled),g=gate.lock()=>g};
        if let Some(bytes) = self.recalled(&key)? {
            return Ok(bytes);
        }
        {
            let mut failures = self
                .failures
                .lock()
                .map_err(|_| ProviderError::Unavailable)?;
            failures.retain(|_, (_, until)| *until > Instant::now());
            if let Some((error, _)) = failures.get(&key) {
                return Err(error.clone());
            }
        }
        let _loader = tokio::select! {biased;_=cancel.cancelled()=>return Err(ProviderError::Cancelled),p=self.loaders.acquire()=>p.map_err(|_|ProviderError::Unavailable)?};
        let path = self.path.join(&key);
        let expected = (node.size - start).min(BLOCK_SIZE as u64) as usize;
        if let Some(bytes) = read_verified(&path, expected).await {
            let bytes = Arc::new(bytes);
            self.remember(key.clone(), bytes.clone())?;
            self.touch(key, expected as u64 + 32).await?;
            return Ok(bytes);
        }
        // Enforce cancellation/deadlines at the shared service boundary even if
        // an adapter ignores the supplied token. Only provider I/O is abandoned;
        // the subsequent local cache publication is awaited to completion.
        let received = tokio::select! { biased;
            _=cancel.cancelled()=>Err(ProviderError::Cancelled),
            result=tokio::time::timeout(RANGE_TIMEOUT,self.sessions.read(provider,scope,node,start,BLOCK_SIZE,cancel))=>
                result.unwrap_or(Err(ProviderError::Unavailable)),
        };
        let bytes = match received {
            Ok(bytes) => bytes,
            Err(error) => {
                if !matches!(error, ProviderError::Cancelled) {
                    let delay = match &error {
                        ProviderError::Throttled(delay) => *delay,
                        _ => Duration::from_secs(2),
                    };
                    let mut failures = self
                        .failures
                        .lock()
                        .map_err(|_| ProviderError::Unavailable)?;
                    failures.retain(|_, (_, until)| *until > Instant::now());
                    if failures.len() < 256 {
                        failures.insert(key, (error.clone(), Instant::now() + delay));
                    }
                }
                return Err(error);
            }
        };
        if bytes.len() != expected {
            return Err(ProviderError::Protocol(
                "provider returned an incomplete block",
            ));
        }
        let _publish = self.publish.lock().await;
        let tmp = self
            .path
            .join(format!("{key}.{}.tmp", uuid::Uuid::new_v4()));
        let cleanup = TemporaryBlock(tmp.clone());
        let result = async {
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)
                .await?;
            file.write_all(&Sha256::digest(&bytes)).await?;
            file.write_all(&bytes).await?;
            // No fsync here, and none of the directory afterwards. Every read
            // checks the block's length and its leading SHA-256 through
            // read_verified, so a block that a power failure left short or
            // unwritten is rejected and downloaded again, exactly like a block
            // that was never cached. Paying two durable syncs per 4 MiB block
            // would buy nothing but would congest the filesystem that cached
            // navigation shares. Local edits are durable in a separate journal.
            tokio::fs::rename(&tmp, &path).await?;
            Ok::<_, std::io::Error>(())
        }
        .await;
        result.map_err(|_| ProviderError::Unavailable)?;
        drop(cleanup);
        self.touch(key.clone(), bytes.len() as u64 + 32).await?;
        self.evict(&key).await?;
        let bytes = Arc::new(bytes);
        self.remember(key, bytes.clone())?;
        Ok(bytes)
    }
    async fn touch(&self, key: String, size: u64) -> Result<(), ProviderError> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || Store::open(db)?.touch_block(&key, size))
            .await
            .map_err(|_| ProviderError::Unavailable)?
            .map_err(|_| ProviderError::Unavailable)
    }
    async fn evict(&self, keep: &str) -> Result<(), ProviderError> {
        let db = self.db.clone();
        let blocks = tokio::task::spawn_blocking(move || Store::open(db)?.oldest_blocks())
            .await
            .map_err(|_| ProviderError::Unavailable)?
            .map_err(|_| ProviderError::Unavailable)?;
        let mut total: u64 = blocks.iter().map(|(_, size)| size).sum();
        for (key, size) in blocks {
            if total <= self.quota {
                break;
            }
            if key == keep {
                continue;
            }
            match tokio::fs::remove_file(self.path.join(&key)).await {
                Ok(()) => (),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(_) => return Err(ProviderError::Unavailable),
            }
            let db = self.db.clone();
            tokio::task::spawn_blocking(move || Store::open(db)?.forget_block(&key))
                .await
                .map_err(|_| ProviderError::Unavailable)?
                .map_err(|_| ProviderError::Unavailable)?;
            total = total.saturating_sub(size);
        }
        Ok(())
    }
}
fn valid_key(key: &str) -> bool {
    key.len() == 64
        && key
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
/// Reconcile publication interrupted between rename and the LRU transaction.
/// Only Cirrove's block names and temporary files are touched.
fn reconcile(path: &Path, db: &Path, quota: u64) -> anyhow::Result<()> {
    let mut store = Store::open(db)?;
    let mut found = std::collections::HashSet::new();
    let indexed: std::collections::HashSet<_> = store
        .oldest_blocks()?
        .into_iter()
        .map(|(key, _)| key)
        .collect();
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !entry.file_type()?.is_file() {
            continue;
        }
        if valid_key(name) {
            found.insert(name.to_owned());
            if !indexed.contains(name) {
                store.touch_block(name, entry.metadata()?.len())?;
            }
        } else if let Some((key, suffix)) = name.split_once('.')
            && valid_key(key)
            && suffix
                .strip_suffix(".tmp")
                .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
        {
            std::fs::remove_file(entry.path())?;
        }
    }
    for key in indexed.difference(&found) {
        store.forget_block(key)?;
    }
    let blocks = store.oldest_blocks()?;
    let mut total: u64 = blocks.iter().map(|(_, size)| size).sum();
    for (key, size) in blocks {
        if total <= quota {
            break;
        }
        std::fs::remove_file(path.join(&key))?;
        store.forget_block(&key)?;
        total = total.saturating_sub(size);
    }
    Ok(())
}
struct TemporaryBlock(PathBuf);
impl Drop for TemporaryBlock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
async fn read_verified(path: &Path, expected: usize) -> Option<Vec<u8>> {
    let file = tokio::fs::File::open(path).await.ok()?;
    if file.metadata().await.ok()?.len() != expected as u64 + 32 {
        return None;
    }
    let mut body = Vec::with_capacity(expected + 32);
    file.take(expected as u64 + 33)
        .read_to_end(&mut body)
        .await
        .ok()?;
    if body.len() != expected + 32 || Sha256::digest(&body[32..])[..] != body[..32] {
        return None;
    }
    Some(body.split_off(32))
}
