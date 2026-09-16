//! A directory open on the mount turns into metadata lookups in this store, and
//! a desktop file indexer performs them by the thousand without being asked. On
//! 2026-09-16 GNOME's localsearch crawled a mount after an operating system
//! update and held a core at roughly 875,000 read syscalls per second for 25
//! minutes. None of those reads reached the disk: the pages were already in the
//! kernel's cache, and the daemon fetched them 4 KiB at a time through a
//! syscall each. The store must therefore serve a working set larger than
//! SQLite's own page cache without paying a syscall per page.
#![allow(clippy::unwrap_used)]
use cirrove_core::{Node, NodeKind, Scope};
use cirrove_store::Store;

/// Per-thread counters. `cargo test` runs tests as threads of one process, so
/// the process-wide file would count whatever another test happens to read.
fn read_syscalls() -> u64 {
    let io = std::fs::read_to_string("/proc/thread-self/io").expect("per-thread io counters");
    io.lines()
        .find_map(|line| line.strip_prefix("syscr:"))
        .and_then(|value| value.trim().parse().ok())
        .expect("syscr")
}

fn scope(drive: &str) -> Scope {
    Scope {
        account: "11111111-2222-3333-4444-555555555555".into(),
        provider: "onedrive".into(),
        collection: drive.into(),
    }
}

/// Bodies are padded to the size a real provider returns. An undersized row
/// makes the whole table fit the default cache and the measurement says
/// nothing.
fn node(id: usize, parent: &str) -> Node {
    Node {
        id: format!("{id:0>48}"),
        parent_id: Some(parent.into()),
        name: format!("{id:0>200}.txt"),
        kind: NodeKind::File,
        size: 5,
        modified_unix: 1,
        etag: Some(format!("{id:0>200}")),
        content_version: Some(format!("{id:0>200}")),
        target: None,
        package: false,
    }
}

const DIRECTORIES: usize = 120;
const PER_DIRECTORY: usize = 250;

#[test]
fn a_lookup_in_a_store_larger_than_the_page_cache_costs_no_read_syscall() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("metadata.db");
    let mut db = Store::open(&path).unwrap();
    let drive = scope("drive");
    let mut identities = Vec::new();
    for directory in 0..DIRECTORIES {
        let parent = format!("dir{directory:0>44}");
        let children: Vec<Node> = (0..PER_DIRECTORY)
            .map(|child| node(directory * PER_DIRECTORY + child, &parent))
            .collect();
        identities.extend(children.iter().map(|child| child.id.clone()));
        db.observe_directory(&drive, &parent, &children).unwrap();
    }
    let bytes = std::fs::metadata(&path).unwrap().len();
    // SQLite's default page cache is 2000 pages, about 2 MiB. A store that fits
    // inside it cannot show the difference this test exists to measure.
    assert!(
        bytes > 16 * 1024 * 1024,
        "fixture must exceed the default page cache by a wide margin, got {bytes} bytes"
    );

    // Walk once so every page has been in the kernel's cache at least once.
    // What follows measures syscalls, not disk.
    for id in &identities {
        db.node(&drive, id).unwrap();
    }

    let before = read_syscalls();
    for id in &identities {
        assert!(db.node(&drive, id).unwrap().is_some());
    }
    let reads = read_syscalls() - before;
    let per_lookup = reads as f64 / identities.len() as f64;
    assert!(
        per_lookup < 0.1,
        "{reads} read syscalls for {} lookups in a {} MiB store, {per_lookup:.2} per lookup: \
         the store is paying a syscall per page for data the kernel already holds",
        identities.len(),
        bytes / 1024 / 1024
    );
}
