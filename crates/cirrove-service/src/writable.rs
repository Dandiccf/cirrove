//! Experimental mounted-edit lifecycle; ordinary account managers stay read-only.
use crate::{
    engine::Engine,
    filesystem::{CloudFs, CloudSession, WriteControl},
    journal::{JournalError, UploadJournal, UploadRecord},
    transfers::TransferWorker,
};
use cirrove_auth::CredentialVault;
use cirrove_core::{CancellationToken, upload::UploadProvider};
use std::{
    io,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Notify;
use tokio_util::task::TaskTracker;

pub struct WritableSession {
    engine: Arc<Engine>,
    control: WriteControl,
    kernel: Option<CloudSession>,
    cancel: CancellationToken,
    workers: TaskTracker,
    journal: Arc<Mutex<UploadJournal>>,
    issue: Arc<Mutex<Option<String>>>,
}
impl WritableSession {
    /// The caller constructs an engine for a disabled, explicitly writable test
    /// account. This owner starts its metadata and upload workers, then owns their
    /// shutdown. It is not installed or selected by the ordinary account manager.
    pub async fn mount(
        engine: Arc<Engine>,
        journal: Arc<Mutex<UploadJournal>>,
        provider: Arc<dyn UploadProvider>,
        vault: Arc<dyn CredentialVault>,
    ) -> io::Result<Self> {
        let fs = CloudFs::new_experimental_writable(engine.clone(), journal.clone()).await?;
        let control = fs.write_control()?;
        if let Err(error) = engine.start().await {
            engine.stop().await;
            return Err(io::Error::other(error));
        }
        let path = engine.account.mount_path.clone();
        let mounted = tokio::task::spawn_blocking(move || fs.mount(&path))
            .await
            .map_err(io::Error::other)
            .and_then(|r| r);
        let kernel = match mounted {
            Ok(kernel) => kernel,
            Err(error) => {
                engine.stop().await;
                return Err(error);
            }
        };
        let cancel = engine.cancel.child_token();
        let workers = TaskTracker::new();
        let issue = Arc::new(Mutex::new(None));
        for _ in 0..2 {
            let worker = TransferWorker::new(
                journal.clone(),
                provider.clone(),
                vault.clone(),
                cancel.clone(),
            );
            workers.spawn(pump(worker, control.wake(), cancel.clone(), issue.clone()));
        }
        workers.close();
        Ok(Self {
            engine,
            control,
            kernel: Some(kernel),
            cancel,
            workers,
            journal,
            issue,
        })
    }
    /// Paginated durable operation state, including fragment progress. This is
    /// private service data, not a log: checkpoints/credentials remain in the vault.
    pub async fn uploads(
        &self,
        after: u64,
        limit: u32,
    ) -> crate::journal::Result<Vec<UploadRecord>> {
        let journal = self.journal.clone();
        tokio::task::spawn_blocking(move || {
            journal
                .lock()
                .map_err(|_| JournalError::Storage)?
                .list(after, limit)
        })
        .await
        .map_err(|_| JournalError::Storage)?
    }
    pub fn worker_issue(&self) -> Option<String> {
        self.issue.lock().ok().and_then(|issue| issue.clone())
    }
    pub fn pending_local_requests(&self) -> usize {
        self.control.pending()
    }
    /// Stops admission, cancels external work, drains local writes and seals dirty
    /// copies before detaching FUSE. A seal failure retains working bytes and is
    /// reported even though the mount and workers are still stopped cleanly.
    pub async fn shutdown(mut self) -> io::Result<()> {
        self.control.freeze();
        self.cancel.cancel();
        self.engine.cancel.cancel();
        let local = self.control.drain().await;
        self.workers.wait().await;
        let detached = if let Some(kernel) = self.kernel.take() {
            tokio::task::spawn_blocking(move || kernel.umount_and_join())
                .await
                .map_err(io::Error::other)
                .and_then(|r| r)
        } else {
            Ok(())
        };
        self.engine.stop().await;
        local.and(detached)
    }
}
impl Drop for WritableSession {
    fn drop(&mut self) {
        if self.kernel.is_some() {
            self.control.freeze();
            self.cancel.cancel();
            self.engine.cancel.cancel();
            // Drop is an interrupted shutdown, never a claim that local edits
            // were drained. Durable snapshots and dirty working files are retained.
            tracing::warn!("writable session dropped without completing local-save shutdown");
        }
    }
}
async fn pump(
    worker: TransferWorker,
    wake: Arc<Notify>,
    cancel: CancellationToken,
    issue: Arc<Mutex<Option<String>>>,
) {
    loop {
        let changed = wake.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if cancel.is_cancelled() {
            return;
        }
        match worker.run_once().await {
            Ok(Some(_)) => continue,
            Ok(None) => {}
            Err(error) => {
                if let Ok(mut issue) = issue.lock() {
                    *issue = Some(error.to_string());
                }
            }
        }
        // Notifications make new saves prompt. A bounded fallback also revisits
        // persisted retry deadlines and missed hints without resetting backoff.
        tokio::select! {
            biased;
            _=cancel.cancelled()=>return,
            _=changed=>{},
            _=tokio::time::sleep(Duration::from_secs(1))=>{},
        }
    }
}
