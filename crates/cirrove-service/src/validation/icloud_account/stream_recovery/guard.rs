//! Permit only inspection/reconciliation of the captured create and checkpoint.
use super::*;
use cirrove_core::upload::{
    Reconciliation, UploadError, UploadProvider, UploadRequest, UploadStep,
};
use secrecy::ExposeSecret;
use std::sync::atomic::{AtomicU64, Ordering};
pub(super) struct Guard {
    pub inner: ICloudWriteProvider,
    pub request: UploadRequest,
    pub operation: String,
    pub inspections: AtomicU64,
    pub reconciliations: AtomicU64,
    pub refused: AtomicU64,
}
impl Guard {
    fn refuse<T>(&self) -> cirrove_core::upload::Result<T> {
        self.refused.fetch_add(1, Ordering::Relaxed);
        Err(UploadError::Uncertain)
    }
    fn check(
        &self,
        operation: &str,
        request: &UploadRequest,
        checkpoint: &SecretString,
    ) -> cirrove_core::upload::Result<()> {
        if operation != self.operation
            || request != &self.request
            || allocated_document(checkpoint).is_none()
        {
            return Err(UploadError::CheckpointInvalid);
        }
        Ok(())
    }
}
// Local encrypted checkpoint envelope only; never expose its token fields.
pub(super) fn allocated_document(checkpoint: &SecretString) -> Option<String> {
    if checkpoint.expose_secret().len() > 32768 {
        return None;
    }
    let value = serde_json::from_str::<serde_json::Value>(checkpoint.expose_secret()).ok()?;
    let inner = value.get("inner")?.as_str()?;
    let inner = serde_json::from_str::<serde_json::Value>(inner).ok()?;
    if !inner.get("receipt")?.is_null() {
        return None;
    }
    let document = inner.get("slot")?.get("document_id")?.as_str()?;
    (!document.is_empty() && document.len() <= 256).then(|| document.to_owned())
}
#[async_trait::async_trait]
impl UploadProvider for Guard {
    async fn begin_upload(
        &self,
        _: &UploadRequest,
        _: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.refuse()
    }
    async fn inspect_upload(
        &self,
        _: &UploadRequest,
        _: &SecretString,
        _: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.refuse()
    }
    async fn upload_part(
        &self,
        _: &UploadRequest,
        _: &SecretString,
        _: u64,
        _: Vec<u8>,
        _: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.refuse()
    }
    async fn upload_stream(
        &self,
        _: &UploadRequest,
        _: &SecretString,
        _: std::fs::File,
        _: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.refuse()
    }
    async fn commit_upload(
        &self,
        _: &UploadRequest,
        _: &SecretString,
        _: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.refuse()
    }
    async fn reconcile_upload(
        &self,
        _: &UploadRequest,
        _: Option<&SecretString>,
        _: &CancellationToken,
    ) -> cirrove_core::upload::Result<Reconciliation> {
        self.refuse()
    }
    async fn inspect_upload_for_operation(
        &self,
        operation: &str,
        request: &UploadRequest,
        checkpoint: &SecretString,
        cancel: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.check(operation, request, checkpoint)?;
        self.inspections.fetch_add(1, Ordering::Relaxed);
        match self
            .inner
            .inspect_upload_for_operation(operation, request, checkpoint, cancel)
            .await?
        {
            UploadStep::Complete(node) => Ok(UploadStep::Complete(node)),
            _ => self.refuse(),
        }
    }
    async fn reconcile_upload_for_operation(
        &self,
        operation: &str,
        request: &UploadRequest,
        checkpoint: Option<&SecretString>,
        cancel: &CancellationToken,
    ) -> cirrove_core::upload::Result<Reconciliation> {
        self.check(
            operation,
            request,
            checkpoint.ok_or(UploadError::CheckpointInvalid)?,
        )?;
        self.reconciliations.fetch_add(1, Ordering::Relaxed);
        self.inner
            .reconcile_upload_for_operation(operation, request, checkpoint, cancel)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn allocated_boundary_rejects_missing_slot_or_a_completed_body_receipt() {
        for inner in [
            serde_json::json!({}),
            serde_json::json!({"slot":null,"receipt":null}),
            serde_json::json!({"slot":{"document_id":"owned"},"receipt":{}}),
        ] {
            let value =
                SecretString::from(serde_json::json!({"inner":inner.to_string()}).to_string());
            assert!(allocated_document(&value).is_none());
        }
        let value=SecretString::from(serde_json::json!({"inner":serde_json::json!({"slot":{"document_id":"owned"},"receipt":null}).to_string()}).to_string());
        assert_eq!(allocated_document(&value).as_deref(), Some("owned"));
    }
}
