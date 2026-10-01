//! Native-document packages: metadata classification, lazy disk staging and refetch.
use super::*;
use cirrove_core::reads::{ReadIdentity, ReadSession, ReadWindowSink};
use std::{
    collections::VecDeque,
    fs::File,
    os::unix::fs::{DirBuilderExt, FileExt, MetadataExt, PermissionsExt},
    path::PathBuf,
};
use tokio::sync::OwnedSemaphorePermit;

const BLOCK: u32 = 4 * 1024 * 1024;
const VERSION: &str = "icloud-artifact-v2:";
const ITEM: &str = "icloud-artifact:";

type Outcome<T> = Result<T, ProviderError>;

pub(super) struct Packages {
    directory: PathBuf,
    limit: u64,
    transfers: Semaphore,
    storage: Arc<Semaphore>,
    classifications: Mutex<VecDeque<(String, String, u64, bool)>>,
    artifacts: Mutex<VecDeque<(Node, Arc<DiskArtifact>)>>,
}

impl Packages {
    async fn stage_file(&self) -> Outcome<Arc<StagedFile>> {
        let reservation = match self.storage.clone().try_acquire_owned() {
            Ok(reservation) => reservation,
            Err(_) => {
                // Drop cache ownership, never an active reader's file. Do
                // not wait indefinitely for applications to close files.
                self.artifacts.lock().await.clear();
                self.storage
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| ProviderError::Unavailable)?
            }
        };
        let directory = self.directory.clone();
        tokio::task::spawn_blocking(move || {
            let file = private_file(&directory)?;
            Ok::<_, ProviderError>(Arc::new(StagedFile {
                file,
                _reservation: reservation,
            }))
        })
        .await
        .map_err(|_| ProviderError::Unavailable)?
    }
}

#[derive(Serialize, Deserialize)]
struct Revision {
    source_etag: String,
    source_size: u64,
    source_parent: String,
    sha256: String,
}

fn candidate(item: &DriveEntry) -> bool {
    // This selects lookups, never determines whether an item is a package.
    item.kind == "FILE"
        && matches!(
            item.extension.to_ascii_lowercase().as_str(),
            "pages" | "numbers" | "key"
        )
}

pub(super) fn package(node: &Node) -> bool {
    node.package && node.kind == NodeKind::Folder && node.id.starts_with("FILE::")
}

pub(super) fn artifact(node: &Node) -> bool {
    node.id.starts_with(ITEM)
}

impl ICloudDrive {
    /// Enable lazy native-document artifacts in private account storage. The
    /// limit bounds one complete archive; four lifetime reservations bound
    /// all staged archives, including evicted artifacts held by active readers.
    /// The ordinary block-cache budget is separate. No directory or provider
    /// request is made at construction.
    pub fn with_package_artifacts(mut self, state: &Path, limit: u64) -> Outcome<Self> {
        if self.index_mode != IndexMode::OnDemand
            || !state.is_absolute()
            || uuid::Uuid::parse_str(&self.scope.account).is_err()
            || limit == 0
            || limit > crate::MAX_WRITE_FILE_SIZE
        {
            return Err(ProviderError::Permission);
        }
        self.packages = Some(Packages {
            directory: state
                .join("accounts")
                .join(&self.scope.account)
                .join("icloud-artifacts"),
            limit,
            transfers: Semaphore::new(2),
            storage: Arc::new(Semaphore::new(4)),
            classifications: Mutex::new(VecDeque::new()),
            artifacts: Mutex::new(VecDeque::new()),
        });
        Ok(self)
    }

    async fn read_session(&self) -> Outcome<ICloudReadSession> {
        let mut state = self.session.lock().await;
        Ok(Self::active_session(&mut state).await?.read_only_fork())
    }

