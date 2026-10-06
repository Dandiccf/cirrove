//! Authorization wrapper for a mounted validation fixture, not a write adapter.
//! All protocol/checkpoint behavior stays in the regular account writer.
use super::super::*;
use crate::journal::{MutationState, UploadJournal};
use cirrove_core::{
    mutation::{
        DeletionSupport, MutationError, MutationIntent, MutationProvider, MutationReceipt,
        MutationReconciliation, MutationRequest,
    },
    upload::{
        Reconciliation, RecoveryLocation, UploadError, UploadProvider, UploadRequest, UploadStep,
    },
};
use std::{collections::HashMap, fs::File, sync::Mutex, time::Duration};

#[derive(Clone)]
pub(super) struct Owned {
    pub scope: Scope,
    pub root: Node,
    pub journal: Arc<Mutex<UploadJournal>>,
}
impl Owned {
    pub fn nodes(&self) -> Result<HashMap<String, Node>> {
        let journal = self
            .journal
            .lock()
            .map_err(|_| anyhow::anyhow!("journal lock failed"))?;
        let uploads = journal.list(0, 256)?;
        let mutations = journal.list_mutations(0, 256)?;
        ensure!(
            uploads.len() < 256 && mutations.len() < 256,
            "validation ownership limit exceeded"
        );
        let mut nodes = HashMap::from([(self.root.id.clone(), self.root.clone())]);
        let mut receipts: Vec<_> = uploads
            .into_iter()
            .filter(|row| row.scope == self.scope && row.state == UploadState::Uploaded)
            .filter_map(|row| {
                let source = match row.intent {
                    UploadIntent::Replace { item, .. } => Some(item),
                    _ => None,
                };
                row.remote.map(|node| (row.sequence, node, source))
            })
            .collect();
        receipts.extend(
            mutations
                .into_iter()
                .filter(|row| {
                    row.request.scope == self.scope && row.state == MutationState::Applied
                })
                .filter_map(|row| {
                    let source = row.request.intent.before().map(|node| node.id.clone());
                    match row.receipt {
                        Some(MutationReceipt::Upsert(node)) => Some((row.sequence, node, source)),
                        _ => None,
                    }
                }),
        );
        receipts.sort_by_key(|(sequence, _, _)| *sequence);
        // Keep old IDs too: recovery of an in-flight handoff is authorized by
        // its historical confirmed receipt, never by a matching current path.
        for (_, node, source) in receipts {
            let inside = node
                .parent_id
                .as_ref()
                .and_then(|id| nodes.get(id))
                .is_some_and(|parent| parent.kind == NodeKind::Folder);
            let known_source = source.as_ref().is_none_or(|id| {
                id != &self.root.id && nodes.get(id).is_some_and(|before| before.kind == node.kind)
            });
            if inside
                && known_source
                && node.id != self.root.id
                && !node.package
                && node.target.is_none()
            {
                nodes.insert(node.id.clone(), node);
            }
        }
        Ok(nodes)
    }
    fn upload(&self, r: &UploadRequest) -> cirrove_core::upload::Result<()> {
        r.require_file_bytes()?;
        if r.scope != self.scope {
            return Err(UploadError::Invalid);
        }
        let nodes = self.nodes().map_err(|_| UploadError::Uncertain)?;
        let (id, kind) = match &r.intent {
            UploadIntent::Create { parent, .. } => (parent, NodeKind::Folder),
            UploadIntent::Replace { item, .. } => (item, NodeKind::File),
        };
        if !nodes
            .get(id)
            .is_some_and(|node| node.kind == kind && !node.package && node.target.is_none())
        {
            return Err(UploadError::Invalid);
        }
        Ok(())
    }
    fn mutation(&self, r: &MutationRequest) -> cirrove_core::mutation::Result<()> {
        if r.scope != self.scope {
            return Err(MutationError::Invalid);
        }
        let nodes = self.nodes().map_err(|_| MutationError::Uncertain)?;
        if let Some(before) = r.intent.before()
            && (before.id == self.root.id
                || !nodes
                    .get(&before.id)
                    .is_some_and(|node| node.kind == before.kind))
        {
            return Err(MutationError::Invalid);
        }
        let parent = match &r.intent {
            MutationIntent::CreateFolder { parent, .. }
            | MutationIntent::Relocate { parent, .. } => Some(parent),
            _ => None,
        };
        if parent.is_some_and(|id| {
            !nodes.get(id).is_some_and(|node| {
                node.kind == NodeKind::Folder && !node.package && node.target.is_none()
            })
        }) {
            return Err(MutationError::Invalid);
        }
        Ok(())
    }
}

pub(super) struct Guarded {
    pub inner: ICloudWriteProvider,
    pub owned: Owned,
    pub boundary: Option<Arc<super::recovery::Boundary>>,
    pub competing: Option<Arc<super::competing::Boundary>>,
    pub relocation: Option<Arc<super::relocation::Boundary>>,
    pub deletion: Option<Arc<super::deletion::Boundary>>,
}

