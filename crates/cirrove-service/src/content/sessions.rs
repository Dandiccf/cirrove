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
type Entry = OnceCell<Option<Arc<dyn ReadSession>>>;
#[derive(Default)]
pub(super) struct Sessions {
    entries: Mutex<HashMap<ReadIdentity, (Arc<Entry>, Instant)>>,
}
impl Sessions {
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
        let entry = Arc::new(OnceCell::new());
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
        let identity = ReadIdentity::new(scope, node)?;
        let Some(entry) = self.entry(&identity)? else {
            return provider
                .read_range(scope, node, offset, length, cancel)
                .await;
        };
        let session = entry
            .get_or_try_init(|| provider.open_read_session(scope, node, cancel))
            .await?;
        match session {
            Some(session) if session.identity() == &identity => {
                session.read_range(offset, length, cancel).await
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
