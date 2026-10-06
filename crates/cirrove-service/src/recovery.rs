//! Local-only recovery access. This never constructs write workers or a provider.
use crate::{engine::Engine, filesystem::WriteControl, journal::*};
use anyhow::{Context, Result, bail};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[derive(Clone)]
pub(crate) struct RecoveryControl {
    // Blocking copies must retain the account lease as well as the journal lease.
    access: Access,
    engine: Arc<Engine>,
}
#[derive(Clone)]
enum Access {
    Writer(WriteControl),
    ReadOnly(Option<RecoveryLease>),
}
/// Every strong journal reference is owned by a lease. Closing its last Arc
/// and opening a successor use the same gate: Weak expiry alone does not mean
/// the journal destructor has finished releasing its SQLite/file owners.
#[derive(Clone)]
struct RecoveryLease {
    journal: Option<Arc<Mutex<RecoveryJournal>>>,
    // Also protect the account if spawn_blocking returns an abandoned lease
    // after the async caller has been cancelled.
    engine: Arc<Engine>,
}
impl Drop for RecoveryLease {
    fn drop(&mut self) {
        let _gate = self
            .engine
            .recovery_journal_gate
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(journal) = self.journal.take() {
            match Arc::try_unwrap(journal) {
                Ok(journal) => {
                    // At this point Weak::upgrade fails, but the journal and
                    // flock still exist. Keep the gate through their full drop.
                    #[cfg(test)]
                    if let Some(probe) = self
                        .engine
                        .recovery_test_hooks
                        .closing
                        .lock()
                        .expect("recovery test hook is not poisoned")
                        .take()
                    {
                        let _ = probe.entered.send(());
                        let _ = probe
                            .release
                            .recv_timeout(std::time::Duration::from_secs(5));
                    }
                    drop(journal);
                }
                Err(shared) => drop(shared),
            }
        }
    }
}
#[cfg(test)]
#[derive(Default)]
pub(crate) struct RecoveryTestHooks {
    pub(crate) closing: Mutex<Option<RecoveryCloseProbe>>,
    pub(crate) opening: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    pub(crate) publishing: Mutex<Option<RecoveryCloseProbe>>,
}
#[cfg(test)]
pub(crate) struct RecoveryCloseProbe {
    pub(crate) entered: tokio::sync::oneshot::Sender<()>,
    pub(crate) release: std::sync::mpsc::Receiver<()>,
}
impl RecoveryControl {
    pub(crate) async fn native_trash_status(&self, id: Uuid) -> Result<(MutationRecord, bool)> {
        match &self.access {
            Access::Writer(writer) => {
                let record = writer.native_trash_record(id).await?;
                let absent = record.state == MutationState::Applied
                    && writer.native_trash_publication(id).await?
                        == PackagePublicationStatus::Absent;
                Ok((record, absent))
            }
            Access::ReadOnly(None) => bail!("no retained native Trash operation"),
            Access::ReadOnly(Some(_)) => self.local(move |j| j.native_trash_status(id)).await,
        }
    }
    pub(crate) fn writer(engine: Arc<Engine>, writer: WriteControl) -> Result<Self> {
        if engine.cancel.is_cancelled() || !writer.belongs_to(&engine) {
            bail!("account changed; refresh before exporting");
        }
        Ok(Self {
            engine,
            access: Access::Writer(writer),
        })
    }
    pub(crate) async fn read_only(engine: Arc<Engine>) -> Result<Self> {
        if engine.account.access != cirrove_auth::AccessMode::ReadOnly
            || !engine.account.enabled
            || engine.cancel.is_cancelled()
        {
            bail!("account is not available for read-only recovery");
        }
        let cached = engine.recovery_journal.clone().lock_owned().await;
        let journal = if let Some(journal) = cached.upgrade() {
            let lease = RecoveryLease {
                journal: Some(journal),
                engine: engine.clone(),
            };
            drop(cached);
            Some(lease)
        } else {
            let owner = engine.clone();
            tokio::task::spawn_blocking(move || -> Result<_> {
                // Transfer the cache guard with the blocking operation. If the
                // caller is cancelled, another opener still cannot miss the
                // live lease before this worker publishes its weak reference.
                let mut cached = cached;
                #[cfg(test)]
                if let Some(opening) = owner
                    .recovery_test_hooks
                    .opening
                    .lock()
                    .expect("recovery test hook is not poisoned")
                    .take()
                {
                    let _ = opening.send(());
                }
                let _gate = owner
                    .recovery_journal_gate
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let path = owner
                    .db
                    .parent()
                    .context("account state unavailable")?
                    .join("journal");
                match std::fs::symlink_metadata(&path) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                    Err(_) => bail!("local recovery journal unavailable"),
                    Ok(_) => {}
                }
                let journal = RecoveryJournal::open(&path, &owner.account.id).map_err(|_| {
                    anyhow::anyhow!("local recovery journal is busy or unavailable")
                })?;
                // Return the lease itself: if the async caller is cancelled,
                // spawn_blocking's abandoned result still closes under the gate.
                let lease = RecoveryLease {
                    journal: Some(Arc::new(Mutex::new(journal))),
                    engine: owner.clone(),
                };
                #[cfg(test)]
                if let Some(probe) = owner
                    .recovery_test_hooks
                    .publishing
                    .lock()
                    .expect("recovery test hook is not poisoned")
                    .take()
                {
                    let _ = probe.entered.send(());
                    let _ = probe
                        .release
                        .recv_timeout(std::time::Duration::from_secs(5));
                }
                *cached = Arc::downgrade(lease.journal.as_ref().expect("live recovery lease"));
                // The returned lease outlives these local guards. Its Drop can
                // therefore acquire the gate even if its receiver was aborted.
                Ok(Some(lease))
            })
            .await??
        };
        if engine.cancel.is_cancelled() {
            bail!("account stopped; refresh before exporting");
        }
        Ok(Self {
            engine,
            access: Access::ReadOnly(journal),
        })
    }
    async fn local<T: Send + 'static>(
        &self,
        work: impl FnOnce(&RecoveryJournal) -> crate::journal::Result<T> + Send + 'static,
    ) -> Result<T> {
        let owner = self.clone();
        tokio::task::spawn_blocking(move || {
            let _account = &owner.engine;
            let Access::ReadOnly(Some(journal)) = &owner.access else {
                bail!("no retained local journal");
            };
            let journal = journal
                .journal
                .as_ref()
                .context("recovery lease closed")?
                .lock()
                .map_err(|_| anyhow::anyhow!("local recovery journal unavailable"))?;
            work(&journal)
                .map_err(|_| anyhow::anyhow!("local recovery version changed or is unavailable"))
        })
        .await?
    }
    pub(crate) async fn working_recovery_list(
        &self,
        after: Option<Uuid>,
        limit: u32,
    ) -> Result<(Vec<WorkingRecovery>, Option<Uuid>)> {
        match &self.access {
            Access::Writer(writer) => Ok(writer.working_recovery_list(after, limit).await?),
            Access::ReadOnly(None) => Ok((Vec::new(), None)),
            _ => {
                self.local(move |j| j.working_recovery_list(after, limit))
                    .await
            }
        }
    }
    pub(crate) async fn working_export_source(
        &self,
        id: Uuid,
        generation: u64,
    ) -> Result<WorkingExportSource> {
        match &self.access {
            Access::Writer(writer) => Ok(writer.working_export_source(id, generation).await?),
            _ => {
                self.local(move |j| j.working_export_source(id, generation))
                    .await
            }
        }
    }
    pub(crate) async fn verify_working_export(
        &self,
        prepared: PreparedWorkingExport,
    ) -> Result<VerifiedWorkingExport> {
        match &self.access {
            Access::Writer(writer) => Ok(writer.verify_working_export(prepared).await?),
            _ => self.local(move |j| j.verify_working_export(prepared)).await,
        }
    }
    pub(crate) async fn local_export_source(&self, id: Uuid) -> Result<LocalExportSource> {
        match &self.access {
            Access::Writer(writer) => Ok(writer.local_export_source(id).await?),
            _ => self.local(move |j| j.local_export_source(id)).await,
        }
    }
    pub(crate) async fn recent_local(
        &self,
        limit: usize,
    ) -> Result<Vec<crate::recent::LocalChange>> {
        match &self.access {
            Access::Writer(writer) => Ok(writer.recent_local(limit.min(200)).await?),
            Access::ReadOnly(None) => Ok(Vec::new()),
            _ => {
                let engine = self.engine.clone();
                self.local(move |journal| {
                    journal
                        .recent_uploads(limit.min(200) as u32)?
                        .into_iter()
                        .map(|record| {
                            // One journal can retain linked collections. Keep
                            // its recorded collection, not the primary drive.
                            if record.scope.account != engine.account.id
                                || record.scope.provider != engine.provider.provider_id()
                                || record.scope.collection.is_empty()
                            {
                                return Err(JournalError::Corrupt);
                            }
                            let completed_name = if record.state == UploadState::Uploaded
                                && let cirrove_core::upload::UploadIntent::Replace { item, .. } =
                                    &record.intent
                            {
                                let current =
                                    record.remote.as_ref().ok_or(JournalError::Corrupt)?;
                                let ordinary = record.representation.is_file_bytes()
                                    && current.kind == cirrove_core::NodeKind::File
                                    && !current.package
                                    && current.size == record.size
                                    && if record.identity_handoff.is_some() {
                                        record.ordinary_handoff_receipt().is_some()
                                    } else {
                                        current.id == *item
                                    };
                                if (!ordinary && record.native_replacement_receipt().is_none())
                                    || current.id.is_empty()
                                    || current.target.is_some()
                                    || current.content_revision().is_none()
                                    || current.parent_id.as_ref().is_none_or(|p| p.is_empty())
                                    || current.name.is_empty()
                                    || current.name.len() > 4096
                                    || matches!(current.name.as_str(), "." | "..")
                                    || current.name.contains(['\0', '/'])
                                {
                                    return Err(JournalError::Corrupt);
                                }
                                // Historical saved-document presentation, not
                                // a lookup of its former item in hidden Trash.
                                Some(current.name.clone())
                            } else {
                                None
                            };
                            let (name, item) = match record.intent {
                                cirrove_core::upload::UploadIntent::Create { name, .. } => {
                                    (name, None)
                                }
                                cirrove_core::upload::UploadIntent::Replace { item, .. } => {
                                    let name = match completed_name {
                                        Some(name) => name,
                                        None => {
                                            match journal.retained_name(&record.scope, &item)? {
                                                Some(name) => name,
                                                None => engine
                                                    .cached_recovery_name(&record.scope, &item)
                                                    .map_err(|_| JournalError::Corrupt)?
                                                    .unwrap_or_else(|| item.clone()),
                                            }
                                        }
                                    };
                                    (name, Some(item))
                                }
                            };
                            Ok(crate::recent::LocalChange {
                                operation: Some(record.id),
                                sequence: record.sequence,
                                name,
                                item,
                                state: format!("{:?}", record.state).to_ascii_lowercase(),
                                size: record.size,
                                saved_at: (record.saved_at > 0).then_some(record.saved_at),
                                transferred: record.transferred_bytes,
                            })
                        })
                        .collect()
                })
                .await
            }
        }
    }
    /// An absent journal is known empty; a failed inspection is not. The caller
    /// keeps its last published counts when this returns an error.
    pub(crate) async fn retained_outcome_counts(&self) -> Result<(u64, u64, u64)> {
        match &self.access {
            Access::ReadOnly(None) => Ok((0, 0, 0)),
            Access::ReadOnly(Some(_)) => self.local(|j| j.retained_outcome_counts()).await,
            Access::Writer(_) => bail!("read-only retained inspection requires no writer"),
        }
    }
    pub(crate) async fn retained_failure_history(
        &self,
        limit: usize,
    ) -> Result<(
        Vec<crate::recent::StuckChange>,
        Vec<crate::recent::StuckChange>,
    )> {
        match &self.access {
            Access::ReadOnly(None) => Ok((Vec::new(), Vec::new())),
            Access::ReadOnly(Some(_)) => {
                let engine = self.engine.clone();
                self.local(move |journal| {
                    use cirrove_core::{mutation::MutationIntent, upload::UploadIntent};
                    let stuck = journal
                        .retained_stuck_mutations(limit)?
                        .into_iter()
                        .map(|record| {
                            let (what, name) = match record.request.intent {
                                MutationIntent::CreateFolder { name, .. } => {
                                    ("create folder", name)
                                }
                                MutationIntent::Relocate { name, .. } => ("move", name),
                                MutationIntent::RemoveFile { before } => {
                                    ("delete file", before.name)
                                }
                                MutationIntent::TrashNativeDocument { before } => {
                                    ("trash native document", before.name)
                                }
                                MutationIntent::RemoveFolder { before } => {
                                    ("delete folder", before.name)
                                }
                            };
                            crate::recent::StuckChange {
                                what: what.into(),
                                name,
                                path: None,
                                state: format!("{:?}", record.state).to_ascii_lowercase(),
                                instead: None,
                            }
                        })
                        .collect();
                    let failed = journal
                        .retained_failed_uploads(limit)?
                        .into_iter()
                        .map(|record| {
                            let (what, name) = match record.intent {
                                UploadIntent::Create { name, .. } => ("save new file", name),
                                UploadIntent::Replace { item, .. } => {
                                    let name = match journal.retained_name(&record.scope, &item)? {
                                        Some(name) => name,
                                        None => engine
                                            .cached_recovery_name(&record.scope, &item)
                                            .map_err(|_| JournalError::Corrupt)?
                                            .unwrap_or(item),
                                    };
                                    ("save", name)
                                }
                            };
                            Ok(crate::recent::StuckChange {
                                what: what.into(),
                                name,
                                path: None,
                                state: format!("{:?}", record.state).to_ascii_lowercase(),
                                instead: None,
                            })
                        })
                        .collect::<crate::journal::Result<Vec<_>>>()?;
                    Ok((stuck, failed))
                })
                .await
            }
            Access::Writer(_) => bail!("read-only retained inspection requires no writer"),
        }
    }
}

