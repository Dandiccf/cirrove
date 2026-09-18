//! A pin only means something if the mount that evicts acts on it. These check
//! the seam between the registry and the cache, which is where a pin recorded
//! correctly and ignored completely would otherwise look identical to one that
//! works.
#![allow(clippy::unwrap_used)]
use super::discovery::{LinkedLibrary, fixture_account};
use super::*;
use crate::jobs::Job;

/// Serves deterministic bytes for one file, so a materialised pin can be checked
/// against blocks that actually exist rather than against an empty cache.
#[derive(Default)]
struct OneFile {
    reads: AtomicU64,
    offline: AtomicBool,
}
impl OneFile {
    fn node(size: u64) -> Node {
        Node {
            package: false,
            id: "pinned-file".into(),
            parent_id: Some("root".into()),
            name: "pinned".into(),
            kind: cirrove_core::NodeKind::File,
            size,
            modified_unix: 0,
            etag: Some("\"v1\"".into()),
            content_version: Some("v1".into()),
            target: None,
        }
    }
}
#[async_trait::async_trait]
impl cirrove_core::MetadataProvider for OneFile {
    fn provider_id(&self) -> &'static str {
        "fixture"
    }
    async fn changes(
        &self,
        _: &Scope,
        _: Option<&cirrove_core::Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<cirrove_core::ChangePage, ProviderError> {
        Ok(cirrove_core::ChangePage {
            changes: vec![],
            checkpoint: cirrove_core::Checkpoint::Complete(cirrove_core::Cursor("done".into())),
        })
    }
}
#[async_trait::async_trait]
impl ReadProvider for OneFile {
    async fn node(
        &self,
        _: &Scope,
        id: &str,
        _: &CancellationToken,
    ) -> std::result::Result<Node, ProviderError> {
        if id == "pinned-file" {
            Ok(Self::node(0))
        } else {
            Err(ProviderError::NotFound)
        }
    }
    async fn children(
        &self,
        _: &Scope,
        _: &str,
        _: Option<&cirrove_core::Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<cirrove_core::DirectoryPage, ProviderError> {
        Err(ProviderError::NotFound)
    }
    async fn read_range(
        &self,
        _: &Scope,
        node: &Node,
        offset: u64,
        length: u32,
        _: &CancellationToken,
    ) -> std::result::Result<Vec<u8>, ProviderError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.offline.load(Ordering::SeqCst) {
            return Err(ProviderError::Unavailable);
        }
        let end = (offset + length as u64).min(node.size);
        Ok((offset..end).map(|i| (i % 251) as u8).collect())
    }
}
async fn engine(temp: &tempfile::TempDir, budget: u64) -> Arc<Engine> {
    let provider = Arc::new(LinkedLibrary {
        linked: AtomicBool::new(false),
        primary_changes: AtomicU64::new(0),
        ..Default::default()
    });
    let mut account = fixture_account(temp.path().join("mount"));
    account.cache_bytes = budget;
    Engine::new(account, provider, temp.path().join("engine"))
        .await
        .unwrap()
}
fn folder(id: &str) -> Node {
    Node {
        package: false,
        id: id.into(),
        parent_id: Some("root".into()),
        name: id.into(),
        kind: cirrove_core::NodeKind::Folder,
        size: 0,
        modified_unix: 0,
        etag: Some("\"v1\"".into()),
        content_version: None,
        target: None,
    }
}
fn file(id: &str, parent: &str, size: u64) -> Node {
    Node {
        package: false,
        id: id.into(),
        parent_id: Some(parent.into()),
        name: id.into(),
        kind: cirrove_core::NodeKind::File,
        size,
        modified_unix: 0,
        etag: Some("\"v1\"".into()),
        content_version: Some("v1".into()),
        target: None,
    }
}
async fn seed_directory(engine: &Arc<Engine>, parent: &str, children: &[Node]) {
    let db = engine.db.clone();
    let (parent, children) = (parent.to_string(), children.to_vec());
    tokio::task::spawn_blocking(move || {
        let mut store = cirrove_store::Store::open(db).unwrap();
        store
            .observe_directory(&scope(), &parent, &children)
            .unwrap();
    })
    .await
    .unwrap();
}
fn scope() -> Scope {
    Scope {
        account: "00000000-0000-4000-8000-000000000015".into(),
        provider: "onedrive".into(),
        collection: "primary".into(),
    }
}

