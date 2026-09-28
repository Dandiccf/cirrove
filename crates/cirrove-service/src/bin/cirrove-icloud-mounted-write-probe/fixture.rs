//! The only writable iCloud namespace here is a freshly created Cirrove test folder.
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
    ICloudDrive, ICloudOwnedFixtureFileRename, ICloudOwnedFixtureFolderCreate,
    ICloudOwnedFixtureFolderRemove, ICloudOwnedFixtureFolderRename, ICloudOwnedFixtureRemove,
    ICloudOwnedFixtureUpload, ICloudOwnedMountedReplace, ICloudReadSession, ValidationFolder,
};
use cirrove_service::journal::{MutationState, UploadJournal, UploadRecord, UploadState};
use secrecy::SecretString;
use std::{
    collections::HashSet,
    fs::File,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

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
    pub journal: Arc<Mutex<UploadJournal>>,
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
        match &step {
            UploadStep::Complete(node) => self.upload_receipt(request, node)?,
            UploadStep::HandoffComplete { current, backup } => {
                self.replace_receipt(request, current, backup)?;
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

    fn replace_receipt(
        &self,
        request: &UploadRequest,
        current: &Node,
        backup: &Node,
    ) -> cirrove_core::upload::Result<()> {
        let UploadIntent::Replace { item, .. } = &request.intent else {
            return Err(UploadError::Invalid);
        };
        if request.scope != self.scope
            || backup.id != *item
            || current.id == *item
            || current.parent_id.as_deref() != Some(&self.root.id)
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
    ) -> cirrove_core::upload::Result<ICloudOwnedMountedReplace> {
        request.validate()?;
        let UploadIntent::Replace {
            item,
            expected_etag,
        } = &request.intent
        else {
            return Err(UploadError::Invalid);
        };
        if request.scope != self.scope {
            return Err(UploadError::Invalid);
        }
        let journal = self
            .removal
            .journal
            .lock()
            .map_err(|_| UploadError::Uncertain)?;
        let mut source = None;
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
                if row.state == UploadState::Uploaded
                    && row.remote.as_ref().is_some_and(|node| node.id == *item)
                {
                    let node = row.remote.as_ref().ok_or(UploadError::Invalid)?;
                    if !confirmed_current_file(&self.scope, &self.root.id, row, node)
                        || node.etag.as_deref() != Some(expected_etag)
                        || source.replace((node.clone(), row.sha256.clone())).is_some()
                    {
                        return Err(UploadError::Invalid);
                    }
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
        let (original, original_sha) = source.ok_or(UploadError::Invalid)?;
        let operation: Uuid = operation.ok_or(UploadError::Invalid)?;
        drop(journal);
        ICloudOwnedMountedReplace::new(
            self.scope.clone(),
            self.removal.folder.clone(),
            original,
            original_sha,
            operation,
            self.removal.apple_id.clone(),
            self.removal.snapshot.clone(),
        )
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
    ) -> cirrove_core::mutation::Result<ICloudOwnedFixtureFolderRename> {
        let session = ICloudReadSession::from_session_snapshot(
            &self.removal.snapshot,
            &self.removal.apple_id,
        )
        .map_err(|_| MutationError::Uncertain)?;
        if reconciliation_only {
            ICloudOwnedFixtureFolderRename::for_reconciliation(
                self.scope.clone(),
                session,
                self.removal.folder.clone(),
                before,
                "Mounted Renamed".into(),
            )
        } else {
            ICloudOwnedFixtureFolderRename::new(
                self.scope.clone(),
                session,
                self.removal.folder.clone(),
                before,
                "Mounted Renamed".into(),
            )
        }
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
            || before.parent_id.as_deref() != Some(&self.root.id)
            || !self.owns(&request.scope, &before.id)
        {
            return Err(MutationError::Invalid);
        }
        let journal = self
            .removal
            .journal
            .lock()
            .map_err(|_| MutationError::Uncertain)?;
        let digest = confirmed_current_file_digest(&journal, &self.scope, &self.root.id, before)?;
        Ok((before.clone(), digest))
    }

    fn file_remover(
        &self,
        before: Node,
        digest: String,
    ) -> cirrove_core::mutation::Result<ICloudOwnedFixtureRemove> {
        let session = ICloudReadSession::from_session_snapshot(
            &self.removal.snapshot,
            &self.removal.apple_id,
        )
        .map_err(|_| MutationError::Uncertain)?;
        ICloudOwnedFixtureRemove::from_confirmed_upload(
            self.scope.clone(),
            session,
            self.removal.folder.clone(),
            before,
            digest,
        )
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
            || before.parent_id.as_deref() != Some(&self.root.id)
            || parent != &self.root.id
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
    ) -> cirrove_core::mutation::Result<ICloudOwnedFixtureFileRename> {
        let session = ICloudReadSession::from_session_snapshot(
            &self.removal.snapshot,
            &self.removal.apple_id,
        )
        .map_err(|_| MutationError::Uncertain)?;
        if reconciliation_only {
            ICloudOwnedFixtureFileRename::for_reconciliation(
                self.scope.clone(),
                session,
                self.removal.folder.clone(),
                before,
                target_name,
                digest,
            )
        } else {
            ICloudOwnedFixtureFileRename::new(
                self.scope.clone(),
                session,
                self.removal.folder.clone(),
                before,
                target_name,
                digest,
            )
        }
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
                if row.state != UploadState::Uploaded
                    || !confirmed_file(scope, root, &row.scope, &row.intent, node)
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
                    || updated.parent_id.as_deref() != Some(root)
                    || updated.kind != NodeKind::File
                    || updated.size != current.size
                    || updated.etag.as_deref().is_none_or(str::is_empty)
                {
                    return Err(MutationError::Invalid);
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
                ensure!(
                    confirmed_current_file(scope, root, row, node),
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
                ensure!(
                    matches!(&row.intent, UploadIntent::Create { parent, .. } if parent == root)
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
                    before.parent_id.as_deref() == Some(root)
                        && before.kind == NodeKind::File
                        && owned.contains(&before.id),
                    "non-fixture file removal in isolated journal"
                ),
                MutationIntent::Relocate {
                    before,
                    parent,
                    name,
                } => ensure!(
                    before.parent_id.as_deref() == Some(root)
                        && parent == root
                        && ((before.kind == NodeKind::Folder
                            && before.name == "Mounted Folder"
                            && name == "Mounted Renamed")
                            || (before.kind == NodeKind::File
                                && before.name != *name
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
                    }
                    (
                        MutationIntent::RemoveFolder { before },
                        MutationReceipt::Removed { item },
                    )
                    | (MutationIntent::RemoveFile { before }, MutationReceipt::Removed { item }) => {
                        ensure!(
                            item == &before.id && row.request.accepts(receipt),
                            "removed folder receipt does not match its owned identity"
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

    fn supports_same_parent_folder_rename(&self) -> bool {
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
    fn staged_recovery_location(
        &self,
        operation: &str,
        request: &UploadRequest,
    ) -> Option<RecoveryLocation> {
        let UploadIntent::Replace { .. } = &request.intent else {
            return None;
        };
        self.replacement(request)
            .ok()?
            .staged_recovery_location(operation, request)
    }

    async fn begin_upload(
        &self,
        r: &UploadRequest,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        match &r.intent {
            UploadIntent::Create { .. } => {
                self.guard_upload(r)?;
                self.upload_step(r, self.upload.begin_upload(r, c).await?)
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
                self.guard_upload(r)?;
                self.upload_step(r, self.upload.inspect_upload(r, s, c).await?)
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
                self.guard_upload(r)?;
                self.upload_step(r, self.upload.upload_part(r, s, o, b, c).await?)
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
                self.guard_upload(r)?;
                self.upload_step(r, self.upload.upload_stream(r, s, f, c).await?)
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
                self.guard_upload(r)?;
                self.upload_step(r, self.upload.commit_upload(r, s, c).await?)
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
            UploadIntent::Create { .. } => {
                self.guard_upload(r)?;
                self.upload.reconcile_upload(r, s, c).await?
            }
            UploadIntent::Replace { .. } => self.replacement(r)?.reconcile_upload(r, s, c).await?,
        };
        match &result {
            Reconciliation::Committed(node) => self.upload_receipt(r, node)?,
            Reconciliation::HandoffCommitted { current, backup } => {
                self.replace_receipt(r, current, backup)?;
            }
            _ => {}
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
            MutationIntent::RemoveFile { .. } => {
                let (before, digest) = self.guard_remove_file(r)?;
                self.file_remover(before, digest)?
                    .prepare_mutation(r, c)
                    .await
            }
            MutationIntent::Relocate { before, name, .. } => match before.kind {
                NodeKind::Folder => {
                    let before = self.guard_rename_folder(r)?;
                    self.folder_renamer(before, false)?
                        .prepare_mutation(r, c)
                        .await
                }
                NodeKind::File => {
                    let (before, digest) = self.guard_rename_file(r)?;
                    self.file_renamer(before, name.clone(), digest, false)?
                        .prepare_mutation(r, c)
                        .await
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
            MutationIntent::Relocate { before, name, .. } => {
                let receipt = match before.kind {
                    NodeKind::Folder => {
                        let before = self.guard_rename_folder(r)?;
                        self.folder_renamer(before, false)?
                            .mutate_prepared(r, prepared, c)
                            .await?
                    }
                    NodeKind::File => {
                        let (before, digest) = self.guard_rename_file(r)?;
                        self.file_renamer(before, name.clone(), digest, false)?
                            .mutate_prepared(r, prepared, c)
                            .await?
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
            MutationIntent::Relocate { before, name, .. } => {
                let result = match before.kind {
                    NodeKind::Folder => {
                        let before = self.guard_rename_folder(r)?;
                        self.folder_renamer(before, true)?
                            .reconcile_prepared_mutation(r, prepared, c)
                            .await?
                    }
                    NodeKind::File => {
                        let (before, digest) = self.guard_rename_file(r)?;
                        self.file_renamer(before, name.clone(), digest, true)?
                            .reconcile_prepared_mutation(r, prepared, c)
                            .await?
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
