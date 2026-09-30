use super::*;
use cirrove_icloud::{ICloudFileReplace, ICloudSealedSignIn};

impl ICloudWriteProvider {
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
        let sign_in = ICloudSealedSignIn {
            apple_id: self.apple_id.clone(),
            credential_id: self.credential_id.clone(),
        };
        if let Some(checkpoint) = checkpoint {
            // Never reconstruct a checkpoint-bearing replacement from the active
            // index: its original may already be in Trash after a prior phase.
            return ICloudFileReplace::from_sealed_checkpoint(
                request,
                operation,
                checkpoint,
                sign_in,
                &self.state,
            )?
            .ok_or(UploadError::CheckpointInvalid);
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
        ICloudFileReplace::from_sealed_session_for_existing(
            self.scope.clone(),
            folder,
            original,
            operation,
            sign_in,
            &self.state,
        )
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
}
