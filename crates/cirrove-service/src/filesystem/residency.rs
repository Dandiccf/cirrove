//! Reference-aware residency for resolved views. Child views retain parent
//! residency; the root and inconsistent kernel counts remain conservative.
use super::{View, payloads};
mod invalidation;
use cirrove_core::{NodeKind, ProviderError};
pub(super) use invalidation::InvalidationCursor;
use invalidation::{Key, ProjectionIndex};
use std::{
    collections::{BTreeMap, VecDeque},
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

/// Compact immutable identity and lifetime record. Captured headers protect a
/// queued callback even before its metadata is loaded on a blocking worker.
pub(super) struct Header {
    pub inode: u64,
    pub parent: u64,
    pub directory: bool,
    pub name_bytes: usize,
    residency: Arc<LookupRefs>,
    parent_residency: Option<Arc<LookupRefs>>,
    primary: Arc<Key>,
    source: Option<Arc<Key>>,
    record: payloads::Record,
}
impl Header {
    fn new(view: &View, record: payloads::Record) -> Self {
        let (primary, source) = invalidation::keys(view);
        Self {
            inode: view.inode,
            parent: view.parent,
            directory: view.node.kind == NodeKind::Folder,
            name_bytes: view.name.len(),
            residency: view.residency.clone(),
            parent_residency: view._parent_residency.clone(),
            primary,
            source,
            record,
        }
    }
    /// Never call while holding the namespace lock.
    pub fn load(&self) -> std::io::Result<View> {
        let data = self.record.load()?;
        if data.scope != self.primary.scope
            || data.node.id.as_str() != self.primary.item.as_ref()
            || (data.node.kind == NodeKind::Folder) != self.directory
            || data.name.len() != self.name_bytes
        {
            return Err(std::io::Error::other("projection identity mismatch"));
        }
        Ok(View {
            inode: self.inode,
            parent: self.parent,
            residency: self.residency.clone(),
            _parent_residency: self.parent_residency.clone(),
            data,
        })
    }
}
struct Entry {
    view: Arc<Header>,
    generation: u64,
    queued: bool,
}
pub(super) struct NamespaceViews {
    entries: BTreeMap<u64, Entry>,
    candidates: VecDeque<(u64, u64)>,
    generation: u64,
    invalidation: ProjectionIndex,
}
impl NamespaceViews {
    pub(super) fn new(root: View, record: payloads::Record) -> Self {
        let root = Arc::new(Header::new(&root, record));
        let mut invalidation = ProjectionIndex::default();
        invalidation.insert(&root);
        Self {
            entries: BTreeMap::from([(
                1,
                Entry {
                    view: root,
                    generation: 0,
                    queued: false,
                },
            )]),
            candidates: VecDeque::new(),
            generation: 0,
            invalidation,
        }
    }
    pub(super) fn get(&self, inode: &u64) -> Option<&Arc<Header>> {
        self.entries.get(inode).map(|entry| &entry.view)
    }
    #[cfg(test)]
    pub(super) fn values(&self) -> impl Iterator<Item = &Arc<Header>> {
        self.entries.values().map(|entry| &entry.view)
    }
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }
    #[cfg(test)]
    pub(super) fn capacity(&self) -> Option<usize> {
        None
    }

    /// Test diagnostics count references, not allocator or shared payload bytes.
    #[cfg(test)]
    pub(super) fn diagnostics(&self) -> serde_json::Value {
        let mut kernel_referenced = 0;
        let mut lease_protected = 0;
        let mut quarantined = 0;
        for entry in self.entries.values() {
            kernel_referenced +=
                usize::from(entry.view.residency.kernel.load(Ordering::SeqCst) > 0);
            lease_protected += usize::from(
                Arc::strong_count(&entry.view.residency) > 1 || Arc::strong_count(&entry.view) > 1,
            );
            quarantined += usize::from(entry.view.residency.quarantine.load(Ordering::SeqCst));
        }
        let (identity_keys, inode_keys) = (self.invalidation.count(), self.entries.len());
        serde_json::json!({"views":self.entries.len(),"map_capacity":null,"map_storage":"btree",
            "kernel_referenced_views":kernel_referenced,"lease_protected_views":lease_protected,
            "quarantined_views":quarantined,"candidate_entries":self.candidates.len(),
            "candidate_capacity":self.candidates.capacity(),"identity_index_entries":identity_keys,
            "inode_index_entries":inode_keys})
    }

    #[cfg(test)]
    pub(super) fn insert(&mut self, view: View) -> Result<View, ProviderError> {
        let record = self
            .entries
            .get(&1)
            .ok_or(ProviderError::NotFound)?
            .view
            .record
            .store
            .save(view.data.clone())
            .map_err(|_| ProviderError::Unavailable)?;
        self.publish(view, record)
    }
    pub(super) fn publish(
        &mut self,
        mut view: View,
        record: payloads::Record,
    ) -> Result<View, ProviderError> {
        let inode = view.inode;
        view._parent_residency = Some(self.parent_lease(inode, view.parent)?);
        if let Some(entry) = self.entries.get_mut(&inode) {
            // All earlier clones (open files, operations and in-flight reads)
            // must pin the same residency even when the visible path changes.
            view.residency = entry.view.residency.clone();
            self.invalidation.remove(&entry.view);
            entry.view = Arc::new(Header::new(&view, record));
        } else {
            self.generation = self
                .generation
                .checked_add(1)
                .ok_or(ProviderError::Protocol("namespace generation exhausted"))?;
            self.entries.insert(
                inode,
                Entry {
                    view: Arc::new(Header::new(&view, record)),
                    generation: self.generation,
                    queued: false,
                },
            );
        }
        self.invalidation.insert(&self.entries[&inode].view);
        self.queue(inode);
        Ok(view)
    }
    fn parent_lease(&self, inode: u64, parent: u64) -> Result<Arc<LookupRefs>, ProviderError> {
        if inode <= 1 {
            return Err(ProviderError::Protocol("reserved namespace inode"));
        }
        let entry = self.entries.get(&parent).ok_or(ProviderError::NotFound)?;
        if !entry.view.directory {
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
            && Arc::strong_count(&entry.view) == 1
    }
    pub(super) fn collect(&mut self, limit: usize) -> usize {
        let mut removed = 0;
        let mut unchanged = self.candidates.len();
        for _ in 0..limit {
            if unchanged == 0 {
                break;
            }
            unchanged -= 1;
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
                // A retired child can release an ancestor examined earlier in
                // this pass. Revisit it within the same total work budget.
                unchanged = self.candidates.len();
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
            data: Arc::new(super::super::Projection {
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
            }),
        }
    }
    pub(super) fn cache() -> NamespaceViews {
        {
            let root = view(1, NodeKind::Folder);
            let store = payloads::Store::limited(16 * 1024 * 1024, 1_000_000, 262144).unwrap();
            let record = store.save(root.data.clone()).unwrap();
            NamespaceViews::new(root, record)
        }
    }

    #[test]
    fn retired_children_release_earlier_candidates_within_the_same_work_budget() {
        let mut cache = cache();
        let mut captures = Vec::new();
        for inode in 2..=20 {
            drop(child(&mut cache, inode, inode - 1, NodeKind::Folder));
            captures.push(cache.get(&inode).unwrap().clone());
        }
        assert_eq!(cache.collect(4096), 0);
        drop(captures);
        assert_eq!(cache.collect(4096), 19);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn captured_evicted_records_survive_moves_forget_and_replacement() {
        let root = view(1, NodeKind::Folder);
        let store = payloads::Store::limited(0, 32, 512).unwrap();
        let record = store.save(root.data.clone()).unwrap();
        let mut cache = NamespaceViews::new(root, record);
        drop(child(&mut cache, 2, 1, NodeKind::Folder));
        drop(child(&mut cache, 3, 1, NodeKind::Folder));
        let original = child(&mut cache, 4, 2, NodeKind::File);
        let old = cache.get(&4).unwrap().clone();
        cache.acquire_lookup(4).unwrap();
        let mut moved = original.clone();
        moved.parent = 3;
        moved.name = "renamed".into();
        Arc::make_mut(&mut moved.node).etag = Some("replacement".into());
        drop(cache.insert(moved).unwrap());
        drop(original);
        assert!(cache.forget(4, 1));
        for _ in 0..3 {
            cache.collect(32);
        }
        assert_eq!(
            cache.len(),
            4,
            "old capture protects both the inode and its old ancestor"
        );
        store.collect().unwrap();
        let restored = old.load().unwrap();
        assert_eq!(restored.parent, 2);
        assert_eq!(restored.name.as_ref(), "item-4");
        assert_eq!(restored.node.etag.as_deref(), Some("version"));
        let current = cache.get(&4).unwrap().clone();
        assert_eq!(current.load().unwrap().name.as_ref(), "renamed");
        assert_eq!(store.usage().0, 0, "all payloads were reloaded from disk");
        drop((old, restored, current));
        for _ in 0..4 {
            cache.collect(32);
        }
        store.collect().unwrap();
        assert_eq!(cache.len(), 1);
        assert_eq!(store.usage().2, 1);
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
        assert_eq!(
            cache.get(&2).unwrap().load().unwrap().name.as_ref(),
            "new-name"
        );
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
