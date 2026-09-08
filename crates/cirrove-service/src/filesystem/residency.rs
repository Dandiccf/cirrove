//! Reference-aware residency for resolved views. Child views retain parent
//! residency; the root and inconsistent kernel counts remain conservative.
use super::View;
mod invalidation;
use cirrove_core::{NodeKind, ProviderError};
pub(super) use invalidation::InvalidationCursor;
use invalidation::ProjectionIndex;
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

struct Entry {
    view: View,
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
    pub(super) fn new(root: View) -> Self {
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
    pub(super) fn get(&self, inode: &u64) -> Option<&View> {
        self.entries.get(inode).map(|entry| &entry.view)
    }
    #[cfg(test)]
    pub(super) fn values(&self) -> impl Iterator<Item = &View> {
        self.entries.values().map(|entry| &entry.view)
    }
    /// Resident view count. Not test-only: the reclamation tick needs it to tell
    /// a mount that has shed what it was holding from one that never held it, and
    /// a shipped daemon that cannot count its own views cannot report on them.
    pub(super) fn len(&self) -> usize {
        self.entries.len()
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
            lease_protected += usize::from(Arc::strong_count(&entry.view.residency) > 1);
            quarantined += usize::from(entry.view.residency.quarantine.load(Ordering::SeqCst));
        }
        let (identity_keys, inode_keys) = (self.invalidation.count(), self.entries.len());
        serde_json::json!({"views":self.entries.len(),"map_storage":"btree",
            "kernel_referenced_views":kernel_referenced,"lease_protected_views":lease_protected,
            "quarantined_views":quarantined,"candidate_entries":self.candidates.len(),
            "candidate_capacity":self.candidates.capacity(),"identity_index_entries":identity_keys,
            "inode_index_entries":inode_keys})
    }

    pub(super) fn insert(&mut self, mut view: View) -> Result<View, ProviderError> {
        let inode = view.inode;
        view._parent_residency = Some(self.parent_lease(inode, view.parent)?);
        // Alias routes keep distinct lifetimes/inodes. Exactly equal immutable
        // metadata can reuse an already live projection, without an intern cache.
        let shared = self
            .invalidation
            .matches(&view.scope, &view.node.id)
            .find_map(|candidate| {
                let existing = &self.entries.get(&candidate)?.view;
                (existing.scope == view.scope && existing.node == view.node).then(|| {
                    (
                        existing.scope.clone(),
                        existing.node.clone(),
                        existing.name.clone(),
                    )
                })
            });
        if let Some((scope, node, name)) = shared {
            view.scope = scope;
            view.node = node;
            if view.name == name {
                view.name = name;
            }
        }
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
                Entry {
                    view: view.clone(),
                    generation: self.generation,
                    queued: false,
                },
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
        // Examine each queued candidate once, but start the count again after a
        // removal: retiring a child can release an ancestor already examined in
        // this pass, and `limit` still bounds the total work either way.
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
                unchanged = self.candidates.len();
            } else if entry.view.residency.kernel.load(Ordering::SeqCst) == 0 {
                self.queue(inode);
            }
        }
        // The queue only grows while lookups retire faster than this drains, so
        // its capacity is a high-water mark of past churn rather than of current
        // work. Give it back once a burst has passed; halving is the threshold so
        // a steady workload never reallocates.
        if self.candidates.capacity() > 64 && self.candidates.len() * 2 < self.candidates.capacity()
        {
            self.candidates.shrink_to_fit();
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
    fn equal_alias_payloads_share_without_reusing_versions_accounts_or_lifetimes() {
        let mut cache = cache();
        let first = cache.insert(view(2, NodeKind::File)).unwrap();
        let mut alias = first.clone();
        alias.inode = 3;
        alias.residency = Arc::default();
        alias.node = Arc::new(first.node.as_ref().clone());
        alias.name = first.name.as_ref().into();
        alias.alias = vec![("drive".into(), "second-link".into())].into();
        assert!(!Arc::ptr_eq(&first.node, &alias.node));
        let alias = cache.insert(alias).unwrap();
        assert!(Arc::ptr_eq(&first.node, &alias.node));
        assert!(Arc::ptr_eq(&first.name, &alias.name));
        assert!(!Arc::ptr_eq(&first.residency, &alias.residency));
        assert_ne!(
            super::super::Inner::inode_key(&first, false).unwrap(),
            super::super::Inner::inode_key(&alias, false).unwrap()
        );
        let mut changed = alias.clone();
        changed.inode = 4;
        changed.residency = Arc::default();
        Arc::make_mut(&mut changed.node).etag = Some("new-version".into());
        let changed = cache.insert(changed).unwrap();
        assert!(!Arc::ptr_eq(&first.node, &changed.node));
        assert_eq!(first.node.etag.as_deref(), Some("version"));
        let mut other = first.clone();
        other.inode = 5;
        other.residency = Arc::default();
        other.node = Arc::new(first.node.as_ref().clone());
        Arc::make_mut(&mut other.scope).account = "other-account".into();
        let other = cache.insert(other).unwrap();
        assert!(!Arc::ptr_eq(&first.node, &other.node));
        drop((first, alias, changed, other));
        cache.collect(128);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn boxed_targets_keep_the_existing_node_json_format() {
        let encoded = r#"{"id":"link","parent_id":"root","name":"Shared","kind":"shortcut","size":0,"modified_unix":0,"etag":null,"content_version":null,"target":{"collection":"shared","item":"target","kind":"folder"}}"#;
        let node: Node = serde_json::from_str(encoded).unwrap();
        assert_eq!(serde_json::to_string(&node).unwrap(), encoded);
        assert_eq!(node.target.as_ref().unwrap().item, "target");
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
    fn one_pass_retires_a_whole_ancestor_chain_within_its_budget() {
        let mut cache = cache();
        for inode in 2..=9 {
            drop(child(&mut cache, inode, inode - 1, NodeKind::Folder));
        }
        for inode in 2..=9 {
            cache.acquire_lookup(inode).unwrap();
        }
        // Released deepest last, so every ancestor is queued before the leaf
        // that holds it and is examined before that leaf is removed.
        for inode in 2..=9 {
            assert!(cache.forget(inode, 1));
        }
        // `forget` already retires the leaf, leaving seven held ancestors. All
        // of them go in ONE pass, well inside the budget. Counting only the
        // queue length at entry retires one level per pass instead, which costs
        // a second per ancestor against the collector's one-second tick.
        assert_eq!(cache.len(), 8);
        assert_eq!(cache.collect(100), 7);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn collect_still_honours_its_limit_when_a_removal_restarts_the_count() {
        let mut cache = cache();
        for inode in 2..=9 {
            drop(child(&mut cache, inode, inode - 1, NodeKind::Folder));
        }
        for inode in 2..=9 {
            cache.acquire_lookup(inode).unwrap();
        }
        for inode in 2..=9 {
            assert!(cache.forget(inode, 1));
        }
        // Restarting the count after a removal must not let a cascade run past
        // the caller's budget: three examinations reach no reclaimable ancestor,
        // because the only one is queued behind the six that still hold leases.
        assert_eq!(cache.collect(3), 0);
        assert_eq!(cache.len(), 8);
        assert_eq!(cache.collect(100), 7);
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
        node.target = Some(Box::new(cirrove_core::RemoteRef {
            collection: "shared".into(),
            item: "target".into(),
            kind: Some(NodeKind::Folder),
        }));
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
