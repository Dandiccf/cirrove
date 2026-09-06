//! Mutable application bytes and their atomically linked immutable saves.
//! All calls use the journal owner's blocking-worker lock. No network work here.
use super::*;
use cirrove_core::mutation::{MutationIntent, MutationRequest};
use std::os::unix::fs::FileExt;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkingFile {
    pub id: Uuid,
    pub scope: Scope,
    pub node: Node,
    pub intent: UploadIntent,
    pub latest: Option<Uuid>,
    pub dirty: bool,
    pub generation: u64,
    /// Preserve the provider's original content tag separately from the mutable
    /// working-file revision, for the first metadata-only operation.
    #[serde(default)]
    pub initial_remote: Option<Node>,
}
impl WorkingFile {
    fn mutation_source(&self) -> Node {
        if self.latest.is_some() {
            return self.node.clone();
        }
        self.initial_remote.clone().unwrap_or_else(|| {
            // Legacy working copies replaced their original content tag. Do not
            // present that local tag as evidence of the remote content version.
            let mut node = self.node.clone();
            node.content_version = None;
            node
        })
    }
}

/// A quota-reserved source being hydrated outside the journal lock. It cannot
/// become visible before every expected byte has been provided and fsynced.
pub struct WorkingSource {
    temporary: tempfile::NamedTempFile,
    size: u64,
    written: u64,
}
impl WorkingSource {
    pub fn write_chunk(&mut self, bytes: &[u8]) -> Result<()> {
        let end = self
            .written
            .checked_add(bytes.len() as u64)
            .ok_or(JournalError::Quota)?;
        if end > self.size {
            return Err(JournalError::Corrupt);
        }
        self.temporary.as_file().write_all_at(bytes, self.written)?;
        self.written = end;
        Ok(())
    }
}

pub(super) struct WorkingCommit {
    pub(super) id: Uuid,
    generation: u64,
    previous: Option<Uuid>,
}

pub(super) fn migrate(db: &mut Connection, version: u32) -> Result<()> {
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS working_files (
        id TEXT PRIMARY KEY, identity TEXT NOT NULL UNIQUE,
        slot TEXT NOT NULL UNIQUE, body TEXT NOT NULL);",
    )?;
    tx.pragma_update(None, "user_version", version.max(5))?;
    tx.commit()?;
    Ok(())
}

fn slot(record: &WorkingFile) -> Result<String> {
    Ok(serde_json::to_string(&(
        &record.scope,
        &record.node.parent_id,
        record.node.name.to_lowercase(),
    ))?)
}

impl UploadJournal {
    pub fn owns_account(&self, account: &str) -> bool {
        self.account == account
    }

    pub fn reserve_working(&mut self, size: u64) -> Result<WorkingSource> {
        let (retained, files) = self.retained_usage()?;
        if size > i64::MAX as u64 || files >= 10_000 || size > self.quota.saturating_sub(retained) {
            return Err(JournalError::Quota);
        }
        let temporary = tempfile::NamedTempFile::new_in(&self.working)?;
        // Logical reservation is included by retained_usage during hydration.
        // Physical ENOSPC is still possible and must fail the local operation.
        temporary.as_file().set_len(size)?;
        Ok(WorkingSource {
            temporary,
            size,
            written: 0,
        })
    }
    pub fn working_files(&self) -> Result<Vec<WorkingFile>> {
        let mut query = self
            .db
            .prepare("SELECT body FROM working_files ORDER BY id")?;
        query
            .query_map([], |r| r.get::<_, String>(0))?
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect()
    }

