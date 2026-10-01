//! Selected native-container admission; bounded exact resource queries only.
use super::*;
use crate::native_trash::NativeTrashAdmission;
use cirrove_core::{
    CancellationToken,
    mutation::{MutationIntent, MutationRequest},
};
impl UploadJournal {
    pub(crate) fn enqueue_native_trash_selection(
        &mut self,
        selection: NativeTrashAdmission,
        cancel: &CancellationToken,
    ) -> Result<Uuid> {
        self.validate_native_selection(&selection, cancel)?;
        if let Some(owner) =
            self.namespace_by_remote(&selection.parent.scope, &selection.target.id)?
        {
            // A completed handoff leaves only a stable local alias following
            // provider metadata. Do not detach or rewrite it: existing views
            // retain that identity, and verified provider absence removes its
            // listing naturally. Locally authoritative owners remain refused.
            let mut expected = selection.target.clone();
            expected.id = owner.node.id.clone();
            directories::localize_parent(&self.db, &selection.parent.scope, &mut expected)?;
            if !owner.follows_remote
                || owner.latest.is_some()
                || owner.working_file.is_some()
                || owner.unlinked
                || !owner.remote_owned
                || owner.scope != selection.parent.scope
                || owner.remote.as_ref() != Some(&selection.target)
                || owner.node != expected
            {
                return Err(JournalError::Intent);
            }
        }
        let request = MutationRequest {
            scope: selection.parent.scope,
            intent: MutationIntent::TrashNativeDocument {
                before: selection.target,
            },
        };
        // No await or lock release between the final local checks and durable
        // enqueue. The provider independently fences remote revision again.
        self.enqueue_mutation(request).map(|r| r.id)
    }
}

impl UploadJournal {
    pub(crate) fn validate_native_selection(
        &self,
        selection: &NativeTrashAdmission,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let NativeTrashAdmission { parent, target } = selection;
        if parent.scope.account != self.account
            || parent.route.len() > 33
            || parent.route.last() != Some(&parent.parent)
            || target.parent_id.as_deref() != Some(&parent.parent.id)
            || self.namespace_publication(parent.frontier)?.through != parent.frontier
        {
            return Err(JournalError::Stale);
        }
        let overlay =
            self.namespace_overlay(&parent.scope, &parent.parent.id, parent.children.clone())?;
        if !overlay.conflicts.is_empty()
            || overlay
                .nodes
                .iter()
                .filter(|n| n.name == target.name)
                .count()
                != 1
        {
            return Err(JournalError::Stale);
        }
        let visible = overlay
            .nodes
            .iter()
            .find(|n| n.name == target.name)
            .ok_or(JournalError::Stale)?;
        if visible != target {
            // The mounted name can retain a local identity after a completed
            // handoff. Only the clean, exact owner of this provider revision is
            // eligible; never normalize away a queued rename or working edit.
            let owner = self
                .namespace_by_local(&parent.scope, &visible.id)?
                .ok_or(JournalError::Stale)?;
            let mut expected = target.clone();
            expected.id = owner.node.id.clone();
            directories::localize_parent(&self.db, &parent.scope, &mut expected)?;
            if owner.node != *visible
                || owner.node != expected
                || owner.remote.as_ref() != Some(target)
                || !owner.remote_owned
                || owner.unlinked
                || owner.working_file.is_some()
                // Already-following objects have completed the clean handoff.
                // namespace_is_clean deliberately excludes them because it is
                // the predicate for selecting the next handoff candidate.
                || (owner.follows_remote && owner.latest.is_some())
                || (!owner.follows_remote && !self.namespace_is_clean(&owner)?)
            {
                return Err(JournalError::Stale);
            }
        }
        let request = MutationRequest {
            scope: parent.scope.clone(),
            intent: MutationIntent::TrashNativeDocument {
                before: target.clone(),
            },
        };
        request.validate().map_err(|_| JournalError::Intent)?;
        let mut resources = mutations::mutation_resources(&request)?;
        for ancestor in &parent.route {
            if let Some(local) = self.namespace_by_remote(&parent.scope, &ancestor.id)?
                && (!local.follows_remote
                    || local.unlinked
                    || local.working_file.is_some()
                    || !local.remote_owned
                    || local.remote.as_ref() != Some(ancestor))
            {
                return Err(JournalError::Stale);
            }
            resources.push(mutations::item_key(&parent.scope, &ancestor.id)?);
        }
        resources.sort();
        resources.dedup();
        for resource in resources {
            let busy: bool = self.db.query_row(
                "SELECT EXISTS(SELECT 1 FROM write_resources r INDEXED BY write_resource
                 JOIN write_queue q ON q.id=r.id
                 LEFT JOIN package_metadata_publication p ON p.operation=q.id
                 LEFT JOIN native_trash_metadata_publication n ON n.operation=q.id
                 WHERE r.resource=?1 AND (q.complete=0 OR p.done=0 OR n.done=0))",
                [resource],
                |r| r.get(0),
            )?;
            if busy {
                return Err(JournalError::Intent);
            }
        }
        if cancel.is_cancelled() {
            return Err(JournalError::Stale);
        }
        Ok(())
    }
}
