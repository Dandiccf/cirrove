//! Recoverable, non-recursive removal of an empty Cirrove-owned test folder.
//! The normal iCloud account never constructs this feature-gated adapter.
use super::{DriveEntry, ICloudReadSession, ValidationFolder};
use async_trait::async_trait;
use cirrove_core::mutation::{
    DeletionSupport, MutationError, MutationIntent, MutationProvider, MutationReceipt,
    MutationReconciliation, MutationRequest, Result as MutationResult,
};
use cirrove_core::{CancellationToken, Node, NodeKind, Scope};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Mutex;
use uuid::Uuid;

enum Observation {
    AtParentEmpty,
    InTrash,
    Unknown,
}

pub struct ICloudOwnedFixtureFolderRemove {
    scope: Scope,
    parent: ValidationFolder,
    before: Node,
    session: Mutex<ICloudReadSession>,
    discard_trash_receipt: AtomicBool,
    reconciliation_only: bool,
}

impl ICloudOwnedFixtureFolderRemove {
    pub fn new(
        scope: Scope,
        session: ICloudReadSession,
        parent: ValidationFolder,
        before: Node,
    ) -> MutationResult<Self> {
        if scope.account.is_empty()
            || scope.provider != "icloud"
            || scope.collection != "drive"
            || session.account_hash.is_none()
            || !parent.id.starts_with("FOLDER::com.apple.CloudDocs::")
            || parent
                .name
                .strip_prefix("Cirrove Write Validation-")
                .is_none_or(|suffix| Uuid::parse_str(suffix).is_err())
            || before.parent_id.as_deref() != Some(parent.id.as_str())
            || before.kind != NodeKind::Folder
            || !before.id.starts_with("FOLDER::com.apple.CloudDocs::")
            || before.id == parent.id
            || before.name.is_empty()
            || before.name.len() > 255
            || matches!(before.name.as_str(), "." | "..")
            || before.name.contains(['/', '\0'])
            || before.etag.as_deref().is_none_or(str::is_empty)
            || before.target.is_some()
            || before.package
        {
            return Err(MutationError::Invalid);
        }
        Ok(Self {
            scope,
            parent,
            before,
            session: Mutex::new(session),
            discard_trash_receipt: AtomicBool::new(false),
            reconciliation_only: false,
        })
    }

    pub fn with_discarded_trash_receipt(self) -> Self {
        self.discard_trash_receipt.store(true, Ordering::Release);
        self
    }

    pub fn reconciliation_only(mut self) -> Self {
        self.reconciliation_only = true;
        self
    }

    fn check(&self, request: &MutationRequest, prepared: Option<&str>) -> MutationResult<()> {
        request.validate()?;
        if request.scope != self.scope
            || !matches!(&request.intent, MutationIntent::RemoveFolder { before } if before == &self.before)
            || prepared.is_some_and(|id| id != self.before.id)
        {
            return Err(MutationError::Invalid);
        }
        Ok(())
    }

    fn check_at_parent(&self, entries: &[DriveEntry]) -> MutationResult<bool> {
        let matching: Vec<_> = entries
            .iter()
            .filter(|entry| entry.drivewsid == self.before.id)
            .collect();
        if matching.len() > 1 {
            return Err(MutationError::Conflict);
        }
        let Some(entry) = matching.first() else {
            return Ok(false);
        };
        if !entry.is_folder()
            || entry.kind != "FOLDER"
            || entry.display_name() != self.before.name
            || entry.parent_id != self.parent.id
            || entry.etag != self.before.etag.as_deref().unwrap_or_default()
            || entries.iter().any(|other| {
                other.drivewsid != self.before.id && other.display_name() == self.before.name
            })
        {
            return Err(MutationError::Conflict);
        }
        Ok(true)
    }

    async fn observe(&self) -> MutationResult<Observation> {
        let mut session = self.session.lock().await;
        let root = session
            .list_root()
            .await
            .map_err(|_| MutationError::Uncertain)?;
        if root
            .iter()
            .filter(|entry| {
                entry.drivewsid == self.parent.id
                    && entry.display_name() == self.parent.name
                    && entry.is_folder()
            })
            .count()
            != 1
        {
            return Err(MutationError::Uncertain);
        }
        let siblings = session
            .list_folder(&self.parent.id)
            .await
            .map_err(|_| MutationError::Uncertain)?;
        if self.check_at_parent(&siblings)? {
            let children = session
                .list_folder(&self.before.id)
                .await
                .map_err(|_| MutationError::Uncertain)?;
            if !children.is_empty() {
                return Err(MutationError::Conflict);
            }
            return Ok(Observation::AtParentEmpty);
        }
        let (trash, complete) = session
            .read_trash_items()
            .await
            .map_err(|_| MutationError::Uncertain)?;
        if !complete {
            return Err(MutationError::Uncertain);
        }
        let matching: Vec<_> = trash
            .iter()
            .filter(|item| {
                item.get("drivewsid").and_then(|value| value.as_str()) == Some(&self.before.id)
            })
            .collect();
        if matching.len() > 1 {
            return Err(MutationError::Conflict);
        }
        let Some(item) = matching.first() else {
            return Ok(Observation::Unknown);
        };
        if item
            .get("type")
            .and_then(|value| value.as_str())
            .is_some_and(|kind| kind != "FOLDER")
            || item.get("restorePath").is_none_or(|path| path.is_null())
        {
            return Err(MutationError::Conflict);
        }
        Ok(Observation::InTrash)
    }
}

