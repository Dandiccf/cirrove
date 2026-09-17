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
/// Views a mount holds before it starts shedding, unless `CIRROVE_VIEW_CEILING`
/// says otherwise.
///
/// Measured rather than chosen: at 200,000 the 500,000-file gate reads 144.3 /
/// 163.3 / 171.7 MiB of anonymous memory over its indexed baseline against a 256
/// MiB budget, in both of ADR 0005's topologies, and the traversal takes 90
/// seconds against 96 with no ceiling at all -- faster, because a map of 200,000
/// entries is cheaper to work than one of 750,438. At 300,000 it passes too, with
/// nine percent of headroom instead of thirty-three, and headroom is what a
/// default is for: the intercept grows with the library and this one was measured
/// on a synthetic 500,000.
///
/// `CIRROVE_VIEW_CEILING=0` restores the unbounded behaviour, which fails the
/// peak criterion at 379.5 to 394.0 MiB.
const DEFAULT_CEILING: usize = 200_000;

#[derive(Default)]
pub(super) struct CeilingMetrics {
    /// Entries this mount has asked the kernel to drop.
    pub asked: AtomicU64,
    /// The largest overshoot seen. A ceiling that holds shows a small one; a
    /// ceiling that does not shows how far it did not hold, which is the number
    /// worth reporting rather than the average.
    pub max_over: AtomicU64,
}

/// Views a mount may hold before it starts shedding.
pub(super) fn configured() -> usize {
    std::env::var("CIRROVE_VIEW_CEILING")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_CEILING)
}

pub(super) async fn run(inner: Arc<Inner>, notifier: fuser::Notifier) {
    loop {
        // Woken by the lookup that went over, never by a timer: a mount under
        // its ceiling costs nothing, which is what a laptop needs from this.
        tokio::select! { biased;
            () = inner.cancel.cancelled() => return,
            () = inner.over_ceiling.notified() => {}
        }
        while !inner.cancel.is_cancelled() {
            let batch = {
                let Ok(mut views) = inner.views.lock() else {
                    return;
                };
                let over = views.over_ceiling() as u64;
                if over > 0 {
                    inner
                        .ceiling_metrics
                        .max_over
                        .fetch_max(over, Ordering::Relaxed);
                }
                views.shed_batch(BATCH)
            };
            if batch.is_empty() {
                break;
            }
            let notifier = notifier.clone();
            // Off the dispatch thread: `inval_entry` writes to /dev/fuse and the
            // kernel handles it synchronously, so a contended parent blocks the
            // caller rather than returning.
            let Ok(asked) = tokio::task::spawn_blocking(move || {
                let mut asked = 0u64;
                for shed in batch {
                    // A failure means the kernel does not have that entry, which
                    // is the outcome being asked for. It is not counted, because
                    // a count of calls that dropped nothing is how a shed loop
                    // doing no work reads as one doing a great deal.
                    match notifier.inval_entry(INodeNo(shed.parent), OsStr::new(shed.name.as_ref()))
                    {
                        Ok(()) => asked += 1,
                        Err(error) => tracing::trace!(
                            inode = shed.inode,
                            parent = shed.parent,
                            %error,
                            "the kernel did not have the entry a ceiling asked it to drop"
                        ),
                    }
                }
                asked
            })
            .await
            else {
                return;
            };
            inner
                .ceiling_metrics
                .asked
                .fetch_add(asked, Ordering::Relaxed);
            tokio::task::yield_now().await;
        }
    }
}
