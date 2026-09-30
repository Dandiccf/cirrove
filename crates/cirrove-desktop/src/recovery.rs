//! Presentation rules for non-mutating recovery of sealed local saves.
use cirrove_service::{
    jobs::{Job, JobKind, JobState},
    recent::LocalChange,
};
use std::path::Path;

pub fn eligible(save: &LocalChange) -> bool {
    save.operation.is_some()
        && matches!(
            save.state.as_str(),
            "pending" | "uploading" | "verify_required" | "verifying" | "conflict" | "failed"
        )
}

/// A terminal job alone is insufficient: it must acknowledge this exact request.
pub fn confirmed(job: &Job, save: &LocalChange, destination: &Path) -> bool {
    job.kind == JobKind::ExportLocal
        && job.state == JobState::Succeeded
        && job.export.as_ref().is_some_and(|receipt| {
            Some(receipt.operation) == save.operation
                && receipt.size == save.size
                && receipt.destination == destination
                && receipt.sha256.len() == 64
                && receipt.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        })
}
