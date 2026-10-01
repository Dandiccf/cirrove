//! Bounded admission/observation jobs; stopping an observer never cancels a queued mutation.
use super::*;
use crate::{
    jobs::{Job, JobKind, JobState},
    journal::MutationState,
    native_trash::NativeTrashInput,
};
impl Engine {
    pub(crate) fn start_native_trash(
        self: &Arc<Self>,
        manager: Arc<crate::manager::Manager>,
        input: NativeTrashInput,
    ) -> Result<Job> {
        anyhow::ensure!(
            self.account.id == input.expected_account_id,
            "native Trash account changed"
        );
        let account = input.expected_account_id.clone();
        let handle = self.jobs.start(
            JobKind::TrashNativeDocument,
            input.path.clone(),
            1,
            0,
            &self.cancel,
        );
        let initial = self
            .jobs
            .find(handle.id())
            .context("native Trash job unavailable")?;
        let engine = self.clone();
        self.tasks.spawn(async move {
            // Do not cancel/drop this future around its durable commit. Admission
            // owns the cancellation fence and returns an ID even after a late stop.
            let operation=match manager.enqueue_native_trash(engine.clone(),input,handle.cancel.clone()).await {
                Ok(operation)=>operation,
                Err(_)=>{let state=if handle.stopping(){JobState::Stopped}else{JobState::Failed};handle.failed(state,Some("native Trash admission was not confirmed; inspect retained operations before retrying".into()));return;}
            };
            handle.native_trash_queued(&account,operation);
            engine.observe_native_trash(manager,handle,account,operation).await;
        });
        Ok(initial)
    }
    pub(crate) fn start_native_trash_watch(
        self: &Arc<Self>,
        manager: Arc<crate::manager::Manager>,
        account: String,
        operation: uuid::Uuid,
    ) -> Result<Job> {
        anyhow::ensure!(self.account.id == account, "native Trash account changed");
        let handle = self.jobs.start(
            JobKind::TrashNativeDocument,
            "Native document Trash".into(),
            1,
            0,
            &self.cancel,
        );
        let initial = self
            .jobs
            .find(handle.id())
            .context("native Trash observer unavailable")?;
        let engine = self.clone();
        // No progress receipt before the manager validates the retained row.
        self.tasks.spawn(async move {
            engine
                .observe_native_trash(manager, handle, account, operation)
                .await;
        });
        Ok(initial)
    }
    async fn observe_native_trash(
        self: Arc<Self>,
        manager: Arc<crate::manager::Manager>,
        handle: crate::jobs::JobHandle,
        account: String,
        operation: uuid::Uuid,
    ) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20 * 60);
        loop {
            let observed = tokio::select! {biased;
                _=handle.cancel.cancelled()=>{handle.failed(JobState::Stopped,Some("stopped watching; any queued native Trash operation remains retained and may complete".into()));return;},
                _=tokio::time::sleep_until(deadline)=>{handle.failed(JobState::Failed,Some("native Trash observation timed out; inspect the retained operation, do not submit another removal".into()));return;},
                result=manager.native_trash_status(&self,&account,operation)=>result,
            };
            match observed {
                Ok(status) if status.operation == operation => {
                    handle.native_trash_queued(&account, operation);
                    if status.state == MutationState::Applied
                        && status.removal_confirmed
                        && status.metadata_removed
                    {
                        handle.native_trashed(&account, operation);
                        return;
                    }
                    if status.state == MutationState::Applied && !status.removal_confirmed {
                        handle.failed(JobState::Failed,Some("native Trash has no matching removal receipt; inspect its retained operation".into()));
                        return;
                    }
                    if matches!(
                        status.state,
                        MutationState::Conflict
                            | MutationState::Failed
                            | MutationState::NeedsReview
                            | MutationState::Resolved
                            | MutationState::VerifyRequired
                    ) {
                        handle.failed(JobState::Failed,Some("native Trash needs review; its saved operation is retained and will not be resubmitted by this observer".into()));
                        return;
                    }
                }
                _ => {
                    handle.failed(JobState::Failed,Some("native Trash account or saved operation is unavailable; no removal was resubmitted".into()));
                    return;
                }
            }
            tokio::select! {biased;
                _=handle.cancel.cancelled()=>{handle.failed(JobState::Stopped,Some("stopped watching; the queued native Trash operation remains retained".into()));return;},
                _=tokio::time::sleep_until(deadline)=>{handle.failed(JobState::Failed,Some("native Trash observation timed out; the operation remains retained".into()));return;},
                _=tokio::time::sleep(Duration::from_millis(500))=>{},
            }
        }
    }
}
