//! The only writable iCloud namespace here is a freshly created Cirrove test tree.
use anyhow::{Result, ensure};
use cirrove_core::mutation::{
    MutationError, MutationIntent, MutationProvider, MutationReceipt, MutationReconciliation,
    MutationRequest,
};
use cirrove_core::upload::{
    Reconciliation, RecoveryLocation, UploadError, UploadIntent, UploadProvider, UploadRequest,
    UploadStep,
};
use cirrove_core::{
    CancellationToken, Change, ChangePage, Checkpoint, Cursor, DirectoryPage, MetadataProvider,
    Node, NodeKind, ProviderError, ReadProvider, Scope,
};
use cirrove_icloud::{
    ICloudDrive, ICloudFileCreate, ICloudFileMove, ICloudFileRename, ICloudFileTrash,
    ICloudFolderCreate, ICloudFolderMove, ICloudFolderRename, ICloudFolderTrash,
    ICloudOwnedMountedReplace, ICloudSealedSignIn,
};
use cirrove_service::journal::{MutationState, UploadJournal, UploadRecord, UploadState};
use cirrove_store::Store;
use secrecy::SecretString;
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use uuid::Uuid;

/// Probe-only phase timing. A dropped future is reported separately from a
/// completed provider error; neither case prints provider responses or IDs.
struct ReplacementPhase {
    name: &'static str,
    started: Instant,
    finished: bool,
}

impl ReplacementPhase {
    fn start(name: &'static str) -> Self {
        eprintln!("replacement provider {name}: start");
        Self {
            name,
            started: Instant::now(),
            finished: false,
        }
    }

    fn finish<T>(
        mut self,
        result: cirrove_core::upload::Result<T>,
    ) -> cirrove_core::upload::Result<T> {
        self.finished = true;
        eprintln!(
            "replacement provider {}: end {:.1}s success={}",
            self.name,
            self.started.elapsed().as_secs_f64(),
            result.is_ok()
        );
        result
    }
}

impl Drop for ReplacementPhase {
    fn drop(&mut self) {
        if !self.finished {
            eprintln!(
                "replacement provider {}: dropped {:.1}s",
                self.name,
                self.started.elapsed().as_secs_f64()
            );
        }
    }
}

pub struct Fixture {
    pub scope: Scope,
    pub root: Node,
    pub read: ICloudDrive,
    pub upload: Arc<ICloudFileCreate>,
    pub folders: ICloudFolderCreate,
    removal: RemovalContext,
    owned: Mutex<HashSet<String>>,
    child_uploads: Mutex<HashMap<String, Arc<ICloudFileCreate>>>,
    replacements: Mutex<HashMap<Uuid, Arc<ICloudOwnedMountedReplace>>>,
}

pub struct RemovalContext {
    pub apple_id: String,
    pub credential_id: String,
    pub state: PathBuf,
    pub journal: Arc<Mutex<UploadJournal>>,
    pub metadata_db: PathBuf,
}

impl Fixture {
    pub fn new(
        scope: Scope,
        root: Node,
        read: ICloudDrive,
        upload: ICloudFileCreate,
        folders: ICloudFolderCreate,
        removal: RemovalContext,
        owned: HashSet<String>,
    ) -> Self {
        Self {
            scope,
            root,
            read,
            upload: Arc::new(upload),
            folders,
            removal,
            owned: Mutex::new(owned),
            child_uploads: Mutex::new(HashMap::new()),
            replacements: Mutex::new(HashMap::new()),
        }
    }

    fn owns(&self, scope: &Scope, id: &str) -> bool {
        scope == &self.scope
            && (id == self.root.id || self.owned.lock().is_ok_and(|set| set.contains(id)))
    }

    fn guard_upload(&self, request: &UploadRequest) -> cirrove_core::upload::Result<()> {
        if request.scope != self.scope {
            return Err(UploadError::Invalid);
        }
        request.require_file_bytes()?;
        let UploadIntent::Create { parent, .. } = &request.intent else {
            return Err(UploadError::Invalid);
        };
        if parent != &self.root.id {
            let journal = self
                .removal
                .journal
                .lock()
                .map_err(|_| UploadError::Uncertain)?;
            confirmed_owned_child_folder(&journal, &self.scope, &self.root.id, parent)
                .map_err(|_| UploadError::Invalid)?;
        }
        Ok(())
    }

    fn uploader(
        &self,
        request: &UploadRequest,
    ) -> cirrove_core::upload::Result<Arc<ICloudFileCreate>> {
        self.guard_upload(request)?;
        let UploadIntent::Create { parent, .. } = &request.intent else {
            return Err(UploadError::Invalid);
        };
        if parent == &self.root.id {
            return Ok(self.upload.clone());
        }
        let mut uploads = self
            .child_uploads
            .lock()
            .map_err(|_| UploadError::Uncertain)?;
        if let Some(upload) = uploads.get(parent) {
            return Ok(upload.clone());
        }
        let child = {
            let journal = self
                .removal
                .journal
                .lock()
                .map_err(|_| UploadError::Uncertain)?;
            confirmed_owned_child_folder(&journal, &self.scope, &self.root.id, parent)
                .map_err(|_| UploadError::Invalid)?
        };
        let upload = Arc::new(ICloudFileCreate::from_sealed_session(
            self.scope.clone(),
            self.removal.apple_id.clone(),
            self.removal.credential_id.clone(),
            &self.removal.state,
            child,
        )?);
        uploads.insert(parent.clone(), upload.clone());
        Ok(upload)
    }

    fn upload_step(
        &self,
        request: &UploadRequest,
        step: UploadStep,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.upload_step_for_operation(request, step, None)
    }

    fn upload_step_for_operation(
        &self,
        request: &UploadRequest,
        step: UploadStep,
        operation: Option<&str>,
    ) -> cirrove_core::upload::Result<UploadStep> {
        match &step {
            UploadStep::Complete(node) => self.upload_receipt(request, node)?,
            UploadStep::HandoffComplete { current, backup } => {
                self.replace_receipt(request, current, backup, operation)?;
            }
            _ => {}
        }
        Ok(step)
    }

