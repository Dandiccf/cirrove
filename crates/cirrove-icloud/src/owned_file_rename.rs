//! Feature-gated rename of one confirmed Cirrove-owned mounted test file.
//! Ordinary iCloud accounts never construct this mutation provider.
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

pub struct ICloudOwnedFixtureFileRename {
    scope: Scope,
    root: ValidationFolder,
    before: Node,
    target_name: String,
    digest: String,
    session: Mutex<ICloudReadSession>,
    discard_response: AtomicBool,
    reconciliation_only: bool,
}

impl ICloudOwnedFixtureFileRename {
    pub fn new(
        scope: Scope,
        session: ICloudReadSession,
        root: ValidationFolder,
        before: Node,
        target_name: String,
        digest: String,
    ) -> MutationResult<Self> {
        Self::build(scope, session, root, before, target_name, digest, false)
    }

    pub fn for_reconciliation(
        scope: Scope,
        session: ICloudReadSession,
        root: ValidationFolder,
        before: Node,
        target_name: String,
        digest: String,
    ) -> MutationResult<Self> {
        Self::build(scope, session, root, before, target_name, digest, true)
    }

    #[allow(clippy::too_many_arguments)]
    fn build(
        scope: Scope,
        session: ICloudReadSession,
        root: ValidationFolder,
        before: Node,
        target_name: String,
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
            || !before.id.starts_with("FILE::com.apple.CloudDocs::")
            || before.parent_id.as_deref() != Some(root.id.as_str())
            || before.name != "Mounted Create.txt"
            || target_name != "Mounted Renamed.txt"
            || before.kind != NodeKind::File
            || before.target.is_some()
            || before.package
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
            before,
            target_name,
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
                if before == &self.before && parent == &self.root.id && name == &self.target_name)
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
        let children = session
            .list_folder(&self.root.id)
            .await
            .map_err(|_| MutationError::Uncertain)?;
        let exact: Vec<_> = children
            .iter()
            .filter(|entry| entry.drivewsid == self.before.id)
            .collect();
        if exact.len() != 1 {
            return Ok(Observation::Unknown);
        }
        let entry = exact[0];
        if entry.is_folder()
            || entry.parent_id != self.root.id
            || entry.docwsid != self.before.id.rsplit("::").next().unwrap_or_default()
            || entry.size != self.before.size
            || children.iter().any(|other| {
                other.drivewsid != self.before.id && other.display_name() == self.target_name
            })
        {
            return Ok(Observation::Conflict);
        }
        let bytes = session
            .read_small_file_in_folder(&self.root.id, &self.before.id)
            .await
            .map_err(|_| MutationError::Uncertain)?;
        if bytes.len() as u64 != self.before.size
            || hex::encode(Sha256::digest(&bytes)) != self.digest
        {
            return Ok(Observation::Conflict);
        }
        if entry.display_name() == self.before.name {
            if Some(entry.etag.as_str()) != self.before.etag.as_deref() {
                return Ok(Observation::Conflict);
            }
            return Ok(Observation::AtSource);
        }
        if entry.display_name() == self.target_name && !entry.etag.is_empty() {
            return Ok(Observation::AtDestination(Node {
                id: entry.drivewsid.clone(),
                parent_id: Some(self.root.id.clone()),
                name: self.target_name.clone(),
                kind: NodeKind::File,
                size: entry.size,
                modified_unix: 0,
                etag: Some(entry.etag.clone()),
                content_version: self.before.content_version.clone(),
                target: None,
                package: false,
            }));
        }
        Ok(Observation::Conflict)
    }
}

#[async_trait]
impl MutationProvider for ICloudOwnedFixtureFileRename {
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
            "rename requires prepared identity",
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
                .send_rename(
                    &self.before.id,
                    self.before.etag.as_deref().ok_or(MutationError::Invalid)?,
                    &self.target_name,
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
    async fn file_rename_requires_prepared_identity_and_restart_never_resends() {
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
        let before = Node {
            id: format!("FILE::com.apple.CloudDocs::{}", Uuid::new_v4()),
            parent_id: Some(root.id.clone()),
            name: "Mounted Create.txt".into(),
            kind: NodeKind::File,
            size: 4,
            modified_unix: 0,
            etag: Some("current-etag".into()),
            content_version: None,
            target: None,
            package: false,
        };
        let digest = hex::encode(Sha256::digest(b"test"));
        let adapter = ICloudOwnedFixtureFileRename::new(
            scope.clone(),
            session,
            root.clone(),
            before.clone(),
            "Mounted Renamed.txt".into(),
            digest.clone(),
        )
        .unwrap();
        let request = MutationRequest {
            scope: scope.clone(),
            intent: MutationIntent::Relocate {
                before: before.clone(),
                parent: root.id.clone(),
                name: "Mounted Renamed.txt".into(),
            },
        };
        assert!(matches!(
            adapter
                .mutate_prepared(&request, None, &CancellationToken::new())
                .await,
            Err(MutationError::Invalid)
        ));
        let mut restarted_session = ICloudReadSession::new().unwrap();
        restarted_session.account_hash = Some("synthetic-account".into());
        let restarted = ICloudOwnedFixtureFileRename::for_reconciliation(
            scope,
            restarted_session,
            root,
            before.clone(),
            "Mounted Renamed.txt".into(),
            digest,
        )
        .unwrap();
        assert!(matches!(
            restarted
                .mutate_prepared(&request, Some(&before.id), &CancellationToken::new())
                .await,
            Err(MutationError::Unsupported(_))
        ));
    }
}
