//! Path changes for exact local temporary streams. Never creates provider work.
use super::*;

impl UploadJournal {
    /// Rename a selected local temporary stream within its existing native owner.
    /// The caller checks generated-child collisions before entering the journal.
    pub(crate) fn rename_native_local_temporary(
        &mut self,
        id: Uuid,
        scope: &Scope,
        selected: &Node,
        name: String,
    ) -> Result<WorkingFile> {
        self.change_native_temporary(id, scope, selected, Some(name))
    }

    /// Preserve unlinked bytes and UUID for held descriptors and local recovery.
    pub(crate) fn unlink_native_local_temporary(
        &mut self,
        id: Uuid,
        scope: &Scope,
        selected: &Node,
    ) -> Result<WorkingFile> {
        self.change_native_temporary(id, scope, selected, None)
    }

    fn change_native_temporary(
        &mut self,
        id: Uuid,
        scope: &Scope,
        selected: &Node,
        name: Option<String>,
    ) -> Result<WorkingFile> {
        let mut object = self.namespace_object(id)?;
        atomic::validate_temporary(&self.db, &object)?;
        let mut file = self.working_file(id)?;
        if scope.account != self.account
            || object.scope != *scope
            || object.unlinked
            || selected.id != object.node.id
            || selected.parent_id != object.node.parent_id
            || selected.name != object.node.name
        {
            return Err(JournalError::Stale);
        }
        if let Some(name) = name {
            UploadIntent::Create {
                parent: object.node.parent_id.clone().ok_or(JournalError::Corrupt)?,
                name: name.clone(),
            }
            .validate()
            .map_err(|_| JournalError::Intent)?;
            let role = self.native_local_stream(id)?.ok_or(JournalError::Stale)?;
            let owner = self.namespace_object(role.source_owner)?;
            // Canonical authority is never acquired by naming a temporary stream.
            if name.to_lowercase() == owner.node.name.to_lowercase() {
                return Err(JournalError::Intent);
            }
            file.node.name = name;
            object.node = file.node.clone();
        } else {
            file.unlinked = true;
            object.unlinked = true;
        }
        object.revision = object.revision.checked_add(1).ok_or(JournalError::Quota)?;
        let tx = self.db.transaction()?;
        tx.execute(
            "UPDATE working_files SET slot=?2,body=?3 WHERE id=?1",
            params![
                id.to_string(),
                slot(&tx, &file)?,
                serde_json::to_string(&file)?
            ],
        )?;
        // Validate the complete new pair even though generic save skips unlinked
        // objects. A rollback restores both records and the old namespace slot.
        atomic::validate_temporary(&tx, &object)?;
        namespace::save(&tx, &object)?;
        tx.commit()?;
        Ok(file)
    }
}
