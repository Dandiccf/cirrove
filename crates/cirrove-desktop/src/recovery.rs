//! Presentation rules for non-mutating recovery of sealed local saves.
pub mod active;
pub mod offline;

use cirrove_service::{
    jobs::{Job, JobKind, JobState},
    journal::WorkingRecovery,
    recent::LocalChange,
};
use std::path::Path;

#[derive(Clone)]
pub enum Selection {
    Saved(LocalChange),
    Working(WorkingRecovery),
}
impl Selection {
    pub fn name(&self) -> &str {
        match self {
            Self::Saved(s) => &s.name,
            Self::Working(s) => &s.name,
        }
    }
    pub fn size(&self) -> u64 {
        match self {
            Self::Saved(s) => s.size,
            Self::Working(s) => s.size,
        }
    }
}
/// A UI success belongs to the selected source and initial service job.
pub fn confirmed_selection(
    initial: &Job,
    job: &Job,
    selection: &Selection,
    destination: &Path,
) -> bool {
    if initial.id != job.id {
        return false;
    }
    match selection {
        Selection::Saved(save) => job.working_export.is_none() && confirmed(job, save, destination),
        Selection::Working(working) => {
            initial.bytes_total == working.size
                && cirrove_service::ExportWorkingRequest {
                    label: String::new(),
                    file: working.file,
                    generation: working.generation,
                    destination: destination.into(),
                }
                .confirmed_receipt(initial, job)
                .is_some()
        }
    }
}

pub fn eligible(save: &LocalChange) -> bool {
    save.operation.is_some()
        && matches!(
            save.state.as_str(),
            "pending"
                | "uploading"
                | "verify_required"
                | "verifyrequired"
                | "verifying"
                | "conflict"
                | "failed"
        )
}

/// A terminal job alone is insufficient: it must acknowledge this exact request.
pub fn confirmed(job: &Job, save: &LocalChange, destination: &Path) -> bool {
    job.kind == JobKind::ExportLocal
        && job.working_export.is_none()
        && job.state == JobState::Succeeded
        && job.export.as_ref().is_some_and(|receipt| {
            Some(receipt.operation) == save.operation
                && receipt.size == save.size
                && receipt.destination == destination
                && receipt.sha256.len() == 64
                && receipt.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        })
}
