//! Short publication locks bind opens and in-flight reads to retained streams.
use super::*;
use tokio_util::task::task_tracker::TaskTrackerToken;

pub(crate) enum ReadSource {
    Working(Uuid),
    Remote(Node, TaskTrackerToken),
}
impl Writeback {
    pub fn is_unlinked(&self, scope: &Scope, item: &str) -> Result<bool> {
        Ok(self
            .projection
            .lock()
            .map_err(|_| Errno::EIO)?
            .local_object(scope, item)
            .is_some_and(|o| o.unlinked))
    }
    /// Register before asynchronous OPEN preparation. Unlink can then preserve
    /// this stream even if OPEN has not returned its file handle yet.
    pub fn register_open(&self, file: OpenFile) -> Result<Arc<OpenFile>> {
        let mut p = self.projection.lock().map_err(|_| Errno::EIO)?;
        let object = p.local_object(&file.view.scope, &file.view.node.id);
        if object.is_some_and(|o| o.unlinked) {
            return Err(Errno::ENOENT);
        }
        let identity = key(
            &file.view.scope,
            object.map_or(&file.view.node.id, |o| &o.node.id),
        );
        p.streams.retain(|_, users| {
            users.retain(|u| u.strong_count() > 0);
            !users.is_empty()
        });
        let file = Arc::new(file);
        p.streams
            .entry(identity)
            .or_default()
            .push(Arc::downgrade(&file));
        Ok(file)
    }
    pub fn publish_open(&self, inner: &Inner, handle: u64, file: OpenFile) -> Result<()> {
        let file = self.register_open(file)?;
        inner
            .files
            .lock()
            .map_err(|_| Errno::EIO)?
            .insert(handle, file);
        Ok(())
    }
    pub fn read_source(&self, file: &OpenFile) -> Result<ReadSource> {
        let p = self.projection.lock().map_err(|_| Errno::EIO)?;
        let object = p.local_object(&file.view.scope, &file.view.node.id);
        if let Some(working) = object.and_then(|o| o.working_file) {
            return Ok(ReadSource::Working(working));
        }
        if file.remote_reads.is_closed() {
            return Err(Errno::ESTALE);
        }
        let mut node = file.node.clone();
        if let Some(remote) = object.and_then(|o| o.remote.as_ref()) {
            node.id = remote.id.clone();
        }
        // Registration and switching to working bytes use the same short lock.
        // The token is a counter, not a filesystem lock held during network I/O.
        Ok(ReadSource::Remote(node, file.remote_reads.token()))
    }
    pub async fn unlink(self: &Arc<Self>, inner: &Arc<Inner>, view: View) -> Result<()> {
        let _lease = self
            .lease(&view.scope, &view.node.id, &inner.cancel)
            .await?;
        let parent = view.node.parent_id.clone().ok_or(Errno::EINVAL)?;
        let name = view.node.name.clone();
        let writer = self.clone();
        let scope = view.scope.as_ref().clone();
        let source = view.node.as_ref().clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut j = writer.journal.lock().map_err(|_| Errno::EIO)?;
            let mut object = Self::materialize(&mut j, scope, source).map_err(error)?;
            if object.unlinked {
                return Err(Errno::ENOENT);
            }
            if object.node.parent_id.as_ref() != Some(&parent) || object.node.name != name {
                return Err(Errno::ESTALE);
            }
            if let Some(id) = object.working_file {
                j.seal_working(id).map_err(error)?;
                object = j.namespace_object(object.id).map_err(error)?;
            }
            let working = object
                .working_file
                .map(|id| j.working_file(id))
                .transpose()
                .map_err(error)?;
            Self::publish_locked(&j, &writer.projection)?;
            let mut p = writer.projection.lock().map_err(|_| Errno::EIO)?;
            let users = p
                .streams
                .get(&key(&object.scope, &object.node.id))
                .into_iter()
                .flatten()
                .filter_map(Weak::upgrade)
                .collect::<Vec<_>>();
            let preserve = (!users.is_empty() && working.is_none())
                || users.iter().any(|u| !u.remote_reads.is_empty());
            let mut next = object.clone();
            next.unlinked = true;
            next.revision = next.revision.checked_add(1).ok_or(Errno::ENOSPC)?;
            let mut next_working = working;
            if let Some(file) = &mut next_working {
                file.unlinked = true;
            }
            if !p.validate_merge(&next, next_working.as_ref())? {
                return Err(Errno::ESTALE);
            }
            let committed = j
                .unlink_namespace_file(object.id, object.revision, preserve)
                .map_err(error)?;
            p.apply(committed.object, committed.working);
            Ok(())
        })
        .await
        .map_err(|_| Errno::EIO)??;
        self.wake.notify_waiters();
        inner.engine.changed.notify_waiters();
        Ok(())
    }
    /// Release an empty directory and queue its conditional remote removal.
    ///
    /// Simpler than `unlink` because a directory has no working bytes and no open
    /// streams to preserve: there is nothing a reader could still be holding. The
    /// caller has already refused a non-empty directory from its own listing; the
    /// journal refuses one whose children exist only locally, and the adapter
    /// checks the provider's side immediately before deleting.
    pub async fn rmdir(self: &Arc<Self>, inner: &Arc<Inner>, view: View) -> Result<()> {
        let _lease = self
            .lease(&view.scope, &view.node.id, &inner.cancel)
            .await?;
        let parent = view.node.parent_id.clone().ok_or(Errno::EINVAL)?;
        let name = view.node.name.clone();
        let writer = self.clone();
        let scope = view.scope.as_ref().clone();
        let source = view.node.as_ref().clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut j = writer.journal.lock().map_err(|_| Errno::EIO)?;
            let object = Self::materialize(&mut j, scope, source).map_err(error)?;
            if object.unlinked {
                return Err(Errno::ENOENT);
            }
            if object.node.kind != NodeKind::Folder {
                return Err(Errno::ENOTDIR);
            }
            if object.node.parent_id.as_ref() != Some(&parent) || object.node.name != name {
                return Err(Errno::ESTALE);
            }
            // A directory whose creation the provider has not acknowledged has no
            // ETag of its own yet.
            //
            // The journal would accept this: the removal chains behind its own
            // creation and is rebound to that receipt before it is sent. What it
            // would not do is cancel anything -- the folder is created in the
            // cloud, syncs out to every other device, and then goes to the
            // recycle bin, for a folder the user made and unmade in one breath.
            // Cancelling an in-flight creation is the operation that avoids that
            // round trip and it is not implemented, so this waits instead.
            //
            // `EBUSY` is the refusal because it is the one true thing to say:
            // not now, try again. It used to be reached as `EINVAL` through the
            // journal, which told the caller its request was malformed.
            if object.remote.is_none() {
                return Err(Errno::EBUSY);
            }
            Self::publish_locked(&j, &writer.projection)?;
            let mut p = writer.projection.lock().map_err(|_| Errno::EIO)?;
            let mut next = object.clone();
            next.unlinked = true;
            next.revision = next.revision.checked_add(1).ok_or(Errno::ENOSPC)?;
            if !p.validate_merge(&next, None)? {
                return Err(Errno::ESTALE);
            }
            let committed = j
                .remove_namespace_directory(object.id, object.revision)
                .map_err(error)?;
            p.apply(committed.object, None);
            Ok(())
        })
        .await
        .map_err(|_| Errno::EIO)??;
        self.wake.notify_waiters();
        inner.engine.changed.notify_waiters();
        Ok(())
    }
    /// Runs after the local namespace call, outside VFS directory locks. Both
    /// replacement streams and unlinked victims keep their original bytes.
    pub async fn preserve_unlinked(self: &Arc<Self>, engine: &Engine) -> Result<bool> {
        let after = *self.preserving_cursor.lock().map_err(|_| Errno::EIO)?;
        let records = self
            .local(move |j| {
                let mut records = Vec::new();
                for r in j.unlinked_readers(after, 16)? {
                    let object = j
                        .namespace_for_operation(r.id)?
                        .ok_or(JournalError::Corrupt)?;
                    records.push((r.sequence, r.id, vec![object.id], false));
                }
                for (sequence, r) in j.replacement_readers(after, 16)? {
                    records.push((sequence, r.id, vec![r.source, r.victim], true));
                }
                records.sort_by_key(|r| r.0);
                records.truncate(16);
                Ok(records)
            })
            .await?;
        if records.is_empty() {
            *self.preserving_cursor.lock().map_err(|_| Errno::EIO)? = 0;
            return Ok(false);
        }
        self.refresh_projection().await?;
        for (sequence, id, objects, replacement) in records {
            *self.preserving_cursor.lock().map_err(|_| Errno::EIO)? = sequence;
            if self
                .maintenance_retries
                .lock()
                .map_err(|_| Errno::EIO)?
                .get(&id)
                .is_some_and(|(_, at)| *at > tokio::time::Instant::now())
            {
                continue;
            }
            let result = async {
                for (index, object) in objects.into_iter().enumerate() {
                    if !self
                        .preserve_stream(engine, object, replacement && index == 0)
                        .await?
                    {
                        return Ok(false);
                    }
                }
                self.local(move |j| {
                    if replacement {
                        j.release_replacement_readers(id)
                    } else {
                        j.release_unlinked_readers(id)
                    }
                })
                .await?;
                self.wake.notify_waiters();
                engine.changed.notify_waiters();
                Ok(true)
            }
            .await;
            match result {
                Ok(true) => {
                    self.maintenance_retries
                        .lock()
                        .map_err(|_| Errno::EIO)?
                        .remove(&id);
                    return Ok(true);
                }
                Ok(false) => continue,
                Err(error) => {
                    let mut retries = self.maintenance_retries.lock().map_err(|_| Errno::EIO)?;
                    let n = retries
                        .get(&id)
                        .map_or(1, |(n, _)| n.saturating_add(1).min(6));
                    retries.insert(
                        id,
                        (
                            n,
                            tokio::time::Instant::now() + Duration::from_secs((1u64 << n).min(60)),
                        ),
                    );
                    return Err(error);
                }
            }
        }
        Ok(false)
    }

    async fn preserve_stream(
        &self,
        engine: &Engine,
        object_id: Uuid,
        require_working: bool,
    ) -> Result<bool> {
        let (object, working, users) = {
            let p = self.projection.lock().map_err(|_| Errno::EIO)?;
            let object = p.objects.get(&object_id).ok_or(Errno::EIO)?.clone();
            let working = object.working_file;
            let users = p
                .streams
                .get(&key(&object.scope, &object.node.id))
                .into_iter()
                .flatten()
                .filter_map(Weak::upgrade)
                .collect::<Vec<_>>();
            (object, working, users)
        };
        // A replacement source remains openable at its new path. Preparation
        // must switch ALL future reads to local bytes before this gate opens.
        if require_working && working.is_none() {
            return Ok(false);
        }
        if !users.is_empty() && working.is_none() {
            let remote = object.remote.as_ref().ok_or(Errno::EIO)?;
            if users.iter().any(|u| {
                u.node.size != remote.size || u.node.content_revision() != remote.content_revision()
            }) {
                return Err(Errno::ESTALE);
            }
            let mut view = users[0].view.clone();
            view.node = object.node.clone().into();
            drop(users);
            self.prepare(engine, &view, false, &engine.cancel).await?;
        }
        let users = {
            let p = self.projection.lock().map_err(|_| Errno::EIO)?;
            let users = p
                .streams
                .get(&key(&object.scope, &object.node.id))
                .into_iter()
                .flatten()
                .filter_map(Weak::upgrade)
                .collect::<Vec<_>>();
            if !users.is_empty()
                && p.local_object(&object.scope, &object.node.id)
                    .and_then(|o| o.working_file)
                    .is_none()
            {
                return Err(Errno::EAGAIN);
            }
            for file in &users {
                file.remote_reads.close();
            }
            users
        };
        tokio::select! {biased;
            _=engine.cancel.cancelled()=>return Err(Errno::ENODEV),
            result=tokio::time::timeout(Duration::from_secs(30),async{for file in users{file.remote_reads.wait().await;}})=>{
                result.map_err(|_|Errno::ETIMEDOUT)?;
            }
        }
        Ok(true)
    }
}
