use super::*;
use cirrove_icloud::{NativeReplacementAbandonEvidence, NativeReplacementAbandonRecord};
fn unavailable() -> std::io::Error {
    std::io::Error::other("native Stage abandonment unavailable")
}
impl WriteControl {
    pub(crate) async fn prepare_native_abandon(
        &self,
        id: uuid::Uuid,
    ) -> std::io::Result<crate::journal::NativeAbandonPreparation> {
        self.native_import_open()?;
        let result = self
            .writer
            .prepare_native_abandon(id)
            .await
            .map_err(|_| unavailable())?;
        self.native_import_open()?;
        Ok(result)
    }
    pub(crate) async fn native_abandon_record(
        &self,
        id: uuid::Uuid,
    ) -> std::io::Result<Option<NativeReplacementAbandonRecord>> {
        self.native_import_open()?;
        let result = self
            .writer
            .native_abandon_record(id)
            .await
            .map_err(|_| unavailable())?;
        self.native_import_open()?;
        Ok(result)
    }
    pub(crate) async fn refresh_native_abandon(&self, id: uuid::Uuid) -> std::io::Result<()> {
        self.writer
            .refresh_operation(id)
            .await
            .map_err(|_| unavailable())
    }
    pub(crate) fn commit_native_abandon(
        &self,
        prepared: crate::journal::NativeAbandonPreparation,
        evidence: NativeReplacementAbandonEvidence,
        cancel: &CancellationToken,
    ) -> std::io::Result<NativeReplacementAbandonRecord> {
        let _admitted = self.inner.edits.admit().map_err(|_| unavailable())?;
        if self.inner.cancel.is_cancelled() || cancel.is_cancelled() {
            return Err(unavailable());
        }
        let record = self
            .writer
            .commit_native_abandon(prepared, evidence, cancel)
            .map_err(|_| unavailable())?;
        // No worker wake: this terminal transition must never restart cloud work.
        self.inner.engine.changed.notify_waiters();
        Ok(record)
    }
}
