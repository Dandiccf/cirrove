#![allow(clippy::unwrap_used)]
use super::*;

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
    assert!(!snapshot.resident(), "a 2400-entry listing must spill");
    assert!(snapshot.unlinked());
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    assert_eq!(
        budget.usage().0,
        snapshot.progress.borrow().data_bytes + names.len() as u64 * INDEX_BYTES
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
    let occupied = budget.start(temp.path()).unwrap();
    occupied.reservation.grow(MAX_BYTES).unwrap();
    let mut other = budget.start(temp.path()).unwrap();
    assert_eq!(
        other.push(1, false, "x").unwrap_err().raw_os_error(),
        Some(libc::ENOSPC)
    );
    assert!(other.finish().is_err());
    drop(occupied);
    assert_eq!(budget.usage(), (0, 0));
    // Starting no longer touches the filesystem, so an unusable directory is
    // reported when the listing outgrows its resident allowance, and a small
    // listing keeps working without one.
    let mut missing = budget.start(&temp.path().join("missing")).unwrap();
    missing.push(1, false, "small").unwrap();
    assert_eq!(
        missing.publish().unwrap().unwrap().page(0).unwrap().len(),
        1
    );
    let mut spilling = budget.start(&temp.path().join("missing")).unwrap();
    let mut spill_error = None;
    for index in 0..SPILL_BYTES / HEADER_BYTES as u64 {
        if let Err(error) = spilling.push(index + 1, false, &format!("entry-{index}")) {
            spill_error = error.raw_os_error();
            break;
        }
    }
    assert_eq!(spill_error, Some(libc::ENOENT), "the spill must report why");
    // A builder that failed stays failed and can never publish afterwards.
    assert!(spilling.finish().is_err());
    drop(missing);
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
            0 => snapshot.damage(Part::Index, 0, &u64::MAX.to_le_bytes()),
            1 => snapshot.damage(Part::Data, 8, &[7]),
            2 => snapshot.damage(Part::Data, 9, &u32::MAX.to_le_bytes()),
            _ => snapshot.truncate(Part::Data, 2),
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
        builder
            .fail_writes(if fail_index { Part::Index } else { Part::Data })
            .unwrap();
        // Accepting an entry succeeds before the stream reports ENOSPC.
        builder.push(1, false, "cloud-file").unwrap();
        assert!(budget.usage().0 > 0);
        let error = builder.finish().err().unwrap();
        assert_eq!(error.raw_os_error(), Some(libc::ENOSPC));
        assert_eq!(budget.usage(), (0, 0));
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    }
}

#[tokio::test]
async fn published_prefix_is_readable_but_never_reports_premature_eof() {
    use tokio::time::{Duration, timeout};
    let temp = tempfile::tempdir().unwrap();
    let budget = Budget::default();
    let mut builder = budget.start(temp.path()).unwrap();
    for index in 0..128 {
        builder
            .push(index + 1, false, &format!("file-{index:04}"))
            .unwrap();
    }
    let snapshot = builder.publish().unwrap().unwrap();
    assert_eq!(snapshot.page(0).unwrap().len(), 128);
    for index in 128..2400 {
        builder
            .push(index + 1, false, &format!("file-{index:04}"))
            .unwrap();
    }
    // Even writes already flushed by BufWriter are invisible beyond the frontier.
    assert_eq!(
        snapshot.page(128).err().unwrap().kind(),
        io::ErrorKind::WouldBlock
    );
    let mut waiting = Box::pin(snapshot.ready(128));
    assert!(
        timeout(Duration::from_millis(10), &mut waiting)
            .await
            .is_err()
    );
    assert!(builder.publish().unwrap().is_none());
    timeout(Duration::from_secs(1), &mut waiting)
        .await
        .unwrap()
        .unwrap();
    drop(waiting);
    for start in [0, 97, 128, 1023, 2399] {
        let mut cursor = start;
        while cursor < 2400 {
            for entry in snapshot.page(cursor).unwrap() {
                assert_eq!(entry.inode, cursor + 1);
                assert_eq!(entry.name, format!("file-{cursor:04}"));
                cursor += 1;
            }
        }
    }
    let mut waiting = Box::pin(snapshot.ready(2400));
    assert!(
        timeout(Duration::from_millis(10), &mut waiting)
            .await
            .is_err()
    );
    assert!(builder.complete().unwrap().is_none());
    timeout(Duration::from_secs(1), &mut waiting)
        .await
        .unwrap()
        .unwrap();
    drop(waiting);
    assert!(snapshot.page(2400).unwrap().is_empty());
    assert_eq!(budget.usage().1, 1);
    drop(snapshot);
    assert_eq!(budget.usage(), (0, 0));
}

#[tokio::test]
async fn late_flush_failure_keeps_the_prefix_and_reports_error_instead_of_eof() {
    for fail_index in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let budget = Budget::default();
        let mut builder = budget.start(temp.path()).unwrap();
        builder.push(1, false, "preserved").unwrap();
        let snapshot = builder.publish().unwrap().unwrap();
        builder
            .fail_writes(if fail_index { Part::Index } else { Part::Data })
            .unwrap();
        builder.push(2, false, "unpublished").unwrap();
        assert_eq!(
            builder.publish().err().unwrap().raw_os_error(),
            Some(libc::ENOSPC)
        );
        assert_eq!(snapshot.page(0).unwrap()[0].name, "preserved");
        assert_eq!(
            snapshot.page(1).err().unwrap().raw_os_error(),
            Some(libc::ENOSPC)
        );
        assert_eq!(
            snapshot.ready(1).await.unwrap_err().raw_os_error(),
            Some(libc::ENOSPC)
        );
        drop(builder);
        assert_eq!(budget.usage().1, 1);
        drop(snapshot);
        assert_eq!(budget.usage(), (0, 0));
    }
}

