//! Bounded, cancellation-aware sealing. The blocking worker owns every permit
//! until archive validation and the final journal commit have actually finished.
use super::*;

pub(super) struct Sealing {
    slots: Arc<tokio::sync::Semaphore>,
    working: Mutex<HashMap<Uuid, Weak<tokio::sync::Mutex<()>>>>,
    #[cfg(test)]
    probe: Mutex<Option<Arc<tests::Probe>>>,
}
impl Default for Sealing {
    fn default() -> Self {
        Self {
            slots: Arc::new(tokio::sync::Semaphore::new(2)),
            working: Mutex::default(),
            #[cfg(test)]
            probe: Mutex::default(),
        }
    }
}
impl Sealing {
    pub(super) async fn permit(&self) -> Result<tokio::sync::OwnedSemaphorePermit> {
        self.slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| Errno::ENODEV)
    }
    fn working(&self, id: Uuid) -> Result<Arc<tokio::sync::Mutex<()>>> {
        let mut gates = self.working.lock().map_err(|_| Errno::EIO)?;
        gates.retain(|_, gate| gate.strong_count() > 0);
        if let Some(gate) = gates.get(&id).and_then(Weak::upgrade) {
            return Ok(gate);
        }
        let gate = Arc::new(tokio::sync::Mutex::new(()));
        gates.insert(id, Arc::downgrade(&gate));
        Ok(gate)
    }
}
impl Writeback {
    pub(super) async fn seal_bounded(&self, id: Uuid) -> Result<()> {
        let cancel = CancellationToken::new();
        let _cancel_on_abandon = cancel.clone().drop_guard();
        // Concurrent fsyncs of the same file wait before reserving bytes. After
        // the first commit, a duplicate observes a clean generation and is a no-op.
        let working = self.sealing.working(id)?.lock_owned().await;
        let permit = self
            .sealing
            .slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| Errno::ENODEV)?;
        let journal = self.journal.clone();
        let wake = self.wake.clone();
        #[cfg(test)]
        let probe = self.sealing.probe.lock().map_err(|_| Errno::EIO)?.clone();
        let record = tokio::task::spawn_blocking(move || {
            let _working = working;
            let _permit = permit;
            #[cfg(test)]
            if let Some(probe) = probe {
                probe.pause(id, &cancel)?;
            }
            if cancel.is_cancelled() {
                return Err(JournalError::Stale);
            }
            let capture = {
                let mut j = journal.lock().map_err(|_| JournalError::Storage)?;
                if cancel.is_cancelled() {
                    return Err(JournalError::Stale);
                }
                let file = j.working_file(id)?;
                if file.native && file.dirty {
                    Some(j.capture_native_working(id)?)
                } else {
                    if !file.native {
                        j.seal_working(id)?;
                    }
                    None
                }
            };
            if let Some(capture) = capture {
                let captured = capture.capture(&cancel)?;
                // No mutex covers archive copy/hash/semantic parsing above.
                let mut j = journal.lock().map_err(|_| JournalError::Storage)?;
                j.seal_captured_native_working(captured, &cancel)?;
            }
            let record = journal
                .lock()
                .map_err(|_| JournalError::Storage)?
                .working_file(id)?;
            // A committed save must wake its worker even if its async observer
            // was abandoned while the durable commit was finishing.
            wake.notify_waiters();
            Ok(record)
        })
        .await
        .map_err(|_| Errno::EIO)?
        .inspect_err(|failure| self.refusals.note(failure))
        .map_err(error)?;
        self.publish(record).await?;
        self.wake.notify_waiters();
        Ok(())
    }
}
#[cfg(test)]
mod tests;
