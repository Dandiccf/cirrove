#![allow(clippy::unwrap_used)]
//! Synthetic retained journals only; no provider, Engine or original account state.
use super::*;
use cirrove_core::mutation::MutationRequest;

fn request(native: bool, item: &str) -> MutationRequest {
    let before = Node {
        id: item.into(),
        parent_id: Some("owned-parent".into()),
        name: if native {
            "Original.numbers"
        } else {
            "ordinary.txt"
        }
        .into(),
        kind: if native {
            NodeKind::Folder
        } else {
            NodeKind::File
        },
        package: native,
        size: 0,
        modified_unix: 0,
        etag: Some("original-revision".into()),
        content_version: None,
        target: None,
    };
    MutationRequest {
        scope: Scope {
            account: "owned".into(),
            provider: "icloud".into(),
            collection: "drive".into(),
        },
        intent: if native {
            MutationIntent::TrashNativeDocument { before }
        } else {
            MutationIntent::RemoveFile { before }
        },
    }
}

fn fixture() -> (PathBuf, UploadJournal, MutationRecord) {
    // Retain fixtures on the root runner's disk TMPDIR, including failing arms.
    let root = tempfile::Builder::new()
        .prefix("native-trash-readonly-")
        .tempdir()
        .unwrap()
        .keep();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.join("journal");
    let mut journal = UploadJournal::open(&path, "owned", 1024 * 1024).unwrap();
    let queued = journal.enqueue_mutation(request(true, "original")).unwrap();
    let claimed = journal.claim_mutation().unwrap().unwrap();
    assert_eq!(claimed.id, queued.id);
    journal
        .acknowledge_mutation(
            queued.id,
            claimed.attempt.unwrap(),
            MutationReceipt::Removed {
                item: "original".into(),
            },
        )
        .unwrap();
    let record = journal.mutation(queued.id).unwrap();
    assert_eq!(record.state, MutationState::Applied);
    assert_eq!(
        journal.native_trash_publication_due(0).unwrap().unwrap().id,
        record.id
    );
    (path, journal, record)
}

fn rows(db: &Connection) -> Vec<Vec<String>> {
    [
        "SELECT json_array(sequence,id,state,body) FROM mutations ORDER BY sequence",
        "SELECT json_array(sequence,id,complete) FROM write_queue ORDER BY sequence",
        "SELECT json_array(id,resource) FROM write_resources ORDER BY id,resource",
        "SELECT json_array(sequence,id,state,body) FROM uploads ORDER BY sequence",
        "SELECT json_array(operation,done,failures,retry_after,observed) FROM native_trash_metadata_publication ORDER BY operation",
    ]
    .into_iter()
    .map(|sql| {
        db.prepare(sql).unwrap().query_map([], |row| row.get::<_, String>(0))
            .unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap()
    })
    .collect()
}

fn protected_rows(db: &Connection) -> Vec<Vec<String>> {
    let mut protected = rows(db);
    protected.pop();
    protected.push(
        db.prepare("SELECT json_array(operation,done,observed) FROM native_trash_metadata_publication ORDER BY operation")
            .unwrap().query_map([], |row| row.get::<_, String>(0)).unwrap()
            .collect::<rusqlite::Result<Vec<_>>>().unwrap(),
    );
    protected
}

fn publication_timers(db: &Connection) -> Vec<(String, i64, i64)> {
    db.prepare("SELECT operation,failures,retry_after FROM native_trash_metadata_publication ORDER BY operation")
        .unwrap().query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap()
}

fn refuse_without_changes(journal: &UploadJournal) {
    let before = protected_rows(&journal.db);
    let timers = publication_timers(&journal.db);
    let selected: String = journal.db.query_row(
        "SELECT p.operation FROM native_trash_metadata_publication p JOIN mutations m ON m.id=p.operation WHERE p.done=0 AND p.retry_after<=0 ORDER BY p.retry_after,p.operation LIMIT 1",
        [], |row| row.get(0),
    ).unwrap();
    let changes = journal.db.total_changes();
    let result = journal.native_trash_publication_due(0);
    // Only this selected publication job's bounded cooldown may change. Every
    // transfer/body/resource and publication operation/done/observation is fixed.
    // Baseline zero changes is allowed; it must still refuse corrupt authority.
    assert_eq!(protected_rows(&journal.db), before);
    let after = publication_timers(&journal.db);
    assert_eq!(after.len(), timers.len());
    let mut deferred = 0;
    for (old, new) in timers.iter().zip(&after) {
        assert_eq!(old.0, new.0);
        if old != new {
            assert_eq!(old.0, selected);
            assert_eq!(new.1, (old.1 + 1).min(6));
            assert!((1..=60).contains(&new.2));
            deferred += 1;
        }
    }
    assert!(deferred <= 1);
    assert_eq!(journal.db.total_changes() - changes, deferred);
    assert!(
        matches!(result, Err(JournalError::Corrupt)),
        "due must refuse corrupt authority before returning a job"
    );
}

