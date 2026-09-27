//! Recoverable deletion of one Cirrove-owned validation file. The normal
//! iCloud service does not construct this feature-gated mutation provider.
use super::{ICloudReadSession, ValidationFile, ValidationFolder};
use async_trait::async_trait;
use cirrove_core::mutation::{
    DeletionSupport, MutationError, MutationIntent, MutationProvider, MutationReceipt,
    MutationReconciliation, MutationRequest, Result as MutationResult,
};
use cirrove_core::{CancellationToken, Node, NodeKind, Scope};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Mutex;
use uuid::Uuid;

enum Observation {
    AtParent,
    InTrash,
    Unknown,
}

pub struct ICloudOwnedFixtureRemove {
    scope: Scope,
    folder: ValidationFolder,
    file: ValidationFile,
    before: Node,
    sha256: Option<String>,
    session: Mutex<ICloudReadSession>,
    discard_trash_receipt: AtomicBool,
    reconciliation_only: bool,
}

impl ICloudOwnedFixtureRemove {
    pub fn new(
        scope: Scope,
        session: ICloudReadSession,
        folder: ValidationFolder,
        file: ValidationFile,
        bytes: &[u8],
    ) -> MutationResult<Self> {
        if scope.account.is_empty()
            || scope.provider != "icloud"
            || scope.collection != "drive"
            || session.account_hash.is_none()
            || !folder.id.starts_with("FOLDER::com.apple.CloudDocs::")
            || folder
                .name
                .strip_prefix("Cirrove Write Validation-")
                .is_none_or(|suffix| Uuid::parse_str(suffix).is_err())
            || file.name != "created-by-cirrove.txt"
            || !file.id.starts_with("FILE::com.apple.CloudDocs::")
            || file.document_id.is_empty()
            || file.id.rsplit("::").next() != Some(file.document_id.as_str())
            || file.etag.is_empty()
            || bytes.is_empty()
            || bytes.len() > 4096
        {
            return Err(MutationError::Invalid);
        }
        let before = Node {
            id: file.id.clone(),
            parent_id: Some(folder.id.clone()),
            name: file.name.clone(),
            kind: NodeKind::File,
            size: bytes.len() as u64,
            modified_unix: 0,
            etag: Some(file.etag.clone()),
            content_version: Some(file.etag.clone()),
            target: None,
            package: false,
        };
        Ok(Self {
            scope,
            folder,
            file,
            before,
            sha256: Some(hex::encode(Sha256::digest(bytes))),
            session: Mutex::new(session),
            discard_trash_receipt: AtomicBool::new(false),
            reconciliation_only: false,
        })
    }

    /// A fresh validation process can reconstruct only the read-only side
    /// from the durable mutation request. It cannot issue another Trash call.
    pub fn for_reconciliation(
        scope: Scope,
        session: ICloudReadSession,
        folder: ValidationFolder,
        before: Node,
    ) -> MutationResult<Self> {
        let document_id = before.id.rsplit("::").next().unwrap_or_default().to_owned();
        let file = ValidationFile {
            id: before.id.clone(),
            document_id,
            etag: before.etag.clone().ok_or(MutationError::Invalid)?,
            name: before.name.clone(),
        };
        if scope.account.is_empty()
            || scope.provider != "icloud"
            || scope.collection != "drive"
            || session.account_hash.is_none()
            || !folder.id.starts_with("FOLDER::com.apple.CloudDocs::")
            || folder
                .name
                .strip_prefix("Cirrove Write Validation-")
                .is_none_or(|suffix| Uuid::parse_str(suffix).is_err())
            || before.parent_id.as_deref() != Some(folder.id.as_str())
            || before.kind != NodeKind::File
            || before.target.is_some()
            || before.package
            || before.size == 0
            || before.size > 4096
            || file.name != "created-by-cirrove.txt"
            || !file.id.starts_with("FILE::com.apple.CloudDocs::")
            || file.document_id.is_empty()
            || file.id.rsplit("::").next() != Some(file.document_id.as_str())
            || file.etag.is_empty()
        {
            return Err(MutationError::Invalid);
        }
        Ok(Self {
            scope,
            folder,
            file,
            before,
            sha256: None,
            session: Mutex::new(session),
            discard_trash_receipt: AtomicBool::new(false),
            reconciliation_only: true,
        })
    }

