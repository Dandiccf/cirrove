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
        })
    }
}
impl WriteControl {
    pub fn pending(&self) -> usize {
        self.inner.edits.tasks.len()
    }
    pub fn freeze(&self) {
        self.inner.edits.close();
        self.inner.cancel.cancel();
    }
    pub async fn drain(&self) -> std::io::Result<()> {
        self.inner.edits.tasks.wait().await;
        self.writer.seal_all().await.map_err(|_| {
            std::io::Error::other(
                "some local edits could not be sealed; working bytes remain retained",
            )
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
