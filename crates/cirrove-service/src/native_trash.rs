//! Explicit, account-bound native-container Trash admission; never a FUSE write.
use cirrove_core::Node;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeTrashInput {
    pub expected_account_id: String,
    pub path: String,
    pub item_id: String,
    pub etag: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NativeTrashStatus {
    pub operation: Uuid,
    pub state: crate::journal::MutationState,
    pub removal_confirmed: bool,
    pub metadata_removed: bool,
}
pub(crate) struct NativeTrashAdmission {
    pub parent: crate::native_import::ImportParent,
    pub target: Node,
}

pub(crate) fn validate(input: &NativeTrashInput) -> bool {
    uuid::Uuid::parse_str(&input.expected_account_id).is_ok()
        && !input.path.is_empty()
        && input.path.len() <= 4096
        && input.path.split('/').count() <= 33
        && input.path.split('/').all(|p| {
            !p.is_empty()
                && p != "."
                && p != ".."
                && p.len() <= 255
                && !p.chars().any(char::is_control)
        })
        && input
            .path
            .rsplit('/')
            .next()
            .is_some_and(|name| cirrove_core::upload::native_package_suffix(name).is_some())
        && input.item_id.starts_with("FILE::com.apple.CloudDocs::")
        && input.item_id.len() > "FILE::com.apple.CloudDocs::".len()
        && input.item_id.len() <= 4096
        && !input.item_id.chars().any(char::is_control)
        && !input.etag.is_empty()
        && !input.etag.contains('*')
        && input.etag.len() <= 4096
        && !input.etag.chars().any(char::is_control)
}
pub(crate) fn matches(input: &NativeTrashInput, node: &Node, parent: &str) -> bool {
    node.id == input.item_id
        && node.etag.as_deref() == Some(&input.etag)
        && node.parent_id.as_deref() == Some(parent)
        && input.path.rsplit('/').next() == Some(node.name.as_str())
        && node.kind == cirrove_core::NodeKind::Folder
        && node.package
        && node.target.is_none()
        && node
            .content_version
            .as_ref()
            .is_none_or(|r| node.etag.as_ref() == Some(r))
}

/// Retained historical receipt evidence; not a fresh cloud observation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NativeTrashSelection {
    pub operation: Uuid,
    pub sequence: u64,
    pub item_id: String,
    pub name: String,
    pub etag: String,
    pub state: crate::journal::MutationState,
    pub removal_receipt_recorded: bool,
    pub metadata_absence_recorded: bool,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NativeTrashListing {
    pub operations: Vec<NativeTrashSelection>,
    /// Continue even when this page is empty. Older journals bound scanned rows.
    pub next: Option<u64>,
}

#[cfg(test)]
mod format_tests {
    use super::*;
    #[test]
    fn selected_formats_never_substitute_for_package_identity() {
        for suffix in ["pages", "numbers", "key"] {
            let input = NativeTrashInput {
                expected_account_id: Uuid::new_v4().to_string(),
                path: format!("Owned/Document.{suffix}"),
                item_id: "FILE::com.apple.CloudDocs::owned".into(),
                etag: "revision".into(),
            };
            assert!(validate(&input));
            let node = Node {
                id: input.item_id.clone(),
                parent_id: Some("parent".into()),
                name: format!("Document.{suffix}"),
                kind: cirrove_core::NodeKind::Folder,
                size: 1,
                modified_unix: 0,
                etag: Some(input.etag.clone()),
                content_version: None,
                target: None,
                package: true,
            };
            assert!(matches(&input, &node, "parent"));
            let mut data = node.clone();
            data.package = false;
            data.kind = cirrove_core::NodeKind::File;
            assert!(!matches(&input, &data, "parent"));
            let mut ambiguous = node.clone();
            ambiguous.package = false;
            assert!(!matches(&input, &ambiguous, "parent"));
            let mut stale = node.clone();
            stale.etag = Some("next".into());
            assert!(!matches(&input, &stale, "parent"));
            let mut other = node.clone();
            other.id = "FILE::com.apple.CloudDocs::other".into();
            assert!(!matches(&input, &other, "parent"));
            let mut renamed = node.clone();
            renamed.name = "Document.zip".into();
            assert!(!matches(&input, &renamed, "parent"));
        }
    }
}
