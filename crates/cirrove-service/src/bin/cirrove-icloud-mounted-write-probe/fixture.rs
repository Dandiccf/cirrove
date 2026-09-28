//! The only writable iCloud namespace here is a freshly created Cirrove test folder.
use anyhow::{Result, ensure};
use cirrove_core::mutation::{
    MutationError, MutationIntent, MutationProvider, MutationReceipt, MutationReconciliation,
    MutationRequest,
};
use cirrove_core::upload::{
    Reconciliation, UploadError, UploadIntent, UploadProvider, UploadRequest, UploadStep,
};
use cirrove_core::{
    CancellationToken, Change, ChangePage, Checkpoint, Cursor, DirectoryPage, MetadataProvider,
    Node, NodeKind, ProviderError, ReadProvider, Scope,
};
use cirrove_icloud::{
    ICloudDrive, ICloudOwnedFixtureFolderCreate, ICloudOwnedFixtureFolderRemove,
    ICloudOwnedFixtureUpload, ICloudReadSession, ValidationFolder,
};
use cirrove_service::journal::{MutationState, UploadJournal, UploadState};
use secrecy::SecretString;
use std::{collections::HashSet, fs::File, sync::Mutex};

pub struct Fixture {
    pub scope: Scope,
    pub root: Node,
    pub read: ICloudDrive,
    pub upload: ICloudOwnedFixtureUpload,
    pub folders: ICloudOwnedFixtureFolderCreate,
    removal: RemovalContext,
    owned: Mutex<HashSet<String>>,
}

pub struct RemovalContext {
    pub folder: ValidationFolder,
    pub apple_id: String,
    pub snapshot: SecretString,
}

impl Fixture {
    pub fn new(
        scope: Scope,
        root: Node,
        read: ICloudDrive,
        upload: ICloudOwnedFixtureUpload,
        folders: ICloudOwnedFixtureFolderCreate,
        removal: RemovalContext,
        owned: HashSet<String>,
    ) -> Self {
        Self {
            scope,
            root,
            read,
            upload,
            folders,
            removal,
            owned: Mutex::new(owned),
        }
    }

    fn owns(&self, scope: &Scope, id: &str) -> bool {
        scope == &self.scope
            && (id == self.root.id || self.owned.lock().is_ok_and(|set| set.contains(id)))
    }

    fn guard_upload(&self, request: &UploadRequest) -> cirrove_core::upload::Result<()> {
        if request.scope != self.scope
            || !matches!(&request.intent, UploadIntent::Create { parent, .. } if parent == &self.root.id)
        {
            return Err(UploadError::Invalid);
        }
        request.validate()
    }

    fn upload_step(
        &self,
        request: &UploadRequest,
        step: UploadStep,
    ) -> cirrove_core::upload::Result<UploadStep> {
        if let UploadStep::Complete(node) = &step {
            self.upload_receipt(request, node)?;
        }
        Ok(step)
    }

    fn upload_receipt(
        &self,
        request: &UploadRequest,
        node: &Node,
    ) -> cirrove_core::upload::Result<()> {
        let UploadIntent::Create { name, .. } = &request.intent else {
            return Err(UploadError::Invalid);
        };
        if node.kind != NodeKind::File
            || node.target.is_some()
            || node.parent_id.as_ref() != Some(&self.root.id)
            || &node.name != name
        {
            return Err(UploadError::Uncertain);
        }
        self.owned
            .lock()
            .map_err(|_| UploadError::Uncertain)?
            .insert(node.id.clone());
        Ok(())
    }

    fn guard_folder(&self, request: &MutationRequest) -> cirrove_core::mutation::Result<()> {
        if request.scope != self.scope
            || !matches!(&request.intent, MutationIntent::CreateFolder { parent, .. } if parent == &self.root.id)
        {
            return Err(MutationError::Invalid);
        }
        request.validate()
    }

