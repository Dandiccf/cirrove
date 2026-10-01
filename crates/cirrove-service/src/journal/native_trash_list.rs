//! Bounded local discovery of retained native Trash operations.
use super::*;
use crate::native_trash::{NativeTrashListing, NativeTrashSelection};
use cirrove_core::mutation::{MutationIntent, MutationReceipt};
impl UploadJournal {
    pub(crate) fn native_trash_list(
        &self,
        scope: &Scope,
        after: Option<u64>,
        limit: u32,
    ) -> Result<NativeTrashListing> {
        if scope.account != self.account || !(1..=100).contains(&limit) {
            return Err(JournalError::Intent);
        }
        let after = i64::try_from(after.unwrap_or(0)).map_err(|_| JournalError::Intent)?;
        let indexed: bool = self.db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='index' AND name='native_trash_operations')", [], |r| r.get(0))?;
        // A pre-index journal opened read-only must not be migrated. Its fallback
        // advances over at most limit rows, including ordinary operations.
        let sql = if indexed {
            "SELECT body FROM mutations INDEXED BY native_trash_operations WHERE sequence>?1 AND json_extract(body,'$.request.intent.kind')='trash_native_document' ORDER BY sequence LIMIT ?2"
        } else {
            "SELECT body FROM mutations WHERE sequence>?1 ORDER BY sequence LIMIT ?2"
        };
        let mut query = self.db.prepare(sql)?;
        let mut rows = query
            .query_map(params![after, limit + 1], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str::<MutationRecord>(&r?)?))
            .collect::<Result<Vec<_>>>()?;
        let more = rows.len() > limit as usize;
        rows.truncate(limit as usize);
        let mut next = if more {
            rows.last().map(|r| r.sequence)
        } else {
            None
        };
        let publication:bool=self.db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='native_trash_metadata_publication')",[],|r|r.get(0))?;
        let mut operations = Vec::with_capacity(rows.len());
        // Leave envelope/cursor overhead well below the public 1MiB exchange
        // ceiling. JSON escaping makes a row count alone insufficient.
        const PAGE_BYTES: usize = 512 * 1024;
        let mut bytes = 0usize;
        let mut scanned = after as u64;
        for row in rows {
            if row.request.scope != *scope {
                scanned = row.sequence;
                continue;
            }
            let MutationIntent::TrashNativeDocument { ref before } = row.request.intent else {
                scanned = row.sequence;
                continue;
            };
            if row.request.validate().is_err() || row.base.is_some() || row.working_file.is_some() {
                return Err(JournalError::Corrupt);
            }
            let removal_receipt_recorded = row.state == MutationState::Applied
                && matches!(&row.receipt,Some(MutationReceipt::Removed{item}) if item==&before.id);
            if row.state == MutationState::Applied && !removal_receipt_recorded {
                return Err(JournalError::Corrupt);
            }
            let metadata_absence_recorded = removal_receipt_recorded
                && publication
                && matches!(
                    self.native_trash_publication_status(row.id)?,
                    PackagePublicationStatus::Absent
                );
            let selection = NativeTrashSelection {
                operation: row.id,
                sequence: row.sequence,
                item_id: before.id.clone(),
                name: before.name.clone(),
                etag: before.etag.clone().ok_or(JournalError::Corrupt)?,
                state: row.state,
                removal_receipt_recorded,
                metadata_absence_recorded,
            };
            let size = serde_json::to_vec(&selection)?.len() + 1;
            if size > PAGE_BYTES {
                return Err(JournalError::Corrupt);
            }
            if bytes + size > PAGE_BYTES {
                // The overflowing row is NOT consumed. Continue after the
                // previous scanned row, including skipped legacy ordinary rows.
                next = Some(scanned);
                break;
            }
            bytes += size;
            scanned = row.sequence;
            operations.push(selection);
        }
        Ok(NativeTrashListing { operations, next })
    }
}
#[cfg(test)]
mod tests;
