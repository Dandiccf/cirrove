//! Local-only recovery access. This never constructs write workers or a provider.
use crate::{engine::Engine, filesystem::WriteControl, journal::*};
use anyhow::{Context, Result, bail};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[derive(Clone)]
pub(crate) struct RecoveryControl {
    // Blocking copies must retain the account lease as well as the journal lease.
    engine: Arc<Engine>,
    access: Access,
}
#[derive(Clone)]
enum Access {
    Writer(WriteControl),
    ReadOnly(Option<Arc<Mutex<RecoveryJournal>>>),
}
impl RecoveryControl {
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
        let mut cached = engine.recovery_journal.lock().await;
        let journal = if let Some(journal) = cached.upgrade() {
            Some(journal)
        } else {
            let owner = engine.clone();
            let journal = tokio::task::spawn_blocking(move || -> Result<_> {
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
                Ok(Some(Arc::new(Mutex::new(journal))))
            })
            .await??;
            if let Some(journal) = &journal {
                *cached = Arc::downgrade(journal);
            }
            journal
        };
        drop(cached);
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
                            let (name, item) = match record.intent {
                                cirrove_core::upload::UploadIntent::Create { name, .. } => {
                                    (name, None)
                                }
                                cirrove_core::upload::UploadIntent::Replace { item, .. } => {
                                    let name = match journal.retained_name(&record.scope, &item)? {
                                        Some(name) => name,
                                        None => engine
                                            .cached_recovery_name(&record.scope, &item)
                                            .map_err(|_| JournalError::Corrupt)?
                                            .unwrap_or_else(|| item.clone()),
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
}
