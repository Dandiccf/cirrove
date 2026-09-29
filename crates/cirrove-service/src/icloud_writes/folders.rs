use super::*;
use crate::journal::MutationState;
use cirrove_core::mutation::{MutationIntent, Result as MutationResult};
use cirrove_icloud::{ICloudFolderCreate, ICloudFolderMove, ICloudFolderRename, ICloudFolderTrash};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FolderPlan {
    version: u8,
    request: MutationRequest,
    destination: Option<Node>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedPlan {
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
            if before.kind != NodeKind::Folder
                || before.package
                || before.target.is_some()
                || before.id == ROOT_ID
            {
                return Err(MutationError::Unsupported("iCloud file mutation routing"));
            }
            let current = self.parent(&before.id).await.map_err(local_error)?;
            if current != *before {
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
            MutationIntent::Relocate { .. } | MutationIntent::RemoveFolder { .. } => None,
            MutationIntent::RemoveFile { .. } => {
                return Err(MutationError::Unsupported("iCloud file removal routing"));
            }
        };
        let plan = FolderPlan {
            version: 1,
            request: request.clone(),
            destination,
        };
        self.folder_adapter(&plan)?;
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
            || saved.prepared.as_deref() != prepared
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

#[async_trait]
impl MutationProvider for ICloudWriteProvider {
    async fn prepare_mutation(
        &self,
        request: &MutationRequest,
        cancel: &CancellationToken,
    ) -> MutationResult<Option<String>> {
        let plan = self.folder_plan(request).await?;
        self.folder_adapter(&plan)?
            .prepare_mutation(request, cancel)
            .await
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
        let operation = self.mutation_operation(operation, request)?;
        // A saved plan marks a potentially sent request. Only reconciliation may
        // inspect it; absence of a receipt is never permission to send it again.
        if self
            .saved_plan(operation, request, prepared)
            .await?
            .is_some()
        {
            return Err(MutationError::Uncertain);
        }
        let plan = self.folder_plan(request).await?;
        let expected = request.intent.before().map(|node| node.id.as_str());
        if prepared != expected {
            return Err(MutationError::Invalid);
        }
        let adapter = self.folder_adapter(&plan)?;
        if cancel.is_cancelled() {
            return Err(MutationError::Uncertain);
        }
        self.save_plan(&SavedPlan {
            operation,
            prepared: prepared.map(str::to_owned),
            plan,
        })
        .await?;
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
        let operation = self.mutation_operation(operation, request)?;
        let Some(saved) = self.saved_plan(operation, request, prepared).await? else {
            return Ok(MutationReconciliation::Indeterminate);
        };
        self.folder_adapter(&saved.plan)?
            .reconcile_operation(&operation.to_string(), request, prepared, cancel)
            .await
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
