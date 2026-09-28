//! Feature-gated, journal-backed move of one empty Cirrove-owned test folder.
//! Ordinary iCloud accounts do not construct this adapter.
use super::{ICloudReadSession, ValidationFolder};
use async_trait::async_trait;
use cirrove_core::mutation::{
    MutationError, MutationIntent, MutationProvider, MutationReceipt, MutationReconciliation,
    MutationRequest, Result as MutationResult,
};
use cirrove_core::{CancellationToken, Node, NodeKind, Scope};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Mutex;
use uuid::Uuid;

enum Observation {
    AtSource,
    AtDestination(Node),
    Conflict,
    Unknown,
}

pub struct ICloudOwnedFixtureFolderMove {
    scope: Scope,
    source: ValidationFolder,
    destination: ValidationFolder,
    before: Node,
    session: Mutex<ICloudReadSession>,
    discard_response: AtomicBool,
    reconciliation_only: bool,
}

impl ICloudOwnedFixtureFolderMove {
    pub fn new(
        scope: Scope,
        session: ICloudReadSession,
        source: ValidationFolder,
        destination: ValidationFolder,
        nested: ValidationFolder,
        etag: String,
    ) -> MutationResult<Self> {
        let before = Node {
            id: nested.id,
            parent_id: Some(source.id.clone()),
            name: nested.name,
            kind: NodeKind::Folder,
            size: 0,
            modified_unix: 0,
            etag: Some(etag),
            content_version: None,
            target: None,
            package: false,
        };
        Self::from_before(scope, session, source, destination, before, false)
    }

    pub fn for_reconciliation(
        scope: Scope,
        session: ICloudReadSession,
        source: ValidationFolder,
        destination: ValidationFolder,
        before: Node,
    ) -> MutationResult<Self> {
        Self::from_before(scope, session, source, destination, before, true)
    }

    fn from_before(
        scope: Scope,
        session: ICloudReadSession,
        source: ValidationFolder,
        destination: ValidationFolder,
        before: Node,
        reconciliation_only: bool,
    ) -> MutationResult<Self> {
        if scope.account.is_empty()
            || scope.provider != "icloud"
            || scope.collection != "drive"
            || session.account_hash.is_none()
            || source.id == destination.id
            || ![&source, &destination].iter().all(|folder| {
                folder.id.starts_with("FOLDER::com.apple.CloudDocs::")
                    && folder
                        .name
                        .strip_prefix("Cirrove Write Validation-")
                        .is_some_and(|suffix| Uuid::parse_str(suffix).is_ok())
            })
            || !before.id.starts_with("FOLDER::com.apple.CloudDocs::")
            || before.id == source.id
            || before.id == destination.id
            || before
                .name
                .strip_prefix("Cirrove Nested Move-")
                .is_none_or(|suffix| Uuid::parse_str(suffix).is_err())
            || before.parent_id.as_deref() != Some(source.id.as_str())
            || before.kind != NodeKind::Folder
            || before.target.is_some()
            || before.package
            || before.size != 0
            || before.etag.as_deref().is_none_or(str::is_empty)
        {
            return Err(MutationError::Invalid);
        }
        Ok(Self {
            scope,
            source,
            destination,
            before,
            session: Mutex::new(session),
            discard_response: AtomicBool::new(false),
            reconciliation_only,
        })
    }

