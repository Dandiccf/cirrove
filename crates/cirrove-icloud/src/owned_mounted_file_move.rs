//! One feature-gated FUSE move from a Cirrove-owned test root into its own
//! journal-confirmed child folder. Ordinary iCloud mounts never use this.
use super::{ICloudReadSession, ValidationFolder};
use async_trait::async_trait;
use cirrove_core::mutation::{
    MutationError, MutationIntent, MutationProvider, MutationReceipt, MutationReconciliation,
    MutationRequest, Result as MutationResult,
};
use cirrove_core::{CancellationToken, Node, NodeKind, Scope};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Mutex;
use uuid::Uuid;

enum Observation {
    AtSource,
    AtDestination(Node),
    Conflict,
    Unknown,
}

pub struct ICloudOwnedMountedFileMove {
    scope: Scope,
    root: ValidationFolder,
    destination: Node,
    before: Node,
    digest: String,
    session: Mutex<ICloudReadSession>,
    discard_response: AtomicBool,
    reconciliation_only: bool,
}

impl ICloudOwnedMountedFileMove {
    pub fn new(
        scope: Scope,
        session: ICloudReadSession,
        root: ValidationFolder,
        destination: Node,
        before: Node,
        digest: String,
    ) -> MutationResult<Self> {
        Self::build(scope, session, root, destination, before, digest, false)
    }

    pub fn for_reconciliation(
        scope: Scope,
        session: ICloudReadSession,
        root: ValidationFolder,
        destination: Node,
        before: Node,
        digest: String,
    ) -> MutationResult<Self> {
        Self::build(scope, session, root, destination, before, digest, true)
    }

