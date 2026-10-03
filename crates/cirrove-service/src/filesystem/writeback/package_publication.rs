//! Metadata-only convergence of already committed native packages.
use super::*;
impl Writeback {
    pub(crate) async fn publish_completed_package(&self, engine: &Engine) -> Result<bool> {
        let now = publication_now();
        let Some(record) = self
            .local(move |journal| journal.package_publication_due(now))
            .await?
        else {
            return Ok(false);
        };
        if record.scope != engine.scope(&record.scope.collection) {
            return Err(Errno::EIO);
        }
        let remote = record.remote.as_ref().ok_or(Errno::EIO)?;
        // Never publish the receipt's potentially stale name/version directly.
        // Engine's observation ticket fences this exact-ID refresh against later
        // observations and updates cached directory membership transactionally.
        let result = tokio::select! {biased;
            _=engine.cancel.cancelled()=>return Err(Errno::ENODEV),
            result=tokio::time::timeout(std::time::Duration::from_secs(30),engine.refresh_node(&record.scope,&remote.id))=>result,
        };
        let status = match result {
            Ok(Ok(node)) => crate::journal::PackagePublicationStatus::Present(node),
            Ok(Err(ProviderError::NotFound)) => crate::journal::PackagePublicationStatus::Absent,
            _ => crate::journal::PackagePublicationStatus::Pending,
        };
        let published = status != crate::journal::PackagePublicationStatus::Pending;
        let now = publication_now();
        self.local(move |journal| journal.finish_package_publication(&record, status, now))
            .await?;
        if published { Ok(true) } else { Err(Errno::EIO) }
    }
}
fn publication_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
#[cfg(test)]
mod tests;
