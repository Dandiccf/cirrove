//! Durable three-step relocation when both parent and name change.
//! A reserved temporary name avoids collisions with the source name at the
//! destination and the final name at the source. Sent phases authorize reads,
//! never a second mutation dispatch after an uncertain response.
use super::*;

pub(super) fn combined(request: &MutationRequest) -> bool {
    matches!(&request.intent, MutationIntent::Relocate { before, parent, name }
        if before.parent_id.as_deref() != Some(parent) && &before.name != name)
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Ready,
    Sent,
    Complete,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Relocation {
    version: u8,
    operation: Uuid,
    request: MutationRequest,
    destination: Node,
    current: Node,
    original_sha256: Option<String>,
    step: u8,
    phase: Phase,
}
impl Relocation {
    fn temporary(&self) -> String {
        format!(".cirrove-move-{}", self.operation)
    }
    fn original(&self) -> MutationResult<&Node> {
        self.request.intent.before().ok_or(MutationError::Invalid)
    }
    fn target(&self) -> MutationResult<(&str, &str)> {
        match &self.request.intent {
            MutationIntent::Relocate { parent, name, .. } => Ok((parent, name)),
            _ => Err(MutationError::Invalid),
        }
    }
    fn child(&self) -> MutationResult<FolderPlan> {
        let (parent, name) = match self.step {
            0 => (
                self.current
                    .parent_id
                    .clone()
                    .ok_or(MutationError::Invalid)?,
                self.temporary(),
            ),
            1 => (self.destination.id.clone(), self.temporary()),
            2 => (self.destination.id.clone(), self.target()?.1.to_owned()),
            _ => return Err(MutationError::Invalid),
        };
        Ok(FolderPlan {
            version: 1,
            request: MutationRequest {
                scope: self.request.scope.clone(),
                intent: MutationIntent::Relocate {
                    before: self.current.clone(),
                    parent,
                    name,
                },
            },
            destination: (self.step == 1).then(|| self.destination.clone()),
            original_sha256: self.original_sha256.clone(),
        })
    }
    fn validate(&self) -> MutationResult<()> {
        self.request.validate()?;
        let before = self.original()?;
        let (parent, name) = self.target()?;
        if self.version != 1
            || !combined(&self.request)
            || self.destination.id != parent
            || self.step > 3
            || (self.phase == Phase::Complete) != (self.step == 3)
            || self.current.id != before.id
            || self.current.kind != before.kind
            || (before.kind == NodeKind::File
                && (self.current.size != before.size
                    || self.current.content_version != before.content_version))
            || self.current.package
            || self.current.target.is_some()
            || self.current.etag.as_deref().is_none_or(|etag| {
                etag.is_empty() || etag.len() > 4096 || etag.contains(['\r', '\n', '*'])
            })
            || self.temporary() == before.name
            || self.temporary() == name
        {
            return Err(MutationError::Invalid);
        }
        let expected_parent = if self.step < 2 {
            before.parent_id.as_deref()
        } else {
            Some(parent)
        };
        let expected_name = match self.step {
            0 => before.name.clone(),
            1 | 2 => self.temporary(),
            3 => name.into(),
            _ => return Err(MutationError::Invalid),
        };
        if self.current.parent_id.as_deref() != expected_parent
            || self.current.name != expected_name
            || (self.step == 0 && self.current != *before)
        {
            return Err(MutationError::Invalid);
        }
        Ok(())
    }
    fn advance(&mut self, receipt: MutationReceipt) -> MutationResult<()> {
        self.validate()?;
        if self.phase != Phase::Sent || !self.child()?.request.accepts(&receipt) {
            return Err(MutationError::Invalid);
        }
        let MutationReceipt::Upsert(current) = receipt else {
            return Err(MutationError::Invalid);
        };
        let mut next = self.clone();
        next.current = current;
        next.step += 1;
        next.phase = if next.step == 3 {
            Phase::Complete
        } else {
            Phase::Ready
        };
        next.validate()?;
        *self = next;
        Ok(())
    }
}

impl ICloudWriteProvider {
    fn validate_relocation(&self, saved: &Relocation) -> MutationResult<()> {
        saved.validate()?;
        if saved.request.scope != self.scope {
            return Err(MutationError::Invalid);
        }
        // Validate every child shape, including the eventual final name, before
        // the first mutation. Actual steps retain their captured ETags.
        for step in 0..3 {
            let mut candidate = saved.clone();
            candidate.current = saved.original()?.clone();
            candidate.step = step;
            candidate.phase = Phase::Ready;
            if step > 0 {
                candidate.current.name = saved.temporary();
            }
            if step > 1 {
                candidate.current.parent_id = Some(saved.destination.id.clone());
            }
            self.folder_adapter(&candidate.child()?)?;
        }
        Ok(())
    }
    async fn store_relocation(&self, saved: &Relocation) -> MutationResult<()> {
        self.validate_relocation(saved)?;
        let value = serde_json::to_string(saved).map_err(|_| MutationError::Invalid)?;
        if value.len() > 32 * 1024 {
            return Err(MutationError::Invalid);
        }
        self.folder_vault
            .save(&self.plan_key(saved.operation), SecretString::from(value))
            .await
            .map_err(|_| MutationError::Uncertain)
    }
    async fn load_relocation(
        &self,
        operation: Uuid,
        request: &MutationRequest,
        prepared: Option<&str>,
    ) -> MutationResult<Option<Relocation>> {
        let Some(value) = self
            .folder_vault
            .load(&self.plan_key(operation))
            .await
            .map_err(|_| MutationError::Uncertain)?
        else {
            return Ok(None);
        };
        if value.expose_secret().len() > 32 * 1024 {
            return Err(MutationError::Invalid);
        }
        let saved: Relocation =
            serde_json::from_str(value.expose_secret()).map_err(|_| MutationError::Invalid)?;
        if saved.operation != operation
            || saved.request != *request
            || (prepared != Some(saved.original()?.id.as_str())
                && !(prepared.is_none() && saved.step == 0 && saved.phase == Phase::Ready))
        {
            return Err(MutationError::Invalid);
        }
        self.validate_relocation(&saved)?;
        Ok(Some(saved))
    }
    async fn relocation_destination_free(
        &self,
        saved: &Relocation,
        cancel: &CancellationToken,
    ) -> MutationResult<()> {
        let target = saved.target()?.1.to_owned();
        let temporary = saved.temporary();
        tokio::select! { biased;
            _=cancel.cancelled()=>Err(MutationError::Uncertain),
            result=async {
                let vault=SealedSessionVault::new(&self.state,&self.scope.account).map_err(|_|MutationError::Invalid)?;
                let snapshot=vault.load(&self.credential_id).await.map_err(|_|MutationError::Uncertain)?.ok_or(MutationError::Uncertain)?;
                let mut session=ICloudReadSession::from_session_snapshot(&snapshot,&self.apple_id).map_err(|_|MutationError::Uncertain)?;
                let children=session.list_folder(&saved.destination.id).await.map_err(|_|MutationError::Uncertain)?;
                if children.iter().any(|item| { let name=item.display_name(); name==target || name==temporary }) { return Err(MutationError::Conflict); }
                Ok(())
            }=>result,
        }
    }
    pub(super) async fn prepare_combined(
        &self,
        operation: &str,
        request: &MutationRequest,
        cancel: &CancellationToken,
    ) -> MutationResult<Option<String>> {
        let operation = self.mutation_operation(operation, request)?;
        let saved = match self.load_relocation(operation, request, None).await? {
            Some(saved) => saved,
            None => {
                let MutationIntent::Relocate { before, parent, .. } = &request.intent else {
                    return Err(MutationError::Invalid);
                };
                let destination = self
                    .parent_excluding(parent, Some(before.id.clone()))
                    .await
                    .map_err(local_error)?;
                let mut saved = Relocation {
                    version: 1,
                    operation,
                    request: request.clone(),
                    destination,
                    current: before.clone(),
                    original_sha256: None,
                    step: 0,
                    phase: Phase::Ready,
                };
                // Reuse exact scoped metadata validation before capturing bytes.
                self.folder_plan(&saved.child()?.request).await?;
                if before.kind == NodeKind::File {
                    saved.original_sha256 =
                        Some(self.capture_original_digest(before, cancel).await?);
                }
                self.store_relocation(&saved).await?;
                saved
            }
        };
        if saved.phase != Phase::Ready || saved.step != 0 {
            return Err(MutationError::Uncertain);
        }
        self.relocation_destination_free(&saved, cancel).await?;
        let child = saved.child()?;
        let prepared = self
            .folder_adapter(&child)?
            .prepare_mutation(&child.request, cancel)
            .await?;
        if prepared.as_deref() != Some(saved.original()?.id.as_str()) {
            return Err(MutationError::Invalid);
        }
        Ok(prepared)
    }
    async fn apply_relocation_step(
        &self,
        saved: &mut Relocation,
        adapter: Arc<dyn MutationProvider>,
        cancel: &CancellationToken,
    ) -> MutationResult<()> {
        if saved.phase != Phase::Ready {
            return Err(MutationError::Uncertain);
        }
        let child = saved.child()?;
        let prepared = adapter.prepare_mutation(&child.request, cancel).await?;
        if prepared.as_deref() != Some(saved.current.id.as_str()) {
            return Err(MutationError::Invalid);
        }
        if cancel.is_cancelled() {
            return Err(MutationError::Uncertain);
        }
        saved.phase = Phase::Sent;
        self.store_relocation(saved).await?;
        let receipt = adapter
            .mutate_prepared(&child.request, prepared.as_deref(), cancel)
            .await?;
        saved.advance(receipt)?;
        self.store_relocation(saved).await
    }
    pub(super) async fn mutate_combined(
        &self,
        operation: &str,
        request: &MutationRequest,
        prepared: Option<&str>,
        cancel: &CancellationToken,
    ) -> MutationResult<MutationReceipt> {
        let operation = self.mutation_operation(operation, request)?;
        let mut saved = self
            .load_relocation(operation, request, prepared)
            .await?
            .ok_or(MutationError::Uncertain)?;
        if prepared != Some(saved.original()?.id.as_str()) || saved.phase == Phase::Sent {
            return Err(MutationError::Uncertain);
        }
        if saved.step == 0 {
            self.relocation_destination_free(&saved, cancel).await?;
        }
        while saved.phase != Phase::Complete {
            let adapter = self.folder_adapter(&saved.child()?)?;
            self.apply_relocation_step(&mut saved, adapter, cancel)
                .await?;
        }
        Ok(MutationReceipt::Upsert(saved.current))
    }
    async fn inspect_relocation_step(
        &self,
        saved: &mut Relocation,
        adapter: Arc<dyn MutationProvider>,
        cancel: &CancellationToken,
    ) -> MutationResult<MutationReconciliation> {
        if saved.phase != Phase::Sent {
            return Err(MutationError::Invalid);
        }
        let child = saved.child()?;
        match adapter
            .reconcile_prepared_mutation(&child.request, Some(&saved.current.id), cancel)
            .await?
        {
            MutationReconciliation::Applied(receipt) => {
                saved.advance(receipt)?;
                self.store_relocation(saved).await?;
                if saved.phase == Phase::Complete {
                    Ok(MutationReconciliation::Applied(MutationReceipt::Upsert(
                        saved.current.clone(),
                    )))
                } else {
                    Ok(MutationReconciliation::Uncommitted)
                }
            }
            // A child that cannot prove dispatch absence must never unlock a replay.
            MutationReconciliation::Uncommitted => Ok(MutationReconciliation::Indeterminate),
            other => Ok(other),
        }
    }
    pub(super) async fn reconcile_combined(
        &self,
        operation: &str,
        request: &MutationRequest,
        prepared: Option<&str>,
        cancel: &CancellationToken,
    ) -> MutationResult<MutationReconciliation> {
        let operation = self.mutation_operation(operation, request)?;
        let Some(mut saved) = self.load_relocation(operation, request, prepared).await? else {
            return Ok(MutationReconciliation::Indeterminate);
        };
        match saved.phase {
            Phase::Ready => Ok(MutationReconciliation::Uncommitted),
            Phase::Complete => Ok(MutationReconciliation::Applied(MutationReceipt::Upsert(
                saved.current,
            ))),
            Phase::Sent => {
                let adapter = self.folder_adapter(&saved.child()?)?;
                self.inspect_relocation_step(&mut saved, adapter, cancel)
                    .await
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::icloud_writes::tests::{fixture, folder};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Child {
        vault: Arc<dyn CredentialVault>,
        key: String,
        request: MutationRequest,
        receipt: Node,
        lost: bool,
        mutations: AtomicUsize,
        inspections: AtomicUsize,
    }
    impl Child {
        fn new(p: &ICloudWriteProvider, saved: &Relocation, lost: bool) -> Self {
            let request = saved.child().unwrap().request;
            let MutationIntent::Relocate {
                before,
                parent,
                name,
            } = &request.intent
            else {
                panic!("relocate");
            };
            let receipt = Node {
                parent_id: Some(parent.clone()),
                name: name.clone(),
                etag: Some(format!("step-{}", saved.step)),
                ..before.clone()
            };
            Self {
                vault: p.folder_vault.clone(),
                key: p.plan_key(saved.operation),
                request,
                receipt,
                lost,
                mutations: AtomicUsize::new(0),
                inspections: AtomicUsize::new(0),
            }
        }
        async fn sent(&self) {
            let secret = self.vault.load(&self.key).await.unwrap().unwrap();
            let saved: Relocation = serde_json::from_str(secret.expose_secret()).unwrap();
            assert!(
                saved.phase == Phase::Sent,
                "dispatch must follow durable sent marker"
            );
            assert!(saved.child().unwrap().request == self.request);
        }
    }
    #[async_trait]
    impl MutationProvider for Child {
        async fn prepare_mutation(
            &self,
            r: &MutationRequest,
            _: &CancellationToken,
        ) -> MutationResult<Option<String>> {
            assert!(*r == self.request);
            Ok(r.intent.before().map(|n| n.id.clone()))
        }
        async fn mutate(
            &self,
            _: &MutationRequest,
            _: &CancellationToken,
        ) -> MutationResult<MutationReceipt> {
            Err(MutationError::Invalid)
        }
        async fn reconcile_mutation(
            &self,
            _: &MutationRequest,
            _: &CancellationToken,
        ) -> MutationResult<MutationReconciliation> {
            Err(MutationError::Invalid)
        }
        async fn mutate_prepared(
            &self,
            r: &MutationRequest,
            p: Option<&str>,
            _: &CancellationToken,
        ) -> MutationResult<MutationReceipt> {
            assert!(*r == self.request);
            assert_eq!(p, Some(self.receipt.id.as_str()));
            self.sent().await;
            self.mutations.fetch_add(1, Ordering::SeqCst);
            if self.lost {
                Err(MutationError::Uncertain)
            } else {
                Ok(MutationReceipt::Upsert(self.receipt.clone()))
            }
        }
        async fn reconcile_prepared_mutation(
            &self,
            r: &MutationRequest,
            p: Option<&str>,
            _: &CancellationToken,
        ) -> MutationResult<MutationReconciliation> {
            assert!(*r == self.request);
            assert_eq!(p, Some(self.receipt.id.as_str()));
            self.sent().await;
            self.inspections.fetch_add(1, Ordering::SeqCst);
            Ok(MutationReconciliation::Applied(MutationReceipt::Upsert(
                self.receipt.clone(),
            )))
        }
    }
    fn plan(p: &ICloudWriteProvider, kind: NodeKind) -> Relocation {
        let before = Node {
            id: format!(
                "{}::com.apple.CloudDocs::source",
                if kind == NodeKind::File {
                    "FILE"
                } else {
                    "FOLDER"
                }
            ),
            name: "Source".into(),
            kind: kind.clone(),
            size: if kind == NodeKind::File { 3 } else { 0 },
            content_version: if kind == NodeKind::File {
                Some("original-content".into())
            } else {
                None
            },
            ..folder(ROOT_ID)
        };
        let destination = Node {
            id: "FOLDER::com.apple.CloudDocs::destination".into(),
            name: "Destination".into(),
            ..folder(ROOT_ID)
        };
        let request = MutationRequest {
            scope: p.scope.clone(),
            intent: MutationIntent::Relocate {
                before: before.clone(),
                parent: destination.id.clone(),
                name: "Target".into(),
            },
        };
        let row = p
            .journal
            .lock()
            .unwrap()
            .enqueue_mutation(request.clone())
            .unwrap();
        Relocation {
            version: 1,
            operation: row.id,
            request,
            destination,
            current: before,
            original_sha256: (kind == NodeKind::File).then(|| "a".repeat(64)),
            step: 0,
            phase: Phase::Ready,
        }
    }

    #[tokio::test]
    async fn combined_relocation_checkpoints_each_step_and_inspects_a_lost_response_without_replay()
    {
        for kind in [NodeKind::File, NodeKind::Folder] {
            let (_temp, p) = fixture();
            let mut saved = plan(&p, kind);
            let original = saved.original().unwrap().clone();
            p.store_relocation(&saved).await.unwrap();
            let cancel = CancellationToken::new();
            let first = Arc::new(Child::new(&p, &saved, true));
            assert!(matches!(
                p.apply_relocation_step(&mut saved, first.clone(), &cancel)
                    .await,
                Err(MutationError::Uncertain)
            ));
            saved = p
                .load_relocation(saved.operation, &saved.request, Some(&original.id))
                .await
                .unwrap()
                .unwrap();
            assert!(saved.phase == Phase::Sent);
            assert!(matches!(
                p.mutate_combined(
                    &saved.operation.to_string(),
                    &saved.request,
                    Some(&original.id),
                    &cancel
                )
                .await,
                Err(MutationError::Uncertain)
            ));
            assert_eq!(first.mutations.load(Ordering::SeqCst), 1);
            let result = p
                .inspect_relocation_step(&mut saved, first.clone(), &cancel)
                .await
                .unwrap();
            assert!(matches!(result, MutationReconciliation::Uncommitted));
            assert_eq!(first.inspections.load(Ordering::SeqCst), 1);
            assert_eq!(saved.current.name, saved.temporary());
            // The source is no longer at its indexed name; replay cannot rebuild
            // from a newer index revision or re-run the initial rename.
            let mut index = Store::open(&p.metadata).unwrap();
            index
                .observe_node(
                    &p.scope,
                    &Node {
                        name: "other actor".into(),
                        etag: Some("later".into()),
                        ..original.clone()
                    },
                )
                .unwrap();
            for expected_step in 1..3 {
                saved = p
                    .load_relocation(saved.operation, &saved.request, Some(&original.id))
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(saved.step, expected_step);
                let child = Arc::new(Child::new(&p, &saved, false));
                p.apply_relocation_step(&mut saved, child.clone(), &cancel)
                    .await
                    .unwrap();
                assert_eq!(child.mutations.load(Ordering::SeqCst), 1);
            }
            let result = p
                .reconcile_combined(
                    &saved.operation.to_string(),
                    &saved.request,
                    Some(&original.id),
                    &cancel,
                )
                .await
                .unwrap();
            let MutationReconciliation::Applied(receipt) = result else {
                panic!("complete receipt");
            };
            assert!(saved.request.accepts(&receipt));
            assert_eq!(saved.current.content_version, original.content_version);
            assert!(saved.phase == Phase::Complete);
            assert!(saved.advance(receipt).is_err());
            let foreign = Uuid::new_v4();
            p.folder_vault
                .save(
                    &p.plan_key(foreign),
                    p.folder_vault
                        .load(&p.plan_key(saved.operation))
                        .await
                        .unwrap()
                        .unwrap(),
                )
                .await
                .unwrap();
            assert!(
                p.load_relocation(foreign, &saved.request, Some(&original.id))
                    .await
                    .is_err()
            );
            let mut malformed = saved.clone();
            malformed.current.parent_id = Some(ROOT_ID.into());
            assert!(p.store_relocation(&malformed).await.is_err());
        }
    }
    #[tokio::test]
    async fn combined_relocation_rejects_descendant_destination_before_saving_a_plan() {
        let (_temp, p) = fixture();
        let mut saved = plan(&p, NodeKind::Folder);
        saved.destination.parent_id = Some(saved.current.id.clone());
        let mut store = Store::open(&p.metadata).unwrap();
        store.observe_node(&p.scope, &saved.current).unwrap();
        store.observe_node(&p.scope, &saved.destination).unwrap();
        assert!(
            p.prepare_mutation_for_operation(
                &saved.operation.to_string(),
                &saved.request,
                &CancellationToken::new()
            )
            .await
            .is_err()
        );
        assert!(
            p.folder_vault
                .load(&p.plan_key(saved.operation))
                .await
                .unwrap()
                .is_none()
        );
    }
}