#[async_trait]
impl MutationProvider for ICloudOwnedFixtureFolderRemove {
    fn deletion(&self) -> DeletionSupport {
        DeletionSupport {
            recycle_bin: true,
            permanent: false,
        }
    }

    async fn prepare_mutation(
        &self,
        request: &MutationRequest,
        cancel: &CancellationToken,
    ) -> MutationResult<Option<String>> {
        self.check(request, None)?;
        if self.reconciliation_only {
            return Err(MutationError::Unsupported("reconciliation-only validation"));
        }
        if cancel.is_cancelled() {
            return Err(MutationError::Uncertain);
        }
        match self.observe().await? {
            Observation::AtParentEmpty => Ok(Some(self.before.id.clone())),
            Observation::InTrash | Observation::Unknown => Err(MutationError::Conflict),
        }
    }

    async fn mutate(
        &self,
        _: &MutationRequest,
        _: &CancellationToken,
    ) -> MutationResult<MutationReceipt> {
        Err(MutationError::Unsupported(
            "owned folder removal requires prepared identity",
        ))
    }

    async fn mutate_prepared(
        &self,
        request: &MutationRequest,
        prepared_item: Option<&str>,
        cancel: &CancellationToken,
    ) -> MutationResult<MutationReceipt> {
        self.check(request, prepared_item)?;
        if self.reconciliation_only || prepared_item != Some(self.before.id.as_str()) {
            return Err(MutationError::Invalid);
        }
        if cancel.is_cancelled() {
            return Err(MutationError::Uncertain);
        }
        if !matches!(self.observe().await?, Observation::AtParentEmpty) {
            return Err(MutationError::Conflict);
        }
        let accepted = {
            let mut session = self.session.lock().await;
            let etag = self.before.etag.as_deref().ok_or(MutationError::Invalid)?;
            if self.discard_trash_receipt.swap(false, Ordering::AcqRel) {
                session
                    .send_trash_without_receipt(&self.before.id, etag)
                    .await
                    .map_err(|_| MutationError::Uncertain)?;
                return Err(MutationError::Uncertain);
            }
            session
                .send_trash(&self.before.id, etag)
                .await
                .map_err(|_| MutationError::Uncertain)?
        };
        if !accepted {
            return Err(MutationError::Uncertain);
        }
        if matches!(self.observe().await?, Observation::InTrash) {
            Ok(MutationReceipt::Removed {
                item: self.before.id.clone(),
            })
        } else {
            Err(MutationError::Uncertain)
        }
    }

    async fn reconcile_mutation(
        &self,
        _: &MutationRequest,
        _: &CancellationToken,
    ) -> MutationResult<MutationReconciliation> {
        Ok(MutationReconciliation::Indeterminate)
    }

