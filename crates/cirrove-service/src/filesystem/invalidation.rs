//! Page committed changes and live projections; never notify under namespace locks.
use super::*;
use cirrove_store::{MetadataChange, MetadataPosition};
use residency::InvalidationCursor;

#[derive(Default)]
pub(super) struct InvalidationMetrics {
    pub batches: AtomicU64,
    pub entries: AtomicU64,
    pub marks: AtomicU64,
    pub max_batch: AtomicU64,
}
async fn invalidate(
    inner: &Arc<Inner>,
    notifier: &fuser::Notifier,
    change: Option<&MetadataChange>,
) -> Result<(), ()> {
    let mut cursor = InvalidationCursor::default();
    loop {
        if inner.cancel.is_cancelled() {
            return Err(());
        }
        let batch = inner
            .views
            .lock()
            .map_err(|_| ())?
            .invalidation_batch(change, cursor);
        cursor = batch.next;
        let notifier = notifier.clone();
        let count = batch.entries.len() as u64;
        if !batch.entries.is_empty() {
            tokio::task::spawn_blocking(move || {
                for entry in batch.entries {
                    // Keep old file mappings intact; versions have separate inodes.
                    let offset = if entry.directory { 0 } else { -1 };
                    let _ = notifier.inval_inode(INodeNo(entry.inode), offset, 0);
                    if entry.entry {
                        let _ =
                            notifier.inval_entry(INodeNo(entry.parent), OsStr::new(&entry.name));
                    }
                }
            })
            .await
            .map_err(|_| ())?;
        }
        inner
            .invalidation_metrics
            .batches
            .fetch_add(1, Ordering::Relaxed);
        inner
            .invalidation_metrics
            .entries
            .fetch_add(count, Ordering::Relaxed);
        inner
            .invalidation_metrics
            .max_batch
            .fetch_max(count, Ordering::Relaxed);
        if batch.complete {
            return Ok(());
        }
        tokio::task::yield_now().await;
    }
}
async fn position(inner: &Inner) -> Result<MetadataPosition, ()> {
    let db = inner.engine.db.clone();
    tokio::task::spawn_blocking(move || Store::open(db)?.metadata_position())
        .await
        .map_err(|_| ())?
        .map_err(|_| ())
}
pub(super) async fn run(
    inner: Arc<Inner>,
    notifier: fuser::Notifier,
    mut wake: tokio::sync::watch::Receiver<u64>,
) {
    let mut cursor = None;
    let mut full_seen = *wake.borrow_and_update();
    let mut retry = true;
    loop {
        if !retry {
            tokio::select! {biased;_=inner.cancel.cancelled()=>return,result=wake.changed()=>if result.is_err(){return;}}
        }
        if inner.cancel.is_cancelled() {
            return;
        }
        let full = *wake.borrow_and_update();
        let work = async {
            // Writable projections can use durable local IDs whose provider
            // bindings are held by the writeback projection, not this view index.
            if cursor.is_none() || full != full_seen || inner.writeback.is_some() {
                // Capture before the sweep: later commits remain pending in watch
                // and in the revision stream, even while kernel notification blocks.
                let next = position(&inner).await?;
                invalidate(&inner, &notifier, None).await?;
                cursor = Some(next);
                full_seen = full;
            } else {
                loop {
                    let db = inner.engine.db.clone();
                    let after = cursor.clone().ok_or(())?;
                    let page = tokio::task::spawn_blocking(move || {
                        Store::open(db)?.metadata_changes(&after)
                    })
                    .await
                    .map_err(|_| ())?
                    .map_err(|_| ())?;
                    inner
                        .invalidation_metrics
                        .marks
                        .fetch_add(page.changes.len() as u64, Ordering::Relaxed);
                    for change in &page.changes {
                        invalidate(&inner, &notifier, Some(change)).await?;
                    }
                    cursor = Some(page.next);
                    if page.complete {
                        break;
                    }
                }
            }
            Ok::<_, ()>(())
        }
        .await;
        retry = work.is_err();
        if retry {
            // An unreadable/replaced revision stream must not silently lose work.
            // Retry through a bounded full sweep; no raw database details are logged.
            cursor = None;
            tokio::select! {biased;_=inner.cancel.cancelled()=>return,_=tokio::time::sleep(Duration::from_secs(1))=>{}}
        }
    }
}