#[tokio::test]
async fn a_pin_reaches_the_cache_that_evicts() {
    let temp = tempfile::tempdir().unwrap();
    let engine = engine(&temp, 64 * 1024 * 1024).await;
    let reserved = 8 * 1024 * 1024;
    engine
        .pin(scope(), "item".into(), false, reserved)
        .await
        .unwrap()
        .expect("the budget holds this");
    // Recording the pin is not the property under test. Eviction reads a view
    // beside the cache, so a pin the engine never published would be a row in a
    // table and nothing else.
    assert_eq!(
        engine.cache.reservations().lock().unwrap().reserved,
        reserved
    );
}
#[tokio::test]
async fn a_refused_pin_changes_nothing() {
    let temp = tempfile::tempdir().unwrap();
    let budget = 16 * 1024 * 1024;
    let engine = engine(&temp, budget).await;
    let refusal = engine
        .pin(scope(), "too-large".into(), false, budget * 2)
        .await
        .unwrap()
        .expect_err("twice the budget cannot fit");
    assert!(matches!(
        refusal,
        cirrove_store::pins::PinRefusal::WouldExceedBudget { .. }
    ));
    assert_eq!(
        engine.cache.reservations().lock().unwrap().reserved,
        0,
        "a refusal must not shrink the budget it declined to claim"
    );
}
#[tokio::test]
async fn a_restart_republishes_pins_before_anything_can_evict() {
    let temp = tempfile::tempdir().unwrap();
    let reserved = 8 * 1024 * 1024;
    {
        let engine = engine(&temp, 64 * 1024 * 1024).await;
        engine
            .pin(scope(), "item".into(), true, reserved)
            .await
            .unwrap()
            .expect("fits");
        engine.stop().await;
    }
    // The registry is durable, the cache's view is not: it is rebuilt in memory.
    // Publishing it has to happen at construction rather than at start, because
    // the cache's reconcile pass evicts to fit the quota while it is being built.
    // A view published even one step later would arrive after the blocks it was
    // meant to protect had already been deleted.
    let engine = engine(&temp, 64 * 1024 * 1024).await;
    assert_eq!(
        engine.cache.reservations().lock().unwrap().reserved,
        reserved,
        "a restarted mount must know what is pinned before anything can evict"
    );
    engine.start().await.unwrap();
    assert_eq!(
        engine.cache.reservations().lock().unwrap().reserved,
        reserved,
        "and starting must not lose it"
    );
    engine.stop().await;
}
#[tokio::test]
async fn unpinning_releases_the_budget_at_the_cache() {
    let temp = tempfile::tempdir().unwrap();
    let engine = engine(&temp, 64 * 1024 * 1024).await;
    engine
        .pin(scope(), "item".into(), false, 8 * 1024 * 1024)
        .await
        .unwrap()
        .expect("fits");
    assert!(engine.unpin(scope(), "item".into()).await.unwrap());
    assert_eq!(engine.cache.reservations().lock().unwrap().reserved, 0);
    assert!(
        !engine.unpin(scope(), "item".into()).await.unwrap(),
        "unpinning what is not pinned reports that rather than pretending"
    );
}

#[tokio::test]
async fn materialising_a_pin_fetches_its_blocks_and_protects_exactly_those() {
    let temp = tempfile::tempdir().unwrap();
    let provider = Arc::new(OneFile::default());
    let mut account = fixture_account(temp.path().join("mount"));
    account.cache_bytes = 64 * 1024 * 1024;
    let engine = Engine::new(account, provider.clone(), temp.path().join("engine"))
        .await
        .unwrap();
    // Two full blocks and a short third, so an off-by-one in the key walk shows
    // up as a count rather than passing on a size that divides evenly.
    let size = 2 * crate::content::BLOCK_SIZE as u64 + 1024;
    let node = OneFile::node(size);
    engine
        .pin(scope(), node.id.clone(), false, size)
        .await
        .unwrap()
        .expect("fits");
    let blocks = engine.materialise_pin(&scope(), &node).await.unwrap();
    assert_eq!(blocks, 3, "a partial last block is still a block");
    assert!(
        provider.reads.load(Ordering::SeqCst) >= 3,
        "content was fetched"
    );
    let reservations = engine.cache.reservations();
    let view = reservations.lock().unwrap();
    assert_eq!(view.protected.len(), 3);
    // Named with the cache's own derivation. A protected set built any other way
    // would cover keys nothing writes, and would look identical to no protection.
    for start in [
        0,
        crate::content::BLOCK_SIZE as u64,
        2 * crate::content::BLOCK_SIZE as u64,
    ] {
        let key = crate::content::block_key(&scope(), &node, start).expect("key");
        assert!(
            view.protected.contains(&key),
            "block at {start} is unprotected"
        );
    }
}

