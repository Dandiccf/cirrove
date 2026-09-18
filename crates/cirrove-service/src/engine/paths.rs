//! An account can subscribe to more than one drive, and every path shown to a
//! person is resolved by walking a node's ancestors until a root is reached.
//! `root` names only the account's own drive, so items in any other one walked
//! up to a parentless node that did not match and resolved to nothing.
//!
//! The owner met it on 2026-09-16, one command after keeping a PDF offline from
//! the file manager: `cirrove pins` named the file
//! `01YQR2QYPXJNXJZ7LXENA2KXH77S2VBEFB`. Refused changes and failed saves in
//! that drive were just as nameless, and each of those is something a person is
//! expected to act on.
#![allow(clippy::unwrap_used)]
use super::relative_path;
use cirrove_core::{Node, NodeKind, Scope};
use cirrove_store::Store;

fn scope() -> Scope {
    Scope {
        account: "11111111-2222-3333-4444-555555555555".into(),
        provider: "onedrive".into(),
        collection: "drive".into(),
    }
}

fn node(id: &str, name: &str, parent: Option<&str>, kind: NodeKind) -> Node {
    Node {
        id: id.into(),
        parent_id: parent.map(ToOwned::to_owned),
        name: name.into(),
        kind,
        size: 7,
        modified_unix: 1,
        etag: None,
        content_version: None,
        target: None,
        package: false,
    }
}

/// Two drives in one account, as the owner's has: the configured root, and a
/// second root reached through a subscription.
fn two_drives() -> (Store, Scope) {
    let mut store = Store::open(":memory:").unwrap();
    let drive = scope();
    let own_root = node("own-root", "root", None, NodeKind::Folder);
    let other_root = node("other-root", "root", None, NodeKind::Folder);
    let folder = node(
        "folder",
        "Unternehmen",
        Some("other-root"),
        NodeKind::Folder,
    );
    let file = node(
        "file",
        "Umlaufbeschluss.pdf",
        Some("folder"),
        NodeKind::File,
    );
    let in_own = node("mine", "Notes.txt", Some("own-root"), NodeKind::File);
    store
        .observe_directory(&drive, "other-root", std::slice::from_ref(&folder))
        .unwrap();
    store
        .observe_directory(&drive, "folder", std::slice::from_ref(&file))
        .unwrap();
    store
        .observe_directory(&drive, "own-root", std::slice::from_ref(&in_own))
        .unwrap();
    // The roots themselves, so the walk can see they have no parent.
    store
        .observe_directory(&drive, "roots", &[own_root, other_root])
        .unwrap();
    (store, drive)
}

#[test]
fn an_item_in_the_accounts_own_drive_keeps_its_path() {
    let (store, drive) = two_drives();
    assert_eq!(
        relative_path(&store, &drive, "own-root", "mine"),
        Some("Notes.txt".to_owned())
    );
}

#[test]
fn an_item_in_another_subscribed_drive_is_not_nameless() {
    let (store, drive) = two_drives();
    assert_eq!(
        relative_path(&store, &drive, "own-root", "file"),
        Some("Unternehmen/Umlaufbeschluss.pdf".to_owned()),
        "an item under a root the account is not configured with still has a \
         path; without this it resolves to nothing and a person is shown a \
         provider identifier where a file name belongs"
    );
}

#[test]
fn a_root_contributes_no_name_of_its_own() {
    let (store, drive) = two_drives();
    let path = relative_path(&store, &drive, "own-root", "file").unwrap();
    assert!(
        !path.starts_with("root/"),
        "the drive root is where the path starts, not a folder in it: {path}"
    );
}

#[test]
fn a_chain_that_is_genuinely_broken_still_resolves_to_nothing() {
    let mut store = Store::open(":memory:").unwrap();
    let drive = scope();
    // A child whose parent was never observed: not a root, just absent. This
    // must stay None rather than become a path rooted at the missing parent.
    let orphan = node("orphan", "Lost.txt", Some("never-seen"), NodeKind::File);
    store
        .observe_directory(&drive, "never-seen", &[orphan])
        .unwrap();
    assert_eq!(relative_path(&store, &drive, "own-root", "orphan"), None);
}