    fn folder_receipt(
        &self,
        request: &MutationRequest,
        receipt: &MutationReceipt,
    ) -> cirrove_core::mutation::Result<()> {
        if !request.accepts(receipt) {
            return Err(MutationError::Uncertain);
        }
        let MutationReceipt::Upsert(node) = receipt else {
            return Err(MutationError::Uncertain);
        };
        self.owned
            .lock()
            .map_err(|_| MutationError::Uncertain)?
            .insert(node.id.clone());
        Ok(())
    }

    fn guard_remove_folder(
        &self,
        request: &MutationRequest,
    ) -> cirrove_core::mutation::Result<Node> {
        request.validate()?;
        let MutationIntent::RemoveFolder { before } = &request.intent else {
            return Err(MutationError::Invalid);
        };
        if request.scope != self.scope
            || before.kind != NodeKind::Folder
            || before.parent_id.as_ref() != Some(&self.root.id)
            || before.id == self.root.id
            || !self.owns(&request.scope, &before.id)
        {
            return Err(MutationError::Invalid);
        }
        Ok(before.clone())
    }

    fn remover(
        &self,
        before: Node,
    ) -> cirrove_core::mutation::Result<ICloudOwnedFixtureFolderRemove> {
        let session = ICloudReadSession::from_session_snapshot(
            &self.removal.snapshot,
            &self.removal.apple_id,
        )
        .map_err(|_| MutationError::Uncertain)?;
        ICloudOwnedFixtureFolderRemove::new(
            self.scope.clone(),
            session,
            self.removal.folder.clone(),
            before,
        )
    }

    fn removed_receipt(
        &self,
        request: &MutationRequest,
        receipt: &MutationReceipt,
    ) -> cirrove_core::mutation::Result<()> {
        let before = self.guard_remove_folder(request)?;
        if !request.accepts(receipt) {
            return Err(MutationError::Uncertain);
        }
        self.owned
            .lock()
            .map_err(|_| MutationError::Uncertain)?
            .remove(&before.id);
        Ok(())
    }
}

fn confirmed_file(
    scope: &Scope,
    root: &str,
    request_scope: &Scope,
    intent: &UploadIntent,
    node: &Node,
) -> bool {
    let UploadIntent::Create { parent, name } = intent else {
        return false;
    };
    request_scope == scope
        && parent == root
        && !node.id.is_empty()
        && node.kind == NodeKind::File
        && node.parent_id.as_deref() == Some(root)
        && &node.name == name
        && node.etag.as_ref().is_some_and(|etag| !etag.is_empty())
        && node.target.is_none()
        && !node.package
}

fn confirmed_folder(
    scope: &Scope,
    root: &str,
    request: &MutationRequest,
    receipt: &MutationReceipt,
) -> bool {
    request.scope == *scope
        && matches!(&request.intent, MutationIntent::CreateFolder { parent, .. } if parent == root)
        && request.accepts(receipt)
        && matches!(receipt, MutationReceipt::Upsert(node)
            if node.kind == NodeKind::Folder && !node.id.is_empty() && node.target.is_none() && !node.package)
}

