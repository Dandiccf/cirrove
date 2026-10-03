//! Explicit native archive admission. No provider calls or mounted write inference.
mod staging;
pub use staging::{ImportAdmissionError, ValidatedPackageArchive};

/// Explicit import input; caller-selected archive names are not package authority.
pub struct NativeImportInput {
    pub source: std::path::PathBuf,
    pub expected_root: String,
    pub parent: String,
    pub name: String,
}
pub(crate) struct ImportParent {
    pub scope: cirrove_core::Scope,
    pub route: Vec<cirrove_core::Node>,
    pub parent: cirrove_core::Node,
    pub children: Vec<cirrove_core::Node>,
    pub frontier: u64,
}

#[cfg(test)]
pub(crate) use staging::tests::archive as synthetic_package_archive;

/// Explicit native replacement input; it does not authorize a mounted save.
pub struct NativeReplaceInput {
    pub selected: crate::native_trash::NativeTrashInput,
    pub source: std::path::PathBuf,
    pub expected_root: String,
}

/// Static admission context only; never includes a provider/parser error or path.
/// Even a failed return can follow durable enqueue: callers must inspect retained work.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ReplacementAdmissionError {
    #[error("selected-document resolution")]
    Selection,
    #[error("local archive capture")]
    Archive,
    #[error("original remote-content verification")]
    Original,
    #[error("final selection recheck")]
    Recheck,
    #[error("durable enqueue")]
    Enqueue,
}
impl ReplacementAdmissionError {
    pub(crate) fn message(error: &anyhow::Error) -> String {
        if let Some(phase) = error.downcast_ref::<Self>() {
            format!(
                "Replacement admission was not confirmed during {phase}; inspect retained operations before retrying."
            )
        } else {
            "Replacement admission was not confirmed; inspect retained operations before retrying."
                .into()
        }
    }
}
