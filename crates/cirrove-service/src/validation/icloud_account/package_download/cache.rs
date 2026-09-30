//! Cache-only replay of a completely downloaded, source-checked artifact.
use super::*;
use crate::content::{BLOCK_SIZE, ContentCache};
use cirrove_core::{
    ChangePage, Cursor, DirectoryPage, MetadataProvider, ReadProvider,
    reads::{ReadIdentity, ReadSession},
};
use std::{
    fs::File,
    os::unix::fs::FileExt,
    sync::atomic::{AtomicUsize, Ordering},
};

struct DiskArtifact {
    identity: ReadIdentity,
    file: Arc<File>,
    hashes: Arc<Vec<[u8; 32]>>,
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
    ) -> std::result::Result<Vec<u8>, ProviderError> {
        if length > BLOCK_SIZE {
            return Err(ProviderError::Protocol("artifact range exceeds bound"));
        }
        let (file, hashes, size) = (self.file.clone(), self.hashes.clone(), self.identity.size);
        tokio::select! { biased;
            _ = cancel.cancelled() => Err(ProviderError::Cancelled),
            result = tokio::task::spawn_blocking(move || {
                let end = offset.saturating_add(u64::from(length)).min(size);
                let mut position = offset;
                let mut result = Vec::new();
                while position < end {
                    let index = position / u64::from(BLOCK_SIZE);
                    let start = index * u64::from(BLOCK_SIZE);
                    let count = (size-start).min(u64::from(BLOCK_SIZE)) as usize;
                    let mut block = vec![0;count];
                    file.read_exact_at(&mut block,start).map_err(|_| ProviderError::Unavailable)?;
                    if Sha256::digest(&block)[..] != hashes[index as usize] { return Err(ProviderError::Protocol("private artifact checksum mismatch")); }
                    let within = (position-start) as usize;
                    let count = (end-position).min((block.len()-within) as u64) as usize;
                    result.extend_from_slice(&block[within..within+count]); position += count as u64;
                }
                Ok(result)
            }) => result.map_err(|_| ProviderError::Unavailable)?,
        }
    }
}

#[derive(Default)]
struct Offline(AtomicUsize);
#[async_trait]
impl MetadataProvider for Offline {
    fn provider_id(&self) -> &'static str {
        "icloud"
    }
    async fn changes(
        &self,
        _: &Scope,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<ChangePage, ProviderError> {
        Err(ProviderError::Unavailable)
    }
}
#[async_trait]
impl ReadProvider for Offline {
    async fn node(
        &self,
        _: &Scope,
        _: &str,
        _: &CancellationToken,
    ) -> std::result::Result<Node, ProviderError> {
        Err(ProviderError::Unavailable)
    }
    async fn children(
        &self,
        _: &Scope,
        _: &str,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<DirectoryPage, ProviderError> {
        Err(ProviderError::Unavailable)
    }
    async fn read_range(
        &self,
        _: &Scope,
        _: &Node,
        _: u64,
        _: u32,
        _: &CancellationToken,
    ) -> std::result::Result<Vec<u8>, ProviderError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(ProviderError::Unavailable)
    }
}

pub(super) async fn verify_and_cache(
    run_dir: &Path,
    run: Uuid,
    source: &str,
    artifact: &cirrove_icloud::PackageDownload,
) -> Result<(Scope, Node, Arc<dyn ReadSession>)> {
    let scope = Scope {
        account: run.to_string(),
        provider: "icloud".into(),
        collection: "drive".into(),
    };
    let node = Node {
        id: format!("icloud-package:{source}"),
        parent_id: Some(ROOT_ID.into()),
        name: "Package Preview.pages".into(),
        kind: NodeKind::File,
        size: artifact.size,
        modified_unix: 0,
        etag: None,
        content_version: Some(format!("icloud-package-sha256:{}", artifact.sha256)),
        target: None,
        package: false,
    };
    let identity = ReadIdentity::new(&scope, &node)?;
    let path = run_dir.join("artifact.package");
    let digest = artifact.sha256.clone();
    let session = tokio::task::spawn_blocking(move || -> Result<DiskArtifact> {
        let file = File::open(path)?;
        ensure!(
            file.metadata()?.len() == identity.size,
            "private artifact size differs"
        );
        let mut hashes = Vec::new();
        let mut hash = Sha256::new();
        let mut start = 0;
        while start < identity.size {
            let count = (identity.size - start).min(u64::from(BLOCK_SIZE)) as usize;
            let mut block = vec![0; count];
            file.read_exact_at(&mut block, start)?;
            hashes.push(Sha256::digest(&block).into());
            hash.update(&block);
            start += count as u64;
        }
        ensure!(
            hex::encode(hash.finalize()) == digest,
            "private artifact digest differs"
        );
        Ok(DiskArtifact {
            identity,
            file: Arc::new(file),
            hashes: Arc::new(hashes),
        })
    })
    .await??;
    let session = Arc::new(session);
    record(&run_dir.join("cache-node.json"), &node)?;
    let cache_path = run_dir.join("shared-cache");
    let index = run_dir.join("cache-blocks.sqlite");
    let cache = ContentCache::new(cache_path.clone(), index.clone(), 64 * 1024 * 1024)?;
    cache
        .stage_session(&scope, &node, session.as_ref(), &CancellationToken::new())
        .await?;
    drop(cache);
    let cache = ContentCache::new(cache_path, index, 64 * 1024 * 1024)?;
    let offline = Offline::default();
    let mut hash = Sha256::new();
    let mut start = 0;
    while start < node.size {
        let count = (node.size - start).min(u64::from(BLOCK_SIZE)) as u32;
        let bytes = cache
            .read(
                &offline,
                &scope,
                &node,
                start,
                count,
                &CancellationToken::new(),
            )
            .await?;
        ensure!(bytes.len() == count as usize, "short reopened cache read");
        hash.update(&bytes);
        start += u64::from(count);
    }
    ensure!(
        hex::encode(hash.finalize()) == artifact.sha256 && offline.0.load(Ordering::SeqCst) == 0,
        "cache replay differs or used the provider"
    );
    Ok((scope, node, session))
}