    async fn classify(
        &self,
        item: Option<&DriveEntry>,
        cancel: &CancellationToken,
    ) -> Outcome<(bool, bool)> {
        let Some(item) = item.filter(|i| candidate(i)) else {
            return Ok((false, false));
        };
        let Some(packages) = &self.packages else {
            return Ok((false, false));
        };
        if item.etag.is_empty() {
            return Err(ProviderError::Protocol("native document has no revision"));
        }
        {
            let cache = packages.classifications.lock().await;
            if let Some((_, _, _, result)) = cache.iter().find(|(id, etag, size, _)| {
                id == &item.drivewsid && etag == &item.etag && *size == item.size
            }) {
                return Ok((*result, false));
            }
        }
        tokio::select! { biased;
            _ = cancel.cancelled() => Err(ProviderError::Cancelled),
            result = async {
                let _permit = self.reads.acquire().await.map_err(|_| ProviderError::Unavailable)?;
                let mut session = self.read_session().await?;
                let representation = session.download_representation(&item.drivewsid).await.map_err(|e| map_read_error(&e))?;
                let result = matches!(representation, crate::ContentRepresentation::Package(_));
                Ok((result, true))
            } => result,
        }
    }

    pub(super) async fn document_nodes(
        &self,
        parent: &str,
        entries: Vec<DriveEntry>,
        cancel: &CancellationToken,
    ) -> Outcome<Vec<Node>> {
        let Some(packages) = &self.packages else {
            return directory_nodes(parent, entries);
        };
        if !entries.iter().any(candidate) {
            return directory_nodes(parent, entries);
        }
        let mut nodes = directory_nodes(parent, entries.clone())?;
        let mut results = Vec::with_capacity(entries.len());
        for chunk in entries.chunks(4) {
            let (a, b, c, d) = tokio::try_join!(
                self.classify(chunk.first(), cancel),
                self.classify(chunk.get(1), cancel),
                self.classify(chunk.get(2), cancel),
                self.classify(chunk.get(3), cancel)
            )?;
            results.extend([a, b, c, d].into_iter().take(chunk.len()));
        }
        // Bind the representation lookup to the same completed folder version.
        // No content is downloaded while listing a folder.
        if results.iter().any(|(_, fresh)| *fresh)
            && fingerprint(&entries)? != fingerprint(&self.list_folder(parent, cancel).await?)?
        {
            return Err(ProviderError::VersionChanged);
        }
        let mut cache = packages.classifications.lock().await;
        for ((item, node), (is_package, _)) in entries.iter().zip(&mut nodes).zip(results) {
            if candidate(item) {
                cache.retain(|(id, _, _, _)| id != &item.drivewsid);
                cache.push_back((
                    item.drivewsid.clone(),
                    item.etag.clone(),
                    item.size,
                    is_package,
                ));
            }
            if is_package {
                node.kind = NodeKind::Folder;
                node.package = true;
                // Keep logical source length for revision validation, never as
                // the generated child's archive length.
            }
        }
        while cache.len() > 1024 {
            cache.pop_front();
        }
        Ok(nodes)
    }

    async fn source(
        &self,
        id: &str,
        parent: &str,
        etag: &str,
        size: u64,
        cancel: &CancellationToken,
    ) -> Outcome<DriveEntry> {
        let entries = self.list_folder(parent, cancel).await?;
        let mut found = entries.into_iter().filter(|i| i.drivewsid == id);
        let source = found.next().ok_or(ProviderError::NotFound)?;
        if found.next().is_some()
            || source.kind != "FILE"
            || etag.is_empty()
            || source.etag != etag
            || source.size != size
        {
            return Err(ProviderError::VersionChanged);
        }
        Ok(source)
    }

