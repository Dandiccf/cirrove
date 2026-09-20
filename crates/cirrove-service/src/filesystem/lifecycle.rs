//! Close edit admission atomically before awaiting the admitted callbacks.
use super::*;
use tokio_util::task::{TaskTracker, task_tracker::TaskTrackerToken};

pub(super) struct EditAdmission {
    open: Mutex<bool>,
    tasks: TaskTracker,
}
impl EditAdmission {
    pub fn new() -> Self {
        Self {
            open: Mutex::new(true),
            tasks: TaskTracker::new(),
        }
    }
    pub fn admit(&self) -> Result<TaskTrackerToken, Errno> {
        let open = self.open.lock().map_err(|_| Errno::EIO)?;
        if !*open {
            return Err(Errno::ENODEV);
        }
        Ok(self.tasks.token())
    }
    fn close(&self) {
        if let Ok(mut open) = self.open.lock() {
            *open = false;
        }
        self.tasks.close();
    }
}

#[derive(Clone)]
pub(crate) struct WriteControl {
    inner: Arc<Inner>,
    writer: Arc<super::writeback::Writeback>,
    /// Set once the writers exist, for the one operation that is not queued
    /// work: permanent deletion (ADR 0008). Everything else here drains a
    /// journal, because a save or a rename must survive a restart. A permanent
    /// delete must not survive anything: it is a person at the machine saying
    /// "destroy this one, now", and a durable queue for destruction would retry
    /// it after the item had moved.
    provider: Option<Arc<dyn cirrove_core::mutation::MutationProvider>>,
}
impl WriteControl {
    pub(crate) fn with_provider(
        mut self,
        provider: Arc<dyn cirrove_core::mutation::MutationProvider>,
    ) -> Self {
        self.writer.set_provider(provider.clone());
        self.provider = Some(provider);
        self
    }
    pub(crate) fn provider(&self) -> Option<Arc<dyn cirrove_core::mutation::MutationProvider>> {
        self.provider.clone()
    }

