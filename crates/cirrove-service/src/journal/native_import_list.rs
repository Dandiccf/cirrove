//! Bounded local discovery of explicit imports; no provider, payload or worker access.
use super::*;
const PAGE_BYTES: usize = 512 * 1024 - 128;
const RECORD_BYTES: i64 = 256 * 1024;

/// Historical retained evidence. Current availability requires explicit watch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeImportSelection {
    pub operation: Uuid,
    pub sequence: u64,
    pub state: UploadState,
    pub parent: String,
    pub name: String,
    pub remote_item: Option<String>,
    pub completion_receipt_recorded: bool,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NativeImportListing {
    pub operations: Vec<NativeImportSelection>,
    /// Advance even over empty legacy pages; an overflowing record is not consumed.
    pub next: Option<u64>,
}
pub(super) fn migrate(db: &Connection) -> Result<()> {
    // Additive query index only. Read-only recovery never installs this index.
    db.execute_batch(
        "CREATE INDEX IF NOT EXISTS native_package_import_operations ON uploads(sequence)
        WHERE json_extract(body,'$.representation.kind')='package_archive';",
    )?;
    Ok(())
}
fn native_id(value: &str, prefix: &str) -> bool {
    value.starts_with(prefix)
        && value.len() > prefix.len()
        && value.len() <= 4096
        && !value.chars().any(char::is_control)
}
fn selection(row: UploadRecord) -> Result<NativeImportSelection> {
    // Mirror watch admission: explicit PACKAGE Create, never a mounted save,
    // replacement, successor, working generation or old-ID handoff.
    let UploadRepresentation::PackageArchive {
        expected_root,
        semantic,
    } = &row.representation
    else {
        return Err(JournalError::Corrupt);
    };
    let UploadIntent::Create { parent, name } = &row.intent else {
        return Err(JournalError::Corrupt);
    };
    let suffix = cirrove_core::upload::native_package_suffix(&expected_root.to_ascii_lowercase());
    if row.representation.validate().is_err()
        || row.intent.validate().is_err()
        || row.base.is_some()
        || row.identity_handoff.is_some()
        || row.working_file.is_some()
        || row.state == UploadState::Preparing
        || row.size > 64 * 1024 * 1024
        || row.sha256.len() != 64
        || !row.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        || !native_id(parent, "FOLDER::com.apple.CloudDocs::")
        || expected_root.len() > 255
        || name.len() > 255
        || suffix.is_none()
        || suffix != cirrove_core::upload::native_package_suffix(&name.to_ascii_lowercase())
        || name
            .chars()
            .any(|c| c.is_control() || matches!(c, '\\' | ':'))
    {
        return Err(JournalError::Corrupt);
    }
    let complete = row.state == UploadState::Uploaded;
    let remote_item = if complete {
        let remote = row.remote.as_ref().ok_or(JournalError::Corrupt)?;
        if row.package_completion.as_ref() != Some(semantic)
            || !native_id(&remote.id, "FILE::com.apple.CloudDocs::")
            || remote.parent_id.as_ref() != Some(parent) || &remote.name != name
            || remote.kind != NodeKind::Folder || !remote.package || remote.target.is_some()
            || remote.etag.as_ref().is_none_or(|tag| tag.is_empty() || tag.len() > 4096
                || tag.chars().any(|c| c.is_control() || c == '*'))
            // Retained create receipts may carry the old exact ETag alias.
            || remote.content_version.as_ref().is_some_and(|tag| Some(tag) != remote.etag.as_ref())
        {
            return Err(JournalError::Corrupt);
        }
        Some(remote.id.clone())
    } else {
        if row.remote.is_some() || row.package_completion.is_some() {
            return Err(JournalError::Corrupt);
        }
        None
    };
    Ok(NativeImportSelection {
        operation: row.id,
        sequence: row.sequence,
        state: row.state,
        parent: parent.clone(),
        name: name.clone(),
        remote_item,
        completion_receipt_recorded: complete,
    })
}
impl UploadJournal {
    pub(crate) fn native_import_list(
        &self,
        scope: &Scope,
        after: Option<u64>,
        limit: u32,
    ) -> Result<NativeImportListing> {
        if scope.account != self.account
            || scope.provider != "icloud"
            || scope.collection != "drive"
            || !(1..=100).contains(&limit)
        {
            return Err(JournalError::Intent);
        }
        let after = i64::try_from(after.unwrap_or(0)).map_err(|_| JournalError::Intent)?;
        let indexed: bool = self.db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='index' AND name='native_package_import_operations')", [], |r| r.get(0))?;
        // Legacy fallback scans at most limit+1 ordinary/native sequence rows.
        // Do not deserialize or return unrelated ordinary record bodies.
        let sql = if indexed {
            "SELECT id,sequence,state,json_extract(body,'$.representation.kind'),
                CASE WHEN length(CAST(body AS BLOB))<=?3 THEN body END
             FROM uploads INDEXED BY native_package_import_operations
             WHERE sequence>?1 AND json_extract(body,'$.representation.kind')='package_archive'
             ORDER BY sequence LIMIT ?2"
        } else {
            "SELECT id,sequence,state,json_extract(body,'$.representation.kind'),
                CASE WHEN json_extract(body,'$.representation.kind')='package_archive'
                    AND length(CAST(body AS BLOB))<=?3 THEN body END
             FROM uploads WHERE sequence>?1 ORDER BY sequence LIMIT ?2"
        };
        let mut query = self.db.prepare(sql)?;
        let mut rows = query.query(params![after, limit + 1, RECORD_BYTES])?;
        let mut operations = Vec::new();
        let mut bytes = 0;
        let mut scanned = after as u64;
        let mut processed = 0;
        while let Some(raw) = rows.next()? {
            if processed == limit {
                return Ok(NativeImportListing {
                    operations,
                    next: Some(scanned),
                });
            }
            let sequence: i64 = raw.get(1)?;
            if sequence <= scanned as i64 {
                return Err(JournalError::Corrupt);
            }
            let kind: Option<String> = raw.get(3)?;
            if kind.as_deref() == Some("package_archive") {
                let id: String = raw.get(0)?;
                let state: String = raw.get(2)?;
                let body: Option<String> = raw.get(4)?;
                let row: UploadRecord = serde_json::from_str(&body.ok_or(JournalError::Corrupt)?)?;
                if row.sequence != sequence as u64
                    || row.id.to_string() != id
                    || serde_json::to_value(row.state)?.as_str() != Some(state.as_str())
                    || !matches!(
                        row.representation,
                        UploadRepresentation::PackageArchive { .. }
                    )
                {
                    return Err(JournalError::Corrupt);
                }
                if row.scope == *scope {
                    let item = selection(row)?;
                    let size = serde_json::to_vec(&item)?.len() + 1;
                    if size > PAGE_BYTES {
                        return Err(JournalError::Corrupt);
                    }
                    if bytes + size > PAGE_BYTES {
                        return Ok(NativeImportListing {
                            operations,
                            next: Some(scanned),
                        });
                    }
                    bytes += size;
                    operations.push(item);
                }
            }
            scanned = sequence as u64;
            processed += 1;
        }
        Ok(NativeImportListing {
            operations,
            next: None,
        })
    }
}

#[cfg(test)]
mod tests;
