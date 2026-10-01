//! Atomically bind native Trash completion to its durable operation and accepted stop.
use super::*;
impl JobHandle {
    pub(crate) fn native_trash_queued(&self, account_id: &str, operation: uuid::Uuid) {
        let mut inner = self.jobs.inner();
        if let Some(entry) = inner.running.iter_mut().find(|r| r.job.id == self.id) {
            if entry
                .job
                .native_trash
                .as_ref()
                .is_some_and(|p| p.operation != operation || p.account_id != account_id)
            {
                return;
            }
            entry.job.native_trash = Some(NativeTrashProgress {
                account_id: account_id.into(),
                operation,
                removal_receipt_recorded: false,
                metadata_absence_recorded: false,
            });
        }
    }
    pub(crate) fn native_trashed(self, account_id: &str, operation: uuid::Uuid) {
        {
            let mut inner = self.jobs.inner();
            let Some(index) = inner
                .running
                .iter()
                .position(|entry| entry.job.id == self.id)
            else {
                return;
            };
            let entry = inner.running.remove(index);
            let mut job = entry.job;
            if entry.cancel.is_cancelled() || job.state == JobState::Stopping {
                job.state = JobState::Stopped;
                job.issue = Some(
                    "stopped watching; the queued native Trash operation remains retained".into(),
                );
            } else if let Some(progress) = job.native_trash.as_mut()
                && progress.operation == operation
                && progress.account_id == account_id
            {
                progress.removal_receipt_recorded = true;
                progress.metadata_absence_recorded = true;
                job.files_done = 1;
                job.state = JobState::Succeeded;
                job.issue = None;
            } else {
                job.state = JobState::Failed;
                job.issue = Some(
                    "native Trash receipt did not match the queued account and operation".into(),
                );
            }
            if inner.ended.len() == RETAINED {
                inner.ended.pop_front();
            }
            inner.ended.push_back((Instant::now(), job));
        }
        self.jobs.ended.notify_waiters();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_trash_progress_names_recorded_evidence_not_current_cloud_state() {
        let value = serde_json::to_value(NativeTrashProgress {
            account_id: "account".into(),
            operation: uuid::Uuid::new_v4(),
            removal_receipt_recorded: true,
            metadata_absence_recorded: true,
        })
        .expect("progress");
        assert_eq!(value["removal_receipt_recorded"], true);
        assert_eq!(value["metadata_absence_recorded"], true);
        assert!(value.get("removal_confirmed").is_none());
        assert!(value.get("metadata_removed").is_none());
    }
    #[test]
    fn native_trash_job_retains_queued_identity_on_failure_and_refuses_rebinding() {
        let jobs = Arc::new(Jobs::default());
        let handle = jobs.start(
            JobKind::TrashNativeDocument,
            "Own.pages".into(),
            1,
            0,
            &CancellationToken::new(),
        );
        let id = handle.id().to_owned();
        let op = uuid::Uuid::new_v4();
        handle.native_trash_queued("account", op);
        handle.native_trash_queued("other", uuid::Uuid::new_v4());
        handle.failed(
            JobState::Failed,
            Some("synthetic observation failure".into()),
        );
        let job = jobs.find(&id).expect("retained");
        let progress = job.native_trash.expect("durable operation");
        assert_eq!(job.state, JobState::Failed);
        assert_eq!(progress.operation, op);
        assert_eq!(progress.account_id, "account");
        assert!(!progress.removal_receipt_recorded && !progress.metadata_absence_recorded);
    }
    #[test]
    fn native_trash_job_requires_matched_account_operation_and_honors_accepted_stop() {
        for arm in 0..4 {
            let jobs = Arc::new(Jobs::default());
            let handle = jobs.start(
                JobKind::TrashNativeDocument,
                "Own.pages".into(),
                1,
                0,
                &CancellationToken::new(),
            );
            let id = handle.id().to_owned();
            let op = uuid::Uuid::new_v4();
            handle.native_trash_queued("account", op);
            if arm == 1 {
                assert_eq!(jobs.stop(&id), Stopped::Asked);
            }
            handle.native_trashed(
                if arm == 2 { "other" } else { "account" },
                if arm == 3 { uuid::Uuid::new_v4() } else { op },
            );
            let job = jobs.find(&id).expect("retained");
            let p = job.native_trash.expect("retained operation");
            assert_eq!(p.operation, op);
            assert_eq!(
                job.state,
                match arm {
                    0 => JobState::Succeeded,
                    1 => JobState::Stopped,
                    _ => JobState::Failed,
                }
            );
            assert_eq!(
                (p.removal_receipt_recorded, p.metadata_absence_recorded),
                (arm == 0, arm == 0)
            );
        }
    }
}
