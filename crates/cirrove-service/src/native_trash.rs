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
        && input.path.ends_with(".pages")
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
