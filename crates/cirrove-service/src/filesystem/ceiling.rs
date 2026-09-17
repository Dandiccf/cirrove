//! A ceiling on resolved views, held by shedding ([ADR 0015]).
//!
//! The kernel holds a reference on every entry it has looked up, and a
//! 500,000-file traversal resolves 750,438 of them. Nothing here removes a view:
//! it asks the kernel to drop the dentry with `notify_inval_entry`, and the
//! FORGET that follows travels the path every other forget travels. That is why
//! shedding cannot corrupt the residency accounting -- it does not touch it.
//!
//! The order is what makes it affordable. `shed_batch` returns the oldest
//! resolved views, which during a traversal are in directories the sweep has
//! left, and `fuse_reverse_inval_entry` takes the parent's `i_rwsem`. Aimed at
//! the directory being read it sustains about 105 a second; aimed behind the
//! sweep, 24,000, with no measurable effect on the lookups
//! (`docs/benchmarks/shedding-where-nobody-is-looking.json`).
//!
//! [ADR 0015]: ../../../docs/adr/0015-a-ceiling-on-resolved-views.md
use super::*;

/// How many entries one pass asks about. Large enough that the lock is taken
/// once per several hundred sheds, small enough that the lock is not held while
/// a traversal is trying to take it.
const BATCH: usize = 512;
/// How long to wait when there is nothing over the ceiling. Short, because the
/// thing it is waiting for is a traversal that resolves 7,800 views a second.
const IDLE: Duration = Duration::from_millis(50);

#[derive(Default)]
pub(super) struct CeilingMetrics {
    /// Entries this mount has asked the kernel to drop.
    pub asked: AtomicU64,
    /// The largest overshoot seen. A ceiling that holds shows a small one; a
    /// ceiling that does not shows how far it did not hold, which is the number
    /// worth reporting rather than the average.
    pub max_over: AtomicU64,
}

/// Views a mount may hold before it starts shedding, from
/// `CIRROVE_VIEW_CEILING`. Zero -- the default -- keeps the previous behaviour
/// exactly and costs nothing, because the resolution queue is never written.
pub(super) fn configured() -> usize {
    std::env::var("CIRROVE_VIEW_CEILING")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

pub(super) async fn run(inner: Arc<Inner>, notifier: fuser::Notifier) {
    loop {
        if inner.cancel.is_cancelled() {
            return;
        }
        let batch = {
            let Ok(mut views) = inner.views.lock() else {
                return;
            };
            let over = views.over_ceiling() as u64;
            if over > 0 {
                inner.ceiling_metrics.max_over.fetch_max(over, Ordering::Relaxed);
            }
            views.shed_batch(BATCH)
        };
        if batch.is_empty() {
            tokio::select! { biased;
                () = inner.cancel.cancelled() => return,
                () = tokio::time::sleep(IDLE) => continue,
            }
        }
        let notifier = notifier.clone();
        // Off the dispatch thread: `inval_entry` writes to /dev/fuse and the
        // kernel handles it synchronously, so a contended parent blocks the
        // caller rather than returning.
        let Ok(asked) = tokio::task::spawn_blocking(move || {
            let mut asked = 0u64;
            for shed in batch {
                // A failure means the kernel does not have that entry, which is
                // the outcome being asked for. It is not counted, because a
                // count of calls that dropped nothing is how a shed loop doing
                // no work reads as one doing a great deal.
                if notifier
                    .inval_entry(INodeNo(shed.parent), OsStr::new(shed.name.as_ref()))
                    .is_ok()
                {
                    asked += 1;
                }
            }
            asked
        })
        .await
        else {
            return;
        };
        inner.ceiling_metrics.asked.fetch_add(asked, Ordering::Relaxed);
        tokio::task::yield_now().await;
    }
}
