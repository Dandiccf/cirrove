use super::*;
impl WriteControl {
    pub(crate) fn enqueue_native_replace(
        &self,
        selection: crate::native_trash::NativeTrashAdmission,
        semantic: cirrove_core::upload::PackageSemanticIdentity,
        archive: crate::native_import::ValidatedPackageArchive,
        cancel: &CancellationToken,
    ) -> std::io::Result<crate::journal::UploadRecord> {
        let _admitted = self
            .inner
            .edits
            .admit()
            .map_err(|_| std::io::Error::other("native replacement mount changed"))?;
        if self.inner.cancel.is_cancelled() || cancel.is_cancelled() {
            return Err(std::io::Error::other(
                "native replacement cancelled before enqueue",
            ));
        }
        let row = self
            .writer
            .enqueue_native_replace(selection, semantic, archive, cancel)
            .map_err(|e| std::io::Error::from_raw_os_error(e.code()))?;
        self.writer.wake.notify_one();
        self.inner.engine.changed.notify_waiters();
        Ok(row)
    }
}
