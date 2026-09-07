#![allow(clippy::unwrap_used)]
use super::*;
use std::os::unix::fs::MetadataExt;

#[test]
fn pages_preserve_cookies_names_and_independent_reader_positions() {
    let temp = tempfile::tempdir().unwrap();
    let budget = Budget::default();
    let mut builder = budget.start(temp.path()).unwrap();
    let names = (0..2400)
        .map(|i| format!("Äpfel-{i}-{}", "x".repeat(i % 300)))
        .collect::<Vec<_>>();
    for (i, name) in names.iter().enumerate() {
        builder.push(i as u64 + 1, i % 2 == 0, name).unwrap();
    }
    let snapshot = Arc::new(builder.finish().unwrap());
    assert_eq!(snapshot.len(), names.len() as u64);
    assert_eq!(snapshot.data.metadata().unwrap().nlink(), 0);
    assert_eq!(snapshot.index.metadata().unwrap().nlink(), 0);
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    assert_eq!(
        budget.usage().0,
        snapshot.data_bytes + names.len() as u64 * INDEX_BYTES
    );
    std::thread::scope(|threads| {
        for start in [0, 713, 2031] {
            let snapshot = &snapshot;
            let names = &names;
            threads.spawn(move || {
                let mut cursor = start;
                while cursor < names.len() {
                    let page = snapshot.page(cursor as u64).unwrap();
                    assert!(!page.is_empty());
                    assert!(page.len() <= PAGE_ENTRIES);
                    assert!(
                        page.iter()
                            .map(|e| e.name.len() + HEADER_BYTES)
                            .sum::<usize>()
                            <= PAGE_BYTES
                    );
                    for entry in page {
                        assert_eq!(entry.inode, cursor as u64 + 1);
                        assert_eq!(entry.name, names[cursor]);
                        assert_eq!(entry.directory, cursor % 2 == 0);
                        cursor += 1;
                    }
                }
            });
        }
    });
    assert!(snapshot.page(u64::MAX).unwrap().is_empty());
    let held_reader = snapshot.clone();
    drop(snapshot);
    assert_eq!(budget.usage().1, 1);
    drop(held_reader);
    assert_eq!(budget.usage(), (0, 0));
}

#[test]
fn failed_builds_release_reservations_and_cannot_publish_a_partial_directory() {
    let temp = tempfile::tempdir().unwrap();
    let budget = Budget::default();
    let mut builder = budget.start(temp.path()).unwrap();
    builder.push(1, true, ".").unwrap();
    assert!(builder.push(2, false, "bad/name").is_err());
    assert!(builder.finish().is_err());
    assert_eq!(budget.usage(), (0, 0));
    let mut occupied = budget.start(temp.path()).unwrap();
    occupied.reservation.grow(MAX_BYTES).unwrap();
    let mut other = budget.start(temp.path()).unwrap();
    assert_eq!(
        other.push(1, false, "x").unwrap_err().raw_os_error(),
        Some(libc::ENOSPC)
    );
    assert!(other.finish().is_err());
    drop(occupied);
    assert_eq!(budget.usage(), (0, 0));
    assert!(budget.start(&temp.path().join("missing")).is_err());
    assert_eq!(budget.usage(), (0, 0));
    let mut too_long = budget.start(temp.path()).unwrap();
    assert_eq!(
        too_long
            .push(1, false, &"x".repeat(PAGE_BYTES))
            .unwrap_err()
            .raw_os_error(),
        Some(libc::ENAMETOOLONG)
    );
    drop(too_long);
    assert_eq!(budget.usage(), (0, 0));
}

#[test]
fn damaged_index_and_record_fail_without_unbounded_allocation_or_panics() {
    for damage in [0, 1, 2, 3] {
        let temp = tempfile::tempdir().unwrap();
        let budget = Budget::default();
        let mut builder = budget.start(temp.path()).unwrap();
        builder.push(1, false, "file").unwrap();
        let snapshot = builder.finish().unwrap();
        match damage {
            0 => snapshot
                .index
                .write_all_at(&u64::MAX.to_le_bytes(), 0)
                .unwrap(),
            1 => snapshot.data.write_all_at(&[7], 8).unwrap(),
            2 => snapshot
                .data
                .write_all_at(&u32::MAX.to_le_bytes(), 9)
                .unwrap(),
            _ => snapshot.data.set_len(2).unwrap(),
        }
        assert!(snapshot.page(0).is_err());
    }
}

#[test]
fn snapshot_admission_returns_capacity_after_the_last_reader_closes() {
    let temp = tempfile::tempdir().unwrap();
    let budget = Budget::default();
    let mut snapshots = (0..MAX_SNAPSHOTS)
        .map(|_| {
            let mut builder = budget.start(temp.path()).unwrap();
            builder.push(1, true, ".").unwrap();
            builder.finish().unwrap()
        })
        .collect::<Vec<_>>();
    let error = budget.start(temp.path()).err().unwrap();
    assert_eq!(error.raw_os_error(), Some(libc::EMFILE));
    assert_eq!(budget.usage().1, MAX_SNAPSHOTS);
    snapshots.pop();
    let replacement = budget.start(temp.path()).unwrap();
    assert_eq!(budget.usage().1, MAX_SNAPSHOTS);
    drop(replacement);
    drop(snapshots);
    assert_eq!(budget.usage(), (0, 0));
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
}

#[test]
fn delayed_disk_full_at_flush_never_publishes_and_releases_the_reservation() {
    let temp = tempfile::tempdir().unwrap();
    let budget = Budget::default();
    for fail_index in [false, true] {
        let mut builder = budget.start(temp.path()).unwrap();
        let full = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .unwrap();
        if fail_index {
            builder.index = BufWriter::new(full);
        } else {
            builder.data = BufWriter::new(full);
        }
        // Buffered writes succeed before the backing file reports ENOSPC.
        builder.push(1, false, "cloud-file").unwrap();
        assert!(budget.usage().0 > 0);
        let error = builder.finish().err().unwrap();
        assert_eq!(error.raw_os_error(), Some(libc::ENOSPC));
        assert_eq!(budget.usage(), (0, 0));
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    }
}
