#![allow(clippy::unwrap_used)]
use super::*;
use cirrove_core::{Checkpoint, NodeKind};
use rusqlite::StatementStatus;

#[cfg(target_os = "linux")]
mod capacity;

fn scope() -> Scope {
    Scope {
        provider: "fixture".into(),
        account: "account".into(),
        collection: "drive".into(),
    }
}
fn node(n: usize) -> Node {
    Node {
        id: format!("item-{n:06}"),
        parent_id: Some("root".into()),
        name: format!("{}-{}", ["Äpfel", "a", "ä", "ß", "Z"][n % 5], n % 37),
        kind: NodeKind::File,
        size: n as u64,
        modified_unix: 7,
        etag: Some("one".into()),
        content_version: None,
        target: None,
    }
}
fn seed(db: &mut Store, nodes: &[Node]) {
    let scope = scope();
    let cursor = db.begin(&scope, true).unwrap();
    db.stage(
        &scope,
        cursor.as_ref(),
        &ChangePage {
            changes: nodes.iter().cloned().map(Change::Upsert).collect(),
            checkpoint: Checkpoint::Complete(Cursor("done".into())),
        },
    )
    .unwrap();
}
fn legacy_directory_format(db: &Connection) {
    // Restore the actual pre-v5 columns and array representation for migration
    // tests; changing user_version alone would leave an impossible old schema.
    db.execute_batch("ALTER TABLE directories ADD COLUMN body TEXT NOT NULL DEFAULT '[]';
        UPDATE directories SET body=(SELECT json_group_array(json(e.body)) FROM directory_entries e WHERE e.scope=directories.scope AND e.parent=directories.parent);
        DROP TABLE directory_entries; DROP INDEX node_parent_name; DROP INDEX observed_parent_name;
        PRAGMA user_version=4;").unwrap();
}

fn legacy_observation_format(db: &Connection) {
    db.execute_batch(
        "DROP TABLE metadata_versions; DROP TABLE metadata_clock;
        DROP TABLE observed_absent; DROP INDEX observed_parent;
        ALTER TABLE observed DROP COLUMN source_revision;
        ALTER TABLE directories DROP COLUMN source_revision;
        PRAGMA user_version=3;",
    )
    .unwrap();
}

fn schema(db: &Connection) -> Vec<(String, String, Option<String>)> {
    db.prepare("SELECT type,name,sql FROM sqlite_master ORDER BY type,name")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap()
}

#[test]
fn concurrent_open_initializes_or_migrates_once_without_losing_metadata() {
    for version in [0, 3, 4] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("metadata.db");
        let expected = if version == 0 {
            None
        } else {
            let mut db = Store::open(&path).unwrap();
            let nodes = (0..100).map(node).collect::<Vec<_>>();
            seed(&mut db, &nodes);
            db.observe_directory(&scope(), "root", &nodes).unwrap();
            let expected = db.children(&scope(), "root").unwrap();
            legacy_directory_format(&db.db);
            if version == 3 {
                legacy_observation_format(&db.db);
            }
            expected
        };
        let barrier = std::sync::Barrier::new(8);
        let inodes = std::thread::scope(|threads| {
            let handles = (0..8)
                .map(|_| {
                    threads.spawn(|| {
                        barrier.wait();
                        let mut db = Store::open(&path).unwrap();
                        assert_eq!(db.children(&scope(), "root").unwrap(), expected);
                        assert_eq!(
                            db.cursor(&scope()).unwrap(),
                            (version != 0).then(|| Cursor("done".into()))
                        );
                        assert_eq!(
                            db.db
                                .pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
                                .unwrap(),
                            5
                        );
                        db.inode("same-identity").unwrap()
                    })
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|t| t.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert!(inodes.iter().all(|inode| *inode == inodes[0]));
        let mut db = Store::open(&path).unwrap();
        assert_ne!(db.inode("another-identity").unwrap(), inodes[0]);
    }
}

#[test]
fn ordered_stream_uses_indexes_without_a_full_directory_sort() {
    let mut db = Store::open(":memory:").unwrap();
    let nodes = (0..2048).map(node).collect::<Vec<_>>();
    seed(&mut db, &nodes);
    let scope = scope();
    let key = Store::key(&scope).unwrap();
    for snapshot in [false, true] {
        if snapshot {
            db.observe_directory(&scope, "root", &nodes).unwrap();
        }
        let mut changed = nodes[1100].clone();
        changed.name = "changed".into();
        db.observe_node(&scope, &changed).unwrap();
        let mut expected = nodes.clone();
        expected[1100] = changed;
        expected.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        let mut actual = Vec::new();
        assert!(
            db.visit_children(&scope, "root", |n| {
                actual.push(n);
                Ok(())
            })
            .unwrap()
        );
        assert_eq!(actual, expected);
        let mut statement = db
            .db
            .prepare(if snapshot {
                SNAPSHOT_CHILDREN
            } else {
                INDEXED_CHILDREN
            })
            .unwrap();
        {
            let mut rows = if snapshot {
                let revision: i64 = db
                    .db
                    .query_row(
                        "SELECT source_revision FROM directories WHERE scope=?1 AND parent='root'",
                        [&key],
                        |r| r.get(0),
                    )
                    .unwrap();
                statement.query(params![key, "root", revision]).unwrap()
            } else {
                statement.query(params![key, "root"]).unwrap()
            };
            let mut count = 0;
            while rows.next().unwrap().is_some() {
                count += 1;
            }
            assert_eq!(count, nodes.len());
        }
        assert_eq!(
            statement.get_status(StatementStatus::Sort),
            0,
            "snapshot={snapshot}: ordered directory required a SQL sort"
        );
        assert_eq!(
            statement.get_status(StatementStatus::FullscanStep),
            0,
            "snapshot={snapshot}: directory read scanned unrelated rows"
        );
    }
}

#[test]
fn read_snapshot_allows_other_writers_and_cancellation_releases_its_transaction() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("metadata.db");
    let mut writer = Store::open(&path).unwrap();
    let nodes = (0..100).map(node).collect::<Vec<_>>();
    writer.observe_directory(&scope(), "root", &nodes).unwrap();
    let reader = Store::open(&path).unwrap();
    writer.db.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut count = 0;
    assert!(
        reader
            .visit_children(&scope(), "root", |_| {
                count += 1;
                Ok(())
            })
            .unwrap()
    );
    assert_eq!(count, 100);
    writer.db.execute_batch("ROLLBACK").unwrap();
    let before = reader.children(&scope(), "root").unwrap().unwrap();
    let mut streamed = Vec::new();
    reader
        .visit_children(&scope(), "root", |n| {
            if streamed.is_empty() {
                let mut moved = nodes[99].clone();
                moved.parent_id = Some("elsewhere".into());
                writer.observe_node(&scope(), &moved)?;
            }
            streamed.push(n);
            Ok(())
        })
        .unwrap();
    assert_eq!(streamed, before, "one traversal mixed metadata revisions");
    assert_eq!(
        reader.children(&scope(), "root").unwrap().unwrap().len(),
        99
    );
    let mut visited = 0;
    assert!(matches!(
        reader.visit_children(&scope(), "root", |_| {
            visited += 1;
            Err(StoreError::OutOfOrder)
        }),
        Err(StoreError::OutOfOrder)
    ));
    assert_eq!(visited, 1);
    assert_eq!(
        reader.children(&scope(), "root").unwrap().unwrap().len(),
        99
    );
    let unknown = Store::open(":memory:").unwrap();
    assert!(
        !unknown
            .visit_children(&scope(), "unknown", |_| panic!(
                "unknown directory yielded entries"
            ))
            .unwrap()
    );
    writer.observe_directory(&scope(), "empty", &[]).unwrap();
    assert!(
        reader
            .visit_children(&scope(), "empty", |_| panic!(
                "empty directory yielded entries"
            ))
            .unwrap()
    );
}

#[test]
fn migration_preserves_observation_order_and_tickets_and_delta_cascades_rows() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("metadata.db");
    let mut db = Store::open(&path).unwrap();
    let nodes = (0..3).map(node).collect::<Vec<_>>();
    seed(&mut db, &nodes);
    db.observe_directory(&scope(), "root", &nodes).unwrap();
    let mut changed = nodes[1].clone();
    changed.name = "new name".into();
    db.observe_node(&scope(), &changed).unwrap();
    let expected = db.children(&scope(), "root").unwrap().unwrap();
    let ticket = db.directory_observation(&scope(), "root").unwrap();
    legacy_directory_format(&db.db);
    drop(db);
    let mut db = Store::open(&path).unwrap();
    assert_eq!(db.children(&scope(), "root").unwrap().unwrap(), expected);
    assert_eq!(db.cursor(&scope()).unwrap(), Some(Cursor("done".into())));
    assert!(matches!(
        db.publish_directory(&ticket, &expected).unwrap(),
        ObservationResult::Published { changed: false, .. }
    ));
    let before = db.children(&scope(), "root").unwrap();
    assert!(
        db.observe_directory(&scope(), "root", &[nodes[0].clone(), nodes[0].clone()])
            .is_err()
    );
    assert_eq!(
        db.children(&scope(), "root").unwrap(),
        before,
        "failed row publication changed visible metadata"
    );
    seed(&mut db, &[]);
    assert_eq!(
        db.db
            .query_row("SELECT count(*) FROM directory_entries", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(db.children(&scope(), "root").unwrap().unwrap().is_empty());
}

#[test]
fn migration_validation_failure_keeps_the_original_schema() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("metadata.db");
    let mut db = Store::open(&path).unwrap();
    db.observe_directory(&scope(), "root", &[node(0)]).unwrap();
    legacy_directory_format(&db.db);
    db.db.execute("DELETE FROM metadata_clock", []).unwrap();
    let before = schema(&db.db);
    drop(db);
    assert!(Store::open(&path).is_err());
    let db = Connection::open(&path).unwrap();
    assert_eq!(
        schema(&db),
        before,
        "validation committed an unusable migration"
    );
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        4
    );
    db.execute("INSERT INTO metadata_clock VALUES(1,'repaired',1)", [])
        .unwrap();
    drop(db);
    assert_eq!(
        Store::open(&path)
            .unwrap()
            .children(&scope(), "root")
            .unwrap()
            .unwrap(),
        vec![node(0)]
    );
}

#[test]
fn invalid_legacy_snapshots_roll_back_without_losing_old_data() {
    for version in [3, 4] {
        for body in [
            "not json".to_owned(),
            "{}".to_owned(),
            serde_json::to_string(&[node(0), node(0)]).unwrap(),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("metadata.db");
            let mut db = Store::open(&path).unwrap();
            db.observe_directory(&scope(), "root", &[node(0)]).unwrap();
            legacy_directory_format(&db.db);
            if version == 3 {
                legacy_observation_format(&db.db);
            }
            db.db
                .execute("UPDATE directories SET body=?1", [&body])
                .unwrap();
            let before_schema = schema(&db.db);
            drop(db);
            assert!(Store::open(&path).is_err());
            let db = Connection::open(&path).unwrap();
            assert_eq!(
                db.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
                    .unwrap(),
                version
            );
            assert_eq!(
                schema(&db),
                before_schema,
                "failed migration left partial schema steps"
            );
            assert_eq!(
                db.query_row("SELECT body FROM directories", [], |r| r
                    .get::<_, String>(0))
                    .unwrap(),
                body
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name='directory_entries'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                0
            );
            db.execute(
                "UPDATE directories SET body=?1",
                [serde_json::to_string(&[node(0)]).unwrap()],
            )
            .unwrap();
            drop(db);
            assert_eq!(
                Store::open(&path)
                    .unwrap()
                    .children(&scope(), "root")
                    .unwrap()
                    .unwrap(),
                vec![node(0)]
            );
        }
    }
}

#[test]
fn initial_wal_handles_short_contention_but_preserves_a_busy_deadline() {
    use std::time::{Duration, Instant};
    for persistent in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("metadata.db");
        let holder = Connection::open(&path).unwrap();
        holder.execute_batch("CREATE TABLE existing(value INTEGER); INSERT INTO existing VALUES(42); BEGIN; SELECT * FROM existing;").unwrap();
        let candidate = Connection::open(&path).unwrap();
        let release = if persistent {
            None
        } else {
            Some(std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(150));
                holder.execute_batch("ROLLBACK").unwrap();
            }))
        };
        let started = Instant::now();
        let result = initial_wal(&candidate);
        if persistent {
            assert_eq!(
                result.unwrap_err().sqlite_error_code(),
                Some(rusqlite::ErrorCode::DatabaseBusy)
            );
            assert!(started.elapsed() >= Duration::from_secs(2));
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "WAL admission exceeded its total deadline"
            );
        } else {
            result.unwrap();
            release.unwrap().join().unwrap();
        }
        assert!(
            candidate.is_autocommit(),
            "journal admission retained a transaction"
        );
        assert_eq!(
            candidate
                .pragma_query_value(None, "busy_timeout", |row| row.get::<_, i64>(0))
                .unwrap(),
            3000
        );
        assert_eq!(
            candidate
                .query_row("SELECT value FROM existing", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            42
        );
    }
}