    pub fn with_discarded_trash_receipt(self) -> Self {
        self.discard_trash_receipt.store(true, Ordering::Release);
        self
    }

    pub fn before_node(&self) -> Node {
        self.before.clone()
    }

    fn check(&self, request: &MutationRequest, prepared: Option<&str>) -> MutationResult<()> {
        request.validate()?;
        if request.scope != self.scope
            || !matches!(&request.intent, MutationIntent::RemoveFile { before } if before == &self.before)
            || prepared.is_some_and(|id| id != self.file.id)
        {
            return Err(MutationError::Invalid);
        }
        Ok(())
    }

    async fn observe(&self) -> MutationResult<Observation> {
        let mut session = self.session.lock().await;
        let root = session
            .list_root()
            .await
            .map_err(|_| MutationError::Uncertain)?;
        if !root.iter().any(|entry| {
            entry.drivewsid == self.folder.id
                && entry.display_name() == self.folder.name
                && entry.is_folder()
        }) {
            return Err(MutationError::Uncertain);
        }
        let children = session
            .list_folder(&self.folder.id)
            .await
            .map_err(|_| MutationError::Uncertain)?;
        let mut matching = children
            .iter()
            .filter(|entry| entry.drivewsid == self.file.id);
        if let Some(entry) = matching.next() {
            if matching.next().is_some()
                || entry.is_folder()
                || entry.docwsid != self.file.document_id
                || entry.display_name() != self.file.name
                || entry.etag != self.file.etag
                || entry.size != self.before.size
            {
                return Err(MutationError::Conflict);
            }
            if let Some(expected) = &self.sha256 {
                let bytes = session
                    .read_small_file_in_folder(&self.folder.id, &self.file.id)
                    .await
                    .map_err(|_| MutationError::Uncertain)?;
                if hex::encode(Sha256::digest(bytes)) != *expected {
                    return Err(MutationError::Conflict);
                }
            }
            return Ok(Observation::AtParent);
        }
        let (trash, complete) = session
            .read_trash_items()
            .await
            .map_err(|_| MutationError::Uncertain)?;
        if !complete {
            return Err(MutationError::Uncertain);
        }
        let mut matching = trash.iter().filter(|item| {
            item.get("drivewsid").and_then(|id| id.as_str()) == Some(self.file.id.as_str())
        });
        let Some(item) = matching.next() else {
            return Ok(Observation::Unknown);
        };
        if matching.next().is_some()
            || item.get("restorePath").is_none_or(|path| path.is_null())
            || item
                .get("docwsid")
                .and_then(|id| id.as_str())
                .is_some_and(|id| !id.is_empty() && id != self.file.document_id)
        {
            return Err(MutationError::Conflict);
        }
        Ok(Observation::InTrash)
    }
}

