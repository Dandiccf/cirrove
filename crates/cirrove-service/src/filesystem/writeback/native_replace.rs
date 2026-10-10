use super::*;
impl Writeback {
    pub(in crate::filesystem) fn enqueue_native_replace(
        &self,
        selection: crate::native_trash::NativeTrashAdmission,
        semantic: cirrove_core::upload::PackageSemanticIdentity,
        archive: crate::native_import::ValidatedPackageArchive,
        cancel: &CancellationToken,
    ) -> Result<crate::journal::UploadRecord> {
        let mut journal = self.journal.lock().map_err(|_| Errno::EIO)?;
        let action = (|| {
            journal.validate_native_selection(&selection, cancel)?;
            journal.enqueue_validated_package_replacement(
                selection.parent.scope,
                selection.target,
                semantic,
                archive,
                cancel,
            )
        })();
        action
            .inspect_err(|failure| self.refusals.note(failure))
            .map_err(error)
    }
}
