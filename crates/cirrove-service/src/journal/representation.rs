//! Durable format boundary for explicitly qualified package archives.
use super::*;
use cirrove_core::CancellationToken;

pub(super) fn migrate(db: &mut Connection, version: u32) -> Result<()> {
    if version < JOURNAL_SCHEMA {
        let tx = db.transaction()?;
        // Older binaries ignore new JSON fields. The schema gate prevents them
        // from replaying a package archive as ordinary file bytes.
        tx.pragma_update(None, "user_version", JOURNAL_SCHEMA)?;
        tx.commit()?;
    }
    Ok(())
}

pub(super) struct Admission {
    pub representation: UploadRepresentation,
    pub receipt: Option<BoundReceipt>,
}
pub(super) struct BoundReceipt {
    pub size: u64,
    pub sha256: String,
    pub cancel: CancellationToken,
}

impl UploadJournal {
    /// Exact indexed queue slot; completed historical creates do not reserve
    /// their old names. A confirmed PACKAGE keeps its slot until metadata was
    /// published, including a published observation that it has disappeared.
    pub(crate) fn native_import_destination_reserved(
        &self,
        scope: &Scope,
        parent: &str,
        name: &str,
    ) -> Result<bool> {
        let intent = UploadIntent::Create {
            parent: parent.to_owned(),
            name: name.to_owned(),
        };
        let keys = mutations::upload_resources(scope, &intent)?;
        let key = keys.first().ok_or(JournalError::Intent)?;
        Ok(self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM write_resources r INDEXED BY write_resource
             JOIN write_queue q ON q.id=r.id
             LEFT JOIN package_metadata_publication p ON p.operation=q.id
             WHERE r.resource=?1 AND (q.complete=0 OR p.done=0))",
            [key],
            |row| row.get(0),
        )?)
    }

    /// Consume a privately captured and semantically validated archive. Raw bytes
    /// are verified again before snapshot/row publication, never afterwards.
    /// Caller must independently authorize account, destination and lifecycle.
    pub fn enqueue_validated_package_archive(
        &mut self,
        scope: Scope,
        intent: UploadIntent,
        archive: crate::native_import::ValidatedPackageArchive,
        cancel: &CancellationToken,
    ) -> Result<UploadRecord> {
        let (file, representation, size, sha256) = archive.into_parts();
        self.enqueue_admitted(
            scope,
            intent,
            WriteOrder::default(),
            None,
            Admission {
                representation,
                receipt: Some(BoundReceipt {
                    size,
                    sha256,
                    cancel: cancel.clone(),
                }),
            },
            file,
        )
    }

    /// Explicit validator entry point, not reachable from normal mounted writes.
    /// A future production caller must verify the exact sealed archive during
    /// admission before enqueue: begin/allocation receives no payload descriptor.
    /// The body adapter must independently verify it before sending bytes.
    /// This validator API's caller-supplied identity is not proof of admission.
    #[cfg(any(test, feature = "icloud-write-probe", feature = "test-support"))]
    pub fn enqueue_package_archive(
        &mut self,
        scope: Scope,
        intent: UploadIntent,
        expected_root: String,
        semantic: PackageSemanticIdentity,
        bytes: impl Read,
    ) -> Result<UploadRecord> {
        self.enqueue_represented(
            scope,
            intent,
            WriteOrder::default(),
            None,
            UploadRepresentation::PackageArchive {
                expected_root,
                semantic,
            },
            bytes,
        )
    }

    /// A provider returns this only after independent content verification of
    /// the exact allocated identity, never on registration response alone.
    pub fn acknowledge_package(
        &mut self,
        id: Uuid,
        attempt: Uuid,
        receipt: PackageUploadReceipt,
    ) -> Result<()> {
        let mut record = self.active_attempt(id, attempt)?;
        let UploadRepresentation::PackageArchive { semantic, .. } = &record.representation else {
            return Err(JournalError::Intent);
        };
        record
            .representation
            .validate()
            .map_err(|_| JournalError::Corrupt)?;
        let UploadIntent::Create { parent, name } = &record.intent else {
            return Err(JournalError::Intent);
        };
        let remote = &receipt.remote;
        if record.identity_handoff.is_some()
            || record.base.is_some()
            || record.working_file.is_some()
            || &receipt.semantic != semantic
            || remote.id.is_empty()
            || remote.id.len() > 4096
            || remote.id.contains('\0')
            || remote.parent_id.as_deref() != Some(parent)
            || &remote.name != name
            || remote.kind != NodeKind::Folder
            || !remote.package
            || remote.target.is_some()
            || remote.content_revision().is_none()
        {
            return Err(JournalError::Corrupt);
        }
        record.remote = Some(receipt.remote);
        record.package_completion = Some(receipt.semantic);
        record.state = UploadState::Uploaded;
        record.transferred_bytes = record.size;
        record.attempt = None;
        self.save(&record)
    }
}

#[cfg(test)]
mod tests;