#[test]
fn native_trash_due_requires_sql_operation_to_equal_body_id() {
    let (_, journal, mut record) = fixture();
    let selected = record.id;
    record.id = Uuid::new_v4();
    journal
        .db
        .execute(
            "UPDATE mutations SET body=?2 WHERE id=?1",
            params![
                selected.to_string(),
                serde_json::to_string(&record).unwrap()
            ],
        )
        .unwrap();
    refuse_without_changes(&journal);
}

#[test]
fn native_trash_due_requires_sql_sequence_to_equal_body_sequence() {
    let (_, journal, record) = fixture();
    journal
        .db
        .execute(
            "UPDATE mutations SET sequence=sequence+100 WHERE id=?1",
            [record.id.to_string()],
        )
        .unwrap();
    refuse_without_changes(&journal);
}

#[test]
fn native_trash_due_requires_sql_state_to_equal_applied_body_state() {
    let (_, journal, record) = fixture();
    journal
        .db
        .execute(
            "UPDATE mutations SET state='pending' WHERE id=?1",
            [record.id.to_string()],
        )
        .unwrap();
    refuse_without_changes(&journal);
}

#[test]
fn native_trash_due_requires_exact_journal_account() {
    let (_, journal, mut record) = fixture();
    record.request.scope.account = "foreign".into();
    journal
        .db
        .execute(
            "UPDATE mutations SET body=?2 WHERE id=?1",
            params![
                record.id.to_string(),
                serde_json::to_string(&record).unwrap()
            ],
        )
        .unwrap();
    refuse_without_changes(&journal);
}

#[test]
fn native_trash_due_rejects_invalid_applied_removal_shapes_without_changes() {
    for arm in 0..6 {
        let (_, journal, mut record) = fixture();
        match arm {
            0 => record.state = MutationState::Pending,
            1 => record.receipt = None,
            2 => {
                record.receipt = Some(MutationReceipt::Removed {
                    item: "foreign".into(),
                })
            }
            3 => {
                record.receipt = Some(MutationReceipt::Upsert(
                    record.request.intent.before().unwrap().clone(),
                ))
            }
            4 => record.request = request(false, "original"),
            5 => {
                let MutationIntent::TrashNativeDocument { before } = &mut record.request.intent
                else {
                    unreachable!()
                };
                before.etag = None;
            }
            _ => unreachable!(),
        }
        journal
            .db
            .execute(
                "UPDATE mutations SET body=?2 WHERE id=?1",
                params![
                    record.id.to_string(),
                    serde_json::to_string(&record).unwrap()
                ],
            )
            .unwrap();
        refuse_without_changes(&journal);
    }
}

