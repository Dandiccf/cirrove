//! File-lifetime reservations for native write staging, separate from the journal.
//! Share one budget across an account's fresh and restored adapters. This is not
//! a host-wide quota and does not account for other independently created budgets.
use cirrove_core::ProviderError;
use std::{
    fs::{File, Metadata},
    io,
    os::unix::fs::{FileExt, MetadataExt, PermissionsExt},
    path::{Component, Path},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, ReadBuf},
    sync::{OwnedSemaphorePermit, Semaphore},
    task::JoinHandle,
};
use tokio_util::io::ReaderStream;

pub const WRITE_STAGING_FILE_LIMIT: u64 = 64 * 1024 * 1024;
const CHUNK: usize = 64 * 1024;
const SLOTS: usize = 4;
type Result<T> = std::result::Result<T, ProviderError>;

#[derive(Clone)]
pub struct WriteStagingBudget {
    slots: Arc<Semaphore>,
}
impl Default for WriteStagingBudget {
    fn default() -> Self {
        Self::new()
    }
}
impl WriteStagingBudget {
    pub fn new() -> Self {
        Self {
            slots: Arc::new(Semaphore::new(SLOTS)),
        }
    }

    /// Refuse pressure immediately. The blocking creator owns the reservation
    /// even if its async waiter disappears before file creation completes.
    pub(crate) async fn create(&self, directory: &Path) -> Result<Arc<WriteStagingFile>> {
        let reservation = self.reserve()?;
        let directory = directory.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let before = private_directory(&directory)?;
            let file = tempfile::tempfile_in(&directory).map_err(|_| ProviderError::Unavailable)?;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|_| ProviderError::Unavailable)?;
            let after = private_directory(&directory)?;
            if before.dev() != after.dev() || before.ino() != after.ino() {
                return Err(ProviderError::Permission);
            }
            validate_empty_file(&file)?;
            Ok(Arc::new(WriteStagingFile::new(file, reservation)))
        })
        .await
        .map_err(|_| ProviderError::Unavailable)?
    }

    /// Compatibility for a caller-owned anonymous, empty Trash-probe sink.
    /// Consumes the descriptor; the guarded native path never extracts it again.
    pub(crate) fn adopt(&self, file: File) -> Result<Arc<WriteStagingFile>> {
        let reservation = self.reserve()?;
        validate_empty_file(&file)?;
        Ok(Arc::new(WriteStagingFile::new(file, reservation)))
    }

    fn reserve(&self) -> Result<OwnedSemaphorePermit> {
        self.slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| ProviderError::Unavailable)
    }

    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub async fn hold_synthetic_file(
        &self,
        directory: &Path,
    ) -> Result<impl Send + Sync + 'static> {
        self.create(directory).await
    }

    #[cfg(test)]
    fn available_permits(&self) -> usize {
        self.slots.available_permits()
    }
}

fn private_directory(directory: &Path) -> Result<Metadata> {
    if !directory.is_absolute() {
        return Err(ProviderError::Permission);
    }
    let mut path = std::path::PathBuf::from("/");
    for part in directory.components() {
        match part {
            Component::RootDir => {}
            Component::Normal(name) => path.push(name),
            _ => return Err(ProviderError::Permission),
        }
        let metadata = std::fs::symlink_metadata(&path).map_err(|_| ProviderError::Unavailable)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(ProviderError::Permission);
        }
    }
    let metadata = std::fs::symlink_metadata(directory).map_err(|_| ProviderError::Unavailable)?;
    if metadata.uid() != owner()? || metadata.permissions().mode() & 0o777 != 0o700 {
        return Err(ProviderError::Permission);
    }
    Ok(metadata)
}
fn owner() -> Result<u32> {
    std::fs::metadata("/proc/self")
        .map(|m| m.uid())
        .map_err(|_| ProviderError::Unavailable)
}
fn validate_empty_file(file: &File) -> Result<()> {
    let metadata = file.metadata().map_err(|_| ProviderError::Unavailable)?;
    if !metadata.is_file()
        || metadata.nlink() != 0
        || metadata.len() != 0
        || metadata.uid() != owner()?
        || metadata.permissions().mode() & 0o777 != 0o600
    {
        return Err(ProviderError::Permission);
    }
    Ok(())
}
fn bound(offset: u64, length: usize) -> Result<()> {
    if length > CHUNK
        || offset
            .checked_add(length as u64)
            .is_none_or(|end| end > WRITE_STAGING_FILE_LIMIT)
    {
        return Err(ProviderError::Protocol(
            "native write staging limit exceeded",
        ));
    }
    Ok(())
}