#[tokio::test]
async fn a_folder_pin_covers_every_file_under_it_and_says_what_it_could_not_see() {
    let temp = tempfile::tempdir().unwrap();
    let provider = Arc::new(OneFile::default());
    let mut account = fixture_account(temp.path().join("mount"));
    account.cache_bytes = 64 * 1024 * 1024;
    let engine = Engine::new(account, provider, temp.path().join("engine"))
        .await
        .unwrap();
    // Two indexed folders and one that was never indexed. The unindexed one is
    // the point: a walk that silently skipped it would report a total covering
    // less than the user asked for, and nothing would say so.
    let files: Vec<Node> = (0..3)
        .map(|i| file(&format!("f{i}"), "top", 1024 * (i + 1)))
        .collect();
    let nested = vec![file("deep", "sub", 4096)];
    seed_directory(
        &engine,
        "top",
        &[
            folder("sub"),
            files[0].clone(),
            files[1].clone(),
            files[2].clone(),
        ],
    )
    .await;
    seed_directory(&engine, "sub", &nested).await;

    let (found, total, complete) = engine.subtree_files(&scope(), "top").await.unwrap();
    assert_eq!(found.len(), 4, "the nested file counts");
    assert_eq!(total, 1024 + 2048 + 3072 + 4096);
    assert!(complete, "every folder in this tree is indexed");

    let (pinned, complete) = engine
        .pin_folder(&scope(), &folder("top"))
        .await
        .unwrap()
        .expect("the budget holds it");
    assert_eq!(pinned, 4);
    assert!(complete);
    let reservations = engine.cache.reservations();
    let view = reservations.lock().unwrap();
    // `subtree_files` sums logical sizes; the cache stores a SHA-256 ahead of
    // every block, so the reservation covers one digest more per block. Pinned
    // blocks are the ones eviction may not take, so a reservation short by that
    // much is never recovered -- it just lets the cache sit over its budget.
    let stored: u64 = found
        .iter()
        .map(|f| crate::content::stored_bytes(f.size))
        .sum();
    assert_eq!(stored, total + 4 * crate::content::BLOCK_DIGEST);
    assert_eq!(view.reserved, stored);
    assert_eq!(
        view.protected.len(),
        4,
        "one block each, all four protected"
    );
}
#[tokio::test]
async fn an_unindexed_subtree_is_reported_rather_than_quietly_excluded() {
    let temp = tempfile::tempdir().unwrap();
    let provider = Arc::new(OneFile::default());
    let engine = Engine::new(
        fixture_account(temp.path().join("mount")),
        provider,
        temp.path().join("engine"),
    )
    .await
    .unwrap();
    // "sub" is named as a child but never indexed itself.
    seed_directory(&engine, "top", &[folder("sub"), file("f0", "top", 1024)]).await;
    let (found, total, complete) = engine.subtree_files(&scope(), "top").await.unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(total, 1024);
    assert!(
        !complete,
        "a total that excluded an unindexed folder must say so, or a folder pin \
         silently reserves less than the user asked to keep"
    );
}

#[tokio::test]
async fn availability_separates_what_a_pin_reserved_from_what_it_kept() {
    let temp = tempfile::tempdir().unwrap();
    let provider = Arc::new(OneFile::default());
    let mut account = fixture_account(temp.path().join("mount"));
    account.cache_bytes = 64 * 1024 * 1024;
    let engine = Engine::new(account, provider, temp.path().join("engine"))
        .await
        .unwrap();
    let size = crate::content::BLOCK_SIZE as u64 + 512;
    let node = OneFile::node(size);
    engine
        .pin(scope(), node.id.clone(), false, size)
        .await
        .unwrap()
        .expect("fits");

    // Reserved but never fetched. Reporting this as available would promise an
    // offline read that cannot be served.
    let before = engine.pin_status().await.unwrap();
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].reserved, size);
    assert_eq!(before[0].resident, 0, "nothing has been fetched yet");
    assert_eq!(before[0].blocks, 0, "and no blocks are claimed yet");

    engine.materialise_pin(&scope(), &node).await.unwrap();
    let after = engine.pin_status().await.unwrap();
    assert_eq!(after[0].blocks, 2, "two blocks, the second a short one");
    assert!(
        after[0].resident >= size,
        "resident {} should cover the file's {size} bytes",
        after[0].resident
    );
}
#[tokio::test]
async fn a_block_missing_from_disk_is_not_counted_as_available() {
    let temp = tempfile::tempdir().unwrap();
    let provider = Arc::new(OneFile::default());
    let mut account = fixture_account(temp.path().join("mount"));
    account.cache_bytes = 64 * 1024 * 1024;
    let engine = Engine::new(account, provider, temp.path().join("engine"))
        .await
        .unwrap();
    let node = OneFile::node(crate::content::BLOCK_SIZE as u64 + 512);
    engine
        .pin(scope(), node.id.clone(), false, node.size)
        .await
        .unwrap()
        .expect("fits");
    engine.materialise_pin(&scope(), &node).await.unwrap();
    // Remove one block behind the cache's back, as a stray deletion or a failed
    // disk would. The index still lists it, so a status that trusted the index
    // would keep reporting the whole pin as available.
    let key = crate::content::block_key(&scope(), &node, 0).expect("key");
    std::fs::remove_file(engine.cache_path().join(&key)).unwrap();
    let status = engine.pin_status().await.unwrap();
    assert!(
        status[0].resident < node.size,
        "a block that is gone from disk must stop counting as available"
    );
}

#[tokio::test]
async fn pinned_content_reads_offline_and_still_does_after_a_restart() {
    let temp = tempfile::tempdir().unwrap();
    let provider = Arc::new(OneFile::default());
    let mut account = fixture_account(temp.path().join("mount"));
    account.cache_bytes = 64 * 1024 * 1024;
    let node = OneFile::node(crate::content::BLOCK_SIZE as u64 + 4096);
    let expected: Vec<u8> = (0..node.size).map(|i| (i % 251) as u8).collect();
    {
        let engine = Engine::new(
            account.clone(),
            provider.clone(),
            temp.path().join("engine"),
        )
        .await
        .unwrap();
        engine
            .pin(scope(), node.id.clone(), false, node.size)
            .await
            .unwrap()
            .expect("fits");
        engine.materialise_pin(&scope(), &node).await.unwrap();
        provider.offline.store(true, Ordering::SeqCst);
        let read = engine
            .cache
            .read(
                provider.as_ref(),
                &scope(),
                &node,
                0,
                node.size as u32,
                &CancellationToken::new(),
            )
            .await
            .expect("a pinned file must read with the provider unreachable");
        assert_eq!(read, expected);
        engine.stop().await;
    }
    // A second engine on the same directory has an empty memory cache, so this
    // can only be served from the blocks on disk. Without the restart the test
    // would pass on the in-process copy and say nothing about durability.
    let engine = Engine::new(account, provider.clone(), temp.path().join("engine"))
        .await
        .unwrap();
    let before = provider.reads.load(Ordering::SeqCst);
    let read = engine
        .cache
        .read(
            provider.as_ref(),
            &scope(),
            &node,
            0,
            node.size as u32,
            &CancellationToken::new(),
        )
        .await
        .expect("pinned content must survive a restart and read while offline");
    assert_eq!(read, expected);
    assert_eq!(
        provider.reads.load(Ordering::SeqCst),
        before,
        "the provider must not have been asked at all"
    );
    engine.stop().await;
}

