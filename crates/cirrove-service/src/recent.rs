//! What changed lately, for a window and a tray to show.
//!
//! "Recent" means changes to content: files and folders that arrived, changed
//! or went away on the cloud side, and saves made locally that are on their
//! way or have arrived. Not files the user merely opened -- a mount stages
//! cache on every read, and a list of everything read would be a list of
//! everything.
//!
//! Remote changes are recorded as the delta feed delivers them, from the
//! second delta on: the first one lists the whole drive, which is a baseline
//! and not activity, and a re-baseline after a reset is the same. A bounded
//! ring, in memory, per account -- a restart forgets it, which is right for a
//! list whose only purpose is "what happened while I was looking away".
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

/// How many remote changes an account remembers.
pub const CAPACITY: usize = 200;

/// One thing that changed on the cloud side.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteChange {
    pub at_unix: u64,
    pub id: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub name: String,
    /// "file" or "folder".
    pub kind: String,
    #[serde(default)]
    pub size: u64,
    /// Gone from the cloud, rather than added or changed. The feed does not
    /// say which of those two an upsert is, and neither does this.
    #[serde(default)]
    pub removed: bool,
}

/// One save made locally, from the upload journal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalChange {
    /// The journal's own order; larger is later.
    pub sequence: u64,
    /// The file's name, when the journal or the index knows it; the item id
    /// when neither does.
    pub name: String,
    /// For a replace, the item the save replaces; the daemon resolves it to
    /// the name above when the index knows it.
    #[serde(default)]
    pub item: Option<String>,
    /// The journal's state word: pending, uploading, uploaded, conflict, failed.
    pub state: String,
    #[serde(default)]
    pub size: u64,
    /// When it was saved, in unix seconds; `None` when the journal predates
    /// the field and genuinely does not know.
    #[serde(default)]
    pub saved_at: Option<u64>,
}

#[derive(Default)]
pub struct RecentChanges(Mutex<VecDeque<RemoteChange>>);
impl RecentChanges {
    pub fn record(&self, change: RemoteChange) {
        if let Ok(mut ring) = self.0.lock() {
            if ring.len() == CAPACITY {
                ring.pop_front();
            }
            ring.push_back(change);
        }
    }
    /// The latest first, at most `limit`.
    pub fn list(&self, limit: usize) -> Vec<RemoteChange> {
        self.0
            .lock()
            .map(|ring| ring.iter().rev().take(limit).cloned().collect())
            .unwrap_or_default()
    }
    pub fn len(&self) -> usize {
        self.0.lock().map(|ring| ring.len()).unwrap_or(0)
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn change(n: u64) -> RemoteChange {
        RemoteChange {
            at_unix: n,
            id: format!("item-{n}"),
            parent_id: None,
            name: format!("file-{n}"),
            kind: "file".into(),
            size: n,
            removed: false,
        }
    }

    #[test]
    fn the_ring_keeps_the_latest_and_lists_them_latest_first() {
        let recent = RecentChanges::default();
        for n in 0..(CAPACITY as u64 + 5) {
            recent.record(change(n));
        }
        assert_eq!(recent.len(), CAPACITY, "bounded");
        let listed = recent.list(3);
        assert_eq!(
            listed.iter().map(|c| c.at_unix).collect::<Vec<_>>(),
            vec![
                CAPACITY as u64 + 4,
                CAPACITY as u64 + 3,
                CAPACITY as u64 + 2
            ],
            "latest first, limited"
        );
        assert_eq!(recent.list(1000).len(), CAPACITY);
        assert_eq!(
            recent.list(1000).last().unwrap().at_unix,
            5,
            "the oldest five fell off the front"
        );
    }
}
