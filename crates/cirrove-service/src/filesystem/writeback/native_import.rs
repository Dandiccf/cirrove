use super::*;
use crate::native_import::{ImportParent, ValidatedPackageArchive};
use cirrove_core::upload::UploadIntent;

impl Writeback {
    pub(in crate::filesystem) async fn native_import_publication(
        &self,
        id: Uuid,
    ) -> Result<crate::journal::PackagePublicationStatus> {
        self.local(move |j| j.package_publication_status(id)).await
    }

    pub(in crate::filesystem) async fn native_import_frontier(&self) -> Result<u64> {
        self.local(|j| Ok(j.namespace_publication(0)?.through))
            .await
    }
    pub(in crate::filesystem) async fn native_import_record(
        &self,
        id: Uuid,
    ) -> Result<crate::journal::UploadRecord> {
        self.local(move |j| j.get(id)).await
    }
    pub(in crate::filesystem) fn enqueue_native_import(
        &self,
        parent: ImportParent,
        name: String,
        archive: ValidatedPackageArchive,
        cancel: &CancellationToken,
    ) -> Result<crate::journal::UploadRecord> {
        let mut journal = self.journal.lock().map_err(|_| Errno::EIO)?;
        let action = (|| {
            if journal.namespace_publication(parent.frontier)?.through != parent.frontier {
                return Err(JournalError::Stale);
            }
            let visible =
                journal.namespace_overlay(&parent.scope, &parent.parent.id, parent.children)?;
            if !visible.conflicts.is_empty() || visible.nodes.iter().any(|node| node.name == name) {
                return Err(JournalError::Intent);
            }
            // Package creates do not yet have a mounted placeholder. Their
            // durable queue rows must still reserve this destination against a
            // second explicit import before metadata publication catches up.
            if journal.native_import_destination_reserved(
                &parent.scope,
                &parent.parent.id,
                &name,
            )? {
                return Err(JournalError::Intent);
            }
            journal.enqueue_validated_package_archive(
                parent.scope,
                UploadIntent::Create {
                    parent: parent.parent.id,
                    name,
                },
                archive,
                cancel,
            )
        })();
        action
            .inspect_err(|failure| self.refusals.note(failure))
            .map_err(error)
    }
}
