//! User-requested import observation; durable upload ownership stays in the journal.
use super::*;
use crate::{
    jobs::{Job, JobKind, JobState},
    journal::{UploadRecord, UploadState},
};

impl Engine {
    pub(crate) fn start_native_import(
        self: &Arc<Self>,
        manager: Arc<crate::manager::Manager>,
        input: crate::native_import::NativeImportInput,
    ) -> Result<Job> {
        let handle = self.jobs.start(
            JobKind::ImportNativePackage,
            input.name.clone(),
            1,
            0,
            &self.cancel,
        );
        let initial = self
            .jobs
            .find(handle.id())
            .context("native import job unavailable")?;
        let engine = self.clone();
        self.tasks.spawn(async move {
            let queued = match manager.enqueue_native_package(engine.clone(), input, handle.cancel.clone()).await {
                Ok(row) => row,
                Err(_) => {
                    let state = if handle.stopping() { JobState::Stopped } else { JobState::Failed };
                    handle.failed(state, Some("native import admission was not confirmed; inspect retained operations before retrying".into()));
                    return;
                }
            };
            handle.native_import_queued(queued.id, queued.size);
            let deadline = tokio::time::Instant::now() + Duration::from_secs(20 * 60);
            loop {
                let observed = match observe(&handle.cancel, deadline, manager.native_import_record(&engine, queued.id)).await {
                    Ok(result) => result,
                    Err(state) => { stop_observing(handle, state); return; }
                };
                match observed {
                    Ok(row) if confirmed_import(&queued, &row) => {
                        let publication = match observe(&handle.cancel, deadline, manager.native_import_publication(&engine, queued.id)).await {
                            Ok(result) => result,
                            Err(state) => { stop_observing(handle, state); return; }
                        };
                        match publication {
                            Ok(crate::journal::PackagePublicationStatus::Pending) => {}
                            Ok(crate::journal::PackagePublicationStatus::Present(remote))
                                if row.remote.as_ref().is_some_and(|receipt| same_publication(receipt, &remote)) => {
                                handle.advance(1, queued.size);
                                handle.native_imported(queued.id, remote);
                                return;
                            }
                            Ok(_) => {
                                handle.failed(JobState::Failed, Some("iCloud verified the upload, but the document is no longer present at the expected name and revision; it will not be uploaded again".into()));
                                return;
                            }
                            Err(_) => {
                                handle.failed(JobState::Failed, Some("iCloud verified the upload; local visibility is not confirmed. The saved operation is retained; do not create another copy to retry".into()));
                                return;
                            }
                        }
                    }
                    Ok(row) if matches!(row.state, UploadState::Failed | UploadState::Conflict | UploadState::VerifyRequired | UploadState::Resolved) => {
                        handle.failed(JobState::Failed, Some("native import is not confirmed; its saved operation is retained. Inspect it before retrying".into()));
                        return;
                    }
                    Err(_) => {
                        handle.failed(JobState::Failed, Some("native import observation ended; the queued operation is retained. Do not submit another copy to retry it".into()));
                        return;
                    }
                    _ => {}
                }
                tokio::select! {
                    biased;
                    _ = handle.cancel.cancelled() => {
                        handle.failed(JobState::Stopped, Some("stopped watching; the queued import remains retained and may finish uploading".into()));
                        return;
                    }
                    _ = tokio::time::sleep_until(deadline) => {
                        handle.failed(JobState::Failed, Some("native import has not been confirmed in time; its queued operation remains retained".into()));
                        return;
                    }
                    _ = tokio::time::sleep(Duration::from_millis(500)) => {}
                }
            }
        });
        Ok(initial)
    }
}
async fn observe<T>(
    cancel: &cirrove_core::CancellationToken,
    deadline: tokio::time::Instant,
    read: impl std::future::Future<Output = T>,
) -> std::result::Result<T, JobState> {
    tokio::select! { biased;
        _ = cancel.cancelled() => Err(JobState::Stopped),
        _ = tokio::time::sleep_until(deadline) => Err(JobState::Failed),
        result = read => Ok(result),
    }
}
fn stop_observing(handle: crate::jobs::JobHandle, state: JobState) {
    handle.failed(state, Some("stopped watching or observation deadline reached; the queued import remains retained and may finish uploading".into()));
}

