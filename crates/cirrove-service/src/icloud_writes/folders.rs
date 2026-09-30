use super::*;
mod relocations;
use crate::journal::MutationState;
use cirrove_core::mutation::{MutationIntent, Result as MutationResult, VerifiedMutationContent};
use cirrove_icloud::{
    ICloudFileMove, ICloudFileRename, ICloudFileTrash, ICloudFolderCreate, ICloudFolderMove,
    ICloudFolderRename, ICloudFolderTrash, ICloudReadSession, SealedSessionVault,
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FolderPlan {
    version: u8,
    request: MutationRequest,
    destination: Option<Node>,
    #[serde(default)]
    original_sha256: Option<String>,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PlanPhase {
    Prepared,
    // Legacy plans had no phase and must remain potentially sent.
    #[default]
    Sent,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedPlan {
    #[serde(default)]
    phase: PlanPhase,
    operation: Uuid,
    prepared: Option<String>,
    plan: FolderPlan,
}

fn local_error(error: UploadError) -> MutationError {
    match error {
        UploadError::Conflict => MutationError::Conflict,
        UploadError::Invalid => MutationError::Invalid,
        _ => MutationError::Uncertain,
    }
}

impl ICloudWriteProvider {
    async fn mutation_source(&self, before: &Node) -> MutationResult<Node> {
        if before.kind == NodeKind::Folder {
            return self.parent(&before.id).await.map_err(local_error);
        }
        if before.kind != NodeKind::File
            || before.size > cirrove_icloud::MAX_WRITE_FILE_SIZE
            || before.package
            || before.target.is_some()
        {
            return Err(MutationError::Unsupported("iCloud file verification limit"));
        }
        let (metadata, scope, id) = (self.metadata.clone(), self.scope.clone(), before.id.clone());
        tokio::task::spawn_blocking(move || {
            let store = Store::open(&metadata).map_err(|_| MutationError::Uncertain)?;
            let chain = store
                .node_chain_to_root(&scope, &id, ROOT_ID)
                .map_err(|_| MutationError::Uncertain)?
                .ok_or(MutationError::Conflict)?;
            if chain
                .iter()
                .skip(1)
                .any(|node| node.kind != NodeKind::Folder || node.package || node.target.is_some())
            {
                return Err(MutationError::Invalid);
            }
            chain.into_iter().next().ok_or(MutationError::Conflict)
        })
        .await
        .map_err(|_| MutationError::Uncertain)?
    }

    async fn capture_original_digest(
        &self,
        before: &Node,
        cancel: &CancellationToken,
    ) -> MutationResult<String> {
        let parent = before.parent_id.as_deref().ok_or(MutationError::Invalid)?;
        let etag = before.etag.as_deref().ok_or(MutationError::Invalid)?;
        // Reuse the bounded streaming verifier; no file bytes or session values
        // enter the metadata index, diagnostics or public journal records.
        tokio::select! { biased;
            _ = cancel.cancelled() => Err(MutationError::Uncertain),
            result = async {
                let vault = SealedSessionVault::new(&self.state, &self.scope.account).map_err(|_| MutationError::Invalid)?;
                let snapshot = vault.load(&self.credential_id).await.map_err(|_| MutationError::Uncertain)?.ok_or(MutationError::Uncertain)?;
                let mut session = ICloudReadSession::from_session_snapshot(&snapshot, &self.apple_id).map_err(|_| MutationError::Uncertain)?;
                session.hash_file_in_folder_for_revision(parent, &before.id, etag, before.size).await.map_err(|_| MutationError::Uncertain)
            } => result,
        }
    }

    fn mutation_operation(
        &self,
        operation: &str,
        request: &MutationRequest,
    ) -> MutationResult<Uuid> {
        request.validate()?;
        if request.scope != self.scope {
            return Err(MutationError::Invalid);
        }
        let id = Uuid::parse_str(operation).map_err(|_| MutationError::Invalid)?;
        let journal = self.journal.lock().map_err(|_| MutationError::Uncertain)?;
        let row = journal.mutation(id).map_err(|_| MutationError::Invalid)?;
        if row.request != *request
            || !matches!(
                row.state,
                MutationState::Pending
                    | MutationState::Applying
                    | MutationState::Verifying
                    | MutationState::VerifyRequired
                    | MutationState::Failed
            )
        {
            return Err(MutationError::Invalid);
        }
        Ok(id)
    }

    async fn folder_plan(&self, request: &MutationRequest) -> MutationResult<FolderPlan> {
        request.validate()?;
        if request.scope != self.scope {
            return Err(MutationError::Invalid);
        }
        if let Some(before) = request.intent.before() {
            if !matches!(before.kind, NodeKind::Folder | NodeKind::File)
                || before.package
                || before.target.is_some()
                || before.id == ROOT_ID
            {
                return Err(MutationError::Invalid);
            }
            let mut current = self.mutation_source(before).await?;
            let mut expected = before.clone();
            if before.kind == NodeKind::File {
                // Listings and upload receipts use different local content
                // annotations. Apple's ETag is the mutation precondition;
                // independent digest verification still precedes any request.
                current.content_version = None;
                expected.content_version = None;
            }
            if current != expected {
                return Err(MutationError::Conflict);
            }
        }
        let destination = match &request.intent {
            MutationIntent::CreateFolder { parent, .. } => {
                Some(self.parent(parent).await.map_err(local_error)?)
            }
            MutationIntent::Relocate {
                before,
                parent,
                name,
            } if before.parent_id.as_deref() != Some(parent) => {
                if name != &before.name {
                    return Err(MutationError::Unsupported(
                        "combined iCloud move and rename",
                    ));
                }
                Some(
                    self.parent_excluding(parent, Some(before.id.clone()))
                        .await
                        .map_err(local_error)?,
                )
            }
            MutationIntent::Relocate { .. }
            | MutationIntent::RemoveFolder { .. }
            | MutationIntent::RemoveFile { .. } => None,
        };
        let plan = FolderPlan {
            version: 1,
            request: request.clone(),
            destination,
            original_sha256: None,
        };
        if !request
            .intent
            .before()
            .is_some_and(|node| node.kind == NodeKind::File)
        {
            self.folder_adapter(&plan)?;
        }
        Ok(plan)
    }

    fn folder_adapter(&self, plan: &FolderPlan) -> MutationResult<Arc<dyn MutationProvider>> {
        if plan.version != 1 || plan.request.scope != self.scope {
            return Err(MutationError::Invalid);
        }
        plan.request.validate()?;
        let scope = self.scope.clone();
        let apple = self.apple_id.clone();
        let credential = self.credential_id.clone();
        if plan
            .request
            .intent
            .before()
            .is_some_and(|node| node.kind == NodeKind::File)
        {
            let digest = plan.original_sha256.clone().ok_or(MutationError::Invalid)?;
            return match &plan.request.intent {
                MutationIntent::Relocate {
                    before,
                    parent,
                    name,
                } if before.parent_id.as_deref() == Some(parent) && plan.destination.is_none() => {
                    Ok(Arc::new(ICloudFileRename::from_sealed_session(
                        scope,
                        apple,
                        credential,
                        &self.state,
                        before.clone(),
                        name.clone(),
                        digest,
                    )?))
                }
                MutationIntent::Relocate {
                    before,
                    parent,
                    name,
                } if name == &before.name => {
                    let destination = plan
                        .destination
                        .as_ref()
                        .filter(|node| &node.id == parent)
                        .ok_or(MutationError::Invalid)?;
                    Ok(Arc::new(ICloudFileMove::from_sealed_session(
                        scope,
                        apple,
                        credential,
                        &self.state,
                        before.clone(),
                        destination.clone(),
                        digest,
                    )?))
                }
                MutationIntent::RemoveFile { before } if plan.destination.is_none() => {
                    Ok(Arc::new(
                        ICloudFileTrash::from_sealed_session(
                            scope,
                            apple,
                            credential,
                            &self.state,
                            before.clone(),
                        )?
                        .with_expected_sha256(digest)?,
                    ))
                }
                _ => Err(MutationError::Unsupported("iCloud file mutation")),
            };
        }
        if plan.original_sha256.is_some() {
            return Err(MutationError::Invalid);
        }
        match &plan.request.intent {
            MutationIntent::CreateFolder { parent, .. } => {
                let destination = plan
                    .destination
                    .as_ref()
                    .filter(|node| &node.id == parent)
                    .ok_or(MutationError::Invalid)?;
                Ok(Arc::new(ICloudFolderCreate::from_sealed_session(
                    scope,
                    apple,
                    credential,
                    &self.state,
                    destination.clone(),
                    self.folder_vault.clone(),
                )?))
            }
            MutationIntent::Relocate {
                before,
                parent,
                name,
            } if before.parent_id.as_deref() == Some(parent) && plan.destination.is_none() => {
                Ok(Arc::new(ICloudFolderRename::from_sealed_session(
                    scope,
                    apple,
                    credential,
                    &self.state,
                    before.clone(),
                    name.clone(),
                )?))
            }
            MutationIntent::Relocate {
                before,
                parent,
                name,
            } if name == &before.name => {
                let destination = plan
                    .destination
                    .as_ref()
                    .filter(|node| &node.id == parent)
                    .ok_or(MutationError::Invalid)?;
                Ok(Arc::new(ICloudFolderMove::from_sealed_session(
                    scope,
                    apple,
                    credential,
                    &self.state,
                    before.clone(),
                    destination.clone(),
                )?))
            }
            MutationIntent::RemoveFolder { before } if plan.destination.is_none() => {
                Ok(Arc::new(ICloudFolderTrash::from_sealed_session(
                    scope,
                    apple,
                    credential,
                    &self.state,
                    before.clone(),
                )?))
            }
            _ => Err(MutationError::Unsupported("iCloud namespace request")),
        }
    }

    fn plan_key(&self, operation: Uuid) -> String {
        format!("icloud-folder-plan/{}/{operation}", self.scope.account)
    }

    async fn saved_plan(
        &self,
        operation: Uuid,
        request: &MutationRequest,
        prepared: Option<&str>,
    ) -> MutationResult<Option<SavedPlan>> {
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
        let saved: SavedPlan =
            serde_json::from_str(value.expose_secret()).map_err(|_| MutationError::Invalid)?;
        if saved.operation != operation
            || saved.plan.request != *request
            || (saved.prepared.as_deref() != prepared
                && !(saved.phase == PlanPhase::Prepared && prepared.is_none()))
            || saved.prepared.as_deref() != request.intent.before().map(|node| node.id.as_str())
        {
            return Err(MutationError::Invalid);
        }
        self.folder_adapter(&saved.plan)?;
        Ok(Some(saved))
    }

    async fn save_plan(&self, saved: &SavedPlan) -> MutationResult<()> {
        let value = serde_json::to_string(saved).map_err(|_| MutationError::Invalid)?;
        if value.len() > 32 * 1024 {
            return Err(MutationError::Invalid);
        }
        self.folder_vault
            .save(&self.plan_key(saved.operation), SecretString::from(value))
            .await
            .map_err(|_| MutationError::Uncertain)
    }
}

// These adapters verify full file bytes against the digest captured under the
// original ETag. Carry that stronger evidence across the provider-neutral journal
// boundary instead of manufacturing an iCloud content-version token.
fn verified_relocation_receipt(
    request: &MutationRequest,
    receipt: MutationReceipt,
    digest: Option<&str>,
) -> MutationResult<MutationReconciliation> {
    if matches!(&request.intent, MutationIntent::Relocate { before, .. } if before.kind == NodeKind::File)
    {
        let proof = VerifiedMutationContent::for_relocation(
            request,
            &receipt,
            digest.ok_or(MutationError::Invalid)?.into(),
        )?;
        Ok(MutationReconciliation::AppliedWithVerifiedContent { receipt, proof })
    } else {
        Ok(MutationReconciliation::Applied(receipt))
    }
}

#[async_trait]
impl MutationProvider for ICloudWriteProvider {
    fn deletion(&self) -> cirrove_core::mutation::DeletionSupport {
        cirrove_core::mutation::DeletionSupport {
            recycle_bin: true,
            permanent: false,
        }
    }

    async fn prepare_mutation(
        &self,
        _: &MutationRequest,
        _: &CancellationToken,
    ) -> MutationResult<Option<String>> {
        Err(MutationError::Unsupported(
            "durable iCloud preparation operation required",
        ))
    }

    async fn prepare_mutation_for_operation(
        &self,
        operation: &str,
        request: &MutationRequest,
        cancel: &CancellationToken,
    ) -> MutationResult<Option<String>> {
        if relocations::combined(request) {
            return self.prepare_combined(operation, request, cancel).await;
        }
        let operation = self.mutation_operation(operation, request)?;
        let saved = match self.saved_plan(operation, request, None).await? {
            Some(saved) if saved.phase == PlanPhase::Prepared => saved,
            Some(_) => return Err(MutationError::Uncertain),
            None => {
                let mut plan = self.folder_plan(request).await?;
                if let Some(before) = request
                    .intent
                    .before()
                    .filter(|node| node.kind == NodeKind::File)
                {
                    plan.original_sha256 =
                        Some(self.capture_original_digest(before, cancel).await?);
                }
                self.folder_adapter(&plan)?;
                let saved = SavedPlan {
                    phase: PlanPhase::Prepared,
                    operation,
                    prepared: request.intent.before().map(|node| node.id.clone()),
                    plan,
                };
                self.save_plan(&saved).await?;
                saved
            }
        };
        let prepared = self
            .folder_adapter(&saved.plan)?
            .prepare_mutation(request, cancel)
            .await?;
        if prepared != saved.prepared {
            return Err(MutationError::Invalid);
        }
        Ok(prepared)
    }

    async fn mutate(
        &self,
        _: &MutationRequest,
        _: &CancellationToken,
    ) -> MutationResult<MutationReceipt> {
        Err(MutationError::Unsupported(
            "durable iCloud mutation operation required",
        ))
    }

    async fn mutate_operation(
        &self,
        operation: &str,
        request: &MutationRequest,
        prepared: Option<&str>,
        cancel: &CancellationToken,
    ) -> MutationResult<MutationReceipt> {
        if relocations::combined(request) {
            return self
                .mutate_combined(operation, request, prepared, cancel)
                .await;
        }
        let operation = self.mutation_operation(operation, request)?;
        let mut saved = self
            .saved_plan(operation, request, prepared)
            .await?
            .ok_or(MutationError::Uncertain)?;
        if saved.phase != PlanPhase::Prepared {
            return Err(MutationError::Uncertain);
        }
        if saved.prepared.as_deref() != prepared {
            return Err(MutationError::Invalid);
        }
        let adapter = self.folder_adapter(&saved.plan)?;
        if cancel.is_cancelled() {
            return Err(MutationError::Uncertain);
        }
        // Persist the possibly-sent boundary before dispatch. A crash beyond
        // here authorizes inspection only, including legacy plans with no phase.
        saved.phase = PlanPhase::Sent;
        self.save_plan(&saved).await?;
        adapter
            .mutate_operation(&operation.to_string(), request, prepared, cancel)
            .await
    }

    async fn reconcile_mutation(
        &self,
        _: &MutationRequest,
        _: &CancellationToken,
    ) -> MutationResult<MutationReconciliation> {
        Ok(MutationReconciliation::Indeterminate)
    }

    async fn reconcile_operation(
        &self,
        operation: &str,
        request: &MutationRequest,
        prepared: Option<&str>,
        cancel: &CancellationToken,
    ) -> MutationResult<MutationReconciliation> {
        if relocations::combined(request) {
            return self
                .reconcile_combined(operation, request, prepared, cancel)
                .await;
        }
        let operation = self.mutation_operation(operation, request)?;
        let Some(saved) = self.saved_plan(operation, request, prepared).await? else {
            return Ok(MutationReconciliation::Indeterminate);
        };
        if saved.phase == PlanPhase::Prepared {
            return Ok(MutationReconciliation::Uncommitted);
        }
        match self
            .folder_adapter(&saved.plan)?
            .reconcile_operation(&operation.to_string(), request, prepared, cancel)
            .await?
        {
            MutationReconciliation::Applied(receipt) => {
                verified_relocation_receipt(request, receipt, saved.plan.original_sha256.as_deref())
            }
            other => Ok(other),
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::tests::{fixture, folder};
    use super::*;

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn saved_folder_plan_survives_index_change_and_refuses_replay_or_rebinding() {
        let (_temp, p) = fixture();
        let parent = folder(ROOT_ID);
        let mut store = Store::open(&p.metadata).unwrap();
        store.observe_node(&p.scope, &parent).unwrap();
        let request = MutationRequest {
            scope: p.scope.clone(),
            intent: MutationIntent::CreateFolder {
                parent: parent.id.clone(),
                name: "Child".into(),
            },
        };
        let row = p
            .journal
            .lock()
            .unwrap()
            .enqueue_mutation(request.clone())
            .unwrap();
        let plan = p.folder_plan(&request).await.unwrap();
        p.save_plan(&SavedPlan {
            phase: PlanPhase::Sent,
            operation: row.id,
            prepared: None,
            plan,
        })
        .await
        .unwrap();
        store
            .observe_node(
                &p.scope,
                &Node {
                    package: true,
                    ..parent.clone()
                },
            )
            .unwrap();
        assert!(p.folder_plan(&request).await.is_err());
        let saved = p.saved_plan(row.id, &request, None).await.unwrap().unwrap();
        assert_eq!(saved.plan.destination.unwrap(), parent);
        assert!(matches!(
            p.mutate_operation(
                &row.id.to_string(),
                &request,
                None,
                &CancellationToken::new()
            )
            .await,
            Err(MutationError::Uncertain)
        ));
        assert!(
            p.saved_plan(row.id, &request, Some("wrong-id"))
                .await
                .is_err()
        );
        let mut foreign = request.clone();
        foreign.scope.account = Uuid::new_v4().to_string();
        assert!(p.mutation_operation(&row.id.to_string(), &foreign).is_err());
        let other = p
            .journal
            .lock()
            .unwrap()
            .enqueue_mutation(request.clone())
            .unwrap();
        let serialized = p
            .folder_vault
            .load(&p.plan_key(row.id))
            .await
            .unwrap()
            .unwrap();
        p.folder_vault
            .save(&p.plan_key(other.id), serialized)
            .await
            .unwrap();
        assert!(p.saved_plan(other.id, &request, None).await.is_err());
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn folder_plans_route_rename_move_trash_and_reject_descendant_moves() {
        let (_temp, p) = fixture();
        let source = folder(ROOT_ID);
        let destination = Node {
            id: "FOLDER::com.apple.CloudDocs::destination".into(),
            ..folder(ROOT_ID)
        };
        let child = Node {
            id: "FOLDER::com.apple.CloudDocs::child".into(),
            ..folder(&source.id)
        };
        let mut store = Store::open(&p.metadata).unwrap();
        let grandchild = Node {
            id: "FOLDER::com.apple.CloudDocs::grandchild".into(),
            ..folder(&child.id)
        };
        for node in [&source, &destination, &child, &grandchild] {
            store.observe_node(&p.scope, node).unwrap();
        }
        for intent in [
            MutationIntent::Relocate {
                before: source.clone(),
                parent: ROOT_ID.into(),
                name: "Renamed".into(),
            },
            MutationIntent::Relocate {
                before: source.clone(),
                parent: destination.id.clone(),
                name: source.name.clone(),
            },
            MutationIntent::RemoveFolder {
                before: source.clone(),
            },
        ] {
            let request = MutationRequest {
                scope: p.scope.clone(),
                intent,
            };
            let plan = p.folder_plan(&request).await.unwrap();
            assert!(p.folder_adapter(&plan).is_ok());
        }
        let request = MutationRequest {
            scope: p.scope.clone(),
            intent: MutationIntent::Relocate {
                before: source.clone(),
                parent: grandchild.id,
                name: source.name.clone(),
            },
        };
        assert!(p.folder_plan(&request).await.is_err());
        let combined = MutationRequest {
            scope: p.scope.clone(),
            intent: MutationIntent::Relocate {
                before: source,
                parent: destination.id,
                name: "New name".into(),
            },
        };
        assert!(matches!(
            p.folder_plan(&combined).await,
            Err(MutationError::Unsupported(_))
        ));
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn read_only_preparation_can_resume_but_legacy_plans_remain_potentially_sent() {
        let (_temp, p) = fixture();
        let request = MutationRequest {
            scope: p.scope.clone(),
            intent: MutationIntent::CreateFolder {
                parent: ROOT_ID.into(),
                name: "Child".into(),
            },
        };
        let row = p
            .journal
            .lock()
            .unwrap()
            .enqueue_mutation(request.clone())
            .unwrap();
        let operation = row.id.to_string();
        let cancel = CancellationToken::new();
        assert!(matches!(
            p.mutate_operation(&operation, &request, None, &cancel)
                .await,
            Err(MutationError::Uncertain)
        ));
        assert!(
            p.prepare_mutation_for_operation(&operation, &request, &cancel)
                .await
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            p.reconcile_operation(&operation, &request, None, &cancel)
                .await
                .unwrap(),
            MutationReconciliation::Uncommitted
        ));
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(
            p.mutate_operation(&operation, &request, None, &cancelled)
                .await
                .is_err()
        );
        assert!(matches!(
            p.reconcile_operation(&operation, &request, None, &cancel)
                .await
                .unwrap(),
            MutationReconciliation::Uncommitted
        ));
        let saved = p
            .folder_vault
            .load(&p.plan_key(row.id))
            .await
            .unwrap()
            .unwrap();
        let mut legacy: serde_json::Value = serde_json::from_str(saved.expose_secret()).unwrap();
        legacy.as_object_mut().unwrap().remove("phase");
        p.folder_vault
            .save(&p.plan_key(row.id), SecretString::from(legacy.to_string()))
            .await
            .unwrap();
        assert!(
            p.saved_plan(row.id, &request, None)
                .await
                .unwrap()
                .unwrap()
                .phase
                == PlanPhase::Sent
        );
        assert!(matches!(
            p.prepare_mutation_for_operation(&operation, &request, &cancel)
                .await,
            Err(MutationError::Uncertain)
        ));
        assert!(matches!(
            p.mutate_operation(&operation, &request, None, &cancel)
                .await,
            Err(MutationError::Uncertain)
        ));
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn interrupted_preparation_does_not_require_prepared_id_to_have_reached_journal() {
        let (_temp, p) = fixture();
        let before = folder(ROOT_ID);
        Store::open(&p.metadata)
            .unwrap()
            .observe_node(&p.scope, &before)
            .unwrap();
        let request = MutationRequest {
            scope: p.scope.clone(),
            intent: MutationIntent::RemoveFolder {
                before: before.clone(),
            },
        };
        let row = p
            .journal
            .lock()
            .unwrap()
            .enqueue_mutation(request.clone())
            .unwrap();
        let saved = SavedPlan {
            phase: PlanPhase::Prepared,
            operation: row.id,
            prepared: Some(before.id.clone()),
            plan: p.folder_plan(&request).await.unwrap(),
        };
        p.save_plan(&saved).await.unwrap();
        let cancel = CancellationToken::new();
        assert!(matches!(
            p.reconcile_operation(&row.id.to_string(), &request, None, &cancel)
                .await
                .unwrap(),
            MutationReconciliation::Uncommitted
        ));
        assert!(
            p.mutate_operation(&row.id.to_string(), &request, None, &cancel)
                .await
                .is_err()
        );
        assert!(
            p.saved_plan(row.id, &request, Some("wrong-id"))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn ordinary_mutation_uses_provider_etag_not_projection_content_version() {
        let (_temp, p) = fixture();
        let parent = folder(ROOT_ID);
        let before = Node {
            id: "FILE::com.apple.CloudDocs::temporary".into(),
            parent_id: Some(parent.id.clone()),
            name: "Temporary.txt".into(),
            kind: NodeKind::File,
            size: 4,
            etag: Some("provider-etag".into()),
            content_version: Some("receipt-content-version".into()),
            ..folder(ROOT_ID)
        };
        let mut store = Store::open(&p.metadata).unwrap();
        store.observe_node(&p.scope, &parent).unwrap();
        let listed = Node {
            content_version: None,
            ..before.clone()
        };
        store.observe_node(&p.scope, &listed).unwrap();
        let request = MutationRequest {
            scope: p.scope.clone(),
            intent: MutationIntent::RemoveFile {
                before: before.clone(),
            },
        };
        let plan = p.folder_plan(&request).await.unwrap();
        assert!(
            p.folder_adapter(&plan).is_err(),
            "digest preflight must remain required"
        );
        assert!(plan.request == request);
        for changed in [
            Node {
                etag: Some("different-etag".into()),
                ..listed.clone()
            },
            Node {
                size: 5,
                ..listed.clone()
            },
            Node {
                name: "Other.txt".into(),
                ..listed.clone()
            },
            Node {
                package: true,
                ..listed.clone()
            },
        ] {
            store.observe_node(&p.scope, &changed).unwrap();
            assert!(p.folder_plan(&request).await.is_err());
        }
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn file_operations_require_and_restore_the_original_digest_and_revision() {
        let (_temp, p) = fixture();
        let parent = folder(ROOT_ID);
        let before = Node {
            id: "FILE::com.apple.CloudDocs::synthetic-file".into(),
            parent_id: Some(parent.id.clone()),
            name: "Original.txt".into(),
            kind: NodeKind::File,
            size: 4,
            etag: Some("original-etag".into()),
            ..folder(ROOT_ID)
        };
        let mut store = Store::open(&p.metadata).unwrap();
        store.observe_node(&p.scope, &parent).unwrap();
        store.observe_node(&p.scope, &before).unwrap();
        for intent in [
            MutationIntent::Relocate {
                before: before.clone(),
                parent: parent.id.clone(),
                name: "Renamed.txt".into(),
            },
            MutationIntent::Relocate {
                before: before.clone(),
                parent: ROOT_ID.into(),
                name: before.name.clone(),
            },
            MutationIntent::RemoveFile {
                before: before.clone(),
            },
        ] {
            let request = MutationRequest {
                scope: p.scope.clone(),
                intent,
            };
            let mut plan = p.folder_plan(&request).await.unwrap();
            assert!(p.folder_adapter(&plan).is_err());
            plan.original_sha256 = Some("malformed-digest".into());
            assert!(p.folder_adapter(&plan).is_err());
            plan.original_sha256 = Some("a".repeat(64));
            assert!(p.folder_adapter(&plan).is_ok());
            let row = p
                .journal
                .lock()
                .unwrap()
                .enqueue_mutation(request.clone())
                .unwrap();
            p.save_plan(&SavedPlan {
                phase: PlanPhase::Prepared,
                operation: row.id,
                prepared: Some(before.id.clone()),
                plan,
            })
            .await
            .unwrap();
            store
                .observe_node(
                    &p.scope,
                    &Node {
                        etag: Some("later-etag".into()),
                        ..before.clone()
                    },
                )
                .unwrap();
            assert!(p.folder_plan(&request).await.is_err());
            let restored = p
                .saved_plan(row.id, &request, Some(&before.id))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                restored.plan.original_sha256.as_deref(),
                Some("a".repeat(64).as_str())
            );
            assert_eq!(restored.plan.request.intent.before().unwrap(), &before);
            store.observe_node(&p.scope, &before).unwrap();
        }
        store
            .observe_node(
                &p.scope,
                &Node {
                    package: true,
                    ..parent
                },
            )
            .unwrap();
        let request = MutationRequest {
            scope: p.scope.clone(),
            intent: MutationIntent::RemoveFile { before },
        };
        assert!(p.folder_plan(&request).await.is_err());
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn cancelled_file_preparation_publishes_no_digest_or_mutation_plan() {
        let (_temp, p) = fixture();
        let before = Node {
            id: "FILE::com.apple.CloudDocs::cancelled-file".into(),
            parent_id: Some(ROOT_ID.into()),
            name: "Cancel.txt".into(),
            kind: NodeKind::File,
            size: 4,
            etag: Some("original-etag".into()),
            ..folder(ROOT_ID)
        };
        Store::open(&p.metadata)
            .unwrap()
            .observe_node(&p.scope, &before)
            .unwrap();
        let request = MutationRequest {
            scope: p.scope.clone(),
            intent: MutationIntent::RemoveFile { before },
        };
        let row = p
            .journal
            .lock()
            .unwrap()
            .enqueue_mutation(request.clone())
            .unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(matches!(
            p.prepare_mutation_for_operation(&row.id.to_string(), &request, &cancel)
                .await,
            Err(MutationError::Uncertain)
        ));
        assert!(
            p.folder_vault
                .load(&p.plan_key(row.id))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[derive(Default)]
    pub(crate) struct MemoryVault(Mutex<std::collections::HashMap<String, String>>);
    #[async_trait]
    impl CredentialVault for MemoryVault {
        async fn load(&self, key: &str) -> anyhow::Result<Option<SecretString>> {
            Ok(self
                .0
                .lock()
                .map_err(|_| anyhow::anyhow!("test vault lock"))?
                .get(key)
                .cloned()
                .map(SecretString::from))
        }
        async fn save(&self, key: &str, value: SecretString) -> anyhow::Result<()> {
            self.0
                .lock()
                .map_err(|_| anyhow::anyhow!("test vault lock"))?
                .insert(key.into(), value.expose_secret().to_string());
            Ok(())
        }
        async fn remove(&self, key: &str) -> anyhow::Result<()> {
            self.0
                .lock()
                .map_err(|_| anyhow::anyhow!("test vault lock"))?
                .remove(key);
            Ok(())
        }
    }
}
