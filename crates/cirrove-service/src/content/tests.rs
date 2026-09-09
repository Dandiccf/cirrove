use super::*;
use cirrove_store::BlockIndex;

const MIB: u64 = 1 << 20;

fn cache(temp: &tempfile::TempDir, quota: u64) -> ContentCache {
    ContentCache::new(temp.path().join("cache"), temp.path().join("db"), quota).expect("cache")
}
/// Publish a block file and index it at `size`, without downloading anything.
/// The index's declared size is what eviction reasons about, so a small file
/// with a large declared size exercises the accounting without the bytes.
fn place(cache: &ContentCache, key: &str, size: u64) {
    std::fs::write(cache.path.join(key), b"x").expect("block file");
    BlockIndex::open(&cache.blocks)
        .expect("index")
        .touch(key, size)
        .expect("touch");
}
fn key(c: char) -> String {
    std::iter::repeat_n(c, 64).collect()
}

#[tokio::test]
async fn eviction_leaves_a_protected_block_and_takes_the_next_one_instead() {
    let temp = tempfile::tempdir().expect("fixture");
    let cache = cache(&temp, 512 * MIB);
    // The protected block is deliberately the oldest, so eviction reaches it
    // first and has to decline. Protecting the newest would pass either way.
    let (pinned, first, second) = (key('a'), key('b'), key('c'));
    // Sized against the cache's own budget rather than the quota it was asked
    // for: the constructor reserves staging out of that, so a guessed size makes
    // the test assert how much staging takes rather than how eviction chooses.
    let size = cache.quota / 2;
    for k in [&pinned, &first, &second] {
        place(&cache, k, size);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    cache
        .reservations()
        .lock()
        .expect("reservations")
        .protected
        .insert(pinned.clone());
    cache.evict("").await.expect("evict");
    assert!(
        cache.path.join(&pinned).exists(),
        "the oldest block was pinned and must survive the pressure that removed the next one"
    );
    assert!(
        !cache.path.join(&first).exists(),
        "eviction must still make room by taking an unprotected block"
    );
    assert!(cache.path.join(&second).exists());
}
#[tokio::test]
async fn a_reservation_shrinks_the_budget_the_ordinary_cache_may_fill() {
    let temp = tempfile::tempdir().expect("fixture");
    let cache = cache(&temp, 512 * MIB);
    let (older, newer) = (key('d'), key('e'));
    let size = cache.quota / 4;
    place(&cache, &older, size);
    std::thread::sleep(std::time::Duration::from_millis(5));
    place(&cache, &newer, size);
    // Half the budget in total evicts nothing.
    cache.evict("").await.expect("evict");
    assert!(cache.path.join(&older).exists());
    // Reserving most of the budget must push the same 200 MiB over the line.
    // Without this the reservation would be a number nobody acts on, and pinned
    // content would be downloaded into space the cache still believes is free.
    cache.reservations().lock().expect("reservations").reserved = cache.quota - size;
    cache.evict("").await.expect("evict");
    assert!(
        !cache.path.join(&older).exists(),
        "the reservation must shrink what unpinned blocks may occupy"
    );
}

/// Fill a cache past its budget and reopen it, optionally telling the new cache
/// that the oldest block is pinned. Returns whether that block survived.
fn survives_reconcile(temp: &tempfile::TempDir, reserve: bool) -> bool {
    let (cache_path, blocks) = (temp.path().join("cache"), temp.path().join("db"));
    let quota = 512 * MIB;
    // Both keys must be hex. A name outside 0-9a-f is not recognised as a block
    // at all: reconcile forgets it from the index and leaves the file, which
    // silently removes the eviction pressure this fixture exists to create.
    let oldest = key('e');
    let size = {
        let cache = ContentCache::new(cache_path.clone(), blocks.clone(), quota).expect("cache");
        let size = cache.quota;
        place(&cache, &oldest, size);
        std::thread::sleep(std::time::Duration::from_millis(5));
        place(&cache, &key('f'), size);
        size
    };
    let mut reservations = Reservations::default();
    if reserve {
        reservations.protected.insert(oldest.clone());
        reservations.reserved = size;
    }
    let _cache = ContentCache::new_reserving(cache_path.clone(), blocks, quota, reservations)
        .expect("cache");
    cache_path.join(&oldest).exists()
}
#[tokio::test]
async fn the_reconcile_pass_at_mount_time_also_leaves_protected_blocks_alone() {
    // Reconcile evicts to fit the quota before any engine exists to publish a
    // reservation, so this is the pass that would silently delete pinned content
    // at every mount. Both halves are needed: the first shows the block survives,
    // and the second shows this fixture would notice if it did not.
    assert!(
        survives_reconcile(&tempfile::tempdir().expect("fixture"), true),
        "a pinned block must survive the mount-time reconcile, not only runtime eviction"
    );
    assert!(
        !survives_reconcile(&tempfile::tempdir().expect("fixture"), false),
        "without the reservation the same block is evicted, so the check above means something"
    );
}