#[tokio::test]
async fn pins_may_not_claim_the_whole_budget_and_leave_nothing_to_read_with() {
    let temp = tempfile::tempdir().unwrap();
    let budget = 640 * 1024 * 1024;
    let engine = engine(&temp, budget).await;
    // A pin covering the entire cache would leave every unpinned read with no
    // room at all: the block it just fetched is the one eviction takes next, so
    // the mount would refetch the same bytes forever and still look healthy.
    let refusal = engine
        .pin(scope(), "greedy".into(), true, budget)
        .await
        .unwrap()
        .expect_err("the whole budget must not be pinnable");
    assert!(matches!(
        refusal,
        cirrove_store::pins::PinRefusal::WouldExceedBudget { .. }
    ));
    let allowed = engine.pinnable_budget();
    assert!(allowed < budget, "some of the budget stays unpinnable");
    assert!(
        allowed >= budget / 2,
        "the headroom is a margin, not half the cache"
    );
    engine
        .pin(scope(), "at-the-limit".into(), true, allowed)
        .await
        .unwrap()
        .expect("pinning up to the limit is allowed");
    let reservations = engine.cache.reservations();
    assert_eq!(reservations.lock().unwrap().reserved, allowed);
}
#[tokio::test]
async fn a_small_cache_keeps_whole_blocks_of_headroom_rather_than_a_tenth_of_very_little() {
    let temp = tempfile::tempdir().unwrap();
    // A tenth of a small cache is less than one block, which would leave a
    // reader unable to hold even the block it is reading.
    let budget = 64 * 1024 * 1024;
    let engine = engine(&temp, budget).await;
    let headroom = budget - engine.pinnable_budget();
    assert!(
        headroom >= 8 * crate::content::BLOCK_SIZE as u64,
        "headroom {headroom} is under eight blocks"
    );
}

/// Editing pinned content with the provider unreachable, and finding both the
/// edit and the pin intact after a restart.
///
/// The two features are built separately and the box asks for them together.
/// Pinning keeps blocks the cache may not evict; the journal keeps bytes that
/// have not reached the provider. A restart rebuilds the cache's view from the
/// registry and the journal's from its own files, and nothing in either path
/// knows about the other, so their surviving together is a claim that has to be
/// made rather than assumed.
#[tokio::test]
async fn a_pinned_file_can_be_edited_offline_and_both_survive_a_restart() {
    let temp = tempfile::tempdir().unwrap();
    let provider = Arc::new(OneFile::default());
    let mut account = fixture_account(temp.path().join("mount"));
    account.cache_bytes = 64 * 1024 * 1024;
    let node = OneFile::node(crate::content::BLOCK_SIZE as u64 + 2048);
    let engine_dir = temp.path().join("engine");
    let journal_dir = temp.path().join("journal");
    let expected: Vec<u8> = (0..node.size).map(|i| (i % 251) as u8).collect();

    let (working_id, sealed_id) = {
        let engine = Engine::new(account.clone(), provider.clone(), engine_dir.clone())
            .await
            .unwrap();
        engine
            .pin(scope(), node.id.clone(), false, node.size)
            .await
            .unwrap()
            .expect("fits");
        engine.materialise_pin(&scope(), &node).await.unwrap();
        provider.offline.store(true, Ordering::SeqCst);

        // The edit happens with the provider unreachable, which is the ordinary
        // case this box describes rather than a fault to recover from.
        let mut journal =
            crate::journal::UploadJournal::open(&journal_dir, &scope().account, 16 * 1024 * 1024)
                .unwrap();
        let working = journal
            .create_working(scope(), node.clone(), false, expected.as_slice())
            .unwrap();
        journal
            .write_working(working.id, 0, b"edited offline")
            .unwrap();
        let sealed = journal
            .seal_working(working.id)
            .unwrap()
            .expect("an offline edit must still produce a pending upload");
        engine.stop().await;
        (working.id, sealed.id)
    };

    // Restart both. Neither rebuild consults the other.
    let engine = Engine::new(account, provider.clone(), engine_dir)
        .await
        .unwrap();
    let journal =
        crate::journal::UploadJournal::open(&journal_dir, &scope().account, 16 * 1024 * 1024)
            .unwrap();

    let pending = journal.list(0, 100).unwrap();
    assert_eq!(pending.len(), 1, "the unsent edit survived");
    assert_eq!(pending[0].id, sealed_id);
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut journal.payload(sealed_id).unwrap(), &mut bytes).unwrap();
    assert_eq!(
        &bytes[..14],
        b"edited offline",
        "and it survived with the edited bytes, not the original ones"
    );
    assert!(journal.working_file(working_id).is_ok());

    let reservations = engine.cache.reservations();
    assert_eq!(
        reservations.lock().unwrap().reserved,
        node.size,
        "the pin's reservation is in force before anything can evict"
    );
    assert_eq!(reservations.lock().unwrap().protected.len(), 2);
    // And the pinned content still reads with the provider still unreachable.
    let read = engine
        .cache
        .read(
            provider.as_ref(),
            &scope(),
            &node,
            0,
            node.size as u32,
            &CancellationToken::new(),
        )
        .await
        .expect("pinned content must still read offline after the restart");
    assert_eq!(read, expected);
    engine.stop().await;
}

