//! Local replacement returns before any source or old-reader download.
use super::*;
use crate::journal::WorkingSource;

impl Writeback {
    pub async fn replace(
        self: &Arc<Self>,
        inner: &Arc<Inner>,
        scope: Scope,
        source: Node,
        victim: Node,
    ) -> Result<Node> {
        let _victim_lease = self.lease(&scope, &victim.id, &inner.cancel).await?;
        let writer = self.clone();
        let moved = tokio::task::spawn_blocking(move || {
            let mut j = writer.journal.lock().map_err(|_| Errno::EIO)?;
            let mut src =
                Self::materialize(&mut j, scope.clone(), source.clone()).map_err(error)?;
            let mut dst = Self::materialize(&mut j, scope, victim.clone()).map_err(error)?;
            if src.node.name != source.name
                || src.node.parent_id != source.parent_id
                || dst.node.name != victim.name
                || dst.node.parent_id != victim.parent_id
            {
                return Err(Errno::ESTALE);
            }
            for object in [&src, &dst] {
                if let Some(file) = object.working_file {
                    j.seal_working(file).map_err(error)?;
                }
            }
            src = j.namespace_object(src.id).map_err(error)?;
            dst = j.namespace_object(dst.id).map_err(error)?;
            Self::publish_locked(&j, &writer.projection)?;
            let mut p = writer.projection.lock().map_err(|_| Errno::EIO)?;
            let needs_readers = src.working_file.is_none()
                || [&src, &dst].into_iter().any(|object| {
                    let users = p
                        .streams
                        .get(&key(&object.scope, &object.node.id))
                        .into_iter()
                        .flatten()
                        .filter_map(Weak::upgrade);
                    users
                        .into_iter()
                        .any(|user| object.working_file.is_none() || !user.remote_reads.is_empty())
                });
            j.replace_namespace_file(src.id, src.revision, dst.id, dst.revision, needs_readers)
                .map_err(error)?;
            p.catch_up(&j)?;
            let node = p.objects.get(&src.id).ok_or(Errno::EIO)?.node.clone();
            Ok(node)
        })
        .await
        .map_err(|_| Errno::EIO)??;
        self.wake.notify_waiters();
        inner.engine.changed.notify_waiters();
        Ok(moved)
    }

    pub(super) async fn download_source(
        &self,
        engine: &Engine,
        scope: &Scope,
        node: &Node,
        cancel: &CancellationToken,
        mut source: WorkingSource,
    ) -> Result<WorkingSource> {
        let size = node.size;
        let mut offset = 0;
        while offset < size {
            let bytes = engine
                .cache
                .read(
                    engine.provider.as_ref(),
                    scope,
                    node,
                    offset,
                    (size - offset).min(u64::from(crate::content::BLOCK_SIZE)) as u32,
                    cancel,
                )
                .await
                .map_err(|e| errno(&e))?;
            if bytes.is_empty() {
                return Err(Errno::EIO);
            }
            offset += bytes.len() as u64;
            source = tokio::task::spawn_blocking(move || {
                source.write_chunk(&bytes)?;
                Ok::<_, JournalError>(source)
            })
            .await
            .map_err(|_| Errno::EIO)?
            .map_err(error)?;
        }
        Ok(source)
    }

    pub(super) async fn prepare_replacement_source(&self, engine: &Engine) -> Result<bool> {
        let Some((record, preparation)) = self.local(|j| j.claim_preparation()).await? else {
            return Ok(false);
        };
        let id = record.id;
        let attempt = record.attempt.ok_or(Errno::EIO)?;
        let result = async {
            if !self.local(move |j| j.resume_prepared(id, attempt)).await? {
                let remote = preparation.remote.as_ref().ok_or(Errno::EIO)?;
                let source = self
                    .local(move |j| j.reserve_preparation(id, attempt))
                    .await?;
                let source = self
                    .download_source(engine, &preparation.scope, remote, &engine.cancel, source)
                    .await?;
                self.local(move |j| j.complete_preparation(id, attempt, source))
                    .await?;
            }
            Ok(())
        }
        .await;
        if let Err(failure) = result {
            let conflict = failure == Errno::ESTALE || failure == Errno::ENOENT;
            let delay = if failure == Errno::ENODEV {
                Duration::ZERO
            } else {
                Duration::from_secs((1u64 << record.failed_attempts.min(5)).min(60))
            };
            self.local(move |j| j.defer_preparation(id, attempt, conflict, delay))
                .await?;
            return Err(failure);
        }
        // Preparation is already committed. Publication failure must not try
        // to defer the now-Pending operation with its expired capture attempt.
        self.refresh_projection().await?;
        self.wake.notify_waiters();
        engine.changed.notify_waiters();
        Ok(true)
    }
}
