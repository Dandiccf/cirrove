//! Local recovery only: no credentials or provider requests.
#![allow(clippy::unwrap_used)]
use cirrove_core::{CancellationToken, Scope};
use cirrove_service::journal::{UploadIntent, UploadJournal};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
};
fn setup(root: &Path, bytes: &[u8]) -> (UploadJournal, uuid::Uuid) {
    let mut journal = UploadJournal::open(&root.join("journal"), "owned", 1024 * 1024).unwrap();
    let row = journal
        .enqueue(
            Scope {
                account: "owned".into(),
                provider: "icloud".into(),
                collection: "drive".into(),
            },
            UploadIntent::Create {
                parent: "root".into(),
                name: "Local.txt".into(),
            },
            bytes,
        )
        .unwrap();
    (journal, row.id)
}
#[test]
fn verified_copy_is_private_and_does_not_change_or_replay_the_operation() {
    let temp = tempfile::tempdir().unwrap();
    for (n, bytes) in [b"".as_slice(), b"owned saved content".as_slice()]
        .into_iter()
        .enumerate()
    {
        let root = temp.path().join(n.to_string());
        fs::create_dir(&root).unwrap();
        let (journal, id) = setup(&root, bytes);
        let before = serde_json::to_value(journal.get(id).unwrap()).unwrap();
        let target = root.join("odd ' name\n.txt");
        let receipt = journal
            .local_export_source(id)
            .unwrap()
            .copy_to(&target, &CancellationToken::new(), |_| {})
            .unwrap();
        assert_eq!(receipt.operation, id);
        assert_eq!(receipt.size, bytes.len() as u64);
        assert_eq!(fs::read(&target).unwrap(), bytes);
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            serde_json::to_value(journal.get(id).unwrap()).unwrap(),
            before
        );
    }
}
#[test]
fn existing_targets_symlinks_parent_traversal_and_journal_destinations_are_refused() {
    let temp = tempfile::tempdir().unwrap();
    let (journal, id) = setup(temp.path(), b"saved");
    let target = temp.path().join("existing");
    fs::write(&target, b"keep").unwrap();
    let alias = temp.path().join("alias");
    symlink(&target, &alias).unwrap();
    let dangling = temp.path().join("dangling");
    symlink(temp.path().join("absent"), &dangling).unwrap();
    let directory = temp.path().join("directory");
    fs::create_dir(&directory).unwrap();
    let parent_alias = temp.path().join("parent-alias");
    symlink(&directory, &parent_alias).unwrap();
    for path in [
        target.clone(),
        alias,
        dangling,
        parent_alias.join("out"),
        directory.join("../out"),
        temp.path().join("journal/out"),
    ] {
        assert!(
            journal
                .local_export_source(id)
                .unwrap()
                .copy_to(&path, &CancellationToken::new(), |_| {})
                .is_err()
        );
    }
    assert_eq!(fs::read(&target).unwrap(), b"keep");
    assert!(!directory.join("out").exists());
    assert!(!temp.path().join("out").exists());
}
#[test]
fn cancellation_corruption_and_destination_races_publish_no_partial_copy() {
    let temp = tempfile::tempdir().unwrap();
    let (journal, id) = setup(temp.path(), b"saved bytes");
    let target = temp.path().join("export");
    let cancel = CancellationToken::new();
    assert!(
        journal
            .local_export_source(id)
            .unwrap()
            .copy_to(&target, &cancel, |_| cancel.cancel())
            .is_err()
    );
    assert!(!target.exists());
    assert!(
        journal
            .local_export_source(id)
            .unwrap()
            .copy_to(&target, &CancellationToken::new(), |_| {
                fs::write(&target, b"other").unwrap();
            })
            .is_err()
    );
    assert_eq!(fs::read(&target).unwrap(), b"other");
    let source = journal.local_export_source(id).unwrap();
    fs::set_permissions(
        temp.path().join("journal/objects").join(id.to_string()),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    fs::write(
        temp.path().join("journal/objects").join(id.to_string()),
        b"wrong bytes",
    )
    .unwrap();
    let corrupt = temp.path().join("corrupt");
    assert!(
        source
            .copy_to(&corrupt, &CancellationToken::new(), |_| {})
            .is_err()
    );
    assert!(!corrupt.exists());
    assert!(!fs::read_dir(temp.path()).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".cirrove-export-")
    }));
}
#[test]
fn a_pinned_source_survives_path_replacement_but_refuses_a_renamed_destination() {
    let temp = tempfile::tempdir().unwrap();
    let (journal, id) = setup(temp.path(), b"saved");
    let source = journal.local_export_source(id).unwrap();
    let path = temp.path().join("journal/objects").join(id.to_string());
    fs::rename(&path, path.with_extension("retained")).unwrap();
    fs::write(&path, b"wrong").unwrap();
    let directory = temp.path().join("output");
    fs::create_dir(&directory).unwrap();
    let moved = temp.path().join("moved");
    let other = temp.path().join("other");
    fs::create_dir(&other).unwrap();
    assert!(
        source
            .copy_to(&directory.join("copy"), &CancellationToken::new(), |_| {
                fs::rename(&directory, &moved).unwrap();
                symlink(&other, &directory).unwrap();
            })
            .is_err()
    );
    assert!(!moved.join("copy").exists());
    assert!(!other.join("copy").exists());
}

