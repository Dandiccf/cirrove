//! Explicit backup-first native save dispatch. Backup names never identify a
//! provider object; only the retained native owner/working markers authorize it.
use super::*;
impl Inner {
    pub(in crate::filesystem) async fn rename_native_backup(
        &self,
        parent: &View,
        source: Node,
        name: String,
    ) -> Result<bool> {
        if !parent.package || parent.kind != NodeKind::Folder {
            return Ok(false);
        };
        let selected_view = self
            .insert(parent, source.clone())
            .await
            .map_err(|e| errno(&e))?;
        self.validate_native_local_route(&selected_view)?;
        let writer = self.writeback.as_ref().ok_or(Errno::EROFS)?;
        if self.engine.account.access != cirrove_auth::AccessMode::ReadWrite
            || self.engine.scope(&parent.scope.collection) != *parent.scope
        {
            return Err(Errno::EACCES);
        };
        if self.cancel.is_cancelled() || self.engine.cancel.is_cancelled() {
            return Err(Errno::ENODEV);
        };
        let parent_node = self.node(parent).await.map_err(|e| errno(&e))?;
        let scope = parent.scope.as_ref().clone();
        let parent_id = parent.id.to_string();
        let gap = writer
            .local(move |j| j.native_backup_selection(&scope, &parent_id))
            .await?;
        if let Some((canonical, revision)) = gap {
            if name != parent_node.name {
                return if source.id == canonical.node.id {
                    Err(Errno::EOPNOTSUPP)
                } else {
                    Ok(false)
                };
            }
            let moved = if source.id == canonical.node.id {
                let selected = canonical.clone();
                let cancel = self.cancel.clone();
                let engine_cancel = self.engine.cancel.clone();
                writer
                    .local(move |j| {
                        if cancel.is_cancelled() || engine_cancel.is_cancelled() {
                            return Err(JournalError::Stale);
                        };
                        let actual = j.working_file(selected.id)?;
                        if actual.scope != selected.scope
                            || actual.node != selected.node
                            || actual.generation != selected.generation
                            || source.parent_id != actual.node.parent_id
                            || source.name != actual.node.name
                        {
                            return Err(JournalError::Stale);
                        };
                        j.rollback_native_backup(selected.id, revision)
                    })
                    .await?
            } else {
                let node = writer
                    .replace_native_temporary(
                        &self.engine,
                        &parent.scope,
                        source,
                        canonical,
                        &self.cancel,
                    )
                    .await?;
                let current = self.view(parent.inode).map_err(|e| errno(&e))?;
                self.insert(&current, node).await.map_err(|e| errno(&e))?;
                self.engine.changed.notify_waiters();
                return Ok(true);
            };
            let moved = writer.publish(moved).await?;
            self.insert(parent, moved.node)
                .await
                .map_err(|e| errno(&e))?;
            self.engine.changed.notify_waiters();
            return Ok(true);
        }
        if source.name != parent_node.name {
            return Ok(false);
        };
        let view = self
            .insert(parent, source.clone())
            .await
            .map_err(|e| errno(&e))?;
        // Hydrate/grant exact source first. A generated preview or another
        // package identity cannot create a backup gap via its filename.
        let canonical = self.prepare_path_edit(&view, Some(&source), None).await?;
        let cancel = self.cancel.clone();
        let engine_cancel = self.engine.cancel.clone();
        let moved = writer
            .local(move |j| {
                if cancel.is_cancelled() || engine_cancel.is_cancelled() {
                    return Err(JournalError::Stale);
                };
                let actual = j.working_file(canonical.id)?;
                if actual.scope != canonical.scope
                    || actual.node != canonical.node
                    || actual.generation != canonical.generation
                    || actual.unlinked
                {
                    return Err(JournalError::Stale);
                };
                let revision = j.namespace_object(actual.id)?.revision;
                j.backup_native_canonical(actual.id, revision, name)
            })
            .await?;
        let moved = writer.publish(moved).await?;
        let current = self.view(parent.inode).map_err(|e| errno(&e))?;
        self.insert(&current, moved.node)
            .await
            .map_err(|e| errno(&e))?;
        self.engine.changed.notify_waiters();
        Ok(true)
    }
    pub(in crate::filesystem) async fn unlink_native_backup(
        &self,
        parent: &View,
        source: Node,
    ) -> Result<bool> {
        let selected_view = self
            .insert(parent, source.clone())
            .await
            .map_err(|e| errno(&e))?;
        self.validate_native_local_route(&selected_view)?;
        let writer = self.writeback.as_ref().ok_or(Errno::EROFS)?;
        if !parent.package {
            return Ok(false);
        };
        if self.engine.account.access != cirrove_auth::AccessMode::ReadWrite
            || self.engine.scope(&parent.scope.collection) != *parent.scope
        {
            return Err(Errno::EACCES);
        };
        let id = {
            let p = writer.projection.lock().map_err(|_| Errno::EIO)?;
            let Some(o) = p.local_object(&parent.scope, &source.id) else {
                return Ok(false);
            };
            if !p.native_local.get(&o.id).is_some_and(|r| r.backup) {
                return Ok(false);
            };
            if o.unlinked {
                return Err(Errno::ESTALE);
            };
            o.id
        };
        let scope = parent.scope.as_ref().clone();
        let cancel = self.cancel.clone();
        let engine_cancel = self.engine.cancel.clone();
        let file = writer
            .local(move |j| {
                if cancel.is_cancelled() || engine_cancel.is_cancelled() {
                    return Err(JournalError::Stale);
                };
                j.unlink_native_backup(id, &scope, &source)
            })
            .await?;
        writer.publish(file).await?;
        self.engine.changed.notify_waiters();
        Ok(true)
    }
}