    fn build(
        scope: Scope,
        session: ICloudReadSession,
        root: ValidationFolder,
        destination: Node,
        before: Node,
        digest: String,
        reconciliation_only: bool,
    ) -> MutationResult<Self> {
        if scope.account.is_empty()
            || scope.provider != "icloud"
            || scope.collection != "drive"
            || session.account_hash.is_none()
            || !root.id.starts_with("FOLDER::com.apple.CloudDocs::")
            || root
                .name
                .strip_prefix("Cirrove Write Validation-")
                .is_none_or(|suffix| Uuid::parse_str(suffix).is_err())
            || destination.id == root.id
            || !destination.id.starts_with("FOLDER::com.apple.CloudDocs::")
            || destination.parent_id.as_deref() != Some(root.id.as_str())
            || destination.name != "Mounted Folder"
            || destination.kind != NodeKind::Folder
            || destination.target.is_some()
            || destination.package
            || !before.id.starts_with("FILE::com.apple.CloudDocs::")
            || before.parent_id.as_deref() != Some(root.id.as_str())
            || before.kind != NodeKind::File
            || before.target.is_some()
            || before.package
            || before.name.is_empty()
            || before.name.len() > 255
            || before.name.contains(['/', '\0', '\r', '\n'])
            || before.size == 0
            || before.size > 4096
            || before.etag.as_deref().is_none_or(str::is_empty)
            || digest.len() != 64
            || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(MutationError::Invalid);
        }
        Ok(Self {
            scope,
            root,
            destination,
            before,
            digest: digest.to_ascii_lowercase(),
            session: Mutex::new(session),
            discard_response: AtomicBool::new(false),
            reconciliation_only,
        })
    }

    pub fn with_discarded_response(self) -> Self {
        self.discard_response.store(true, Ordering::Release);
        self
    }

    fn check(&self, request: &MutationRequest, prepared: Option<&str>) -> MutationResult<()> {
        request.validate()?;
        if request.scope != self.scope
            || !matches!(&request.intent, MutationIntent::Relocate { before, parent, name }
                if before == &self.before && parent == &self.destination.id && name == &self.before.name)
            || prepared.is_some_and(|id| id != self.before.id)
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
        if root
            .iter()
            .filter(|entry| {
                entry.drivewsid == self.root.id
                    && entry.display_name() == self.root.name
                    && entry.is_folder()
            })
            .count()
            != 1
        {
            return Ok(Observation::Unknown);
        }
        let source = session
            .list_folder(&self.root.id)
            .await
            .map_err(|_| MutationError::Uncertain)?;
        let destination_entry: Vec<_> = source
            .iter()
            .filter(|entry| entry.drivewsid == self.destination.id)
            .collect();
        if destination_entry.len() != 1
            || !destination_entry[0].is_folder()
            || destination_entry[0].display_name() != self.destination.name
            || destination_entry[0].parent_id != self.root.id
        {
            return Ok(Observation::Conflict);
        }
        let destination = session
            .list_folder(&self.destination.id)
            .await
            .map_err(|_| MutationError::Uncertain)?;
        let source_file: Vec<_> = source
            .iter()
            .filter(|entry| entry.drivewsid == self.before.id)
            .collect();
        let destination_file: Vec<_> = destination
            .iter()
            .filter(|entry| entry.drivewsid == self.before.id)
            .collect();
        if source_file.len() > 1
            || destination_file.len() > 1
            || source.iter().any(|entry| {
                entry.drivewsid != self.before.id && entry.drivewsid != self.destination.id
            })
            || destination
                .iter()
                .any(|entry| entry.drivewsid != self.before.id)
        {
            return Ok(Observation::Conflict);
        }
        if source_file.len() == 1 && destination_file.is_empty() {
            let entry = source_file[0];
            if entry.is_folder()
                || entry.parent_id != self.root.id
                || entry.docwsid != self.before.id.rsplit("::").next().unwrap_or_default()
                || entry.display_name() != self.before.name
                || Some(entry.etag.as_str()) != self.before.etag.as_deref()
                || entry.size != self.before.size
            {
                return Ok(Observation::Conflict);
            }
            let bytes = session
                .read_small_file_in_folder(&self.root.id, &self.before.id)
                .await
                .map_err(|_| MutationError::Uncertain)?;
            return if hex::encode(Sha256::digest(bytes)) == self.digest {
                Ok(Observation::AtSource)
            } else {
                Ok(Observation::Conflict)
            };
        }
        if source_file.is_empty() && destination_file.len() == 1 {
            let entry = destination_file[0];
            if entry.is_folder()
                || entry.parent_id != self.destination.id
                || entry.docwsid != self.before.id.rsplit("::").next().unwrap_or_default()
                || entry.display_name() != self.before.name
                || entry.etag.is_empty()
                || entry.size != self.before.size
            {
                return Ok(Observation::Conflict);
            }
            let bytes = session
                .read_small_file_in_folder(&self.destination.id, &self.before.id)
                .await
                .map_err(|_| MutationError::Uncertain)?;
            return if hex::encode(Sha256::digest(bytes)) == self.digest {
                Ok(Observation::AtDestination(Node {
                    id: entry.drivewsid.clone(),
                    parent_id: Some(self.destination.id.clone()),
                    name: self.before.name.clone(),
                    kind: NodeKind::File,
                    size: entry.size,
                    modified_unix: 0,
                    etag: Some(entry.etag.clone()),
                    content_version: self.before.content_version.clone(),
                    target: None,
                    package: false,
                }))
            } else {
                Ok(Observation::Conflict)
            };
        }
        Ok(Observation::Unknown)
    }
}

