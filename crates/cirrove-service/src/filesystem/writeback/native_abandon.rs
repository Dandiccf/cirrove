use super::*;
use cirrove_icloud::{NativeReplacementAbandonEvidence, NativeReplacementAbandonRecord};
impl Writeback {
    pub(in crate::filesystem) async fn prepare_native_abandon(
        &self,
        id: Uuid,
    ) -> Result<crate::journal::NativeAbandonPreparation> {
        self.local(move |j| j.prepare_native_stage_abandonment(id))
            .await
    }
    pub(in crate::filesystem) async fn native_abandon_record(
        &self,
        id: Uuid,
    ) -> Result<Option<NativeReplacementAbandonRecord>> {
        self.local(move |j| j.native_stage_abandonment(id)).await
    }
    pub(in crate::filesystem) fn commit_native_abandon(
        &self,
        prepared: crate::journal::NativeAbandonPreparation,
        evidence: NativeReplacementAbandonEvidence,
        cancel: &CancellationToken,
    ) -> Result<NativeReplacementAbandonRecord> {
        self.journal
            .lock()
            .map_err(|_| Errno::EIO)?
            .abandon_native_stage(prepared, evidence, cancel)
            .map_err(error)
    }
}