#[async_trait]
impl MutationProvider for ICloudOwnedFixtureRemove {
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
            Observation::AtParent => Ok(Some(self.file.id.clone())),
            Observation::InTrash | Observation::Unknown => Err(MutationError::Conflict),
        }
    }

    async fn mutate(
        &self,
        _: &MutationRequest,
        _: &CancellationToken,
    ) -> MutationResult<MutationReceipt> {
        Err(MutationError::Unsupported(
            "owned fixture requires prepared identity",
        ))
    }

    async fn mutate_prepared(
        &self,
        request: &MutationRequest,
        prepared_item: Option<&str>,
        cancel: &CancellationToken,
    ) -> MutationResult<MutationReceipt> {
        self.check(request, prepared_item)?;
        if self.reconciliation_only {
            return Err(MutationError::Unsupported("reconciliation-only validation"));
        }
        if prepared_item != Some(self.file.id.as_str()) {
            return Err(MutationError::Invalid);
        }
        if cancel.is_cancelled() {
            return Err(MutationError::Uncertain);
        }
        if !matches!(self.observe().await?, Observation::AtParent) {
            return Err(MutationError::Conflict);
        }
        let accepted = {
            let mut session = self.session.lock().await;
            if self.discard_trash_receipt.swap(false, Ordering::AcqRel) {
                session
                    .send_trash_without_receipt(&self.file.id, &self.file.etag)
                    .await
                    .map_err(|_| MutationError::Uncertain)?;
                return Err(MutationError::Uncertain);
            }
            session
                .send_trash(&self.file.id, &self.file.etag)
                .await
                .map_err(|_| MutationError::Uncertain)?
        };
        if !accepted {
            return Err(MutationError::Uncertain);
        }
        if matches!(self.observe().await?, Observation::InTrash) {
            Ok(MutationReceipt::Removed {
                item: self.file.id.clone(),
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
        if prepared_item != Some(self.file.id.as_str()) {
            return Err(MutationError::Invalid);
        }
        match self.observe().await {
            Ok(Observation::InTrash) => {
                Ok(MutationReconciliation::Applied(MutationReceipt::Removed {
                    item: self.file.id.clone(),
                }))
            }
            Ok(Observation::AtParent | Observation::Unknown) | Err(MutationError::Uncertain) => {
                Ok(MutationReconciliation::Indeterminate)
            }
            Err(MutationError::Conflict) => Ok(MutationReconciliation::Conflict),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn fixture() -> (ICloudOwnedFixtureRemove, MutationRequest) {
        let mut session = ICloudReadSession::new().unwrap();
        session.account_hash = Some("synthetic-account".into());
        let scope = Scope {
            account: Uuid::new_v4().to_string(),
            provider: "icloud".into(),
            collection: "drive".into(),
        };
        let folder = ValidationFolder {
            id: format!("FOLDER::com.apple.CloudDocs::{}", Uuid::new_v4()),
            name: format!("Cirrove Write Validation-{}", Uuid::new_v4()),
        };
        let document = Uuid::new_v4().to_string();
        let file = ValidationFile {
            id: format!("FILE::com.apple.CloudDocs::{document}"),
            document_id: document,
            etag: "fixture-etag".into(),
            name: "created-by-cirrove.txt".into(),
        };
        let provider =
            ICloudOwnedFixtureRemove::new(scope.clone(), session, folder, file, b"old").unwrap();
        let request = MutationRequest {
            scope,
            intent: MutationIntent::RemoveFile {
                before: provider.before_node(),
            },
        };
        (provider, request)
    }

    #[tokio::test]
    async fn only_exact_prepared_fixture_can_reach_network() {
        let (provider, request) = fixture();
        assert_eq!(
            provider.deletion(),
            DeletionSupport {
                recycle_bin: true,
                permanent: false,
            }
        );
        assert!(
            provider
                .mutate_prepared(&request, None, &CancellationToken::new())
                .await
                .is_err()
        );
        assert!(
            provider
                .mutate_prepared(&request, Some("other"), &CancellationToken::new())
                .await
                .is_err()
        );
        let mut other = request.clone();
        other.scope.account = Uuid::new_v4().to_string();
        assert!(
            provider
                .prepare_mutation(&other, &CancellationToken::new())
                .await
                .is_err()
        );
        let mut other = request.clone();
        other.intent = MutationIntent::RemoveFolder {
            before: provider.before_node(),
        };
        assert!(
            provider
                .reconcile_prepared_mutation(
                    &other,
                    Some(&provider.file.id),
                    &CancellationToken::new()
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn restarted_fixture_can_reconcile_but_cannot_delete_again() {
        let (provider, request) = fixture();
        let mut session = ICloudReadSession::new().unwrap();
        session.account_hash = Some("synthetic-account".into());
        let folder = ValidationFolder {
            id: provider.folder.id.clone(),
            name: provider.folder.name.clone(),
        };
        let restarted = ICloudOwnedFixtureRemove::for_reconciliation(
            request.scope.clone(),
            session,
            folder,
            provider.before_node(),
        )
        .unwrap();
        let error = restarted
            .mutate_prepared(&request, Some(&provider.file.id), &CancellationToken::new())
            .await
            .err()
            .unwrap();
        assert!(matches!(error, MutationError::Unsupported(_)));
        assert!(
            restarted
                .prepare_mutation(&request, &CancellationToken::new())
                .await
                .is_err()
        );
    }
}
