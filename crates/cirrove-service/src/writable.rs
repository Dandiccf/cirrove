//! Experimental mounted-edit lifecycle; ordinary account managers stay read-only.
use crate::{
    engine::Engine,
    filesystem::{CloudFs, CloudSession, WriteControl},
    journal::{JournalError, UploadJournal, UploadRecord},
    mutations::MutationWorker,
    transfers::TransferWorker,
};
use cirrove_auth::CredentialVault;
use cirrove_core::{CancellationToken, mutation::MutationProvider, upload::UploadProvider};
use std::{
    io,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::task::TaskTracker;

/// Experimental sessions need both content and conditional namespace writes.
pub trait WriteProvider: UploadProvider + MutationProvider {}
impl<T: UploadProvider + MutationProvider> WriteProvider for T {}

/// The workers behind a writable mount: two transfer pumps, a mutation pump and
/// the maintenance loop, with the shutdown order they require.
///
/// Extracted so the account manager can own the same set for an ordinary mount.
/// `WritableSession` is the validators' owner of it and stays that.
pub(crate) struct WriteWorkers {
    control: WriteControl,
    cancel: CancellationToken,
    workers: TaskTracker,
    issue: Arc<Mutex<Option<String>>>,
}
impl WriteWorkers {
    pub(crate) fn spawn(
        control: WriteControl,
        journal: Arc<Mutex<UploadJournal>>,
        provider: Arc<dyn WriteProvider>,
        vault: Arc<dyn CredentialVault>,
        parent: &CancellationToken,
    ) -> Self {
        let cancel = parent.child_token();
        let workers = TaskTracker::new();
        let issue = Arc::new(Mutex::new(None));
        for _ in 0..2 {
            let worker = TransferWorker::new(
                journal.clone(),
                provider.clone(),
                vault.clone(),
                cancel.clone(),
            );
            workers.spawn(pump(
                WriteWorker::Upload(worker),
                control.clone(),
                cancel.clone(),
                issue.clone(),
            ));
        }
        let mutations = MutationWorker::new(journal.clone(), provider, cancel.clone());
        workers.spawn(pump(
            WriteWorker::Mutation(mutations),
            control.clone(),
            cancel.clone(),
            issue.clone(),
        ));
        workers.spawn(maintain(control.clone(), cancel.clone(), issue.clone()));
        workers.close();
        Self {
            control,
            cancel,
            workers,
            issue,
        }
    }
    pub(crate) fn worker_issue(&self) -> Option<String> {
        self.issue.lock().ok().and_then(|issue| issue.clone())
    }
    pub(crate) fn pending_local_requests(&self) -> usize {
        self.control.pending()
    }
    /// Stop admitting work, drain what was already admitted, and wait for every
    /// worker to finish. The caller unmounts afterwards and never before: a
    /// worker can still be holding a projection the kernel is able to reach.
    pub(crate) async fn drain(self) -> io::Result<()> {
        self.control.freeze();
        self.cancel.cancel();
        let local = self.control.drain().await;
        self.workers.wait().await;
        local
    }
}

pub struct WritableSession {
    engine: Arc<Engine>,
    kernel: Option<CloudSession>,
    writers: Option<WriteWorkers>,
    journal: Arc<Mutex<UploadJournal>>,
}
impl WritableSession {
    /// The caller constructs an engine for a disabled, explicitly writable test
    /// account. This owner starts metadata, upload and namespace workers, then owns their
    /// shutdown. It is not installed or selected by the ordinary account manager.
    pub async fn mount(
        engine: Arc<Engine>,
        journal: Arc<Mutex<UploadJournal>>,
        provider: Arc<dyn WriteProvider>,
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
        let writers =
            WriteWorkers::spawn(control, journal.clone(), provider, vault, &engine.cancel);
        Ok(Self {
            engine,
            kernel: Some(kernel),
            writers: Some(writers),
            journal,
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
    pub fn namespace_conflicts(&self) -> io::Result<Vec<crate::journal::NamespaceCollision>> {
        self.writers
            .as_ref()
            .ok_or_else(|| io::Error::other("session is shutting down"))?
            .control
            .conflicts()
    }
    pub async fn mutations(
        &self,
        after: u64,
        limit: u32,
    ) -> crate::journal::Result<Vec<crate::journal::MutationRecord>> {
        let journal = self.journal.clone();
        tokio::task::spawn_blocking(move || {
            journal
                .lock()
                .map_err(|_| JournalError::Storage)?
                .list_mutations(after, limit)
        })
        .await
        .map_err(|_| JournalError::Storage)?
    }
    pub fn worker_issue(&self) -> Option<String> {
        self.writers.as_ref().and_then(WriteWorkers::worker_issue)
    }
    pub fn pending_local_requests(&self) -> usize {
        self.writers
            .as_ref()
            .map_or(0, WriteWorkers::pending_local_requests)
    }
    /// Stops admission, cancels external work, drains local writes and seals dirty
    /// copies before detaching FUSE. A seal failure retains working bytes and is
    /// reported even though the mount and workers are still stopped cleanly.
    pub async fn shutdown(mut self) -> io::Result<()> {
        self.engine.cancel.cancel();
        let local = match self.writers.take() {
            Some(writers) => writers.drain().await,
            None => Ok(()),
        };
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
async fn maintain(
    control: WriteControl,
    cancel: CancellationToken,
    issue: Arc<Mutex<Option<String>>>,
) {
    let wake = control.wake();
    let mut failed = 0u32;
    loop {
        if cancel.is_cancelled() {
            return;
        }
        let changed = wake.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        let delay = match control.maintain().await {
            Ok(worked) => {
                failed = 0;
                if worked {
                    Duration::from_secs(2)
                } else {
                    Duration::from_millis(500)
                }
            }
            Err(error) => {
                failed = failed.saturating_add(1).min(6);
                if let Ok(mut issue) = issue.lock() {
                    *issue = Some(error.to_string());
                }
                Duration::from_secs((1u64 << failed).min(60))
            }
        };
        if failed > 0 || delay >= Duration::from_secs(2) {
            tokio::select! {biased; _=cancel.cancelled()=>return, _=tokio::time::sleep(delay)=>{},}
        } else {
            tokio::select! {biased; _=cancel.cancelled()=>return, _=changed=>{}, _=tokio::time::sleep(delay)=>{},}
        }
    }
}
impl Drop for WritableSession {
    fn drop(&mut self) {
        if self.kernel.is_some() {
            if let Some(writers) = &self.writers {
                writers.control.freeze();
                writers.cancel.cancel();
            }
            self.engine.cancel.cancel();
            // Drop is an interrupted shutdown, never a claim that local edits
            // were drained. Durable snapshots and dirty working files are retained.
            tracing::warn!("writable session dropped without completing local-save shutdown");
        }
    }
}
enum WriteWorker {
    Upload(TransferWorker),
    Mutation(MutationWorker),
}
impl WriteWorker {
    async fn run_once(&self) -> Result<Option<(uuid::Uuid, Option<String>)>, String> {
        match self {
            Self::Upload(worker) => worker
                .run_once()
                .await
                .map(|r| r.map(|r| (r.id, r.issue)))
                .map_err(|e| e.to_string()),
            Self::Mutation(worker) => worker
                .run_once()
                .await
                .map(|r| r.map(|r| (r.id, r.issue)))
                .map_err(|e| e.to_string()),
        }
    }
}
async fn pump(
    worker: WriteWorker,
    control: WriteControl,
    cancel: CancellationToken,
    issue: Arc<Mutex<Option<String>>>,
) {
    let wake = control.wake();
    loop {
        let changed = wake.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if cancel.is_cancelled() {
            return;
        }
        match worker.run_once().await {
            Ok(Some((id, worker_issue))) => {
                let refresh = control.refresh_operation(id).await;
                if let Some(error) = worker_issue.or_else(|| refresh.err().map(|e| e.to_string()))
                    && let Ok(mut issue) = issue.lock()
                {
                    *issue = Some(error);
                }
                // A receipt may unblock the other worker kind immediately.
                wake.notify_waiters();
                continue;
            }
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