    pub fn working_file(&self, id: Uuid) -> Result<WorkingFile> {
        let body: String = self
            .db
            .query_row(
                "SELECT body FROM working_files WHERE id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(JournalError::Missing)?;
        Ok(serde_json::from_str(&body)?)
    }

    pub fn working_by_identity(&self, scope: &Scope, item: &str) -> Result<Option<WorkingFile>> {
        let body: Option<String> = self
            .db
            .query_row(
                "SELECT body FROM working_files WHERE identity=?1",
                [serde_json::to_string(&(scope, item))?],
                |r| r.get(0),
            )
            .optional()?;
        body.map(|b| Ok(serde_json::from_str(&b)?)).transpose()
    }

    fn working_descriptor(&self, id: Uuid, writable: bool) -> Result<File> {
        self.working_file(id)?;
        let file = OpenOptions::new()
            .read(true)
            .write(writable)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.working.join(id.to_string()))?;
        owned_private(&file)?;
        Ok(file)
    }

    pub fn read_working(&self, id: Uuid, offset: u64, count: u32) -> Result<Vec<u8>> {
        let file = self.working_descriptor(id, false)?;
        let count = (file.metadata()?.len().saturating_sub(offset))
            .min(u64::from(count))
            .min(8 * 1024 * 1024);
        let mut bytes = vec![0; count as usize];
        file.read_exact_at(&mut bytes, offset)?;
        Ok(bytes)
    }

    fn save_working(&mut self, record: &WorkingFile) -> Result<()> {
        if self.db.execute(
            "UPDATE working_files SET slot=?2,body=?3 WHERE id=?1",
            params![
                record.id.to_string(),
                slot(record)?,
                serde_json::to_string(record)?
            ],
        )? != 1
        {
            return Err(JournalError::Missing);
        }
        Ok(())
    }

    /// The supplied bytes must be a complete, version-checked source for an
    /// existing file. Hydrate it outside this journal lock. New files are empty.
    pub fn create_working(
        &mut self,
        scope: Scope,
        node: Node,
        new: bool,
        mut bytes: impl Read,
    ) -> Result<WorkingFile> {
        let mut source = self.reserve_working(node.size)?;
        let mut buffer = [0; 128 * 1024];
        loop {
            let count = bytes.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            source.write_chunk(&buffer[..count])?;
        }
        self.publish_working(scope, node, new, source)
    }

