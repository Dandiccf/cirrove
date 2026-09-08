//! A cache block carries its own SHA-256, so a block that a power failure left
//! short or unwritten is rejected and downloaded again. That is what allows the
//! publication path to skip the two durable syncs per block which would
//! otherwise congest the filesystem cached navigation shares.
#![allow(clippy::unwrap_used)]
use super::*;
use cirrove_core::{
    Change, ChangePage, Checkpoint, Cursor, DirectoryPage, MetadataProvider, NodeKind,
};
use std::sync::atomic::{AtomicU64, Ordering};

struct CountingProvider {
    bytes: Vec<u8>,
    reads: AtomicU64,
}
#[async_trait::async_trait]
impl MetadataProvider for CountingProvider {
    fn provider_id(&self) -> &'static str {
        "fixture"
    }
    async fn changes(
        &self,
        _: &Scope,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<ChangePage, ProviderError> {
        Ok(ChangePage {
            changes: Vec::<Change>::new(),
            checkpoint: Checkpoint::Complete(Cursor("fixture".into())),
        })
    }
}
#[async_trait::async_trait]
impl ReadProvider for CountingProvider {
    async fn node(
        &self,
        _: &Scope,
        _: &str,
        _: &CancellationToken,
    ) -> std::result::Result<Node, ProviderError> {
        Err(ProviderError::NotFound)
    }
    async fn children(
        &self,
        _: &Scope,
        _: &str,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<DirectoryPage, ProviderError> {
        Ok(DirectoryPage {
            nodes: vec![],
            next: None,
        })
    }
    async fn read_range(
        &self,
        _: &Scope,
        _: &Node,
        offset: u64,
        length: u32,
        _: &CancellationToken,
    ) -> std::result::Result<Vec<u8>, ProviderError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let start = offset as usize;
        let end = (start + length as usize).min(self.bytes.len());
        Ok(self.bytes[start..end].to_vec())
    }
}

fn node(size: u64) -> Node {
    Node {
        id: "block-file".into(),
        parent_id: Some("root".into()),
        name: "block-file.bin".into(),
        kind: NodeKind::File,
        size,
        modified_unix: 0,
        etag: Some("1".into()),
        content_version: Some("v1".into()),
        target: None,
    }
}

#[tokio::test]
async fn a_block_that_did_not_survive_is_rejected_and_fetched_again() {
    let temp = tempfile::tempdir().unwrap();
    let cache_dir = temp.path().join("cache");
    let db = temp.path().join("metadata.db");
    let keeper = Store::open(&db).unwrap();
    let size = 96 * 1024u64;
    let provider = CountingProvider {
        bytes: (0..size).map(|i| (i % 251) as u8).collect(),
        reads: AtomicU64::new(0),
    };
    let scope = Scope {
        account: "account".into(),
        provider: "fixture".into(),
        collection: "drive".into(),
    };
    let node = node(size);
    let cancel = CancellationToken::new();
    let open = || ContentCache::new(cache_dir.clone(), db.clone(), 64 * 1024 * 1024).unwrap();

    let cache = open();
    let first = cache
        .read(&provider, &scope, &node, 0, size as u32, &cancel)
        .await
        .unwrap();
    assert_eq!(first.len(), size as usize);
    assert_eq!(provider.reads.load(Ordering::SeqCst), 1);

    // A fresh cache has no resident block, so this must come from the file.
    let cache = open();
    let repeated = cache
        .read(&provider, &scope, &node, 0, size as u32, &cancel)
        .await
        .unwrap();
    assert_eq!(repeated, first);
    assert_eq!(
        provider.reads.load(Ordering::SeqCst),
        1,
        "a published block must be served without another request"
    );

    for damage in ["truncate", "flip"] {
        let block = std::fs::read_dir(&cache_dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.is_file())
            .expect("a published block");
        let mut bytes = std::fs::read(&block).unwrap();
        // Exactly the two shapes a crash without fsync can leave behind: a file
        // shorter than it should be, or one whose bytes never reached the disk.
        if damage == "truncate" {
            bytes.truncate(bytes.len() / 2);
        } else {
            let last = bytes.len() - 1;
            bytes[last] ^= 0xff;
        }
        std::fs::write(&block, &bytes).unwrap();

        let before = provider.reads.load(Ordering::SeqCst);
        let cache = open();
        let recovered = cache
            .read(&provider, &scope, &node, 0, size as u32, &cancel)
            .await
            .unwrap();
        assert_eq!(
            recovered, first,
            "{damage} must not change what a reader sees"
        );
        assert_eq!(
            provider.reads.load(Ordering::SeqCst),
            before + 1,
            "{damage} must force exactly one fresh request"
        );
    }
    drop(keeper);
}
