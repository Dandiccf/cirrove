use super::{DriveItem, OneDrive, map_item};
use async_trait::async_trait;
use cirrove_core::mutation::{
    MutationError, MutationIntent, MutationProvider, MutationReceipt, MutationReconciliation,
    MutationRequest, Result,
};
use cirrove_core::upload::UploadError;
use cirrove_core::{CancellationToken, Change, ProviderError, ReadProvider};
use reqwest::{Method, StatusCode};
use serde_json::json;

fn error(error: UploadError) -> MutationError {
    match error {
        UploadError::Provider(p) => p.into(),
        UploadError::Conflict => MutationError::Conflict,
        UploadError::Quota => MutationError::Quota,
        UploadError::Locked => MutationError::Locked,
        UploadError::Invalid => MutationError::Invalid,
        UploadError::Unsupported(s) => MutationError::Unsupported(s),
        _ => MutationError::Uncertain,
    }
}
impl OneDrive {
    fn check_mutation(&self, request: &MutationRequest) -> Result<()> {
        request.validate()?;
        if request.scope.account != self.account || request.scope.provider != "onedrive" {
            return Err(MutationError::Invalid);
        }
        Ok(())
    }
}
#[async_trait]
impl MutationProvider for OneDrive {
    async fn mutate(
        &self,
        request: &MutationRequest,
        cancel: &CancellationToken,
    ) -> Result<MutationReceipt> {
        self.check_mutation(request)?;
        if let MutationIntent::CreateFolder { parent, name } = &request.intent {
            let receipt = MutationReceipt::Upsert(
                self.create_folder(&request.scope, parent, name, cancel)
                    .await
                    .map_err(error)?,
            );
            return if request.accepts(&receipt) {
                Ok(receipt)
            } else {
                Err(MutationError::Uncertain)
            };
        }
        self.upload_call(cancel, async {
            let before = request.intent.before().ok_or(UploadError::Invalid)?;
            if matches!(request.intent, MutationIntent::RemoveFile { .. }) {
                let bytes = self
                    .request_bytes(self.resource_url(&[
                        "drives",
                        &request.scope.collection,
                        "items",
                        &before.id,
                    ])?)
                    .await?;
                let current: DriveItem =
                    serde_json::from_slice(&bytes).map_err(|_| UploadError::Uncertain)?;
                if current.id != before.id
                    || current.file.is_none()
                    || current.remote_item.is_some()
                    || current.e_tag != before.etag
                    || current
                        .parent_reference
                        .as_ref()
                        .and_then(|p| p.drive_id.as_ref())
                        != Some(&request.scope.collection)
                {
                    return Err(UploadError::Conflict);
                }
            }
            let url =
                self.resource_url(&["drives", &request.scope.collection, "items", &before.id])?;
            let (method, body) = match &request.intent {
                MutationIntent::Relocate { parent, name, .. } => (
                    Method::PATCH,
                    Some(json!({
                        "name": name,
                        "parentReference": { "id": parent },
                        "@microsoft.graph.conflictBehavior": "fail"
                    })),
                ),
                MutationIntent::RemoveFile { .. } => (Method::DELETE, None),
                _ => return Err(UploadError::Invalid),
            };
            let response = self
                .authorized_upload(method, url, body, before.etag.as_deref())
                .await?;
            if matches!(request.intent, MutationIntent::RemoveFile { .. })
                && response.status() == StatusCode::NO_CONTENT
            {
                return Ok(MutationReceipt::Removed {
                    item: before.id.clone(),
                });
            }
            let (status, bytes) = self.upload_body(response, false).await?;
            if status != StatusCode::OK
                || matches!(request.intent, MutationIntent::RemoveFile { .. })
            {
                return Err(UploadError::Uncertain);
            }
            let item: DriveItem =
                serde_json::from_slice(&bytes).map_err(|_| UploadError::Uncertain)?;
            if item
                .parent_reference
                .as_ref()
                .and_then(|p| p.drive_id.as_ref())
                != Some(&request.scope.collection)
            {
                return Err(UploadError::Uncertain);
            }
            let Change::Upsert(node) = map_item(item)? else {
                return Err(UploadError::Uncertain);
            };
            let receipt = MutationReceipt::Upsert(node);
            if !request.accepts(&receipt) {
                return Err(UploadError::Uncertain);
            }
            Ok(receipt)
        })
        .await
        .map_err(error)
    }
    async fn reconcile_mutation(
        &self,
        request: &MutationRequest,
        cancel: &CancellationToken,
    ) -> Result<MutationReconciliation> {
        self.check_mutation(request)?;
        let Some(before) = request.intent.before() else {
            // A matching folder name cannot prove which actor created it. Do not
            // adopt somebody else's directory or replay creation after a lost reply.
            return Ok(MutationReconciliation::Indeterminate);
        };
        let node = match self.node(&request.scope, &before.id, cancel).await {
            Ok(node) => node,
            Err(ProviderError::NotFound) => return Ok(MutationReconciliation::Indeterminate),
            Err(error) => return Err(error.into()),
        };
        let receipt = MutationReceipt::Upsert(node.clone());
        if request.accepts(&receipt) {
            // Same immutable item identity at the requested location is enough
            // for a namespace change. Content may have changed independently.
            return Ok(MutationReconciliation::Applied(receipt));
        }
        if node.etag == before.etag
            && node.name == before.name
            && node.parent_id == before.parent_id
            && node.kind == before.kind
            && node.target == before.target
        {
            Ok(MutationReconciliation::Uncommitted)
        } else {
            Ok(MutationReconciliation::Conflict)
        }
    }
}
