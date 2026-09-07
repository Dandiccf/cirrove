//! Capture an unchanged cloud source after a local replacement has returned.
//! Preparing operations are durable queue barriers, never uploadable snapshots.
use super::*;
use replacements::ReplacementCommit;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UploadPreparation {
    pub operation: Uuid,
    pub source: Uuid,
    pub scope: Scope,
    /// A source receipt supplies its original version; the target's receipt
    /// supplies the destination ETag independently through UploadRecord::base.
    pub source_base: Option<Uuid>,
    pub remote: Option<Node>,
    /// Stored before the complete immutable file receives its final name.
    #[serde(default)]
    pub sha256: Option<String>,
}

pub(super) fn migrate(db: &mut Connection, version: u32) -> Result<()> {
    if version < 13 {
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute_batch("CREATE TABLE IF NOT EXISTS upload_preparations(operation TEXT PRIMARY KEY,body TEXT NOT NULL);")?;
        tx.pragma_update(None, "user_version", 13)?;
        tx.commit()?;
    }
    db.prepare("SELECT operation,body FROM upload_preparations LIMIT 0")?;
    // Download attempts belong to the old process. A prepared immutable file
    // can be adopted, but neither a queue barrier nor upload is acknowledged.
    db.execute(
        "UPDATE uploads SET body=json_set(body,'$.attempt',NULL) WHERE state='preparing'",
        [],
    )?;
    // New-process opens can still request remote source bytes until its working
    // stream is materialized. They need a new reader gate even though none of
    // the old process's descriptors survived.
    db.execute(
        "UPDATE file_replacements SET body=json_set(body,'$.local_ready',json('false'))
        WHERE id IN(SELECT u.id FROM uploads u JOIN upload_preparations p ON p.operation=u.id
            JOIN namespace_objects o ON o.id=json_extract(p.body,'$.source')
            WHERE json_extract(u.body,'$.sha256')='' AND o.working IS NULL)",
        [],
    )?;
    Ok(())
}

impl UploadJournal {
    pub(super) fn enqueue_preparing_replacement(
        &mut self,
        scope: Scope,
        intent: UploadIntent,
        order: WriteOrder,
        plan: ReplacementCommit,
    ) -> Result<UploadRecord> {
        if scope.account != self.account || scope != plan.source.scope {
            return Err(JournalError::Account);
        }
        if plan.working_id().is_some() || plan.source.node.size > i64::MAX as u64 {
            return Err(JournalError::Intent);
        }
        intent.validate().map_err(|_| JournalError::Intent)?;
        if let Some(base) = &order.base {
            self.ensure_successor_free(base.predecessor)?;
        }
        barriers::validate(&self.db, &scope, &order.prerequisites)?;
        let mut record = UploadRecord {
            id: Uuid::new_v4(),
            sequence: 0,
            scope: scope.clone(),
            intent,
            state: UploadState::Preparing,
            size: plan.source.node.size,
            sha256: String::new(),
            attempt: None,
            remote: None,
            base: order.base,
            working_file: None,
            session_key: None,
            transferred_bytes: 0,
            retry_at: 0,
            failed_attempts: 0,
        };
        let preparation = UploadPreparation {
            operation: record.id,
            source: plan.source.id,
            scope,
            source_base: plan.source.latest,
            remote: if plan.source.latest.is_none() {
                plan.source.remote.clone()
            } else {
                None
            },
            sha256: None,
        };
        if preparation.source_base.is_none()
            && preparation
                .remote
                .as_ref()
                .is_none_or(|n| n.content_revision().is_none())
        {
            return Err(JournalError::Intent);
        }
        let tx = self.db.transaction()?;
        record.sequence = mutations::queue_insert(
            &tx,
            record.id,
            mutations::upload_resources(&record.scope, &record.intent)?,
        )?;
        generations::insert_dependency(&tx, record.id, record.sequence, record.base.as_ref())?;
        barriers::insert(
            &tx,
            record.id,
            record.sequence,
            &record.scope,
            &order.prerequisites,
        )?;
        tx.execute(
            "INSERT INTO uploads(id,resource,state,body,sequence) VALUES(?1,?2,'preparing',?3,?4)",
            params![
                record.id.to_string(),
                resource(&record.intent, &record.scope)?,
                serde_json::to_string(&record)?,
                record.sequence as i64
            ],
        )?;
        tx.execute(
            "INSERT INTO upload_preparations VALUES(?1,?2)",
            params![record.id.to_string(), serde_json::to_string(&preparation)?],
        )?;
        replacements::commit(&tx, &plan, &record)?;
        tx.commit()?;
        Ok(record)
    }