    pub(super) async fn package_children(
        &self,
        parent: &Node,
        cancel: &CancellationToken,
    ) -> Outcome<DirectoryPage> {
        if !package(parent) {
            return Err(ProviderError::Permission);
        }
        let source_parent = parent
            .parent_id
            .as_deref()
            .filter(|p| p.starts_with("FOLDER::"))
            .ok_or(ProviderError::Permission)?;
        let etag = parent
            .etag
            .as_deref()
            .ok_or(ProviderError::VersionChanged)?;
        let packages = self.packages.as_ref().ok_or(ProviderError::Unavailable)?;
        {
            let artifacts = packages.artifacts.lock().await;
            for (node, _) in artifacts.iter() {
                let revision = revision(node)?;
                if node.parent_id.as_deref() == Some(parent.id.as_str())
                    && revision.source_etag == etag
                    && revision.source_size == parent.size
                    && revision.source_parent == source_parent
                {
                    let mut node = node.clone();
                    node.name.clone_from(&parent.name);
                    return Ok(DirectoryPage {
                        nodes: vec![node],
                        next: None,
                    });
                }
            }
        }
        let source = self
            .source(&parent.id, source_parent, etag, parent.size, cancel)
            .await?;
        let (node, _) = self
            .materialize(source_parent, &source, None, cancel)
            .await?;
        Ok(DirectoryPage {
            nodes: vec![node],
            next: None,
        })
    }

    async fn materialize(
        &self,
        parent: &str,
        source: &DriveEntry,
        expected: Option<&Node>,
        cancel: &CancellationToken,
    ) -> Outcome<(Node, Arc<DiskArtifact>)> {
        let packages = self.packages.as_ref().ok_or(ProviderError::Unavailable)?;
        tokio::select! { biased;
            _ = cancel.cancelled() => Err(ProviderError::Cancelled),
            result = async {
                let _permit = packages.transfers.acquire().await.map_err(|_| ProviderError::Unavailable)?;
                let reader = packages.stage_file().await?;
                let mut sink = Sink { file: reader.clone(), offset: 0 };
                let mut session = self.read_session().await?;
                let receipt = session.download_package(parent, source, packages.limit, &mut sink, cancel).await.map_err(|e| map_read_error(&e))?;
                drop(sink);
                let token = cancel.clone();
                let (reader, receipt) = tokio::task::spawn_blocking(move || {
                    let receipt = crate::package_archive::canonical_export(&reader, &receipt, &token)?;
                    Ok::<_, ProviderError>((reader, receipt))
                }).await.map_err(|_| ProviderError::Unavailable)??;
                let revision = Revision { source_etag: source.etag.clone(), source_size: source.size, source_parent: parent.into(), sha256: receipt.sha256.clone() };
                let node = Node {
                    id: format!("{ITEM}{}", source.drivewsid), parent_id: Some(source.drivewsid.clone()), name: source.display_name(), kind: NodeKind::File,
                    size: receipt.size, modified_unix: 0, etag: None,
                    content_version: Some(format!("{VERSION}{}", serde_json::to_string(&revision).map_err(|_| ProviderError::Unavailable)?)), target: None, package: false,
                };
                if let Some(expected) = expected
                    && ReadIdentity::new(&self.scope, expected)? != ReadIdentity::new(&self.scope, &node)? {
                    return Err(ProviderError::VersionChanged);
                }
                let identity = ReadIdentity::new(&self.scope, &node)?;
                let disk = tokio::task::spawn_blocking(move || DiskArtifact::new(identity, reader, &receipt.sha256)).await.map_err(|_| ProviderError::Unavailable)??;
                if cancel.is_cancelled() { return Err(ProviderError::Cancelled); }
                let disk = Arc::new(disk);
                let mut artifacts = packages.artifacts.lock().await;
                artifacts.retain(|(old, _)| old.id != node.id);
                artifacts.push_back((node.clone(), disk.clone()));
                while artifacts.len() > 2 { artifacts.pop_front(); }
                Ok((node, disk))
            } => result,
        }
    }

