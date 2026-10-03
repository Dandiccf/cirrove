//! Publish only ordered provider absence after an exact native Trash receipt.
use super::*;
impl Writeback {
    pub(crate) async fn publish_completed_native_trash(&self, engine: &Engine) -> Result<bool> {
        let now = publication_now();
        let Some(record) = self
            .local(move |j| j.native_trash_publication_due(now))
            .await?
        else {
            return Ok(false);
        };
        if record.request.scope != engine.scope(&record.request.scope.collection) {
            return Err(Errno::EIO);
        }
        let item = &record.request.intent.before().ok_or(Errno::EIO)?.id;
        let result = tokio::select! {biased;
            _=engine.cancel.cancelled()=>return Err(Errno::ENODEV),
            result=tokio::time::timeout(std::time::Duration::from_secs(30),engine.refresh_node(&record.request.scope,item))=>result,
        };
        let status = match result {
            Ok(Err(ProviderError::NotFound)) => crate::journal::PackagePublicationStatus::Absent,
            Ok(Ok(node)) => crate::journal::PackagePublicationStatus::Present(node),
            _ => crate::journal::PackagePublicationStatus::Pending,
        };
        // refresh_node commits absence with an observation ticket. A superseding
        // observation returns Present/VersionChanged instead of false absence.
        let removed = status == crate::journal::PackagePublicationStatus::Absent;
        let now = publication_now();
        self.local(move |j| j.finish_native_trash_publication(&record, status, now))
            .await?;
        if removed { Ok(true) } else { Err(Errno::EIO) }
    }
    pub(in crate::filesystem) async fn native_trash_publication(
        &self,
        id: Uuid,
    ) -> Result<crate::journal::PackagePublicationStatus> {
        self.local(move |j| j.native_trash_publication_status(id))
            .await
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