    pub fn upload_preparation(&self, id: Uuid) -> Result<UploadPreparation> {
        let body: Option<String> = self
            .db
            .query_row(
                "SELECT body FROM upload_preparations WHERE operation=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        Ok(serde_json::from_str(&body.ok_or(JournalError::Missing)?)?)
    }

    /// At most one preparation is acquired per call. Source receipt barriers
    /// must be complete; target uploads and reader preservation may proceed in
    /// parallel. Downloading a source cannot authorize target publication.
    pub fn claim_preparation(&mut self) -> Result<Option<(UploadRecord, UploadPreparation)>> {
        let id: Option<String> = self.db.query_row(
            "SELECT u.id FROM uploads u WHERE u.state='preparing'
             AND json_extract(u.body,'$.attempt') IS NULL
             AND coalesce(json_extract(u.body,'$.retry_at'),0)<=?1
             AND NOT EXISTS(SELECT 1 FROM write_prerequisites b LEFT JOIN write_queue p ON p.id=b.predecessor
                WHERE b.operation=u.id AND coalesce(p.complete,0)!=1)
             ORDER BY u.sequence LIMIT 1", [now_seconds() as i64], |r|r.get(0)).optional()?;
        let Some(id) = id else { return Ok(None) };
        let id = Uuid::parse_str(&id).map_err(|_| JournalError::Corrupt)?;
        let mut record = self.get(id)?;
        let mut preparation = self.upload_preparation(id)?;
        if preparation.operation != id || preparation.scope != record.scope {
            return Err(JournalError::Corrupt);
        }
        if preparation.remote.is_none() {
            preparation.remote = self
                .operation(preparation.source_base.ok_or(JournalError::Corrupt)?)?
                .confirmed_node();
        }
        if preparation.remote.as_ref().is_none_or(|n| {
            n.kind != NodeKind::File
                || n.target.is_some()
                || n.size != record.size
                || n.content_revision().is_none()
                || n.etag.is_none()
        }) {
            record.state = UploadState::Failed;
            self.save(&record)?;
            return Err(JournalError::Corrupt);
        }
        record.attempt = Some(Uuid::new_v4());
        // Freeze the resolved source, so retries never follow a newer edit.
        let tx = self.db.transaction()?;
        tx.execute(
            "UPDATE upload_preparations SET body=?2 WHERE operation=?1",
            params![id.to_string(), serde_json::to_string(&preparation)?],
        )?;
        tx.execute(
            "UPDATE uploads SET body=?2 WHERE id=?1",
            params![id.to_string(), serde_json::to_string(&record)?],
        )?;
        tx.commit()?;
        Ok(Some((record, preparation)))
    }

    fn active_preparation(
        &self,
        id: Uuid,
        attempt: Uuid,
    ) -> Result<(UploadRecord, UploadPreparation)> {
        let record = self.get(id)?;
        if record.state != UploadState::Preparing || record.attempt != Some(attempt) {
            return Err(JournalError::Stale);
        }
        let preparation = self.upload_preparation(id)?;
        if preparation.remote.is_none()
            || preparation.scope != record.scope
            || preparation.operation != id
        {
            return Err(JournalError::Corrupt);
        }
        Ok((record, preparation))
    }

    pub fn reserve_preparation(&mut self, id: Uuid, attempt: Uuid) -> Result<WorkingSource> {
        let (record, _) = self.active_preparation(id, attempt)?;
        self.reserve_working_named(record.size, &format!(".cirrove-preparing-{id}-"))
    }

    pub(super) fn recover_preparation_files(&mut self) -> Result<()> {
        for directory in [&self.working, &self.objects] {
            let mut changed = false;
            for entry in std::fs::read_dir(directory)? {
                let entry = entry?;
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                let Some(rest) = name.strip_prefix(".cirrove-preparing-") else {
                    continue;
                };
                let Some(id) = rest.get(..36).and_then(|s| Uuid::parse_str(s).ok()) else {
                    continue;
                };
                if rest.as_bytes().get(36) != Some(&b'-') {
                    continue;
                }
                let preparation = self.upload_preparation(id)?;
                if preparation.scope.account != self.account {
                    return Err(JournalError::Account);
                }
                let file = OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(entry.path())?;
                owned_private(&file)?;
                // Only operation-labelled remote download/snapshot temporaries.
                // Mutable edits and finalized snapshots have plain UUID names.
                std::fs::remove_file(entry.path())?;
                changed = true;
            }
            if changed {
                File::open(directory)?.sync_all()?;
            }
        }
        Ok(())
    }

    pub fn defer_preparation(
        &mut self,
        id: Uuid,
        attempt: Uuid,
        conflict: bool,
        delay: std::time::Duration,
    ) -> Result<()> {
        let (mut record, _) = self.active_preparation(id, attempt)?;
        record.attempt = None;
        if conflict {
            record.state = UploadState::Conflict;
        }
        record.failed_attempts = record.failed_attempts.saturating_add(1);
        record.retry_at = now_seconds()
            .saturating_add(delay.as_secs())
            .min(i64::MAX as u64);
        self.save(&record)
    }

    /// A file at this operation's immutable path was published only after all
    /// source bytes were supplied and fsynced. Reopening verifies its private
    /// descriptor and length; incomplete downloads retain temporary names.
    fn prepared_payload(&self, record: &UploadRecord) -> Result<Option<(File, String)>> {
        let mut file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.objects.join(record.id.to_string()))
        {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(JournalError::Corrupt),
        };
        owned_private(&file)?;
        if file.metadata()?.len() != record.size
            || file.metadata()?.permissions().mode() & 0o222 != 0
        {
            return Err(JournalError::Corrupt);
        }
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 128 * 1024];
        loop {
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hash.update(&buffer[..n]);
        }
        file.seek(SeekFrom::Start(0))?;
        let digest = hex::encode(hash.finalize());
        if self.upload_preparation(record.id)?.sha256.as_deref() != Some(digest.as_str()) {
            return Err(JournalError::Corrupt);
        }
        Ok(Some((file, digest)))
    }