pub(crate) struct WriteStagingFile {
    file: File,
    _reservation: OwnedSemaphorePermit,
    #[cfg(test)]
    read_probe: std::sync::Mutex<Option<ReadProbe>>,
}
impl WriteStagingFile {
    fn new(file: File, reservation: OwnedSemaphorePermit) -> Self {
        Self {
            file,
            _reservation: reservation,
            #[cfg(test)]
            read_probe: std::sync::Mutex::new(None),
        }
    }

    /// Crate-internal only: use inside a guard-owned blocking closure. Do not
    /// clone, return or otherwise retain a raw descriptor outside that closure.
    pub(crate) fn borrowed_file(&self) -> &File {
        &self.file
    }

    /// Blocking copy callers use this guard before every actual positional write.
    pub(crate) fn write_chunk_at(&self, offset: u64, bytes: &[u8]) -> Result<()> {
        bound(offset, bytes.len())?;
        self.file
            .write_all_at(bytes, offset)
            .map_err(|_| ProviderError::Unavailable)
    }

    pub(crate) async fn write_at(self: &Arc<Self>, offset: u64, bytes: Vec<u8>) -> Result<()> {
        bound(offset, bytes.len())?;
        self.clone()
            .blocking(move |file| file.write_chunk_at(offset, &bytes))
            .await
    }

    #[cfg(test)]
    pub(crate) async fn read_at(self: &Arc<Self>, offset: u64, length: u32) -> Result<Vec<u8>> {
        bound(offset, length as usize)?;
        let file = self.clone();
        tokio::task::spawn_blocking(move || file.read_chunk_at(offset, length as usize))
            .await
            .map_err(|_| ProviderError::Unavailable)?
            .map_err(|_| ProviderError::Unavailable)
    }

    /// The closure owns this Arc until its actual terminal result, regardless of
    /// cancellation of the JoinHandle waiter. It must not export raw descriptors.
    pub(crate) async fn blocking<T, E, F>(self: Arc<Self>, action: F) -> std::result::Result<T, E>
    where
        T: Send + 'static,
        E: From<ProviderError> + Send + 'static,
        F: FnOnce(&Self) -> std::result::Result<T, E> + Send + 'static,
    {
        tokio::task::spawn_blocking(move || action(&self))
            .await
            .map_err(|_| E::from(ProviderError::Unavailable))?
    }

    /// The body and every detached blocking read retain the same file owner.
    pub(crate) fn body(self: Arc<Self>, size: u64) -> Result<reqwest::Body> {
        Ok(reqwest::Body::wrap_stream(ReaderStream::with_capacity(
            self.reader(size)?,
            CHUNK,
        )))
    }
    fn reader(self: Arc<Self>, size: u64) -> Result<FileReader> {
        if size > WRITE_STAGING_FILE_LIMIT
            || self
                .file
                .metadata()
                .map_err(|_| ProviderError::Unavailable)?
                .len()
                != size
        {
            return Err(ProviderError::Protocol(
                "native write staging length differs",
            ));
        }
        Ok(FileReader {
            file: self,
            offset: 0,
            size,
            pending: None,
            buffered: Vec::new(),
            buffered_at: 0,
        })
    }
    fn read_chunk_at(&self, offset: u64, length: usize) -> io::Result<Vec<u8>> {
        bound(offset, length)
            .map_err(|_| io::Error::other("native write staging limit exceeded"))?;
        #[cfg(test)]
        if let Some(probe) = self
            .read_probe
            .lock()
            .map_err(|_| io::Error::other("staging probe unavailable"))?
            .take()
        {
            let _ = probe.entered.send(());
            probe
                .release
                .recv_timeout(std::time::Duration::from_secs(5))
                .map_err(|_| io::Error::other("staging probe timed out"))?;
        }
        let mut bytes = vec![0; length];
        let count = self.file.read_at(&mut bytes, offset)?;
        bytes.truncate(count);
        Ok(bytes)
    }
}

