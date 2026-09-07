//! Bounded session residency. No map lock survives provider or filesystem I/O.
use cirrove_core::{
    CancellationToken, Node, ProviderError, ReadProvider, Scope,
    reads::{ReadIdentity, ReadSession},
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::OnceCell;

const SESSION_LIMIT: usize = 64;
const IDLE_LIFETIME: Duration = Duration::from_secs(60);
use super::{
    BLOCK_SIZE,
    windows::{Reservation, Staging, Window},
};
struct Entry {
    session: OnceCell<Option<Arc<dyn ReadSession>>>,
    progress: Mutex<Progress>,
}
#[derive(Default)]
struct Progress {
    end: Option<u64>,
    growth: u32,
    slot: Option<Arc<Slot>>,
    speed_limit: Option<u32>,
}
struct Slot {
    start: u64,
    length: u32,
    created: Instant,
    value: OnceCell<Result<Window, ProviderError>>,
    reservation: Mutex<Option<Reservation>>,
}
impl Slot {
    fn contains(&self, offset: u64, length: u32) -> bool {
        offset >= self.start && offset - self.start + length as u64 <= self.length as u64
    }
}
impl Entry {
    fn new() -> Self {
        Self {
            session: OnceCell::new(),
            progress: Mutex::new(Progress::default()),
        }
    }
    fn transferred(&self, bytes: u32, elapsed: Duration) -> Result<(), ProviderError> {
        // Aim for at most five seconds per speculative window at the observed
        // end-to-end rate, leaving headroom under the provider deadline.
        let bound = ((bytes as u128 * 5_000_000_000u128) / elapsed.as_nanos().max(1))
            .min(64 * 1024 * 1024) as u32
            / BLOCK_SIZE
            * BLOCK_SIZE;
        self.progress
            .lock()
            .map_err(|_| ProviderError::Unavailable)?
            .speed_limit = Some(bound);
        Ok(())
    }
    fn progressed(&self, offset: u64, length: u32) -> Result<(), ProviderError> {
        let mut p = self
            .progress
            .lock()
            .map_err(|_| ProviderError::Unavailable)?;
        if p.end != Some(offset) {
            p.growth = BLOCK_SIZE;
        }
        p.end = Some(offset + length as u64);
        if p.slot.as_ref().is_some_and(|slot| {
            slot.value.get().is_some_and(Result::is_ok)
                && p.end == Some(slot.start + slot.length as u64)
        }) {
            p.slot = None;
        }
        Ok(())
    }
    fn window(
        &self,
        offset: u64,
        length: u32,
        remaining: u64,
        limit: u32,
        staging: &Staging,
    ) -> Result<Option<Arc<Slot>>, ProviderError> {
        let mut p = self
            .progress
            .lock()
            .map_err(|_| ProviderError::Unavailable)?;
        if let Some(slot) = &p.slot {
            let abandoned = slot.value.get().is_none() && Arc::strong_count(slot) == 1;
            let failed = slot.value.get().is_some_and(Result::is_err)
                && slot.created.elapsed() >= Duration::from_secs(2);
            if abandoned || failed {
                p.slot = None;
                p.end = None;
                p.growth = BLOCK_SIZE;
            }
        }
        if let Some(slot) = &p.slot {
            if slot.contains(offset, length) {
                return Ok(Some(slot.clone()));
            }
            // A different range remains independent of a pending window.
            if slot.value.get().is_none_or(Result::is_err) {
                return Ok(None);
            }
        }
        if p.end != Some(offset) || limit <= BLOCK_SIZE || !offset.is_multiple_of(BLOCK_SIZE as u64)
        {
            return Ok(None);
        }
        p.slot = None;
        let wanted = p
            .growth
            .max(BLOCK_SIZE)
            .saturating_mul(2)
            .min(limit)
            .min(p.speed_limit.unwrap_or(64 * 1024 * 1024))
            .min(remaining.min(u32::MAX as u64) as u32);
        let Some(reservation) = staging.reserve(wanted) else {
            return Ok(None);
        };
        p.growth = reservation.length;
        let slot = Arc::new(Slot {
            start: offset,
            length: reservation.length,
            created: Instant::now(),
            value: OnceCell::new(),
            reservation: Mutex::new(Some(reservation)),
        });
        p.slot = Some(slot.clone());
        Ok(Some(slot))
    }
}
#[derive(Default)]
pub(super) struct Sessions {
    entries: Mutex<HashMap<ReadIdentity, (Arc<Entry>, Instant)>>,
    staging: Option<Arc<Staging>>,
}
impl Sessions {
    pub(super) fn with_staging(staging: Arc<Staging>) -> Self {
        Self {
            staging: Some(staging),
            ..Default::default()
        }
    }
    fn entry(&self, identity: &ReadIdentity) -> Result<Option<Arc<Entry>>, ProviderError> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| ProviderError::Unavailable)?;
        entries.retain(|_, (entry, used)| {
            Arc::strong_count(entry) > 1 || used.elapsed() < IDLE_LIFETIME
        });
        if let Some((entry, used)) = entries.get_mut(identity) {
            *used = Instant::now();
            return Ok(Some(entry.clone()));
        }
        if entries.len() >= SESSION_LIMIT {
            let idle = entries
                .iter()
                .filter(|(_, (entry, _))| Arc::strong_count(entry) == 1)
                .min_by_key(|(_, (_, used))| *used)
                .map(|(key, _)| key.clone());
            if let Some(key) = idle {
                entries.remove(&key);
            } else {
                return Ok(None);
            }
        }
        let entry = Arc::new(Entry::new());
        entries.insert(identity.clone(), (entry.clone(), Instant::now()));
        Ok(Some(entry))
    }
    pub(super) async fn read(
        &self,
        provider: &dyn ReadProvider,
        scope: &Scope,
        node: &Node,
        offset: u64,
        length: u32,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>, ProviderError> {
        let staging = self.staging.as_deref();
        let identity = ReadIdentity::new(scope, node)?;
        let Some(entry) = self.entry(&identity)? else {
            return provider
                .read_range(scope, node, offset, length, cancel)
                .await;
        };
        let session = entry
            .session
            .get_or_try_init(|| provider.open_read_session(scope, node, cancel))
            .await?;
        match session {
            Some(session) if session.identity() == &identity => {
                let count = (node.size.saturating_sub(offset)).min(length as u64) as u32;
                let window = if let Some(staging) = staging {
                    entry.window(
                        offset,
                        count,
                        node.size.saturating_sub(offset),
                        session.window_limit(),
                        staging,
                    )?
                } else {
                    None
                };
                let result = if let (Some(slot), Some(staging)) = (window, staging) {
                    let result = slot
                        .value
                        .get_or_init(|| async {
                            let reservation = slot
                                .reservation
                                .lock()
                                .map_err(|_| ProviderError::Unavailable)?
                                .take();
                            let reservation = reservation
                                .or_else(|| staging.reserve(slot.length))
                                .filter(|reservation| reservation.length == slot.length)
                                .ok_or(ProviderError::Unavailable)?;
                            let start = Instant::now();
                            let mut writer = staging.writer(reservation).await?;
                            session
                                .read_window(slot.start, slot.length, &mut writer, cancel)
                                .await?;
                            let window = writer.finish(slot.start)?;
                            staging.validated(slot.length);
                            entry.transferred(slot.length, start.elapsed())?;
                            Ok(window)
                        })
                        .await;
                    match result {
                        Ok(window) => {
                            let result = window.read(offset, count).await;
                            if result.is_err() {
                                let mut p = entry
                                    .progress
                                    .lock()
                                    .map_err(|_| ProviderError::Unavailable)?;
                                if p.slot
                                    .as_ref()
                                    .is_some_and(|current| Arc::ptr_eq(current, &slot))
                                {
                                    p.slot = None;
                                    p.end = None;
                                    p.growth = BLOCK_SIZE;
                                }
                            }
                            result
                        }
                        Err(error) => Err(error.clone()),
                    }
                } else {
                    let start = Instant::now();
                    let result = session.read_range(offset, length, cancel).await;
                    if result.is_ok() {
                        entry.transferred(count, start.elapsed())?;
                    }
                    result
                };
                if result.is_ok() {
                    entry.progressed(offset, count)?;
                }
                result
            }
            Some(_) => Err(ProviderError::Protocol(
                "provider returned a different read identity",
            )),
            None => {
                provider
                    .read_range(scope, node, offset, length, cancel)
                    .await
            }
        }
    }
}

#[cfg(test)]
mod tests;
