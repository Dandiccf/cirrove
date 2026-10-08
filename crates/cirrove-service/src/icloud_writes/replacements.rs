use super::*;
use cirrove_icloud::{ICloudFileReplace, ICloudSealedSignIn};

impl ICloudWriteProvider {
    /// Select locally before TransferWorker reserves or restores a checkpoint.
    /// A retained reservation wins over the new target suffix; no network runs
    /// while the journal owner is held.
    pub(super) fn ordinary_recovery_location(
        &self,
        operation: Uuid,
        request: &UploadRequest,
    ) -> Result<cirrove_core::upload::RecoveryLocation> {
        request.require_file_bytes()?;
        let (retained, predecessor) = {
            let journal = self.journal.lock().map_err(|_| UploadError::Uncertain)?;
            let row = journal.get(operation).map_err(|_| UploadError::Invalid)?;
            let retained = row.reserved_recovery_location();
            let predecessor = if retained.is_none() {
                journal
                    .confirmed_upload_base(operation)
                    .map_err(|_| UploadError::Conflict)?
            } else {
                None
            };
            (retained, predecessor)
        };
        if let Some(location) = retained {
            return Ok(location);
        }
        let UploadIntent::Replace {
            item,
            expected_etag,
        } = &request.intent
        else {
            return Err(UploadError::Invalid);
        };
        let original =
            if let Some(original) = predecessor {
                original
            } else {
                let store = Store::open(&self.metadata).map_err(|_| UploadError::Uncertain)?;
                let chain = store
                    .node_chain_to_root(&self.scope, item, ROOT_ID)
                    .map_err(|_| UploadError::Uncertain)?
                    .ok_or(UploadError::Conflict)?;
                if chain.iter().skip(1).any(|node| {
                    node.kind != NodeKind::Folder || node.package || node.target.is_some()
                }) {
                    return Err(UploadError::Invalid);
                }
                chain.into_iter().next().ok_or(UploadError::Conflict)?
            };
        if original.id != *item
            || original.kind != NodeKind::File
            || original.package
            || original.target.is_some()
            || original.etag.as_deref() != Some(expected_etag.as_str())
        {
            return Err(UploadError::Conflict);
        }
        Ok(ICloudFileReplace::recovery_location_for_name(
            operation,
            &original.name,
        ))
    }

    pub(super) async fn replacement(
        &self,
        operation: &str,
        request: &UploadRequest,
        checkpoint: Option<&SecretString>,
    ) -> Result<ICloudFileReplace> {
        let operation = self.validate_operation(operation, request)?;
        let UploadIntent::Replace {
            item,
            expected_etag,
        } = &request.intent
        else {
            return Err(UploadError::Invalid);
        };
        let reserved = self
            .journal
            .lock()
            .map_err(|_| UploadError::Uncertain)?
            .get(operation)
            .map_err(|_| UploadError::Invalid)?
            .reserved_recovery_location();
        let sign_in = ICloudSealedSignIn {
            apple_id: self.apple_id.clone(),
            credential_id: self.credential_id.clone(),
        };
        if let Some(checkpoint) = checkpoint {
            // Never reconstruct a checkpoint-bearing replacement from the active
            // index: its original may already be in Trash after a prior phase.
            let actor = ICloudFileReplace::from_sealed_checkpoint(
                request,
                operation,
                checkpoint,
                sign_in,
                &self.state,
            )?
            .ok_or(UploadError::CheckpointInvalid)?;
            return match reserved {
                Some(location) => actor.with_recovery_location(location),
                None => Ok(actor),
            };
        }
        // A preceding handoff can publish a new remote ID before the read index
        // sees it. Use only this operation's resolved, confirmed dependency.
        let predecessor = self
            .journal
            .lock()
            .map_err(|_| UploadError::Uncertain)?
            .confirmed_upload_base(operation)
            .map_err(|_| UploadError::Conflict)?;
        let (original, folder) = if let Some(original) = predecessor {
            let parent = original.parent_id.as_deref().ok_or(UploadError::Invalid)?;
            let folder = self.parent(parent).await?;
            (original, folder)
        } else {
            self.replacement_metadata(item, expected_etag).await?
        };
        let actor = ICloudFileReplace::from_sealed_session_for_existing(
            self.scope.clone(),
            folder,
            original,
            operation,
            sign_in,
            &self.state,
        )?;
        match reserved {
            Some(location) => actor.with_recovery_location(location),
            None => Ok(actor),
        }
    }