/// What a file manager gets for a listing: each path's own pin, a recursive pin
/// on a folder above it, or nothing -- and a path that does not exist answered
/// with its own refusal while the rest of the listing still comes back.
#[tokio::test]
async fn path_states_tell_a_direct_pin_from_an_inherited_one_and_from_none() {
    let temp = tempfile::tempdir().unwrap();
    let engine = engine(&temp, 64 * 1024 * 1024).await;
    // Paths resolve in the account's own scope -- its id, its provider, its
    // drive -- which is what a request from a file manager names. The
    // fixture's `scope()` is a different one, and rows seeded under it would
    // be invisible to a path lookup.
    let drive = engine.scope(&engine.account.drive.id);
    let mut sub = folder("sub");
    sub.parent_id = Some("top".into());
    for (parent, children) in [
        ("root", vec![folder("top")]),
        (
            "top",
            vec![
                sub.clone(),
                file("f0", "top", 1024),
                file("f1", "top", 2048),
            ],
        ),
        ("sub", vec![file("deep", "sub", 4096)]),
    ] {
        let (db, scope) = (engine.db.clone(), drive.clone());
        tokio::task::spawn_blocking(move || {
            cirrove_store::Store::open(db)
                .unwrap()
                .observe_directory(&scope, parent, &children)
                .unwrap();
        })
        .await
        .unwrap();
    }
    engine
        .pin(drive.clone(), "f0".into(), false, 1024)
        .await
        .unwrap()
        .expect("fits");
    // The recursive pin as a record, not materialised: this provider serves
    // no content, and what is under test is which pin covers which path.
    engine
        .pin(drive.clone(), "sub".into(), true, 4096)
        .await
        .unwrap()
        .expect("fits");

    let states = engine
        .path_states(&[
            "top/f0".into(),
            "top/f1".into(),
            "top/sub/deep".into(),
            "top/sub".into(),
            "top/missing".into(),
        ])
        .await
        .unwrap();
    let by_path: std::collections::HashMap<_, _> =
        states.into_iter().map(|s| (s.path.clone(), s)).collect();

    let f0 = &by_path["top/f0"];
    assert_eq!(f0.pinned.as_deref(), Some("direct"), "{f0:?}");
    assert_eq!(
        (f0.kind.as_str(), f0.item.as_str(), f0.size),
        ("file", "f0", 1024)
    );
    assert_eq!(f0.resident, 0, "pinned but never fetched keeps nothing");
    assert_eq!(by_path["top/f1"].pinned, None, "a sibling is not covered");
    assert_eq!(
        by_path["top/sub/deep"].pinned.as_deref(),
        Some("inherited"),
        "a recursive pin on the folder above covers the file"
    );
    let sub = &by_path["top/sub"];
    assert_eq!(
        (sub.kind.as_str(), sub.pinned.as_deref()),
        ("folder", Some("direct"))
    );
    assert!(
        by_path["top/missing"].refusal.is_some(),
        "a path that does not exist is refused on its own, not with the listing"
    );
}

/// Residency is read from the block files, not the index, for the same reason
/// pin_status does: between a block being lost and the index rebuilt, the index
/// still lists it.
#[tokio::test]
async fn what_is_on_disk_is_counted_from_the_block_files() {
    let temp = tempfile::tempdir().unwrap();
    let provider = Arc::new(OneFile::default());
    let mut account = fixture_account(temp.path().join("mount"));
    account.cache_bytes = 64 * 1024 * 1024;
    let node = OneFile::node(crate::content::BLOCK_SIZE as u64 + 4096);
    let engine = Engine::new(account, provider, temp.path().join("engine"))
        .await
        .unwrap();
    assert_eq!(
        super::resident_bytes(&engine.cache_path(), &scope(), &node),
        0,
        "nothing fetched, nothing resident"
    );
    engine
        .pin(scope(), node.id.clone(), false, node.size)
        .await
        .unwrap()
        .expect("fits");
    engine.materialise_pin(&scope(), &node).await.unwrap();
    assert_eq!(
        super::resident_bytes(&engine.cache_path(), &scope(), &node),
        node.size,
        "both blocks fetched: the whole file, digests not counted"
    );
    let key = crate::content::block_key(&scope(), &node, 0).expect("key");
    std::fs::remove_file(engine.cache_path().join(&key)).unwrap();
    let resident = super::resident_bytes(&engine.cache_path(), &scope(), &node);
    assert!(
        0 < resident && resident < node.size,
        "one block gone: part of the file, not all and not none ({resident})"
    );
}