    pub(super) async fn artifact_session(
        &self,
        node: &Node,
        cancel: &CancellationToken,
    ) -> Outcome<Arc<dyn ReadSession>> {
        let expected = ReadIdentity::new(&self.scope, node)?;
        let packages = self.packages.as_ref().ok_or(ProviderError::Unavailable)?;
        {
            let artifacts = packages.artifacts.lock().await;
            if let Some((_, disk)) = artifacts.iter().find(|(_, disk)| disk.identity == expected) {
                return Ok(disk.clone());
            }
        }
        let revision = revision(node)?;
        let id = node
            .id
            .strip_prefix(ITEM)
            .ok_or(ProviderError::Permission)?;
        if node.parent_id.as_deref() != Some(id) || !id.starts_with("FILE::") {
            return Err(ProviderError::Permission);
        }
        let source = self
            .source(
                id,
                &revision.source_parent,
                &revision.source_etag,
                revision.source_size,
                cancel,
            )
            .await?;
        let (_, disk) = self
            .materialize(&revision.source_parent, &source, Some(node), cancel)
            .await?;
        Ok(disk)
    }
}

fn revision(node: &Node) -> Outcome<Revision> {
    let encoded = node
        .content_version
        .as_deref()
        .and_then(|s| s.strip_prefix(VERSION))
        .ok_or(ProviderError::VersionChanged)?;
    let value: Revision =
        serde_json::from_str(encoded).map_err(|_| ProviderError::VersionChanged)?;
    if !value.source_parent.starts_with("FOLDER::")
        || value.source_etag.is_empty()
        || value.sha256.len() != 64
        || !value.sha256.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(ProviderError::VersionChanged);
    }
    Ok(value)
}

fn private_file(directory: &Path) -> Outcome<File> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)
        .map_err(|_| ProviderError::Unavailable)?;
    let metadata = std::fs::symlink_metadata(directory).map_err(|_| ProviderError::Unavailable)?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.uid()
            != std::fs::metadata("/proc/self")
                .map_err(|_| ProviderError::Unavailable)?
                .uid()
    {
        return Err(ProviderError::Permission);
    }
    // Anonymous file: crashes and last-reader drop release it without a cleanup
    // traversal. No artifact names or provider URLs are persisted here.
    tempfile::tempfile_in(directory).map_err(|_| ProviderError::Unavailable)
}

// Keep the permit beside the only File, not beside a cache entry or session.
// Blocking tasks retain this Arc even when their async waiter is cancelled.
struct StagedFile {
    file: File,
    _reservation: OwnedSemaphorePermit,
}
impl std::ops::Deref for StagedFile {
    type Target = File;
    fn deref(&self) -> &File {
        &self.file
    }
}
struct Sink {
    file: Arc<StagedFile>,
    offset: u64,
}
#[async_trait]
impl ReadWindowSink for Sink {
    async fn write_chunk(&mut self, bytes: &[u8]) -> Outcome<()> {
        if bytes.len() > 64 * 1024 {
            return Err(ProviderError::Protocol("oversized package chunk"));
        }
        let (file, offset, bytes) = (self.file.clone(), self.offset, bytes.to_vec());
        let length = bytes.len() as u64;
        tokio::task::spawn_blocking(move || file.write_all_at(&bytes, offset))
            .await
            .map_err(|_| ProviderError::Unavailable)?
            .map_err(|_| ProviderError::Unavailable)?;
        self.offset += length;
        Ok(())
    }
}

