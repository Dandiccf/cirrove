use super::{ChildrenResponse, DriveItem, OneDrive, map_item};
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
            if matches!(request.intent, MutationIntent::RemoveFolder { .. }) {
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
                    || current.folder.is_none()
                    || current.file.is_some()
                    // A OneNote notebook carries a folder facet. Removing one
                    // through rmdir would be a surprise, so it is refused here
                    // rather than left to the caller to notice.
                    || current.package.is_some()
                    || current.deleted.is_some()
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
                // Emptiness is checked separately, last, and from the children
                // listing rather than the folder's own `childCount`. Graph's
                // DELETE on a folder is recursive, and a folder's eTag and
                // lastModifiedDateTime do not move when a child is added --
                // measured, see `folder_etag_and_mtime_ignore_their_children`.
                // So the eTag precondition below cannot detect a new child, and
                // the item's own counters are not trusted to either. The listing
                // was measured to be fresh immediately after a create.
                //
                // This narrows the window to one round trip. It does not close
                // it: a child created between this call and the DELETE is
                // destroyed by it. Nothing available over Graph closes it.
                let mut children = self.resource_url(&[
                    "drives",
                    &request.scope.collection,
                    "items",
                    &before.id,
                    "children",
                ])?;
                children
                    .query_pairs_mut()
                    .append_pair("$top", "1")
                    .append_pair("$select", "id");
                let bytes = self.request_bytes(children).await?;
                let page: ChildrenResponse =
                    serde_json::from_slice(&bytes).map_err(|_| UploadError::Uncertain)?;
                if !page.value.is_empty() {
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
                MutationIntent::RemoveFile { .. } | MutationIntent::RemoveFolder { .. } => {
                    (Method::DELETE, None)
                }
                _ => return Err(UploadError::Invalid),
            };
            let response = self
                .authorized_upload(method, url, body, before.etag.as_deref())
                .await?;
            if matches!(
                request.intent,
                MutationIntent::RemoveFile { .. } | MutationIntent::RemoveFolder { .. }
            ) && response.status() == StatusCode::NO_CONTENT
            {
                return Ok(MutationReceipt::Removed {
                    item: before.id.clone(),
                });
            }
            let (status, bytes) = self.upload_body(response, false).await?;
            if status != StatusCode::OK
                || matches!(
                    request.intent,
                    MutationIntent::RemoveFile { .. } | MutationIntent::RemoveFolder { .. }
                )
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
