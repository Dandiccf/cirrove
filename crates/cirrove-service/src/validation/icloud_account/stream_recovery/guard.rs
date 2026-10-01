//! Permit only inspection/reconciliation at the captured body/registration boundary.
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
    pub complete_body: bool,
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
            || document(checkpoint, self.complete_body).is_none()
        {
            return Err(UploadError::CheckpointInvalid);
        }
        Ok(())
    }
}
// Local encrypted checkpoint envelope only; never expose its token fields.
pub(super) fn allocated_document(checkpoint: &SecretString) -> Option<String> {
    document(checkpoint, false)
}
pub(super) fn completed_body_document(checkpoint: &SecretString) -> Option<String> {
    document(checkpoint, true)
}
fn document(checkpoint: &SecretString, complete_body: bool) -> Option<String> {
    if checkpoint.expose_secret().len() > 32768 {
        return None;
    }
    let value = serde_json::from_str::<serde_json::Value>(checkpoint.expose_secret()).ok()?;
    let inner = match (value.get("inner"), value.get("phase")) {
        (Some(inner), None) => inner.as_str()?,
        (None, Some(phase)) => {
            let phase = phase.as_object()?;
            if phase.len() != 1 {
                return None;
            }
            phase.get("Stage")?.get("inner")?.as_str()?
        }
        _ => return None,
    };
    let inner = serde_json::from_str::<serde_json::Value>(inner).ok()?;
    let receipt = inner.get("receipt")?;
    if (complete_body && !receipt.is_object()) || (!complete_body && !receipt.is_null()) {
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

#[cfg(test)]
mod replacement_tests {
    use super::*;
    #[test]
    fn replacement_guard_accepts_only_the_allocated_stage_not_handoff_or_receipt() {
        let inner = serde_json::json!({"slot":{"document_id":"owned"},"receipt":null}).to_string();
        let checkpoint = |value: serde_json::Value| SecretString::from(value.to_string());
        assert_eq!(
            allocated_document(&checkpoint(
                serde_json::json!({"phase":{"Stage":{"inner":inner}}})
            ))
            .as_deref(),
            Some("owned")
        );
        for value in [
            serde_json::json!({"phase":{"Handoff":{"inner":inner}}}),
            serde_json::json!({"inner":inner,"phase":{"Stage":{"inner":inner}}}),
            serde_json::json!({"phase":{"Stage":{"inner":inner},"Handoff":{}}}),
        ] {
            assert!(allocated_document(&checkpoint(value)).is_none());
        }
    }
}

#[cfg(test)]
mod registration_tests {
    use super::*;
    #[test]
    fn completed_body_boundary_cannot_be_confused_with_an_incomplete_stream() {
        let checkpoint = |receipt: serde_json::Value| {
            SecretString::from(serde_json::json!({"inner":serde_json::json!({"slot":{"document_id":"owned"},"receipt":receipt}).to_string()}).to_string())
        };
        // This helper classifies the envelope. The real adapter validates every
        // receipt field and its size against the exact request before any read.
        let complete = checkpoint(
            serde_json::json!({"fileChecksum":"fixture","referenceChecksum":"fixture","wrappingKey":"fixture","size":3}),
        );
        assert_eq!(completed_body_document(&complete).as_deref(), Some("owned"));
        assert!(allocated_document(&complete).is_none());
        for receipt in [
            serde_json::Value::Null,
            serde_json::json!(false),
            serde_json::json!("not a receipt"),
            serde_json::json!([]),
        ] {
            assert!(completed_body_document(&checkpoint(receipt)).is_none());
        }
        assert!(completed_body_document(&SecretString::from("{}")).is_none());
    }
}
