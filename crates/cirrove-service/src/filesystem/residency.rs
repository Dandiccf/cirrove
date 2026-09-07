//! Reference-aware residency for resolved views. Child views retain parent
//! residency; the root and inconsistent kernel counts remain conservative.
use super::View;
mod invalidation;
use cirrove_core::{NodeKind, ProviderError};
pub(super) use invalidation::InvalidationCursor;
use invalidation::ProjectionIndex;
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
}
pub(super) struct NamespaceViews {
    entries: HashMap<u64, Box<Entry>>,
    candidates: VecDeque<(u64, u64)>,
    generation: u64,
    invalidation: ProjectionIndex,
}
impl NamespaceViews {
    pub(super) fn new(root: View) -> Self {
        let mut invalidation = ProjectionIndex::default();
        invalidation.insert(&root);
        Self {
            entries: HashMap::from([(
                1,
                Box::new(Entry {
                    view: root,
                    generation: 0,
                    queued: false,
                }),
            )]),
            candidates: VecDeque::new(),
            generation: 0,
            invalidation,
        }
    }
    pub(super) fn get(&self, inode: &u64) -> Option<&View> {
        self.entries.get(inode).map(|entry| &entry.view)
    }
    #[cfg(test)]
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
        view._parent_residency = Some(self.parent_lease(inode, view.parent)?);
        if let Some(entry) = self.entries.get_mut(&inode) {
            // All earlier clones (open files, operations and in-flight reads)
            // must pin the same residency even when the visible path changes.
            view.residency = entry.view.residency.clone();
            self.invalidation.remove(&entry.view);
            entry.view = view.clone();
        } else {
            self.generation = self
                .generation
                .checked_add(1)
                .ok_or(ProviderError::Protocol("namespace generation exhausted"))?;
            self.entries.insert(
                inode,
                Box::new(Entry {
                    view: view.clone(),
                    generation: self.generation,
                    queued: false,
                }),
            );
        }
        self.invalidation.insert(&view);
        self.queue(inode);
        Ok(view)
    }
    fn parent_lease(&self, inode: u64, parent: u64) -> Result<Arc<LookupRefs>, ProviderError> {
        if inode <= 1 {
            return Err(ProviderError::Protocol("reserved namespace inode"));
        }
        let entry = self.entries.get(&parent).ok_or(ProviderError::NotFound)?;
        if entry.view.node.kind != NodeKind::Folder {
            return Err(ProviderError::Protocol(
                "namespace parent is not a directory",
            ));
        }
        let lease = entry.view.residency.clone();
        let mut ancestor = parent;
        // No allocations or I/O under this map's lock. Every successful insert
        // preserves an acyclic parent graph. The entry count bounds even an
        // inconsistent preexisting chain without imposing a new path-depth cap.
        for _ in 0..self.entries.len() {
            if ancestor == inode {
                return Err(ProviderError::Protocol("namespace ancestor cycle"));
            }
            if ancestor == 1 {
                return Ok(lease);
            }
            ancestor = self
                .entries
                .get(&ancestor)
                .ok_or(ProviderError::NotFound)?
                .view
                .parent;
        }
        Err(ProviderError::Protocol("namespace ancestor cycle"))
    }
    fn queue(&mut self, inode: u64) {
        if let Some(entry) = self.entries.get_mut(&inode)
            && inode != 1
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
                if let Some(entry) = self.entries.remove(&inode) {
                    self.invalidation.remove(&entry.view);
                }
            } else {
                self.queue(inode);
            }
        }
        true
    }
    fn reclaimable(entry: &Entry) -> bool {
        entry.view.inode != 1 && entry.view.residency.kernel.load(Ordering::SeqCst) == 0
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
                if let Some(entry) = self.entries.remove(&inode) {
                    self.invalidation.remove(&entry.view);
                }
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

    pub(super) fn view(inode: u64, kind: NodeKind) -> View {
        View {
            residency: Arc::default(),
            _parent_residency: None,
            inode,
            parent: 1,
            scope: Scope {
                account: "account".into(),
                provider: "fixture".into(),
                collection: "drive".into(),
            }
            .into(),
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
            }
            .into(),
            name: format!("item-{inode}").into(),
            alias: vec![].into(),
            reference: false,
            entry: None,
            ancestry: vec![].into(),
        }
    }
    pub(super) fn cache() -> NamespaceViews {
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
        assert_eq!(cache.get(&2).unwrap().name.as_ref(), "new-name");
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
    fn collection_work_is_bounded_and_preserves_a_held_directory() {
        let mut cache = cache();
        for inode in 2..8 {
            drop(cache.insert(view(inode, NodeKind::File)).unwrap());
        }
        let held = cache.insert(view(8, NodeKind::Folder)).unwrap();
        cache.acquire_lookup(8).unwrap();
        assert!(cache.forget(8, 1));
        assert_eq!(cache.collect(2), 2);
        assert_eq!(cache.len(), 6);
        assert_eq!(cache.collect(2), 2);
        assert_eq!(cache.collect(2), 2);
        assert_eq!(cache.len(), 2);
        assert!(cache.get(&8).is_some());
        drop(held);
        assert_eq!(cache.collect(2), 1);
        assert!(cache.get(&8).is_none());
        assert!(cache.forget(1, u64::MAX));
        assert!(cache.get(&1).is_some());
    }

    fn child(cache: &mut NamespaceViews, inode: u64, parent: u64, kind: NodeKind) -> View {
        let mut node = view(inode, kind);
        node.parent = parent;
        cache.insert(node).unwrap()
    }

    #[test]
    fn child_and_queued_operation_leases_keep_the_whole_ancestor_chain() {
        let mut cache = cache();
        drop(child(&mut cache, 2, 1, NodeKind::Folder));
        drop(child(&mut cache, 3, 2, NodeKind::Folder));
        let queued = child(&mut cache, 4, 3, NodeKind::File);
        for id in 2..=4 {
            cache.acquire_lookup(id).unwrap();
        }
        for id in 2..=4 {
            assert!(cache.forget(id, 1));
        }
        assert_eq!(cache.collect(100), 0);
        assert!(cache.get(&2).is_some() && cache.get(&3).is_some());
        // The callback already owns its View even if its async body has not run.
        assert_eq!(queued.parent, 3);
        drop(queued);
        for _ in 0..3 {
            cache.collect(100);
        }
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn shared_projection_metadata_preserves_inode_encoding_and_detaches_edits() {
        use super::super::Inner;
        let parent = view(1, NodeKind::Folder);
        let first = Inner::project(&parent, view(3, NodeKind::File).node.as_ref().clone()).unwrap();
        let sibling =
            Inner::project(&parent, view(4, NodeKind::File).node.as_ref().clone()).unwrap();
        assert!(Arc::ptr_eq(&first.scope, &sibling.scope));
        assert!(Arc::ptr_eq(&first.alias, &sibling.alias));
        assert!(Arc::ptr_eq(&first.ancestry, &sibling.ancestry));
        // Persistent inode keys must remain byte-for-byte compatible with owned metadata.
        assert_eq!(
            Inner::inode_key(&first, false).unwrap(),
            r#"["content-inode-v1",["account",[],"drive","item-3"],["etag","version"],0]"#
        );
        assert_eq!(
            Inner::inode_key(&first, true).unwrap(),
            r#"["account",[],"drive","item-3"]"#
        );
        let mut changed = first.clone();
        assert!(Arc::ptr_eq(&first.node, &changed.node));
        Arc::make_mut(&mut changed.node).etag = Some("replacement".into());
        assert_eq!(first.node.etag.as_deref(), Some("version"));
        assert_ne!(
            Inner::inode_key(&first, false).unwrap(),
            Inner::inode_key(&changed, false).unwrap()
        );
        assert_eq!(
            Inner::inode_key(&first, true).unwrap(),
            Inner::inode_key(&changed, true).unwrap()
        );

        let mut node = view(5, NodeKind::Shortcut).node.as_ref().clone();
        node.target = Some(cirrove_core::RemoteRef {
            collection: "shared".into(),
            item: "target".into(),
            kind: Some(NodeKind::Folder),
        });
        let link = Inner::project(&parent, node.clone()).unwrap();
        assert_eq!(link.entry.as_deref(), Some(&node));
        assert!(link.node.target.is_none());
        assert_eq!(link.node.id, "target");
        assert_eq!(parent.scope.collection, "drive");
        assert!(parent.alias.is_empty());
        assert!(parent.ancestry.is_empty());
        assert_eq!(
            Inner::inode_key(&link, false).unwrap(),
            r#"["account",[["drive","item-5"]],"shared","target"]"#
        );
        let mut another = node;
        another.id = "another-shortcut".into();
        let another = Inner::project(&parent, another).unwrap();
        assert_ne!(
            Inner::inode_key(&link, false).unwrap(),
            Inner::inode_key(&another, false).unwrap()
        );
        // Appending a linked folder route cannot change its siblings or parent.
        assert_eq!(
            link.alias.as_ref(),
            &vec![("drive".into(), "item-5".into())]
        );
        assert_eq!(
            link.ancestry.as_ref(),
            &vec![("shared".into(), "target".into())]
        );
    }

    #[test]
    fn listing_only_projection_protects_parent_without_acquiring_kernel_references() {
        let mut cache = cache();
        let parent = child(&mut cache, 2, 1, NodeKind::Folder);
        let projected =
            super::super::Inner::project(&parent, view(3, NodeKind::File).node.as_ref().clone())
                .unwrap();
        drop(parent);
        assert_eq!(cache.collect(100), 0);
        assert_eq!(cache.len(), 2);
        drop(projected);
        assert_eq!(cache.collect(100), 1);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn moving_a_view_preserves_old_and_new_parent_leases_until_their_users_close() {
        let mut cache = cache();
        drop(child(&mut cache, 2, 1, NodeKind::Folder));
        drop(child(&mut cache, 3, 1, NodeKind::Folder));
        let old = child(&mut cache, 4, 2, NodeKind::File);
        let new = child(&mut cache, 4, 3, NodeKind::File);
        assert!(Arc::ptr_eq(&old.residency, &new.residency));
        assert_eq!(cache.collect(100), 0);
        drop(old);
        cache.collect(100);
        assert!(cache.get(&2).is_none());
        assert!(cache.get(&3).is_some());
        drop(new);
        for _ in 0..2 {
            cache.collect(100);
        }
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn ancestor_cycles_and_missing_parents_are_rejected_without_changing_live_views() {
        let mut cache = cache();
        drop(child(&mut cache, 2, 1, NodeKind::Folder));
        drop(child(&mut cache, 3, 2, NodeKind::Folder));
        let mut cycle = view(2, NodeKind::Folder);
        cycle.parent = 3;
        assert!(cache.insert(cycle).is_err());
        assert_eq!(cache.get(&2).unwrap().parent, 1);
        let mut missing = view(4, NodeKind::File);
        missing.parent = 99;
        assert!(matches!(
            cache.insert(missing),
            Err(ProviderError::NotFound)
        ));
        assert!(cache.get(&4).is_none());
        assert!(cache.insert(view(1, NodeKind::Folder)).is_err());
        for _ in 0..2 {
            cache.collect(100);
        }
        assert_eq!(cache.len(), 1);
    }
}