/// A folder pinned without `recursive` is answered in words. It used to fall
/// through to materialising the folder's blocks -- it has none -- and the error
/// closed the connection with no reply, which the CLI reported as "EOF while
/// parsing a value".
#[tokio::test]
async fn a_folder_pin_without_recursive_is_refused_in_words_not_dropped() {
    let temp = tempfile::tempdir().unwrap();
    let engine = engine(&temp, 64 * 1024 * 1024).await;
    let reply = engine
        .apply_pin_request(&crate::PinRequest {
            item: Some("root".into()),
            ..Default::default()
        })
        .await
        .expect("a refusal is a reply, not an error");
    assert!(!reply.accepted);
    assert!(
        reply
            .refusal
            .as_deref()
            .is_some_and(|r| r.contains("--recursive")),
        "the refusal names the way to do it: {:?}",
        reply.refusal
    );
    let pins = engine.pin_status().await.unwrap();
    assert!(pins.is_empty(), "nothing was recorded for the refused pin");
}

#[tokio::test]
async fn a_pin_says_where_the_item_is_and_not_only_its_provider_id() {
    // Status is what the window and the tray read, and a list of pins that can
    // only say 016WYNLZUU5GZ4UDRX2JFJDE3W7US5P7FK is a list nobody can act on:
    // the whole point of showing what is kept offline is deciding what to stop
    // keeping.
    let temp = tempfile::tempdir().unwrap();
    let engine = engine(&temp, 64 * 1024 * 1024).await;

    let dokumente = folder("Dokumente");
    let report = file("Report.docx", "Dokumente", 4096);
    seed_directory(&engine, "root", std::slice::from_ref(&dokumente)).await;
    seed_directory(&engine, "Dokumente", std::slice::from_ref(&report)).await;

    engine
        .pin(scope(), report.id.clone(), false, report.size)
        .await
        .unwrap()
        .expect("fits");
    engine
        .pin(scope(), dokumente.id.clone(), true, 0)
        .await
        .unwrap()
        .expect("fits");

    let pins = engine.pin_status().await.unwrap();
    let at = |item: &str| {
        pins.iter()
            .find(|p| p.item == item)
            .unwrap_or_else(|| panic!("{item} is pinned"))
            .path
            .clone()
    };
    assert_eq!(
        at("Report.docx"),
        Some("Dokumente/Report.docx".to_owned()),
        "a file's path is walked up through its folders"
    );
    assert_eq!(
        at("Dokumente"),
        Some("Dokumente".to_owned()),
        "a folder directly under the root is its own name"
    );
}

#[tokio::test]
async fn a_pin_whose_parents_are_not_indexed_has_no_path_rather_than_half_of_one() {
    // The item is indexed; the folder above it is not, so the chain never
    // reaches the root. "orphan" as a path would name a file in the drive root
    // that is a different file, and a caller shown that would unpin the wrong
    // thing. No path at all is the honest answer, and the caller falls back to
    // the id.
    let temp = tempfile::tempdir().unwrap();
    let engine = engine(&temp, 64 * 1024 * 1024).await;
    let orphan = file("orphan", "a-folder-nobody-listed", 4096);
    seed_directory(
        &engine,
        "a-folder-nobody-listed",
        std::slice::from_ref(&orphan),
    )
    .await;
    engine
        .pin(scope(), orphan.id.clone(), false, orphan.size)
        .await
        .unwrap()
        .expect("fits");

    let pins = engine.pin_status().await.unwrap();
    assert_eq!(pins.len(), 1);
    assert_eq!(pins[0].path, None);
}

/// Serves content only when the test lets it, so a fetch can be caught in the
/// act rather than inferred from its result.
#[derive(Default)]
struct Gated {
    open: AtomicBool,
    opened: Notify,
    reads: AtomicU64,
    /// Read number from which every read fails, counting from one. Zero never
    /// fails, which is what `Default` gives.
    fail_from: AtomicU64,
}
impl Gated {
    fn release(&self) {
        self.open.store(true, Ordering::SeqCst);
        self.opened.notify_waiters();
    }
    async fn wait_open(&self) {
        loop {
            let waiting = self.opened.notified();
            tokio::pin!(waiting);
            // Registered before the check, so a release between the two is not
            // a test that hangs once in a hundred runs.
            waiting.as_mut().enable();
            if self.open.load(Ordering::SeqCst) {
                return;
            }
            waiting.await;
        }
    }
}
#[async_trait::async_trait]
impl cirrove_core::MetadataProvider for Gated {
    fn provider_id(&self) -> &'static str {
        "fixture"
    }
    async fn changes(
        &self,
        _: &Scope,
        _: Option<&cirrove_core::Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<cirrove_core::ChangePage, ProviderError> {
        Ok(cirrove_core::ChangePage {
            changes: vec![],
            checkpoint: cirrove_core::Checkpoint::Complete(cirrove_core::Cursor("done".into())),
        })
    }
}
#[async_trait::async_trait]
impl ReadProvider for Gated {
    async fn node(
        &self,
        _: &Scope,
        id: &str,
        _: &CancellationToken,
    ) -> std::result::Result<Node, ProviderError> {
        match id {
            "root" => Ok(folder("root")),
            "top" => Ok(folder("top")),
            other if other.starts_with('f') => Ok(file(other, "top", GATED_FILE)),
            _ => Err(ProviderError::NotFound),
        }
    }
    async fn children(
        &self,
        _: &Scope,
        _: &str,
        _: Option<&cirrove_core::Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<cirrove_core::DirectoryPage, ProviderError> {
        Err(ProviderError::NotFound)
    }
    async fn read_range(
        &self,
        _: &Scope,
        node: &Node,
        offset: u64,
        length: u32,
        cancel: &CancellationToken,
    ) -> std::result::Result<Vec<u8>, ProviderError> {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(ProviderError::Unavailable),
            () = self.wait_open() => {}
        }
        let read = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
        let fail_from = self.fail_from.load(Ordering::SeqCst);
        if fail_from > 0 && read >= fail_from {
            return Err(ProviderError::Unavailable);
        }
        let end = (offset + length as u64).min(node.size);
        Ok((offset..end).map(|i| (i % 251) as u8).collect())
    }
}
/// One block each, so a file is one read and a count of reads is a count of
/// files.
const GATED_FILE: u64 = 4096;