    fn upload_receipt(
        &self,
        request: &UploadRequest,
        node: &Node,
    ) -> cirrove_core::upload::Result<()> {
        let UploadIntent::Create { parent, name } = &request.intent else {
            return Err(UploadError::Invalid);
        };
        if node.kind != NodeKind::File
            || node.target.is_some()
            || node.parent_id.as_ref() != Some(parent)
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

    fn replace_receipt(
        &self,
        request: &UploadRequest,
        current: &Node,
        backup: &Node,
        operation: Option<&str>,
    ) -> cirrove_core::upload::Result<()> {
        let UploadIntent::Replace { item, .. } = &request.intent else {
            return Err(UploadError::Invalid);
        };
        let replacement = match operation {
            Some(operation) => self.replacement_for_operation(request, operation)?,
            None => self.replacement(request)?,
        };
        if request.scope != self.scope
            || backup.id != *item
            || current.id == *item
            || current.parent_id.as_deref() != Some(replacement.parent_id())
            || current.kind != NodeKind::File
            || current.size != request.size
            || current.target.is_some()
            || current.package
        {
            return Err(UploadError::Uncertain);
        }
        let mut owned = self.owned.lock().map_err(|_| UploadError::Uncertain)?;
        if owned.contains(&current.id) && !owned.contains(item) {
            return Ok(());
        }
        if !owned.remove(item) {
            return Err(UploadError::Uncertain);
        }
        owned.insert(current.id.clone());
        Ok(())
    }

    fn replacement(
        &self,
        request: &UploadRequest,
    ) -> cirrove_core::upload::Result<Arc<ICloudOwnedMountedReplace>> {
        let journal = self
            .removal
            .journal
            .lock()
            .map_err(|_| UploadError::Uncertain)?;
        let mut operation = None;
        let mut after = 0;
        loop {
            let rows = journal
                .list(after, 256)
                .map_err(|_| UploadError::Uncertain)?;
            if rows.is_empty() {
                break;
            }
            for row in &rows {
                if row.scope != self.scope {
                    return Err(UploadError::Invalid);
                }
                if row.intent == request.intent
                    && row.scope == request.scope
                    && row.size == request.size
                    && row.sha256 == request.sha256
                    && row.state != UploadState::Uploaded
                    && operation.replace(row.id).is_some()
                {
                    return Err(UploadError::Invalid);
                }
            }
            after = rows.last().expect("nonempty page").sequence;
            if after > 100_000 {
                return Err(UploadError::Invalid);
            }
        }
        let operation = operation.ok_or(UploadError::Invalid)?.to_string();
        drop(journal);
        self.replacement_for_operation(request, &operation)
    }

    fn replacement_operation(
        &self,
        request: &UploadRequest,
        operation: &str,
    ) -> cirrove_core::upload::Result<Uuid> {
        request.require_file_bytes()?;
        let UploadIntent::Replace { item, .. } = &request.intent else {
            return Err(UploadError::Invalid);
        };
        if request.scope != self.scope || !self.owns(&request.scope, item) {
            return Err(UploadError::Invalid);
        }
        let operation = Uuid::parse_str(operation).map_err(|_| UploadError::Invalid)?;
        let row = self
            .removal
            .journal
            .lock()
            .map_err(|_| UploadError::Uncertain)?
            .get(operation)
            .map_err(|_| UploadError::Uncertain)?;
        if row.scope != request.scope
            || row.intent != request.intent
            || row.size != request.size
            || row.sha256 != request.sha256
            || row.state == UploadState::Uploaded
        {
            return Err(UploadError::Invalid);
        }
        Ok(operation)
    }

    fn replacement_from_checkpoint(
        &self,
        request: &UploadRequest,
        operation: &str,
        checkpoint: &SecretString,
    ) -> cirrove_core::upload::Result<Arc<ICloudOwnedMountedReplace>> {
        let id = self.replacement_operation(request, operation)?;
        let restored = ICloudOwnedMountedReplace::from_sealed_checkpoint(
            request,
            id,
            checkpoint,
            ICloudSealedSignIn {
                apple_id: self.removal.apple_id.clone(),
                credential_id: self.removal.credential_id.clone(),
            },
            &self.removal.state,
        )?;
        let Some(restored) = restored else {
            return self.replacement_for_operation(request, operation);
        };
        if !self.owns(&request.scope, restored.parent_id()) {
            return Err(UploadError::Invalid);
        }
        let restored = Arc::new(restored);
        self.replacements
            .lock()
            .map_err(|_| UploadError::Uncertain)?
            .insert(id, restored.clone());
        Ok(restored)
    }

    fn replacement_for_operation(
        &self,
        request: &UploadRequest,
        operation: &str,
    ) -> cirrove_core::upload::Result<Arc<ICloudOwnedMountedReplace>> {
        let operation = self.replacement_operation(request, operation)?;
        if let Some(saved) = self
            .replacements
            .lock()
            .map_err(|_| UploadError::Uncertain)?
            .get(&operation)
            .cloned()
        {
            return Ok(saved);
        }
        let UploadIntent::Replace {
            item,
            expected_etag,
        } = &request.intent
        else {
            return Err(UploadError::Invalid);
        };
        let store = Store::open(&self.removal.metadata_db).map_err(|_| UploadError::Uncertain)?;
        let chain = store
            .node_chain_to_root(&self.scope, item, &self.root.id)
            .map_err(|_| UploadError::Uncertain)?
            .ok_or(UploadError::Invalid)?;
        let original = chain.first().ok_or(UploadError::Invalid)?.clone();
        if original.kind != NodeKind::File
            || original.id != *item
            || original.etag.as_deref() != Some(expected_etag)
            || chain
                .iter()
                .skip(1)
                .any(|ancestor| ancestor.kind != NodeKind::Folder)
        {
            return Err(UploadError::Invalid);
        }
        let folder = if chain.len() == 1 {
            Node {
                parent_id: Some(cirrove_icloud::ROOT_ID.into()),
                ..self.root.clone()
            }
        } else {
            chain[1].clone()
        };
        let replacement = Arc::new(ICloudOwnedMountedReplace::from_sealed_session_for_existing(
            self.scope.clone(),
            folder,
            original,
            operation,
            ICloudSealedSignIn {
                apple_id: self.removal.apple_id.clone(),
                credential_id: self.removal.credential_id.clone(),
            },
            &self.removal.state,
        )?);
        self.replacements
            .lock()
            .map_err(|_| UploadError::Uncertain)?
            .insert(operation, replacement.clone());
        Ok(replacement)
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

    fn guard_rename_folder(
        &self,
        request: &MutationRequest,
    ) -> cirrove_core::mutation::Result<Node> {
        request.validate()?;
        let MutationIntent::Relocate {
            before,
            parent,
            name,
        } = &request.intent
        else {
            return Err(MutationError::Invalid);
        };
        if request.scope != self.scope
            || before.kind != NodeKind::Folder
            || before.parent_id.as_deref() != Some(&self.root.id)
            || parent != &self.root.id
            || before.name != "Mounted Folder"
            || name != "Mounted Renamed"
            || before.id == self.root.id
            || !self.owns(&request.scope, &before.id)
        {
            return Err(MutationError::Invalid);
        }
        Ok(before.clone())
    }

    fn folder_renamer(
        &self,
        before: Node,
        reconciliation_only: bool,
    ) -> cirrove_core::mutation::Result<ICloudFolderRename> {
        let adapter = ICloudFolderRename::from_sealed_session(
            self.scope.clone(),
            self.removal.apple_id.clone(),
            self.removal.credential_id.clone(),
            &self.removal.state,
            before,
            "Mounted Renamed".into(),
        )?;
        Ok(if reconciliation_only {
            adapter.reconciliation_only()
        } else {
            adapter
        })
    }

    fn guard_move_folder(
        &self,
        request: &MutationRequest,
    ) -> cirrove_core::mutation::Result<(Node, Node)> {
        request.validate()?;
        let MutationIntent::Relocate {
            before,
            parent,
            name,
        } = &request.intent
        else {
            return Err(MutationError::Invalid);
        };
        if request.scope != self.scope
            || before.kind != NodeKind::Folder
            || before.parent_id.as_deref() != Some(&self.root.id)
            || before.name != "Mounted Folder"
            || parent == &self.root.id
            || name != &before.name
            || !self.owns(&request.scope, &before.id)
            || !self.owns(&request.scope, parent)
        {
            return Err(MutationError::Invalid);
        }
        let journal = self
            .removal
            .journal
            .lock()
            .map_err(|_| MutationError::Uncertain)?;
        let destination =
            confirmed_owned_child_folder(&journal, &self.scope, &self.root.id, parent)?;
        if destination.name != "Mounted Move Destination" {
            return Err(MutationError::Invalid);
        }
        Ok((before.clone(), destination))
    }

    fn folder_mover(
        &self,
        before: Node,
        destination: Node,
        reconciliation_only: bool,
    ) -> cirrove_core::mutation::Result<ICloudFolderMove> {
        let adapter = ICloudFolderMove::from_sealed_session(
            self.scope.clone(),
            self.removal.apple_id.clone(),
            self.removal.credential_id.clone(),
            &self.removal.state,
            before,
            destination,
        )?;
        Ok(if reconciliation_only {
            adapter.reconciliation_only()
        } else {
            adapter
        })
    }

    fn remover(&self, before: Node) -> cirrove_core::mutation::Result<ICloudFolderTrash> {
        ICloudFolderTrash::from_sealed_session(
            self.scope.clone(),
            self.removal.apple_id.clone(),
            self.removal.credential_id.clone(),
            &self.removal.state,
            before,
        )
    }

    fn guard_remove_file(
        &self,
        request: &MutationRequest,
    ) -> cirrove_core::mutation::Result<(Node, String)> {
        request.validate()?;
        let MutationIntent::RemoveFile { before } = &request.intent else {
            return Err(MutationError::Invalid);
        };
        if request.scope != self.scope
            || before.kind != NodeKind::File
            || !self.owns(&request.scope, &before.id)
        {
            return Err(MutationError::Invalid);
        }
        let journal = self
            .removal
            .journal
            .lock()
            .map_err(|_| MutationError::Uncertain)?;
        let parent = before.parent_id.as_deref().ok_or(MutationError::Invalid)?;
        if parent != self.root.id {
            confirmed_owned_child_folder(&journal, &self.scope, &self.root.id, parent)?;
        }
        let digest = confirmed_current_file_digest(&journal, &self.scope, &self.root.id, before)?;
        Ok((before.clone(), digest))
    }

    fn file_remover(
        &self,
        before: Node,
        digest: String,
    ) -> cirrove_core::mutation::Result<ICloudFileTrash> {
        ICloudFileTrash::from_sealed_session(
            self.scope.clone(),
            self.removal.apple_id.clone(),
            self.removal.credential_id.clone(),
            &self.removal.state,
            before,
        )
        .and_then(|provider| provider.with_expected_sha256(digest))
    }

    fn guard_rename_file(
        &self,
        request: &MutationRequest,
    ) -> cirrove_core::mutation::Result<(Node, String)> {
        request.validate()?;
        let MutationIntent::Relocate {
            before,
            parent,
            name,
        } = &request.intent
        else {
            return Err(MutationError::Invalid);
        };
        if request.scope != self.scope
            || before.kind != NodeKind::File
            || before.parent_id.as_deref() != Some(parent)
            || before.name == *name
        {
            return Err(MutationError::Invalid);
        }
        self.guard_remove_file(&MutationRequest {
            scope: request.scope.clone(),
            intent: MutationIntent::RemoveFile {
                before: before.clone(),
            },
        })
    }

    fn file_renamer(
        &self,
        before: Node,
        target_name: String,
        digest: String,
        reconciliation_only: bool,
    ) -> cirrove_core::mutation::Result<ICloudFileRename> {
        let parent = before.parent_id.as_deref().ok_or(MutationError::Invalid)?;
        if parent != self.root.id {
            let journal = self
                .removal
                .journal
                .lock()
                .map_err(|_| MutationError::Uncertain)?;
            confirmed_owned_child_folder(&journal, &self.scope, &self.root.id, parent)?;
        }
        let adapter = ICloudFileRename::from_sealed_session(
            self.scope.clone(),
            self.removal.apple_id.clone(),
            self.removal.credential_id.clone(),
            &self.removal.state,
            before,
            target_name,
            digest,
        )?;
        Ok(if reconciliation_only {
            adapter.reconciliation_only()
        } else {
            adapter
        })
    }

    fn guard_move_file(
        &self,
        request: &MutationRequest,
    ) -> cirrove_core::mutation::Result<(Node, Node, String)> {
        request.validate()?;
        let MutationIntent::Relocate {
            before,
            parent,
            name,
        } = &request.intent
        else {
            return Err(MutationError::Invalid);
        };
        if request.scope != self.scope
            || before.kind != NodeKind::File
            || before.parent_id.as_deref() != Some(&self.root.id)
            || parent == &self.root.id
            || name != &before.name
            || !self.owns(&request.scope, parent)
        {
            return Err(MutationError::Invalid);
        }
        let (before, digest) = self.guard_remove_file(&MutationRequest {
            scope: request.scope.clone(),
            intent: MutationIntent::RemoveFile {
                before: before.clone(),
            },
        })?;
        let journal = self
            .removal
            .journal
            .lock()
            .map_err(|_| MutationError::Uncertain)?;
        let destination =
            confirmed_owned_child_folder(&journal, &self.scope, &self.root.id, parent)?;
        Ok((before, destination, digest))
    }

    fn file_mover(
        &self,
        before: Node,
        destination: Node,
        digest: String,
        reconciliation_only: bool,
    ) -> cirrove_core::mutation::Result<ICloudFileMove> {
        let adapter = ICloudFileMove::from_sealed_session(
            self.scope.clone(),
            self.removal.apple_id.clone(),
            self.removal.credential_id.clone(),
            &self.removal.state,
            before,
            destination,
            digest,
        )?;
        Ok(if reconciliation_only {
            adapter.reconciliation_only()
        } else {
            adapter
        })
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

    fn removed_file_receipt(
        &self,
        request: &MutationRequest,
        receipt: &MutationReceipt,
    ) -> cirrove_core::mutation::Result<()> {
        let (before, _) = self.guard_remove_file(request)?;
        if !request.accepts(receipt) {
            return Err(MutationError::Uncertain);
        }
        self.owned
            .lock()
            .map_err(|_| MutationError::Uncertain)?
            .remove(&before.id);
        Ok(())
    }

    pub async fn inspect_failed_nested_rename(&self) -> anyhow::Result<()> {
        let request = {
            let journal = self
                .removal
                .journal
                .lock()
                .map_err(|_| anyhow::anyhow!("journal lock unavailable"))?;
            let rows = journal.list_mutations(0, 16)?;
            let row = rows.last().ok_or_else(|| anyhow::anyhow!("no mutation"))?;
            ensure!(
                row.state == MutationState::Failed,
                "last mutation is not failed"
            );
            row.request.clone()
        };
        let (before, digest) = self
            .guard_rename_file(&request)
            .map_err(|_| anyhow::anyhow!("nested rename refused by fixture guard"))?;
        println!("nested rename fixture guard passed");
        let MutationIntent::Relocate { name, .. } = &request.intent else {
            anyhow::bail!("last mutation is not relocation");
        };
        let adapter = self
            .file_renamer(before, name.clone(), digest, false)
            .map_err(|_| anyhow::anyhow!("nested rename adapter construction refused"))?;
        println!("nested rename adapter construction passed");
        adapter
            .prepare_mutation(&request, &CancellationToken::new())
            .await
            .map_err(|_| anyhow::anyhow!("nested rename read-only preflight refused"))?;
        println!("nested rename read-only preflight passed");
        Ok(())
    }

    /// Inspect a failed replacement without mounting, enqueueing or sending a
    /// provider write. The adapter's begin step only serializes a checkpoint.
    pub async fn inspect_failed_replace(&self) -> anyhow::Result<()> {
        let request = {
            let journal = self
                .removal
                .journal
                .lock()
                .map_err(|_| anyhow::anyhow!("journal lock unavailable"))?;
            let rows = journal.list(0, 16)?;
            let row = rows.last().ok_or_else(|| anyhow::anyhow!("no upload"))?;
            ensure!(
                row.state == UploadState::Failed
                    && matches!(row.intent, UploadIntent::Replace { .. })
                    && row.session_key.is_none()
                    && row.remote.is_none()
                    && row.transferred_bytes == 0,
                "only a pre-checkpoint failed replacement may be inspected"
            );
            UploadRequest {
                representation: Default::default(),
                scope: row.scope.clone(),
                intent: row.intent.clone(),
                size: row.size,
                sha256: row.sha256.clone(),
            }
        };
        let adapter = self
            .replacement(&request)
            .map_err(|_| anyhow::anyhow!("replacement journal guard or constructor refused"))?;
        println!("replacement adapter construction passed");
        ensure!(
            matches!(
                adapter
                    .begin_upload(&request, &CancellationToken::new())
                    .await,
                Ok(UploadStep::Prepared(_))
            ),
            "mutation-free replacement checkpoint preparation refused"
        );
        println!("replacement mutation-free checkpoint preparation passed");
        Ok(())
    }

    pub async fn retry_failed_nested_rename(&self) -> anyhow::Result<()> {
        self.inspect_failed_nested_rename().await?;
        let mut journal = self
            .removal
            .journal
            .lock()
            .map_err(|_| anyhow::anyhow!("journal lock unavailable"))?;
        let rows = journal.list_mutations(0, 16)?;
        let row = rows.last().ok_or_else(|| anyhow::anyhow!("no mutation"))?;
        ensure!(
            row.state == MutationState::Failed && row.prepared_item.is_none(),
            "only an unsent failed mutation may be retried"
        );
        journal.request_mutation_retry(row.id)?;
        println!("unsent nested rename queued for read-only reconciliation");
        Ok(())
    }
}

fn confirmed_owned_child_folder(
    journal: &UploadJournal,
    scope: &Scope,
    root: &str,
    id: &str,
) -> cirrove_core::mutation::Result<Node> {
    let mut after = 0;
    let mut folder = None;
    loop {
        let rows = journal
            .list_mutations(after, 256)
            .map_err(|_| MutationError::Uncertain)?;
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            if row.state == MutationState::Applied {
                match (&row.request.intent, row.receipt.as_ref()) {
                    (
                        MutationIntent::CreateFolder { parent, name },
                        Some(MutationReceipt::Upsert(node)),
                    ) if node.id == id => {
                        let valid = row.request.scope == *scope
                            && parent == root
                            && matches!(
                                name.as_str(),
                                "Mounted Folder" | "Mounted Move Destination"
                            )
                            && node.name == *name
                            && node.parent_id.as_deref() == Some(root)
                            && node.kind == NodeKind::Folder
                            && node.target.is_none()
                            && !node.package;
                        if !valid {
                            return Err(MutationError::Invalid);
                        }
                        if folder.replace(node.clone()).is_some() {
                            return Err(MutationError::Invalid);
                        }
                    }
                    _ => {}
                }
                if let MutationIntent::RemoveFolder { before } = &row.request.intent
                    && before.id == id
                {
                    return Err(MutationError::Invalid);
                }
            }
        }
        after = rows.last().expect("nonempty page").sequence;
        if after > 100_000 {
            return Err(MutationError::Invalid);
        }
    }
    folder.ok_or(MutationError::Invalid)
}

fn confirmed_current_file_digest(
    journal: &UploadJournal,
    scope: &Scope,
    root: &str,
    before: &Node,
) -> cirrove_core::mutation::Result<String> {
    let mut after = 0;
    let mut digest = None;
    let mut current = None;
    loop {
        let rows = journal
            .list(after, 256)
            .map_err(|_| MutationError::Uncertain)?;
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            if row.remote.as_ref().is_some_and(|node| node.id == before.id) {
                let node = row.remote.as_ref().ok_or(MutationError::Invalid)?;
                let UploadIntent::Create { parent, .. } = &row.intent else {
                    return Err(MutationError::Invalid);
                };
                if parent != root {
                    confirmed_owned_child_folder(journal, scope, root, parent)?;
                }
                if row.state != UploadState::Uploaded
                    || !confirmed_file(scope, parent, &row.scope, &row.intent, node)
                    || node.kind != before.kind
                    || node.size != before.size
                    || row.size != before.size
                    || row.sha256.len() != 64
                    || digest.replace(row.sha256.clone()).is_some()
                    || current.replace(node.clone()).is_some()
                {
                    return Err(MutationError::Invalid);
                }
            }
        }
        after = rows.last().expect("nonempty page").sequence;
        if after > 100_000 {
            return Err(MutationError::Invalid);
        }
    }
    let mut current = current.ok_or(MutationError::Invalid)?;
    after = 0;
    loop {
        let rows = journal
            .list_mutations(after, 256)
            .map_err(|_| MutationError::Uncertain)?;
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            if let MutationIntent::RemoveFile { before: removed } = &row.request.intent
                && removed.id == before.id
                && row.state == MutationState::Applied
            {
                return Err(MutationError::Invalid);
            }
            if let MutationIntent::Relocate { before: prior, .. } = &row.request.intent
                && prior.id == before.id
                && row.state == MutationState::Applied
            {
                let Some(receipt @ MutationReceipt::Upsert(updated)) = row.receipt.as_ref() else {
                    return Err(MutationError::Invalid);
                };
                if row.request.scope != *scope
                    || !same_file_version(prior, &current)
                    || !row.request.accepts(receipt)
                    || updated.id != current.id
                    || updated.kind != NodeKind::File
                    || updated.size != current.size
                    || updated.etag.as_deref().is_none_or(str::is_empty)
                {
                    return Err(MutationError::Invalid);
                }
                let parent = updated.parent_id.as_deref().ok_or(MutationError::Invalid)?;
                if parent != root {
                    confirmed_owned_child_folder(journal, scope, root, parent)?;
                }
                current = updated.clone();
            }
        }
        after = rows.last().expect("nonempty page").sequence;
        if after > 100_000 {
            return Err(MutationError::Invalid);
        }
    }
    if !same_file_version(&current, before) {
        return Err(MutationError::Invalid);
    }
    digest.ok_or(MutationError::Invalid)
}