    async fn reconcile_prepared_mutation(
        &self,
        request: &MutationRequest,
        prepared_item: Option<&str>,
        _: &CancellationToken,
    ) -> MutationResult<MutationReconciliation> {
        self.check(request, prepared_item)?;
        if prepared_item != Some(self.before.id.as_str()) {
            return Err(MutationError::Invalid);
        }
        match self.observe().await {
            Ok(Observation::InTrash) => {
                Ok(MutationReconciliation::Applied(MutationReceipt::Removed {
                    item: self.before.id.clone(),
                }))
            }
            Ok(Observation::AtParentEmpty | Observation::Unknown)
            | Err(MutationError::Uncertain) => Ok(MutationReconciliation::Indeterminate),
            Err(MutationError::Conflict) => Ok(MutationReconciliation::Conflict),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn fixture() -> (ICloudOwnedFixtureFolderRemove, MutationRequest, DriveEntry) {
        let mut session = ICloudReadSession::new().unwrap();
        session.account_hash = Some("synthetic-account".into());
        let scope = Scope {
            account: Uuid::new_v4().to_string(),
            provider: "icloud".into(),
            collection: "drive".into(),
        };
        let parent = ValidationFolder {
            id: format!("FOLDER::com.apple.CloudDocs::{}", Uuid::new_v4()),
            name: format!("Cirrove Write Validation-{}", Uuid::new_v4()),
        };
        let before = Node {
            id: format!("FOLDER::com.apple.CloudDocs::{}", Uuid::new_v4()),
            parent_id: Some(parent.id.clone()),
            name: "Nested Cirrove folder".into(),
            kind: NodeKind::Folder,
            size: 0,
            modified_unix: 0,
            etag: Some("folder-etag".into()),
            content_version: None,
            target: None,
            package: false,
        };
        let entry = DriveEntry {
            drivewsid: before.id.clone(),
            docwsid: before.id.rsplit("::").next().unwrap().into(),
            item_id: String::new(),
            zone: "com.apple.CloudDocs".into(),
            name: before.name.clone(),
            extension: String::new(),
            parent_id: parent.id.clone(),
            etag: "folder-etag".into(),
            kind: "FOLDER".into(),
            size: 0,
            items: vec![],
            number_of_items: Some(0),
        };
        let request = MutationRequest {
            scope: scope.clone(),
            intent: MutationIntent::RemoveFolder {
                before: before.clone(),
            },
        };
        (
            ICloudOwnedFixtureFolderRemove::new(scope, session, parent, before).unwrap(),
            request,
            entry,
        )
    }

    #[test]
    fn only_exact_owned_folder_and_etag_pass_parent_guard() {
        let (provider, request, entry) = fixture();
        assert!(provider.check(&request, Some(&entry.drivewsid)).is_ok());
        assert!(
            provider
                .check_at_parent(std::slice::from_ref(&entry))
                .unwrap()
        );
        let mut changed = entry.clone();
        changed.etag = "new-revision".into();
        assert!(matches!(
            provider.check_at_parent(&[changed]),
            Err(MutationError::Conflict)
        ));
        let mut changed = entry.clone();
        changed.kind = "FILE".into();
        assert!(matches!(
            provider.check_at_parent(&[changed]),
            Err(MutationError::Conflict)
        ));
        let mut other = request.clone();
        let MutationIntent::RemoveFolder { before } = &mut other.intent else {
            unreachable!()
        };
        before.id = format!("FOLDER::com.apple.CloudDocs::{}", Uuid::new_v4());
        assert!(provider.check(&other, Some(&entry.drivewsid)).is_err());
    }

    #[test]
    fn mounted_owned_folder_name_is_accepted_without_weakening_identity() {
        let (provider, _, _) = fixture();
        let mut session = ICloudReadSession::new().unwrap();
        session.account_hash = Some("synthetic-account".into());
        let mut before = provider.before.clone();
        before.name = "Mounted Folder".into();
        assert!(
            ICloudOwnedFixtureFolderRemove::new(
                provider.scope.clone(),
                session,
                provider.parent.clone(),
                before
            )
            .is_ok()
        );
    }

    #[tokio::test]
    async fn unprepared_and_reconciliation_only_adapters_never_send_trash() {
        let (provider, request, _) = fixture();
        assert!(matches!(
            provider.mutate(&request, &CancellationToken::new()).await,
            Err(MutationError::Unsupported(_))
        ));
        let provider = provider.reconciliation_only();
        assert!(
            provider
                .mutate_prepared(
                    &request,
                    request.intent.before().map(|node| node.id.as_str()),
                    &CancellationToken::new(),
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn nonempty_child_listing_stops_before_any_trash_request() {
        let (provider, request, entry) = fixture();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        provider.session.lock().await.drive_endpoint =
            Some(url::Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap());
        let parent = &provider.parent;
        let responses = [
            serde_json::json!([{
                "drivewsid": crate::ROOT_ID,
                "type": "FOLDER",
                "numberOfItems": 1,
                "items": [{
                    "drivewsid": parent.id,
                    "name": parent.name,
                    "type": "FOLDER"
                }]
            }]),
            serde_json::json!([{
                "drivewsid": parent.id,
                "type": "FOLDER",
                "numberOfItems": 1,
                "items": [entry]
            }]),
            serde_json::json!([{
                "drivewsid": provider.before.id,
                "type": "FOLDER",
                "numberOfItems": 1,
                "items": [{
                    "drivewsid": "FILE::com.apple.CloudDocs::new-child",
                    "name": "new-child",
                    "type": "FILE"
                }]
            }]),
        ];
        let server = tokio::spawn(async move {
            for response in responses {
                let (mut peer, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let header_end = loop {
                    let mut chunk = [0_u8; 4096];
                    let count = peer.read(&mut chunk).await.unwrap();
                    assert!(count > 0 && request.len() + count < 16 * 1024);
                    request.extend_from_slice(&chunk[..count]);
                    if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                        break end + 4;
                    }
                };
                let headers = std::str::from_utf8(&request[..header_end]).unwrap();
                assert!(headers.starts_with("POST /retrieveItemDetailsInFolders HTTP/1.1"));
                let body = response.to_string();
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                peer.write_all(reply.as_bytes()).await.unwrap();
            }
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(250), listener.accept())
                    .await
                    .is_err()
            );
        });
        let prepared = provider.before.id.clone();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            provider.mutate_prepared(&request, Some(&prepared), &CancellationToken::new()),
        )
        .await;
        assert!(matches!(result, Ok(Err(MutationError::Conflict))));
        server.await.unwrap();
    }
}
