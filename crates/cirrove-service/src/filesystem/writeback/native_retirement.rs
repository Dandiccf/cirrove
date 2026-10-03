//! Clean native retirement is distinct from ordinary namespace handoff.
//! All provider work precedes locks; every exclusion survives blocking commit.
use super::*;
use crate::journal::NativeRetirementCandidate;
use tokio::sync::{OwnedMutexGuard, OwnedRwLockWriteGuard};
struct Exclusion {
    _activity: Vec<OwnedRwLockWriteGuard<()>>,
    _sealing: OwnedMutexGuard<()>,
}
impl Projection {
    fn native_retirement_idle(&mut self, candidate: &NativeRetirementCandidate) -> bool {
        // A historical immutable reader can predate several two-ID handoffs.
        // Conservatively exclude all native snapshot/derived-artifact users in
        // this exact account+collection instead of guessing ancestry by name.
        self.native_readers
            .retain(|_, session| session.strong_count() > 0);
        if self.native_readers.iter().any(|(identity, session)| {
            identity.scope == candidate.scope && session.strong_count() > 0
        }) {
            return false;
        }
        self.streams.retain(|_, users| {
            users.retain(|user| user.strong_count() > 0);
            !users.is_empty()
        });
        !self
            .streams
            .values()
            .flatten()
            .filter_map(Weak::upgrade)
            .any(|file| {
                *file.view.scope == candidate.scope
                    && (file.view.id.as_ref() == candidate.owner_local
                        || file.view.id.as_ref() == candidate.child_local
                        || file.node.id.starts_with("icloud-artifact:")
                        || file.native_snapshot.get().is_some())
            })
    }
}
impl Writeback {
    fn native_retirement_exclude(
        &self,
        candidate: &NativeRetirementCandidate,
    ) -> Result<Option<Exclusion>> {
        let Some(sealing) = self.sealing.try_working(candidate.working)? else {
            return Ok(None);
        };
        let mut identities = vec![
            candidate.owner_local.clone(),
            candidate.child_local.clone(),
            candidate.original_archive.clone(),
            format!("icloud-artifact:{}", candidate.current.id),
        ];
        identities.sort();
        identities.dedup();
        let mut activity = Vec::with_capacity(identities.len());
        for identity in identities {
            let Ok(gate) = self
                .activity_gate(key(&candidate.scope, &identity))?
                .try_write_owned()
            else {
                return Ok(None);
            };
            activity.push(gate);
        }
        if !self
            .projection
            .lock()
            .map_err(|_| Errno::EIO)?
            .native_retirement_idle(candidate)
        {
            return Ok(None);
        }
        Ok(Some(Exclusion {
            _activity: activity,
            _sealing: sealing,
        }))
    }
    /// None means skipped without provider I/O; Some means this bounded pass
    /// spent its one fresh observation. Absence is never retirement evidence.
    pub(super) async fn maintain_native(
        self: &Arc<Self>,
        engine: &Engine,
        owner: Uuid,
    ) -> Result<Option<bool>> {
        if engine.cancel.is_cancelled() {
            return Ok(Some(false));
        }
        let Ok(permit) = self.native_retirement.clone().try_acquire_owned() else {
            return Ok(None);
        };
        if self
            .maintenance_retries
            .lock()
            .map_err(|_| Errno::EIO)?
            .get(&owner)
            .is_some_and(|(_, at)| *at > tokio::time::Instant::now())
        {
            return Ok(None);
        }
        let Some(candidate) = self
            .local(move |j| j.native_retirement_for_owner(owner))
            .await?
        else {
            return Ok(None);
        };
        if engine.account.access != cirrove_auth::AccessMode::ReadWrite
            || candidate.scope != engine.scope(&candidate.scope.collection)
        {
            return Err(Errno::EACCES);
        }
        let Some(idle) = self.native_retirement_exclude(&candidate)? else {
            return Ok(None);
        };
        drop(idle); // Neither applications nor sealing wait behind provider I/O.
        let cancel = engine.cancel.child_token();
        let _abandoned = cancel.clone().drop_guard();
        let observed = tokio::select! {biased;
            _=cancel.cancelled()=>return Ok(Some(false)),
            result=tokio::time::timeout(Duration::from_secs(30),engine.refresh_node(&candidate.scope,&candidate.current.id))=>result.map_err(|_|Errno::ETIMEDOUT).and_then(|v|v.map_err(|e|errno(&e))),
        };
        let observed = match observed {
            Ok(node) if node == candidate.current => node,
            result => {
                let mut retries = self.maintenance_retries.lock().map_err(|_| Errno::EIO)?;
                let failures = retries
                    .get(&owner)
                    .map_or(1, |(n, _)| n.saturating_add(1).min(6));
                retries.insert(
                    owner,
                    (
                        failures,
                        tokio::time::Instant::now()
                            + Duration::from_secs((1u64 << failures).min(60)),
                    ),
                );
                return match result {
                    Ok(_) => Ok(Some(true)),
                    Err(error) => Err(error),
                };
            }
        };
        let Some(exclusion) = self.native_retirement_exclude(&candidate)? else {
            return Ok(Some(true));
        };
        let writer = self.clone();
        let token = cancel.clone();
        let result = tokio::task::spawn_blocking(move || {
            let _exclusion = exclusion;
            let _permit = permit;
            if token.is_cancelled() {
                return Ok(false);
            }
            let mut journal = writer.journal.lock().map_err(|_| Errno::EIO)?;
            Self::publish_locked(&journal, &writer.projection)?;
            let mut projection = writer.projection.lock().map_err(|_| Errno::EIO)?;
            if token.is_cancelled() || !projection.native_retirement_idle(&candidate) {
                return Ok(false);
            }
            let preview = journal
                .native_retirement_preview(&candidate, &observed)
                .map_err(error)?;
            let prepared = projection.prepare_batch(preview)?.ok_or(Errno::ESTALE)?;
            if token.is_cancelled() {
                return Ok(false);
            }
            journal
                .retire_native_working(&candidate, &observed)
                .map_err(error)?;
            projection.apply_prepared(prepared);
            drop(projection);
            // Durable detach and paired publication precede physical collection.
            // This closure retains both activity and seal exclusions to terminal.
            journal.collect_retired_working(16).map_err(error)?;
            writer.wake.notify_waiters();
            Ok(true)
        })
        .await
        .map_err(|_| Errno::EIO)?;
        match result {
            Ok(changed) => {
                self.maintenance_retries
                    .lock()
                    .map_err(|_| Errno::EIO)?
                    .remove(&owner);
                if changed {
                    engine.changed.notify_waiters();
                }
                Ok(Some(true))
            }
            Err(error) if error == Errno::ESTALE => Ok(Some(true)),
            Err(error) => Err(error),
        }
    }
}
#[cfg(test)]
mod tests;