fn same_file_version(left: &Node, right: &Node) -> bool {
    left.id == right.id
        && left.parent_id == right.parent_id
        && left.name == right.name
        && left.kind == NodeKind::File
        && right.kind == NodeKind::File
        && left.size == right.size
        && left.etag == right.etag
        && left.target.is_none()
        && right.target.is_none()
        && !left.package
        && !right.package
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

fn confirmed_current_file(scope: &Scope, root: &str, row: &UploadRecord, node: &Node) -> bool {
    row.scope == *scope
        && row.state == UploadState::Uploaded
        && node.kind == NodeKind::File
        && node.parent_id.as_deref() == Some(root)
        && node.size == row.size
        && node.etag.as_deref().is_some_and(|etag| !etag.is_empty())
        && node.target.is_none()
        && !node.package
        && row.sha256.len() == 64
        && row
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        && match &row.intent {
            UploadIntent::Create { .. } => {
                confirmed_file(scope, root, &row.scope, &row.intent, node)
            }
            UploadIntent::Replace { item, .. } => item != &node.id && !node.name.is_empty(),
        }
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
    let mut owned_folders = HashSet::new();
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
            if row.state == UploadState::Uploaded {
                let node = row
                    .remote
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("uploaded fixture lacks remote receipt"))?;
                let parent = match &row.intent {
                    UploadIntent::Create { parent, .. } if parent != root => {
                        confirmed_owned_child_folder(journal, scope, root, parent)
                            .map_err(|_| anyhow::anyhow!("unconfirmed nested upload parent"))?;
                        parent.as_str()
                    }
                    UploadIntent::Replace { .. } if node.parent_id.as_deref() != Some(root) => {
                        let parent = node
                            .parent_id
                            .as_deref()
                            .ok_or_else(|| anyhow::anyhow!("replacement has no parent"))?;
                        confirmed_owned_child_folder(journal, scope, root, parent).map_err(
                            |_| anyhow::anyhow!("unconfirmed nested replacement parent"),
                        )?;
                        parent
                    }
                    _ => root,
                };
                ensure!(
                    confirmed_current_file(scope, parent, row, node),
                    "uploaded fixture receipt does not match its intent"
                );
                if let UploadIntent::Replace { item, .. } = &row.intent {
                    ensure!(
                        owned.remove(item),
                        "replacement has no confirmed old identity"
                    );
                }
                ensure!(
                    owned.insert(node.id.clone()),
                    "duplicate remote identity in isolated journal"
                );
            } else {
                if let UploadIntent::Create { parent, .. } = &row.intent
                    && parent != root
                {
                    confirmed_owned_child_folder(journal, scope, root, parent)
                        .map_err(|_| anyhow::anyhow!("unconfirmed pending upload parent"))?;
                }
                ensure!(
                    matches!(&row.intent, UploadIntent::Create { .. })
                        || matches!(&row.intent, UploadIntent::Replace { item, .. } if owned.contains(item)),
                    "non-fixture pending upload in isolated journal"
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
                MutationIntent::TrashNativeDocument { .. } => {
                    anyhow::bail!("native Trash is outside ordinary fixture")
                }
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
                MutationIntent::RemoveFile { before } => ensure!(
                    before
                        .parent_id
                        .as_deref()
                        .is_some_and(|parent| parent == root || owned_folders.contains(parent))
                        && before.kind == NodeKind::File
                        && owned.contains(&before.id),
                    "non-fixture file removal in isolated journal"
                ),
                MutationIntent::Relocate {
                    before,
                    parent,
                    name,
                } => ensure!(
                    ((before.kind == NodeKind::Folder
                        && before.parent_id.as_deref() == Some(root)
                        && parent == root
                        && before.name == "Mounted Folder"
                        && name == "Mounted Renamed")
                        || (before.kind == NodeKind::Folder
                            && before.parent_id.as_deref() == Some(root)
                            && parent != root
                            && owned_folders.contains(parent)
                            && confirmed_owned_child_folder(journal, scope, root, parent)
                                .is_ok_and(|folder| folder.name == "Mounted Move Destination")
                            && before.name == "Mounted Folder"
                            && name == &before.name
                            && row.request.validate().is_ok())
                        || (before.kind == NodeKind::File
                            && ((before.parent_id.as_deref() == Some(root)
                                && parent == root
                                && before.name != *name)
                                || (before.parent_id.as_deref() == Some(root)
                                    && owned_folders.contains(parent)
                                    && before.name == *name)
                                || (before.parent_id.as_deref() == Some(parent)
                                    && owned_folders.contains(parent)
                                    && before.name != *name))
                            && row.request.validate().is_ok()))
                        && owned.contains(&before.id),
                    "non-fixture rename in isolated journal"
                ),
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
                        owned_folders.insert(node.id.clone());
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
                        owned_folders.remove(item);
                    }
                    (MutationIntent::RemoveFile { before }, MutationReceipt::Removed { item }) => {
                        ensure!(
                            item == &before.id && row.request.accepts(receipt),
                            "removed file receipt does not match its owned identity"
                        );
                        owned.remove(item);
                    }
                    (MutationIntent::Relocate { before, .. }, MutationReceipt::Upsert(node)) => {
                        ensure!(
                            row.request.accepts(receipt)
                                && node.id == before.id
                                && owned.contains(&node.id),
                            "renamed folder receipt does not retain its owned identity"
                        )
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
    use sha2::{Digest, Sha256};

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

    #[tokio::test]
    async fn fixture_folder_preparation_persists_bound_unsent_checkpoint_without_sign_in() {
        use cirrove_auth::CredentialVault;
        use secrecy::ExposeSecret;
        #[derive(Default)]
        struct Vault(Mutex<HashMap<String, String>>);
        #[async_trait::async_trait]
        impl CredentialVault for Vault {
            async fn load(&self, key: &str) -> anyhow::Result<Option<SecretString>> {
                Ok(self
                    .0
                    .lock()
                    .unwrap()
                    .get(key)
                    .cloned()
                    .map(SecretString::from))
            }
            async fn save(&self, key: &str, value: SecretString) -> anyhow::Result<()> {
                self.0
                    .lock()
                    .unwrap()
                    .insert(key.into(), value.expose_secret().into());
                Ok(())
            }
            async fn remove(&self, _: &str) -> anyhow::Result<()> {
                panic!("preparation must not remove evidence")
            }
        }
        let private = tempfile::tempdir().unwrap();
        let scope = Scope {
            account: Uuid::new_v4().to_string(),
            ..scope()
        };
        let apple_id = "synthetic@example.invalid".to_owned();
        let credential_id = Uuid::new_v4().to_string();
        let root = Node {
            id: "FOLDER::com.apple.CloudDocs::owned".into(),
            parent_id: Some(cirrove_icloud::ROOT_ID.into()),
            size: 0,
            ..node("unused", NodeKind::Folder, "Owned fixture")
        };
        let vault = Arc::new(Vault::default());
        // All providers are lazy sealed-session constructors. There is no saved
        // sign-in, so preparation must remain entirely local.
        let fixture = Fixture::new(
            scope.clone(),
            root.clone(),
            ICloudDrive::on_demand_from_sealed_session(
                scope.clone(),
                apple_id.clone(),
                credential_id.clone(),
                private.path(),
            )
            .unwrap(),
            ICloudFileCreate::from_sealed_session(
                scope.clone(),
                apple_id.clone(),
                credential_id.clone(),
                private.path(),
                root.clone(),
            )
            .unwrap(),
            ICloudFolderCreate::from_sealed_session(
                scope.clone(),
                apple_id.clone(),
                credential_id.clone(),
                private.path(),
                root.clone(),
                vault.clone(),
            )
            .unwrap(),
            RemovalContext {
                apple_id,
                credential_id,
                state: private.path().into(),
                journal: Arc::new(Mutex::new(
                    UploadJournal::open(&private.path().join("journal"), &scope.account, 4096)
                        .unwrap(),
                )),
                metadata_db: private.path().join("unused-metadata.db"),
            },
            HashSet::new(),
        );
        let operation = Uuid::new_v4().to_string();
        let request = MutationRequest {
            scope: scope.clone(),
            intent: MutationIntent::CreateFolder {
                parent: root.id.clone(),
                name: "Child".into(),
            },
        };
        let mut foreign = request.clone();
        foreign.scope.account = Uuid::new_v4().to_string();
        let cancel = CancellationToken::new();
        assert!(
            fixture
                .prepare_mutation_for_operation(&operation, &foreign, &cancel)
                .await
                .is_err()
        );
        assert!(vault.0.lock().unwrap().is_empty());
        assert!(
            fixture
                .prepare_mutation_for_operation(&operation, &request, &cancel)
                .await
                .unwrap()
                .is_none()
        );
        let key = format!("icloud-folder-create/{}/{operation}", scope.account);
        let checkpoint = vault
            .load(&key)
            .await
            .unwrap()
            .expect("operation-aware preparation must persist evidence");
        let value: serde_json::Value = serde_json::from_str(checkpoint.expose_secret()).unwrap();
        assert_eq!(value["version"], 2);
        assert_eq!(value["state"]["phase"], "not_sent");
        assert_eq!(value["operation"], operation);
        assert_eq!(value["scope"], serde_json::to_value(&scope).unwrap());
        assert_eq!(value["parent"], root.id);
        assert_eq!(value["name"], "Child");
        assert!(matches!(
            fixture
                .reconcile_operation(&operation, &request, None, &cancel)
                .await
                .unwrap(),
            MutationReconciliation::Uncommitted
        ));
    }

    #[test]
    fn restored_owned_accepts_only_confirmed_populated_folder_move() {
        let private = tempfile::tempdir().unwrap();
        let mut journal =
            UploadJournal::open(&private.path().join("journal"), "account-a", 1024 * 1024).unwrap();
        let scope = scope();
        let mut source = node("folder-source", NodeKind::Folder, "Mounted Folder");
        source.size = 0;
        let mut destination = node(
            "folder-destination",
            NodeKind::Folder,
            "Mounted Move Destination",
        );
        destination.size = 0;
        for folder in [&source, &destination] {
            let create = journal
                .enqueue_mutation(MutationRequest {
                    scope: scope.clone(),
                    intent: MutationIntent::CreateFolder {
                        parent: "fixture-root".into(),
                        name: folder.name.clone(),
                    },
                })
                .unwrap();
            let claimed = journal.claim_mutation().unwrap().unwrap();
            journal
                .acknowledge_mutation(
                    create.id,
                    claimed.attempt.unwrap(),
                    MutationReceipt::Upsert(folder.clone()),
                )
                .unwrap();
        }
        let file = Node {
            parent_id: Some(source.id.clone()),
            ..node("child-file", NodeKind::File, "Move Child.txt")
        };
        let upload = journal
            .enqueue(
                scope.clone(),
                UploadIntent::Create {
                    parent: source.id.clone(),
                    name: file.name.clone(),
                },
                b"ab".as_slice(),
            )
            .unwrap();
        let claimed = journal.claim_next().unwrap().unwrap();
        journal
            .acknowledge(upload.id, claimed.attempt.unwrap(), file.clone())
            .unwrap();
        let moved = journal
            .enqueue_mutation(MutationRequest {
                scope: scope.clone(),
                intent: MutationIntent::Relocate {
                    before: source.clone(),
                    parent: destination.id.clone(),
                    name: source.name.clone(),
                },
            })
            .unwrap();
        let claimed = journal.claim_mutation().unwrap().unwrap();
        let receipt = Node {
            parent_id: Some(destination.id.clone()),
            etag: Some("moved-etag".into()),
            ..source.clone()
        };
        journal
            .acknowledge_mutation(
                moved.id,
                claimed.attempt.unwrap(),
                MutationReceipt::Upsert(receipt),
            )
            .unwrap();
        let owned = restored_owned(&journal, &scope, "fixture-root").unwrap();
        assert!(owned.contains(&source.id));
        assert!(owned.contains(&destination.id));
        assert!(owned.contains(&file.id));
    }

    #[test]
    fn a_second_edit_follows_the_confirmed_rename_receipt() {
        let private = tempfile::tempdir().unwrap();
        let mut journal =
            UploadJournal::open(&private.path().join("journal"), "account-a", 1024 * 1024).unwrap();
        let scope = scope();
        let original = node("file-a", NodeKind::File, "Mounted Create.txt");
        let upload = journal
            .enqueue(
                scope.clone(),
                UploadIntent::Create {
                    parent: "fixture-root".into(),
                    name: original.name.clone(),
                },
                b"ab".as_slice(),
            )
            .unwrap();
        let claimed = journal.claim_next().unwrap().unwrap();
        journal
            .acknowledge(upload.id, claimed.attempt.unwrap(), original.clone())
            .unwrap();
        let mut renamed = original.clone();
        renamed.name = "Mounted Renamed.txt".into();
        renamed.etag = Some("renamed-etag".into());
        let queued = journal
            .enqueue_mutation(MutationRequest {
                scope: scope.clone(),
                intent: MutationIntent::Relocate {
                    before: original.clone(),
                    parent: "fixture-root".into(),
                    name: renamed.name.clone(),
                },
            })
            .unwrap();
        let claimed = journal.claim_mutation().unwrap().unwrap();
        journal
            .record_prepared_mutation(queued.id, claimed.attempt.unwrap(), original.id.clone())
            .unwrap();
        journal
            .acknowledge_mutation(
                queued.id,
                claimed.attempt.unwrap(),
                MutationReceipt::Upsert(renamed.clone()),
            )
            .unwrap();
        assert_eq!(
            confirmed_current_file_digest(&journal, &scope, "fixture-root", &renamed).unwrap(),
            hex::encode(Sha256::digest(b"ab"))
        );
        assert!(
            confirmed_current_file_digest(&journal, &scope, "fixture-root", &original).is_err(),
            "the old version must not authorize another edit"
        );
    }

    #[test]
    fn owned_nested_destination_survives_journal_restore() {
        let private = tempfile::tempdir().unwrap();
        let mut journal =
            UploadJournal::open(&private.path().join("journal"), "account-a", 1024 * 1024).unwrap();
        let scope = scope();
        let file = node("file-a", NodeKind::File, "Mounted Create.txt");
        let upload = journal
            .enqueue(
                scope.clone(),
                UploadIntent::Create {
                    parent: "fixture-root".into(),
                    name: file.name.clone(),
                },
                b"ab".as_slice(),
            )
            .unwrap();
        let claimed = journal.claim_next().unwrap().unwrap();
        journal
            .acknowledge(upload.id, claimed.attempt.unwrap(), file.clone())
            .unwrap();
        let folder = node("folder-a", NodeKind::Folder, "Mounted Folder");
        let create = journal
            .enqueue_mutation(MutationRequest {
                scope: scope.clone(),
                intent: MutationIntent::CreateFolder {
                    parent: "fixture-root".into(),
                    name: folder.name.clone(),
                },
            })
            .unwrap();
        let claimed = journal.claim_mutation().unwrap().unwrap();
        journal
            .acknowledge_mutation(
                create.id,
                claimed.attempt.unwrap(),
                MutationReceipt::Upsert(folder.clone()),
            )
            .unwrap();
        let move_record = journal
            .enqueue_mutation(MutationRequest {
                scope: scope.clone(),
                intent: MutationIntent::Relocate {
                    before: file.clone(),
                    parent: folder.id.clone(),
                    name: "Mounted Create.txt".into(),
                },
            })
            .unwrap();
        let owned = restored_owned(&journal, &scope, "fixture-root").unwrap();
        assert!(owned.contains(&folder.id));
        assert!(owned.contains("file-a"));
        let claimed = journal.claim_mutation().unwrap().unwrap();
        journal
            .record_prepared_mutation(move_record.id, claimed.attempt.unwrap(), file.id.clone())
            .unwrap();
        let mut moved = file;
        moved.parent_id = Some(folder.id);
        moved.etag = Some("moved-etag".into());
        journal
            .acknowledge_mutation(
                move_record.id,
                claimed.attempt.unwrap(),
                MutationReceipt::Upsert(moved.clone()),
            )
            .unwrap();
        assert_eq!(
            confirmed_current_file_digest(&journal, &scope, "fixture-root", &moved).unwrap(),
            hex::encode(Sha256::digest(b"ab"))
        );
    }

    #[test]
    fn nested_create_requires_applied_parent_and_restores_exact_receipt() {
        let private = tempfile::tempdir().unwrap();
        let mut journal =
            UploadJournal::open(&private.path().join("journal"), "account-a", 1024 * 1024).unwrap();
        let scope = scope();
        let folder = node("folder-a", NodeKind::Folder, "Mounted Folder");
        let file = Node {
            parent_id: Some(folder.id.clone()),
            ..node("file-nested", NodeKind::File, "Nested Create.txt")
        };
        let upload = journal
            .enqueue(
                scope.clone(),
                UploadIntent::Create {
                    parent: folder.id.clone(),
                    name: file.name.clone(),
                },
                b"ab".as_slice(),
            )
            .unwrap();
        assert!(restored_owned(&journal, &scope, "fixture-root").is_err());
        let create = journal
            .enqueue_mutation(MutationRequest {
                scope: scope.clone(),
                intent: MutationIntent::CreateFolder {
                    parent: "fixture-root".into(),
                    name: folder.name.clone(),
                },
            })
            .unwrap();
        let claimed = journal.claim_mutation().unwrap().unwrap();
        journal
            .acknowledge_mutation(
                create.id,
                claimed.attempt.unwrap(),
                MutationReceipt::Upsert(folder.clone()),
            )
            .unwrap();
        assert!(restored_owned(&journal, &scope, "fixture-root").is_ok());
        let claimed = journal.claim_next().unwrap().unwrap();
        journal
            .acknowledge(upload.id, claimed.attempt.unwrap(), file.clone())
            .unwrap();
        let owned = restored_owned(&journal, &scope, "fixture-root").unwrap();
        assert!(owned.contains(&folder.id));
        assert!(owned.contains(&file.id));
        assert_eq!(
            confirmed_current_file_digest(&journal, &scope, "fixture-root", &file).unwrap(),
            hex::encode(Sha256::digest(b"ab"))
        );
        let remove = journal
            .enqueue_mutation(MutationRequest {
                scope: scope.clone(),
                intent: MutationIntent::RemoveFile {
                    before: file.clone(),
                },
            })
            .unwrap();
        assert!(
            restored_owned(&journal, &scope, "fixture-root")
                .unwrap()
                .contains(&file.id)
        );
        let claimed = journal.claim_mutation().unwrap().unwrap();
        journal
            .record_prepared_mutation(remove.id, claimed.attempt.unwrap(), file.id.clone())
            .unwrap();
        journal
            .acknowledge_mutation(
                remove.id,
                claimed.attempt.unwrap(),
                MutationReceipt::Removed {
                    item: file.id.clone(),
                },
            )
            .unwrap();
        let owned = restored_owned(&journal, &scope, "fixture-root").unwrap();
        assert!(owned.contains(&folder.id));
        assert!(!owned.contains(&file.id));
        assert!(confirmed_current_file_digest(&journal, &scope, "fixture-root", &file).is_err());
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
    async fn validate_write_target(
        &self,
        scope: &Scope,
        node: &Node,
        cancel: &CancellationToken,
    ) -> Result<(), ProviderError> {
        if !self.owns(scope, &node.id)
            || (node.id != self.root.id
                && node
                    .parent_id
                    .as_deref()
                    .is_none_or(|parent| !self.owns(scope, parent)))
        {
            return Err(ProviderError::Permission);
        }
        self.read.validate_write_target(scope, node, cancel).await
    }
    fn unknown_directories_require_fetch(&self) -> bool {
        true
    }

    fn supports_same_parent_folder_rename(&self) -> bool {
        true
    }

    fn supports_cross_parent_folder_move(&self) -> bool {
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
        if let Some(node) = page
            .nodes
            .iter()
            .find(|node| node.id == id && node.parent_id.as_ref() == Some(&self.root.id))
        {
            return Ok(node.clone());
        }
        // A confirmed move may put an owned file one level below the test
        // root. Resolve its exact parent ID; never search by a path/name.
        for folder in page.nodes.iter().filter(|node| {
            node.kind == NodeKind::Folder
                && node.parent_id.as_ref() == Some(&self.root.id)
                && self.owns(scope, &node.id)
        }) {
            let nested = self.read.children(scope, &folder.id, None, cancel).await?;
            if let Some(node) = nested
                .nodes
                .into_iter()
                .find(|node| node.id == id && node.parent_id.as_ref() == Some(&folder.id))
            {
                return Ok(node);
            }
        }
        Err(ProviderError::Unavailable)
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
        if !self.owns(scope, &node.id) || node.id == self.root.id {
            return Err(ProviderError::Permission);
        }
        if node.parent_id.as_ref() != Some(&self.root.id) {
            let Some(parent) = node.parent_id.as_deref() else {
                return Err(ProviderError::Permission);
            };
            if !self.owns(scope, parent) {
                return Err(ProviderError::Permission);
            }
            let folder = self.node(scope, parent, cancel).await?;
            if folder.kind != NodeKind::Folder || folder.parent_id.as_deref() != Some(&self.root.id)
            {
                return Err(ProviderError::Permission);
            }
        }
        self.read
            .read_range(scope, node, offset, length, cancel)
            .await
    }
}

#[async_trait::async_trait]
impl UploadProvider for Fixture {
    fn inspection_timeout(&self, request: &UploadRequest) -> Duration {
        // Resuming a handoff repeats the same full integrity checks as commit.
        self.commit_timeout(request)
    }

    fn commit_timeout(&self, request: &UploadRequest) -> Duration {
        // Full before/after Trash backup verification exceeded the default
        // deadline in registered live runs. Keep every integrity check.
        Duration::from_secs(if matches!(request.intent, UploadIntent::Replace { .. }) {
            300
        } else {
            125
        })
    }

    fn staged_recovery_location(
        &self,
        operation: &str,
        request: &UploadRequest,
    ) -> Option<RecoveryLocation> {
        let UploadIntent::Replace { .. } = &request.intent else {
            return None;
        };
        let operation = self.replacement_operation(request, operation).ok()?;
        Some(ICloudOwnedMountedReplace::recovery_location(operation))
    }

    async fn begin_upload_for_operation(
        &self,
        operation: &str,
        r: &UploadRequest,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        match &r.intent {
            UploadIntent::Create { .. } => self.begin_upload(r, c).await,
            UploadIntent::Replace { .. } => {
                let phase = ReplacementPhase::start("begin");
                let result = async {
                    let step = self
                        .replacement_for_operation(r, operation)?
                        .begin_upload(r, c)
                        .await?;
                    self.upload_step_for_operation(r, step, Some(operation))
                }
                .await;
                phase.finish(result)
            }
        }
    }

    async fn inspect_upload_for_operation(
        &self,
        operation: &str,
        r: &UploadRequest,
        s: &SecretString,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        match &r.intent {
            UploadIntent::Create { .. } => self.inspect_upload(r, s, c).await,
            UploadIntent::Replace { .. } => {
                let phase = ReplacementPhase::start("inspect");
                let result = async {
                    let step = self
                        .replacement_from_checkpoint(r, operation, s)?
                        .inspect_upload(r, s, c)
                        .await?;
                    self.upload_step_for_operation(r, step, Some(operation))
                }
                .await;
                phase.finish(result)
            }
        }
    }

    async fn upload_part_for_operation(
        &self,
        operation: &str,
        r: &UploadRequest,
        s: &SecretString,
        offset: u64,
        bytes: Vec<u8>,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        match &r.intent {
            UploadIntent::Create { .. } => self.upload_part(r, s, offset, bytes, c).await,
            UploadIntent::Replace { .. } => {
                let phase = ReplacementPhase::start("part");
                let result = async {
                    let step = self
                        .replacement_from_checkpoint(r, operation, s)?
                        .upload_part(r, s, offset, bytes, c)
                        .await?;
                    self.upload_step_for_operation(r, step, Some(operation))
                }
                .await;
                phase.finish(result)
            }
        }
    }

    async fn upload_stream_for_operation(
        &self,
        operation: &str,
        r: &UploadRequest,
        s: &SecretString,
        file: File,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        match &r.intent {
            UploadIntent::Create { .. } => self.upload_stream(r, s, file, c).await,
            UploadIntent::Replace { .. } => {
                let phase = ReplacementPhase::start("stream");
                let result = async {
                    let step = self
                        .replacement_from_checkpoint(r, operation, s)?
                        .upload_stream(r, s, file, c)
                        .await?;
                    self.upload_step_for_operation(r, step, Some(operation))
                }
                .await;
                phase.finish(result)
            }
        }
    }

    async fn commit_upload_for_operation(
        &self,
        operation: &str,
        r: &UploadRequest,
        s: &SecretString,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        match &r.intent {
            UploadIntent::Create { .. } => self.commit_upload(r, s, c).await,
            UploadIntent::Replace { .. } => {
                let phase = ReplacementPhase::start("commit");
                let result = async {
                    let step = self
                        .replacement_from_checkpoint(r, operation, s)?
                        .commit_upload(r, s, c)
                        .await?;
                    self.upload_step_for_operation(r, step, Some(operation))
                }
                .await;
                phase.finish(result)
            }
        }
    }

    async fn reconcile_upload_for_operation(
        &self,
        operation: &str,
        r: &UploadRequest,
        s: Option<&SecretString>,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<Reconciliation> {
        let UploadIntent::Replace { .. } = &r.intent else {
            return self.reconcile_upload(r, s, c).await;
        };
        let phase = ReplacementPhase::start("reconcile");
        let result = async {
            let replacement = match s {
                Some(checkpoint) => self.replacement_from_checkpoint(r, operation, checkpoint)?,
                None => self.replacement_for_operation(r, operation)?,
            };
            let result = replacement.reconcile_upload(r, s, c).await?;
            match &result {
                Reconciliation::Committed(node) => self.upload_receipt(r, node)?,
                Reconciliation::HandoffCommitted { current, backup } => {
                    self.replace_receipt(r, current, backup, Some(operation))?;
                }
                _ => {}
            }
            Ok(result)
        }
        .await;
        phase.finish(result)
    }

    async fn begin_upload(
        &self,
        r: &UploadRequest,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        match &r.intent {
            UploadIntent::Create { .. } => {
                self.upload_step(r, self.uploader(r)?.begin_upload(r, c).await?)
            }
            UploadIntent::Replace { .. } => {
                self.upload_step(r, self.replacement(r)?.begin_upload(r, c).await?)
            }
        }
    }
    async fn inspect_upload(
        &self,
        r: &UploadRequest,
        s: &SecretString,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        match &r.intent {
            UploadIntent::Create { .. } => {
                self.upload_step(r, self.uploader(r)?.inspect_upload(r, s, c).await?)
            }
            UploadIntent::Replace { .. } => {
                self.upload_step(r, self.replacement(r)?.inspect_upload(r, s, c).await?)
            }
        }
    }
    async fn upload_part(
        &self,
        r: &UploadRequest,
        s: &SecretString,
        o: u64,
        b: Vec<u8>,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        match &r.intent {
            UploadIntent::Create { .. } => {
                self.upload_step(r, self.uploader(r)?.upload_part(r, s, o, b, c).await?)
            }
            UploadIntent::Replace { .. } => {
                self.upload_step(r, self.replacement(r)?.upload_part(r, s, o, b, c).await?)
            }
        }
    }
    async fn upload_stream(
        &self,
        r: &UploadRequest,
        s: &SecretString,
        f: File,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        match &r.intent {
            UploadIntent::Create { .. } => {
                self.upload_step(r, self.uploader(r)?.upload_stream(r, s, f, c).await?)
            }
            UploadIntent::Replace { .. } => {
                self.upload_step(r, self.replacement(r)?.upload_stream(r, s, f, c).await?)
            }
        }
    }
    async fn commit_upload(
        &self,
        r: &UploadRequest,
        s: &SecretString,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        match &r.intent {
            UploadIntent::Create { .. } => {
                self.upload_step(r, self.uploader(r)?.commit_upload(r, s, c).await?)
            }
            UploadIntent::Replace { .. } => {
                self.upload_step(r, self.replacement(r)?.commit_upload(r, s, c).await?)
            }
        }
    }
    async fn reconcile_upload(
        &self,
        r: &UploadRequest,
        s: Option<&SecretString>,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<Reconciliation> {
        let result = match &r.intent {
            UploadIntent::Create { .. } => self.uploader(r)?.reconcile_upload(r, s, c).await?,
            UploadIntent::Replace { .. } => self.replacement(r)?.reconcile_upload(r, s, c).await?,
        };
        match &result {
            Reconciliation::Committed(node) => self.upload_receipt(r, node)?,
            Reconciliation::HandoffCommitted { current, backup } => {
                self.replace_receipt(r, current, backup, None)?;
            }
            _ => {}
        }
        Ok(result)
    }
}

#[async_trait::async_trait]
impl MutationProvider for Fixture {
    async fn prepare_mutation_for_operation(
        &self,
        operation: &str,
        r: &MutationRequest,
        c: &CancellationToken,
    ) -> cirrove_core::mutation::Result<Option<String>> {
        if matches!(&r.intent, MutationIntent::CreateFolder { .. }) {
            self.guard_folder(r)?;
            self.folders
                .prepare_mutation_for_operation(operation, r, c)
                .await
        } else {
            self.prepare_mutation(r, c).await
        }
    }

    async fn prepare_mutation(
        &self,
        r: &MutationRequest,
        c: &CancellationToken,
    ) -> cirrove_core::mutation::Result<Option<String>> {
        match &r.intent {
            MutationIntent::TrashNativeDocument { .. } => {
                Err(MutationError::Unsupported("native document Trash"))
            }
            MutationIntent::CreateFolder { .. } => {
                self.guard_folder(r)?;
                Ok(None)
            }
            MutationIntent::RemoveFolder { .. } => {
                let before = self.guard_remove_folder(r)?;
                self.remover(before)?.prepare_mutation(r, c).await
            }
            MutationIntent::RemoveFile { .. } => {
                let (before, digest) = self.guard_remove_file(r)?;
                self.file_remover(before, digest)?
                    .prepare_mutation(r, c)
                    .await
            }
            MutationIntent::Relocate {
                before,
                parent,
                name,
            } => match before.kind {
                NodeKind::Folder => {
                    if before.parent_id.as_deref() == Some(parent) {
                        let before = self.guard_rename_folder(r)?;
                        self.folder_renamer(before, false)?
                            .prepare_mutation(r, c)
                            .await
                    } else {
                        let (before, destination) = self.guard_move_folder(r)?;
                        self.folder_mover(before, destination, false)?
                            .prepare_mutation(r, c)
                            .await
                    }
                }
                NodeKind::File => {
                    if before.parent_id.as_deref() == Some(parent) {
                        let (before, digest) = self.guard_rename_file(r)?;
                        self.file_renamer(before, name.clone(), digest, false)?
                            .prepare_mutation(r, c)
                            .await
                    } else {
                        let (before, destination, digest) = self.guard_move_file(r)?;
                        self.file_mover(before, destination, digest, false)?
                            .prepare_mutation(r, c)
                            .await
                    }
                }
                NodeKind::Shortcut => Err(MutationError::Invalid),
            },
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
            MutationIntent::TrashNativeDocument { .. } => {
                Err(MutationError::Unsupported("native document Trash"))
            }
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
            MutationIntent::RemoveFile { .. } => {
                let (before, digest) = self.guard_remove_file(r)?;
                let receipt = self
                    .file_remover(before, digest)?
                    .mutate_prepared(r, prepared, c)
                    .await?;
                self.removed_file_receipt(r, &receipt)?;
                Ok(receipt)
            }
            MutationIntent::Relocate {
                before,
                parent,
                name,
            } => {
                let receipt = match before.kind {
                    NodeKind::Folder => {
                        if before.parent_id.as_deref() == Some(parent) {
                            let before = self.guard_rename_folder(r)?;
                            self.folder_renamer(before, false)?
                                .mutate_prepared(r, prepared, c)
                                .await?
                        } else {
                            let (before, destination) = self.guard_move_folder(r)?;
                            self.folder_mover(before, destination, false)?
                                .mutate_prepared(r, prepared, c)
                                .await?
                        }
                    }
                    NodeKind::File => {
                        if before.parent_id.as_deref() == Some(parent) {
                            let (before, digest) = self.guard_rename_file(r)?;
                            self.file_renamer(before, name.clone(), digest, false)?
                                .mutate_prepared(r, prepared, c)
                                .await?
                        } else {
                            let (before, destination, digest) = self.guard_move_file(r)?;
                            self.file_mover(before, destination, digest, false)?
                                .mutate_prepared(r, prepared, c)
                                .await?
                        }
                    }
                    NodeKind::Shortcut => return Err(MutationError::Invalid),
                };
                self.folder_receipt(r, &receipt)?;
                Ok(receipt)
            }
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
            MutationIntent::TrashNativeDocument { .. } => {
                Err(MutationError::Unsupported("native document Trash"))
            }
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
            MutationIntent::RemoveFile { .. } => {
                let (before, digest) = self.guard_remove_file(r)?;
                let result = self
                    .file_remover(before, digest)?
                    .reconcile_prepared_mutation(r, prepared, c)
                    .await?;
                if let MutationReconciliation::Applied(receipt) = &result {
                    self.removed_file_receipt(r, receipt)?;
                }
                Ok(result)
            }
            MutationIntent::Relocate {
                before,
                parent,
                name,
            } => {
                let result = match before.kind {
                    NodeKind::Folder => {
                        if before.parent_id.as_deref() == Some(parent) {
                            let before = self.guard_rename_folder(r)?;
                            self.folder_renamer(before, true)?
                                .reconcile_prepared_mutation(r, prepared, c)
                                .await?
                        } else {
                            let (before, destination) = self.guard_move_folder(r)?;
                            self.folder_mover(before, destination, true)?
                                .reconcile_prepared_mutation(r, prepared, c)
                                .await?
                        }
                    }
                    NodeKind::File => {
                        if before.parent_id.as_deref() == Some(parent) {
                            let (before, digest) = self.guard_rename_file(r)?;
                            let adapter = self.file_renamer(before, name.clone(), digest, true)?;
                            if prepared.is_some() {
                                adapter.reconcile_prepared_mutation(r, prepared, c).await?
                            } else {
                                adapter.reconcile_mutation(r, c).await?
                            }
                        } else {
                            let (before, destination, digest) = self.guard_move_file(r)?;
                            self.file_mover(before, destination, digest, true)?
                                .reconcile_prepared_mutation(r, prepared, c)
                                .await?
                        }
                    }
                    NodeKind::Shortcut => return Err(MutationError::Invalid),
                };
                if let MutationReconciliation::Applied(receipt) = &result {
                    self.folder_receipt(r, receipt)?;
                }
                Ok(result)
            }
        }
    }
}