/// Recover only identities whose durable, successful receipts belong to this
/// exact account, collection and run-owned root. An uncertain operation stays
/// under the shared worker's reconciliation instead of becoming readable by a
/// same-name guess. No provider request occurs while the journal is read.
pub fn restored_owned(
    journal: &UploadJournal,
    scope: &Scope,
    root: &str,
) -> Result<HashSet<String>> {
    let mut owned = HashSet::new();
    let mut after = 0;
    loop {
        let rows = journal.list(after, 256)?;
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            ensure!(
                row.scope == *scope,
                "foreign account in isolated upload journal"
            );
            ensure!(
                matches!(&row.intent, UploadIntent::Create { parent, .. } if parent == root),
                "non-fixture upload in isolated journal"
            );
            if row.state == UploadState::Uploaded {
                let node = row
                    .remote
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("uploaded fixture lacks remote receipt"))?;
                ensure!(
                    confirmed_file(scope, root, &row.scope, &row.intent, node),
                    "uploaded fixture receipt does not match its create intent"
                );
                ensure!(
                    owned.insert(node.id.clone()),
                    "duplicate remote identity in isolated journal"
                );
            }
        }
        after = rows.last().expect("nonempty page").sequence;
        ensure!(after <= 100_000, "isolated fixture journal exceeds bound");
    }
    after = 0;
    loop {
        let rows = journal.list_mutations(after, 256)?;
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            ensure!(
                row.request.scope == *scope,
                "foreign account in isolated mutation journal"
            );
            match &row.request.intent {
                MutationIntent::CreateFolder { parent, .. } => ensure!(
                    parent == root,
                    "non-fixture folder create in isolated journal"
                ),
                MutationIntent::RemoveFolder { before } => ensure!(
                    before.parent_id.as_deref() == Some(root)
                        && before.kind == NodeKind::Folder
                        && owned.contains(&before.id),
                    "non-fixture folder removal in isolated journal"
                ),
                _ => anyhow::bail!("unsupported mutation in isolated journal"),
            }
            if row.state == MutationState::Applied {
                let receipt = row
                    .receipt
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("applied folder lacks remote receipt"))?;
                match (&row.request.intent, receipt) {
                    (MutationIntent::CreateFolder { .. }, MutationReceipt::Upsert(node)) => {
                        ensure!(
                            confirmed_folder(scope, root, &row.request, receipt),
                            "applied folder receipt does not match its create intent"
                        );
                        ensure!(
                            owned.insert(node.id.clone()),
                            "duplicate remote identity in isolated journal"
                        );
                    }
                    (
                        MutationIntent::RemoveFolder { before },
                        MutationReceipt::Removed { item },
                    ) => {
                        ensure!(
                            item == &before.id && row.request.accepts(receipt),
                            "removed folder receipt does not match its owned identity"
                        );
                        owned.remove(item);
                    }
                    _ => anyhow::bail!("unexpected folder receipt in isolated journal"),
                }
            }
        }
        after = rows.last().expect("nonempty page").sequence;
        ensure!(after <= 100_000, "isolated fixture journal exceeds bound");
    }
    Ok(owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> Scope {
        Scope {
            account: "account-a".into(),
            provider: "icloud".into(),
            collection: "drive".into(),
        }
    }

    fn node(id: &str, kind: NodeKind, name: &str) -> Node {
        Node {
            id: id.into(),
            parent_id: Some("fixture-root".into()),
            name: name.into(),
            kind,
            size: 2,
            modified_unix: 0,
            etag: Some("etag".into()),
            content_version: None,
            target: None,
            package: false,
        }
    }

    #[test]
    fn recovered_ids_require_exact_scope_parent_intent_and_receipt() {
        let scope = scope();
        let file = node("file-a", NodeKind::File, "Owned.txt");
        let intent = UploadIntent::Create {
            parent: "fixture-root".into(),
            name: "Owned.txt".into(),
        };
        assert!(confirmed_file(
            &scope,
            "fixture-root",
            &scope,
            &intent,
            &file
        ));
        let foreign = Scope {
            account: "other-account".into(),
            ..scope.clone()
        };
        assert!(!confirmed_file(
            &scope,
            "fixture-root",
            &foreign,
            &intent,
            &file
        ));
        assert!(!confirmed_file(
            &scope,
            "other-root",
            &scope,
            &intent,
            &file
        ));
        let request = MutationRequest {
            scope: scope.clone(),
            intent: MutationIntent::CreateFolder {
                parent: "fixture-root".into(),
                name: "Owned Folder".into(),
            },
        };
        let folder = node("folder-a", NodeKind::Folder, "Owned Folder");
        assert!(confirmed_folder(
            &scope,
            "fixture-root",
            &request,
            &MutationReceipt::Upsert(folder.clone())
        ));
        assert!(!confirmed_folder(
            &scope,
            "other-root",
            &request,
            &MutationReceipt::Upsert(folder)
        ));
    }
}