#[tokio::test]
async fn abandoned_producer_wakes_readers_and_last_reader_cancels_production() {
    use tokio::time::{Duration, timeout};
    let temp = tempfile::tempdir().unwrap();
    let budget = Budget::default();
    let mut builder = budget.start(temp.path()).unwrap();
    builder.push(1, false, "preserved").unwrap();
    let snapshot = builder.publish().unwrap().unwrap();
    let mut waiting = Box::pin(snapshot.ready(1));
    assert!(
        timeout(Duration::from_millis(10), &mut waiting)
            .await
            .is_err()
    );
    drop(builder);
    assert_eq!(
        timeout(Duration::from_secs(1), &mut waiting)
            .await
            .unwrap()
            .unwrap_err()
            .raw_os_error(),
        Some(libc::EIO)
    );
    drop(waiting);
    assert_eq!(snapshot.page(0).unwrap()[0].name, "preserved");
    drop(snapshot);
    assert_eq!(budget.usage(), (0, 0));

    let mut builder = budget.start(temp.path()).unwrap();
    builder.push(1, false, "closed").unwrap();
    let snapshot = builder.publish().unwrap().unwrap();
    drop(snapshot);
    assert_eq!(budget.usage().1, 1, "writer still owns the backing files");
    assert_eq!(
        builder.publish().err().unwrap().raw_os_error(),
        Some(libc::ECANCELED)
    );
    drop(builder);
    assert_eq!(budget.usage(), (0, 0));
}

#[test]
fn an_ordinary_listing_stays_resident_while_a_large_one_spills() {
    let temp = tempfile::tempdir().unwrap();
    let budget = Budget::default();
    let names = |count: u64| {
        (0..count)
            .map(|index| format!("Projektunterlagen {index:04}.txt"))
            .collect::<Vec<_>>()
    };
    let read = |snapshot: &Snapshot, count: u64| {
        let mut seen = Vec::new();
        let mut cursor = 0;
        while cursor < count {
            for entry in snapshot.page(cursor).unwrap() {
                assert_eq!(entry.inode, cursor + 1);
                seen.push(entry.name);
                cursor += 1;
            }
        }
        seen
    };
    // An ordinary directory is served entirely from memory, so no snapshot file
    // is created on the filesystem the content cache also writes to.
    let small = names(500);
    let mut builder = budget.start(temp.path()).unwrap();
    for (index, name) in small.iter().enumerate() {
        builder.push(index as u64 + 1, false, name).unwrap();
    }
    let resident = builder.finish().unwrap();
    assert!(resident.resident());
    assert!(resident.unlinked());
    assert_eq!(read(&resident, 500), small);
    let resident_bytes = budget.usage().0;
    drop(resident);

    // Past the allowance the same listing spills, with identical contents and
    // identical byte accounting.
    let large = names(4000);
    let mut builder = budget.start(temp.path()).unwrap();
    for (index, name) in large.iter().enumerate() {
        builder.push(index as u64 + 1, false, name).unwrap();
    }
    let spilled = builder.finish().unwrap();
    assert!(!spilled.resident());
    assert!(spilled.unlinked());
    assert_eq!(read(&spilled, 4000), large);
    assert!(budget.usage().0 > resident_bytes);
    drop(spilled);
    assert_eq!(budget.usage(), (0, 0));
}

#[tokio::test]
async fn a_spill_preserves_the_prefix_an_open_reader_already_publishes() {
    let temp = tempfile::tempdir().unwrap();
    let budget = Budget::default();
    let mut builder = budget.start(temp.path()).unwrap();
    for index in 0..200u64 {
        builder
            .push(index + 1, false, &format!("früh-{index:04}"))
            .unwrap();
    }
    let snapshot = builder.publish().unwrap().unwrap();
    assert!(snapshot.resident(), "a short prefix starts resident");
    assert_eq!(snapshot.page(0).unwrap().len(), 200);

    // The same reader keeps its positions while the producer moves the bytes to
    // anonymous files underneath it.
    for index in 200..6000u64 {
        builder
            .push(index + 1, false, &format!("früh-{index:04}"))
            .unwrap();
    }
    assert!(builder.publish().unwrap().is_none());
    snapshot.ready(200).await.unwrap();
    assert!(!snapshot.resident(), "the listing must have spilled");
    let mut cursor = 0;
    while cursor < 6000 {
        for entry in snapshot.page(cursor).unwrap() {
            assert_eq!(entry.inode, cursor + 1);
            assert_eq!(entry.name, format!("früh-{cursor:04}"));
            cursor += 1;
        }
    }
    builder.complete().unwrap();
    snapshot.ready(6000).await.unwrap();
    assert!(snapshot.page(6000).unwrap().is_empty());
    drop(snapshot);
    assert_eq!(budget.usage(), (0, 0));
}
