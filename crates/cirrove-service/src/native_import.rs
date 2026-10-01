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
