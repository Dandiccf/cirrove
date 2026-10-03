//! Reconstruct exact native Trash recovery independently of the metadata index.
use super::*;
use cirrove_core::mutation::{MutationIntent, Result as MutationResult};
impl ICloudWriteProvider {
    pub(crate) fn native_trash_adapter(
        &self,
        request: &MutationRequest,
    ) -> MutationResult<cirrove_icloud::ICloudNativeTrash> {
        request.validate()?;
        let MutationIntent::TrashNativeDocument { before } = &request.intent else {
            return Err(MutationError::Invalid);
        };
        if request.scope != self.scope {
            return Err(MutationError::Invalid);
        }
        let staging = self
            .package_staging()
            .map_err(|_| MutationError::Uncertain)?;
        cirrove_icloud::ICloudNativeTrash::from_sealed_session(
            self.scope.clone(),
            self.apple_id.clone(),
            self.credential_id.clone(),
            &self.state,
            before.clone(),
            staging,
        )
        .map(|adapter| adapter.with_write_staging_budget(self.package_staging_budget.clone()))
    }
}
