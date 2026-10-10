//! Feature-gated, conditional move of one Cirrove-owned validation file.
//! The ordinary iCloud account never constructs this mutation provider.
use super::{DriveEntry, ICloudReadSession, ValidationFile, ValidationFolder};
use async_trait::async_trait;
use cirrove_core::mutation::{
    MutationError, MutationIntent, MutationProvider, MutationReceipt, MutationReconciliation,
    MutationRequest, Result as MutationResult,
};
use cirrove_core::{CancellationToken, Node, NodeKind, Scope};
use sha2::{Digest, Sha256};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{Mutex, Notify};
use uuid::Uuid;

enum Observation {
    SourceIntact,
    Moved(Node),
    Conflict,
    Unknown,
}

/// A test-only pause after the worker's final free-destination check.
/// It lets another owned fixture occupy the name before the move request.
#[derive(Default)]
pub struct ICloudOwnedMovePause {
    ready: Notify,
    proceed: Notify,
}

impl ICloudOwnedMovePause {
    pub async fn reached(&self) {
        self.ready.notified().await;
    }

    pub fn resume(&self) {
        self.proceed.notify_one();
    }
}

pub struct ICloudOwnedFixtureMove {
    scope: Scope,
    source: ValidationFolder,
    destination: ValidationFolder,
    before: Node,
    digest: String,
    session: Mutex<ICloudReadSession>,
    reconciliation_only: bool,
    discard_response: AtomicBool,
    before_send_pause: Option<Arc<ICloudOwnedMovePause>>,
}

