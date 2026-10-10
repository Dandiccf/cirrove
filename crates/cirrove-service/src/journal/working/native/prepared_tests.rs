#![allow(clippy::unwrap_used)]
use super::tests::{binding, bytes, count, edit, journal, publish, temp};
use super::*;

#[test]
fn native_prepared_seal_adopts_the_validated_inode_and_reopens_exact_receipt() {
    let t = temp();
    let initial = bytes(b"original");
    let bound = binding(&t, &initial);
    let mut j = journal(&t);
    let f = publish(&mut j, bound, &initial);
    let edited = bytes(b"prepared immutable generation");
    edit(&mut j, f.id, &edited);
    let copy = j
        .capture_native_working(f.id)
        .unwrap()
        .capture(&CancellationToken::new())
        .unwrap();
    let captured = copy.file.metadata().unwrap();
    let staged_path = copy._reservation.to_path_buf();
    let expected_hash = copy.sha256.clone();
    // The already reserved capture occupies the entire remaining quota.
    // Adopting it must not demand room for a second identical object.
    j.quota = j.retained_usage().unwrap().0;
    let row = j
        .seal_captured_native_working(copy, &CancellationToken::new())
        .unwrap();
    let object = j.objects.join(row.id.to_string());
    let published = std::fs::metadata(&object).unwrap();
    assert_eq!(
        (published.dev(), published.ino()),
        (captured.dev(), captured.ino()),
        "native seal copied the archive again under the journal lock"
    );
    assert!(!staged_path.exists());
    assert_eq!(std::fs::read(&object).unwrap(), edited);
    assert_eq!(row.sha256, expected_hash);
    assert!(!j.working_file(f.id).unwrap().dirty);
    drop(j);
    let j = journal(&t);
    assert_eq!(j.get(row.id).unwrap().sha256, expected_hash);
    assert_eq!(j.read_working(f.id, 0, 1_000_000).unwrap(), edited);
}

#[test]
fn native_prepared_cancel_and_changed_generation_preserve_dirty_bytes_without_queue() {
    for cancelled in [false, true] {
        let t = temp();
        let initial = bytes(b"original");
        let bound = binding(&t, &initial);
        let mut j = journal(&t);
        let f = publish(&mut j, bound, &initial);
        let edited = bytes(b"captured");
        edit(&mut j, f.id, &edited);
        let copy = j
            .capture_native_working(f.id)
            .unwrap()
            .capture(&CancellationToken::new())
            .unwrap();
        let token = CancellationToken::new();
        let retained = if cancelled {
            token.cancel();
            edited
        } else {
            let later = bytes(b"newer unsealed generation");
            edit(&mut j, f.id, &later);
            later
        };
        assert!(j.seal_captured_native_working(copy, &token).is_err());
        assert_eq!(count(&j), 0);
        assert!(j.working_file(f.id).unwrap().dirty);
        assert_eq!(j.read_working(f.id, 0, 1_000_000).unwrap(), retained);
        assert_eq!(std::fs::read_dir(&j.objects).unwrap().count(), 0);
    }
}

#[test]
fn native_prepared_failed_row_commit_retains_published_inode_and_dirty_generation() {
    let t = temp();
    let initial = bytes(b"original");
    let bound = binding(&t, &initial);
    let mut j = journal(&t);
    let f = publish(&mut j, bound, &initial);
    let edited = bytes(b"retained if publication cannot commit");
    edit(&mut j, f.id, &edited);
    let copy = j
        .capture_native_working(f.id)
        .unwrap()
        .capture(&CancellationToken::new())
        .unwrap();
    let inode = copy.file.metadata().unwrap().ino();
    j.db.execute_batch("CREATE TEMP TRIGGER fail_native_row BEFORE INSERT ON uploads BEGIN SELECT RAISE(ABORT,'fixture publication failure'); END;").unwrap();
    assert!(
        j.seal_captured_native_working(copy, &CancellationToken::new())
            .is_err()
    );
    assert_eq!(count(&j), 0);
    let orphans: Vec<_> = std::fs::read_dir(&j.objects)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(orphans.len(), 1);
    assert_eq!(std::fs::metadata(&orphans[0]).unwrap().ino(), inode);
    assert_eq!(std::fs::read(&orphans[0]).unwrap(), edited);
    assert!(j.working_file(f.id).unwrap().dirty);
    drop(j);
    let j = journal(&t);
    assert_eq!(count(&j), 0);
    assert!(j.working_file(f.id).unwrap().dirty);
    assert_eq!(j.read_working(f.id, 0, 1_000_000).unwrap(), edited);
    assert!(
        orphans[0].exists(),
        "restart discarded unacknowledged snapshot bytes"
    );
}
