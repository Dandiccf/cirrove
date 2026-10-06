//! Explicit upload representation: archive bytes are not remote logical bytes.
use serde::{Deserialize, Serialize};
/// The default remains strict version 1. Version 2 adds canonical directory closure.
/// Version 1 commits to the canonical framing documented in the benchmark
/// contract. Unknown versions must be rejected, not silently reinterpreted.
pub const PACKAGE_SEMANTIC_IDENTITY_VERSION: u32 = 1;
/// Caller-selected local archive layout, independent of the provider's remote
/// DATA/PACKAGE representation. Flat Numbers never supplies a wrapper root.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageSourceLayout {
    #[default]
    Wrapped,
    FlatNumbers,
}
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
        if !matches!(self.version, 1 | 2)
            || self.sha256.len() != 64
            || !self
                .sha256
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            || self.entries == 0
            || self.entries > 10_000
            || self.files == 0
            || self.files > self.entries
            || (self.version == 2 && self.files == self.entries)
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
    /// Explicit local Numbers export without an outer directory. The remote
    /// representation is still verified separately as a native package.
    FlatNumbersArchive { semantic: PackageSemanticIdentity },
    /// Explicit two-ID archive replacement. Original content/revision is retained
    /// separately from the sealed replacement bytes; neither is a path alias.
    PackageReplacementArchive {
        expected_root: String,
        semantic: PackageSemanticIdentity,
        original: Box<crate::Node>,
        original_semantic: PackageSemanticIdentity,
    },
    FlatNumbersReplacementArchive {
        semantic: PackageSemanticIdentity,
        original: Box<crate::Node>,
        original_semantic: PackageSemanticIdentity,
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
        }
        | Self::PackageReplacementArchive {
            expected_root,
            semantic,
            ..
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
        if let Self::FlatNumbersArchive { semantic }
        | Self::FlatNumbersReplacementArchive { semantic, .. } = self
        {
            semantic.validate()?;
            if semantic.version != 2 {
                return Err(super::UploadError::Invalid);
            }
        }
        if let Self::PackageReplacementArchive {
            original,
            original_semantic,
            ..
        }
        | Self::FlatNumbersReplacementArchive {
            original,
            original_semantic,
            ..
        } = self
        {
            original_semantic.validate()?;
            if original.kind != crate::NodeKind::Folder
                || !original.package
                || original.target.is_some()
                || original.id.is_empty()
                || original.id.len() > 4096
                || original.id.contains('\0')
                || original.etag.as_ref().is_none_or(|v| {
                    v.is_empty() || v.len() > 4096 || v.contains(['\0', '\r', '\n', '*'])
                })
                || original.content_version.is_some()
                || original
                    .parent_id
                    .as_ref()
                    .is_none_or(|v| v.is_empty() || v.len() > 4096 || v.contains('\0'))
                || original.name.is_empty()
                || original.name.len() > 255
                || original.name.chars().any(|c| c.is_control() || c == '/')
            {
                return Err(super::UploadError::Invalid);
            }
        }
        if let Self::FlatNumbersReplacementArchive { original, .. } = self
            && !original.name.to_ascii_lowercase().ends_with(".numbers")
        {
            return Err(super::UploadError::Invalid);
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

/// Both exact package identities must be independently read back. This proves a
/// two-ID handoff, not same-ID editing or preservation of external sharing links.
pub struct PackageHandoffReceipt {
    pub original: crate::Node,
    pub current: PackageUploadReceipt,
    pub backup: PackageUploadReceipt,
}

#[cfg(test)]
mod semantic_version_tests {
    use super::*;
    #[test]
    fn semantic_versions_are_explicit_and_v2_requires_a_directory_root() {
        let mut proof = PackageSemanticIdentity {
            version: 1,
            sha256: "a".repeat(64),
            entries: 1,
            files: 1,
            expanded_bytes: 1,
        };
        assert!(proof.validate().is_ok());
        proof.version = 2;
        assert!(proof.validate().is_err());
        proof.entries = 2;
        assert!(proof.validate().is_ok());
        for version in [0, 3, u32::MAX] {
            proof.version = version;
            assert!(proof.validate().is_err());
        }
        assert_eq!(PACKAGE_SEMANTIC_IDENTITY_VERSION, 1);
    }
}

/// Filename pairing for an already proven native PACKAGE. A suffix is never
/// representation evidence or authority to write an ordinary DATA document.
pub fn native_package_suffix(name: &str) -> Option<&'static str> {
    if name.len() > 1024 || name.chars().any(|c| c.is_control() || c == '/') {
        return None;
    }
    [".pages", ".numbers", ".key"].into_iter().find(|suffix| {
        name.as_bytes()
            .get(name.len().saturating_sub(suffix.len())..)
            .is_some_and(|tail| tail.eq_ignore_ascii_case(suffix.as_bytes()))
    })
}

#[cfg(test)]
mod format_tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    #[test]
    fn flat_numbers_source_requires_explicit_v2() {
        let proof = PackageSemanticIdentity {
            version: 2,
            sha256: "a".repeat(64),
            entries: 3,
            files: 1,
            expanded_bytes: 1,
        };
        let value = UploadRepresentation::FlatNumbersArchive { semantic: proof };
        value.validate().unwrap();
        let UploadRepresentation::FlatNumbersArchive { mut semantic } = value else {
            unreachable!()
        };
        semantic.version = 1;
        assert!(
            UploadRepresentation::FlatNumbersArchive { semantic }
                .validate()
                .is_err()
        );
    }
    #[test]
    fn flat_numbers_replacement_requires_selected_package_identity_and_revision() {
        let proof = PackageSemanticIdentity {
            version: 2,
            sha256: "a".repeat(64),
            entries: 3,
            files: 1,
            expanded_bytes: 1,
        };
        let original = crate::Node {
            id: "selected-id".into(),
            parent_id: Some("selected-parent".into()),
            name: "Selected.numbers".into(),
            kind: crate::NodeKind::Folder,
            size: 1,
            modified_unix: 0,
            etag: Some("selected-etag".into()),
            content_version: None,
            target: None,
            package: true,
        };
        let mut request = super::super::UploadRequest {
            representation: UploadRepresentation::FlatNumbersReplacementArchive {
                semantic: proof.clone(),
                original: Box::new(original),
                original_semantic: proof,
            },
            scope: crate::Scope {
                account: "owned".into(),
                provider: "icloud".into(),
                collection: "drive".into(),
            },
            intent: super::super::UploadIntent::Replace {
                item: "selected-id".into(),
                expected_etag: "selected-etag".into(),
            },
            size: 1,
            sha256: "b".repeat(64),
        };
        request.validate().unwrap();
        request.intent = super::super::UploadIntent::Replace {
            item: "different-id".into(),
            expected_etag: "selected-etag".into(),
        };
        assert!(request.validate().is_err());
        request.intent = super::super::UploadIntent::Replace {
            item: "selected-id".into(),
            expected_etag: "different-etag".into(),
        };
        assert!(request.validate().is_err());
        request.intent = super::super::UploadIntent::Replace {
            item: "selected-id".into(),
            expected_etag: "selected-etag".into(),
        };
        let UploadRepresentation::FlatNumbersReplacementArchive { original, .. } =
            &mut request.representation
        else {
            unreachable!()
        };
        original.package = false;
        assert!(request.validate().is_err());
        let UploadRepresentation::FlatNumbersReplacementArchive { original, .. } =
            &mut request.representation
        else {
            unreachable!()
        };
        original.package = true;
        original.name = "Selected.pages".into();
        assert!(request.validate().is_err());
    }
    #[test]
    fn native_format_names_are_bounded_single_components_not_representation_proofs() {
        for suffix in [".pages", ".numbers", ".key"] {
            assert_eq!(
                native_package_suffix(&format!("Owned{suffix}")),
                Some(suffix)
            );
            for bad in [
                format!("../Owned{suffix}"),
                format!("Root/Owned{suffix}"),
                format!("bad\0{suffix}"),
                format!("{}{suffix}", "x".repeat(1025)),
            ] {
                assert!(native_package_suffix(&bad).is_none());
            }
        }
        assert_eq!(
            native_package_suffix(".pages"),
            Some(".pages"),
            "legacy Pages name remains valid"
        );
        assert_eq!(native_package_suffix("Legacy:name.pages"), Some(".pages"));
        assert_eq!(native_package_suffix("Legacy\\name.pages"), Some(".pages"));
        assert!(native_package_suffix("Owned.zip").is_none());
        assert!(native_package_suffix("Owned.numbers.pages/Document").is_none());
        assert!(UploadRepresentation::FileBytes.validate().is_ok());
    }
    #[test]
    fn generic_package_replacement_keeps_provider_independent_names_and_roots() {
        let proof = PackageSemanticIdentity {
            version: 1,
            sha256: "a".repeat(64),
            entries: 1,
            files: 1,
            expanded_bytes: 1,
        };
        for (name, root) in [
            ("Native.bundle", "Source.bundle"),
            ("OpaqueContainer", "BoundRoot"),
            (".pages", ".pages"),
        ] {
            let original = crate::Node {
                id: "provider-item".into(),
                parent_id: Some("provider-parent".into()),
                name: name.into(),
                kind: crate::NodeKind::Folder,
                size: 1,
                modified_unix: 0,
                etag: Some("v1".into()),
                content_version: None,
                target: None,
                package: true,
            };
            let value = UploadRepresentation::PackageReplacementArchive {
                expected_root: root.into(),
                semantic: proof.clone(),
                original: Box::new(original),
                original_semantic: proof.clone(),
            };
            value.validate().unwrap();
            let mut data = value.clone();
            if let UploadRepresentation::PackageReplacementArchive { original, .. } = &mut data {
                original.package = false;
                original.kind = crate::NodeKind::File;
            }
            assert!(
                data.validate().is_err(),
                "generic DTO still requires explicit package representation"
            );
        }
    }
}

#[cfg(test)]
mod case_policy_tests {
    use super::*;
    #[test]
    fn native_format_case_is_classification_only_not_root_normalization() {
        for (name, suffix) in [
            ("Owned.PAGES", ".pages"),
            ("Owned.NuMbErS", ".numbers"),
            ("Owned.KEY", ".key"),
            (".PaGeS", ".pages"),
        ] {
            assert_eq!(native_package_suffix(name), Some(suffix));
        }
        assert_eq!(native_package_suffix("Owned.éKEY"), None);
        assert_eq!(native_package_suffix("Owned.KEY/child"), None);
    }
}
