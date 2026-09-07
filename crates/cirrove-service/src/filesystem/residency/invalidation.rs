//! Reverse indexes select live aliases and revisions without cloning the view map.
use super::*;
use cirrove_core::Scope;
use cirrove_store::{MetadataChange, MetadataChangeKind};
use std::{collections::BTreeSet, ops::Bound};
type Key = (String, String, String, String, u64);
const ENTRIES: usize = 128;
const BYTES: usize = 64 * 1024;
#[derive(Default)]
pub(super) struct ProjectionIndex {
    identities: BTreeSet<Key>,
    all: BTreeSet<u64>,
}
#[derive(Default)]
pub(in crate::filesystem) struct InvalidationCursor {
    key: Option<Key>,
    inode: Option<u64>,
}
pub(in crate::filesystem) struct Invalidation {
    pub inode: u64,
    pub parent: u64,
    pub name: String,
    pub directory: bool,
    pub entry: bool,
}
pub(in crate::filesystem) struct InvalidationBatch {
    pub entries: Vec<Invalidation>,
    pub next: InvalidationCursor,
    pub complete: bool,
}
fn key(scope: &Scope, item: &str, inode: u64) -> Key {
    (
        scope.account.clone(),
        scope.provider.clone(),
        scope.collection.clone(),
        item.into(),
        inode,
    )
}
fn keys(view: &View) -> impl Iterator<Item = Key> {
    let primary = key(&view.scope, &view.node.id, view.inode);
    let source = view.entry.as_ref().and_then(|entry| {
        view.alias.last().map(|(collection, _)| {
            (
                view.scope.account.clone(),
                view.scope.provider.clone(),
                collection.clone(),
                entry.id.clone(),
                view.inode,
            )
        })
    });
    std::iter::once(primary).chain(source)
}
impl ProjectionIndex {
    pub(super) fn insert(&mut self, view: &View) {
        self.all.insert(view.inode);
        self.identities.extend(keys(view));
    }
    pub(super) fn remove(&mut self, view: &View) {
        self.all.remove(&view.inode);
        for key in keys(view) {
            self.identities.remove(&key);
        }
    }
}
impl NamespaceViews {
    /// None explicitly requests the conservative local-change/full-recovery path.
    /// Every call examines at most 128 index entries and copies bounded names;
    /// callers release the namespace lock before notifying the kernel.
    pub(in crate::filesystem) fn invalidation_batch(
        &self,
        change: Option<&MetadataChange>,
        mut cursor: InvalidationCursor,
    ) -> InvalidationBatch {
        let mut entries = Vec::new();
        let mut bytes = 0;
        let mut complete = true;
        let mut visited = 0;
        let mut push = |inode| {
            if inode == 1 {
                return true;
            }
            let Some(view) = self.get(&inode) else {
                return true;
            };
            if change.is_some_and(|c| c.kind == MetadataChangeKind::Directory)
                && view.node.kind != NodeKind::Folder
            {
                return true;
            }
            if !entries.is_empty() && bytes + view.name.len() > BYTES {
                return false;
            }
            bytes += view.name.len();
            entries.push(Invalidation {
                inode,
                parent: view.parent,
                name: view.name.clone(),
                directory: view.node.kind == NodeKind::Folder,
                entry: !change.is_some_and(|c| c.kind == MetadataChangeKind::Directory),
            });
            true
        };
        if let Some(change) = change {
            let lower = key(
                &change.scope,
                if change.kind == MetadataChangeKind::Scope {
                    ""
                } else {
                    &change.identity
                },
                0,
            );
            let start = cursor
                .key
                .clone()
                .map_or(Bound::Included(lower), Bound::Excluded);
            for key in self
                .invalidation
                .identities
                .range((start, Bound::Unbounded))
            {
                if key.0 != change.scope.account
                    || key.1 != change.scope.provider
                    || key.2 != change.scope.collection
                    || (change.kind != MetadataChangeKind::Scope && key.3 != change.identity)
                {
                    break;
                }
                if visited == ENTRIES || !push(key.4) {
                    complete = false;
                    break;
                }
                visited += 1;
                cursor.key = Some(key.clone());
            }
        } else {
            let start = cursor.inode.map_or(Bound::Unbounded, Bound::Excluded);
            for inode in self.invalidation.all.range((start, Bound::Unbounded)) {
                if visited == ENTRIES || !push(*inode) {
                    complete = false;
                    break;
                }
                visited += 1;
                cursor.inode = Some(*inode);
            }
        }
        InvalidationBatch {
            entries,
            next: cursor,
            complete,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::super::tests::{cache, view};
    use super::*;
    fn change(view: &View, kind: MetadataChangeKind) -> MetadataChange {
        MetadataChange {
            scope: view.scope.clone(),
            kind,
            identity: view.node.id.clone(),
        }
    }
    fn selected(cache: &NamespaceViews, change: Option<&MetadataChange>) -> Vec<u64> {
        let mut cursor = InvalidationCursor::default();
        let mut ids = Vec::new();
        loop {
            let batch = cache.invalidation_batch(change, cursor);
            assert!(batch.entries.len() <= ENTRIES);
            ids.extend(batch.entries.iter().map(|e| e.inode));
            cursor = batch.next;
            if batch.complete {
                break;
            }
        }
        ids
    }
    #[test]
    fn identity_selection_preserves_aliases_versions_and_account_boundaries() {
        let mut cache = cache();
        let mut first = view(2, NodeKind::File);
        first.node.id = "target".into();
        let target = change(&first, MetadataChangeKind::Item);
        let held = cache.insert(first.clone()).unwrap();
        let mut version = first.clone();
        version.inode = 3;
        version.residency = Arc::default();
        version.node.etag = Some("new".into());
        let held_version = cache.insert(version).unwrap();
        let mut link = first.clone();
        link.inode = 4;
        link.residency = Arc::default();
        link.alias = vec![("source-drive".into(), "shortcut".into())];
        let mut entry = link.node.clone();
        entry.id = "shortcut".into();
        link.entry = Some(entry);
        let held_link = cache.insert(link).unwrap();
        let mut other = first.clone();
        other.inode = 5;
        other.residency = Arc::default();
        other.scope.account = "another-account".into();
        let held_other = cache.insert(other).unwrap();
        assert_eq!(selected(&cache, Some(&target)), vec![2, 3, 4]);
        let mut source = target.clone();
        source.scope.collection = "source-drive".into();
        source.identity = "shortcut".into();
        assert_eq!(selected(&cache, Some(&source)), vec![4]);
        let mut moved = held_link.clone();
        moved.alias = vec![("another-source".into(), "shortcut".into())];
        drop(cache.insert(moved).unwrap());
        assert!(selected(&cache, Some(&source)).is_empty());
        drop((first, held, held_version, held_link, held_other));
        cache.collect(128);
        assert_eq!(cache.len(), 1);
        assert!(cache.invalidation.identities.iter().all(|k| k.4 == 1));
    }
    #[test]
    fn full_and_scope_sweeps_page_live_entries_and_tolerate_retirement_between_pages() {
        let mut cache = cache();
        let mut held = Vec::new();
        for inode in 2..1002 {
            held.push(cache.insert(view(inode, NodeKind::File)).unwrap());
        }
        let mut scope = change(&held[0], MetadataChangeKind::Scope);
        scope.identity.clear();
        assert_eq!(selected(&cache, Some(&scope)).len(), 1000);
        assert_eq!(selected(&cache, None).len(), 1000);
        let first = cache.invalidation_batch(None, InvalidationCursor::default());
        assert!(!first.complete);
        drop(held);
        cache.collect(4096);
        let next = cache.invalidation_batch(None, first.next);
        assert!(next.complete);
        assert!(next.entries.is_empty());
        assert_eq!(cache.invalidation.all.len(), 1);
    }
}