    /// Resolve the path a writable mount actually presents, including natural
    /// names retained by the durable writeback namespace after provider IDs are
    /// assigned. Control-socket features such as file-manager pinning must walk
    /// the same overlay as FUSE or a visible path can be rejected even while it
    /// is open in the file manager.
    pub(crate) async fn resolve_visible_path(
        &self,
        engine: &Arc<Engine>,
        path: &str,
    ) -> std::io::Result<(Scope, Node)> {
        fn unavailable(error: impl std::fmt::Display) -> std::io::Error {
            std::io::Error::other(error.to_string())
        }

        let mut scope = engine.scope(&engine.account.drive.id);
        let mut node = engine
            .node(&scope, &engine.account.root_id)
            .await
            .map_err(unavailable)?;
        for name in path.split('/').filter(|part| !part.is_empty()) {
            if let Some(target) = node.target.clone() {
                scope = engine.scope(&target.collection);
                node = engine
                    .node(&scope, &target.item)
                    .await
                    .map_err(unavailable)?;
            }
            let parent = node.id.clone();
            let provider_parent = self
                .writer
                .directory_identity(&scope, &parent)
                .map_err(|error| std::io::Error::from_raw_os_error(error.code()))?;
            let children = match provider_parent {
                Some(provider_parent) => engine
                    .children(&scope, &provider_parent)
                    .await
                    .map_err(unavailable)?,
                None => Vec::new(),
            };
            node = self
                .writer
                .overlay(&scope, &parent, children)
                .map_err(|error| std::io::Error::from_raw_os_error(error.code()))?
                .into_iter()
                .find(|child| child.name == name)
                .ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::NotFound, "remote item not found")
                })?;
        }
        if let Some(target) = node.target.clone() {
            scope = engine.scope(&target.collection);
            node = engine
                .node(&scope, &target.item)
                .await
                .map_err(unavailable)?;
        }
        if let Some(item) = self
            .writer
            .remote_identity(&scope, &node.id)
            .map_err(|error| std::io::Error::from_raw_os_error(error.code()))?
        {
            node = engine.node(&scope, &item).await.map_err(unavailable)?;
        }
        Ok((scope, node))
    }
}
impl CloudFs {
    pub(crate) fn write_control(&self) -> std::io::Result<WriteControl> {
        let writer = self
            .inner
            .writeback
            .clone()
            .ok_or_else(|| std::io::Error::other("filesystem is not writable"))?;
        Ok(WriteControl {
            inner: self.inner.clone(),
            writer,
            provider: None,
        })
    }
}
impl WriteControl {
    pub async fn maintain(&self) -> std::io::Result<bool> {
        self.writer
            .maintain(&self.inner.engine)
            .await
            .map_err(|_| std::io::Error::other("local namespace maintenance failed"))
    }
    pub async fn refresh_operation(&self, id: uuid::Uuid) -> std::io::Result<()> {
        self.writer
            .refresh_operation(id)
            .await
            .map_err(|_| std::io::Error::other("local namespace refresh failed"))?;
        self.inner.engine.changed.notify_waiters();
        Ok(())
    }
    /// Try the stuck changes again, where trying again is a sensible thing to
    /// do. See [`super::writeback::Writeback::retry_stuck`].
    pub async fn retry_stuck(&self) -> std::io::Result<(u64, u64)> {
        self.writer
            .retry_stuck()
            .await
            .map_err(|_| std::io::Error::other("the stuck changes could not be queued again"))
    }
    pub async fn discard_stuck(&self) -> std::io::Result<u64> {
        self.writer
            .discard_stuck()
            .await
            .map_err(|_| std::io::Error::other("could not discard the stuck changes"))
    }
    pub async fn stuck_changes(&self) -> std::io::Result<u64> {
        self.writer
            .stuck_changes()
            .await
            .map_err(|_| std::io::Error::other("local namespace is unavailable"))
    }
    pub async fn failed_uploads(&self) -> std::io::Result<u64> {
        self.writer
            .failed_uploads()
            .await
            .map_err(|_| std::io::Error::other("local namespace is unavailable"))
    }
    pub async fn stuck_changes_named(
        &self,
        limit: usize,
    ) -> std::io::Result<Vec<crate::recent::StuckChange>> {
        self.writer
            .stuck_changes_named(limit)
            .await
            .map_err(|_| std::io::Error::other("local namespace is unavailable"))
    }
    pub async fn failed_uploads_named(
        &self,
        limit: usize,
    ) -> std::io::Result<Vec<crate::recent::StuckChange>> {
        self.writer
            .failed_uploads_named(limit)
            .await
            .map_err(|_| std::io::Error::other("local namespace is unavailable"))
    }
    pub async fn keep_both_plans(
        &self,
        limit: usize,
    ) -> std::io::Result<Vec<crate::recent::SavePlan>> {
        self.writer
            .keep_both_plans(limit)
            .await
            .map_err(|_| std::io::Error::other("local namespace is unavailable"))
    }
    pub async fn keep_both(
        &self,
        plans: Vec<(uuid::Uuid, String, String)>,
    ) -> std::io::Result<u64> {
        self.writer
            .keep_both(plans)
            .await
            .map_err(|_| std::io::Error::other("local namespace is unavailable"))
    }
    pub async fn recent_local(
        &self,
        limit: usize,
    ) -> std::io::Result<Vec<crate::recent::LocalChange>> {
        self.writer
            .recent_local(limit)
            .await
            .map_err(|_| std::io::Error::other("local namespace is unavailable"))
    }
    pub fn conflicts(&self) -> std::io::Result<Vec<crate::journal::NamespaceCollision>> {
        self.writer
            .conflicts()
            .map_err(|_| std::io::Error::other("local namespace is unavailable"))
    }
    pub fn pending(&self) -> usize {
        self.inner.edits.tasks.len()
    }
    pub fn freeze(&self) {
        self.inner.edits.close();
        self.inner.cancel.cancel();
    }
    pub async fn drain(&self) -> std::io::Result<()> {
        self.inner.edits.tasks.wait().await;
        // Keep the errno. Sealing fails for two unrelated reasons -- a working
        // file that would not fsync, or a publication that failed afterwards --
        // and collapsing both into one sentence cost a CI failure that could
        // not be told apart from the other.
        self.writer.seal_all().await.map_err(|error| {
            std::io::Error::other(format!(
                "some local edits could not be sealed; working bytes remain retained ({error:?})"
            ))
        })
    }
    pub fn wake(&self) -> Arc<tokio::sync::Notify> {
        self.writer.wake.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn frozen_admission_waits_for_the_last_accepted_edit() {
        let gate = EditAdmission::new();
        let first = gate.admit().expect("admission");
        let second = gate.admit().expect("admission");
        gate.close();
        assert!(gate.admit().is_err());
        drop(first);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), gate.tasks.wait())
                .await
                .is_err()
        );
        drop(second);
        tokio::time::timeout(Duration::from_secs(1), gate.tasks.wait())
            .await
            .expect("drained");
        assert!(gate.admit().is_err());
    }
}
