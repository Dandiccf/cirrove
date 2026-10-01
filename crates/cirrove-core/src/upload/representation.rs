//! Explicit upload representation: archive bytes are not remote logical bytes.
use serde::{Deserialize, Serialize};
/// Version 1 commits to the canonical framing documented in the benchmark
/// contract. Unknown versions must be rejected, not silently reinterpreted.
pub const PACKAGE_SEMANTIC_IDENTITY_VERSION: u32 = 1;
/// A content identity, not upload authority or a remote revision receipt. Bind
/// this separately to the account, operation, expected root and source revision.
/// Contains no archive paths, provider identifiers or document contents.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "IdentityWire")]
pub struct PackageSemanticIdentity {
    pub version: u32,
    pub sha256: String,
    pub entries: u32,
    pub files: u32,
    pub expanded_bytes: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityWire {
    version: u32,
    sha256: String,
    entries: u32,
    files: u32,
    expanded_bytes: u64,
}
impl TryFrom<IdentityWire> for PackageSemanticIdentity {
    type Error = &'static str;
    fn try_from(value: IdentityWire) -> std::result::Result<Self, Self::Error> {
        let identity = Self {
            version: value.version,
            sha256: value.sha256,
            entries: value.entries,
            files: value.files,
            expanded_bytes: value.expanded_bytes,
        };
        identity
            .validate()
            .map_err(|_| "invalid package semantic identity")?;
        Ok(identity)
    }
}
impl PackageSemanticIdentity {
    pub fn validate(&self) -> super::Result<()> {
        if self.version != PACKAGE_SEMANTIC_IDENTITY_VERSION
            || self.sha256.len() != 64
            || !self
                .sha256
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            || self.entries == 0
            || self.entries > 10_000
            || self.files == 0
            || self.files > self.entries
            || self.expanded_bytes > 64 * 1024 * 1024
        {
            return Err(super::UploadError::Invalid);
        }
        Ok(())
    }
}

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UploadRepresentation {
    #[default]
    FileBytes,
    PackageArchive {
        expected_root: String,
        semantic: PackageSemanticIdentity,
    },
}
impl UploadRepresentation {
    pub fn is_file_bytes(&self) -> bool {
        matches!(self, Self::FileBytes)
    }
    pub fn validate(&self) -> super::Result<()> {
        if let Self::PackageArchive {
            expected_root,
            semantic,
        } = self
        {
            if expected_root.is_empty()
                || expected_root.len() > 1024
                || matches!(expected_root.as_str(), "." | "..")
                || expected_root
                    .chars()
                    .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
            {
                return Err(super::UploadError::Invalid);
            }
            semantic.validate()?;
        }
        Ok(())
    }
}
/// Independent verified remote content, never merely an add-package response.
/// Its remote node retains the provider's logical size; raw bytes remain on the request.
pub struct PackageUploadReceipt {
    pub remote: crate::Node,
    pub semantic: PackageSemanticIdentity,
}
