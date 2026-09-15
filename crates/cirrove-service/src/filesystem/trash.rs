//! Which names the mount root refuses to become a wastebasket under.
//!
//! The list is not a matter of taste: it is what GIO looks for and creates when
//! a file manager deletes something on a filesystem that is not the user's home.
//! Getting it wrong in either direction is a real failure. Too narrow and a
//! `.Trash-1000` appears in the user's cloud drive on the next Delete; too wide
//! and an ordinary folder the user made becomes uncreatable.

use super::is_trash_directory;

#[test]
fn the_names_a_file_manager_would_create_are_refused() {
    // `.Trash` is the shared one, `.Trash-$uid` the per-user fallback GIO
    // creates when the first is absent -- which on a cloud mount it always is,
    // because nothing here sets a sticky bit.
    assert!(is_trash_directory(".Trash"));
    assert!(is_trash_directory(".Trash-0"));
    assert!(is_trash_directory(".Trash-1000"));
    assert!(is_trash_directory(".Trash-4294967294"));
}

#[test]
fn an_ordinary_folder_that_merely_reads_like_one_is_not() {
    // A user's own folder must stay creatable. No trash implementation looks for
    // any of these, so refusing them would cost the user a name and buy nothing.
    for name in [
        "Trash",
        ".Trashcan",
        ".Trash-",
        ".Trash-user",
        ".Trash-1000-old",
        ".Trash-1000 ",
        ".trash-1000",
        "Papierkorb",
        "..Trash-1000",
    ] {
        assert!(!is_trash_directory(name), "{name} must stay creatable");
    }
}
