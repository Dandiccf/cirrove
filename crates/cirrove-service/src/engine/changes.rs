//! Coalescing wake generations retain events arriving during invalidation work.
use tokio::sync::watch;
pub struct ChangeNotifications {
    sender: watch::Sender<u64>,
}
impl Default for ChangeNotifications {
    fn default() -> Self {
        Self {
            sender: watch::channel(0).0,
        }
    }
}
impl ChangeNotifications {
    /// Local namespace changes and explicit recovery requests need a full sweep.
    pub fn notify_waiters(&self) {
        self.sender.send_modify(|full| *full = full.wrapping_add(1));
    }
    pub fn notify_one(&self) {
        self.notify_waiters();
    }
    /// Wake after a metadata commit whose identities are in the revision index.
    /// Journal-only/local changes must use the conservative notification methods.
    pub fn metadata(&self) {
        self.sender.send_modify(|_| {});
    }
    pub(crate) fn subscribe(&self) -> watch::Receiver<u64> {
        self.sender.subscribe()
    }
}
#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    #[tokio::test]
    async fn remote_and_full_wakes_survive_work_between_receiver_polls() {
        let signal = ChangeNotifications::default();
        let mut receiver = signal.subscribe();
        signal.metadata();
        receiver.changed().await.unwrap();
        assert_eq!(*receiver.borrow_and_update(), 0);
        signal.metadata();
        signal.notify_waiters();
        signal.metadata();
        receiver.changed().await.unwrap();
        assert_eq!(*receiver.borrow_and_update(), 1);
        assert!(!receiver.has_changed().unwrap());
    }
}
