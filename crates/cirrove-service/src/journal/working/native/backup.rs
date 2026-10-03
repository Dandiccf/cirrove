//! Local backup-first gap. Only promotion queues a typed native replacement.
//! The provider identity/name and immutable operation provenance stay untouched.
use super::*;
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Gap {
    owner: Uuid,
    working: Uuid,
    scope: Scope,
    canonical: String,
    backup: String,
}
pub(super) fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS native_backup_gaps(working TEXT PRIMARY KEY,owner TEXT NOT NULL UNIQUE,body TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS native_backup_streams(working TEXT PRIMARY KEY,owner TEXT NOT NULL,body TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS native_backup_owner ON native_backup_streams(owner);")?;
    Ok(())
}
pub(super) fn gap(db: &Connection, working: Uuid) -> Result<Option<Gap>> {
    let value: Option<(String, String)> = db
        .query_row(
            "SELECT owner,body FROM native_backup_gaps WHERE working=?1",
            [working.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    value
        .map(|(owner, body)| {
            let g: Gap = serde_json::from_str(&body)?;
            if g.working != working || g.owner.to_string() != owner {
                return Err(JournalError::Corrupt);
            }
            Ok(g)
        })
        .transpose()
}
pub(super) fn validate_gap(
    db: &Connection,
    child: &NamespaceObject,
    owner: &NamespaceObject,
) -> Result<()> {
    let role = child.native_archive.as_ref().ok_or(JournalError::Stale)?;
    match (role.backed_up, gap(db, child.id)?) {
        (false, None) => Ok(()),
        (true, Some(g))
            if !role.retired
                && g.working == child.id
                && g.owner == owner.id
                && g.scope == child.scope
                && g.scope == owner.scope
                && g.canonical == owner.node.name
                && g.backup == child.node.name
                && g.backup != g.canonical =>
        {
            Ok(())
        }
        _ => Err(JournalError::Stale),
    }
}
/// A visible backup has no Head, provider ownership, native archive role or
/// namespace operation. Its old bytes and immutable Binding remain recoverable.
pub(crate) fn validate_local_backup(db: &Connection, object: &NamespaceObject) -> Result<bool> {
    let body: Option<String> = db
        .query_row(
            "SELECT body FROM native_backup_streams WHERE working=?1",
            [object.id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    let Some(body) = body else {
        return Ok(false);
    };
    let g: Gap = serde_json::from_str(&body)?;
    let (owner, bound): (String, String) = db.query_row(
        "SELECT owner,binding FROM native_detached_streams WHERE working=?1",
        [object.id.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let owner_node = namespace::by_id(db, g.owner)?;
    let binding: Binding = serde_json::from_str(&bound)?;
    binding.validate()?;
    let working: String = db.query_row(
        "SELECT body FROM working_files WHERE id=?1",
        [object.id.to_string()],
        |r| r.get(0),
    )?;
    let working: WorkingFile = serde_json::from_str(&working)?;
    let active: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM native_working_heads WHERE working=?1) OR EXISTS(SELECT 1 FROM native_working_bindings WHERE working=?1) OR EXISTS(SELECT 1 FROM native_temporary_streams WHERE working=?1) OR EXISTS(SELECT 1 FROM native_backup_gaps WHERE working=?1) OR EXISTS(SELECT 1 FROM namespace_operations WHERE object=?1)", [object.id.to_string()], |r| r.get(0))?;
    if active
        || g.working != object.id
        || g.owner.to_string() != owner
        || g.scope != object.scope
        || binding.scope != g.scope
        || owner_node.scope != g.scope
        || object.native_archive.is_some()
        || object.remote_owned
        || object.remote.is_some()
        || object.latest.is_some()
        || object.follows_remote
        || object.node.name != g.backup
        || g.backup == g.canonical
        || object.node.parent_id.as_ref() != Some(&owner_node.node.id)
        || object.working_file != Some(object.id)
        || working.id != object.id
        || working.node != object.node
        || working.scope != object.scope
        || !working.native
        || working.unlinked != object.unlinked
        || object.node.kind != NodeKind::File
        || object.node.package
        || object.node.target.is_some()
    {
        return Err(JournalError::Stale);
    }
    Ok(true)
}
pub(super) fn detach(tx: &rusqlite::Transaction<'_>, g: &Gap) -> Result<()> {
    tx.execute(
        "INSERT INTO native_backup_streams VALUES(?1,?2,?3)",
        params![
            g.working.to_string(),
            g.owner.to_string(),
            serde_json::to_string(g)?
        ],
    )?;
    if tx.execute(
        "DELETE FROM native_backup_gaps WHERE working=?1",
        [g.working.to_string()],
    )? != 1
    {
        return Err(JournalError::Stale);
    }
    Ok(())
}
impl UploadJournal {
    /// Caller excludes source/child rename activity. This is a local namespace
    /// transition only; no remote Relocate/Remove or new upload is admitted.
    pub fn backup_native_canonical(
        &mut self,
        id: Uuid,
        revision: u64,
        name: String,
    ) -> Result<WorkingFile> {
        let mut child = self.namespace_object(id)?;
        projection::validate_child(&self.db, &child)?;
        let role = child.native_archive.as_ref().ok_or(JournalError::Stale)?;
        if role.retired
            || role.backed_up
            || child.unlinked
            || child.revision != revision
            || name == child.node.name
        {
            return Err(JournalError::Stale);
        }
        let owner = self.namespace_object(role.source_owner)?;
        UploadIntent::Create {
            parent: owner.node.id.clone(),
            name: name.clone(),
        }
        .validate()
        .map_err(|_| JournalError::Intent)?;
        if namespace::entry_slot(&self.db, &owner.scope, &owner.node.id, &name)?
            == namespace::entry_slot(&self.db, &owner.scope, &owner.node.id, &owner.node.name)?
        {
            return Err(JournalError::Intent);
        }
        let mut file = self.working_file(id)?;
        self.working_descriptor(id, false)?.sync_all()?;
        let g = Gap {
            owner: owner.id,
            working: id,
            scope: file.scope.clone(),
            canonical: owner.node.name,
            backup: name.clone(),
        };
        file.node.name = name;
        file.generation = file.generation.checked_add(1).ok_or(JournalError::Quota)?;
        child.node = file.node.clone();
        child
            .native_archive
            .as_mut()
            .ok_or(JournalError::Stale)?
            .backed_up = true;
        child.revision = child.revision.checked_add(1).ok_or(JournalError::Quota)?;
        let tx = self.db.transaction()?;
        tx.execute(
            "INSERT INTO native_backup_gaps VALUES(?1,?2,?3)",
            params![
                id.to_string(),
                g.owner.to_string(),
                serde_json::to_string(&g)?
            ],
        )?;
        tx.execute(
            "UPDATE working_files SET body=?2 WHERE id=?1",
            params![id.to_string(), serde_json::to_string(&file)?],
        )?;
        namespace::save(&tx, &child)?;
        tx.commit()?;
        Ok(file)
    }
    /// Restore only this exact local gap; an occupied canonical name, changed
    /// revision (including a predecessor acknowledgement), or marker mismatch
    /// fails atomically. Accepted bytes and cloud state are never rolled back.
    pub fn rollback_native_backup(&mut self, id: Uuid, revision: u64) -> Result<WorkingFile> {
        let mut child = self.namespace_object(id)?;
        projection::validate_child(&self.db, &child)?;
        let g = gap(&self.db, id)?.ok_or(JournalError::Stale)?;
        if child.revision != revision || child.unlinked {
            return Err(JournalError::Stale);
        }
        let mut file = self.working_file(id)?;
        file.node.name = g.canonical;
        file.generation = file.generation.checked_add(1).ok_or(JournalError::Quota)?;
        child.node = file.node.clone();
        child
            .native_archive
            .as_mut()
            .ok_or(JournalError::Stale)?
            .backed_up = false;
        child.revision = child.revision.checked_add(1).ok_or(JournalError::Quota)?;
        let tx = self.db.transaction()?;
        tx.execute(
            "DELETE FROM native_backup_gaps WHERE working=?1",
            [id.to_string()],
        )?;
        tx.execute(
            "UPDATE working_files SET body=?2 WHERE id=?1",
            params![id.to_string(), serde_json::to_string(&file)?],
        )?;
        namespace::save(&tx, &child)?;
        tx.commit()?;
        Ok(file)
    }
}
#[cfg(test)]
mod tests;

impl UploadJournal {
    pub(crate) fn native_backup_selection(
        &self,
        scope: &Scope,
        parent: &str,
    ) -> Result<Option<(WorkingFile, u64)>> {
        let owner = namespace::by_local(&self.db, scope, parent)?
            .or(namespace::by_remote(&self.db, scope, parent)?);
        let Some(owner) = owner else { return Ok(None) };
        let id: Option<String> = self
            .db
            .query_row(
                "SELECT working FROM native_backup_gaps WHERE owner=?1",
                [owner.id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        let Some(id) = id else { return Ok(None) };
        let id = Uuid::parse_str(&id).map_err(|_| JournalError::Corrupt)?;
        let child = self.namespace_object(id)?;
        projection::validate_child(&self.db, &child)?;
        if child.scope != *scope || child.unlinked {
            return Err(JournalError::Stale);
        };
        Ok(Some((self.working_file(id)?, child.revision)))
    }
    pub(crate) fn unlink_native_backup(
        &mut self,
        id: Uuid,
        scope: &Scope,
        selected: &Node,
    ) -> Result<WorkingFile> {
        let mut child = self.namespace_object(id)?;
        if !validate_local_backup(&self.db, &child)?
            || child.unlinked
            || child.scope != *scope
            || child.node.id != selected.id
            || child.node.parent_id != selected.parent_id
            || child.node.name != selected.name
        {
            return Err(JournalError::Stale);
        };
        let mut file = self.working_file(id)?;
        file.unlinked = true;
        file.generation = file.generation.checked_add(1).ok_or(JournalError::Quota)?;
        child.unlinked = true;
        child.revision = child.revision.checked_add(1).ok_or(JournalError::Quota)?;
        let tx = self.db.transaction()?;
        tx.execute(
            "UPDATE working_files SET slot=NULL,body=?2 WHERE id=?1",
            params![id.to_string(), serde_json::to_string(&file)?],
        )?;
        namespace::save(&tx, &child)?;
        tx.commit()?;
        Ok(file)
    }
}

impl UploadJournal {
    pub(crate) fn native_local_writable(&self, id: Uuid) -> Result<bool> {
        let object = self.namespace_object(id)?;
        if object.unlinked {
            return Ok(false);
        };
        if let Some(role) = self.native_local_stream(id)? {
            return Ok(!role.detached || role.backup);
        };
        if gap(&self.db, id)?.is_some() {
            projection::validate_child(&self.db, &object)?;
            return Ok(true);
        };
        Ok(false)
    }
}