    pub fn publish_working(
        &mut self,
        scope: Scope,
        mut node: Node,
        new: bool,
        source: WorkingSource,
    ) -> Result<WorkingFile> {
        if source.temporary.path().parent() != Some(self.working.as_path())
            || source.written != source.size
            || node.size != source.size
        {
            return Err(JournalError::Corrupt);
        }
        if scope.account != self.account {
            return Err(JournalError::Account);
        }
        if scope.provider.is_empty()
            || scope.collection.is_empty()
            || node.kind != NodeKind::File
            || node.target.is_some()
            || (new && node.size != 0)
        {
            return Err(JournalError::Intent);
        }
        let intent = if new {
            UploadIntent::Create {
                parent: node.parent_id.clone().ok_or(JournalError::Intent)?,
                name: node.name.clone(),
            }
        } else {
            UploadIntent::Replace {
                item: node.id.clone(),
                expected_etag: node.etag.clone().ok_or(JournalError::Intent)?,
            }
        };
        intent.validate().map_err(|_| JournalError::Intent)?;
        // Validate the name/parent even for replacements (whose upload intent
        // only contains the stable remote ID and original ETag).
        UploadIntent::Create {
            parent: node.parent_id.clone().ok_or(JournalError::Intent)?,
            name: node.name.clone(),
        }
        .validate()
        .map_err(|_| JournalError::Intent)?;
        let id = Uuid::new_v4();
        let initial_remote = (!new).then(|| node.clone());
        if new {
            node.id = format!("local-{id}");
        }
        node.content_version = Some(format!("working-{id}"));
        let record = WorkingFile {
            id,
            scope,
            node,
            intent,
            latest: None,
            dirty: new,
            generation: 0,
            initial_remote,
        };
        let identity = serde_json::to_string(&(&record.scope, &record.node.id))?;
        let slot = slot(&record)?;
        let exists: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM working_files WHERE identity=?1 OR slot=?2)",
            params![identity, slot],
            |r| r.get(0),
        )?;
        if exists {
            return Err(JournalError::Stale);
        }
        let temporary = source.temporary;
        temporary.as_file().sync_all()?;
        temporary
            .persist_noclobber(self.working.join(id.to_string()))
            .map_err(|_| JournalError::Storage)?;
        File::open(&self.working)?.sync_all()?;
        // Publication failures retain the orphan and count it against quota.
        self.db.execute(
            "INSERT INTO working_files(id,identity,slot,body) VALUES(?1,?2,?3,?4)",
            params![
                id.to_string(),
                identity,
                slot,
                serde_json::to_string(&record)?
            ],
        )?;
        Ok(record)
    }

    fn prepare_working_change(&mut self, id: Uuid, size: u64) -> Result<(WorkingFile, File)> {
        if size > i64::MAX as u64 {
            return Err(JournalError::Quota);
        }
        let mut record = self.working_file(id)?;
        let file = self.working_descriptor(id, true)?;
        let growth = size.saturating_sub(file.metadata()?.len());
        if growth > self.quota.saturating_sub(self.retained_bytes()?) {
            return Err(JournalError::Quota);
        }
        // Persist dirty BEFORE changing bytes. A partial write or crash can then
        // never make recovery discard this mutable copy as an unchanged cache.
        record.dirty = true;
        record.generation = record
            .generation
            .checked_add(1)
            .ok_or(JournalError::Quota)?;
        self.save_working(&record)?;
        Ok((record, file))
    }

    pub fn write_working(
        &mut self,
        id: Uuid,
        offset: u64,
        bytes: &[u8],
    ) -> Result<(u32, WorkingFile)> {
        if bytes.len() > 8 * 1024 * 1024 {
            return Err(JournalError::Intent);
        }
        if bytes.is_empty() {
            return Ok((0, self.working_file(id)?));
        }
        let end = offset
            .checked_add(bytes.len() as u64)
            .ok_or(JournalError::Quota)?;
        let (mut record, file) = self.prepare_working_change(id, end)?;
        let count = file.write_at(bytes, offset)?;
        record.node.size = file.metadata()?.len();
        record.node.modified_unix = now_seconds();
        self.save_working(&record)?;
        Ok((count as u32, record))
    }

    pub fn truncate_working(&mut self, id: Uuid, size: u64) -> Result<WorkingFile> {
        let (mut record, file) = self.prepare_working_change(id, size)?;
        file.set_len(size)?;
        record.node.size = size;
        record.node.modified_unix = now_seconds();
        self.save_working(&record)?;
        Ok(record)
    }

    pub fn seal_working(&mut self, id: Uuid) -> Result<Option<UploadRecord>> {
        let record = self.working_file(id)?;
        let mut bytes = self.working_descriptor(id, true)?;
        bytes.sync_all()?;
        if !record.dirty {
            return Ok(None);
        }
        let (intent, base) = match record.latest {
            Some(previous) => (
                self.upload_intent_after(previous)?.1,
                Some(UploadBase {
                    predecessor: previous,
                    resolved: false,
                }),
            ),
            None => (record.intent.clone(), None),
        };
        self.enqueue_generation(
            record.scope,
            intent,
            base,
            Some(WorkingCommit {
                id,
                generation: record.generation,
                previous: record.latest,
            }),
            &mut bytes,
        )
        .map(Some)
    }

    /// Persist a local working-file relocation together with its ordered remote
    /// intent. No provider request is made here, including for a pending create.
    /// Replacement of another local object is a separate namespace operation.
    pub fn relocate_working(
        &mut self,
        id: Uuid,
        parent: String,
        name: String,
    ) -> Result<MutationRecord> {
        UploadIntent::Create {
            parent: parent.clone(),
            name: name.clone(),
        }
        .validate()
        .map_err(|_| JournalError::Intent)?;
        let current = self.working_file(id)?;
        if current.node.id == parent {
            return Err(JournalError::Intent);
        }
        if current.dirty {
            self.seal_working(id)?;
        }
        let mut working = self.working_file(id)?;
        let request = MutationRequest {
            scope: working.scope.clone(),
            intent: MutationIntent::Relocate {
                before: working.mutation_source(),
                parent: parent.clone(),
                name: name.clone(),
            },
        };
        let base = match working.latest {
            Some(predecessor) => {
                self.validate_mutation_base(predecessor, &request)?;
                self.ensure_successor_free(predecessor)?;
                Some(WriteBase {
                    predecessor,
                    resolved: false,
                })
            }
            None => {
                request.validate().map_err(|_| JournalError::Intent)?;
                None
            }
        };
        working.node.parent_id = Some(parent);
        working.node.name = name;
        working.generation = working
            .generation
            .checked_add(1)
            .ok_or(JournalError::Quota)?;
        self.enqueue_bound_mutation(request, base, Some(working))
    }

    pub(super) fn recover_working(&mut self) -> Result<()> {
        for mut record in self.working_files()? {
            let size = self.working_descriptor(record.id, false)?.metadata()?.len();
            if size != record.node.size {
                if !record.dirty {
                    return Err(JournalError::Corrupt);
                }
                record.node.size = size;
                self.save_working(&record)?;
            }
        }
        Ok(())
    }
}

