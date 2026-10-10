use super::*;
use crate::native_trash::NativeTrashAdmission;
impl Writeback {
    pub(in crate::filesystem) async fn native_trash_record(
        &self,
        id: Uuid,
    ) -> Result<crate::journal::MutationRecord> {
        self.local(move |j| j.native_trash_record(id)).await
    }
    pub(in crate::filesystem) fn enqueue_native_trash(
        &self,
        selection: NativeTrashAdmission,
        cancel: &CancellationToken,
    ) -> Result<Uuid> {
        let mut journal = self.journal.lock().map_err(|_| Errno::EIO)?;
        let action = journal.enqueue_native_trash_selection(selection, cancel);
        action
            .inspect_err(|failure| self.refusals.note(failure))
            .map_err(error)
    }
}

impl Writeback {
    pub(in crate::filesystem) async fn native_trash_list(
        &self,
        scope: Scope,
        after: Option<u64>,
        limit: u32,
    ) -> Result<crate::native_trash::NativeTrashListing> {
        self.local(move |j| j.native_trash_list(&scope, after, limit))
            .await
    }
    pub(in crate::filesystem) async fn native_replacement_list(
        &self,
        scope: Scope,
        after: Option<u64>,
        limit: u32,
    ) -> Result<crate::journal::NativeReplacementListing> {
        self.local(move |j| j.native_replacement_list(&scope, after, limit))
            .await
    }
}
