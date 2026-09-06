//! Change notifications are hints, never authoritative metadata or file bytes.
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationState {
    #[default]
    Polling,
    Connecting,
    Connected,
    Retrying,
    SignInRequired,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ChangeHint {
    pub generation: u64,
    pub state: NotificationState,
}

/// A bounded, coalescing signal for one account/collection. A generation survives
/// a busy consumer; it is not consumed by reading metadata or connection status.
#[derive(Clone)]
pub struct ChangeHintSender(watch::Sender<ChangeHint>);
impl ChangeHintSender {
    pub fn channel() -> (Self, watch::Receiver<ChangeHint>) {
        let (sender, receiver) = watch::channel(ChangeHint::default());
        (Self(sender), receiver)
    }
    pub fn changed(&self) {
        self.0
            .send_modify(|hint| hint.generation = hint.generation.wrapping_add(1));
    }
    /// A successful subscription also requests catch-up across a possible gap.
    pub fn connected(&self) {
        self.0.send_modify(|hint| {
            hint.state = NotificationState::Connected;
            hint.generation = hint.generation.wrapping_add(1);
        });
    }
    pub fn state(&self, state: NotificationState) {
        self.0.send_modify(|hint| hint.state = state);
    }
}

pub enum WatchEnd {
    /// This adapter has no direct notification implementation. Polling continues.
    Unsupported,
    /// A healthy session reached its renewal deadline; subscribe again now.
    Renew,
}