#[async_trait]
impl MutationProvider for ICloudOwnedMountedFileMove {
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
            Observation::AtSource => Ok(Some(self.before.id.clone())),
            Observation::AtDestination(_) | Observation::Conflict => Err(MutationError::Conflict),
            Observation::Unknown => Err(MutationError::Uncertain),
        }
    }

    async fn mutate(
        &self,
        _: &MutationRequest,
        _: &CancellationToken,
    ) -> MutationResult<MutationReceipt> {
        Err(MutationError::Unsupported(
            "move requires prepared identity",
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
        if prepared_item != Some(self.before.id.as_str()) {
            return Err(MutationError::Invalid);
        }
        if cancel.is_cancelled() {
            return Err(MutationError::Uncertain);
        }
        if !matches!(self.observe().await?, Observation::AtSource) {
            return Err(MutationError::Conflict);
        }
        let accepted = {
            let mut session = self.session.lock().await;
            session
                .send_move(
                    &self.before.id,
                    self.before.etag.as_deref().ok_or(MutationError::Invalid)?,
                    &self.destination.id,
                )
                .await
                .map_err(|_| MutationError::Uncertain)?
        };
        if self.discard_response.swap(false, Ordering::AcqRel) {
            return Err(MutationError::Uncertain);
        }
        if !accepted {
            return Err(MutationError::Uncertain);
        }
        match self.observe().await? {
            Observation::AtDestination(node) => Ok(MutationReceipt::Upsert(node)),
            Observation::Conflict => Err(MutationError::Conflict),
            Observation::AtSource | Observation::Unknown => Err(MutationError::Uncertain),
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
        match self.observe().await? {
            Observation::AtDestination(node) => Ok(MutationReconciliation::Applied(
                MutationReceipt::Upsert(node),
            )),
            Observation::Conflict => Ok(MutationReconciliation::Conflict),
            Observation::AtSource | Observation::Unknown => {
                Ok(MutationReconciliation::Indeterminate)
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mounted_move_requires_exact_prepared_identity() {
        let mut session = ICloudReadSession::new().unwrap();
        session.account_hash = Some("synthetic-account".into());
        let scope = Scope {
            account: Uuid::new_v4().to_string(),
            provider: "icloud".into(),
            collection: "drive".into(),
        };
        let root = ValidationFolder {
            id: format!("FOLDER::com.apple.CloudDocs::{}", Uuid::new_v4()),
            name: format!("Cirrove Write Validation-{}", Uuid::new_v4()),
        };
        let destination = Node {
            id: format!("FOLDER::com.apple.CloudDocs::{}", Uuid::new_v4()),
            parent_id: Some(root.id.clone()),
            name: "Mounted Folder".into(),
            kind: NodeKind::Folder,
            size: 0,
            modified_unix: 0,
            etag: Some("folder-etag".into()),
            content_version: None,
            target: None,
            package: false,
        };
        let before = Node {
            id: format!("FILE::com.apple.CloudDocs::{}", Uuid::new_v4()),
            parent_id: Some(root.id.clone()),
            name: "Mounted Create.txt".into(),
            kind: NodeKind::File,
            size: 4,
            modified_unix: 0,
            etag: Some("file-etag".into()),
            content_version: None,
            target: None,
            package: false,
        };
        let move_provider = ICloudOwnedMountedFileMove::new(
            scope.clone(),
            session,
            root,
            destination.clone(),
            before.clone(),
            hex::encode(Sha256::digest(b"test")),
        )
        .unwrap();
        let request = MutationRequest {
            scope,
            intent: MutationIntent::Relocate {
                before: before.clone(),
                parent: destination.id,
                name: before.name,
            },
        };
        assert!(matches!(
            move_provider
                .mutate_prepared(&request, None, &CancellationToken::new())
                .await,
            Err(MutationError::Invalid)
        ));
        let restarted = ICloudOwnedMountedFileMove::for_reconciliation(
            move_provider.scope.clone(),
            ICloudReadSession::new().unwrap(),
            move_provider.root.clone(),
            move_provider.destination.clone(),
            move_provider.before.clone(),
            move_provider.digest.clone(),
        );
        assert!(restarted.is_err(), "session identity must be present");
        let mut session = ICloudReadSession::new().unwrap();
        session.account_hash = Some("synthetic-account".into());
        let restarted = ICloudOwnedMountedFileMove::for_reconciliation(
            move_provider.scope.clone(),
            session,
            move_provider.root.clone(),
            move_provider.destination.clone(),
            move_provider.before.clone(),
            move_provider.digest.clone(),
        )
        .unwrap();
        assert!(matches!(
            restarted
                .prepare_mutation(&request, &CancellationToken::new())
                .await,
            Err(MutationError::Unsupported(_))
        ));
        assert!(matches!(
            restarted
                .mutate_prepared(
                    &request,
                    Some(&move_provider.before.id),
                    &CancellationToken::new(),
                )
                .await,
            Err(MutationError::Unsupported(_))
        ));
    }
}
