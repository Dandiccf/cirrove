//! Bounded reads of coalesced, committed metadata revision marks.
use super::*;
const PAGE_ENTRIES: usize = 256;
const PAGE_BYTES: usize = 64 * 1024;
const MAX_MARK_BYTES: usize = 1024 * 1024;
const FIRST: &str =
    "SELECT revision,scope,kind,identity FROM metadata_versions INDEXED BY metadata_change_order
    WHERE revision>?1 AND revision<=?2 ORDER BY revision,scope,kind,identity LIMIT 256";
const NEXT: &str =
    "SELECT revision,scope,kind,identity FROM metadata_versions INDEXED BY metadata_change_order
    WHERE (revision,scope,kind,identity)>(?1,?2,?3,?4) AND revision<=?5
    ORDER BY revision,scope,kind,identity LIMIT 256";

#[derive(Clone, Debug)]
pub struct MetadataPosition {
    database: String,
    through: i64,
    upper: Option<i64>,
    after: Option<(i64, String, u8, String)>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MetadataChangeKind {
    Scope,
    Item,
    Directory,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataChange {
    pub scope: Scope,
    pub kind: MetadataChangeKind,
    pub identity: String,
}
pub struct MetadataChanges {
    pub changes: Vec<MetadataChange>,
    /// Advance only after all returned changes have been handled.
    pub next: MetadataPosition,
    pub complete: bool,
}
pub(super) fn migrate(tx: &rusqlite::Transaction<'_>, version: u32) -> Result<()> {
    if version < 6 {
        tx.execute_batch("CREATE INDEX IF NOT EXISTS metadata_change_order ON metadata_versions(revision,scope,kind,identity);")?;
    }
    Ok(())
}
pub(super) fn validate(db: &Connection) -> Result<()> {
    db.prepare(FIRST)?;
    Ok(())
}
impl Store {
    /// Starting point for a mount's invalidation reader. This is not a provider
    /// cursor. If the database identity changes, perform a full invalidation.
    pub fn metadata_position(&self) -> Result<MetadataPosition> {
        let (database, through) = observations::clock(&self.db)?;
        Ok(MetadataPosition {
            database,
            through,
            upper: None,
            after: None,
        })
    }
    /// Read at most 256 marks, with a 64 KiB batch budget (one larger mark, up to
    /// 1 MiB, may occupy a page alone). Each call releases its SQLite snapshot.
    /// Replaced marks beyond the captured upper revision are read next round;
    /// scope resets cannot disappear because their newer scope mark replaces them.
    pub fn metadata_changes(&self, position: &MetadataPosition) -> Result<MetadataChanges> {
        let tx = self.db.unchecked_transaction()?;
        let (database, revision) = observations::clock(&tx)?;
        if database != position.database || revision < position.through {
            return Err(StoreError::OutOfOrder);
        }
        let upper = position.upper.unwrap_or(revision);
        let mut stmt = tx.prepare(if position.after.is_some() {
            NEXT
        } else {
            FIRST
        })?;
        let mut rows = if let Some((rev, scope, kind, identity)) = &position.after {
            stmt.query(params![rev, scope, kind, identity, upper])?
        } else {
            stmt.query(params![position.through, upper])?
        };
        let mut next = position.clone();
        next.upper = Some(upper);
        let mut changes = Vec::new();
        let mut bytes = 0;
        let mut complete = true;
        while let Some(row) = rows.next()? {
            let scope = row
                .get_ref(1)?
                .as_str()
                .map_err(|_| StoreError::OutOfOrder)?;
            let identity = row
                .get_ref(3)?
                .as_str()
                .map_err(|_| StoreError::OutOfOrder)?;
            let size = scope.len() + identity.len();
            if size > MAX_MARK_BYTES {
                return Err(StoreError::OutOfOrder);
            }
            if !changes.is_empty() && bytes + size > PAGE_BYTES {
                complete = false;
                break;
            }
            let kind: u8 = row.get(2)?;
            changes.push(MetadataChange {
                scope: serde_json::from_str(scope)?,
                identity: identity.into(),
                kind: match kind {
                    0 => MetadataChangeKind::Scope,
                    1 => MetadataChangeKind::Item,
                    2 => MetadataChangeKind::Directory,
                    _ => return Err(StoreError::OutOfOrder),
                },
            });
            next.after = Some((row.get(0)?, scope.into(), kind, identity.into()));
            bytes += size;
            if changes.len() == PAGE_ENTRIES {
                complete = false;
                break;
            }
        }
        drop(rows);
        drop(stmt);
        if complete {
            next.through = upper;
            next.upper = None;
            next.after = None;
        }
        tx.commit()?;
        Ok(MetadataChanges {
            changes,
            next,
            complete,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    fn scope() -> Scope {
        Scope {
            account: "fixture".into(),
            provider: "fixture".into(),
            collection: "drive".into(),
        }
    }
    fn insert(db: &Store, revision: i64, start: usize, count: usize) {
        let key = Store::key(&scope()).unwrap();
        db.db
            .execute("UPDATE metadata_clock SET revision=?1", [revision])
            .unwrap();
        for id in start..start + count {
            db.db.execute("INSERT INTO metadata_versions VALUES(?1,1,?2,?3) ON CONFLICT(scope,kind,identity) DO UPDATE SET revision=excluded.revision",params![key,format!("item-{id:06}"),revision]).unwrap();
        }
    }
    #[test]
    fn migration_from_schema_five_preserves_metadata_and_rolls_back_bad_index() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("metadata.db");
        let db = Store::open(&path).unwrap();
        insert(&db, 1, 0, 5);
        let old = db.metadata_position().unwrap();
        drop(db);
        let raw = Connection::open(&path).unwrap();
        raw.execute_batch("DROP INDEX metadata_change_order; CREATE INDEX metadata_change_order ON feeds(scope); PRAGMA user_version=5;").unwrap();
        assert!(Store::open(&path).is_err());
        assert_eq!(
            raw.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
                .unwrap(),
            5
        );
        assert_eq!(
            raw.query_row("SELECT count(*) FROM metadata_versions", [], |r| r
                .get::<_, u32>(0))
                .unwrap(),
            5
        );
        raw.execute_batch("DROP INDEX metadata_change_order;")
            .unwrap();
        let db = Store::open(&path).unwrap();
        assert_eq!(
            db.db
                .pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
                .unwrap(),
            6
        );
        assert!(db.metadata_changes(&old).unwrap().changes.is_empty());
    }
    #[test]
    fn pages_share_a_revision_boundary_without_holding_a_read_transaction() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("metadata.db");
        let db = Store::open(&path).unwrap();
        let position = db.metadata_position().unwrap();
        insert(&db, 1, 0, 600);
        let first = db.metadata_changes(&position).unwrap();
        assert_eq!(first.changes.len(), 256);
        assert!(!first.complete);
        let other = Store::open(&path).unwrap();
        insert(&other, 2, 300, 1);
        let mut next = first.next;
        let mut count = 256;
        loop {
            let page = db.metadata_changes(&next).unwrap();
            count += page.changes.len();
            next = page.next;
            if page.complete {
                break;
            }
        }
        assert_eq!(count, 599);
        let page = db.metadata_changes(&next).unwrap();
        assert_eq!(page.changes.len(), 1);
        assert_eq!(page.changes[0].identity, "item-000300");
        assert!(page.complete);
        assert!(db.metadata_changes(&page.next).unwrap().changes.is_empty());
    }
    #[test]
    fn scope_reset_during_pagination_and_database_replacement_are_detected() {
        let db = Store::open(":memory:").unwrap();
        let position = db.metadata_position().unwrap();
        insert(&db, 1, 0, 600);
        let first = db.metadata_changes(&position).unwrap();
        db.db.execute("DELETE FROM metadata_versions", []).unwrap();
        db.db
            .execute("UPDATE metadata_clock SET revision=2", [])
            .unwrap();
        db.db
            .execute(
                "INSERT INTO metadata_versions VALUES(?1,0,'',2)",
                [Store::key(&scope()).unwrap()],
            )
            .unwrap();
        let page = db.metadata_changes(&first.next).unwrap();
        assert!(page.complete);
        assert!(page.changes.is_empty());
        let page = db.metadata_changes(&page.next).unwrap();
        assert_eq!(page.changes[0].kind, MetadataChangeKind::Scope);
        let different = Store::open(":memory:").unwrap();
        assert!(matches!(
            different.metadata_changes(&page.next),
            Err(StoreError::OutOfOrder)
        ));
    }
    #[test]
    fn revision_range_uses_its_covering_index_and_bounds_large_identity_batches() {
        let db = Store::open(":memory:").unwrap();
        insert(&db, 1, 0, 20_000);
        let position = db.metadata_position().unwrap();
        insert(&db, 2, 20_000, 1);
        let mut statement = db.db.prepare(FIRST).unwrap();
        let mut rows = statement.query(params![1, 2]).unwrap();
        assert!(rows.next().unwrap().is_some());
        assert!(rows.next().unwrap().is_none());
        drop(rows);
        assert_eq!(
            statement.get_status(rusqlite::StatementStatus::FullscanStep),
            0
        );
        assert_eq!(statement.get_status(rusqlite::StatementStatus::Sort), 0);
        let key = Store::key(&scope()).unwrap();
        for i in 0..3 {
            db.db
                .execute(
                    "INSERT INTO metadata_versions VALUES(?1,1,?2,2)",
                    params![key, format!("{i}{}", "x".repeat(40_000))],
                )
                .unwrap();
        }
        let page = db.metadata_changes(&position).unwrap();
        assert_eq!(page.changes.len(), 1);
        assert!(!page.complete);
    }
}