/// The scope the engine builds for itself, which is what a test going through
/// the request path must seed under. `scope()` above names "onedrive" while
/// these fixtures identify as "fixture"; the tests that call `pin_folder`
/// directly are self-consistent because they hand it in, and this one cannot be.
fn engine_scope(engine: &Engine) -> Scope {
    engine.scope(&engine.account.drive.id)
}
async fn seed_directory_in(engine: &Arc<Engine>, scope: &Scope, parent: &str, children: &[Node]) {
    let db = engine.db.clone();
    let (scope, parent, children) = (scope.clone(), parent.to_string(), children.to_vec());
    tokio::task::spawn_blocking(move || {
        let mut store = cirrove_store::Store::open(db).unwrap();
        store.observe_directory(&scope, &parent, &children).unwrap();
    })
    .await
    .unwrap();
}

/// Three files under one folder, indexed, with a gated provider behind them.
async fn gated_folder(temp: &tempfile::TempDir) -> (Arc<Engine>, Arc<Gated>) {
    let provider = Arc::new(Gated::default());
    let mut account = fixture_account(temp.path().join("mount"));
    account.cache_bytes = 64 * 1024 * 1024;
    let engine = Engine::new(account, provider.clone(), temp.path().join("engine"))
        .await
        .unwrap();
    let files: Vec<Node> = (0..3)
        .map(|i| file(&format!("f{i}"), "top", GATED_FILE))
        .collect();
    let scope = engine_scope(&engine);
    seed_directory_in(&engine, &scope, "root", &[folder("top")]).await;
    seed_directory_in(&engine, &scope, "top", &files).await;
    (engine, provider)
}
fn keep(item: &str, recursive: bool) -> crate::PinRequest {
    crate::PinRequest {
        item: Some(item.into()),
        recursive,
        ..Default::default()
    }
}

#[tokio::test]
async fn keeping_a_folder_offline_answers_before_the_first_byte_arrives() {
    // The request used to fetch every file before replying, and the client half
    // gives up after three seconds -- so anything that took longer than that
    // told the person who asked "Cirrove pin timed out" while the daemon went on
    // keeping the folder. Nothing here has been fetched when the answer arrives.
    let temp = tempfile::tempdir().unwrap();
    let (engine, provider) = gated_folder(&temp).await;
    let reply = tokio::time::timeout(
        Duration::from_secs(2),
        engine.apply_pin_request(&keep("top", true)),
    )
    .await
    .expect("the request answers without waiting for the fetch")
    .unwrap();
    assert!(reply.accepted, "{reply:?}");
    assert_eq!(reply.files, 3, "{reply:?}");
    let job = reply
        .job
        .clone()
        .expect("the fetch is named so it can be watched");
    assert_eq!(
        provider.reads.load(Ordering::SeqCst),
        0,
        "nothing fetched yet"
    );

    let listed = engine.jobs.list();
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].id, job);
    assert_eq!(listed[0].name, "top", "named where it sits, not by item id");
    assert_eq!(listed[0].files_total, 3);
    assert_eq!(listed[0].files_done, 0);
    assert!(listed[0].bytes_total >= 3 * GATED_FILE);

    provider.release();
    assert!(
        tokio::time::timeout(Duration::from_secs(10), engine.jobs.wait(&job))
            .await
            .expect("the fetch finishes")
            .is_none(),
        "a job that did what it was asked leaves no progress bar behind"
    );
    let pins = engine.pin_status().await.unwrap();
    assert_eq!(pins.len(), 1);
    assert!(pins[0].resident > 0, "the content is really here: {pins:?}");
}

