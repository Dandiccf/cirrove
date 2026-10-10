//! Internal local-only discovery; no worker, provider or credential access.
use super::*;
// Reserve envelope/cursor overhead so the serialized page itself fits 512 KiB.
const PAGE_BYTES: usize = 512 * 1024 - 128;
const RECORD_BYTES: i64 = 256 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NativeReplacementIdentity {
    pub item: String,
    pub parent: String,
    pub name: String,
    pub etag: String,
    pub logical_size: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NativeReplacementSelection {
    pub operation: Uuid,
    pub sequence: u64,
    pub state: UploadState,
    pub original: NativeReplacementIdentity,
    pub current: Option<NativeReplacementIdentity>,
    pub recovery: Option<NativeReplacementIdentity>,
    /// Historical typed receipt evidence, never a fresh cloud observation.
    pub handoff_receipt_recorded: bool,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NativeReplacementListing {
    pub operations: Vec<NativeReplacementSelection>,
    /// Advance even for empty legacy pages. Overflow rows are not consumed.
    pub next: Option<u64>,
}
pub(super) fn migrate(db: &Connection) -> Result<()> {
    // Additive performance index; representation/schema compatibility stays 17.
    db.execute_batch(
        "CREATE INDEX IF NOT EXISTS native_package_replacement_operations_v21 ON uploads(sequence)
        WHERE json_extract(body,'$.representation.kind') IN('package_replacement_archive','flat_numbers_replacement_archive');
        CREATE INDEX IF NOT EXISTS native_package_replacement_operations_flat_pages_v21 ON uploads(sequence)
        WHERE json_extract(body,'$.representation.kind') IN('package_replacement_archive','flat_numbers_replacement_archive','flat_pages_replacement_archive');",
    )?;
    Ok(())
}
fn identity(node: &Node) -> Result<NativeReplacementIdentity> {
    let etag = node.etag.as_ref().ok_or(JournalError::Corrupt)?;
    let parent = node.parent_id.as_ref().ok_or(JournalError::Corrupt)?;
    if node.kind != NodeKind::Folder
        || !node.package
        || node.target.is_some()
        || node.content_version.is_some()
        || !node.id.starts_with("FILE::com.apple.CloudDocs::")
        || node.id.ends_with("::")
        || node.id.len() > 4096
        || node.id.chars().any(char::is_control)
        || !parent.starts_with("FOLDER::com.apple.CloudDocs::")
        || parent.ends_with("::")
        || parent.len() > 4096
        || parent.chars().any(char::is_control)
        || node.name.is_empty()
        || node.name.len() > 255
        || node.name.chars().any(|c| c.is_control() || c == '/')
        || etag.is_empty()
        || etag.len() > 4096
        || etag.chars().any(|c| c.is_control() || c == '*')
    {
        return Err(JournalError::Corrupt);
    }
    Ok(NativeReplacementIdentity {
        item: node.id.clone(),
        parent: parent.clone(),
        name: node.name.clone(),
        etag: etag.clone(),
        logical_size: node.size,
    })
}
fn selection(row: UploadRecord) -> Result<NativeReplacementSelection> {
    package_replacement::validate(&row.scope, &row.intent, &row.representation)
        .map_err(|_| JournalError::Corrupt)?;
    let (original, semantic) = match &row.representation {
        UploadRepresentation::PackageReplacementArchive {
            original, semantic, ..
        }
        | UploadRepresentation::FlatNumbersReplacementArchive {
            original, semantic, ..
        }
        | UploadRepresentation::FlatPagesReplacementArchive {
            original, semantic, ..
        } => (original, semantic),
        _ => return Err(JournalError::Corrupt),
    };
    if row.base.is_some()
        || row.working_file.is_some()
        || row.size > 64 * 1024 * 1024
        || row.sha256.len() != 64
        || !row.sha256.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(JournalError::Corrupt);
    }
    let backup = row
        .identity_handoff
        .as_ref()
        .and_then(|r| r.backup.as_ref());
    let complete = row.state == UploadState::Uploaded;
    if complete {
        let remote = row.remote.as_ref().ok_or(JournalError::Corrupt)?;
        let backup = backup.ok_or(JournalError::Corrupt)?;
        if row
            .identity_handoff
            .as_ref()
            .is_none_or(|reservation| !reservation.matches_native_replacement_backup(original))
            || row.package_completion.as_ref() != Some(semantic)
            || remote.id == original.id
            || remote.parent_id != original.parent_id
            || remote.name != original.name
            || backup.id != original.id
            || backup.parent_id.as_deref() != Some("FOLDER::com.apple.CloudDocs::TRASH_ROOT")
            || backup.name != original.name
            || backup.size != original.size
        {
            return Err(JournalError::Corrupt);
        }
    } else if row.remote.is_some() || backup.is_some() || row.package_completion.is_some() {
        return Err(JournalError::Corrupt);
    }
    Ok(NativeReplacementSelection {
        operation: row.id,
        sequence: row.sequence,
        state: row.state,
        original: identity(original)?,
        current: row.remote.as_ref().map(identity).transpose()?,
        recovery: backup.map(identity).transpose()?,
        handoff_receipt_recorded: complete,
    })
}
impl UploadJournal {
    #[allow(dead_code)] // Internal contract; public observer wiring follows separately.
    pub(crate) fn native_replacement_list(
        &self,
        scope: &Scope,
        after: Option<u64>,
        limit: u32,
    ) -> Result<NativeReplacementListing> {
        if scope.account != self.account
            || scope.provider != "icloud"
            || scope.collection != "drive"
            || !(1..=100).contains(&limit)
        {
            return Err(JournalError::Intent);
        }
        let after = i64::try_from(after.unwrap_or(0)).map_err(|_| JournalError::Intent)?;
        let indexed:bool=self.db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='index' AND name='native_package_replacement_operations_flat_pages_v21')",[],|r|r.get(0))?;
        let sql = if indexed {
            "SELECT id,sequence,state,CASE WHEN length(CAST(body AS BLOB))<=?3 THEN body END FROM uploads INDEXED BY native_package_replacement_operations_flat_pages_v21 WHERE sequence>?1 AND json_extract(body,'$.representation.kind') IN('package_replacement_archive','flat_numbers_replacement_archive','flat_pages_replacement_archive') ORDER BY sequence LIMIT ?2"
        } else {
            "SELECT id,sequence,state,CASE WHEN length(CAST(body AS BLOB))<=?3 THEN body END FROM uploads WHERE sequence>?1 ORDER BY sequence LIMIT ?2"
        };
        let mut statement = self.db.prepare(sql)?;
        let mut rows = statement.query(params![after, limit + 1, RECORD_BYTES])?;
        let mut operations = Vec::new();
        let mut bytes = 0;
        let mut scanned = after as u64;
        let mut processed = 0;
        while let Some(raw) = rows.next()? {
            if processed == limit {
                return Ok(NativeReplacementListing {
                    operations,
                    next: Some(scanned),
                });
            }
            let id: String = raw.get(0)?;
            let sequence: i64 = raw.get(1)?;
            let state: String = raw.get(2)?;
            let body: Option<String> = raw.get(3)?;
            let row: UploadRecord = serde_json::from_str(&body.ok_or(JournalError::Corrupt)?)?;
            if sequence <= scanned as i64
                || row.sequence != sequence as u64
                || row.id.to_string() != id
                || serde_json::to_value(row.state)?.as_str() != Some(state.as_str())
            {
                return Err(JournalError::Corrupt);
            }
            if row.scope == *scope
                && matches!(
                    row.representation,
                    UploadRepresentation::PackageReplacementArchive { .. }
                        | UploadRepresentation::FlatNumbersReplacementArchive { .. }
                        | UploadRepresentation::FlatPagesReplacementArchive { .. }
                )
            {
                let item = selection(row)?;
                let size = serde_json::to_vec(&item)?.len() + 1;
                if size > PAGE_BYTES {
                    return Err(JournalError::Corrupt);
                }
                if bytes + size > PAGE_BYTES {
                    return Ok(NativeReplacementListing {
                        operations,
                        next: Some(scanned),
                    });
                }
                bytes += size;
                operations.push(item);
            }
            scanned = sequence as u64;
            processed += 1;
        }
        Ok(NativeReplacementListing {
            operations,
            next: None,
        })
    }
}
#[cfg(test)]
mod tests;
