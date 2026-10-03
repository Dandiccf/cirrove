//! Exact metadata observation for ordinary owned folders only.
use super::*;

fn ordinary_id(id: &str) -> bool {
    if id.len() > 512
        || id
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\'))
    {
        return false;
    }
    let mut parts = id.split("::");
    matches!((parts.next(),parts.next(),parts.next(),parts.next()),
        (Some("FOLDER"),Some("com.apple.CloudDocs"),Some(key),None) if !key.is_empty() && !key.contains(':'))
}
impl ICloudDrive {
    pub(super) async fn plain_folder_node(
        &self,
        id: &str,
        cancel: &CancellationToken,
    ) -> Result<Node, ProviderError> {
        if id == crate::write_transport::TRASH_ROOT {
            return Err(ProviderError::NotFound);
        }
        if !ordinary_id(id) {
            return Err(ProviderError::Unavailable);
        }
        let item = tokio::select! {biased;
            _=cancel.cancelled()=>return Err(ProviderError::Cancelled),
            result=async {
                let _permit=self.reads.acquire().await.map_err(|_|ProviderError::Unavailable)?;
                let mut session={let mut state=self.session.lock().await;Self::active_session(&mut state).await?.read_only_fork()};
                if cancel.is_cancelled() {return Err(ProviderError::Cancelled);}
                session.active_folder_metadata(id).await.map_err(|error|map_read_error(&error))
            }=>result?,
        };
        if item.parent_id == crate::write_transport::TRASH_ROOT || item.parent_id == "TRASH_ROOT" {
            return Err(ProviderError::NotFound);
        }
        if item.drivewsid != id
            || item.kind != "FOLDER"
            || item.zone != "com.apple.CloudDocs"
            || !ordinary_id(&item.parent_id)
            || item.parent_id == id
        {
            return Err(ProviderError::Protocol(
                "invalid iCloud ordinary folder metadata",
            ));
        }
        let parent = item.parent_id.clone();
        // No document expansion, content download, or app-container coercion.
        directory_nodes(&parent, vec![item])?
            .pop()
            .ok_or(ProviderError::Unavailable)
    }
}
