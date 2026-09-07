//! Conservative residency for resolved regular-file views. Directory ancestry
//! remains pinned until its separate lifetime model is implemented.
use super::View;
use cirrove_core::{NodeKind, ProviderError};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

#[derive(Default)]
pub(super) struct LookupRefs {
    kernel: AtomicU64,
    quarantine: AtomicBool,
}

struct Entry {
    view: View,
    generation: u64,
    queued: bool,
    directory: bool,
}
pub(super) struct NamespaceViews {
    entries: HashMap<u64, Box<Entry>>,
    candidates: VecDeque<(u64, u64)>,
    generation: u64,
}
impl NamespaceViews {
    pub(super) fn new(root: View) -> Self {
        Self {
            entries: HashMap::from([(
                1,
                Box::new(Entry {
                    view: root,
                    generation: 0,
                    queued: false,
                    directory: true,
                }),
            )]),
            candidates: VecDeque::new(),
            generation: 0,
        }
    }
    pub(super) fn get(&self, inode: &u64) -> Option<&View> {
        self.entries.get(inode).map(|entry| &entry.view)
    }
    pub(super) fn values(&self) -> impl Iterator<Item = &View> {
        self.entries.values().map(|entry| &entry.view)
    }
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }
    #[cfg(test)]
    pub(super) fn capacity(&self) -> usize {
        self.entries.capacity()
    }

    pub(super) fn insert(&mut self, mut view: View) -> Result<View, ProviderError> {
        let inode = view.inode;
        if let Some(entry) = self.entries.get_mut(&inode) {
            // All earlier clones (open files, operations and in-flight reads)
            // must pin the same residency even when the visible path changes.
            view.residency = entry.view.residency.clone();
            entry.directory |= view.node.kind != NodeKind::File;
            entry.view = view.clone();
        } else {
            self.generation = self
                .generation
                .checked_add(1)
                .ok_or(ProviderError::Protocol("namespace generation exhausted"))?;
            self.entries.insert(
                inode,
                Box::new(Entry {
                    directory: view.node.kind != NodeKind::File,
                    view: view.clone(),
                    generation: self.generation,
                    queued: false,
                }),
            );
        }
        self.queue(inode);
        Ok(view)
    }
    fn queue(&mut self, inode: u64) {
        if let Some(entry) = self.entries.get_mut(&inode)
            && !entry.directory
            && !entry.queued
            && !entry.view.residency.quarantine.load(Ordering::SeqCst)
        {
            entry.queued = true;
            self.candidates.push_back((inode, entry.generation));
        }
    }
    pub(super) fn acquire_lookup(&mut self, inode: u64) -> Result<(), ProviderError> {
        let entry = self.entries.get(&inode).ok_or(ProviderError::NotFound)?;
        let refs = &entry.view.residency;
        let count = refs.kernel.load(Ordering::SeqCst);
        let Some(next) = count.checked_add(1) else {
            refs.quarantine.store(true, Ordering::SeqCst);
            return Err(ProviderError::Protocol(
                "namespace reference count exhausted",
            ));
        };
        refs.kernel.store(next, Ordering::SeqCst);
        Ok(())
    }
    /// Returns false for an inconsistent reference count. Preserve the view in
    /// that case rather than turning an accounting error into use-after-forget.
    pub(super) fn forget(&mut self, inode: u64, count: u64) -> bool {
        if inode == 1 || count == 0 {
            return true;
        }
        let Some(entry) = self.entries.get(&inode) else {
            return false;
        };
        let refs = &entry.view.residency;
        let Some(next) = refs.kernel.load(Ordering::SeqCst).checked_sub(count) else {
            refs.quarantine.store(true, Ordering::SeqCst);
            return false;
        };
        refs.kernel.store(next, Ordering::SeqCst);
        if next == 0 {
            if Self::reclaimable(entry) {
                self.entries.remove(&inode);
            } else {
                self.queue(inode);
            }
        }
        true
    }
    fn reclaimable(entry: &Entry) -> bool {
        !entry.directory && entry.view.residency.kernel.load(Ordering::SeqCst) == 0
            && !entry.view.residency.quarantine.load(Ordering::SeqCst)
            // NamespaceViews is locked by the caller. At count one, no external
            // View exists from which another thread could clone this token.
            && Arc::strong_count(&entry.view.residency) == 1
    }
    pub(super) fn collect(&mut self, limit: usize) -> usize {
        let mut removed = 0;
        for _ in 0..limit.min(self.candidates.len()) {
            let Some((inode, generation)) = self.candidates.pop_front() else {
                break;
            };
            let Some(entry) = self
                .entries
                .get_mut(&inode)
                .filter(|e| e.generation == generation)
            else {
                continue;
            };
            entry.queued = false;
            if Self::reclaimable(entry) {
                self.entries.remove(&inode);
                removed += 1;
            } else if entry.view.residency.kernel.load(Ordering::SeqCst) == 0 {
                self.queue(inode);
            }
        }
        removed
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use cirrove_core::{Node, Scope};

    fn view(inode: u64, kind: NodeKind) -> View {
        View {
            residency: Arc::default(),
            inode,
            parent: 1,
            scope: Scope {
                account: "account".into(),
                provider: "fixture".into(),
                collection: "drive".into(),
            },
            node: Node {
                id: format!("item-{inode}"),
                parent_id: Some("root".into()),
                name: format!("item-{inode}"),
                kind,
                size: 0,
                modified_unix: 0,
                etag: Some("version".into()),
                content_version: None,
                target: None,
            },
            name: format!("item-{inode}"),
            alias: vec![],
            reference: false,
            entry: None,
            ancestry: vec![],
        }
    }
    fn cache() -> NamespaceViews {
        NamespaceViews::new(view(1, NodeKind::Folder))
    }

    #[test]
    fn kernel_references_and_cloned_views_both_protect_residency() {
        let mut cache = cache();
        let open = cache.insert(view(2, NodeKind::File)).unwrap();
        cache.acquire_lookup(2).unwrap();
        cache.acquire_lookup(2).unwrap();
        assert!(cache.forget(2, 1));
        assert_eq!(cache.collect(128), 0);
        assert!(cache.forget(2, 1));
        assert_eq!(
            cache.collect(128),
            0,
            "open/operation clone must survive FORGET"
        );
        let in_flight = open.clone();
        drop(open);
        assert_eq!(cache.collect(128), 0);
        drop(in_flight);
        assert_eq!(cache.collect(128), 1);
        assert!(cache.get(&2).is_none());
        assert!(cache.get(&1).is_some());
    }

    #[test]
    fn published_path_change_reuses_the_old_open_views_residency() {
        let mut cache = cache();
        let old = cache.insert(view(2, NodeKind::File)).unwrap();
        let mut moved = view(2, NodeKind::File);
        moved.name = "new-name".into();
        let latest = cache.insert(moved).unwrap();
        assert!(Arc::ptr_eq(&old.residency, &latest.residency));
        drop(latest);
        assert_eq!(cache.collect(128), 0);
        assert_eq!(cache.get(&2).unwrap().name, "new-name");
        drop(old);
        assert_eq!(cache.collect(128), 1);
    }

    #[test]
    fn stale_candidate_cannot_reclaim_a_new_generation_of_the_inode() {
        let mut cache = cache();
        drop(cache.insert(view(2, NodeKind::File)).unwrap());
        cache.acquire_lookup(2).unwrap();
        assert!(cache.forget(2, 1));
        assert!(cache.get(&2).is_none());
        drop(cache.insert(view(2, NodeKind::File)).unwrap());
        cache.acquire_lookup(2).unwrap();
        assert_eq!(cache.collect(128), 0);
        assert!(cache.get(&2).is_some());
        assert!(cache.forget(2, 1));
        assert!(cache.get(&2).is_none());
    }

    #[test]
    fn reference_underflow_and_overflow_preserve_affected_entries() {
        let mut cache = cache();
        drop(cache.insert(view(2, NodeKind::File)).unwrap());
        cache.acquire_lookup(2).unwrap();
        assert!(!cache.forget(2, 2));
        assert!(cache.forget(2, 1));
        assert_eq!(cache.collect(128), 0);
        assert!(cache.get(&2).is_some());
        assert!(!cache.forget(99, 1));
        let live = cache.insert(view(3, NodeKind::File)).unwrap();
        live.residency.kernel.store(u64::MAX, Ordering::SeqCst);
        assert!(cache.acquire_lookup(3).is_err());
        live.residency.kernel.store(0, Ordering::SeqCst);
        drop(live);
        assert_eq!(cache.collect(128), 0);
    }

    #[test]
    fn collection_work_is_bounded_and_does_not_discard_directory_ancestry() {
        let mut cache = cache();
        for inode in 2..8 {
            drop(cache.insert(view(inode, NodeKind::File)).unwrap());
        }
        drop(cache.insert(view(8, NodeKind::Folder)).unwrap());
        cache.acquire_lookup(8).unwrap();
        assert!(cache.forget(8, 1));
        assert_eq!(cache.collect(2), 2);
        assert_eq!(cache.len(), 6);
        assert_eq!(cache.collect(2), 2);
        assert_eq!(cache.collect(2), 2);
        assert_eq!(cache.len(), 2);
        assert!(cache.get(&8).is_some());
        assert!(cache.forget(1, u64::MAX));
        assert!(cache.get(&1).is_some());
    }
}
