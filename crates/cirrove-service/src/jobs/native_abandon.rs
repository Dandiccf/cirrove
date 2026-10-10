use super::*;
impl JobHandle {
    pub(crate) fn native_abandon_started(&self, account: &str, operation: uuid::Uuid) {
        let mut inner = self.jobs.inner();
        if let Some(entry) = inner.running.iter_mut().find(|e| e.job.id == self.id) {
            if entry
                .job
                .native_abandon
                .as_ref()
                .is_some_and(|p| p.account_id != account || p.operation != operation)
            {
                return;
            }
            entry.job.native_abandon = Some(NativeAbandonProgress {
                account_id: account.into(),
                operation,
                receipt: None,
            });
        }
    }
    pub(crate) fn native_abandoned(self, receipt: crate::native_abandon::NativeAbandonReceipt) {
        {
            let mut inner = self.jobs.inner();
            let Some(index) = inner.running.iter().position(|e| e.job.id == self.id) else {
                return;
            };
            let mut job = inner.running.remove(index).job;
            if let Some(p) = job.native_abandon.as_mut()
                && p.operation == receipt.operation
                && p.account_id == receipt.account_id
            {
                p.receipt = Some(receipt);
                job.state = JobState::Succeeded;
                job.files_done = 1;
                job.issue = None;
                // The local commit has already happened. A concurrent stop must
                // not suppress its durable receipt or imply it was rolled back.
            } else {
                job.state = JobState::Failed;
                job.issue = Some(
                    "Abandonment receipt binding changed; inspect the recorded local outcome."
                        .into(),
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
    fn receipt(
        account: &str,
        operation: uuid::Uuid,
    ) -> crate::native_abandon::NativeAbandonReceipt {
        let original = cirrove_core::Node {
            id: "FILE::com.apple.CloudDocs::original".into(),
            parent_id: Some("FOLDER::com.apple.CloudDocs::owned".into()),
            name: "Owned.pages".into(),
            kind: cirrove_core::NodeKind::Folder,
            size: 3,
            modified_unix: 0,
            etag: Some("v1".into()),
            content_version: None,
            target: None,
            package: true,
        };
        let mut stage = original.clone();
        stage.id = "FILE::com.apple.CloudDocs::stage".into();
        stage.name = format!("staged-by-cirrove-{operation}.pages");
        crate::native_abandon::NativeAbandonReceipt {
            account_id: account.into(),
            operation,
            original,
            retained_stage: stage,
            observed_unix: 1,
        }
    }
    #[test]
    fn native_abandon_job_never_hides_committed_receipt_after_late_stop_or_rebinds_identity() {
        for arm in 0..4 {
            let jobs = Arc::new(Jobs::default());
            let operation = uuid::Uuid::new_v4();
            let handle = jobs.start(
                JobKind::AbandonNativeStage,
                "Owned.pages".into(),
                1,
                0,
                &CancellationToken::new(),
            );
            let id = handle.id().to_owned();
            handle.native_abandon_started("account", operation);
            handle.native_abandon_started("other", uuid::Uuid::new_v4());
            if arm == 1 {
                assert_eq!(jobs.stop(&id), Stopped::Asked);
            }
            handle.native_abandoned(receipt(
                if arm == 2 { "other" } else { "account" },
                if arm == 3 {
                    uuid::Uuid::new_v4()
                } else {
                    operation
                },
            ));
            let job = jobs.find(&id).expect("retained job");
            let progress = job.native_abandon.expect("bound operation");
            assert_eq!(progress.operation, operation);
            assert_eq!(progress.account_id, "account");
            assert_eq!(
                job.state,
                if arm < 2 {
                    JobState::Succeeded
                } else {
                    JobState::Failed
                }
            );
            assert_eq!(progress.receipt.is_some(), arm < 2);
        }
    }
}
