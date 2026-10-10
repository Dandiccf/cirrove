//! Feature-gated, journal-backed move of one Cirrove-owned test folder.
//! Ordinary iCloud accounts do not construct this adapter.
use super::{ICloudReadSession, ValidationFile, ValidationFolder};
use async_trait::async_trait;
use cirrove_core::mutation::{
    MutationError, MutationIntent, MutationProvider, MutationReceipt, MutationReconciliation,
    MutationRequest, Result as MutationResult,
};
use cirrove_core::{CancellationToken, Node, NodeKind, Scope};
use serde::{Deserialize, Serialize};
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

// Persisted in the validation-only folder's `before` node, so a fresh worker
// cannot reconcile a different child after losing the move response.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct OwnedChild {
    id: String,
    document_id: String,
    etag: String,
    name: String,
    size: u64,
    sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct OwnedNestedChild {
    id: String,
    name: String,
    etag: String,
    file: OwnedChild,
}

const CHILD_PREFIX: &str = "cirrove-owned-child-v1:";
const NESTED_CHILD_PREFIX: &str = "cirrove-owned-nested-child-v1:";

pub struct ICloudOwnedFixtureFolderMove {
    scope: Scope,
    source: ValidationFolder,
    destination: ValidationFolder,
    before: Node,
    child: Option<OwnedChild>,
    nested_child: Option<OwnedNestedChild>,
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

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_child(
        scope: Scope,
        session: ICloudReadSession,
        source: ValidationFolder,
        destination: ValidationFolder,
        nested: ValidationFolder,
        etag: String,
        file: &ValidationFile,
        bytes: &[u8],
    ) -> MutationResult<Self> {
        if bytes.is_empty() || bytes.len() > 4096 {
            return Err(MutationError::Invalid);
        }
        let child = OwnedChild {
            id: file.id.clone(),
            document_id: file.document_id.clone(),
            etag: file.etag.clone(),
            name: file.name.clone(),
            size: bytes.len() as u64,
            sha256: hex::encode(Sha256::digest(bytes)),
        };
        let mut adapter = Self::new(scope, session, source, destination, nested, etag)?;
        adapter.before.content_version = Some(format!(
            "{CHILD_PREFIX}{}",
            serde_json::to_string(&child).map_err(|_| MutationError::Invalid)?
        ));
        adapter.child = Some(child);
        Ok(adapter)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_grandchild(
        scope: Scope,
        session: ICloudReadSession,
        source: ValidationFolder,
        destination: ValidationFolder,
        outer: ValidationFolder,
        outer_etag: String,
        inner: &ValidationFolder,
        inner_etag: &str,
        file: &ValidationFile,
        bytes: &[u8],
    ) -> MutationResult<Self> {
        if bytes.is_empty() || bytes.len() > 4096 || inner_etag.is_empty() {
            return Err(MutationError::Invalid);
        }
        let nested = OwnedNestedChild {
            id: inner.id.clone(),
            name: inner.name.clone(),
            etag: inner_etag.to_owned(),
            file: OwnedChild {
                id: file.id.clone(),
                document_id: file.document_id.clone(),
                etag: file.etag.clone(),
                name: file.name.clone(),
                size: bytes.len() as u64,
                sha256: hex::encode(Sha256::digest(bytes)),
            },
        };
        let mut adapter = Self::new(scope, session, source, destination, outer, outer_etag)?;
        adapter.before.content_version = Some(format!(
            "{NESTED_CHILD_PREFIX}{}",
            serde_json::to_string(&nested).map_err(|_| MutationError::Invalid)?
        ));
        adapter.nested_child = Some(nested);
        Ok(adapter)
    }

    fn from_before(
        scope: Scope,
        session: ICloudReadSession,
        source: ValidationFolder,
        destination: ValidationFolder,
        before: Node,
        reconciliation_only: bool,
    ) -> MutationResult<Self> {
        let (child, nested_child) = match before.content_version.as_deref() {
            None => (None, None),
            Some(encoded) if encoded.starts_with(CHILD_PREFIX) => {
                let payload = encoded
                    .strip_prefix(CHILD_PREFIX)
                    .ok_or(MutationError::Invalid)?;
                let child: OwnedChild =
                    serde_json::from_str(payload).map_err(|_| MutationError::Invalid)?;
                if !Self::valid_child(&child)
                    || serde_json::to_string(&child).map_err(|_| MutationError::Invalid)? != payload
                {
                    return Err(MutationError::Invalid);
                }
                (Some(child), None)
            }
            Some(encoded) if encoded.starts_with(NESTED_CHILD_PREFIX) => {
                let payload = encoded
                    .strip_prefix(NESTED_CHILD_PREFIX)
                    .ok_or(MutationError::Invalid)?;
                let nested: OwnedNestedChild =
                    serde_json::from_str(payload).map_err(|_| MutationError::Invalid)?;
                if !nested.id.starts_with("FOLDER::com.apple.CloudDocs::")
                    || nested
                        .name
                        .strip_prefix("Cirrove Nested Move-")
                        .is_none_or(|suffix| Uuid::parse_str(suffix).is_err())
                    || nested.etag.is_empty()
                    || !Self::valid_child(&nested.file)
                    || serde_json::to_string(&nested).map_err(|_| MutationError::Invalid)?
                        != payload
                {
                    return Err(MutationError::Invalid);
                }
                (None, Some(nested))
            }
            Some(_) => return Err(MutationError::Invalid),
        };
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
            child,
            nested_child,
            session: Mutex::new(session),
            discard_response: AtomicBool::new(false),
            reconciliation_only,
        })
    }

    fn valid_child(child: &OwnedChild) -> bool {
        !child.id.is_empty()
            && !child.document_id.is_empty()
            && !child.etag.is_empty()
            && child.name == "created-by-cirrove.txt"
            && child.size > 0
            && child.size <= 4096
            && child.sha256.len() == 64
            && child.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    }

    pub fn before_node(&self) -> Node {
        self.before.clone()
    }

    pub fn matches_child_record(
        &self,
        id: &str,
        document_id: &str,
        etag: &str,
        size: u64,
        sha256: &str,
    ) -> bool {
        self.child.as_ref().is_some_and(|child| {
            child.id == id
                && child.document_id == document_id
                && child.etag == etag
                && child.size == size
                && child.sha256 == sha256
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn matches_nested_record(
        &self,
        inner_id: &str,
        inner_name: &str,
        inner_etag: &str,
        file_id: &str,
        document_id: &str,
        etag: &str,
        size: u64,
        sha256: &str,
    ) -> bool {
        self.nested_child.as_ref().is_some_and(|nested| {
            nested.id == inner_id
                && nested.name == inner_name
                && nested.etag == inner_etag
                && nested.file.id == file_id
                && nested.file.document_id == document_id
                && nested.file.etag == etag
                && nested.file.size == size
                && nested.file.sha256 == sha256
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
        let mut grandchild_etag_unchanged = true;
        if let Some(inner) = &self.nested_child {
            if nested.len() != 1
                || nested[0].drivewsid != inner.id
                || nested[0].display_name() != inner.name
                || nested[0].parent_id != self.before.id
                || !nested[0].is_folder()
            {
                return Ok(Observation::Conflict);
            }
            let children = session
                .list_folder(&inner.id)
                .await
                .map_err(|_| MutationError::Uncertain)?;
            let file = &inner.file;
            if children.len() != 1
                || children[0].drivewsid != file.id
                || children[0].docwsid != file.document_id
                || children[0].display_name() != file.name
                || children[0].parent_id != inner.id
                || children[0].is_folder()
                || children[0].size != file.size
            {
                return Ok(Observation::Conflict);
            }
            grandchild_etag_unchanged = children[0].etag == file.etag;
            let bytes = session
                .read_small_file_in_folder(&inner.id, &file.id)
                .await
                .map_err(|_| MutationError::Uncertain)?;
            if bytes.len() as u64 != file.size || hex::encode(Sha256::digest(&bytes)) != file.sha256
            {
                return Ok(Observation::Conflict);
            }
        }
        match &self.child {
            None if self.nested_child.is_none() && !nested.is_empty() => {
                return Ok(Observation::Conflict);
            }
            Some(child) => {
                if nested.len() != 1
                    || nested[0].drivewsid != child.id
                    || nested[0].docwsid != child.document_id
                    || nested[0].display_name() != child.name
                    || nested[0].parent_id != self.before.id
                    || nested[0].is_folder()
                    || nested[0].size != child.size
                {
                    return Ok(Observation::Conflict);
                }
                let bytes = session
                    .read_small_file_in_folder(&self.before.id, &child.id)
                    .await
                    .map_err(|_| MutationError::Uncertain)?;
                if bytes.len() as u64 != child.size
                    || hex::encode(Sha256::digest(&bytes)) != child.sha256
                {
                    return Ok(Observation::Conflict);
                }
            }
            None => {}
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
                || self
                    .child
                    .as_ref()
                    .is_some_and(|child| nested[0].etag != child.etag)
                || self.nested_child.as_ref().is_some_and(|inner| {
                    nested[0].etag != inner.etag
                        || inner.id == self.before.id
                        || inner.file.id == self.before.id
                })
                || !grandchild_etag_unchanged
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

    #[tokio::test]
    async fn populated_folder_manifest_is_bound_to_journal_and_restart_is_read_only() {
        let (empty, _) = fixture();
        let mut session = ICloudReadSession::new().unwrap();
        session.account_hash = Some("synthetic-account".into());
        let file = ValidationFile {
            id: format!("FILE::com.apple.CloudDocs::{}", Uuid::new_v4()),
            document_id: format!("doc-{}", Uuid::new_v4()),
            etag: "file-etag".into(),
            name: "created-by-cirrove.txt".into(),
        };
        let provider = ICloudOwnedFixtureFolderMove::new_with_child(
            empty.scope.clone(),
            session,
            empty.source.clone(),
            empty.destination.clone(),
            ValidationFolder {
                id: empty.before.id.clone(),
                name: empty.before.name.clone(),
            },
            empty.before.etag.clone().unwrap(),
            &file,
            b"test bytes",
        )
        .unwrap();
        let request = MutationRequest {
            scope: provider.scope.clone(),
            intent: MutationIntent::Relocate {
                before: provider.before_node(),
                parent: provider.destination.id.clone(),
                name: provider.before.name.clone(),
            },
        };
        let mut changed = request.clone();
        if let MutationIntent::Relocate { before, .. } = &mut changed.intent {
            before.content_version = Some("cirrove-owned-child-v1:{}".into());
        }
        assert!(matches!(
            provider
                .prepare_mutation(&changed, &CancellationToken::new())
                .await,
            Err(MutationError::Invalid)
        ));
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
        assert_eq!(restarted.child, provider.child);
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

    #[tokio::test]
    async fn two_level_manifest_survives_restart_without_enabling_a_second_move() {
        let (empty, _) = fixture();
        let mut session = ICloudReadSession::new().unwrap();
        session.account_hash = Some("synthetic-account".into());
        let inner = ValidationFolder {
            id: format!("FOLDER::com.apple.CloudDocs::{}", Uuid::new_v4()),
            name: format!("Cirrove Nested Move-{}", Uuid::new_v4()),
        };
        let file = ValidationFile {
            id: format!("FILE::com.apple.CloudDocs::{}", Uuid::new_v4()),
            document_id: format!("doc-{}", Uuid::new_v4()),
            etag: "file-etag".into(),
            name: "created-by-cirrove.txt".into(),
        };
        let provider = ICloudOwnedFixtureFolderMove::new_with_grandchild(
            empty.scope.clone(),
            session,
            empty.source.clone(),
            empty.destination.clone(),
            ValidationFolder {
                id: empty.before.id.clone(),
                name: empty.before.name.clone(),
            },
            empty.before.etag.clone().unwrap(),
            &inner,
            "inner-etag",
            &file,
            b"test bytes",
        )
        .unwrap();
        let request = MutationRequest {
            scope: provider.scope.clone(),
            intent: MutationIntent::Relocate {
                before: provider.before_node(),
                parent: provider.destination.id.clone(),
                name: provider.before.name.clone(),
            },
        };
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
        assert_eq!(restarted.nested_child, provider.nested_child);
        assert!(!restarted.matches_nested_record(
            &inner.id,
            &inner.name,
            "wrong-etag",
            &file.id,
            &file.document_id,
            &file.etag,
            10,
            &hex::encode(Sha256::digest(b"test bytes")),
        ));
        assert!(matches!(
            restarted
                .mutate_prepared(
                    &request,
                    Some(&provider.before.id),
                    &CancellationToken::new(),
                )
                .await,
            Err(MutationError::Unsupported(_))
        ));
    }
}