#[test]
fn native_trash_pending_publication_survives_metadata_only_schema20_and21_reopen() {
    for version in [20, 21] {
        let (path, mut journal, record) = fixture();
        let ordinary = journal
            .enqueue_mutation(request(false, "ordinary"))
            .unwrap();
        let claimed = journal.claim_mutation().unwrap().unwrap();
        assert_eq!(claimed.id, ordinary.id);
        journal
            .acknowledge_mutation(
                ordinary.id,
                claimed.attempt.unwrap(),
                MutationReceipt::Removed {
                    item: "ordinary".into(),
                },
            )
            .unwrap();
        assert_eq!(
            journal.native_trash_publication_due(0).unwrap().unwrap().id,
            record.id
        );
        let pending = journal
            .enqueue_mutation(request(true, "pending-native"))
            .unwrap();
        assert_eq!(pending.state, MutationState::Pending);
        journal
            .db
            .pragma_update(None, "user_version", version)
            .unwrap();
        let before = rows(&journal.db);
        drop(journal);
        // Existing metadata-only API: no writable UploadJournal reopen or migration.
        let metadata = MetadataPublicationJournal::open(&path, "owned").unwrap();
        let db = Connection::open_with_flags(
            path.join("uploads.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .unwrap();
        assert_eq!(
            db.pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))
                .unwrap(),
            version
        );
        assert_eq!(rows(&db), before);
        let due: Vec<String> = db.prepare("SELECT operation FROM native_trash_metadata_publication WHERE done=0 AND retry_after<=0 ORDER BY operation").unwrap().query_map([], |row| row.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
        assert_eq!(due, vec![record.id.to_string()]);
        // Pending native mutation is retained, not falsely published or completed.
        let saved: MutationRecord = serde_json::from_str(
            &db.query_row::<String, _, _>(
                "SELECT body FROM mutations WHERE id=?1",
                [pending.id.to_string()],
                |row| row.get(0),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(saved.state, MutationState::Pending);
        assert!(saved.receipt.is_none());
        drop(db);
        drop(metadata);
    }
}

#[test]
fn native_trash_present_is_not_absent_and_done_stays_done_after_metadata_restart() {
    let (path, journal, record) = fixture();
    let mut restored = record.request.intent.before().unwrap().clone();
    restored.etag = Some("later-restored-revision".into());
    journal
        .finish_native_trash_publication(
            &record,
            PackagePublicationStatus::Present(restored.clone()),
            0,
        )
        .unwrap();
    assert_eq!(
        journal.native_trash_publication_status(record.id).unwrap(),
        PackagePublicationStatus::Present(restored.clone())
    );
    assert!(journal.native_trash_publication_due(0).unwrap().is_none());
    let before = rows(&journal.db);
    drop(journal);
    let metadata = MetadataPublicationJournal::open(&path, "owned").unwrap();
    let db = Connection::open_with_flags(
        path.join("uploads.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(rows(&db), before);
    drop(db);
    drop(metadata);
    let journal = UploadJournal::open(&path, "owned", 1024 * 1024).unwrap();
    assert_eq!(
        journal.native_trash_publication_status(record.id).unwrap(),
        PackagePublicationStatus::Present(restored)
    );
    assert_eq!(
        journal
            .native_trash_publication_due(61)
            .unwrap()
            .unwrap()
            .id,
        record.id
    );
    journal
        .finish_native_trash_publication(&record, PackagePublicationStatus::Absent, 61)
        .unwrap();
    let completed = rows(&journal.db);
    drop(journal);
    let metadata = MetadataPublicationJournal::open(&path, "owned").unwrap();
    let db = Connection::open_with_flags(
        path.join("uploads.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(rows(&db), completed);
    drop(db);
    drop(metadata);
    let journal = UploadJournal::open(&path, "owned", 1024 * 1024).unwrap();
    assert!(
        journal
            .native_trash_publication_due(1000)
            .unwrap()
            .is_none()
    );
    let before = rows(&journal.db);
    assert!(matches!(
        journal.finish_native_trash_publication(
            &record,
            PackagePublicationStatus::Present(record.request.intent.before().unwrap().clone()),
            1000
        ),
        Err(JournalError::Stale)
    ));
    assert_eq!(rows(&journal.db), before);
    assert_eq!(
        journal.native_trash_publication_status(record.id).unwrap(),
        PackagePublicationStatus::Absent
    );
}

fn readonly_db(path: &Path) -> Connection {
    Connection::open_with_flags(
        path.join("uploads.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .unwrap()
}

fn transfer_rows(db: &Connection) -> Vec<Vec<String>> {
    [
        "SELECT json_array(sequence,id,state,body) FROM mutations ORDER BY sequence",
        "SELECT json_array(sequence,id,complete) FROM write_queue ORDER BY sequence",
        "SELECT json_array(id,resource) FROM write_resources ORDER BY id,resource",
        "SELECT json_array(sequence,id,state,body) FROM uploads ORDER BY sequence",
    ]
    .into_iter()
    .map(|sql| {
        db.prepare(sql)
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    })
    .collect()
}

#[test]
fn native_trash_readonly_metadata_due_and_finish_preserve_schema20_and21_transfers_and_source() {
    for version in [20, 21] {
        let (path, journal, record) = fixture();
        let source = path.parent().unwrap().join("retained-source.numbers");
        let original = b"synthetic retained local source, never a provider archive";
        std::fs::write(&source, original).unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o400)).unwrap();
        journal
            .db
            .pragma_update(None, "user_version", version)
            .unwrap();
        let transfers = transfer_rows(&journal.db);
        let owner = std::fs::read(path.join("owner.lock")).unwrap();
        drop(journal);
        let metadata = MetadataPublicationJournal::open(&path, "owned").unwrap();
        assert!(matches!(
            UploadJournal::open(&path, "owned", 1024 * 1024),
            Err(JournalError::Busy)
        ));
        let selected = metadata.due_native_trash(0).unwrap().unwrap();
        assert_eq!(
            serde_json::to_value(&selected).unwrap(),
            serde_json::to_value(&record).unwrap()
        );
        let mut later = selected.request.intent.before().unwrap().clone();
        later.etag = Some("later-active-revision".into());
        metadata
            .finish_native_trash(
                &selected,
                PackagePublicationStatus::Present(later.clone()),
                0,
            )
            .unwrap();
        let db = readonly_db(&path);
        let (done, observed): (bool, String) = db
            .query_row(
                "SELECT done,observed FROM native_trash_metadata_publication WHERE operation=?1",
                [record.id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(!done);
        assert_eq!(serde_json::from_str::<Node>(&observed).unwrap(), later);
        assert!(metadata.due_native_trash(0).unwrap().is_none());
        assert_eq!(
            metadata.due_native_trash(61).unwrap().unwrap().id,
            record.id
        );
        metadata
            .finish_native_trash(&selected, PackagePublicationStatus::Absent, 61)
            .unwrap();
        assert!(metadata.due_native_trash(1000).unwrap().is_none());
        assert!(matches!(
            metadata.finish_native_trash(&selected, PackagePublicationStatus::Present(later), 1000),
            Err(JournalError::Stale)
        ));
        assert_eq!(transfer_rows(&db), transfers);
        assert_eq!(
            db.pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))
                .unwrap(),
            version
        );
        assert_eq!(std::fs::read(&source).unwrap(), original);
        assert_eq!(
            std::fs::metadata(&source).unwrap().permissions().mode() & 0o777,
            0o400
        );
        assert_eq!(std::fs::read(path.join("owner.lock")).unwrap(), owner);
        drop(db);
        drop(metadata);
        let reopened = MetadataPublicationJournal::open(&path, "owned").unwrap();
        assert!(reopened.due_native_trash(1000).unwrap().is_none());
        let db = readonly_db(&path);
        assert_eq!(transfer_rows(&db), transfers);
        assert_eq!(
            db.pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))
                .unwrap(),
            version
        );
    }
}

#[test]
fn native_trash_readonly_metadata_missing_table_is_no_work_without_ddl() {
    for version in [20, 21] {
        let (path, journal, _) = fixture();
        // Only this owned synthetic fixture loses its additive publication schema.
        journal.db.execute_batch("DROP TRIGGER native_trash_metadata_on_update; DROP TRIGGER native_trash_metadata_on_insert; DROP TABLE native_trash_metadata_publication;").unwrap();
        journal
            .db
            .pragma_update(None, "user_version", version)
            .unwrap();
        let transfers = transfer_rows(&journal.db);
        let schema_sql =
            "SELECT json_array(type,name,tbl_name,sql) FROM sqlite_master ORDER BY type,name";
        let schema: Vec<String> = journal
            .db
            .prepare(schema_sql)
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let owner = std::fs::read(path.join("owner.lock")).unwrap();
        drop(journal);
        let metadata = MetadataPublicationJournal::open(&path, "owned").unwrap();
        assert!(metadata.due_native_trash(0).unwrap().is_none());
        assert!(metadata.due_native_trash(1000).unwrap().is_none());
        let db = readonly_db(&path);
        let after: Vec<String> = db
            .prepare(schema_sql)
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(after, schema);
        assert_eq!(transfer_rows(&db), transfers);
        assert_eq!(
            db.pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))
                .unwrap(),
            version
        );
        assert_eq!(std::fs::read(path.join("owner.lock")).unwrap(), owner);
    }
}

#[test]
fn native_trash_readonly_metadata_rejects_schema14_through19_without_migration() {
    for version in 14..=19 {
        let (path, journal, _) = fixture();
        // A synthetic unsupported header, not an authentic older producer image.
        journal
            .db
            .pragma_update(None, "user_version", version)
            .unwrap();
        let before = rows(&journal.db);
        let owner = std::fs::read(path.join("owner.lock")).unwrap();
        drop(journal);
        assert!(matches!(
            MetadataPublicationJournal::open(&path, "owned"),
            Err(JournalError::Schema)
        ));
        let db = readonly_db(&path);
        assert_eq!(
            db.pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))
                .unwrap(),
            version
        );
        assert_eq!(rows(&db), before);
        assert_eq!(std::fs::read(path.join("owner.lock")).unwrap(), owner);
    }
}

#[test]
fn native_trash_readonly_metadata_corrupt_head_cooldown_allows_valid_sibling() {
    let (path, mut journal, mut corrupt) = fixture();
    let sibling = journal
        .enqueue_mutation(request(true, "sibling-native"))
        .unwrap();
    let claimed = journal.claim_mutation().unwrap().unwrap();
    assert_eq!(claimed.id, sibling.id);
    journal
        .acknowledge_mutation(
            sibling.id,
            claimed.attempt.unwrap(),
            MutationReceipt::Removed {
                item: "sibling-native".into(),
            },
        )
        .unwrap();
    let valid = journal.mutation(sibling.id).unwrap();
    let selected = corrupt.id;
    corrupt.id = Uuid::new_v4();
    journal
        .db
        .execute(
            "UPDATE mutations SET body=?2 WHERE id=?1",
            params![
                selected.to_string(),
                serde_json::to_string(&corrupt).unwrap()
            ],
        )
        .unwrap();
    journal
        .db
        .execute(
            "UPDATE native_trash_metadata_publication SET retry_after=1 WHERE operation=?1",
            [sibling.id.to_string()],
        )
        .unwrap();
    let before = protected_rows(&journal.db);
    let owner = std::fs::read(path.join("owner.lock")).unwrap();
    drop(journal);
    let metadata = MetadataPublicationJournal::open(&path, "owned").unwrap();
    assert!(matches!(
        metadata.due_native_trash(1),
        Err(JournalError::Corrupt)
    ));
    let db = readonly_db(&path);
    assert_eq!(protected_rows(&db), before);
    let deferred: (i64, i64) = db
        .query_row(
            "SELECT failures,retry_after FROM native_trash_metadata_publication WHERE operation=?1",
            [selected.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(deferred, (1, 3));
    let next = metadata.due_native_trash(1).unwrap().unwrap();
    assert_eq!(
        serde_json::to_value(&next).unwrap(),
        serde_json::to_value(&valid).unwrap()
    );
    assert_eq!(protected_rows(&db), before);
    metadata
        .finish_native_trash(&next, PackagePublicationStatus::Absent, 1)
        .unwrap();
    assert!(metadata.due_native_trash(1).unwrap().is_none());
    assert_eq!(transfer_rows(&db), before[..4]);
    assert_eq!(std::fs::read(path.join("owner.lock")).unwrap(), owner);
}

#[test]
fn native_trash_readonly_metadata_finish_refuses_changed_body_and_queue_authority() {
    for arm in 0..4 {
        let (path, journal, record) = fixture();
        drop(journal);
        let metadata = MetadataPublicationJournal::open(&path, "owned").unwrap();
        let selected = metadata.due_native_trash(0).unwrap().unwrap();
        let db = Connection::open_with_flags(
            path.join("uploads.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .unwrap();
        // Deliberate local corruption after selection, never a provider callback.
        match arm {
            0 => {
                let mut changed = selected.clone();
                changed.retry_at += 1;
                db.execute(
                    "UPDATE mutations SET body=?2 WHERE id=?1",
                    params![
                        record.id.to_string(),
                        serde_json::to_string(&changed).unwrap()
                    ],
                )
                .unwrap();
            }
            1 => {
                db.execute(
                    "UPDATE write_queue SET complete=0 WHERE id=?1",
                    [record.id.to_string()],
                )
                .unwrap();
            }
            2 => {
                db.execute(
                    "UPDATE write_queue SET sequence=sequence+100 WHERE id=?1",
                    [record.id.to_string()],
                )
                .unwrap();
            }
            3 => {
                db.execute(
                    "DELETE FROM write_queue WHERE id=?1",
                    [record.id.to_string()],
                )
                .unwrap();
            }
            _ => unreachable!(),
        }
        let before = rows(&db);
        let changes = db.total_changes();
        let result = metadata.finish_native_trash(&selected, PackagePublicationStatus::Absent, 0);
        assert_eq!(rows(&db), before);
        assert_eq!(db.total_changes(), changes);
        assert!(matches!(
            result,
            Err(JournalError::Stale | JournalError::Corrupt)
        ));
    }
}
