//! In-place edits to the exact canonical native export archive only.
use super::*;
use cirrove_core::reads::{ReadIdentity, ReadSession};

pub(in crate::filesystem) async fn read_snapshot(
    session: Arc<dyn ReadSession>,
    offset: u64,
    size: u32,
    cancel: &CancellationToken,
) -> std::result::Result<Vec<u8>, ProviderError> {
    if size > 8 * 1024 * 1024 {
        return Err(ProviderError::Protocol("native read exceeds bound"));
    }
    let end = offset
        .saturating_add(u64::from(size))
        .min(session.identity().size);
    let mut at = offset;
    let mut result = Vec::new();
    while at < end {
        let count = (end - at).min(64 * 1024) as u32;
        let bytes = session.read_range(at, count, cancel).await?;
        if bytes.len() != count as usize {
            return Err(ProviderError::VersionChanged);
        }
        result.extend_from_slice(&bytes);
        at += u64::from(count);
    }
    Ok(result)
}
async fn provider_call<T>(
    deadline: tokio::time::Instant,
    cancel: &CancellationToken,
    call: impl std::future::Future<Output = std::result::Result<T, ProviderError>>,
) -> Result<T> {
    tokio::select! { biased;
        _=cancel.cancelled()=>Err(Errno::ENODEV),
        result=tokio::time::timeout_at(deadline,call)=>result.map_err(|_|Errno::ETIMEDOUT)?.map_err(|e|errno(&e)),
    }
}
struct NativeRoute {
    source: View,
    ancestors: Vec<(Scope, Node)>,
}
impl Inner {
    fn native_route(&self, view: &View) -> Result<Option<NativeRoute>> {
        let views = self.views.lock().map_err(|_| Errno::EIO)?;
        let mut current = view.clone();
        let mut seen = std::collections::HashSet::new();
        let mut route = Vec::new();
        while current.inode != 1 {
            if !seen.insert(current.inode) || seen.len() > 128 {
                return Err(Errno::ELOOP);
            }
            route.push(current.clone());
            current = views.get(&current.parent).ok_or(Errno::ESTALE)?.clone();
        }
        if !route.iter().any(|v| v.package) {
            return Ok(None);
        }
        if route.len() < 2
            || route[0].package
            || !route[1].package
            || route[0].kind != NodeKind::File
            || route[1].kind != NodeKind::Folder
            || route.iter().any(|v| {
                v.reference || v.entry.is_some() || !v.alias.is_empty() || v.scope != view.scope
            })
            || route.iter().skip(2).any(|v| v.package)
        {
            return Err(Errno::EOPNOTSUPP);
        }
        let mut ancestors = Vec::new();
        for ancestor in route.iter().skip(2).rev() {
            ancestors.push((
                ancestor.scope.as_ref().clone(),
                ancestor
                    .node
                    .as_ref()
                    .ok_or(Errno::ESTALE)?
                    .as_ref()
                    .clone(),
            ));
        }
        Ok(Some(NativeRoute {
            source: route[1].clone(),
            ancestors,
        }))
    }
    pub(in crate::filesystem) async fn prepare_path_edit(
        &self,
        view: &View,
        pathname: Option<&Node>,
        truncate: Option<u64>,
    ) -> Result<WorkingFile> {
        let writer = self.writeback.as_ref().ok_or(Errno::EROFS)?;
        let Some(route) = self.native_route(view)? else {
            self.refuse_within_package(view)?;
            self.capture_ancestors(view).await?;
            return match truncate {
                Some(size) => {
                    writer
                        .truncate_path(&self.engine, view, pathname, size, &self.cancel)
                        .await
                }
                None => {
                    writer
                        .prepare(&self.engine, view, pathname, false, &self.cancel)
                        .await
                }
            };
        };
        writer.capture_ancestors(route.ancestors).await?;
        let source = self.node(&route.source).await.map_err(|e| errno(&e))?;
        let record = writer
            .prepare_native(
                &self.engine,
                view,
                &source,
                pathname,
                truncate,
                &self.cancel,
            )
            .await?;
        // The inode's visible route changes; clones already held by old open
        // readers retain their original node and immutable archive session.
        let owner = writer
            .node(
                &record.scope,
                record.node.parent_id.as_deref().ok_or(Errno::EIO)?,
            )?
            .ok_or(Errno::ESTALE)?;
        let mut source_view = route.source;
        let old_source = source_view.id.clone();
        source_view.remember(&owner, true);
        if let Some((_, id)) = Arc::make_mut(&mut source_view.ancestry).last_mut()
            && id.as_str() == old_source.as_ref()
        {
            *id = owner.id.clone();
        }
        let mut child = view.clone();
        child.remember(&record.node, true);
        let mut views = self.views.lock().map_err(|_| Errno::EIO)?;
        for (expected, replacement) in [
            (old_source.as_ref(), &source_view),
            (view.id.as_ref(), &child),
        ] {
            let current = views.get(&replacement.inode).ok_or(Errno::ESTALE)?;
            if current.scope != replacement.scope
                || (current.id.as_ref() != expected && current.id != replacement.id)
            {
                return Err(Errno::ESTALE);
            }
        }
        views.insert(source_view).map_err(|e| errno(&e))?;
        views.insert(child).map_err(|e| errno(&e))?;
        self.engine.changed.notify_waiters();
        Ok(record)
    }
}
impl Writeback {
    pub(in crate::filesystem) async fn native_open_file(
        &self,
        original: &OpenFile,
        record: WorkingFile,
        cancel: &CancellationToken,
    ) -> Result<Arc<OpenFile>> {
        if !record.native || record.scope != *original.view.scope {
            return Err(Errno::ESTALE);
        }
        let mut view = original.view.clone();
        view.remember(&record.node, true);
        let lease = self.lease(&view.scope, &view.id, cancel).await?;
        self.register_open(OpenFile {
            view,
            node: record.node,
            flags: original.flags,
            _lease: Some(lease),
            remote_reads: tokio_util::task::TaskTracker::new(),
            native_snapshot: std::sync::OnceLock::new(),
        })
    }
    async fn pin_native_readers(
        &self,
        scope: &Scope,
        node: &Node,
        session: Arc<dyn ReadSession>,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let identity = ReadIdentity::new(scope, node).map_err(|e| errno(&e))?;
        if session.identity() != &identity {
            return Err(Errno::ESTALE);
        }
        let readers = {
            let mut p = self.projection.lock().map_err(|_| Errno::EIO)?;
            p.native_readers.retain(|_, v| v.strong_count() > 0);
            p.native_readers
                .insert(identity.clone(), Arc::downgrade(&session));
            let users: Vec<_> = p
                .streams
                .get(&key(scope, &node.id))
                .into_iter()
                .flatten()
                .filter_map(Weak::upgrade)
                .collect();
            for file in &users {
                if ReadIdentity::new(&file.view.scope, &file.node)
                    .ok()
                    .as_ref()
                    != Some(&identity)
                {
                    return Err(Errno::ESTALE);
                }
                if let Some(old) = file.native_snapshot.get() {
                    if old.identity() != session.identity() {
                        return Err(Errno::ESTALE);
                    }
                } else {
                    let _ = file.native_snapshot.set(session.clone());
                }
                file.remote_reads.close();
            }
            users
        };
        for file in readers {
            tokio::select! { biased;
                _=cancel.cancelled()=>return Err(Errno::ENODEV),
                _=file.remote_reads.wait()=>{},
            }
        }
        Ok(())
    }
    async fn prepare_native(
        &self,
        engine: &Engine,
        view: &View,
        source: &Node,
        pathname: Option<&Node>,
        truncate: Option<u64>,
        cancel: &CancellationToken,
    ) -> Result<WorkingFile> {
        if engine.account.access != cirrove_auth::AccessMode::ReadWrite
            || engine.scope(&view.scope.collection) != *view.scope
        {
            return Err(Errno::EACCES);
        }
        if cancel.is_cancelled() || engine.cancel.is_cancelled() {
            return Err(Errno::ENODEV);
        }
        let gate = {
            let mut gates = self.hydrating.lock().map_err(|_| Errno::EIO)?;
            gates.retain(|_, v| v.strong_count() > 0);
            let identity = key(&view.scope, &source.id);
            if let Some(gate) = gates.get(&identity).and_then(Weak::upgrade) {
                gate
            } else {
                let gate = Arc::new(tokio::sync::Mutex::new(()));
                gates.insert(identity, Arc::downgrade(&gate));
                gate
            }
        };
        let token = cancel.child_token();
        let _abandoned = token.clone().drop_guard();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(360);
        let _gate = tokio::select! { biased;
            _=token.cancelled()=>return Err(Errno::ENODEV),
            v=tokio::time::timeout_at(deadline,gate.lock())=>v.map_err(|_|Errno::ETIMEDOUT)?,
        };
        let (scope, selected_source, selected_archive, pathname) = (
            view.scope.as_ref().clone(),
            source.clone(),
            view.node.as_ref().ok_or(Errno::EINVAL)?.as_ref().clone(),
            pathname.cloned(),
        );
        let selected = self
            .local(move |j| {
                j.select_native_edit(
                    &scope,
                    &selected_source,
                    &selected_archive,
                    pathname.as_ref(),
                )
            })
            .await?;
        if let Some(id) = selected.working {
            let cancel = token.clone();
            let record = self
                .local(move |j| {
                    selected.recheck(j)?;
                    if cancel.is_cancelled() {
                        return Err(JournalError::Stale);
                    }
                    match truncate {
                        Some(size) => j.truncate_working(id, size),
                        None => j.working_file(id),
                    }
                })
                .await?;
            return self.publish(record).await;
        }
        let permit = tokio::select! { biased;
            _=token.cancelled()=>return Err(Errno::ENODEV),
            p=tokio::time::timeout_at(deadline,self.sealing.permit())=>p.map_err(|_|Errno::ETIMEDOUT)??,
        };
        let binding = provider_call(
            deadline,
            &token,
            engine
                .provider
                .resolve_native_archive(&selected.scope, &selected.archive, &token),
        )
        .await?
        .ok_or(Errno::EOPNOTSUPP)?;
        if binding.scope != selected.scope
            || binding.archive != selected.archive
            || binding.source != selected.source
        {
            return Err(Errno::ESTALE);
        }
        let session = provider_call(
            deadline,
            &token,
            engine
                .provider
                .staged_content_session(&selected.scope, &selected.archive, &token),
        )
        .await?
        .ok_or(Errno::EOPNOTSUPP)?;
        if session.identity()
            != &ReadIdentity::new(&selected.scope, &selected.archive).map_err(|e| errno(&e))?
        {
            return Err(Errno::ESTALE);
        }
        tokio::time::timeout_at(
            deadline,
            self.pin_native_readers(&selected.scope, &selected.archive, session.clone(), &token),
        )
        .await
        .map_err(|_| Errno::ETIMEDOUT)??;
        let check = selected.clone();
        let reserve_token = token.clone();
        let (mut hydration, mut permit) = self
            .local(move |j| {
                check.recheck(j)?;
                if reserve_token.is_cancelled() || tokio::time::Instant::now() >= deadline {
                    return Err(JournalError::Stale);
                }
                Ok((j.reserve_native_working(binding)?, permit))
            })
            .await?;
        let mut offset = 0;
        while offset < selected.archive.size {
            let count = (selected.archive.size - offset).min(64 * 1024) as u32;
            let bytes =
                provider_call(deadline, &token, session.read_range(offset, count, &token)).await?;
            if bytes.len() != count as usize {
                return Err(Errno::EIO);
            }
            let task = tokio::task::spawn_blocking(move || {
                hydration.write_chunk(&bytes)?;
                Ok::<_, JournalError>((hydration, permit))
            });
            let result = tokio::select! { biased;
                _=token.cancelled()=>return Err(Errno::ENODEV),
                result=tokio::time::timeout_at(deadline,task)=>result.map_err(|_|Errno::ETIMEDOUT)?.map_err(|_|Errno::EIO)?.map_err(error)?,
            };
            (hydration, permit) = result;
            offset += u64::from(count);
        }
        let validate_token = token.clone();
        let task = tokio::task::spawn_blocking(move || {
            Ok::<_, JournalError>((hydration.validate(&validate_token)?, permit))
        });
        let (ready, permit) = tokio::select! { biased;
            _=token.cancelled()=>return Err(Errno::ENODEV),
            result=tokio::time::timeout_at(deadline,task)=>result.map_err(|_|Errno::ETIMEDOUT)?.map_err(|_|Errno::EIO)?.map_err(error)?,
        };
        let record = self
            .local(move |j| {
                let _permit = permit;
                selected.recheck(j)?;
                if token.is_cancelled() || tokio::time::Instant::now() >= deadline {
                    return Err(JournalError::Stale);
                }
                let file = j.publish_native_working(ready)?;
                match truncate {
                    Some(size) => j.truncate_working(file.id, size),
                    None => Ok(file),
                }
            })
            .await?;
        self.publish(record).await
    }
}
#[cfg(test)]
mod tests;