#[async_trait::async_trait]
impl UploadProvider for Guarded {
    fn requires_begin_payload(&self, r: &UploadRequest) -> bool {
        self.owned.upload(r).is_ok() && self.inner.requires_begin_payload(r)
    }
    fn begin_is_mutation_free_until_checkpoint(&self, r: &UploadRequest) -> bool {
        self.owned.upload(r).is_ok() && self.inner.begin_is_mutation_free_until_checkpoint(r)
    }
    fn inspection_timeout(&self, r: &UploadRequest) -> Duration {
        self.inner.inspection_timeout(r)
    }
    fn commit_timeout(&self, r: &UploadRequest) -> Duration {
        self.inner.commit_timeout(r)
    }
    fn staged_recovery_location(&self, o: &str, r: &UploadRequest) -> Option<RecoveryLocation> {
        self.owned.upload(r).ok()?;
        self.inner.staged_recovery_location(o, r)
    }
    async fn begin_upload(
        &self,
        _: &UploadRequest,
        _: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        Err(UploadError::Invalid)
    }
    async fn inspect_upload(
        &self,
        _: &UploadRequest,
        _: &SecretString,
        _: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        Err(UploadError::Invalid)
    }
    async fn upload_part(
        &self,
        _: &UploadRequest,
        _: &SecretString,
        _: u64,
        _: Vec<u8>,
        _: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        Err(UploadError::Invalid)
    }
    async fn commit_upload(
        &self,
        _: &UploadRequest,
        _: &SecretString,
        _: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        Err(UploadError::Invalid)
    }
    async fn reconcile_upload(
        &self,
        _: &UploadRequest,
        _: Option<&SecretString>,
        _: &CancellationToken,
    ) -> cirrove_core::upload::Result<Reconciliation> {
        Err(UploadError::Invalid)
    }
    async fn begin_upload_for_operation(
        &self,
        o: &str,
        r: &UploadRequest,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.owned.upload(r)?;
        self.inner.begin_upload_for_operation(o, r, c).await
    }
    async fn begin_upload_from_payload_for_operation(
        &self,
        o: &str,
        r: &UploadRequest,
        f: File,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.owned.upload(r)?;
        self.inner
            .begin_upload_from_payload_for_operation(o, r, f, c)
            .await
    }
    async fn inspect_upload_for_operation(
        &self,
        o: &str,
        r: &UploadRequest,
        s: &SecretString,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.owned.upload(r)?;
        self.inner.inspect_upload_for_operation(o, r, s, c).await
    }
    async fn upload_stream_for_operation(
        &self,
        o: &str,
        r: &UploadRequest,
        s: &SecretString,
        f: File,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.owned.upload(r)?;
        self.inner.upload_stream_for_operation(o, r, s, f, c).await
    }
    async fn commit_upload_for_operation(
        &self,
        o: &str,
        r: &UploadRequest,
        s: &SecretString,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.owned.upload(r)?;
        if let Some(boundary) = &self.boundary {
            boundary.before_commit(r, s)?;
        }
        if let Some(competing) = &self.competing {
            competing.before_commit(o, r, s).await?;
        }
        let step = self.inner.commit_upload_for_operation(o, r, s, c).await?;
        if let Some(boundary) = &self.boundary {
            boundary.after_commit(o, r, s, &step)?;
        }
        Ok(step)
    }
    async fn reconcile_upload_for_operation(
        &self,
        o: &str,
        r: &UploadRequest,
        s: Option<&SecretString>,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<Reconciliation> {
        self.owned.upload(r)?;
        self.inner.reconcile_upload_for_operation(o, r, s, c).await
    }
}
#[async_trait::async_trait]
impl MutationProvider for Guarded {
    fn deletion(&self) -> DeletionSupport {
        self.inner.deletion()
    }
    async fn mutate(
        &self,
        _: &MutationRequest,
        _: &CancellationToken,
    ) -> cirrove_core::mutation::Result<MutationReceipt> {
        Err(MutationError::Invalid)
    }
    async fn reconcile_mutation(
        &self,
        _: &MutationRequest,
        _: &CancellationToken,
    ) -> cirrove_core::mutation::Result<MutationReconciliation> {
        Err(MutationError::Invalid)
    }
    async fn prepare_mutation_for_operation(
        &self,
        o: &str,
        r: &MutationRequest,
        c: &CancellationToken,
    ) -> cirrove_core::mutation::Result<Option<String>> {
        self.owned.mutation(r)?;
        self.inner.prepare_mutation_for_operation(o, r, c).await
    }
    async fn mutate_operation(
        &self,
        o: &str,
        r: &MutationRequest,
        p: Option<&str>,
        c: &CancellationToken,
    ) -> cirrove_core::mutation::Result<MutationReceipt> {
        self.owned.mutation(r)?;
        if let Some(boundary) = &self.relocation {
            boundary.before(o, r)?;
        }
        if let Some(boundary) = &self.deletion {
            boundary.before(o, r)?;
        }
        let receipt = self.inner.mutate_operation(o, r, p, c).await?;
        if let Some(boundary) = &self.deletion {
            boundary.after(o, r, &receipt)?;
        }
        if let Some(boundary) = &self.relocation {
            boundary.after(o, r, &receipt)?;
        }
        Ok(receipt)
    }
    async fn reconcile_operation(
        &self,
        o: &str,
        r: &MutationRequest,
        p: Option<&str>,
        c: &CancellationToken,
    ) -> cirrove_core::mutation::Result<MutationReconciliation> {
        self.owned.mutation(r)?;
        self.inner.reconcile_operation(o, r, p, c).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mounted_authority_requires_confirmed_in_tree_receipts_and_survives_reopen() {
        let temp = tempfile::tempdir().expect("fixture");
        let scope = Scope {
            account: Uuid::new_v4().to_string(),
            provider: "icloud".into(),
            collection: "drive".into(),
        };
        let root = Node {
            id: "owned-root".into(),
            parent_id: Some(ROOT_ID.into()),
            name: "fixture".into(),
            kind: NodeKind::Folder,
            size: 0,
            modified_unix: 0,
            etag: None,
            content_version: None,
            target: None,
            package: false,
        };
        let journal = Arc::new(Mutex::new(
            UploadJournal::open(&temp.path().join("journal"), &scope.account, 1024 * 1024)
                .expect("journal"),
        ));
        let owned = Owned {
            scope: scope.clone(),
            root: root.clone(),
            journal,
        };
        let create = UploadRequest {
            representation: Default::default(),
            scope: scope.clone(),
            intent: UploadIntent::Create {
                parent: root.id.clone(),
                name: NAME.into(),
            },
            size: 1,
            sha256: hex::encode(Sha256::digest(b"x")),
        };
        assert!(owned.upload(&create).is_ok());
        let file = Node {
            id: "owned-file".into(),
            parent_id: Some(root.id.clone()),
            name: NAME.into(),
            kind: NodeKind::File,
            size: 1,
            etag: Some("revision".into()),
            ..root.clone()
        };
        let replacement = UploadRequest {
            representation: Default::default(),
            intent: UploadIntent::Replace {
                item: file.id.clone(),
                expected_etag: "revision".into(),
            },
            ..create.clone()
        };
        {
            let mut journal = owned.journal.lock().expect("lock");
            journal
                .enqueue(scope.clone(), create.intent.clone(), &b"x"[..])
                .expect("queue");
        }
        assert!(owned.upload(&replacement).is_err());
        {
            let mut journal = owned.journal.lock().expect("lock");
            let row = journal.claim_next().expect("claim").expect("row");
            journal
                .acknowledge(row.id, row.attempt.expect("attempt"), file.clone())
                .expect("receipt");
        }
        assert!(owned.upload(&replacement).is_ok());
        let foreign = Node {
            id: "foreign-file".into(),
            parent_id: Some("foreign-root".into()),
            ..file.clone()
        };
        {
            let mut journal = owned.journal.lock().expect("lock");
            journal
                .enqueue(
                    scope.clone(),
                    UploadIntent::Create {
                        parent: "foreign-root".into(),
                        name: NAME.into(),
                    },
                    &b"x"[..],
                )
                .expect("foreign fixture queue");
            let row = journal.claim_next().expect("claim").expect("row");
            journal
                .acknowledge(row.id, row.attempt.expect("attempt"), foreign.clone())
                .expect("foreign fixture receipt");
        }
        assert!(!owned.nodes().expect("nodes").contains_key(&foreign.id));
        assert!(
            owned
                .upload(&UploadRequest {
                    representation: Default::default(),
                    scope: Scope {
                        account: "other-account".into(),
                        ..scope.clone()
                    },
                    ..create.clone()
                })
                .is_err()
        );
        assert!(
            owned
                .upload(&UploadRequest {
                    representation: Default::default(),
                    intent: UploadIntent::Create {
                        parent: "foreign-root".into(),
                        name: NAME.into()
                    },
                    ..create
                })
                .is_err()
        );
        assert!(
            owned
                .mutation(&MutationRequest {
                    scope: scope.clone(),
                    intent: MutationIntent::RemoveFolder {
                        before: root.clone()
                    }
                })
                .is_err()
        );
        drop(owned);
        let reopened = Owned {
            scope: scope.clone(),
            root,
            journal: Arc::new(Mutex::new(
                UploadJournal::open(&temp.path().join("journal"), &scope.account, 1024 * 1024)
                    .expect("reopen"),
            )),
        };
        assert!(reopened.upload(&replacement).is_ok());
        assert!(!reopened.nodes().expect("nodes").contains_key(&foreign.id));
    }
}