struct FileReader {
    file: Arc<WriteStagingFile>,
    offset: u64,
    size: u64,
    pending: Option<JoinHandle<io::Result<Vec<u8>>>>,
    buffered: Vec<u8>,
    buffered_at: usize,
}
impl AsyncRead for FileReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if buffer.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        if self.buffered_at < self.buffered.len() {
            let count = buffer
                .remaining()
                .min(self.buffered.len() - self.buffered_at);
            buffer.put_slice(&self.buffered[self.buffered_at..self.buffered_at + count]);
            self.buffered_at += count;
            self.offset += count as u64;
            if self.buffered_at == self.buffered.len() {
                self.buffered.clear();
                self.buffered_at = 0;
            }
            return Poll::Ready(Ok(()));
        }
        if self.offset == self.size {
            return Poll::Ready(Ok(()));
        }
        if self.pending.is_none() {
            let count = buffer
                .remaining()
                .min(CHUNK)
                .min((self.size - self.offset) as usize);
            let file = self.file.clone();
            let offset = self.offset;
            self.pending = Some(tokio::task::spawn_blocking(move || {
                file.read_chunk_at(offset, count)
            }));
        }
        let Some(pending) = self.pending.as_mut() else {
            return Poll::Ready(Err(io::Error::other("native staging reader unavailable")));
        };
        let bytes = match std::future::Future::poll(Pin::new(pending), cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(result) => {
                self.pending = None;
                match result {
                    Ok(Ok(bytes)) => bytes,
                    Ok(Err(error)) => return Poll::Ready(Err(error)),
                    Err(_) => {
                        return Poll::Ready(Err(io::Error::other(
                            "native staging read unavailable",
                        )));
                    }
                }
            }
        };
        if bytes.is_empty() || bytes.len() as u64 > self.size - self.offset {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "native staging length changed",
            )));
        }
        self.buffered = bytes;
        self.buffered_at = 0;
        self.as_mut().poll_read(cx, buffer)
    }
}

