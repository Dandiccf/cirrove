use super::*;
use crate::{
    filesystem::WriteControl,
    jobs::{Job, JobKind, JobState},
    native_abandon::NativeAbandonRequest,
};
impl Engine {
    pub(crate) fn start_native_abandon_job(
        self: &Arc<Self>,
        manager: Arc<crate::manager::Manager>,
        control: WriteControl,
        input: NativeAbandonRequest,
    ) -> Result<Job> {
        let handle = self.jobs.start(
            JobKind::AbandonNativeStage,
            "Retain staged document and saved recovery".into(),
            1,
            0,
            &self.cancel,
        );
        handle.native_abandon_started(&input.expected_account_id, input.operation);
        let initial = self
            .jobs
            .find(handle.id())
            .context("abandonment job unavailable")?;
        let engine = self.clone();
        self.tasks.spawn(async move {
            match manager.abandon_native_stage(engine,control,input,handle.cancel.clone()).await {
                Ok(receipt)=>handle.native_abandoned(receipt),
                Err(_)=>{
                    let state=if handle.stopping(){JobState::Stopped}else{JobState::Failed};
                    handle.failed(state,Some("Local abandonment not confirmed. Inspect native-stage-abandonment before resubmitting. Saved bytes, checkpoints and cloud items remain retained.".into()));
                }
            }
        });
        Ok(initial)
    }
}