#[async_trait::async_trait]
impl MetadataProvider for Fixture {
    fn provider_id(&self) -> &'static str {
        "icloud"
    }

    async fn changes(
        &self,
        scope: &Scope,
        _cursor: Option<&Cursor>,
        _cancel: &CancellationToken,
    ) -> Result<ChangePage, ProviderError> {
        if scope != &self.scope {
            return Err(ProviderError::Permission);
        }
        Ok(ChangePage {
            changes: vec![Change::Upsert(self.root.clone())],
            checkpoint: Checkpoint::Complete(Cursor("isolated-icloud-fixture".into())),
        })
    }
}

#[async_trait::async_trait]
impl ReadProvider for Fixture {
    fn unknown_directories_require_fetch(&self) -> bool {
        true
    }

    async fn node(
        &self,
        scope: &Scope,
        id: &str,
        cancel: &CancellationToken,
    ) -> Result<Node, ProviderError> {
        if !self.owns(scope, id) {
            return Err(ProviderError::Permission);
        }
        if id == self.root.id {
            return Ok(self.root.clone());
        }
        let page = self
            .read
            .children(scope, &self.root.id, None, cancel)
            .await?;
        page.nodes
            .into_iter()
            .find(|node| node.id == id && node.parent_id.as_ref() == Some(&self.root.id))
            .ok_or(ProviderError::Unavailable)
    }

    async fn children(
        &self,
        scope: &Scope,
        parent: &str,
        cursor: Option<&Cursor>,
        cancel: &CancellationToken,
    ) -> Result<DirectoryPage, ProviderError> {
        if scope != &self.scope || !self.owns(scope, parent) {
            return Err(ProviderError::Permission);
        }
        if parent != self.root.id
            && self.node(scope, parent, cancel).await?.kind != NodeKind::Folder
        {
            return Err(ProviderError::Permission);
        }
        let mut page = self.read.children(scope, parent, cursor, cancel).await?;
        if parent == self.root.id {
            page.nodes.retain(|node| self.owns(scope, &node.id));
        } else if page.nodes.iter().any(|node| !self.owns(scope, &node.id)) {
            // A fresh validation folder can still have been changed by another
            // client. Do not project an incomplete empty view and let FUSE
            // accept rmdir before the provider rejects a nonempty folder.
            return Err(ProviderError::Unavailable);
        }
        Ok(page)
    }