fn same_publication(receipt: &cirrove_core::Node, observed: &cirrove_core::Node) -> bool {
    receipt.id == observed.id
        && receipt.parent_id == observed.parent_id
        && receipt.name == observed.name
        && receipt.size == observed.size
        && receipt.etag.as_ref().is_some_and(|etag| !etag.is_empty())
        && receipt.etag == observed.etag
        // Older iCloud PACKAGE-create receipts copied ETag into content_version.
        // Recognize only that exact legacy alias; never collapse the two tag
        // namespaces globally or accept a different content tag. The current
        // read provider's source-folder observation remains canonical.
        && receipt.content_version.as_ref().is_none_or(|tag| Some(tag) == receipt.etag.as_ref())
        && observed.content_version.is_none()
        && receipt.kind == cirrove_core::NodeKind::Folder
        && receipt.package
        && receipt.target.is_none()
        && observed.kind == cirrove_core::NodeKind::Folder
        && observed.package
        && observed.target.is_none()
}

fn confirmed_import(queued: &UploadRecord, row: &UploadRecord) -> bool {
    let cirrove_core::upload::UploadRepresentation::PackageArchive { semantic, .. } =
        &queued.representation
    else {
        return false;
    };
    let cirrove_core::upload::UploadIntent::Create { parent, name } = &queued.intent else {
        return false;
    };
    row.id == queued.id
        && row.scope == queued.scope
        && row.intent == queued.intent
        && row.representation == queued.representation
        && row.size == queued.size
        && row.sha256 == queued.sha256
        && row.state == UploadState::Uploaded
        && row.package_completion.as_ref() == Some(semantic)
        && row.remote.as_ref().is_some_and(|remote| {
            !remote.id.is_empty()
                && remote.parent_id.as_ref() == Some(parent)
                && &remote.name == name
                && remote.kind == cirrove_core::NodeKind::Folder
                && remote.package
                && remote.target.is_none()
                && remote.content_revision().is_some()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cirrove_core::{
        Node, NodeKind, Scope,
        upload::{PackageSemanticIdentity, UploadIntent},
    };

    fn package_source() -> Node {
        Node {
            id: "FILE::com.apple.CloudDocs::import".into(),
            parent_id: Some("FOLDER::com.apple.CloudDocs::parent".into()),
            name: "Imported.pages".into(),
            kind: NodeKind::Folder,
            size: 98835,
            modified_unix: 0,
            etag: Some("native-v1".into()),
            content_version: None,
            target: None,
            package: true,
        }
    }
    #[test]
    fn native_publication_accepts_canonical_and_retained_etag_alias_receipts() {
        let observed = package_source();
        for legacy in [false, true] {
            let mut receipt = observed.clone();
            if legacy {
                receipt.content_version = receipt.etag.clone();
            }
            let original = receipt.clone();
            assert!(same_publication(&receipt, &observed), "legacy={legacy}");
            assert_eq!(
                receipt, original,
                "comparison must not rewrite retained receipts"
            );
        }
    }
    #[test]
    fn native_publication_keeps_identity_size_revision_and_package_shape_fenced() {
        for legacy in [false, true] {
            let observed = package_source();
            let mut receipt = observed.clone();
            if legacy {
                receipt.content_version = receipt.etag.clone();
            }
            for arm in [
                "id",
                "parent",
                "name",
                "size",
                "etag",
                "missing-etag",
                "empty-etag",
                "foreign-content",
                "aliased-observation",
                "kind",
                "package",
            ] {
                let mut changed = observed.clone();
                match arm {
                    "id" => changed.id.push_str("-other"),
                    "parent" => changed.parent_id = Some("moved".into()),
                    "name" => changed.name = "Other.pages".into(),
                    "size" => changed.size += 1,
                    "etag" => changed.etag = Some("native-v2".into()),
                    "missing-etag" => changed.etag = None,
                    "empty-etag" => changed.etag = Some(String::new()),
                    "foreign-content" => changed.content_version = Some("different-tag".into()),
                    "aliased-observation" => changed.content_version = changed.etag.clone(),
                    "kind" => changed.kind = NodeKind::File,
                    _ => changed.package = false,
                }
                assert!(
                    !same_publication(&receipt, &changed),
                    "{arm}, legacy={legacy}"
                );
            }
            receipt.content_version = Some("different-tag".into());
            assert!(!same_publication(&receipt, &observed));
        }
        let mut no_revision = package_source();
        no_revision.etag = None;
        assert!(!same_publication(&no_revision, &no_revision));
    }
    #[tokio::test]
    async fn blocked_observation_can_be_stopped_or_timed_out() {
        for stopped in [true, false] {
            let cancel = cirrove_core::CancellationToken::new();
            if stopped {
                cancel.cancel();
            }
            let deadline = tokio::time::Instant::now()
                + if stopped {
                    Duration::from_secs(60)
                } else {
                    Duration::ZERO
                };
            let result = tokio::time::timeout(
                Duration::from_secs(1),
                observe(&cancel, deadline, std::future::pending::<()>()),
            )
            .await
            .expect("blocked journal observation must remain interruptible");
            assert_eq!(
                result,
                Err(if stopped {
                    JobState::Stopped
                } else {
                    JobState::Failed
                })
            );
        }
    }
    #[test]
    fn import_success_requires_exact_queued_identity_and_verified_package_receipt() {
        let dir = tempfile::tempdir().expect("fixture");
        let mut journal =
            crate::journal::UploadJournal::open(&dir.path().join("journal"), "owned", 1024)
                .expect("journal");
        let semantic = PackageSemanticIdentity {
            version: 1,
            sha256: "a".repeat(64),
            entries: 1,
            files: 1,
            expanded_bytes: 8,
        };
        let queued = journal
            .enqueue_package_archive(
                Scope {
                    account: "owned".into(),
                    provider: "icloud".into(),
                    collection: "drive".into(),
                },
                UploadIntent::Create {
                    parent: "root".into(),
                    name: "Copy.pages".into(),
                },
                "Original.pages".into(),
                semantic.clone(),
                b"archive".as_slice(),
            )
            .expect("queued");
        assert!(!confirmed_import(&queued, &queued));
        let mut confirmed = queued.clone();
        confirmed.state = UploadState::Uploaded;
        confirmed.package_completion = Some(semantic);
        confirmed.remote = Some(Node {
            id: "exact-package".into(),
            parent_id: Some("root".into()),
            name: "Copy.pages".into(),
            kind: NodeKind::Folder,
            size: 8,
            modified_unix: 0,
            etag: Some("v1".into()),
            content_version: None,
            target: None,
            package: true,
        });
        assert!(confirmed_import(&queued, &confirmed));
        let visible = confirmed.remote.as_ref().expect("remote");
        assert!(same_publication(visible, visible));
        let mut later = visible.clone();
        later.etag = Some("v2".into());
        assert!(!same_publication(visible, &later));
        later = visible.clone();
        later.parent_id = Some("moved".into());
        assert!(!same_publication(visible, &later));
        for variant in 0..7 {
            let mut wrong = confirmed.clone();
            match variant {
                0 => wrong.id = uuid::Uuid::new_v4(),
                1 => wrong.scope.account = "other".into(),
                2 => wrong.package_completion = None,
                3 => wrong.sha256 = "b".repeat(64),
                4 => wrong.remote.as_mut().expect("remote").package = false,
                5 => wrong.remote.as_mut().expect("remote").parent_id = Some("other".into()),
                _ => wrong.state = UploadState::VerifyRequired,
            }
            assert!(!confirmed_import(&queued, &wrong), "variant {variant}");
        }
    }
}
