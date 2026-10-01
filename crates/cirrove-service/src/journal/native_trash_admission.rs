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
        let NativeTrashAdmission { parent, target } = selection;
        if parent.scope.account != self.account
            || parent.route.len() > 33
            || parent.route.last() != Some(&parent.parent)
            || target.parent_id.as_deref() != Some(&parent.parent.id)
            || self.namespace_publication(parent.frontier)?.through != parent.frontier
        {
            return Err(JournalError::Stale);
        }
        let overlay = self.namespace_overlay(&parent.scope, &parent.parent.id, parent.children)?;
        if !overlay.conflicts.is_empty()
            || overlay
                .nodes
                .iter()
                .filter(|n| n.name == target.name)
                .count()
                != 1
            || !overlay.nodes.iter().any(|n| n == &target)
        {
            return Err(JournalError::Stale);
        }
        let request = MutationRequest {
            scope: parent.scope.clone(),
            intent: MutationIntent::TrashNativeDocument {
                before: target.clone(),
            },
        };
        request.validate().map_err(|_| JournalError::Intent)?;
        // Native containers have no writable namespace object. Never discard an
        // adopted object's held/local state by treating it as a standalone row.
        if self
            .namespace_by_remote(&parent.scope, &target.id)?
            .is_some()
        {
            return Err(JournalError::Intent);
        }
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
        // No await or lock release between the final local checks and durable
        // enqueue. The provider independently fences remote revision again.
        self.enqueue_mutation(request).map(|r| r.id)
    }
}
