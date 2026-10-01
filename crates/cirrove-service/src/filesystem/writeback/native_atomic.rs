//! Narrow local temporary save path for an already authorized canonical archive.
use super::*;

impl Inner {
    pub(in crate::filesystem) async fn create_native_temporary(
        &self,
        parent: &View,
        children: &[Node],
        name: String,
    ) -> Result<Option<WorkingFile>> {
        if !parent.package {
            return Ok(None);
        }
        let source = self.node(parent).await.map_err(|e| errno(&e))?;
        let canonical = children
            .iter()
            .find(|n| {
                n.name == source.name
                    && n.kind == NodeKind::File
                    && !n.package
                    && n.target.is_none()
            })
            .cloned();
        let writer = self.writeback.as_ref().ok_or(Errno::EROFS)?;
        if self.engine.account.access != cirrove_auth::AccessMode::ReadWrite
            || self.engine.scope(&parent.scope.collection) != *parent.scope
        {
            return Err(Errno::EACCES);
        }
        let canonical = if let Some(canonical) = canonical {
            let view = self
                .insert(parent, canonical.clone())
                .await
                .map_err(|e| errno(&e))?;
            self.prepare_path_edit(&view, Some(&canonical), None)
                .await?
        } else {
            let scope = parent.scope.as_ref().clone();
            let id = parent.id.to_string();
            writer
                .local(move |j| j.native_backup_selection(&scope, &id))
                .await?
                .ok_or(Errno::EOPNOTSUPP)?
                .0
        };
        let checked = self
            .insert(parent, canonical.node.clone())
            .await
            .map_err(|e| errno(&e))?;
        self.validate_native_local_route(&checked)?;
        let cancel = self.cancel.clone();
        let expected = canonical.clone();
        let file = writer
            .local(move |j| {
                if cancel.is_cancelled() {
                    return Err(JournalError::Stale);
                }
                let current = j.working_file(expected.id)?;
                if current.scope != expected.scope
                    || current.node != expected.node
                    || current.generation != expected.generation
                    || current.unlinked
                {
                    return Err(JournalError::Stale);
                }
                j.create_native_temporary(current.id, name)
            })
            .await?;
        Ok(Some(writer.publish(file).await?))
    }
    /// Local temporary lifecycle only: never dispatch a namespace mutation.
    pub(in crate::filesystem) async fn change_native_temporary(
        &self,
        parent: &View,
        source: Node,
        name: Option<String>,
    ) -> Result<()> {
        if !parent.package || parent.kind != NodeKind::Folder {
            return Err(Errno::EOPNOTSUPP);
        }
        let writer = self.writeback.as_ref().ok_or(Errno::EROFS)?;
        if self.engine.account.access != cirrove_auth::AccessMode::ReadWrite
            || self.engine.scope(&parent.scope.collection) != *parent.scope
        {
            return Err(Errno::EACCES);
        }
        let selected = {
            let projection = writer.projection.lock().map_err(|_| Errno::EIO)?;
            let object = projection
                .local_object(&parent.scope, &source.id)
                .ok_or(Errno::EOPNOTSUPP)?;
            if projection
                .native_local
                .get(&object.id)
                .is_none_or(|role| role.detached)
                || object.unlinked
            {
                return Err(Errno::EOPNOTSUPP);
            }
            object.id
        };
        let scope = parent.scope.as_ref().clone();
        let cancel = self.cancel.clone();
        let account_cancel = self.engine.cancel.clone();
        let file = writer
            .local(move |journal| {
                if cancel.is_cancelled() || account_cancel.is_cancelled() {
                    return Err(JournalError::Stale);
                }
                match name {
                    Some(name) => {
                        journal.rename_native_local_temporary(selected, &scope, &source, name)
                    }
                    None => journal.unlink_native_local_temporary(selected, &scope, &source),
                }
            })
            .await?;
        let file = writer.publish(file).await?;
        if !file.unlinked {
            self.insert(parent, file.node)
                .await
                .map_err(|e| errno(&e))?;
        }
        self.engine.changed.notify_waiters();
        Ok(())
    }
    pub(in crate::filesystem) async fn rename_native_temporary(
        &self,
        parent: &View,
        destination: &View,
        source: Node,
        victim: Node,
    ) -> Result<()> {
        if !parent.package
            || !destination.package
            || parent.inode != destination.inode
            || parent.scope != destination.scope
        {
            return Err(Errno::EOPNOTSUPP);
        }
        let view = self
            .insert(parent, victim.clone())
            .await
            .map_err(|e| errno(&e))?;
        // Only the current canonical archive can be the victim. A preview or
        // foreign generated entry fails this existing exact binding admission.
        let canonical = self.prepare_path_edit(&view, Some(&victim), None).await?;
        let writer = self.writeback.as_ref().ok_or(Errno::EROFS)?;
        let moved = writer
            .replace_native_temporary(&self.engine, &parent.scope, source, canonical, &self.cancel)
            .await?;
        let current_parent = self.view(parent.inode).map_err(|e| errno(&e))?;
        self.insert(&current_parent, moved)
            .await
            .map_err(|e| errno(&e))?;
        self.engine.changed.notify_waiters();
        Ok(())
    }
}
impl Writeback {
    pub(super) async fn prepare_native_local(
        &self,
        engine: &Engine,
        view: &View,
        pathname: Option<&Node>,
        truncate: Option<u64>,
        cancel: &CancellationToken,
    ) -> Result<Option<WorkingFile>> {
        let selected = {
            let p = self.projection.lock().map_err(|_| Errno::EIO)?;
            let Some(object) = p.local_object(&view.scope, &view.id) else {
                return Ok(None);
            };
            let gap = object.native_archive.as_ref().is_some_and(|r| r.backed_up);
            let role = p.native_local.get(&object.id);
            if !gap && role.is_none() {
                return Ok(None);
            }
            if (!gap && role.is_some_and(|r| r.detached && !r.backup)) || object.unlinked {
                return Err(Errno::ESTALE);
            }
            object.clone()
        };
        if engine.account.access != cirrove_auth::AccessMode::ReadWrite
            || engine.scope(&view.scope.collection) != *view.scope
        {
            return Err(Errno::EACCES);
        }
        if cancel.is_cancelled() || engine.cancel.is_cancelled() {
            return Err(Errno::ENODEV);
        }
        let path = pathname.cloned();
        let cancel = cancel.clone();
        let file = self
            .local(move |j| {
                if cancel.is_cancelled() {
                    return Err(JournalError::Stale);
                }
                let actual = j.namespace_object(selected.id)?;
                if !j.native_local_writable(selected.id)?
                    || actual.scope != selected.scope
                    || actual.node.id != selected.node.id
                    || actual.node.parent_id != selected.node.parent_id
                    || actual.node.name != selected.node.name
                    || actual.unlinked
                    || path.as_ref().is_some_and(|p| {
                        p.id != actual.node.id
                            || p.parent_id != actual.node.parent_id
                            || p.name != actual.node.name
                    })
                {
                    return Err(JournalError::Stale);
                }
                match truncate {
                    Some(size) => j.truncate_working(actual.id, size),
                    None => j.working_file(actual.id),
                }
            })
            .await?;
        Ok(Some(self.publish(file).await?))
    }
    pub(super) async fn replace_native_temporary(
        &self,
        engine: &Engine,
        scope: &Scope,
        source: Node,
        canonical: WorkingFile,
        cancel: &CancellationToken,
    ) -> Result<Node> {
        if engine.account.access != cirrove_auth::AccessMode::ReadWrite
            || engine.scope(&scope.collection) != *scope
            || *scope != canonical.scope
        {
            return Err(Errno::EACCES);
        }
        if cancel.is_cancelled() || engine.cancel.is_cancelled() {
            return Err(Errno::ENODEV);
        }
        let temporary = {
            let p = self.projection.lock().map_err(|_| Errno::EIO)?;
            let o = p.local_object(scope, &source.id).ok_or(Errno::ESTALE)?;
            if p.native_local.get(&o.id).is_none_or(|r| r.detached)
                || o.unlinked
                || o.node.parent_id != source.parent_id
                || o.node.name != source.name
            {
                return Err(Errno::EOPNOTSUPP);
            }
            o.id
        };
        let token = cancel.child_token();
        let _abandoned = token.clone().drop_guard();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(360);
        let mut ids = [temporary, canonical.id];
        ids.sort_unstable();
        let mut gates = Vec::new();
        for id in ids {
            let gate = self.sealing.working(id)?;
            gates.push(tokio::select!{biased;_=token.cancelled()=>return Err(Errno::ENODEV),r=tokio::time::timeout_at(deadline,gate.lock_owned())=>r.map_err(|_|Errno::ETIMEDOUT)?});
        }
        let permit = tokio::select! {biased;_=token.cancelled()=>return Err(Errno::ENODEV),r=tokio::time::timeout_at(deadline,self.sealing.permit())=>r.map_err(|_|Errno::ETIMEDOUT)??};
        let journal = self.journal.clone();
        let wake = self.wake.clone();
        let worker_token = token.clone();
        let task = tokio::task::spawn_blocking(move || {
            let _gates = gates;
            let _permit = permit;
            let capture = {
                let mut j = journal.lock().map_err(|_| JournalError::Storage)?;
                if worker_token.is_cancelled() {
                    return Err(JournalError::Stale);
                }
                let current = j.working_file(canonical.id)?;
                let temp = j.working_file(temporary)?;
                if current.scope != canonical.scope
                    || current.node != canonical.node
                    || current.generation != canonical.generation
                    || current.unlinked
                    || temp.node.id != source.id
                    || temp.node.parent_id != source.parent_id
                    || temp.node.name != source.name
                {
                    return Err(JournalError::Stale);
                }
                j.capture_native_temporary(temporary, canonical.id)?
            };
            let captured = capture.capture(&worker_token)?;
            let mut j = journal.lock().map_err(|_| JournalError::Storage)?;
            j.replace_native_temporary(captured, &worker_token)?;
            wake.notify_waiters();
            j.working_file(temporary)
        });
        let file = tokio::select! {biased;
            result=task=>result.map_err(|_|Errno::EIO)?.map_err(error)?,
            _=token.cancelled()=>return Err(Errno::ENODEV),
            _=tokio::time::sleep_until(deadline)=>{token.cancel();return Err(Errno::ETIMEDOUT);},
        };
        Ok(self.publish(file).await?.node)
    }
}