    /// Finish the same original operation, not a later save on its source.
    /// Existing working bytes (including newer edits) are never overwritten.
    pub fn complete_preparation(
        &mut self,
        id: Uuid,
        attempt: Uuid,
        source: WorkingSource,
    ) -> Result<()> {
        let (record, mut preparation) = self.active_preparation(id, attempt)?;
        let mut input = source.complete_descriptor(&self.working)?;
        if input.metadata()?.len() != record.size {
            return Err(JournalError::Corrupt);
        }
        let existing = self.prepared_payload(&record)?;
        let (retained, files) = self.retained_usage()?;
        if existing.is_none()
            && (files >= 10_000 || record.size > self.quota.saturating_sub(retained))
        {
            return Err(JournalError::Quota);
        }
        let mut temporary = if existing.is_none() {
            Some(
                tempfile::Builder::new()
                    .prefix(&format!(".cirrove-preparing-{id}-"))
                    .tempfile_in(&self.objects)?,
            )
        } else {
            None
        };
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 128 * 1024];
        loop {
            let n = input.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            if let Some(file) = &mut temporary {
                file.write_all(&buffer[..n])?;
            }
            hash.update(&buffer[..n]);
        }
        let digest = hex::encode(hash.finalize());
        if preparation
            .sha256
            .as_ref()
            .is_some_and(|expected| expected != &digest)
        {
            return Err(JournalError::Corrupt);
        }
        if let Some((_, previous)) = existing
            && previous != digest
        {
            return Err(JournalError::Corrupt);
        }
        if let Some(temporary) = &temporary {
            temporary
                .as_file()
                .set_permissions(std::fs::Permissions::from_mode(0o400))?;
            temporary.as_file().sync_all()?;
        }
        if preparation.sha256.is_none() {
            preparation.sha256 = Some(digest.clone());
            self.db.execute(
                "UPDATE upload_preparations SET body=?2 WHERE operation=?1",
                params![id.to_string(), serde_json::to_string(&preparation)?],
            )?;
        }
        if let Some(temporary) = temporary {
            temporary
                .persist_noclobber(self.objects.join(id.to_string()))
                .map_err(|_| JournalError::Storage)?;
            File::open(&self.objects)?.sync_all()?;
        }
        self.finish_preparation(record, preparation, digest, Some(source))
    }

    /// Recover a completed snapshot whose final SQL publication was interrupted.
    /// This never adopts partial temporary bytes or acknowledges a cloud upload.
    pub fn resume_prepared(&mut self, id: Uuid, attempt: Uuid) -> Result<bool> {
        let (record, preparation) = self.active_preparation(id, attempt)?;
        let Some((mut file, digest)) = self.prepared_payload(&record)? else {
            return Ok(false);
        };
        let source = if self
            .namespace_object(preparation.source)?
            .working_file
            .is_none()
        {
            let mut source = self.reserve_preparation(id, attempt)?;
            let mut buffer = [0u8; 128 * 1024];
            loop {
                let n = file.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                source.write_chunk(&buffer[..n])?;
            }
            Some(source)
        } else {
            None
        };
        self.finish_preparation(record, preparation, digest, source)?;
        Ok(true)
    }

    fn finish_preparation(
        &mut self,
        mut record: UploadRecord,
        preparation: UploadPreparation,
        digest: String,
        source: Option<WorkingSource>,
    ) -> Result<()> {
        let mut object = self.namespace_object(preparation.source)?;
        if object.working_file.is_none() {
            let source = source.ok_or(JournalError::Corrupt)?;
            self.publish_working_for(
                object.id,
                preparation.scope,
                preparation.remote.ok_or(JournalError::Corrupt)?,
                source,
            )?;
            object = self.namespace_object(object.id)?;
        }
        record.working_file = object.working_file;
        record.sha256 = digest;
        record.state = UploadState::Pending;
        record.attempt = None;
        record.retry_at = 0;
        self.save(&record)
    }
}