impl ICloudOwnedFixtureMove {
    pub fn new(
        scope: Scope,
        session: ICloudReadSession,
        source: ValidationFolder,
        destination: ValidationFolder,
        file: ValidationFile,
        bytes: &[u8],
    ) -> MutationResult<Self> {
        if bytes.is_empty() || bytes.len() > 4096 || file.name != "created-by-cirrove.txt" {
            return Err(MutationError::Invalid);
        }
        let digest = hex::encode(Sha256::digest(bytes));
        let before = Node {
            id: file.id,
            parent_id: Some(source.id.clone()),
            name: file.name,
            kind: NodeKind::File,
            size: bytes.len() as u64,
            modified_unix: 0,
            etag: Some(file.etag),
            content_version: Some(format!("sha256:{digest}")),
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
        let digest = before
            .content_version
            .as_deref()
            .and_then(|version| version.strip_prefix("sha256:"))
            .ok_or(MutationError::Invalid)?
            .to_owned();
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
            || !before.id.starts_with("FILE::com.apple.CloudDocs::")
            || before.name != "created-by-cirrove.txt"
            || before.parent_id.as_deref() != Some(source.id.as_str())
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
            source,
            destination,
            before,
            digest: digest.to_ascii_lowercase(),
            session: Mutex::new(session),
            reconciliation_only,
            discard_response: AtomicBool::new(false),
            before_send_pause: None,
        })
    }

    pub fn before_node(&self) -> Node {
        self.before.clone()
    }

    pub fn with_discarded_response(self) -> Self {
        self.discard_response.store(true, Ordering::Release);
        self
    }

    pub fn with_pause_before_send(mut self, pause: Arc<ICloudOwnedMovePause>) -> Self {
        self.before_send_pause = Some(pause);
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

    async fn observed_bytes_match(
        session: &mut ICloudReadSession,
        parent: &str,
        item: &DriveEntry,
        digest: &str,
    ) -> MutationResult<bool> {
        let bytes = session
            .read_small_file_in_folder(parent, &item.drivewsid)
            .await
            .map_err(|_| MutationError::Uncertain)?;
        Ok(hex::encode(Sha256::digest(bytes)) == digest)
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
        let source_matches: Vec<_> = source
            .iter()
            .filter(|item| item.drivewsid == self.before.id)
            .collect();
        let destination_matches: Vec<_> = destination
            .iter()
            .filter(|item| item.drivewsid == self.before.id)
            .collect();
        if source_matches.len() > 1 || destination_matches.len() > 1 {
            return Ok(Observation::Conflict);
        }
        if source.len() == 1 && destination.is_empty() && source_matches.len() == 1 {
            let item = source_matches[0];
            if item.is_folder()
                || item.docwsid != self.before.id.rsplit("::").next().unwrap_or_default()
                || item.display_name() != self.before.name
                || Some(item.etag.as_str()) != self.before.etag.as_deref()
                || item.size != self.before.size
                || !Self::observed_bytes_match(&mut session, &self.source.id, item, &self.digest)
                    .await?
            {
                return Ok(Observation::Conflict);
            }
            return Ok(Observation::SourceIntact);
        }
        if source.is_empty() && destination.len() == 1 && destination_matches.len() == 1 {
            let item = destination_matches[0];
            if item.is_folder()
                || item.docwsid != self.before.id.rsplit("::").next().unwrap_or_default()
                || item.display_name() != self.before.name
                || item.etag.is_empty()
                || item.size != self.before.size
                || !Self::observed_bytes_match(
                    &mut session,
                    &self.destination.id,
                    item,
                    &self.digest,
                )
                .await?
            {
                return Ok(Observation::Conflict);
            }
            return Ok(Observation::Moved(Node {
                id: item.drivewsid.clone(),
                parent_id: Some(self.destination.id.clone()),
                name: item.display_name(),
                kind: NodeKind::File,
                size: item.size,
                modified_unix: 0,
                etag: Some(item.etag.clone()),
                content_version: self.before.content_version.clone(),
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
impl MutationProvider for ICloudOwnedFixtureMove {
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
            Observation::SourceIntact => Ok(Some(self.before.id.clone())),
            Observation::Moved(_) | Observation::Conflict => Err(MutationError::Conflict),
            Observation::Unknown => Err(MutationError::Uncertain),
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
        if prepared_item != Some(self.before.id.as_str()) {
            return Err(MutationError::Invalid);
        }
        if cancel.is_cancelled() {
            return Err(MutationError::Uncertain);
        }
        if !matches!(self.observe().await?, Observation::SourceIntact) {
            return Err(MutationError::Conflict);
        }
        if let Some(pause) = &self.before_send_pause {
            pause.ready.notify_one();
            pause.proceed.notified().await;
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
            Observation::Moved(node) => Ok(MutationReceipt::Upsert(node)),
            Observation::Conflict => Err(MutationError::Conflict),
            Observation::SourceIntact | Observation::Unknown => Err(MutationError::Uncertain),
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
            Observation::Moved(node) => Ok(MutationReconciliation::Applied(
                MutationReceipt::Upsert(node),
            )),
            Observation::Conflict => Ok(MutationReconciliation::Conflict),
            Observation::SourceIntact | Observation::Unknown => {
                Ok(MutationReconciliation::Indeterminate)
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn fixture() -> (ICloudOwnedFixtureMove, MutationRequest) {
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
        let document = Uuid::new_v4().to_string();
        let file = ValidationFile {
            id: format!("FILE::com.apple.CloudDocs::{document}"),
            document_id: document,
            etag: "fixture-etag".into(),
            name: "created-by-cirrove.txt".into(),
        };
        let provider = ICloudOwnedFixtureMove::new(
            scope.clone(),
            session,
            source,
            destination.clone(),
            file,
            b"owned fixture",
        )
        .unwrap();
        let request = MutationRequest {
            scope,
            intent: MutationIntent::Relocate {
                before: provider.before_node(),
                parent: destination.id,
                name: "created-by-cirrove.txt".into(),
            },
        };
        (provider, request)
    }

    #[tokio::test]
    async fn only_the_exact_prepared_move_can_reach_network() {
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
        let mut other = request.clone();
        other.scope.account = Uuid::new_v4().to_string();
        assert!(matches!(
            provider
                .prepare_mutation(&other, &CancellationToken::new())
                .await,
            Err(MutationError::Invalid)
        ));
        let mut other = request.clone();
        if let MutationIntent::Relocate { name, .. } = &mut other.intent {
            *name = "different.txt".into();
        }
        assert!(matches!(
            provider
                .prepare_mutation(&other, &CancellationToken::new())
                .await,
            Err(MutationError::Invalid)
        ));
    }

    #[tokio::test]
    async fn restarted_adapter_cannot_issue_another_move() {
        let (provider, request) = fixture();
        let mut session = ICloudReadSession::new().unwrap();
        session.account_hash = Some("synthetic-account".into());
        let restarted = ICloudOwnedFixtureMove::for_reconciliation(
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