impl RecoveryControl {
    pub(crate) async fn native_trash_list(
        &self,
        scope: cirrove_core::Scope,
        after: Option<u64>,
        limit: u32,
    ) -> Result<crate::native_trash::NativeTrashListing> {
        match &self.access {
            Access::Writer(writer) => Ok(writer.native_trash_list(scope, after, limit).await?),
            Access::ReadOnly(None) => Ok(crate::native_trash::NativeTrashListing::default()),
            _ => {
                self.local(move |j| j.native_trash_list(&scope, after, limit))
                    .await
            }
        }
    }
    pub(crate) async fn native_replacement_list(
        &self,
        scope: cirrove_core::Scope,
        after: Option<u64>,
        limit: u32,
    ) -> Result<crate::journal::NativeReplacementListing> {
        match &self.access {
            Access::Writer(writer) => {
                Ok(writer.native_replacement_list(scope, after, limit).await?)
            }
            Access::ReadOnly(None) => Ok(crate::journal::NativeReplacementListing::default()),
            _ => {
                self.local(move |j| j.native_replacement_list(&scope, after, limit))
                    .await
            }
        }
    }
}

impl RecoveryControl {
    pub(crate) async fn native_abandon_record(
        &self,
        id: uuid::Uuid,
    ) -> Result<Option<cirrove_icloud::NativeReplacementAbandonRecord>> {
        match &self.access {
            Access::Writer(writer) => Ok(writer.native_abandon_record(id).await?),
            Access::ReadOnly(None) => Ok(None),
            _ => {
                self.local(move |journal| journal.native_stage_abandonment(id))
                    .await
            }
        }
    }
}

impl RecoveryControl {
    pub(crate) async fn native_import_list(
        &self,
        scope: cirrove_core::Scope,
        after: Option<u64>,
        limit: u32,
    ) -> Result<crate::journal::NativeImportListing> {
        match &self.access {
            Access::Writer(writer) => Ok(writer.native_import_list(scope, after, limit).await?),
            Access::ReadOnly(None) => Ok(crate::journal::NativeImportListing::default()),
            _ => {
                self.local(move |j| j.native_import_list(&scope, after, limit))
                    .await
            }
        }
    }
}
