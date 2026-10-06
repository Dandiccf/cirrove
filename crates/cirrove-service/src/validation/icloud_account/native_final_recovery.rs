//! Feature-only loss of a genuine native final receipt. Never selected by the daemon.
//! Recovery delegates only exact terminal inspection; all mutation entrypoints veto.
use crate::{
    icloud_writes::ICloudWriteProvider,
    journal::{UploadJournal, UploadRecord, UploadState},
};
use anyhow::{Result, ensure};
use cirrove_core::{
    CancellationToken, Node, NodeKind, Scope,
    mutation::{
        MutationError, MutationProvider, MutationReceipt, MutationReconciliation, MutationRequest,
    },
    upload::{
        PackageHandoffReceipt, PackageSemanticIdentity, Reconciliation, RecoveryLocation,
        UploadError, UploadProvider, UploadRequest, UploadStep,
    },
};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use uuid::Uuid;

const TRASH: &str = "FOLDER::com.apple.CloudDocs::TRASH_ROOT";
fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn record(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    File::open(
        path.parent()
            .ok_or_else(|| anyhow::anyhow!("owned record parent absent"))?,
    )?
    .sync_all()?;
    Ok(())
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Receipt {
    pub original: Node,
    pub current: Node,
    pub backup: Node,
    pub current_semantic: PackageSemanticIdentity,
    pub backup_semantic: PackageSemanticIdentity,
}
impl Receipt {
    fn actual(value: &PackageHandoffReceipt) -> Self {
        Self {
            original: value.original.clone(),
            current: value.current.remote.clone(),
            backup: value.backup.remote.clone(),
            current_semantic: value.current.semantic.clone(),
            backup_semantic: value.backup.semantic.clone(),
        }
    }
    fn check(&self, request: &UploadRequest) -> cirrove_core::upload::Result<()> {
        let (cirrove_core::upload::UploadRepresentation::PackageReplacementArchive {
            original,
            original_semantic,
            semantic,
            ..
        }
        | cirrove_core::upload::UploadRepresentation::FlatNumbersReplacementArchive {
            original,
            original_semantic,
            semantic,
        }) = &request.representation
        else {
            return Err(UploadError::Invalid);
        };
        let package = |node: &Node| {
            node.kind == NodeKind::Folder
                && node.package
                && node.target.is_none()
                && node.content_version.is_none()
                && node.id.starts_with("FILE::com.apple.CloudDocs::")
                && node
                    .etag
                    .as_ref()
                    .is_some_and(|v| !v.is_empty() && !v.contains(['*', '\0', '\r', '\n']))
        };
        if self.original != **original
            || self.current_semantic != *semantic
            || self.backup_semantic != *original_semantic
            || !package(&self.current)
            || !package(&self.backup)
            || self.current.id == original.id
            || self.current.parent_id != original.parent_id
            || self.current.name != original.name
            || self.backup.id != original.id
            || self.backup.parent_id.as_deref() != Some(TRASH)
            || self.backup.name != original.name
            || self.backup.size != original.size
        {
            return Err(UploadError::CheckpointInvalid);
        }
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LossMarker {
    pub version: u32,
    pub run: Uuid,
    pub operation: Uuid,
    pub pid: u32,
    pub request: UploadRequest,
    pub checkpoint_sha256: String,
    pub receipt: Receipt,
    pub acknowledgement_returned: bool,
}
#[derive(Clone)]
pub(crate) enum Mode {
    Lose,
    Recover {
        operation: Uuid,
        checkpoint_sha256: String,
        receipt: Box<Receipt>,
    },
}
#[derive(Default)]
pub(crate) struct Counts {
    pub inspections: AtomicU64,
    pub reconciliations: AtomicU64,
    pub refused_uploads: AtomicU64,
    pub refused_namespace: AtomicU64,
}
impl Counts {
    pub(crate) fn value(&self) -> serde_json::Value {
        serde_json::json!({"inspections":self.inspections.load(Ordering::SeqCst),"reconciliations":self.reconciliations.load(Ordering::SeqCst),"refused_uploads":self.refused_uploads.load(Ordering::SeqCst),"refused_namespace":self.refused_namespace.load(Ordering::SeqCst)})
    }
}
/// Only a separately registered developer competitor arm may configure this.
/// The deadline is the original arm's active deadline, never a renewed clock.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PreTrashRelease {
    version: u32,
    run: Uuid,
    operation: Uuid,
    attempt: Uuid,
    checkpoint_sha256: String,
}

pub(crate) struct Guard {
    inner: ICloudWriteProvider,
    request: UploadRequest,
    journal: Arc<Mutex<UploadJournal>>,
    completed: Vec<UploadRecord>,
    mutations: Vec<crate::journal::MutationRecord>,
    operation: Mutex<Option<Uuid>>,
    directory: PathBuf,
    run: Uuid,
    mode: Mode,
    pub counts: Counts,
    pre_trash_deadline: Option<tokio::time::Instant>,
    pre_trash_limit: Duration,
    pre_trash_used: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    pub hold_instead_of_exit: bool,
}
impl Guard {
    pub(crate) fn new(
        inner: ICloudWriteProvider,
        request: UploadRequest,
        journal: Arc<Mutex<UploadJournal>>,
        completed: Vec<UploadRecord>,
        directory: PathBuf,
        run: Uuid,
        mode: Mode,
    ) -> cirrove_core::upload::Result<Self> {
        request.validate()?;
        if !matches!(&request.representation,cirrove_core::upload::UploadRepresentation::PackageReplacementArchive{semantic,original_semantic,..} | cirrove_core::upload::UploadRepresentation::FlatNumbersReplacementArchive{semantic,original_semantic,..} if semantic.version==2 && original_semantic.version==2)
            || run.is_nil()
        {
            return Err(UploadError::Invalid);
        }
        let operation = match &mode {
            Mode::Lose => None,
            Mode::Recover { operation, .. } => Some(*operation),
        };
        let mutations = journal
            .lock()
            .map_err(|_| UploadError::Uncertain)?
            .list_mutations(0, 2)
            .map_err(|_| UploadError::Uncertain)?;
        if mutations.len() >= 2 {
            return Err(UploadError::Invalid);
        }
        Ok(Self {
            inner,
            request,
            journal,
            completed,
            mutations,
            operation: Mutex::new(operation),
            directory,
            run,
            mode,
            counts: Counts::default(),
            pre_trash_deadline: None,
            pre_trash_limit: Duration::ZERO,
            pre_trash_used: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            hold_instead_of_exit: false,
        })
    }
    /// A future explicit competitor registration supplies its already bounded clock.
    /// Existing loss/recovery callers never opt in and retain their exact behavior.
    pub(crate) fn with_pre_trash_pause(
        mut self,
        active_deadline: tokio::time::Instant,
        pause_limit: Duration,
    ) -> cirrove_core::upload::Result<Self> {
        let remaining = active_deadline.saturating_duration_since(tokio::time::Instant::now());
        if !matches!(self.mode, Mode::Lose)
            || remaining.is_zero()
            || remaining > Duration::from_secs(600)
            || pause_limit < Duration::from_secs(1)
            || pause_limit > Duration::from_secs(120)
        {
            return Err(UploadError::Invalid);
        }
        self.pre_trash_deadline = Some(active_deadline);
        self.pre_trash_limit = pause_limit;
        Ok(self)
    }
    async fn pause_before_trash(
        &self,
        operation: &str,
        request: &UploadRequest,
        checkpoint: &SecretString,
        cancel: &CancellationToken,
    ) -> cirrove_core::upload::Result<()> {
        let Some(active_deadline) = self.pre_trash_deadline else {
            return Ok(());
        };
        let id = self.operation(operation, request)?;
        let adapter = self
            .inner
            .native_package_adapter(operation, request, Some(checkpoint))
            .await?;
        let diagnostic = adapter.native_checkpoint_diagnostic(operation, request, checkpoint)?;
        // Drop the concrete adapter before awaiting any competitor/release event.
        drop(adapter);
        let phase = diagnostic.get("phase").and_then(serde_json::Value::as_str);
        if phase != Some("handoff-move-old-armed") {
            if phase.is_some_and(|v| v.starts_with("handoff-"))
                && !self.pre_trash_used.load(Ordering::SeqCst)
            {
                return Err(UploadError::CheckpointInvalid);
            }
            return Ok(());
        }
        if self.pre_trash_used.swap(true, Ordering::SeqCst) {
            return Err(UploadError::CheckpointInvalid);
        }
        let original = match &request.representation {
            cirrove_core::upload::UploadRepresentation::PackageReplacementArchive {
                original,
                ..
            }
            | cirrove_core::upload::UploadRepresentation::FlatNumbersReplacementArchive {
                original,
                ..
            } => original,
            _ => return Err(UploadError::Invalid),
        };
        let original_id = diagnostic
            .get("original_id")
            .and_then(serde_json::Value::as_str)
            .ok_or(UploadError::CheckpointInvalid)?;
        let staged_id = diagnostic
            .get("staged_id")
            .and_then(serde_json::Value::as_str)
            .ok_or(UploadError::CheckpointInvalid)?;
        let parent_id = diagnostic
            .get("parent_id")
            .and_then(serde_json::Value::as_str)
            .ok_or(UploadError::CheckpointInvalid)?;
        if original_id != original.id
            || Some(parent_id) != original.parent_id.as_deref()
            || staged_id == original_id
            || !staged_id.starts_with("FILE::com.apple.CloudDocs::")
            || staged_id.len() > 4096
        {
            return Err(UploadError::CheckpointInvalid);
        }
        let row = self
            .journal
            .lock()
            .map_err(|_| UploadError::Uncertain)?
            .get(id)
            .map_err(|_| UploadError::CheckpointInvalid)?;
        let attempt = row.attempt.ok_or(UploadError::CheckpointInvalid)?;
        if row.state != UploadState::Uploading || row.session_key != Some(id) {
            return Err(UploadError::CheckpointInvalid);
        }
        let frozen = serde_json::to_vec(&row).map_err(|_| UploadError::Invalid)?;
        let checkpoint_sha256 = hash(checkpoint.expose_secret().as_bytes());
        let until = active_deadline.min(tokio::time::Instant::now() + self.pre_trash_limit);
        if cancel.is_cancelled() || tokio::time::Instant::now() >= until {
            return Err(UploadError::Uncertain);
        }
        match std::fs::symlink_metadata(self.directory.join("pre-trash-release.json")) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err(UploadError::CheckpointInvalid),
        }
        record(&self.directory.join("pre-trash-paused.json"), &serde_json::json!({
            "version":1,"run":self.run,"operation":id,"attempt":attempt,
            "checkpoint_sha256":checkpoint_sha256,"phase":"handoff-move-old-armed",
            "inner_commit_called":false,"original_id":original_id,"staged_id":staged_id,"parent_id":parent_id
        })).map_err(|_| UploadError::Uncertain)?;
        loop {
            // Also catches time consumed by durable marker publication.
            if cancel.is_cancelled() || tokio::time::Instant::now() >= until {
                return Err(UploadError::Uncertain);
            }
            let release = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(self.directory.join("pre-trash-release.json"));
            match release {
                Ok(mut file) => {
                    let meta = file.metadata().map_err(|_| UploadError::Uncertain)?;
                    let uid = std::fs::metadata("/proc/self")
                        .map_err(|_| UploadError::Uncertain)?
                        .uid();
                    if !meta.is_file()
                        || meta.uid() != uid
                        || meta.nlink() != 1
                        || meta.permissions().mode() & 0o7777 != 0o400
                        || meta.len() > 2048
                    {
                        return Err(UploadError::CheckpointInvalid);
                    }
                    let mut bytes = Vec::new();
                    std::io::Read::by_ref(&mut file)
                        .take(2049)
                        .read_to_end(&mut bytes)
                        .map_err(|_| UploadError::Uncertain)?;
                    if bytes.len() > 2048 {
                        return Err(UploadError::CheckpointInvalid);
                    }
                    let release: PreTrashRelease = serde_json::from_slice(&bytes)
                        .map_err(|_| UploadError::CheckpointInvalid)?;
                    if release.version != 1
                        || release.run != self.run
                        || release.operation != id
                        || release.attempt != attempt
                        || release.checkpoint_sha256 != checkpoint_sha256
                    {
                        return Err(UploadError::CheckpointInvalid);
                    }
                    self.operation(operation, request)?;
                    let current = self
                        .journal
                        .lock()
                        .map_err(|_| UploadError::Uncertain)?
                        .get(id)
                        .map_err(|_| UploadError::CheckpointInvalid)?;
                    if serde_json::to_vec(&current).map_err(|_| UploadError::Invalid)? != frozen {
                        return Err(UploadError::CheckpointInvalid);
                    }
                    if cancel.is_cancelled() || tokio::time::Instant::now() >= until {
                        return Err(UploadError::Uncertain);
                    }
                    return Ok(());
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(UploadError::CheckpointInvalid),
            }
            tokio::select! {
                _ = cancel.cancelled() => return Err(UploadError::Uncertain),
                _ = tokio::time::sleep(Duration::from_millis(50)) => {},
            }
        }
    }
    fn refuse<T>(&self) -> cirrove_core::upload::Result<T> {
        self.counts.refused_uploads.fetch_add(1, Ordering::SeqCst);
        Err(UploadError::Uncertain)
    }
    fn refuse_namespace<T>(&self) -> cirrove_core::mutation::Result<T> {
        self.counts.refused_namespace.fetch_add(1, Ordering::SeqCst);
        Err(MutationError::Uncertain)
    }
    fn operation(
        &self,
        operation: &str,
        request: &UploadRequest,
    ) -> cirrove_core::upload::Result<Uuid> {
        let id = Uuid::parse_str(operation).map_err(|_| UploadError::Invalid)?;
        if id.is_nil() || request != &self.request {
            return Err(UploadError::Invalid);
        }
        let journal = self.journal.lock().map_err(|_| UploadError::Uncertain)?;
        let rows = journal.list(0, 4).map_err(|_| UploadError::Uncertain)?;
        if serde_json::to_vec(
            &journal
                .list_mutations(0, 2)
                .map_err(|_| UploadError::Uncertain)?,
        )
        .map_err(|_| UploadError::Invalid)?
            != serde_json::to_vec(&self.mutations).map_err(|_| UploadError::Invalid)?
        {
            return Err(UploadError::Invalid);
        }
        if rows.len() != self.completed.len() + 1
            || self.completed.iter().any(|before| {
                rows.iter()
                    .find(|row| row.id == before.id)
                    .is_none_or(|after| {
                        serde_json::to_vec(before).ok() != serde_json::to_vec(after).ok()
                    })
            })
        {
            return Err(UploadError::Invalid);
        }
        let row = rows
            .iter()
            .find(|row| row.id == id)
            .ok_or(UploadError::Invalid)?;
        if row.scope != request.scope
            || row.intent != request.intent
            || row.representation != request.representation
            || row.size != request.size
            || row.sha256 != request.sha256
            || row.working_file.is_some()
            || row.base.is_some()
            || !matches!(row.state, UploadState::Uploading | UploadState::Verifying)
            || row.remote.is_some()
            || row.package_completion.is_some()
            || row.identity_handoff.as_ref().is_some_and(|h| {
                serde_json::to_value(h)
                    .ok()
                    .is_none_or(|v| !v["backup"].is_null())
            })
        {
            return Err(UploadError::Invalid);
        }
        drop(journal);
        let mut bound = self.operation.lock().map_err(|_| UploadError::Uncertain)?;
        match *bound {
            Some(expected) if expected != id => return Err(UploadError::Invalid),
            None => {
                if !matches!(self.mode, Mode::Lose) {
                    return Err(UploadError::Invalid);
                }
                record(&self.directory.join("operation-bound.json"),&serde_json::json!({"version":1,"run":self.run,"operation":id,"request":request})).map_err(|_|UploadError::Uncertain)?;
                *bound = Some(id);
            }
            _ => {}
        }
        Ok(id)
    }
    async fn armed(
        &self,
        operation: &str,
        request: &UploadRequest,
        checkpoint: &SecretString,
    ) -> cirrove_core::upload::Result<()> {
        self.operation(operation, request)?;
        if let Mode::Recover {
            checkpoint_sha256, ..
        } = &self.mode
            && hash(checkpoint.expose_secret().as_bytes()) != *checkpoint_sha256
        {
            return Err(UploadError::CheckpointInvalid);
        }
        let adapter = self
            .inner
            .native_package_adapter(operation, request, Some(checkpoint))
            .await?;
        let diagnostic = adapter.native_checkpoint_diagnostic(operation, request, checkpoint)?;
        if diagnostic.get("phase").and_then(serde_json::Value::as_str)
            != Some("handoff-install-armed")
        {
            return Err(UploadError::CheckpointInvalid);
        }
        Ok(())
    }
    fn terminal(&self, receipt: &PackageHandoffReceipt) -> cirrove_core::upload::Result<Receipt> {
        let actual = Receipt::actual(receipt);
        actual.check(&self.request)?;
        if let Mode::Recover { receipt, .. } = &self.mode
            && receipt.as_ref() != &actual
        {
            return Err(UploadError::CheckpointInvalid);
        }
        Ok(actual)
    }
    fn mutation_allowed(
        &self,
        operation: &str,
        request: &UploadRequest,
    ) -> cirrove_core::upload::Result<()> {
        if matches!(self.mode, Mode::Recover { .. }) {
            return self.refuse();
        }
        self.operation(operation, request)?;
        Ok(())
    }
    fn lost(
        &self,
        operation: &str,
        request: &UploadRequest,
        checkpoint: &SecretString,
        receipt: &PackageHandoffReceipt,
    ) -> cirrove_core::upload::Result<()> {
        let actual = self.terminal(receipt)?;
        let id = Uuid::parse_str(operation).map_err(|_| UploadError::Invalid)?;
        let journal = self.journal.lock().map_err(|_| UploadError::Uncertain)?;
        let row = journal.get(id).map_err(|_| UploadError::Uncertain)?;
        if row.state != UploadState::Uploading
            || row.attempt.is_none()
            || row.session_key != Some(id)
            || row.transferred_bytes != row.size
            || row.remote.is_some()
            || row.package_completion.is_some()
            || row.identity_handoff.as_ref().is_none_or(|h| {
                serde_json::to_value(h)
                    .ok()
                    .is_none_or(|v| !v["backup"].is_null())
            })
        {
            return Err(UploadError::Invalid);
        }
        drop(journal);
        let marker = LossMarker {
            version: 1,
            run: self.run,
            operation: id,
            pid: std::process::id(),
            request: request.clone(),
            checkpoint_sha256: hash(checkpoint.expose_secret().as_bytes()),
            receipt: actual,
            acknowledgement_returned: false,
        };
        #[cfg(test)]
        if self.hold_instead_of_exit {
            record(
                &self.directory.join("lost-final-confirmation.json"),
                &marker,
            )
            .map_err(|_| UploadError::Uncertain)?;
            return Err(UploadError::Uncertain);
        }
        persist_marker_and_exit(&self.directory, &marker)
    }
}

#[async_trait::async_trait]
impl UploadProvider for Guard {
    fn requires_begin_payload(&self, r: &UploadRequest) -> bool {
        matches!(self.mode, Mode::Lose)
            && r == &self.request
            && self.inner.requires_begin_payload(r)
    }
    fn inspection_timeout(&self, r: &UploadRequest) -> Duration {
        self.inner.inspection_timeout(r)
    }
    fn commit_timeout(&self, r: &UploadRequest) -> Duration {
        self.inner.commit_timeout(r)
    }
    fn begin_is_mutation_free_until_checkpoint(&self, r: &UploadRequest) -> bool {
        matches!(self.mode, Mode::Lose)
            && r == &self.request
            && self.inner.begin_is_mutation_free_until_checkpoint(r)
    }
    fn staged_recovery_location(&self, o: &str, r: &UploadRequest) -> Option<RecoveryLocation> {
        if r != &self.request
            || self
                .operation
                .lock()
                .ok()
                .and_then(|v| *v)
                .is_some_and(|id| id.to_string() != o)
        {
            None
        } else {
            self.inner.staged_recovery_location(o, r)
        }
    }
    async fn begin_upload(
        &self,
        _: &UploadRequest,
        _: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.refuse()
    }
    async fn upload_part(
        &self,
        _: &UploadRequest,
        _: &SecretString,
        _: u64,
        _: Vec<u8>,
        _: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.refuse()
    }
    async fn upload_stream(
        &self,
        _: &UploadRequest,
        _: &SecretString,
        _: File,
        _: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.refuse()
    }
    async fn commit_upload(
        &self,
        _: &UploadRequest,
        _: &SecretString,
        _: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.refuse()
    }
    async fn inspect_upload(
        &self,
        _: &UploadRequest,
        _: &SecretString,
        _: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.refuse()
    }
    async fn reconcile_upload(
        &self,
        _: &UploadRequest,
        _: Option<&SecretString>,
        _: &CancellationToken,
    ) -> cirrove_core::upload::Result<Reconciliation> {
        self.refuse()
    }
    async fn upload_part_for_operation(
        &self,
        _: &str,
        _: &UploadRequest,
        _: &SecretString,
        _: u64,
        _: Vec<u8>,
        _: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.refuse()
    }
    async fn begin_upload_for_operation(
        &self,
        o: &str,
        r: &UploadRequest,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.mutation_allowed(o, r)?;
        self.inner.begin_upload_for_operation(o, r, c).await
    }
    async fn begin_upload_from_payload_for_operation(
        &self,
        o: &str,
        r: &UploadRequest,
        f: std::fs::File,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.mutation_allowed(o, r)?;
        self.inner
            .begin_upload_from_payload_for_operation(o, r, f, c)
            .await
    }
    async fn allocate_upload_for_operation(
        &self,
        o: &str,
        r: &UploadRequest,
        s: &SecretString,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.mutation_allowed(o, r)?;
        self.inner.allocate_upload_for_operation(o, r, s, c).await
    }
    async fn upload_stream_for_operation(
        &self,
        o: &str,
        r: &UploadRequest,
        s: &SecretString,
        f: File,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.mutation_allowed(o, r)?;
        self.inner.upload_stream_for_operation(o, r, s, f, c).await
    }
    async fn commit_upload_for_operation(
        &self,
        o: &str,
        r: &UploadRequest,
        s: &SecretString,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.mutation_allowed(o, r)?;
        self.pause_before_trash(o, r, s, c).await?;
        let step = self.inner.commit_upload_for_operation(o, r, s, c).await?;
        if let UploadStep::PackageHandoffComplete(receipt) = &step {
            self.armed(o, r, s).await?;
            self.lost(o, r, s, receipt)?;
        }
        Ok(step)
    }
    async fn inspect_upload_for_operation(
        &self,
        o: &str,
        r: &UploadRequest,
        s: &SecretString,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        if matches!(self.mode, Mode::Lose) {
            self.operation(o, r)?;
            let step = self.inner.inspect_upload_for_operation(o, r, s, c).await?;
            if let UploadStep::PackageHandoffComplete(receipt) = &step {
                self.armed(o, r, s).await?;
                self.lost(o, r, s, receipt)?;
            }
            return Ok(step);
        }
        self.armed(o, r, s).await?;
        self.counts.inspections.fetch_add(1, Ordering::SeqCst);
        match self.inner.inspect_upload_for_operation(o, r, s, c).await? {
            UploadStep::PackageHandoffComplete(receipt) => {
                self.terminal(&receipt)?;
                Ok(UploadStep::PackageHandoffComplete(receipt))
            }
            _ => Err(UploadError::Uncertain),
        }
    }
    async fn reconcile_upload_for_operation(
        &self,
        o: &str,
        r: &UploadRequest,
        s: Option<&SecretString>,
        c: &CancellationToken,
    ) -> cirrove_core::upload::Result<Reconciliation> {
        let saved = s.ok_or(UploadError::CheckpointInvalid)?;
        self.armed(o, r, saved).await?;
        self.counts.reconciliations.fetch_add(1, Ordering::SeqCst);
        match self
            .inner
            .reconcile_upload_for_operation(o, r, s, c)
            .await?
        {
            Reconciliation::PackageHandoffCommitted(receipt) => {
                self.terminal(&receipt)?;
                if matches!(self.mode, Mode::Lose) {
                    self.lost(o, r, saved, &receipt)?;
                }
                Ok(Reconciliation::PackageHandoffCommitted(receipt))
            }
            _ => Err(UploadError::Uncertain),
        }
    }
}
#[async_trait::async_trait]
impl MutationProvider for Guard {
    async fn delete_permanently(
        &self,
        _: &Scope,
        _: &str,
        _: Option<&str>,
        _: &CancellationToken,
    ) -> cirrove_core::mutation::Result<()> {
        self.refuse_namespace()
    }
    async fn prepare_mutation(
        &self,
        _: &MutationRequest,
        _: &CancellationToken,
    ) -> cirrove_core::mutation::Result<Option<String>> {
        self.refuse_namespace()
    }
    async fn prepare_mutation_for_operation(
        &self,
        _: &str,
        _: &MutationRequest,
        _: &CancellationToken,
    ) -> cirrove_core::mutation::Result<Option<String>> {
        self.refuse_namespace()
    }
    async fn mutate(
        &self,
        _: &MutationRequest,
        _: &CancellationToken,
    ) -> cirrove_core::mutation::Result<MutationReceipt> {
        self.refuse_namespace()
    }
    async fn mutate_prepared(
        &self,
        _: &MutationRequest,
        _: Option<&str>,
        _: &CancellationToken,
    ) -> cirrove_core::mutation::Result<MutationReceipt> {
        self.refuse_namespace()
    }
    async fn mutate_operation(
        &self,
        _: &str,
        _: &MutationRequest,
        _: Option<&str>,
        _: &CancellationToken,
    ) -> cirrove_core::mutation::Result<MutationReceipt> {
        self.refuse_namespace()
    }
    async fn reconcile_mutation(
        &self,
        _: &MutationRequest,
        _: &CancellationToken,
    ) -> cirrove_core::mutation::Result<MutationReconciliation> {
        self.refuse_namespace()
    }
    async fn reconcile_prepared_mutation(
        &self,
        _: &MutationRequest,
        _: Option<&str>,
        _: &CancellationToken,
    ) -> cirrove_core::mutation::Result<MutationReconciliation> {
        self.refuse_namespace()
    }
    async fn reconcile_operation(
        &self,
        _: &str,
        _: &MutationRequest,
        _: Option<&str>,
        _: &CancellationToken,
    ) -> cirrove_core::mutation::Result<MutationReconciliation> {
        self.refuse_namespace()
    }
}

pub(crate) fn persist_marker_and_exit(
    directory: &Path,
    marker: &LossMarker,
) -> cirrove_core::upload::Result<()> {
    if marker.pid != std::process::id() || marker.acknowledgement_returned {
        return Err(UploadError::Invalid);
    }
    record(&directory.join("lost-final-confirmation.json"), marker)
        .map_err(|_| UploadError::Uncertain)?;
    std::process::exit(86)
}
pub(crate) mod registration;
pub use registration::{icloud_native_final_loss, icloud_native_final_recover};

#[cfg(test)]
#[path = "native_final_recovery/flat_authority_tests.rs"]
mod flat_authority_tests;