#[cfg(test)]
struct ReadProbe {
    entered: tokio::sync::oneshot::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;
    fn directory() -> tempfile::TempDir {
        tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap()
    }
    async fn released(budget: &WriteStagingBudget, expected: usize) {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while budget.available_permits() != expected {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn shared_budget_refuses_fifth_file_until_final_owner_closes() {
        let dir = directory();
        let budget = WriteStagingBudget::new();
        let sibling = budget.clone();
        let mut files = Vec::new();
        for _ in 0..SLOTS {
            files.push(budget.create(dir.path()).await.unwrap());
        }
        let held = files[0].clone();
        files[0]
            .write_at(0, b"held archive".to_vec())
            .await
            .unwrap();
        assert!(matches!(
            sibling.create(dir.path()).await,
            Err(ProviderError::Unavailable)
        ));
        drop(files.remove(0));
        assert!(matches!(
            sibling.create(dir.path()).await,
            Err(ProviderError::Unavailable)
        ));
        assert_eq!(held.read_at(0, 12).await.unwrap(), b"held archive");
        drop(held);
        let replacement = sibling.create(dir.path()).await.unwrap();
        assert_eq!(budget.available_permits(), 0);
        drop(replacement);
        drop(files);
        assert_eq!(budget.available_permits(), SLOTS);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn aborted_semantic_waiter_keeps_file_reservation_until_blocking_owner_finishes() {
        let dir = directory();
        let budget = WriteStagingBudget::new();
        let file = budget.create(dir.path()).await.unwrap();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let task = tokio::spawn(file.clone().blocking(move |owner| {
            entered.send(()).unwrap();
            wait.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            assert_eq!(owner.borrowed_file().metadata().unwrap().nlink(), 0);
            Ok::<_, ProviderError>(())
        }));
        tokio::time::timeout(std::time::Duration::from_secs(2), ready)
            .await
            .unwrap()
            .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        drop(file);
        assert_eq!(budget.available_permits(), SLOTS - 1);
        let mut remaining = Vec::new();
        for _ in 0..SLOTS - 1 {
            remaining.push(budget.create(dir.path()).await.unwrap());
        }
        assert!(matches!(
            budget.create(dir.path()).await,
            Err(ProviderError::Unavailable)
        ));
        release.send(()).unwrap();
        released(&budget, 1).await;
        drop(remaining);
        assert_eq!(budget.available_permits(), SLOTS);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn aborted_body_reader_keeps_reservation_in_detached_blocking_read() {
        let dir = directory();
        let budget = WriteStagingBudget::new();
        let file = budget.create(dir.path()).await.unwrap();
        file.write_at(0, b"body".to_vec()).await.unwrap();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        *file.read_probe.lock().unwrap() = Some(ReadProbe {
            entered,
            release: wait,
        });
        let mut reader = file.clone().reader(4).unwrap();
        let task = tokio::spawn(async move {
            let mut bytes = [0; 4];
            reader.read_exact(&mut bytes).await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), ready)
            .await
            .unwrap()
            .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        drop(file);
        assert_eq!(budget.available_permits(), SLOTS - 1);
        release.send(()).unwrap();
        released(&budget, SLOTS).await;
    }

    #[tokio::test]
    async fn chunk_and_archive_limits_refuse_before_writing_and_body_checks_exact_size() {
        let dir = directory();
        let budget = WriteStagingBudget::new();
        let file = budget.create(dir.path()).await.unwrap();
        assert!(file.write_at(0, vec![0; CHUNK + 1]).await.is_err());
        assert!(
            file.write_at(WRITE_STAGING_FILE_LIMIT, vec![1])
                .await
                .is_err()
        );
        assert!(file.write_at(u64::MAX, vec![1]).await.is_err());
        assert!(file.read_at(0, CHUNK as u32 + 1).await.is_err());
        assert_eq!(file.borrowed_file().metadata().unwrap().len(), 0);
        file.write_at(0, b"exact".to_vec()).await.unwrap();
        assert!(file.clone().body(4).is_err());
        let body = file.clone().body(5).unwrap();
        drop(file);
        assert_eq!(budget.available_permits(), SLOTS - 1);
        drop(body);
        assert_eq!(budget.available_permits(), SLOTS);
    }

    #[tokio::test]
    async fn body_reader_preserves_multiple_chunks_and_small_consumer_buffers() {
        let dir = directory();
        let budget = WriteStagingBudget::new();
        let file = budget.create(dir.path()).await.unwrap();
        let expected: Vec<u8> = (0..CHUNK * 2 + 19).map(|n| (n % 251) as u8).collect();
        for (index, chunk) in expected.chunks(CHUNK).enumerate() {
            file.write_at((index * CHUNK) as u64, chunk.to_vec())
                .await
                .unwrap();
        }
        let mut reader = file.clone().reader(expected.len() as u64).unwrap();
        drop(file);
        let mut actual = Vec::new();
        for size in [7, CHUNK + 11, 3, CHUNK * 2] {
            let mut buffer = vec![0; size];
            let count = reader.read(&mut buffer).await.unwrap();
            assert!(count <= CHUNK);
            actual.extend_from_slice(&buffer[..count]);
        }
        reader.read_to_end(&mut actual).await.unwrap();
        assert_eq!(actual, expected);
        assert_eq!(reader.read(&mut [0; 1]).await.unwrap(), 0);
        assert_eq!(budget.available_permits(), SLOTS - 1);
        drop(reader);
        assert_eq!(budget.available_permits(), SLOTS);
    }

    #[tokio::test]
    async fn symlink_directory_named_nonempty_and_nonprivate_adoptions_are_refused() {
        let dir = directory();
        let budget = WriteStagingBudget::new();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(dir.path(), &link).unwrap();
        assert!(matches!(
            budget.create(&link).await,
            Err(ProviderError::Permission)
        ));
        assert!(matches!(
            budget.create(Path::new("relative")).await,
            Err(ProviderError::Permission)
        ));
        assert!(
            budget
                .adopt(File::create(dir.path().join("named")).unwrap())
                .is_err()
        );
        let file = tempfile::tempfile_in(dir.path()).unwrap();
        file.set_len(1).unwrap();
        assert!(budget.adopt(file).is_err());
        let file = tempfile::tempfile_in(dir.path()).unwrap();
        file.set_permissions(std::fs::Permissions::from_mode(0o644))
            .unwrap();
        assert!(budget.adopt(file).is_err());
        assert_eq!(budget.available_permits(), SLOTS);
    }
}
