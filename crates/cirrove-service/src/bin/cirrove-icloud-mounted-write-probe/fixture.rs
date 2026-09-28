//! The only writable iCloud namespace here is a freshly created Cirrove test folder.
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
use cirrove_icloud::{ICloudDrive, ICloudOwnedFixtureFolderCreate, ICloudOwnedFixtureUpload};
use secrecy::SecretString;
use std::{collections::HashSet, fs::File, sync::Mutex};

pub struct Fixture {
    pub scope: Scope,
    pub root: Node,
    pub read: ICloudDrive,
    pub upload: ICloudOwnedFixtureUpload,
    pub folders: ICloudOwnedFixtureFolderCreate,
    owned: Mutex<HashSet<String>>,
}

impl Fixture {
    pub fn new(
        scope: Scope,
        root: Node,
        read: ICloudDrive,
        upload: ICloudOwnedFixtureUpload,
        folders: ICloudOwnedFixtureFolderCreate,
    ) -> Self {
        Self {
            scope,
            root,
            read,
            upload,
            folders,
            owned: Mutex::new(HashSet::new()),
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
        if scope != &self.scope || parent != self.root.id {
            return Err(ProviderError::Permission);
        }
        let mut page = self.read.children(scope, parent, cursor, cancel).await?;
        page.nodes.retain(|node| self.owns(scope, &node.id));
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
        self.guard_folder(r)?;
        let receipt = self
            .folders
            .mutate_operation(operation, r, prepared, c)
            .await?;
        self.folder_receipt(r, &receipt)?;
        Ok(receipt)
    }
    async fn reconcile_operation(
        &self,
        operation: &str,
        r: &MutationRequest,
        prepared: Option<&str>,
        c: &CancellationToken,
    ) -> cirrove_core::mutation::Result<MutationReconciliation> {
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
}
