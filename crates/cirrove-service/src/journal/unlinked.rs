//! A removed directory entry does not end an open file's local lifetime.
use super::*;
use cirrove_core::mutation::{MutationIntent, MutationRequest};

pub(super) fn migrate(db: &mut Connection, version: u32) -> Result<()> {
    if version < 9 {
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute_batch(
            "CREATE TABLE working_streams_migration (
            id TEXT PRIMARY KEY, identity TEXT NOT NULL UNIQUE,
            slot TEXT UNIQUE, body TEXT NOT NULL);
            INSERT INTO working_streams_migration SELECT id,identity,slot,body FROM working_files;
            DROP TABLE working_files;
            ALTER TABLE working_streams_migration RENAME TO working_files;",
        )?;
        tx.pragma_update(None, "user_version", 9)?;
        tx.commit()?;
    }
    let required: bool = db.query_row(
        "SELECT \"notnull\" FROM pragma_table_info('working_files') WHERE name='slot'",
        [],
        |r| r.get(0),
    )?;
    if required {
        return Err(JournalError::Corrupt);
    }
    db.execute_batch(
        "CREATE INDEX IF NOT EXISTS mutation_local_readers ON mutations(sequence)
        WHERE coalesce(json_extract(body,'$.local_ready'),1)=0;",
    )?;
    Ok(())
}

pub struct UnlinkedFile {
    pub object: NamespaceObject,
    pub working: Option<WorkingFile>,
    pub mutation: MutationRecord,
}

impl UploadJournal {
    /// Atomically release the name and queue conditional remote deletion. The
    /// caller seals dirty bytes first. A reader barrier delays cloud deletion
    /// without holding the kernel's parent-directory lock during preservation.
    /// Descriptor writes after this point never create another upload.
    pub fn unlink_namespace_file(
        &mut self,
        id: Uuid,
        revision: u64,
        preserve_readers: bool,
    ) -> Result<UnlinkedFile> {
        let mut object = self.namespace_object(id)?;
        if object.scope.account != self.account {
            return Err(JournalError::Account);
        }
        if object.unlinked || object.follows_remote || object.revision != revision {
            return Err(JournalError::Stale);
        }
        if object.node.kind != NodeKind::File || object.node.target.is_some() {
            return Err(JournalError::Intent);
        }
        let mut working = object
            .working_file
            .map(|id| self.working_file(id))
            .transpose()?;
        if working.as_ref().is_some_and(|w| {
            w.dirty || w.unlinked || w.latest != object.latest || w.node != object.node
        }) {
            return Err(JournalError::Stale);
        }
        let before = if object.latest.is_some() {
            object.node.clone()
        } else {
            object.remote.clone().ok_or(JournalError::Stale)?
        };
        let request = MutationRequest {
            scope: object.scope.clone(),
            intent: MutationIntent::RemoveFile { before },
        };
        let base = if let Some(predecessor) = object.latest {
            self.validate_mutation_base(predecessor, &request)?;
            self.ensure_successor_free(predecessor)?;
            Some(WriteBase {
                predecessor,
                resolved: false,
            })
        } else {
            request.validate().map_err(|_| JournalError::Intent)?;
            None
        };
        let mut mutation = MutationRecord {
            id: Uuid::new_v4(),
            sequence: 0,
            request,
            state: MutationState::Pending,
            attempt: None,
            receipt: None,
            retry_at: 0,
            failed_attempts: 0,
            base,
            working_file: object.working_file,
            local_ready: !preserve_readers,
        };
        object.unlinked = true;
        object.latest = Some(mutation.id);
        object.revision = object.revision.checked_add(1).ok_or(JournalError::Quota)?;
        if let Some(file) = &mut working {
            file.unlinked = true;
            file.latest = Some(mutation.id);
            file.generation = file.generation.checked_add(1).ok_or(JournalError::Quota)?;
        }
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        mutation.sequence = super::mutations::queue_insert(
            &tx,
            mutation.id,
            super::mutations::mutation_resources(&mutation.request)?,
        )?;
        super::generations::insert_dependency(
            &tx,
            mutation.id,
            mutation.sequence,
            mutation.base.as_ref(),
        )?;
        tx.execute(
            "INSERT INTO mutations VALUES(?1,?2,'pending',?3)",
            params![
                mutation.sequence as i64,
                mutation.id.to_string(),
                serde_json::to_string(&mutation)?
            ],
        )?;
        if let Some(file) = &working
            && tx.execute(
                "UPDATE working_files SET slot=NULL,body=?2 WHERE id=?1",
                params![file.id.to_string(), serde_json::to_string(file)?],
            )? != 1
        {
            return Err(JournalError::Stale);
        }
        super::namespace::save(&tx, &object)?;
        tx.commit()?;
        Ok(UnlinkedFile {
            object,
            working,
            mutation,
        })
    }
    pub fn unlinked_readers(&self, after: u64, limit: u32) -> Result<Vec<MutationRecord>> {
        let mut query = self.db.prepare(
            "SELECT body FROM mutations WHERE sequence>?1
            AND coalesce(json_extract(body,'$.local_ready'),1)=0 ORDER BY sequence LIMIT ?2",
        )?;
        query
            .query_map(
                params![after.min(i64::MAX as u64) as i64, limit.min(32)],
                |r| r.get::<_, String>(0),
            )?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }
    /// Called only after current-process readers are local or gone. Receipt
    /// prerequisites still independently gate the eventual conditional DELETE.
    pub fn release_unlinked_readers(&mut self, id: Uuid) -> Result<()> {
        let mut operation = self.mutation(id)?;
        if operation.local_ready {
            return Ok(());
        }
        let object = self
            .namespace_for_operation(id)?
            .ok_or(JournalError::Corrupt)?;
        if !object.unlinked
            || object.latest != Some(id)
            || !matches!(
                operation.state,
                MutationState::Pending | MutationState::VerifyRequired
            )
            || !matches!(operation.request.intent, MutationIntent::RemoveFile { .. })
        {
            return Err(JournalError::Corrupt);
        }
        operation.local_ready = true;
        self.save_mutation(&operation)
    }
    pub(super) fn recover_unlinked_readers(&mut self) -> Result<()> {
        // Exclusive journal ownership proves no previous live filesystem owner.
        // Process-local descriptors cannot survive a dead FUSE connection. This
        // releases only reader barriers; pending bytes and remote lineage remain.
        loop {
            let pending = self.unlinked_readers(0, 32)?;
            if pending.is_empty() {
                return Ok(());
            }
            for operation in pending {
                self.release_unlinked_readers(operation.id)?;
            }
        }
    }
}
