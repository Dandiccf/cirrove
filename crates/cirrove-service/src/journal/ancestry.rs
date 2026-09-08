//! Retain observed routes to local changes, including links between collections.
//! These snapshots never enqueue folder recreation or grant provider permission.
use super::*;
use std::collections::{HashMap, HashSet};
type Identity = (String, String, String, String);
fn identity(scope: &Scope, item: &str) -> Identity {
    (
        scope.account.clone(),
        scope.provider.clone(),
        scope.collection.clone(),
        item.into(),
    )
}
#[derive(Default)]
pub(crate) struct RetainedAncestors {
    objects: HashSet<Uuid>,
    directories: HashSet<Identity>,
    slots: HashMap<(Identity, String), HashSet<Uuid>>,
}
impl RetainedAncestors {
    pub(crate) fn new<'a>(objects: impl IntoIterator<Item = &'a NamespaceObject>) -> Self {
        let objects = objects.into_iter().collect::<Vec<_>>();
        let mut folders = HashMap::new();
        let mut links: HashMap<Identity, Vec<usize>> = HashMap::new();
        for (index, object) in objects.iter().enumerate().filter(|(_, o)| !o.unlinked) {
            if object.node.kind == NodeKind::Folder {
                folders.insert(identity(&object.scope, &object.node.id), index);
            }
            if let Some(target) = &object.node.target {
                let mut scope = object.scope.clone();
                scope.collection = target.collection.clone();
                links
                    .entry(identity(&scope, &target.item))
                    .or_default()
                    .push(index);
            }
        }
        let mut pending = objects
            .iter()
            .enumerate()
            .filter(|(_, o)| !o.follows_remote && !o.unlinked)
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        let mut result = Self::default();
        while let Some(index) = pending.pop() {
            let object = objects[index];
            if !result.objects.insert(object.id) {
                continue;
            }
            if let Some(slot) = slot(object) {
                result.slots.entry(slot).or_default().insert(object.id);
            }
            if object.node.kind == NodeKind::Folder {
                result
                    .directories
                    .insert(identity(&object.scope, &object.node.id));
            }
            if let Some(target) = &object.node.target
                && target.kind.as_ref().is_none_or(|k| *k == NodeKind::Folder)
            {
                let mut scope = object.scope.clone();
                scope.collection = target.collection.clone();
                result.directories.insert(identity(&scope, &target.item));
            }
            if let Some(parent) = &object.node.parent_id {
                let key = identity(&object.scope, parent);
                if let Some(index) = folders.get(&key) {
                    pending.push(*index);
                }
                if let Some(indices) = links.get(&key) {
                    pending.extend(indices);
                }
            }
            // A link addresses provider identity; local parents address local
            // identity. Do not collapse these domains when looking up a route.
            if let Some(remote) = &object.remote
                && let Some(indices) = links.get(&identity(&object.scope, &remote.id))
            {
                pending.extend(indices);
            }
        }
        result
    }
    fn check_slot(&self, object: &NamespaceObject) -> Result<()> {
        if slot(object)
            .and_then(|s| self.slots.get(&s))
            .is_some_and(|owners| owners.iter().any(|id| *id != object.id))
        {
            return Err(JournalError::Stale);
        }
        Ok(())
    }
    pub(crate) fn contains(&self, id: Uuid) -> bool {
        self.objects.contains(&id)
    }
    pub(crate) fn directory(&self, scope: &Scope, item: &str) -> bool {
        self.directories.contains(&identity(scope, item))
    }
}
fn slot(object: &NamespaceObject) -> Option<(Identity, String)> {
    let parent = object.node.parent_id.as_ref()?;
    let name = match object.names {
        NamespaceNames::Sensitive => object.node.name.clone(),
        NamespaceNames::Insensitive => object.node.name.to_lowercase(),
    };
    Some((identity(&object.scope, parent), name))
}
fn valid_text(value: &str) -> bool {
    !value.is_empty() && value.len() <= 4096 && !value.contains('\0')
}
fn valid_node(node: &Node) -> bool {
    valid_text(&node.id)
        && valid_text(&node.name)
        && !matches!(node.name.as_str(), "." | "..")
        && !node.name.contains('/')
        && node.parent_id.as_deref().is_some_and(valid_text)
        && match (&node.kind, &node.target) {
            (NodeKind::Folder, None) => true,
            (NodeKind::Shortcut, Some(target)) => {
                valid_text(&target.collection) && valid_text(&target.item)
            }
            _ => false,
        }
}

