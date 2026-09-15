#![allow(clippy::unwrap_used)]
//! What `local-data` reports, and the one thing `--discard-removed` must never
//! touch.
//!
//! An uninstall deliberately leaves the person's data alone, which is right,
//! and until now left them with no way to find out what that cost or to get any
//! of it back: `forget` moves a removed connection's data into `removed/` and
//! nothing ever looks at it again. On a real machine that was 2.6 GB whose only
//! documented remedy was `rm -rf` on a path from the guide.
//!
//! The dangerous version of this feature is one that can be talked into
//! deleting a drive someone is still using, so that is what these assert.

use cirrove_service::accounts::{discard_set_aside, local_data};
use std::fs;
use std::os::unix::fs::PermissionsExt;

fn state_with(live: &[(&str, usize)], removed: &[(&str, usize)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();
    // A real state directory is 0700 and the config lock insists on it, so the
    // fixture has to be one too.
    fs::set_permissions(state, fs::Permissions::from_mode(0o700)).unwrap();
    for (id, bytes) in live {
        let account = state.join("accounts").join(id);
        fs::create_dir_all(&account).unwrap();
        fs::write(account.join("cache.bin"), vec![7u8; *bytes]).unwrap();
    }
    for (name, bytes) in removed {
        let aside = state.join("removed").join(name);
        fs::create_dir_all(&aside).unwrap();
        fs::write(aside.join("cache.bin"), vec![7u8; *bytes]).unwrap();
    }
    dir
}

#[test]
fn set_aside_data_is_reported_and_is_the_part_that_can_be_reclaimed() {
    let dir = state_with(&[("live-id", 4096)], &[("gone-id-1700000000", 8192)]);
    let data = local_data(dir.path()).unwrap();
    assert_eq!(
        data.set_aside.len(),
        1,
        "the removed connection must be listed"
    );
    assert_eq!(data.set_aside[0].bytes, 8192);
    assert_eq!(
        data.set_aside[0].removed_at, 1_700_000_000,
        "the stamp comes off the name"
    );
    assert_eq!(
        data.reclaimable_bytes(),
        8192,
        "only the set-aside data counts as reclaimable; a live account's does not"
    );
    assert!(data.total_bytes() >= 4096 + 8192);
}

#[test]
fn discarding_removed_data_never_reaches_a_live_account() {
    let dir = state_with(&[("live-id", 4096)], &[("gone-id-1700000000", 8192)]);
    let (count, bytes) = discard_set_aside(dir.path()).unwrap();
    assert_eq!((count, bytes), (1, 8192));
    assert!(
        !dir.path()
            .join("removed")
            .join("gone-id-1700000000")
            .exists(),
        "the set-aside data should be gone"
    );
    assert!(
        dir.path()
            .join("accounts")
            .join("live-id")
            .join("cache.bin")
            .exists(),
        "a connection still in use must be untouched -- this is the whole safety property"
    );
}

#[test]
fn a_state_directory_with_nothing_set_aside_deletes_nothing_and_does_not_fail() {
    let dir = state_with(&[("live-id", 4096)], &[]);
    assert_eq!(discard_set_aside(dir.path()).unwrap(), (0, 0));
    assert!(
        dir.path()
            .join("accounts")
            .join("live-id")
            .join("cache.bin")
            .exists()
    );
}
