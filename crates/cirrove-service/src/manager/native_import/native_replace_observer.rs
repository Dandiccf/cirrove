//! Internal bound job/watch API; no socket verb or GUI submission.
use super::*;
impl Manager {
    pub async fn start_native_replacement(
        self: &Arc<Self>,
        engine: Arc<Engine>,
        input: crate::native_import::NativeReplaceInput,
    ) -> Result<crate::jobs::Job> {
        if input.selected.expected_account_id != engine.account.id {
            bail!("native replacement account changed");
        }
        let control = self.native_import_control(&engine).await?;
        engine.start_native_replacement_job(self.clone(), control, input)
    }
    pub async fn watch_native_replacement(
        self: &Arc<Self>,
        engine: Arc<Engine>,
        expected_account_id: &str,
        operation: uuid::Uuid,
    ) -> Result<crate::jobs::Job> {
        if engine.account.id != expected_account_id {
            bail!("native replacement account changed");
        }
        let control = self.native_import_control(&engine).await?;
        let (row, _) = self
            .native_replacement_observation(&engine, &control, operation)
            .await?;
        engine.start_native_replacement_watch(self.clone(), control, row)
    }
    pub(crate) async fn native_replacement_observation(
        &self,
        engine: &Arc<Engine>,
        control: &WriteControl,
        operation: uuid::Uuid,
    ) -> Result<(
        crate::journal::UploadRecord,
        crate::journal::PackagePublicationStatus,
    )> {
        self.native_import_same_control(engine, control).await?;
        let row = control.native_import_record(operation).await?;
        if row.scope != engine.scope(&engine.account.drive.id)
            || !matches!(
                row.representation,
                UploadRepresentation::PackageReplacementArchive { .. }
            )
            || !matches!(row.intent, UploadIntent::Replace { .. })
            || row.representation.validate().is_err()
        {
            bail!("operation is not this account's native replacement");
        }
        let publication = control.native_import_publication(operation).await?;
        self.native_import_same_control(engine, control).await?;
        Ok((row, publication))
    }
}
