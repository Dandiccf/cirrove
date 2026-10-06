//! Exact-ID metadata convergence after a confirmed standalone native removal.
//! Receipt acknowledgement does not itself hide cached metadata.
use super::*;
use cirrove_core::mutation::{MutationIntent, MutationReceipt};

#[cfg(test)]
mod authority_tests;
#[cfg(test)]
mod readonly_tests;

pub(super) fn migrate(db: &mut Connection) -> Result<()> {
    let tx = db.transaction()?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS native_trash_metadata_publication(
        operation TEXT PRIMARY KEY, done INTEGER NOT NULL DEFAULT 0 CHECK(done IN(0,1)),
        failures INTEGER NOT NULL DEFAULT 0, retry_after INTEGER NOT NULL DEFAULT 0, observed TEXT);
        CREATE INDEX IF NOT EXISTS native_trash_metadata_due ON native_trash_metadata_publication(done,retry_after,operation);
        CREATE INDEX IF NOT EXISTS native_trash_operations ON mutations(sequence)
        WHERE json_extract(body,'$.request.intent.kind')='trash_native_document';
        CREATE INDEX IF NOT EXISTS applied_native_trash_receipts ON mutations(sequence)
        WHERE state='applied' AND json_extract(body,'$.request.intent.kind')='trash_native_document';
        CREATE TRIGGER IF NOT EXISTS native_trash_metadata_on_update AFTER UPDATE OF state,body ON mutations
        WHEN NEW.state='applied' AND json_extract(NEW.body,'$.request.intent.kind')='trash_native_document'
        BEGIN INSERT OR IGNORE INTO native_trash_metadata_publication(operation) VALUES(NEW.id); END;
        CREATE TRIGGER IF NOT EXISTS native_trash_metadata_on_insert AFTER INSERT ON mutations
        WHEN NEW.state='applied' AND json_extract(NEW.body,'$.request.intent.kind')='trash_native_document'
        BEGIN INSERT OR IGNORE INTO native_trash_metadata_publication(operation) VALUES(NEW.id); END;
        INSERT OR IGNORE INTO native_trash_metadata_publication(operation)
        SELECT id FROM mutations WHERE state='applied'
          AND json_extract(body,'$.request.intent.kind')='trash_native_document';")?;
    tx.commit()?;
    Ok(())
}
fn completed(record: &MutationRecord) -> bool {
    record.state == MutationState::Applied
        && record.attempt.is_none()
        && record.prepared_item.as_ref().is_none_or(|item| {
            record
                .request
                .intent
                .before()
                .is_some_and(|before| item == &before.id)
        })
        && matches!(
            record.request.intent,
            MutationIntent::TrashNativeDocument { .. }
        )
        && record.request.validate().is_ok()
        && record.base.is_none()
        && record.working_file.is_none()
        && matches!(record.receipt.as_ref(), Some(receipt @ MutationReceipt::Removed { .. }) if record.request.accepts(receipt))
}
impl UploadJournal {
    pub(crate) fn native_trash_record(&self, id: Uuid) -> Result<MutationRecord> {
        let (sequence, state, body, queue_sequence, queue_complete): (
            i64,
            String,
            String,
            Option<i64>,
            Option<bool>,
        ) = self
            .db
            .query_row(
                "SELECT m.sequence,m.state,m.body,q.sequence,q.complete FROM mutations m
             LEFT JOIN write_queue q ON q.id=m.id WHERE m.id=?1",
                [id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?
            .ok_or(JournalError::Missing)?;
        let record: MutationRecord =
            serde_json::from_str(&body).map_err(|_| JournalError::Corrupt)?;
        if sequence <= 0
            || record.id != id
            || record.sequence != sequence as u64
            || queue_sequence != Some(sequence)
            || queue_complete != Some(record.state == MutationState::Applied)
            || serde_json::to_value(record.state)?.as_str() != Some(state.as_str())
            || record.request.scope.account != self.account
            || record.request.validate().is_err()
            || record.base.is_some()
            || record.working_file.is_some()
        {
            return Err(JournalError::Corrupt);
        }
        if !matches!(
            record.request.intent,
            MutationIntent::TrashNativeDocument { .. }
        ) {
            return Err(JournalError::Intent);
        }
        Ok(record)
    }
    pub(crate) fn native_trash_publication_status(
        &self,
        id: Uuid,
    ) -> Result<PackagePublicationStatus> {
        let record = self.native_trash_record(id)?;
        if !matches!(
            record.request.intent,
            MutationIntent::TrashNativeDocument { .. }
        ) {
            return Err(JournalError::Intent);
        }
        if record.state != MutationState::Applied {
            return Ok(PackagePublicationStatus::Pending);
        }
        if !completed(&record) {
            return Err(JournalError::Corrupt);
        }
        let exists: bool = self.db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='native_trash_metadata_publication')",[],|r|r.get(0))?;
        if !exists {
            return Ok(PackagePublicationStatus::Pending);
        }
        let row: Option<(bool, Option<String>)> = self
            .db
            .query_row(
                "SELECT done,observed FROM native_trash_metadata_publication WHERE operation=?1",
                [id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match row {
            Some((true, None)) => Ok(PackagePublicationStatus::Absent),
            Some((false, Some(value))) => {
                let node: Node = serde_json::from_str(&value)?;
                if record
                    .request
                    .intent
                    .before()
                    .is_none_or(|before| before.id != node.id)
                {
                    return Err(JournalError::Corrupt);
                }
                Ok(PackagePublicationStatus::Present(node))
            }
            Some((true, Some(_))) => Err(JournalError::Corrupt),
            _ => Ok(PackagePublicationStatus::Pending),
        }
    }
    pub(crate) fn native_trash_publication_due(&self, now: u64) -> Result<Option<MutationRecord>> {
        let now = i64::try_from(now).map_err(|_| JournalError::Stale)?;
        let tx = self.db.unchecked_transaction()?;
        let selected: Option<String> = tx
            .query_row(
                "SELECT p.operation FROM native_trash_metadata_publication p
             WHERE p.done=0 AND p.retry_after<=?1 ORDER BY p.retry_after,p.operation LIMIT 1",
                [now],
                |r| r.get(0),
            )
            .optional()?;
        let Some(selected) = selected else {
            tx.commit()?;
            return Ok(None);
        };
        let result = Uuid::parse_str(&selected)
            .map_err(|_| JournalError::Corrupt)
            .and_then(|id| {
                self.native_trash_record(id)
                    .map_err(|_| JournalError::Corrupt)
            })
            .and_then(|record| {
                if completed(&record) {
                    Ok(record)
                } else {
                    Err(JournalError::Corrupt)
                }
            });
        if result.is_err() {
            // Defer only this invalid local job. Its raw owner, transfer rows
            // and receipt remain retained; a bad head cannot starve its sibling.
            tx.execute("UPDATE native_trash_metadata_publication SET retry_after=?2+min(60,(1 << min(failures+1,6))),failures=min(failures+1,6) WHERE operation=?1 AND done=0",params![selected,now])?;
        }
        tx.commit()?;
        result.map(Some)
    }
    pub(crate) fn finish_native_trash_publication(
        &self,
        expected: &MutationRecord,
        status: PackagePublicationStatus,
        now: u64,
    ) -> Result<()> {
        let tx = self.db.unchecked_transaction()?;
        let current = self.native_trash_record(expected.id)?;
        if !completed(&current)
            || serde_json::to_value(&current)? != serde_json::to_value(expected)?
        {
            return Err(JournalError::Stale);
        }
        let now = i64::try_from(now).map_err(|_| JournalError::Stale)?;
        let changed = match status {
            PackagePublicationStatus::Absent => tx.execute(
                "UPDATE native_trash_metadata_publication SET done=1,retry_after=0,observed=NULL WHERE operation=?1 AND done=0", [expected.id.to_string()])?,
            status => {
                let observed = match status {
                    PackagePublicationStatus::Present(node) => {
                        if current.request.intent.before().is_none_or(|before| before.id != node.id) { return Err(JournalError::Stale); }
                        Some(serde_json::to_string(&node)?)
                    }
                    _ => None,
                };
                // An active exact ID can be propagation delay or a later restore.
                // Keep it visible and retry with bounded cooldown, never hide it.
                tx.execute("UPDATE native_trash_metadata_publication SET retry_after=?2+min(60,(1 << min(failures+1,6))),failures=min(failures+1,6),observed=?3 WHERE operation=?1 AND done=0",
                    params![expected.id.to_string(), now, observed])?
            }
        };
        if changed != 1 {
            return Err(JournalError::Stale);
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use cirrove_core::mutation::MutationRequest;
    #[test]
    fn native_trash_publication_backfills_applied_history_once_without_changing_receipts() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut journal = UploadJournal::open(temp.path(), "owned", 1024 * 1024).unwrap();
        let before = Node {
            id: "native".into(),
            parent_id: Some("root".into()),
            name: "Original.pages".into(),
            kind: NodeKind::Folder,
            package: true,
            size: 0,
            modified_unix: 0,
            etag: Some("E1".into()),
            content_version: None,
            target: None,
        };
        let scope = Scope {
            account: "owned".into(),
            provider: "icloud".into(),
            collection: "drive".into(),
        };
        let native = journal
            .enqueue_mutation(MutationRequest {
                scope: scope.clone(),
                intent: MutationIntent::TrashNativeDocument {
                    before: before.clone(),
                },
            })
            .unwrap();
        let attempt = journal.claim_mutation().unwrap().unwrap().attempt.unwrap();
        journal
            .acknowledge_mutation(
                native.id,
                attempt,
                MutationReceipt::Removed {
                    item: before.id.clone(),
                },
            )
            .unwrap();
        let ordinary = journal
            .enqueue_mutation(MutationRequest {
                scope,
                intent: MutationIntent::RemoveFile {
                    before: Node {
                        id: "ordinary".into(),
                        name: "ordinary.txt".into(),
                        kind: NodeKind::File,
                        package: false,
                        ..before
                    },
                },
            })
            .unwrap();
        let attempt = journal.claim_mutation().unwrap().unwrap().attempt.unwrap();
        journal
            .acknowledge_mutation(
                ordinary.id,
                attempt,
                MutationReceipt::Removed {
                    item: "ordinary".into(),
                },
            )
            .unwrap();
        let row = journal.mutation(native.id).unwrap();
        let bytes = serde_json::to_vec(&row).unwrap();
        // Model a committed receipt from before this additive publication table.
        journal
            .db
            .execute("DELETE FROM native_trash_metadata_publication", [])
            .unwrap();
        drop(journal);
        let journal = UploadJournal::open(temp.path(), "owned", 1024 * 1024).unwrap();
        assert_eq!(
            journal.native_trash_publication_due(0).unwrap().unwrap().id,
            native.id
        );
        assert_eq!(
            serde_json::to_vec(&journal.mutation(native.id).unwrap()).unwrap(),
            bytes
        );
        assert!(
            journal
                .native_trash_publication_status(ordinary.id)
                .is_err()
        );
        journal
            .finish_native_trash_publication(&row, PackagePublicationStatus::Absent, 0)
            .unwrap();
        drop(journal);
        let journal = UploadJournal::open(temp.path(), "owned", 1024 * 1024).unwrap();
        assert!(journal.native_trash_publication_due(60).unwrap().is_none());
        assert_eq!(
            journal.native_trash_publication_status(native.id).unwrap(),
            PackagePublicationStatus::Absent
        );
        assert_eq!(
            serde_json::to_vec(&journal.mutation(native.id).unwrap()).unwrap(),
            bytes
        );
    }
}
