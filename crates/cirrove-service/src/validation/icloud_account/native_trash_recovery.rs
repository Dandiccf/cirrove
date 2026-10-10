//! Feature-only loss of a genuine standalone native Trash receipt, before ACK.
//! Recovery can inspect the registered operation, but cannot submit any write.
use crate::journal::{MutationRecord, MutationState, UploadJournal, UploadRecord, UploadState};
use cirrove_core::{
    CancellationToken, Scope,
    mutation::{
        MutationError, MutationIntent, MutationProvider, MutationReceipt, MutationReconciliation,
        MutationRequest,
    },
    upload::{Reconciliation, UploadError, UploadProvider, UploadRequest, UploadStep},
};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use uuid::Uuid;

#[cfg(test)]
mod guard_tests;
mod metadata;
mod registration;
pub use metadata::icloud_owned_native_trash_metadata;
#[cfg(test)]
mod tls_tests;
mod verify;
pub use registration::{icloud_native_trash_loss, icloud_native_trash_recover};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LossMarker {
    pub version: u8,
    pub run: Uuid,
    pub operation: Uuid,
    pub pid: u32,
    pub request: MutationRequest,
    pub prepared_item: String,
    pub receipt: MutationReceipt,
    pub acknowledgement_returned: bool,
}
#[derive(Clone)]
pub(crate) enum Mode {
    Lose,
    Recover {
        operation: Uuid,
        receipt: MutationReceipt,
    },
}
#[derive(Default)]
pub(crate) struct Counts {
    mutations: AtomicU64,
    preparations: AtomicU64,
    inspections: AtomicU64,
    reconciliations: AtomicU64,
    refused_mutations: AtomicU64,
    refused_uploads: AtomicU64,
}
impl Counts {
    pub(crate) fn value(&self) -> serde_json::Value {
        serde_json::json!({
            "mutations":self.mutations.load(Ordering::SeqCst),
            "preparations":self.preparations.load(Ordering::SeqCst),
            "inspections":self.inspections.load(Ordering::SeqCst),
            "reconciliations":self.reconciliations.load(Ordering::SeqCst),
            "refused_mutations":self.refused_mutations.load(Ordering::SeqCst),
            "refused_uploads":self.refused_uploads.load(Ordering::SeqCst)
        })
    }
}
pub(crate) struct Guard {
    inner: Arc<dyn MutationProvider>,
    request: MutationRequest,
    journal: Arc<Mutex<UploadJournal>>,
    completed_uploads: Vec<UploadRecord>,
    completed_mutations: Vec<MutationRecord>,
    directory: PathBuf,
    run: Uuid,
    mode: Mode,
    operation: Mutex<Option<Uuid>>,
    attempt: Mutex<Option<Uuid>>,
    pub counts: Counts,
    #[cfg(test)]
    pub hold_instead_of_exit: bool,
}
fn equal<T: Serialize>(a: &T, b: &T) -> bool {
    matches!((serde_json::to_vec(a), serde_json::to_vec(b)), (Ok(a), Ok(b)) if a == b)
}
impl Guard {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        inner: Arc<dyn MutationProvider>,
        request: MutationRequest,
        journal: Arc<Mutex<UploadJournal>>,
        completed_uploads: Vec<UploadRecord>,
        completed_mutations: Vec<MutationRecord>,
        directory: PathBuf,
        run: Uuid,
        mode: Mode,
    ) -> cirrove_core::mutation::Result<Self> {
        request.validate()?;
        let MutationIntent::TrashNativeDocument { before } = &request.intent else {
            return Err(MutationError::Invalid);
        };
        if run.is_nil()
            || request.scope.provider != "icloud"
            || request.scope.collection != "drive"
            || Uuid::parse_str(&request.scope.account)
                .ok()
                .is_none_or(|id| id.is_nil())
            || before
                .id
                .strip_prefix("FILE::com.apple.CloudDocs::")
                .is_none_or(|id| id.is_empty() || id.contains(['/', '\0', ':']))
            || before
                .parent_id
                .as_deref()
                .and_then(|p| p.strip_prefix("FOLDER::com.apple.CloudDocs::"))
                .is_none_or(|p| p.is_empty() || p == "TRASH_ROOT" || p.contains(['/', '\0', ':']))
            || before.name.len() > 255
            || cirrove_core::upload::native_package_suffix(&before.name).is_none()
            || before.etag.as_deref().is_none_or(str::is_empty)
            || before
                .content_version
                .as_ref()
                .is_some_and(|version| Some(version) != before.etag.as_ref())
            || before.target.is_some()
            || completed_uploads.len() > 998
            || completed_mutations.len() > 998
            || completed_uploads.iter().any(|r| {
                r.scope != request.scope || r.state != UploadState::Uploaded || r.attempt.is_some()
            })
            || completed_mutations.iter().any(|r| {
                r.request.scope != request.scope
                    || r.state != MutationState::Applied
                    || r.attempt.is_some()
            })
        {
            return Err(MutationError::Invalid);
        }
        let operation = match &mode {
            Mode::Lose => None,
            Mode::Recover { operation, receipt } => {
                if operation.is_nil() || !request.accepts(receipt) {
                    return Err(MutationError::Invalid);
                }
                Some(*operation)
            }
        };
        Ok(Self {
            inner,
            request,
            journal,
            completed_uploads,
            completed_mutations,
            directory,
            run,
            mode,
            operation: Mutex::new(operation),
            attempt: Mutex::new(None),
            counts: Counts::default(),
            #[cfg(test)]
            hold_instead_of_exit: false,
        })
    }
    fn refuse<T>(&self) -> cirrove_core::mutation::Result<T> {
        self.counts.refused_mutations.fetch_add(1, Ordering::SeqCst);
        Err(MutationError::Uncertain)
    }
    fn refuse_upload<T>(&self) -> cirrove_core::upload::Result<T> {
        self.counts.refused_uploads.fetch_add(1, Ordering::SeqCst);
        Err(UploadError::Uncertain)
    }
    fn binding(
        &self,
        operation: &str,
        request: &MutationRequest,
        prepared: Option<&str>,
        preparing: bool,
    ) -> cirrove_core::mutation::Result<Uuid> {
        let id = Uuid::parse_str(operation).map_err(|_| MutationError::Invalid)?;
        if id.is_nil() || request != &self.request {
            return self.refuse();
        }
        let before = self.request.intent.before().ok_or(MutationError::Invalid)?;
        let journal = self.journal.lock().map_err(|_| MutationError::Uncertain)?;
        let uploads = journal
            .list(0, (self.completed_uploads.len() + 1) as u32)
            .map_err(|_| MutationError::Uncertain)?;
        let mutations = journal
            .list_mutations(0, (self.completed_mutations.len() + 2) as u32)
            .map_err(|_| MutationError::Uncertain)?;
        if !equal(&uploads, &self.completed_uploads)
            || mutations.len() != self.completed_mutations.len() + 1
            || !equal(
                &mutations
                    .iter()
                    .filter(|r| r.id != id)
                    .cloned()
                    .collect::<Vec<_>>(),
                &self.completed_mutations,
            )
        {
            return self.refuse();
        }
        let row = journal
            .native_trash_record(id)
            .map_err(|_| MutationError::Invalid)?;
        let state = match self.mode {
            Mode::Lose => MutationState::Applying,
            Mode::Recover { .. } => MutationState::Verifying,
        };
        if row.request != *request
            || row.state != state
            || row.attempt.is_none()
            || row.receipt.is_some()
            || row.verified_content.is_some()
            || !row.local_ready
            || row.base.is_some()
            || row.working_file.is_some()
            || if preparing {
                prepared.is_some() || row.prepared_item.is_some()
            } else {
                prepared != Some(before.id.as_str()) || row.prepared_item.as_deref() != prepared
            }
        {
            return self.refuse();
        }
        drop(journal);
        let mut latch = self
            .operation
            .lock()
            .map_err(|_| MutationError::Uncertain)?;
        match *latch {
            Some(expected) if expected != id => return self.refuse(),
            None if matches!(self.mode, Mode::Lose) => *latch = Some(id),
            None => return self.refuse(),
            _ => (),
        }
        let mut attempt = self.attempt.lock().map_err(|_| MutationError::Uncertain)?;
        match *attempt {
            Some(expected) if row.attempt != Some(expected) => return self.refuse(),
            None => *attempt = row.attempt,
            _ => (),
        }
        Ok(id)
    }
    fn lost(
        &self,
        operation: Uuid,
        request: &MutationRequest,
        prepared: &str,
        receipt: MutationReceipt,
    ) -> cirrove_core::mutation::Result<()> {
        let marker = LossMarker {
            version: 1,
            run: self.run,
            operation,
            pid: std::process::id(),
            request: request.clone(),
            prepared_item: prepared.into(),
            receipt,
            acknowledgement_returned: false,
        };
        let bytes = serde_json::to_vec_pretty(&marker).map_err(|_| MutationError::Invalid)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(self.directory.join("lost-native-trash-confirmation.json"))
            .map_err(|_| MutationError::Uncertain)?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| MutationError::Uncertain)?;
        File::open(&self.directory)
            .and_then(|f| f.sync_all())
            .map_err(|_| MutationError::Uncertain)?;
        #[cfg(test)]
        if self.hold_instead_of_exit {
            return Err(MutationError::Uncertain);
        }
        std::process::exit(86);
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
        self.refuse()
    }
    async fn prepare_mutation(
        &self,
        _: &MutationRequest,
        _: &CancellationToken,
    ) -> cirrove_core::mutation::Result<Option<String>> {
        self.refuse()
    }
    async fn mutate(
        &self,
        _: &MutationRequest,
        _: &CancellationToken,
    ) -> cirrove_core::mutation::Result<MutationReceipt> {
        self.refuse()
    }
    async fn reconcile_mutation(
        &self,
        _: &MutationRequest,
        _: &CancellationToken,
    ) -> cirrove_core::mutation::Result<MutationReconciliation> {
        self.refuse()
    }
    async fn mutate_prepared(
        &self,
        _: &MutationRequest,
        _: Option<&str>,
        _: &CancellationToken,
    ) -> cirrove_core::mutation::Result<MutationReceipt> {
        self.refuse()
    }
    async fn reconcile_prepared_mutation(
        &self,
        _: &MutationRequest,
        _: Option<&str>,
        _: &CancellationToken,
    ) -> cirrove_core::mutation::Result<MutationReconciliation> {
        self.refuse()
    }
    async fn prepare_mutation_for_operation(
        &self,
        o: &str,
        r: &MutationRequest,
        c: &CancellationToken,
    ) -> cirrove_core::mutation::Result<Option<String>> {
        if !matches!(self.mode, Mode::Lose) || c.is_cancelled() {
            return self.refuse();
        }
        self.binding(o, r, None, true)?;
        if self
            .counts
            .preparations
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return self.refuse();
        }
        let prepared = self.inner.prepare_mutation_for_operation(o, r, c).await?;
        self.binding(o, r, None, true)?;
        if prepared.as_deref() != r.intent.before().map(|n| n.id.as_str()) {
            return self.refuse();
        }
        Ok(prepared)
    }
    async fn mutate_operation(
        &self,
        o: &str,
        r: &MutationRequest,
        p: Option<&str>,
        c: &CancellationToken,
    ) -> cirrove_core::mutation::Result<MutationReceipt> {
        if !matches!(self.mode, Mode::Lose) || c.is_cancelled() {
            return self.refuse();
        }
        let id = self.binding(o, r, p, false)?;
        if self
            .counts
            .mutations
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return self.refuse();
        }
        let receipt = self.inner.mutate_operation(o, r, p, c).await?;
        self.binding(o, r, p, false)?;
        if !r.accepts(&receipt) || !matches!(receipt, MutationReceipt::Removed { .. }) {
            return self.refuse();
        }
        self.lost(id, r, p.ok_or(MutationError::Invalid)?, receipt)?;
        Err(MutationError::Uncertain)
    }
    async fn reconcile_operation(
        &self,
        o: &str,
        r: &MutationRequest,
        p: Option<&str>,
        c: &CancellationToken,
    ) -> cirrove_core::mutation::Result<MutationReconciliation> {
        let Mode::Recover {
            receipt: expected, ..
        } = &self.mode
        else {
            return self.refuse();
        };
        if c.is_cancelled() {
            return self.refuse();
        }
        self.binding(o, r, p, false)?;
        self.counts.inspections.fetch_add(1, Ordering::SeqCst);
        self.counts.reconciliations.fetch_add(1, Ordering::SeqCst);
        let outcome = self.inner.reconcile_operation(o, r, p, c).await?;
        self.binding(o, r, p, false)?;
        match outcome {
            MutationReconciliation::Applied(receipt)
                if receipt == *expected && r.accepts(&receipt) =>
            {
                Ok(MutationReconciliation::Applied(receipt))
            }
            MutationReconciliation::Uncommitted | MutationReconciliation::Indeterminate => {
                Ok(MutationReconciliation::Indeterminate)
            }
            _ => self.refuse(),
        }
    }
}
#[async_trait::async_trait]
impl UploadProvider for Guard {
    async fn begin_upload(
        &self,
        _: &UploadRequest,
        _: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.refuse_upload()
    }
    async fn allocate_upload_for_operation(
        &self,
        _: &str,
        _: &UploadRequest,
        _: &SecretString,
        _: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.refuse_upload()
    }
    async fn inspect_upload(
        &self,
        _: &UploadRequest,
        _: &SecretString,
        _: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.refuse_upload()
    }
    async fn upload_part(
        &self,
        _: &UploadRequest,
        _: &SecretString,
        _: u64,
        _: Vec<u8>,
        _: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.refuse_upload()
    }
    async fn upload_stream(
        &self,
        _: &UploadRequest,
        _: &SecretString,
        _: File,
        _: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.refuse_upload()
    }
    async fn commit_upload(
        &self,
        _: &UploadRequest,
        _: &SecretString,
        _: &CancellationToken,
    ) -> cirrove_core::upload::Result<UploadStep> {
        self.refuse_upload()
    }
    async fn reconcile_upload(
        &self,
        _: &UploadRequest,
        _: Option<&SecretString>,
        _: &CancellationToken,
    ) -> cirrove_core::upload::Result<Reconciliation> {
        self.refuse_upload()
    }
}
