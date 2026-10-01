//! Observation does not own, retry or discard durable replacement work.
use super::native_import::{fresh_publication, observe, same_publication};
use super::*;
use crate::{
    filesystem::WriteControl,
    jobs::{Job, JobHandle, JobKind, JobState},
    journal::{PackagePublicationStatus, UploadRecord, UploadState},
};
use cirrove_core::{Node, upload::UploadRepresentation};
fn original(row: &UploadRecord) -> Result<&Node> {
    match &row.representation {
        UploadRepresentation::PackageReplacementArchive { original, .. } => Ok(original),
        _ => anyhow::bail!("not a native replacement"),
    }
}
fn confirmed(queued: &UploadRecord, row: &UploadRecord) -> bool {
    row.id == queued.id
        && row.scope == queued.scope
        && row.intent == queued.intent
        && row.representation == queued.representation
        && row.size == queued.size
        && row.sha256 == queued.sha256
        && row.native_replacement_receipt().is_some()
}

impl Engine {
    pub(crate) fn start_native_replacement_job(
        self: &Arc<Self>,
        manager: Arc<crate::manager::Manager>,
        control: WriteControl,
        input: crate::native_import::NativeReplaceInput,
    ) -> Result<Job> {
        let handle = self.jobs.start(
            JobKind::ReplaceNativePackage,
            input.selected.path.clone(),
            1,
            0,
            &self.cancel,
        );
        let initial = self
            .jobs
            .find(handle.id())
            .context("replacement job unavailable")?;
        let engine = self.clone();
        self.tasks.spawn(async move {
            match manager.enqueue_native_replacement(engine.clone(), input, handle.cancel.clone()).await {
                Ok(row) => engine.observe_replacement(manager, control, handle, row, false).await,
                Err(_) => {
                    let state = if handle.stopping() { JobState::Stopped } else { JobState::Failed };
                    handle.failed(state, Some("Replacement admission was not confirmed; inspect retained operations before retrying.".into()));
                }
            }
        });
        Ok(initial)
    }
    pub(crate) fn start_native_replacement_watch(
        self: &Arc<Self>,
        manager: Arc<crate::manager::Manager>,
        control: WriteControl,
        row: UploadRecord,
    ) -> Result<Job> {
        let before = original(&row)?.clone();
        let handle = self.jobs.start(
            JobKind::ReplaceNativePackage,
            before.name.clone(),
            1,
            row.size,
            &self.cancel,
        );
        handle.native_replace_queued(row.id, before, row.size);
        let initial = self
            .jobs
            .find(handle.id())
            .context("replacement watch unavailable")?;
        let engine = self.clone();
        self.tasks.spawn(async move {
            engine
                .observe_replacement(manager, control, handle, row, true)
                .await;
        });
        Ok(initial)
    }
    async fn observe_replacement(
        self: Arc<Self>,
        manager: Arc<crate::manager::Manager>,
        control: WriteControl,
        handle: JobHandle,
        queued: UploadRecord,
        fresh: bool,
    ) {
        let Ok(before) = original(&queued).cloned() else {
            handle.failed(JobState::Failed, Some("Invalid saved replacement.".into()));
            return;
        };
        handle.native_replace_queued(queued.id, before.clone(), queued.size);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20 * 60);
        loop {
            let result = observe(&handle.cancel, deadline, async {
                let (row, publication) = manager
                    .native_replacement_observation(&self, &control, queued.id)
                    .await?;
                if confirmed(&queued, &row) {
                    let visible = match publication {
                        PackagePublicationStatus::Pending => return Ok(None),
                        PackagePublicationStatus::Present(node) => node,
                        _ => anyhow::bail!("replacement visibility absent"),
                    };
                    let receipt = row.remote.as_ref().context("replacement receipt missing")?;
                    if !same_publication(receipt, &visible) {
                        anyhow::bail!("replacement publication changed");
                    }
                    if fresh {
                        match fresh_publication(&self, &row).await? {
                            PackagePublicationStatus::Present(node) if same_publication(receipt, &node) => {},
                            _ => anyhow::bail!("replacement current visibility changed"),
                        }
                    }
                    let (latest, published) = manager
                        .native_replacement_observation(&self, &control, queued.id)
                        .await?;
                    if !confirmed(&queued, &latest)
                        || latest.remote != row.remote
                        || latest.native_replacement_receipt() != row.native_replacement_receipt()
                        || !matches!(published, PackagePublicationStatus::Present(ref node) if same_publication(receipt, node))
                    {
                        anyhow::bail!("replacement receipt changed during observation");
                    }
                    return Ok(Some((
                        receipt.clone(),
                        row.native_replacement_receipt()
                            .map(|(_, _, backup)| backup.clone())
                            .context("replacement recovery missing")?,
                    )));
                }
                if matches!(row.state, UploadState::Uploaded | UploadState::Failed | UploadState::Conflict | UploadState::VerifyRequired | UploadState::Resolved) {
                    anyhow::bail!("replacement requires retained-operation inspection");
                }
                Ok::<Option<(Node, Node)>, anyhow::Error>(None)
            }).await;
            match result {
                Ok(Ok(Some((current, recovery)))) => {
                    handle.advance(1, queued.size);
                    handle.native_replaced(queued.id, before, current, recovery);
                    return;
                }
                Ok(Ok(None)) => {}
                Ok(Err(_)) => {
                    handle.failed(JobState::Failed,Some("Replacement not confirmed; its saved operation remains retained. Do not submit it again.".into()));
                    return;
                }
                Err(state) => {
                    handle.failed(state,Some("Stopped watching; the saved replacement remains retained and may finish.".into()));
                    return;
                }
            }
            tokio::select! {
                biased;
                _ = handle.cancel.cancelled() => {
                    handle.failed(JobState::Stopped, Some("Stopped watching; the saved replacement remains retained.".into()));
                    return;
                }
                _ = tokio::time::sleep_until(deadline) => {
                    handle.failed(JobState::Failed, Some("Replacement observation timed out; saved operation retained.".into()));
                    return;
                }
                _ = tokio::time::sleep(Duration::from_millis(100)) => {},
            }
        }
    }
}