#[test]
fn an_export_reads_its_pinned_generation_even_if_the_source_path_is_replaced() {
    let temp = tempfile::tempdir().unwrap();
    let (journal, id) = setup(temp.path(), b"original");
    let source = journal.local_export_source(id).unwrap();
    let path = temp.path().join("journal/objects").join(id.to_string());
    fs::rename(&path, path.with_extension("retained")).unwrap();
    fs::write(&path, b"new data").unwrap();
    let target = temp.path().join("exported");
    source
        .copy_to(&target, &CancellationToken::new(), |_| {})
        .unwrap();
    assert_eq!(fs::read(target).unwrap(), b"original");
}

#[test]
fn offline_reader_locks_without_recovering_or_creating_a_journal() {
    use cirrove_service::journal::{RecoveryJournal, UploadState};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    assert!(RecoveryJournal::open(&root, "owned").is_err());
    assert!(!root.exists());
    let (journal, id) = setup(temp.path(), b"retained version");
    assert!(RecoveryJournal::open(&root, "owned").is_err());
    drop(journal);
    // Persist an interrupted state without running the ordinary reopen recovery.
    let db = rusqlite::Connection::open(root.join("uploads.db")).unwrap();
    db.execute("UPDATE uploads SET state='uploading',body=json_set(body,'$.state','uploading') WHERE id=?1", [id.to_string()]).unwrap();
    drop(db);
    let before = fs::read(root.join("uploads.db")).unwrap();
    assert!(matches!(
        RecoveryJournal::open(&root, "different-account"),
        Err(cirrove_service::journal::JournalError::Account)
    ));
    let recovery = RecoveryJournal::open(&root, "owned").unwrap();
    assert!(UploadJournal::open(&root, "owned", 1024).is_err());
    let rows = recovery.list(0, 999).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state, UploadState::Uploading);
    recovery
        .local_export_source(id)
        .unwrap()
        .copy_to(&temp.path().join("copy"), &CancellationToken::new(), |_| {})
        .unwrap();
    assert_eq!(
        recovery.list(0, 200).unwrap()[0].state,
        UploadState::Uploading
    );
    drop(recovery);
    assert_eq!(fs::read(root.join("uploads.db")).unwrap(), before);
    assert_eq!(
        fs::read(temp.path().join("copy")).unwrap(),
        b"retained version"
    );
    let alias = temp.path().join("alias");
    symlink(&root, &alias).unwrap();
    assert!(RecoveryJournal::open(&alias, "owned").is_err());
}

#[test]
fn offline_reader_refuses_future_schema_and_symlinked_database_or_objects() {
    use cirrove_service::journal::RecoveryJournal;
    let temp = tempfile::tempdir().unwrap();
    let (journal, _) = setup(temp.path(), b"retained");
    drop(journal);
    let root = temp.path().join("journal");
    let db_path = root.join("uploads.db");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    db.pragma_update(None, "user_version", 21).unwrap();
    drop(db);
    let before = fs::read(&db_path).unwrap();
    assert!(RecoveryJournal::open(&root, "owned").is_err());
    assert_eq!(fs::read(&db_path).unwrap(), before);
    let db = rusqlite::Connection::open(&db_path).unwrap();
    db.pragma_update(None, "user_version", 14).unwrap();
    drop(db);
    let moved = root.join("retained-db");
    fs::rename(&db_path, &moved).unwrap();
    symlink(&moved, &db_path).unwrap();
    assert!(RecoveryJournal::open(&root, "owned").is_err());
    fs::remove_file(&db_path).unwrap();
    fs::rename(moved, &db_path).unwrap();
    let objects = root.join("objects");
    let retained = root.join("retained-objects");
    fs::rename(&objects, &retained).unwrap();
    symlink(retained, objects).unwrap();
    assert!(RecoveryJournal::open(&root, "owned").is_err());
}