struct DiskArtifact {
    identity: ReadIdentity,
    file: Arc<StagedFile>,
    hashes: Arc<Vec<[u8; 32]>>,
}
impl DiskArtifact {
    fn new(identity: ReadIdentity, file: Arc<StagedFile>, sha256: &str) -> Outcome<Self> {
        if file
            .metadata()
            .map_err(|_| ProviderError::Unavailable)?
            .len()
            != identity.size
        {
            return Err(ProviderError::VersionChanged);
        }
        let mut hashes = Vec::new();
        let mut hash = Sha256::new();
        let mut start = 0;
        while start < identity.size {
            let count = (identity.size - start).min(u64::from(BLOCK)) as usize;
            let mut block = vec![0; count];
            file.read_exact_at(&mut block, start)
                .map_err(|_| ProviderError::Unavailable)?;
            hashes.push(Sha256::digest(&block).into());
            hash.update(&block);
            start += count as u64;
        }
        if hex::encode(hash.finalize()) != sha256 {
            return Err(ProviderError::VersionChanged);
        }
        Ok(Self {
            identity,
            file,
            hashes: Arc::new(hashes),
        })
    }
}
#[async_trait]
impl ReadSession for DiskArtifact {
    fn identity(&self) -> &ReadIdentity {
        &self.identity
    }
    async fn read_range(
        &self,
        offset: u64,
        length: u32,
        cancel: &CancellationToken,
    ) -> Outcome<Vec<u8>> {
        if length > BLOCK {
            return Err(ProviderError::Protocol("artifact range exceeds bound"));
        }
        let (file, hashes, size) = (self.file.clone(), self.hashes.clone(), self.identity.size);
        tokio::select! { biased;
            _ = cancel.cancelled() => Err(ProviderError::Cancelled),
            result = tokio::task::spawn_blocking(move || {
                let end = offset.saturating_add(u64::from(length)).min(size);
                let mut position = offset; let mut result = Vec::new();
                while position < end {
                    let index = position / u64::from(BLOCK); let start = index * u64::from(BLOCK);
                    let mut block = vec![0; (size-start).min(u64::from(BLOCK)) as usize];
                    file.read_exact_at(&mut block,start).map_err(|_| ProviderError::Unavailable)?;
                    if Sha256::digest(&block)[..] != hashes[index as usize] { return Err(ProviderError::VersionChanged); }
                    let within = (position-start) as usize; let count = (end-position).min((block.len()-within) as u64) as usize;
                    result.extend_from_slice(&block[within..within+count]); position += count as u64;
                }
                Ok(result)
            }) => result.map_err(|_| ProviderError::Unavailable)?,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    async fn fixture(
        state: &Path,
        changed: bool,
    ) -> (
        ICloudDrive,
        Scope,
        Arc<AtomicUsize>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let lookups = Arc::new(AtomicUsize::new(0));
        let count = lookups.clone();
        let server = tokio::spawn(async move {
            let mut listings = 0;
            loop {
                let (mut peer, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                loop {
                    let mut bytes = [0; 4096];
                    let n = peer.read(&mut bytes).await.unwrap();
                    assert!(n > 0 && request.len() < 32 * 1024);
                    request.extend_from_slice(&bytes[..n]);
                    if request.windows(4).any(|x| x == b"\r\n\r\n") {
                        break;
                    }
                }
                let header = std::str::from_utf8(&request).unwrap();
                let body = if header.starts_with("GET /ws/com.apple.CloudDocs/download/by_id?") {
                    count.fetch_add(1, Ordering::SeqCst);
                    let field = if header.contains("document_id=package ") {
                        "package_token"
                    } else {
                        "data_token"
                    };
                    serde_json::json!({field:{"url":"https://fixture.icloud-content.com/archive"}})
                } else {
                    assert!(header.starts_with("POST /retrieveItemDetailsInFolders"));
                    listings += 1;
                    let etag = if changed && listings > 1 { "v2" } else { "v1" };
                    let items = [
                        serde_json::json!({"drivewsid":"FILE::com.apple.CloudDocs::package","type":"FILE","name":"Package","extension":"pages","etag":etag,"size":42}),
                        serde_json::json!({"drivewsid":"FILE::com.apple.CloudDocs::ordinary","type":"FILE","name":"Ordinary","extension":"pages","etag":"v1","size":12}),
                        serde_json::json!({"drivewsid":"FILE::com.apple.CloudDocs::pdf","type":"FILE","name":"Plain","extension":"pdf","etag":"v1","size":12}),
                        serde_json::json!({"drivewsid":"FOLDER::com.apple.CloudDocs::folder","type":"FOLDER","name":"Folder","extension":"pages","etag":"v1"}),
                    ];
                    serde_json::json!([{"drivewsid":ROOT_ID,"type":"FOLDER","numberOfItems":4,"items":items}])
                };
                let data = serde_json::to_vec(&body).unwrap();
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    data.len()
                );
                peer.write_all(header.as_bytes()).await.unwrap();
                peer.write_all(&data).await.unwrap();
            }
        });
        let mut session = ICloudReadSession::new().unwrap();
        session.drive_endpoint = Some(format!("http://{address}/").parse().unwrap());
        session.docs_endpoint = session.drive_endpoint.clone();
        let scope = Scope {
            account: uuid::Uuid::new_v4().to_string(),
            provider: "icloud".into(),
            collection: "drive".into(),
        };
        let drive = ICloudDrive::on_demand_from_live_session(scope.clone(), session)
            .unwrap()
            .with_package_artifacts(state, 64 * 1024 * 1024)
            .unwrap();
        (drive, scope, lookups, server)
    }

    #[tokio::test]
    async fn staging_reservation_outlives_cache_eviction_and_cancelled_waiters() {
        let temp = tempfile::tempdir().unwrap();
        let (drive, scope, _, server) = fixture(temp.path(), false).await;
        let packages = drive.packages.as_ref().unwrap();
        let mut readers = Vec::new();
        for index in 0..4 {
            let file = packages.stage_file().await.unwrap();
            file.write_all_at(b"data", 0).unwrap();
            let identity = ReadIdentity {
                scope: scope.clone(),
                item: index.to_string(),
                revision_namespace: "content".into(),
                revision: "r1".into(),
                size: 4,
            };
            let disk = Arc::new(
                DiskArtifact::new(identity, file, &hex::encode(Sha256::digest(b"data"))).unwrap(),
            );
            let node = Node {
                id: index.to_string(),
                parent_id: None,
                name: "artifact".into(),
                kind: NodeKind::File,
                size: 4,
                modified_unix: 0,
                etag: None,
                content_version: Some("r1".into()),
                target: None,
                package: false,
            };
            let mut cache = packages.artifacts.lock().await;
            cache.push_back((node, disk.clone()));
            while cache.len() > 2 {
                cache.pop_front();
            }
            readers.push(disk);
        }
        assert_eq!(packages.artifacts.lock().await.len(), 2);
        // Admission evicts the remaining cache owners. Four live sessions still
        // retain their files and must prevent a fifth allocation.
        assert!(matches!(
            packages.stage_file().await,
            Err(ProviderError::Unavailable)
        ));
        assert!(packages.artifacts.lock().await.is_empty());
        assert_eq!(
            readers[0]
                .read_range(0, 4, &CancellationToken::new())
                .await
                .unwrap(),
            b"data"
        );
        let reader = readers.pop().unwrap();
        let file = reader.file.clone();
        let (release, wait) = std::sync::mpsc::channel();
        let (started, ready) = tokio::sync::oneshot::channel();
        let task = tokio::task::spawn_blocking(move || {
            started.send(()).unwrap();
            wait.recv().unwrap();
            let mut bytes = [0; 4];
            file.read_exact_at(&mut bytes, 0).unwrap();
            assert_eq!(&bytes, b"data");
        });
        ready.await.unwrap();
        drop(reader);
        task.abort(); // A running blocking read keeps the file even after cancellation.
        assert!(matches!(
            packages.stage_file().await,
            Err(ProviderError::Unavailable)
        ));
        release.send(()).unwrap();
        task.await.unwrap();
        let replacement = packages.stage_file().await.unwrap();
        assert!(matches!(
            packages.stage_file().await,
            Err(ProviderError::Unavailable)
        ));
        drop(replacement);
        drop(readers);
        assert_eq!(packages.storage.available_permits(), 4);
        assert!(
            std::fs::read_dir(&packages.directory)
                .unwrap()
                .next()
                .is_none()
        );
        server.abort();
    }

    #[tokio::test]
    async fn normal_listing_confirms_packages_without_downloading_or_misclassifying_names() {
        let temp = tempfile::tempdir().unwrap();
        let (drive, scope, lookups, server) = fixture(temp.path(), false).await;
        let page = drive
            .children(&scope, ROOT_ID, None, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(page.nodes.len(), 4);
        assert!(package(&page.nodes[0]));
        assert_eq!(page.nodes[0].size, 42);
        assert_eq!(page.nodes[1].kind, NodeKind::File);
        assert!(!page.nodes[1].package);
        assert_eq!(page.nodes[2].kind, NodeKind::File);
        assert!(!page.nodes[2].package);
        assert_eq!(page.nodes[3].kind, NodeKind::Folder);
        assert!(!page.nodes[3].package);
        assert_eq!(lookups.load(Ordering::SeqCst), 2);
        let repeated = drive
            .children(&scope, ROOT_ID, None, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(repeated.nodes, page.nodes);
        assert_eq!(lookups.load(Ordering::SeqCst), 2);
        let mut foreign = scope;
        foreign.account = uuid::Uuid::new_v4().to_string();
        assert!(matches!(
            drive
                .children(&foreign, ROOT_ID, None, &CancellationToken::new())
                .await,
            Err(ProviderError::Permission)
        ));
        assert!(
            !temp.path().join("accounts").exists(),
            "listing must not stage content"
        );
        server.abort();
    }

    #[tokio::test]
    async fn changed_folder_does_not_commit_package_classification() {
        let temp = tempfile::tempdir().unwrap();
        let (drive, scope, _, server) = fixture(temp.path(), true).await;
        assert!(matches!(
            drive
                .children(&scope, ROOT_ID, None, &CancellationToken::new())
                .await,
            Err(ProviderError::VersionChanged)
        ));
        assert!(
            drive
                .packages
                .as_ref()
                .unwrap()
                .classifications
                .lock()
                .await
                .is_empty()
        );
        server.abort();
    }

    #[tokio::test]
    async fn disk_artifact_checks_cross_block_reads_and_refuses_changed_private_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("artifacts");
        let file = private_file(&directory).unwrap();
        let bytes: Vec<u8> = (0..BLOCK as usize + 17).map(|v| (v % 251) as u8).collect();
        file.write_all_at(&bytes, 0).unwrap();
        let identity = ReadIdentity {
            scope: Scope {
                account: "a".into(),
                provider: "icloud".into(),
                collection: "drive".into(),
            },
            item: "file".into(),
            revision_namespace: "content".into(),
            revision: "r1".into(),
            size: bytes.len() as u64,
        };
        let file = Arc::new(StagedFile {
            file,
            _reservation: Arc::new(Semaphore::new(1)).try_acquire_owned().unwrap(),
        });
        let disk = DiskArtifact::new(identity, file, &hex::encode(Sha256::digest(&bytes))).unwrap();
        let offset = BLOCK as u64 - 5;
        assert_eq!(
            disk.read_range(offset, 22, &CancellationToken::new())
                .await
                .unwrap(),
            bytes[BLOCK as usize - 5..]
        );
        disk.file.write_all_at(b"changed", 0).unwrap();
        assert!(matches!(
            disk.read_range(0, 7, &CancellationToken::new()).await,
            Err(ProviderError::VersionChanged)
        ));
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(matches!(
            disk.read_range(offset, 1, &cancel).await,
            Err(ProviderError::Cancelled)
        ));
        assert!(
            std::fs::read_dir(&directory).unwrap().next().is_none(),
            "staging must not leave named files"
        );
    }
}
