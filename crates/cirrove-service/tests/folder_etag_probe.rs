//! A OneDrive folder's eTag does **not** move when a child is added.
//!
//! This was measured, not assumed, because it decides whether `rmdir` can be
//! built safely. Graph's DELETE on a folder is recursive: it removes whatever the
//! folder contains. POSIX `rmdir` promises the opposite -- it refuses a non-empty
//! directory -- so offering that promise over Graph means verifying emptiness and
//! then deleting under a precondition that fails if the folder changed in between.
//!
//! The prediction recorded before the first run was that the eTag *does* move,
//! because `childCount` is part of the folder item. It was wrong. Adding a child
//! left both the eTag and `lastModifiedDateTime` byte-identical.
//!
//! **The consequence is the useful part.** `If-Match` on a folder's eTag cannot
//! close the window between an emptiness check and a recursive DELETE, and a
//! delayed eTag update would not help either -- it would let the precondition pass
//! at exactly the moment it needs to fail. Any `rmdir` built on Graph therefore
//! carries a residual window in which a concurrently created child is destroyed by
//! a delete that believed the folder was empty. That window can be narrowed to one
//! round trip and it can be detected afterwards, but it cannot be closed with a
//! precondition, and no design should be written as though it can.
//!
//! Live account, so ignored by default:
//!
//!     CIRROVE_PROBE_STATE=.local-state/write-validation \
//!       cargo test -p cirrove-service --test folder_etag_probe -- --ignored --nocapture
//!
//! It creates one folder and one child and leaves both behind, because nothing in
//! Cirrove can remove a folder yet -- which is the reason this probe exists.
use cirrove_core::{CancellationToken, ReadProvider, Scope};
use std::path::PathBuf;
use std::time::Duration;

#[tokio::test]
#[ignore = "requires a live write-access account"]
async fn folder_etag_and_mtime_ignore_their_children() {
    let state = PathBuf::from(
        std::env::var("CIRROVE_PROBE_STATE").expect("set CIRROVE_PROBE_STATE to the state dir"),
    );
    let account = cirrove_service::accounts::Settings::load(&state)
        .expect("settings")
        .accounts
        .into_iter()
        .find(|a| a.access == cirrove_auth::AccessMode::ReadWrite && !a.enabled)
        .expect("a disabled, explicitly writable account");

    let graph = cirrove_service::accounts::provider(&account).expect("provider");
    let scope = Scope {
        account: account.id.clone(),
        provider: "onedrive".into(),
        collection: account.drive.id.clone(),
    };
    let cancel = CancellationToken::new();
    let run = uuid::Uuid::new_v4();

    let folder = graph
        .create_folder(
            &scope,
            &account.root_id,
            &format!("Cirrove-Rmdir-Probe-{run}"),
            &cancel,
        )
        .await
        .expect("probe folder");
    let empty = graph
        .node(&scope, &folder.id, &cancel)
        .await
        .expect("read the empty folder");

    graph
        .create_folder(&scope, &folder.id, "child", &cancel)
        .await
        .expect("child folder");
    let immediate = graph
        .node(&scope, &folder.id, &cancel)
        .await
        .expect("read the occupied folder");

    // A delayed update would not rescue the design -- a precondition that becomes
    // correct half a minute later is exactly the one that passes during the race --
    // but whether the parent ever notices its children is worth knowing for change
    // detection, so it is measured rather than left open.
    tokio::time::sleep(Duration::from_secs(60)).await;
    let settled = graph
        .node(&scope, &folder.id, &cancel)
        .await
        .expect("read the folder again");

    let children = graph
        .children(&scope, &folder.id, None, &cancel)
        .await
        .expect("list children");

    println!(
        "{}",
        serde_json::json!({
            "folder": folder.id,
            "etag_when_empty": empty.etag,
            "etag_immediately_after_child": immediate.etag,
            "etag_after_60s": settled.etag,
            "mtime_when_empty": empty.modified_unix,
            "mtime_immediately_after_child": immediate.modified_unix,
            "mtime_after_60s": settled.modified_unix,
            "children": children.nodes.len(),
        })
    );

    assert_eq!(children.nodes.len(), 1, "the child should be listed");
    // The measured behaviour, asserted so that a provider change is noticed rather
    // than silently making a documented hazard obsolete or a design unsound.
    assert_eq!(
        empty.etag, immediate.etag,
        "the folder eTag moved on child creation; If-Match may now be usable for \
         rmdir, and docs/adr and the rmdir design should be revisited"
    );
    assert_eq!(
        empty.modified_unix, immediate.modified_unix,
        "the folder mtime moved on child creation; revisit the rmdir design"
    );
    assert_eq!(
        empty.etag, settled.etag,
        "the folder eTag moved within a minute of child creation; a delayed update \
         does not make If-Match safe, but it changes what change detection can see"
    );
}