#[tokio::test]
async fn a_job_reports_how_far_it_has_got() {
    let temp = tempfile::tempdir().unwrap();
    let (engine, provider) = gated_folder(&temp).await;
    let reply = engine.apply_pin_request(&keep("top", true)).await.unwrap();
    let job = reply.job.clone().unwrap();
    provider.release();
    // Watch it move rather than assert one instant: three files is a short race
    // and the point is that the number is not stuck at zero.
    let moved = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(seen) = engine.jobs.find(&job)
                && seen.files_done > 0
            {
                return seen;
            }
            if engine.jobs.find(&job).is_none() {
                return Job::default();
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("progress is reported while the fetch runs");
    assert!(
        moved.files_done > 0 || moved == Job::default(),
        "either it was caught moving, or it was over before it could be"
    );
    tokio::time::timeout(Duration::from_secs(10), engine.jobs.wait(&job))
        .await
        .unwrap();
}

#[tokio::test]
async fn stopping_a_fetch_releases_the_pin_it_was_filling() {
    let temp = tempfile::tempdir().unwrap();
    let (engine, provider) = gated_folder(&temp).await;
    let reply = engine.apply_pin_request(&keep("top", true)).await.unwrap();
    let job = reply.job.clone().unwrap();
    assert_eq!(
        engine.pin_status().await.unwrap().len(),
        1,
        "the reservation is made before the fetch, which is why it can be refused at once"
    );
    assert_eq!(engine.jobs.stop(&job), crate::jobs::Stopped::Asked);
    provider.release();
    let ended = tokio::time::timeout(Duration::from_secs(10), engine.jobs.wait(&job))
        .await
        .expect("a stopped fetch ends promptly");
    assert_eq!(
        ended.map(|job| job.state),
        Some(crate::jobs::JobState::Stopped),
        "the record says it was stopped, rather than vanishing"
    );
    assert!(
        engine.pin_status().await.unwrap().is_empty(),
        "a person who stopped a fetch did not ask to keep part of a folder"
    );
    assert!(
        !engine.cancel.is_cancelled(),
        "stopping a job must not stop the account"
    );
}

#[tokio::test]
async fn a_fetch_that_fails_keeps_what_it_got_and_says_why() {
    let temp = tempfile::tempdir().unwrap();
    let (engine, provider) = gated_folder(&temp).await;
    // The first file arrives; the rest do not.
    provider.fail_from.store(2, Ordering::SeqCst);
    let reply = engine.apply_pin_request(&keep("top", true)).await.unwrap();
    let job = reply.job.clone().unwrap();
    provider.release();
    let ended = tokio::time::timeout(Duration::from_secs(10), engine.jobs.wait(&job))
        .await
        .expect("a failing fetch ends")
        .expect("a failure stays visible");
    assert_eq!(ended.state, crate::jobs::JobState::Failed);
    assert!(
        ended.issue.is_some(),
        "a progress bar that vanishes tells nobody anything"
    );
    assert_eq!(ended.files_done, 1, "how far it got is the useful part");
    let pins = engine.pin_status().await.unwrap();
    assert_eq!(pins.len(), 1, "the pin stays; it is what a retry would use");
    assert!(
        pins[0].resident > 0,
        "one file of three is one file a person can open on a train: {pins:?}"
    );
}

#[tokio::test]
async fn a_single_file_is_a_job_too() {
    // Most are small enough that nobody sees the job, but "most" is not a size
    // limit: one file can be a four-gigabyte recording.
    let temp = tempfile::tempdir().unwrap();
    let (engine, provider) = gated_folder(&temp).await;
    let reply = tokio::time::timeout(
        Duration::from_secs(2),
        engine.apply_pin_request(&keep("f0", false)),
    )
    .await
    .expect("answers without waiting for the file")
    .unwrap();
    assert!(reply.accepted, "{reply:?}");
    let job = reply.job.clone().expect("named so it can be watched");
    assert_eq!(engine.jobs.find(&job).map(|j| j.files_total), Some(1));
    provider.release();
    tokio::time::timeout(Duration::from_secs(10), engine.jobs.wait(&job))
        .await
        .unwrap();
    let pins = engine.pin_status().await.unwrap();
    assert!(pins[0].resident > 0, "{pins:?}");
}

#[tokio::test]
async fn a_pin_can_be_released_by_id_even_where_the_provider_cannot_resolve_it() {
    // Found on a real drive: a folder inside a linked SharePoint library is
    // pinned under that library's scope, and unpinning by id looked the id up
    // in the account's own drive -- "remote item not found". The window's Stop
    // keeping button acts by id, so such a pin could not be released at all,
    // and the same lookup would fail with the network down, which is when
    // someone most wants the space back.
    let temp = tempfile::tempdir().unwrap();
    let (engine, _provider) = gated_folder(&temp).await;
    // A pin recorded in a collection this provider knows nothing about, which
    // is what a linked library looks like from here.
    let elsewhere = Scope {
        collection: "a-linked-library".into(),
        ..engine_scope(&engine)
    };
    engine
        .pin(
            elsewhere,
            "item-the-provider-cannot-resolve".into(),
            true,
            4096,
        )
        .await
        .unwrap()
        .expect("fits");
    assert_eq!(engine.pin_status().await.unwrap().len(), 1);
    let reply = engine
        .apply_unpin_request(&keep("item-the-provider-cannot-resolve", false))
        .await
        .unwrap();
    assert!(reply.accepted, "{reply:?}");
    assert!(reply.refusal.is_none(), "{reply:?}");
    assert!(
        engine.pin_status().await.unwrap().is_empty(),
        "the record is gone and the budget with it"
    );
    // And an id that names nothing pinned is still told so, rather than being
    // reported as released.
    let missing = engine
        .apply_unpin_request(&keep("never-pinned", false))
        .await
        .unwrap();
    assert!(!missing.accepted, "{missing:?}");
    assert!(missing.refusal.is_some(), "{missing:?}");
}
