//! The ceiling holds the resident view count during a traversal (ADR 0015).
//!
//! This is the assertion the peak criterion never had. `real_combined_namespace_churn`
//! measures the peak at 500,000 files and takes five minutes; this runs in
//! seconds and fails if shedding stops working, which is what a gate is for.
use super::*;

/// Files the fixture holds. Enough that an unbounded traversal is several times
/// the ceiling, small enough to run in seconds.
const FILES: usize = 20_000;
const PER_DIRECTORY: usize = 2_000;
const CEILING: usize = 4_000;

/// Stat every file under the mount, which is what an indexer does and what puts
/// a kernel reference on every view.
fn traverse(mount: &std::path::Path) -> usize {
    let mut seen = 0;
    let mut directories = std::fs::read_dir(mount)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    directories.sort();
    for directory in directories {
        let mut entries = std::fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        entries.sort();
        for entry in entries {
            // `metadata`, not the `read_dir` entry's cached type: a LOOKUP is
            // what takes the reference a ceiling has to shed.
            let _ = std::fs::metadata(&entry).unwrap();
            seen += 1;
        }
    }
    seen
}

pub(super) async fn a_ceiling_holds_during_a_traversal() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let provider = Arc::new(GeneratedLibrary {
        files: FILES,
        per_directory: PER_DIRECTORY,
        revision: AtomicU32::new(1),
        content_reads: AtomicU64::new(0),
        foreground_requests: AtomicU64::new(0),
    });
    let engine = Engine::new(
        account(mount.clone()),
        provider.clone(),
        temp.path().join("state"),
    )
    .await
    .unwrap();
    let scope = engine.scope("capacity-drive");
    crate::refresh(
        provider.as_ref(),
        &scope,
        &engine.db,
        false,
        &engine.cancel,
        None,
    )
    .await
    .unwrap();
    let fs = CloudFs::new(engine.clone()).unwrap();
    let inner = fs.inner.clone();
    // `mount` starts the ceiling task with the rest of them.
    let session = fs.mount(&mount).unwrap();
    // Set here rather than through the environment: the environment is
    // process-wide and these fixtures share a process, and a test that changes
    // it changes the test beside it.
    inner.views.lock().unwrap().set_ceiling(CEILING);

    let walked = mount.clone();
    let seen = tokio::task::spawn_blocking(move || traverse(&walked))
        .await
        .unwrap();
    assert_eq!(seen, FILES, "the traversal did not see every file");
    let held = inner.views.lock().unwrap().len();
    let asked = inner
        .ceiling_metrics
        .asked
        .load(std::sync::atomic::Ordering::Relaxed);

    // Twice the ceiling, not the ceiling itself: shedding runs behind the
    // traversal in batches and the overshoot is real -- it reached ten percent
    // at 500,000 files. What this refuses is the unbounded count, which is
    // FILES plus its directories and routes, several times this bound.
    assert!(
        held <= CEILING * 2,
        "the ceiling did not hold: {held} views against a ceiling of {CEILING}, \
         with {asked} entries shed"
    );
    // A count of views that never grew would also pass the assertion above, and
    // would mean the traversal never made any. This is what separates them.
    assert!(
        asked >= (FILES - CEILING * 2) as u64,
        "only {asked} entries were shed for {FILES} files at a ceiling of {CEILING}"
    );
    drop(session);
}