    pub fn before_node(&self) -> Node {
        self.before.clone()
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
            || prepared.is_some_and(|item| item != self.before.id)
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
        for folder in [&self.source, &self.destination] {
            if root
                .iter()
                .filter(|entry| {
                    entry.drivewsid == folder.id
                        && entry.display_name() == folder.name
                        && entry.is_folder()
                })
                .count()
                != 1
            {
                return Ok(Observation::Unknown);
            }
        }
        let source = session
            .list_folder(&self.source.id)
            .await
            .map_err(|_| MutationError::Uncertain)?;
        let destination = session
            .list_folder(&self.destination.id)
            .await
            .map_err(|_| MutationError::Uncertain)?;
        let nested = session
            .list_folder(&self.before.id)
            .await
            .map_err(|_| MutationError::Uncertain)?;
        if !nested.is_empty() {
            return Ok(Observation::Conflict);
        }
        let at_source: Vec<_> = source
            .iter()
            .filter(|entry| entry.drivewsid == self.before.id)
            .collect();
        let at_destination: Vec<_> = destination
            .iter()
            .filter(|entry| entry.drivewsid == self.before.id)
            .collect();
        if at_source.len() > 1 || at_destination.len() > 1 {
            return Ok(Observation::Conflict);
        }
        if source.len() == 1 && destination.is_empty() && at_source.len() == 1 {
            let entry = at_source[0];
            if entry.kind != "FOLDER"
                || entry.display_name() != self.before.name
                || entry.parent_id != self.source.id
                || Some(entry.etag.as_str()) != self.before.etag.as_deref()
            {
                return Ok(Observation::Conflict);
            }
            return Ok(Observation::AtSource);
        }
        if source.is_empty() && destination.len() == 1 && at_destination.len() == 1 {
            let entry = at_destination[0];
            if entry.kind != "FOLDER"
                || entry.display_name() != self.before.name
                || entry.parent_id != self.destination.id
                || entry.etag.is_empty()
            {
                return Ok(Observation::Conflict);
            }
            return Ok(Observation::AtDestination(Node {
                id: entry.drivewsid.clone(),
                parent_id: Some(self.destination.id.clone()),
                name: entry.display_name(),
                kind: NodeKind::Folder,
                size: 0,
                modified_unix: 0,
                etag: Some(entry.etag.clone()),
                content_version: None,
                target: None,
                package: false,
            }));
        }
        if !source.is_empty() || !destination.is_empty() {
            Ok(Observation::Conflict)
        } else {
            Ok(Observation::Unknown)
        }
    }
}

#[async_trait]
impl MutationProvider for ICloudOwnedFixtureFolderMove {
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
            "owned folder move requires prepared identity",
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

    fn fixture() -> (ICloudOwnedFixtureFolderMove, MutationRequest) {
        let mut session = ICloudReadSession::new().unwrap();
        session.account_hash = Some("synthetic-account".into());
        let scope = Scope {
            account: Uuid::new_v4().to_string(),
            provider: "icloud".into(),
            collection: "drive".into(),
        };
        let source = ValidationFolder {
            id: format!("FOLDER::com.apple.CloudDocs::{}", Uuid::new_v4()),
            name: format!("Cirrove Write Validation-{}", Uuid::new_v4()),
        };
        let destination = ValidationFolder {
            id: format!("FOLDER::com.apple.CloudDocs::{}", Uuid::new_v4()),
            name: format!("Cirrove Write Validation-{}", Uuid::new_v4()),
        };
        let nested = ValidationFolder {
            id: format!("FOLDER::com.apple.CloudDocs::{}", Uuid::new_v4()),
            name: format!("Cirrove Nested Move-{}", Uuid::new_v4()),
        };
        let provider = ICloudOwnedFixtureFolderMove::new(
            scope.clone(),
            session,
            source,
            destination.clone(),
            nested,
            "fixture-etag".into(),
        )
        .unwrap();
        let request = MutationRequest {
            scope,
            intent: MutationIntent::Relocate {
                before: provider.before_node(),
                parent: destination.id,
                name: provider.before.name.clone(),
            },
        };
        (provider, request)
    }

    #[tokio::test]
    async fn only_the_exact_prepared_folder_can_reach_move_items() {
        let (provider, request) = fixture();
        assert!(matches!(
            provider
                .mutate_prepared(&request, None, &CancellationToken::new())
                .await,
            Err(MutationError::Invalid)
        ));
        assert!(matches!(
            provider
                .mutate_prepared(&request, Some("other"), &CancellationToken::new())
                .await,
            Err(MutationError::Invalid)
        ));
        let mut wrong = request.clone();
        if let MutationIntent::Relocate { name, .. } = &mut wrong.intent {
            *name = "different".into();
        }
        assert!(matches!(
            provider
                .prepare_mutation(&wrong, &CancellationToken::new())
                .await,
            Err(MutationError::Invalid)
        ));
    }

    #[tokio::test]
    async fn restarted_adapter_cannot_send_a_second_folder_move() {
        let (provider, request) = fixture();
        let mut session = ICloudReadSession::new().unwrap();
        session.account_hash = Some("synthetic-account".into());
        let restarted = ICloudOwnedFixtureFolderMove::for_reconciliation(
            request.scope.clone(),
            session,
            provider.source.clone(),
            provider.destination.clone(),
            provider.before_node(),
        )
        .unwrap();
        assert!(matches!(
            restarted
                .mutate_prepared(
                    &request,
                    Some(&provider.before.id),
                    &CancellationToken::new()
                )
                .await,
            Err(MutationError::Unsupported(_))
        ));
    }
}
