//! Acknowledged native packages need metadata publication, never another upload.
use super::*;
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PackagePublicationStatus {
    Pending,
    Present(Node),
    Absent,
}
pub(super) fn migrate(db: &mut Connection) -> Result<()> {
    let tx = db.transaction()?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS package_metadata_publication(
        operation TEXT PRIMARY KEY, done INTEGER NOT NULL DEFAULT 0 CHECK(done IN(0,1)),
        failures INTEGER NOT NULL DEFAULT 0, retry_after INTEGER NOT NULL DEFAULT 0, observed TEXT);
        CREATE INDEX IF NOT EXISTS package_metadata_due ON package_metadata_publication(done,retry_after,operation);
        CREATE INDEX IF NOT EXISTS uploaded_package_receipts_v21 ON uploads(sequence)
        WHERE state='uploaded' AND json_extract(body,'$.representation.kind') IN('package_archive','package_replacement_archive','flat_numbers_archive','flat_numbers_replacement_archive')
          AND json_type(body,'$.package_completion')='object';
        CREATE TRIGGER IF NOT EXISTS package_metadata_on_update_v21 AFTER UPDATE OF state,body ON uploads
        WHEN NEW.state='uploaded' AND json_extract(NEW.body,'$.representation.kind') IN('package_archive','package_replacement_archive','flat_numbers_archive','flat_numbers_replacement_archive')
          AND json_type(NEW.body,'$.package_completion')='object'
        BEGIN INSERT OR IGNORE INTO package_metadata_publication(operation) VALUES(NEW.id); END;
        CREATE TRIGGER IF NOT EXISTS package_metadata_on_insert_v21 AFTER INSERT ON uploads
        WHEN NEW.state='uploaded' AND json_extract(NEW.body,'$.representation.kind') IN('package_archive','package_replacement_archive','flat_numbers_archive','flat_numbers_replacement_archive')
          AND json_type(NEW.body,'$.package_completion')='object'
        BEGIN INSERT OR IGNORE INTO package_metadata_publication(operation) VALUES(NEW.id); END;
        INSERT OR IGNORE INTO package_metadata_publication(operation)
        SELECT id FROM uploads WHERE state='uploaded'
          AND json_extract(body,'$.representation.kind') IN('package_archive','package_replacement_archive','flat_numbers_archive','flat_numbers_replacement_archive')
          AND json_type(body,'$.package_completion')='object';")?;
    tx.commit()?;
    Ok(())
}
fn completed(record: &UploadRecord) -> bool {
    // Matching persisted proofs are not admission authority: recheck the source
    // layout, format and selected identity before any local publication work.
    let request = cirrove_core::upload::UploadRequest {
        representation: record.representation.clone(),
        scope: record.scope.clone(),
        intent: record.intent.clone(),
        size: record.size,
        sha256: record.sha256.clone(),
    };
    if request.validate().is_err() {
        return false;
    }
    let semantic = match &record.representation {
        UploadRepresentation::PackageArchive { semantic, .. }
        | UploadRepresentation::FlatNumbersArchive { semantic } => semantic,
        UploadRepresentation::PackageReplacementArchive { semantic, .. }
        | UploadRepresentation::FlatNumbersReplacementArchive { semantic, .. }
            if record
                .identity_handoff
                .as_ref()
                .is_some_and(|r| r.backup.is_some()) =>
        {
            semantic
        }
        _ => return false,
    };
    record.state == UploadState::Uploaded
        && record.package_completion.as_ref() == Some(semantic)
        && record.remote.as_ref().is_some_and(|node| {
            node.package && node.kind == cirrove_core::NodeKind::Folder && node.target.is_none()
        })
}
impl UploadJournal {
    pub(crate) fn package_publication_status(&self, id: Uuid) -> Result<PackagePublicationStatus> {
        let record = self.get(id)?;
        if record.representation.is_file_bytes() {
            return Err(JournalError::Intent);
        }
        if record.state != UploadState::Uploaded {
            return Ok(PackagePublicationStatus::Pending);
        }
        if !completed(&record) {
            return Err(JournalError::Corrupt);
        }
        let row: Option<(bool, Option<String>)> = self
            .db
            .query_row(
                "SELECT done,observed FROM package_metadata_publication WHERE operation=?1",
                [id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match row {
            Some((true, Some(value))) => {
                let node: Node = serde_json::from_str(&value)?;
                if record
                    .remote
                    .as_ref()
                    .is_none_or(|remote| remote.id != node.id)
                {
                    return Err(JournalError::Corrupt);
                }
                Ok(PackagePublicationStatus::Present(node))
            }
            Some((true, None)) => Ok(PackagePublicationStatus::Absent),
            _ => Ok(PackagePublicationStatus::Pending),
        }
    }
    /// One due entry, selected by its own queue index; completed file history is
    /// never polled. The upload row itself stays byte-for-byte unchanged.
    pub(crate) fn package_publication_due(&self, now: u64) -> Result<Option<UploadRecord>> {
        let body: Option<String> = self
            .db
            .query_row(
                "SELECT u.body FROM package_metadata_publication p
            JOIN uploads u ON u.id=p.operation WHERE p.done=0 AND p.retry_after<=?1
            ORDER BY p.retry_after,p.operation LIMIT 1",
                [i64::try_from(now).map_err(|_| JournalError::Stale)?],
                |r| r.get(0),
            )
            .optional()?;
        body.map(|body| {
            let record: UploadRecord = serde_json::from_str(&body)?;
            if !completed(&record) {
                return Err(JournalError::Corrupt);
            }
            Ok(record)
        })
        .transpose()
    }
    /// A stale callback cannot acknowledge a different operation's receipt.
    pub(crate) fn finish_package_publication(
        &self,
        expected: &UploadRecord,
        status: PackagePublicationStatus,
        now: u64,
    ) -> Result<()> {
        let current = self.get(expected.id)?;
        if !completed(&current)
            || current.scope != expected.scope
            || current.intent != expected.intent
            || current.remote != expected.remote
            || current.package_completion != expected.package_completion
            || current.representation != expected.representation
            || current.size != expected.size
            || current.sha256 != expected.sha256
        {
            return Err(JournalError::Stale);
        }
        let now = i64::try_from(now).map_err(|_| JournalError::Stale)?;
        let changed=match status {
            PackagePublicationStatus::Present(node)=>{
                if current.remote.as_ref().is_none_or(|remote|remote.id!=node.id) {return Err(JournalError::Stale);}
                self.db.execute("UPDATE package_metadata_publication SET done=1,retry_after=0,observed=?2 WHERE operation=?1 AND done=0",params![expected.id.to_string(),serde_json::to_string(&node)?])?
            },
            PackagePublicationStatus::Absent=>self.db.execute("UPDATE package_metadata_publication SET done=1,retry_after=0,observed=NULL WHERE operation=?1 AND done=0",[expected.id.to_string()])?,
            PackagePublicationStatus::Pending=>self.db.execute("UPDATE package_metadata_publication SET retry_after=?2+min(60,(1 << min(failures+1,6))),failures=min(failures+1,6) WHERE operation=?1 AND done=0",params![expected.id.to_string(),now])?,
        };
        if changed != 1 {
            return Err(JournalError::Stale);
        }
        Ok(())
    }
}
