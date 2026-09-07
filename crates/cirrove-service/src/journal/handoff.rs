//! Detach acknowledged working bytes without losing the stable local identity.
//! Callers must exclude open/in-flight users of the object for the commit. Network
//! metadata is obtained before that exclusion and checked against its revision.
use super::*;

pub(super) fn migrate(db: &mut Connection, version: u32) -> Result<()> {
    if version < 8 {
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let has_column: bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('uploads') WHERE name='payload_present')",[],|r|r.get(0))?;
        if !has_column {
            tx.execute_batch("ALTER TABLE uploads ADD COLUMN payload_present INTEGER NOT NULL DEFAULT 1 CHECK(payload_present IN (0,1));")?;
        }
        tx.execute_batch("CREATE TABLE IF NOT EXISTS retired_working(id TEXT PRIMARY KEY);
            CREATE INDEX IF NOT EXISTS uploaded_payloads ON uploads(sequence) WHERE state='uploaded' AND payload_present=1;
            CREATE INDEX IF NOT EXISTS namespace_handoff_candidates ON namespace_objects(id) WHERE coalesce(json_extract(body,'$.follows_remote'),0)=0;
            CREATE INDEX IF NOT EXISTS namespace_operation_objects ON namespace_operations(object);")?;
        tx.pragma_update(None, "user_version", 8)?;
        tx.commit()?;
    }
    db.prepare("SELECT id FROM retired_working LIMIT 0")?;
    db.prepare("SELECT payload_present FROM uploads LIMIT 0")?;
    Ok(())
}

impl UploadJournal {
    /// Acknowledged immutable payloads are no longer pending edits. Keep their
    /// receipts and lineage, and checkpoint collection so each pass is bounded.
    pub fn collect_uploaded_payloads(&mut self, limit: u32) -> Result<usize> {
        let ids = {
            let mut query = self.db.prepare(
                "SELECT id FROM uploads WHERE state='uploaded' AND payload_present=1
                ORDER BY sequence LIMIT ?1",
            )?;
            query
                .query_map([limit.min(1000)], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        for id in &ids {
            let id = Uuid::parse_str(id).map_err(|_| JournalError::Corrupt)?;
            self.prune_uploaded_payload(id)?;
            self.db.execute(
                "UPDATE uploads SET payload_present=0 WHERE id=?1 AND state='uploaded'",
                [id.to_string()],
            )?;
        }
        Ok(ids.len())
    }
    /// Sealed is not uploaded. Every operation attached to this object must be
    /// confirmed, and a newer dirty generation must never be discarded.
    pub fn namespace_is_clean(&self, object: &NamespaceObject) -> Result<bool> {
        if object.scope.account != self.account
            || object.follows_remote
            || object.unlinked
            || object.remote.is_none()
        {
            return Ok(false);
        }
        if let Some(id) = object.working_file {
            let working = self.working_file(id)?;
            if working.dirty || working.latest != object.latest || working.node != object.node {
                return Ok(false);
            }
        }
        let pending: bool = self.db.query_row("SELECT EXISTS(
            SELECT 1 FROM namespace_operations n
            LEFT JOIN uploads u ON u.id=n.operation LEFT JOIN mutations m ON m.id=n.operation
            WHERE n.object=?1 AND (coalesce(u.state,m.state,'missing') NOT IN ('uploaded','applied')))",
            [object.id.to_string()], |r|r.get(0))?;
        if pending {
            return Ok(false);
        }
        if let Some(latest) = object.latest {
            let (sequence, node) = match self.get(latest) {
                Ok(record) if record.state == UploadState::Uploaded => {
                    (record.sequence, record.remote)
                }
                Ok(_) => return Ok(false),
                Err(JournalError::Missing) => {
                    let record = self.mutation(latest)?;
                    if record.state != MutationState::Applied {
                        return Ok(false);
                    }
                    let node = match record.receipt {
                        Some(cirrove_core::mutation::MutationReceipt::Upsert(node)) => Some(node),
                        _ => None,
                    };
                    (record.sequence, node)
                }
                Err(error) => return Err(error),
            };
            if node.as_ref() != object.remote.as_ref() || sequence != object.remote_sequence {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Commit the handoff after a fresh provider observation. A changed object,
    /// pending successor or dirty generation invalidates the candidate. Physical
    /// deletion is separate and retryable, including after process failure.
    pub fn handoff_namespace(
        &mut self,
        id: Uuid,
        revision: u64,
        remote: Node,
    ) -> Result<NamespaceObject> {
        let object = self.namespace_object(id)?;
        if object.revision != revision || !self.namespace_is_clean(&object)? {
            return Err(JournalError::Stale);
        }
        let working = object.working_file;
        let object = self.following_namespace(&object, remote)?;
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        // The account lease and caller's journal mutex serialize the validation
        // above with all local edits. The detach and cleanup intent commit together.
        if let Some(working) = working {
            tx.execute(
                "INSERT INTO retired_working VALUES(?1)",
                [working.to_string()],
            )?;
            if tx.execute(
                "DELETE FROM working_files WHERE id=?1",
                [working.to_string()],
            )? != 1
            {
                return Err(JournalError::Stale);
            }
        }
        super::namespace::save(&tx, &object)?;
        tx.commit()?;
        Ok(object)
    }

    pub(crate) fn handoff_candidates(
        &self,
        after: Option<Uuid>,
        limit: u32,
    ) -> Result<Vec<NamespaceObject>> {
        let mut query = self.db.prepare(
            "SELECT body FROM namespace_objects WHERE id>coalesce(?1,'')
            AND coalesce(json_extract(body,'$.follows_remote'),0)=0 ORDER BY id LIMIT ?2",
        )?;
        query
            .query_map(
                params![after.map(|id| id.to_string()), limit.min(32)],
                |r| r.get::<_, String>(0),
            )?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }

    /// Only explicit retirement records authorize deletion. Unknown spool files
    /// remain retained and quota-accounted. No cloud path is involved.
    pub fn collect_retired_working(&mut self, limit: u32) -> Result<usize> {
        let ids = {
            let mut query = self
                .db
                .prepare("SELECT id FROM retired_working ORDER BY id LIMIT ?1")?;
            query
                .query_map([limit.min(1000)], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut removed = 0;
        for id in ids {
            let id = Uuid::parse_str(&id).map_err(|_| JournalError::Corrupt)?;
            let active: bool = self.db.query_row(
                "SELECT EXISTS(SELECT 1 FROM working_files WHERE id=?1)",
                [id.to_string()],
                |r| r.get(0),
            )?;
            if active {
                return Err(JournalError::Corrupt);
            }
            let path = self.working.join(id.to_string());
            match OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&path)
            {
                Ok(file) => {
                    owned_private(&file)?;
                    std::fs::remove_file(&path)?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            File::open(&self.working)?.sync_all()?;
            self.db
                .execute("DELETE FROM retired_working WHERE id=?1", [id.to_string()])?;
            removed += 1;
        }
        Ok(removed)
    }
}