pub(super) fn commit_relocation(
    tx: &rusqlite::Transaction<'_>,
    mut working: WorkingFile,
    mutation: &MutationRecord,
) -> Result<()> {
    let body: String = tx.query_row(
        "SELECT body FROM working_files WHERE id=?1",
        [working.id.to_string()],
        |r| r.get(0),
    )?;
    let old: WorkingFile = serde_json::from_str(&body)?;
    if old.dirty
        || old.latest != working.latest
        || old.scope != working.scope
        || old.node.id != working.node.id
        || old.generation.checked_add(1) != Some(working.generation)
        || mutation.base.as_ref().map(|b| b.predecessor) != old.latest
        || mutation.request.scope != old.scope
        || mutation.working_file != Some(old.id)
    {
        return Err(JournalError::Stale);
    }
    let MutationIntent::Relocate {
        before,
        parent,
        name,
    } = &mutation.request.intent
    else {
        return Err(JournalError::Intent);
    };
    if before != &old.mutation_source()
        || working.node.parent_id.as_ref() != Some(parent)
        || &working.node.name != name
    {
        return Err(JournalError::Stale);
    }
    let occupied: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM working_files WHERE slot=?1 AND id!=?2)",
        params![slot(&working)?, working.id.to_string()],
        |r| r.get(0),
    )?;
    if occupied {
        return Err(JournalError::Stale);
    }
    working.latest = Some(mutation.id);
    tx.execute(
        "UPDATE working_files SET slot=?2,body=?3 WHERE id=?1",
        params![
            working.id.to_string(),
            slot(&working)?,
            serde_json::to_string(&working)?
        ],
    )?;
    Ok(())
}

pub(super) fn commit_generation(
    tx: &rusqlite::Transaction<'_>,
    commit: &WorkingCommit,
    upload: &UploadRecord,
) -> Result<()> {
    let body: String = tx.query_row(
        "SELECT body FROM working_files WHERE id=?1",
        [commit.id.to_string()],
        |r| r.get(0),
    )?;
    let mut record: WorkingFile = serde_json::from_str(&body)?;
    if !record.dirty
        || record.generation != commit.generation
        || record.latest != commit.previous
        || record.scope != upload.scope
    {
        return Err(JournalError::Stale);
    }
    record.latest = Some(upload.id);
    record.dirty = false;
    record.node.size = upload.size;
    tx.execute(
        "UPDATE working_files SET body=?2 WHERE id=?1",
        params![commit.id.to_string(), serde_json::to_string(&record)?],
    )?;
    Ok(())
}