    async fn read_range(
        &self,
        scope: &Scope,
        node: &Node,
        offset: u64,
        length: u32,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>, ProviderError> {
        if !self.owns(scope, &node.id)
            || node.id == self.root.id
            || node.parent_id.as_ref() != Some(&self.root.id)
        {
            return Err(ProviderError::Permission);
        }
        self.read
            .read_range(scope, node, offset, length, cancel)
            .await
    }
}

#[async_trait::async_trait]
impl UploadProvider for Fixture {
    async fn begin_upload(
        &self,
        r: &UploadRequest,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.guard_upload(r)?;
        self.upload_step(r, self.upload.begin_upload(r, c).await?)
    }
    async fn inspect_upload(
        &self,
        r: &UploadRequest,
        s: &SecretString,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.guard_upload(r)?;
        self.upload_step(r, self.upload.inspect_upload(r, s, c).await?)
    }
    async fn upload_part(
        &self,
        r: &UploadRequest,
        s: &SecretString,
        o: u64,
        b: Vec<u8>,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.guard_upload(r)?;
        self.upload_step(r, self.upload.upload_part(r, s, o, b, c).await?)
    }
    async fn upload_stream(
        &self,
        r: &UploadRequest,
        s: &SecretString,
        f: File,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.guard_upload(r)?;
        self.upload_step(r, self.upload.upload_stream(r, s, f, c).await?)
    }
    async fn commit_upload(
        &self,
        r: &UploadRequest,
        s: &SecretString,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.guard_upload(r)?;
        self.upload_step(r, self.upload.commit_upload(r, s, c).await?)
    }
    async fn reconcile_upload(
        &self,
        r: &UploadRequest,
        s: Option<&SecretString>,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<Reconciliation> {
        self.guard_upload(r)?;
        let result = self.upload.reconcile_upload(r, s, c).await?;
        if let Reconciliation::Committed(node) = &result {
            self.upload_receipt(r, node)?;
        }
        Ok(result)
    }
}

#[async_trait::async_trait]
impl MutationProvider for Fixture {
    async fn prepare_mutation(
        &self,
        r: &MutationRequest,
        c: &CancellationToken,
    ) -> cirrove_core::mutation::Result<Option<String>> {
        match &r.intent {
            MutationIntent::CreateFolder { .. } => {
                self.guard_folder(r)?;
                Ok(None)
            }
            MutationIntent::RemoveFolder { .. } => {
                let before = self.guard_remove_folder(r)?;
                self.remover(before)?.prepare_mutation(r, c).await
            }
            _ => Err(MutationError::Unsupported(
                "isolated iCloud fixture operation",
            )),
        }
    }
    async fn mutate(
        &self,
        _r: &MutationRequest,
        _c: &CancellationToken,
    ) -> cirrove_core::mutation::Result<MutationReceipt> {
        Err(MutationError::Unsupported(
            "isolated folder create requires a durable operation ID",
        ))
    }
    async fn reconcile_mutation(
        &self,
        _r: &MutationRequest,
        _c: &CancellationToken,
    ) -> cirrove_core::mutation::Result<MutationReconciliation> {
        Ok(MutationReconciliation::Indeterminate)
    }
    async fn mutate_operation(
        &self,
        operation: &str,
        r: &MutationRequest,
        prepared: Option<&str>,
        c: &CancellationToken,
    ) -> cirrove_core::mutation::Result<MutationReceipt> {
        match &r.intent {
            MutationIntent::CreateFolder { .. } => {
                self.guard_folder(r)?;
                let receipt = self
                    .folders
                    .mutate_operation(operation, r, prepared, c)
                    .await?;
                self.folder_receipt(r, &receipt)?;
                Ok(receipt)
            }
            MutationIntent::RemoveFolder { .. } => {
                let before = self.guard_remove_folder(r)?;
                let receipt = self
                    .remover(before)?
                    .mutate_prepared(r, prepared, c)
                    .await?;
                self.removed_receipt(r, &receipt)?;
                Ok(receipt)
            }
            _ => Err(MutationError::Unsupported(
                "isolated iCloud fixture operation",
            )),
        }
    }
    async fn reconcile_operation(
        &self,
        operation: &str,
        r: &MutationRequest,
        prepared: Option<&str>,
        c: &CancellationToken,
    ) -> cirrove_core::mutation::Result<MutationReconciliation> {
        match &r.intent {
            MutationIntent::CreateFolder { .. } => {
                self.guard_folder(r)?;
                let result = self
                    .folders
                    .reconcile_operation(operation, r, prepared, c)
                    .await?;
                if let MutationReconciliation::Applied(receipt) = &result {
                    self.folder_receipt(r, receipt)?;
                }
                Ok(result)
            }
            MutationIntent::RemoveFolder { .. } => {
                let before = self.guard_remove_folder(r)?;
                let result = self
                    .remover(before)?
                    .reconcile_prepared_mutation(r, prepared, c)
                    .await?;
                if let MutationReconciliation::Applied(receipt) = &result {
                    self.removed_receipt(r, receipt)?;
                }
                Ok(result)
            }
            _ => Err(MutationError::Unsupported(
                "isolated iCloud fixture operation",
            )),
        }
    }
}