    async fn replacement_metadata(&self, item: &str, expected_etag: &str) -> Result<(Node, Node)> {
        let (metadata, scope, item, expected_etag) = (
            self.metadata.clone(),
            self.scope.clone(),
            item.to_owned(),
            expected_etag.to_owned(),
        );
        let (original, folder) =
            tokio::task::spawn_blocking(move || {
                let store = Store::open(&metadata).map_err(|_| UploadError::Uncertain)?;
                let chain = store
                    .node_chain_to_root(&scope, &item, ROOT_ID)
                    .map_err(|_| UploadError::Uncertain)?
                    .ok_or(UploadError::Conflict)?;
                if chain.iter().skip(1).any(|node| {
                    node.kind != NodeKind::Folder || node.package || node.target.is_some()
                }) {
                    return Err(UploadError::Invalid);
                }
                let mut chain = chain.into_iter();
                let original = chain.next().ok_or(UploadError::Conflict)?;
                if original.kind != NodeKind::File
                    || original.package
                    || original.target.is_some()
                    || original.etag.as_deref() != Some(&expected_etag)
                {
                    return Err(UploadError::Conflict);
                }
                Ok((original, chain.next()))
            })
            .await
            .map_err(|_| UploadError::Uncertain)??;
        let folder = match folder {
            Some(folder) => folder,
            None if original.parent_id.as_deref() == Some(ROOT_ID) => self.parent(ROOT_ID).await?,
            None => return Err(UploadError::Conflict),
        };
        if original.parent_id.as_deref() != Some(&folder.id) {
            return Err(UploadError::Conflict);
        }
        Ok((original, folder))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::super::tests::{fixture, folder};
    use super::*;

    /// The worker reserves before restoring a checkpoint. The hint must agree
    /// with the current target type, including an ACKed predecessor not indexed yet.
    #[tokio::test]
    async fn replacement_router_reserves_keynote_recovery_name_before_checkpoint() {
        let (_temp, p) = fixture();
        let original = Node {
            id: "FILE::com.apple.CloudDocs::confirmed-keynote".into(),
            parent_id: Some(ROOT_ID.into()),
            name: "Genuine.key".into(),
            kind: NodeKind::File,
            size: 1,
            etag: Some("confirmed-A".into()),
            ..folder(ROOT_ID)
        };
        let row = {
            let mut journal = p.journal.lock().unwrap();
            let working = journal
                .create_working(
                    p.scope.clone(),
                    Node {
                        size: 0,
                        etag: None,
                        ..original.clone()
                    },
                    true,
                    &b""[..],
                )
                .unwrap();
            journal.write_working(working.id, 0, b"x").unwrap();
            let first = journal.seal_working(working.id).unwrap().unwrap();
            let claimed = journal.claim_next().unwrap().unwrap();
            assert_eq!(claimed.id, first.id);
            journal
                .acknowledge(first.id, claimed.attempt.unwrap(), original.clone())
                .unwrap();
            let successor = journal.truncate_working(working.id, 0).unwrap();
            journal
                .write_working(successor.id, 0, b"replacement")
                .unwrap();
            let next = journal.seal_working(successor.id).unwrap().unwrap();
            assert_eq!(next.working_file, Some(working.id));
            assert_eq!(next.base.as_ref().unwrap().predecessor, first.id);
            let claimed = journal.claim_next().unwrap().unwrap();
            assert_eq!(claimed.id, next.id);
            claimed
        };
        let request = UploadRequest {
            representation: Default::default(),
            scope: row.scope.clone(),
            intent: row.intent.clone(),
            size: row.size,
            sha256: row.sha256.clone(),
        };
        assert!(
            Store::open(&p.metadata)
                .unwrap()
                .node(&p.scope, &original.id)
                .unwrap()
                .is_none()
        );
        let expected = cirrove_core::upload::RecoveryLocation::Trash {
            local_name: format!("recovery-by-cirrove-{}.key", row.id),
            parent: "FOLDER::com.apple.CloudDocs::TRASH_ROOT".into(),
        };
        let hinted = p.staged_recovery_location(&row.id.to_string(), &request);
        assert_eq!(
            hinted,
            Some(expected.clone()),
            "ordinary Keynote router must reserve target-derived recovery suffix before checkpoint restore"
        );
        let mut journal = p.journal.lock().unwrap();
        let before = journal.get(row.id).unwrap();
        assert!(before.identity_handoff.is_none());
        journal
            .reserve_identity_handoff(row.id, row.attempt.unwrap(), expected)
            .unwrap();
        let after = journal.get(row.id).unwrap();
        assert!(after.identity_handoff.is_some());
        assert_eq!(after.scope, before.scope);
        assert_eq!(after.intent, before.intent);
        assert_eq!(after.state, before.state);
        assert_eq!(after.size, before.size);
        assert_eq!(after.sha256, before.sha256);
    }

    #[tokio::test]
    async fn replacement_router_keeps_legacy_reservation_and_checkpoint_pair() {
        let (_temp, p) = fixture();
        let parent = p.parent(ROOT_ID).await.unwrap();
        let original = Node {
            id: "FILE::com.apple.CloudDocs::legacy-keynote".into(),
            parent_id: Some(parent.id.clone()),
            name: "Original.key".into(),
            kind: NodeKind::File,
            size: 1,
            etag: Some("original-A".into()),
            ..folder(ROOT_ID)
        };
        let row = {
            let mut journal = p.journal.lock().unwrap();
            let working = journal
                .create_working(
                    p.scope.clone(),
                    Node {
                        size: 0,
                        etag: None,
                        ..original.clone()
                    },
                    true,
                    &b""[..],
                )
                .unwrap();
            journal.write_working(working.id, 0, b"x").unwrap();
            let first = journal.seal_working(working.id).unwrap().unwrap();
            let claimed = journal.claim_next().unwrap().unwrap();
            journal
                .acknowledge(first.id, claimed.attempt.unwrap(), original.clone())
                .unwrap();
            let successor = journal.truncate_working(working.id, 0).unwrap();
            journal.write_working(successor.id, 0, b"new").unwrap();
            let next = journal.seal_working(successor.id).unwrap().unwrap();
            assert_eq!(next.working_file, Some(working.id));
            assert_eq!(next.base.as_ref().unwrap().predecessor, first.id);
            let claimed = journal.claim_next().unwrap().unwrap();
            assert_eq!(claimed.id, next.id);
            journal
                .reserve_identity_handoff(
                    claimed.id,
                    claimed.attempt.unwrap(),
                    ICloudFileReplace::recovery_location(claimed.id),
                )
                .unwrap();
            claimed
        };
        let request = UploadRequest {
            representation: Default::default(),
            scope: row.scope.clone(),
            intent: row.intent.clone(),
            size: row.size,
            sha256: row.sha256.clone(),
        };
        let operation = row.id.to_string();
        let expected = ICloudFileReplace::recovery_location(row.id);
        let hint = p.staged_recovery_location(&operation, &request);
        assert_eq!(
            hint,
            Some(expected.clone()),
            "legacy reservation must retain its exact txt recovery name"
        );
        let fresh = p.replacement(&operation, &request, None).await.unwrap();
        assert_eq!(
            fresh.staged_recovery_location(&operation, &request),
            hint,
            "fresh actor must adopt the complete retained legacy pair before any begin"
        );
        let producer = ICloudFileReplace::from_sealed_session_in_folder(
            p.scope.clone(),
            parent,
            original,
            "a".repeat(64),
            row.id,
            ICloudSealedSignIn {
                apple_id: p.apple_id.clone(),
                credential_id: p.credential_id.clone(),
            },
            &p.state,
        )
        .unwrap();
        let UploadStep::Prepared(checkpoint) = producer
            .begin_upload(&request, &CancellationToken::new())
            .await
            .unwrap()
        else {
            panic!("fixture checkpoint must be prepared without provider I/O")
        };
        // Simulate the actual retained version2 wire: absence means the old complete txt pair.
        let mut legacy: serde_json::Value =
            serde_json::from_str(checkpoint.expose_secret()).unwrap();
        assert!(
            legacy
                .as_object_mut()
                .unwrap()
                .remove("stage_suffix")
                .is_some()
        );
        let legacy = SecretString::from(legacy.to_string());
        let restored = p
            .replacement(&operation, &request, Some(&legacy))
            .await
            .unwrap();
        assert_eq!(
            restored.staged_recovery_location(&operation, &request),
            Some(expected)
        );
        assert!(
            p.replacement(&operation, &request, Some(&checkpoint))
                .await
                .is_err(),
            "bound key checkpoint cannot be rebound to legacy txt reservation"
        );
        let retained = p.journal.lock().unwrap().get(row.id).unwrap();
        assert_eq!(retained.scope, row.scope);
        assert_eq!(retained.intent, row.intent);
        assert_eq!(retained.state, row.state);
        assert_eq!(retained.size, row.size);
        assert_eq!(retained.sha256, row.sha256);
    }

    #[tokio::test]
    async fn replacement_router_indexed_suffix_requires_exact_identity_revision_and_scope() {
        let (_temp, p) = fixture();
        let original = Node {
            id: "FILE::com.apple.CloudDocs::indexed-keynote".into(),
            parent_id: Some(ROOT_ID.into()),
            name: "Indexed.key".into(),
            kind: NodeKind::File,
            size: 1,
            etag: Some("indexed-A".into()),
            ..folder(ROOT_ID)
        };
        Store::open(&p.metadata)
            .unwrap()
            .observe_node(&p.scope, &original)
            .unwrap();
        let row = p
            .journal
            .lock()
            .unwrap()
            .enqueue(
                p.scope.clone(),
                UploadIntent::Replace {
                    item: original.id.clone(),
                    expected_etag: original.etag.clone().unwrap(),
                },
                &b"new"[..],
            )
            .unwrap();
        let request = UploadRequest {
            representation: Default::default(),
            scope: row.scope.clone(),
            intent: row.intent.clone(),
            size: row.size,
            sha256: row.sha256.clone(),
        };
        assert_eq!(
            p.staged_recovery_location(&row.id.to_string(), &request),
            Some(ICloudFileReplace::recovery_location_for_name(
                row.id,
                "Indexed.key"
            ))
        );
        let mut foreign = request.clone();
        foreign.scope.account = Uuid::new_v4().to_string();
        assert!(
            p.staged_recovery_location(&row.id.to_string(), &foreign)
                .is_none()
        );
        for changed in [
            Node {
                etag: Some("changed-A".into()),
                ..original.clone()
            },
            Node {
                package: true,
                ..original.clone()
            },
            Node {
                parent_id: Some("FOLDER::com.apple.CloudDocs::unknown".into()),
                ..original.clone()
            },
        ] {
            Store::open(&p.metadata)
                .unwrap()
                .observe_node(&p.scope, &changed)
                .unwrap();
            assert!(
                p.staged_recovery_location(&row.id.to_string(), &request)
                    .is_none()
            );
        }
    }

    #[tokio::test]
    async fn replacement_router_uses_confirmed_predecessor_before_metadata_refresh() {
        for nested in [false, true] {
            let (_temp, p) = fixture();
            let parent = if nested {
                folder(ROOT_ID)
            } else {
                p.parent(ROOT_ID).await.unwrap()
            };
            if nested {
                Store::open(&p.metadata)
                    .unwrap()
                    .observe_node(&p.scope, &parent)
                    .unwrap();
            }
            let original = Node {
                id: "FILE::com.apple.CloudDocs::newly-confirmed".into(),
                parent_id: Some(parent.id.clone()),
                name: "Confirmed.txt".into(),
                kind: NodeKind::File,
                size: 1,
                etag: Some("receipt-revision".into()),
                ..folder(ROOT_ID)
            };
            let row = {
                let mut journal = p.journal.lock().unwrap();
                let first = journal
                    .enqueue(
                        p.scope.clone(),
                        UploadIntent::Create {
                            parent: parent.id.clone(),
                            name: original.name.clone(),
                        },
                        &b"x"[..],
                    )
                    .unwrap();
                let claimed = journal.claim_next().unwrap().unwrap();
                journal
                    .acknowledge(first.id, claimed.attempt.unwrap(), original.clone())
                    .unwrap();
                let next = journal.enqueue_after(first.id, &b"new"[..]).unwrap();
                let claimed = journal.claim_next().unwrap().unwrap();
                assert_eq!(claimed.id, next.id);
                claimed
            };
            let request = UploadRequest {
                representation: Default::default(),
                scope: row.scope.clone(),
                intent: row.intent.clone(),
                size: row.size,
                sha256: row.sha256.clone(),
            };
            let operation = row.id.to_string();
            assert_eq!(
                p.replacement(&operation, &request, None)
                    .await
                    .unwrap()
                    .parent_id(),
                parent.id
            );
            // A request-only sibling operation has no right to borrow this receipt.
            let unrelated = p
                .journal
                .lock()
                .unwrap()
                .enqueue(p.scope.clone(), row.intent.clone(), &b"new"[..])
                .unwrap();
            assert!(
                p.replacement(&unrelated.id.to_string(), &request, None)
                    .await
                    .is_err()
            );
            if nested {
                Store::open(&p.metadata)
                    .unwrap()
                    .observe_node(
                        &p.scope,
                        &Node {
                            package: true,
                            ..parent
                        },
                    )
                    .unwrap();
                assert!(p.replacement(&operation, &request, None).await.is_err());
            }
        }
    }

    #[tokio::test]
    async fn replacement_router_restores_captured_source_in_root_and_nested_folder() {
        for nested in [false, true] {
            let (_temp, p) = fixture();
            let parent = if nested {
                folder(ROOT_ID)
            } else {
                p.parent(ROOT_ID).await.unwrap()
            };
            let original = Node {
                id: "FILE::com.apple.CloudDocs::original".into(),
                parent_id: Some(parent.id.clone()),
                name: "Original.txt".into(),
                kind: NodeKind::File,
                size: 1,
                etag: Some("original-revision".into()),
                ..folder(ROOT_ID)
            };
            let mut store = Store::open(&p.metadata).unwrap();
            if nested {
                store.observe_node(&p.scope, &parent).unwrap();
            }
            store.observe_node(&p.scope, &original).unwrap();
            let row = p
                .journal
                .lock()
                .unwrap()
                .enqueue(
                    p.scope.clone(),
                    UploadIntent::Replace {
                        item: original.id.clone(),
                        expected_etag: original.etag.clone().unwrap(),
                    },
                    &b"new"[..],
                )
                .unwrap();
            let request = UploadRequest {
                representation: Default::default(),
                scope: row.scope.clone(),
                intent: row.intent.clone(),
                size: row.size,
                sha256: row.sha256.clone(),
            };
            let operation = row.id.to_string();
            assert_eq!(
                p.replacement(&operation, &request, None)
                    .await
                    .unwrap()
                    .parent_id(),
                parent.id
            );
            assert_eq!(
                p.staged_recovery_location(&operation, &request),
                Some(ICloudFileReplace::recovery_location(row.id))
            );
            // The adapter emits a real checkpoint without network I/O when the
            // fixture supplies its already-known original digest.
            let producer = ICloudFileReplace::from_sealed_session_in_folder(
                p.scope.clone(),
                parent.clone(),
                original.clone(),
                "a".repeat(64),
                row.id,
                ICloudSealedSignIn {
                    apple_id: p.apple_id.clone(),
                    credential_id: p.credential_id.clone(),
                },
                &p.state,
            )
            .unwrap();
            let UploadStep::Prepared(checkpoint) = producer
                .begin_upload(&request, &CancellationToken::new())
                .await
                .unwrap()
            else {
                panic!("replacement must persist before staging mutation")
            };
            store
                .observe_node(
                    &p.scope,
                    &Node {
                        etag: Some("later-revision".into()),
                        ..original.clone()
                    },
                )
                .unwrap();
            assert!(p.replacement(&operation, &request, None).await.is_err());
            assert_eq!(
                p.replacement(&operation, &request, Some(&checkpoint))
                    .await
                    .unwrap()
                    .parent_id(),
                parent.id
            );
            let duplicate = p
                .journal
                .lock()
                .unwrap()
                .enqueue(p.scope.clone(), row.intent, &b"new"[..])
                .unwrap();
            assert!(
                p.replacement(&duplicate.id.to_string(), &request, Some(&checkpoint))
                    .await
                    .is_err()
            );
            let mut foreign = request.clone();
            foreign.scope.account = Uuid::new_v4().to_string();
            assert!(
                p.replacement(&operation, &foreign, Some(&checkpoint))
                    .await
                    .is_err()
            );
            assert!(p.staged_recovery_location(&operation, &foreign).is_none());
            assert!(
                p.replacement(
                    &operation,
                    &request,
                    Some(&SecretString::from("{}".to_string()))
                )
                .await
                .is_err()
            );
            if nested {
                store.observe_node(&p.scope, &original).unwrap();
                store
                    .observe_node(
                        &p.scope,
                        &Node {
                            package: true,
                            ..parent
                        },
                    )
                    .unwrap();
                assert!(p.replacement(&operation, &request, None).await.is_err());
            }
        }
    }
    #[tokio::test]
    async fn native_data_replacement_refuses_app_owned_ancestor_but_accepts_plain_cloud_folder() {
        for extension in ["pages", "numbers", "key"] {
            let (_temp, provider) = fixture();
            let mut store = Store::open(&provider.metadata).unwrap();
            let root = Node {
                id: ROOT_ID.into(),
                parent_id: None,
                ..folder(ROOT_ID)
            };
            store.observe_node(&provider.scope, &root).unwrap();
            let app = Node {
                id: "FOLDER::com.apple.CloudDocs::app".into(),
                package: true,
                ..folder(ROOT_ID)
            };
            let parent = Node {
                id: "FOLDER::com.apple.CloudDocs::nested".into(),
                ..folder(&app.id)
            };
            let native = Node {
                id: "FILE::com.apple.CloudDocs::native-data".into(),
                parent_id: Some(parent.id.clone()),
                name: format!("Owned.{extension}"),
                kind: NodeKind::File,
                etag: Some("v1".into()),
                size: 3,
                package: false,
                ..folder(ROOT_ID)
            };
            for node in [&app, &parent, &native] {
                store.observe_node(&provider.scope, node).unwrap();
            }
            assert!(
                matches!(
                    provider.replacement_metadata(&native.id, "v1").await,
                    Err(UploadError::Invalid)
                ),
                "{extension}"
            );
            // Only the enclosing classification changes. The valid counterpart
            // proves rejection is not an incomplete node chain or missing leaf.
            store
                .observe_node(
                    &provider.scope,
                    &Node {
                        package: false,
                        ..app
                    },
                )
                .unwrap();
            let (selected, selected_parent) = provider
                .replacement_metadata(&native.id, "v1")
                .await
                .unwrap();
            assert_eq!(selected, native);
            assert_eq!(selected_parent, parent);
            assert!(
                provider
                    .replacement_metadata(&native.id, "different")
                    .await
                    .is_err()
            );
        }
    }
}