pub(super) fn check_route_slot(db: &Connection, object: &NamespaceObject) -> Result<()> {
    let mut query = db.prepare("SELECT body FROM namespace_objects")?;
    let objects = query
        .query_map([], |r| r.get::<_, String>(0))?
        .map(|r| Ok(serde_json::from_str::<NamespaceObject>(&r?)?))
        .collect::<Result<Vec<_>>>()?;
    RetainedAncestors::new(&objects).check_slot(object)
}

pub(super) fn check_new_slot(db: &Connection, object: &NamespaceObject) -> Result<()> {
    if object.unlinked || object.follows_remote {
        return Ok(());
    }
    match namespace::by_id(db, object.id) {
        Ok(old)
            if !old.follows_remote
                && !old.unlinked
                && old.node.parent_id == object.node.parent_id
                && old.node.name == object.node.name =>
        {
            return Ok(());
        }
        Ok(_) | Err(JournalError::Missing) => {}
        Err(e) => return Err(e),
    }
    check_route_slot(db, object)
}
impl UploadJournal {
    /// Capture an already traversed path before accepting a local mutation.
    /// Entries are ordered root to leaf, with source-side entries for links.
    /// Root itself is implicit. This records no new provider operation.
    pub fn capture_namespace_ancestors(&mut self, entries: Vec<(Scope, Node)>) -> Result<()> {
        if entries.len() > 128 {
            return Err(JournalError::Quota);
        }
        let objects = self.namespace_objects()?;
        let retained = RetainedAncestors::new(&objects);
        let mut count = objects.len();
        let tx = self.db.transaction()?;
        for (scope, node) in entries {
            if scope.account != self.account {
                return Err(JournalError::Account);
            }
            if !valid_text(&scope.provider) || !valid_text(&scope.collection) || !valid_node(&node)
            {
                return Err(JournalError::Intent);
            }
            namespace::ensure_legacy_policy(&tx, &scope)?;
            let old = namespace::by_local(&tx, &scope, &node.id)?;
            if let Some(old) = &old {
                if old.unlinked || old.node.kind != node.kind {
                    return Err(JournalError::Stale);
                }
                if !old.follows_remote || retained.contains(old.id) {
                    continue;
                }
            }
            let mut remote = node.clone();
            if let Some(old) = &old {
                remote.id = old.remote.as_ref().ok_or(JournalError::Corrupt)?.id.clone();
            }
            if let Some(parent) = &remote.parent_id
                && let Some(folder) = namespace::by_local(&tx, &scope, parent)?
            {
                remote.parent_id = Some(folder.remote.ok_or(JournalError::Stale)?.id);
            }
            let mut local = node;
            directories::localize_parent(&tx, &scope, &mut local)?;
            let object = if let Some(mut old) = old {
                if old.node == local && old.remote.as_ref() == Some(&remote) {
                    retained.check_slot(&old)?;
                    continue;
                }
                old.node = local;
                old.remote = Some(remote);
                old.revision = old.revision.checked_add(1).ok_or(JournalError::Quota)?;
                old
            } else {
                if count >= 10_000 {
                    return Err(JournalError::Quota);
                }
                count += 1;
                NamespaceObject {
                    id: Uuid::new_v4(),
                    names: namespace::policy(&tx, &scope)?,
                    scope,
                    node: local,
                    remote: Some(remote),
                    remote_owned: true,
                    remote_sequence: 0,
                    working_file: None,
                    latest: None,
                    revision: 0,
                    follows_remote: true,
                    unlinked: false,
                }
            };
            retained.check_slot(&object)?;
            namespace::save(&tx, &object)?;
        }
        tx.commit()?;
        #[cfg(feature = "test-support")]
        crate::journal::durable::record("ancestry::capture_namespace_ancestors");
        Ok(())
    }
}
