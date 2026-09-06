//! Crash-resumable metadata staging. Visible rows and the completed cursor advance
//! in one transaction, only after the last page. This is not an upload journal.
use cirrove_core::{Change, ChangePage, Cursor, Node, Scope};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("metadata database error")]
    Database(#[from] rusqlite::Error),
    #[error("invalid stored metadata")]
    Encoding(#[from] serde_json::Error),
    #[error("no refresh is in progress")]
    NoRefresh,
    #[error("page does not match the saved cursor")]
    OutOfOrder,
    #[error("unsupported database schema version")]
    SchemaVersion,
}
pub type Result<T> = std::result::Result<T, StoreError>;

pub struct Store {
    db: Connection,
}
impl Store {
    /// Call from a blocking worker, never from a filesystem callback or while a
    /// network request is outstanding. One connection per worker.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(3))?;
        let version: u32 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > 1 {
            return Err(StoreError::SchemaVersion);
        }
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;
            BEGIN IMMEDIATE;
            CREATE TABLE IF NOT EXISTS feeds (
                scope TEXT PRIMARY KEY, cursor TEXT, pending INTEGER NOT NULL DEFAULT 0,
                next_cursor TEXT, reset INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS nodes (
                scope TEXT NOT NULL, id TEXT NOT NULL, body TEXT NOT NULL,
                PRIMARY KEY(scope,id));
            CREATE TABLE IF NOT EXISTS staged (
                scope TEXT NOT NULL, id TEXT NOT NULL, body TEXT,
                PRIMARY KEY(scope,id));
            PRAGMA user_version=1; COMMIT;",
        )?;
        Ok(Self { db })
    }
    fn key(scope: &Scope) -> Result<String> {
        Ok(serde_json::to_string(scope)?)
    }
    /// Resume an interrupted refresh, or start from the completed cursor.
    /// Explicit reset stages a new baseline without clearing the visible index.
    pub fn begin(&mut self, scope: &Scope, reset: bool) -> Result<Option<Cursor>> {
        let key = Self::key(scope)?;
        let tx = self.db.transaction()?;
        tx.execute("INSERT OR IGNORE INTO feeds(scope) VALUES(?1)", [&key])?;
        if reset {
            tx.execute("DELETE FROM staged WHERE scope=?1", [&key])?;
            tx.execute(
                "UPDATE feeds SET pending=1, next_cursor=NULL, reset=1 WHERE scope=?1",
                [&key],
            )?;
        } else {
            tx.execute("UPDATE feeds SET pending=1,next_cursor=cursor,reset=(cursor IS NULL) WHERE scope=?1 AND pending=0", [&key])?;
        }
        let cursor = tx.query_row(
            "SELECT next_cursor FROM feeds WHERE scope=?1",
            [&key],
            |r| r.get::<_, Option<String>>(0),
        )?;
        tx.commit()?;
        Ok(cursor.map(Cursor))
    }
    pub fn stage(
        &mut self,
        scope: &Scope,
        expected: Option<&Cursor>,
        page: &ChangePage,
    ) -> Result<()> {
        let key = Self::key(scope)?;
        let tx = self.db.transaction()?;
        let feed = tx
            .query_row(
                "SELECT pending,next_cursor,reset FROM feeds WHERE scope=?1",
                [&key],
                |r| {
                    Ok((
                        r.get::<_, bool>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, bool>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((true, saved, reset)) = feed else {
            return Err(StoreError::NoRefresh);
        };
        if saved.as_deref() != expected.map(|c| c.0.as_str()) {
            return Err(StoreError::OutOfOrder);
        }
        for change in &page.changes {
            let (id, body) = match change {
                Change::Upsert(node) => (node.id.as_str(), Some(serde_json::to_string(node)?)),
                Change::Delete { id } => (id.as_str(), None),
            };
            tx.execute("INSERT INTO staged(scope,id,body) VALUES(?1,?2,?3) ON CONFLICT(scope,id) DO UPDATE SET body=excluded.body", params![key,id,body])?;
        }
        if page.checkpoint.complete() {
            if reset {
                tx.execute("DELETE FROM nodes WHERE scope=?1", [&key])?;
            }
            tx.execute("DELETE FROM nodes WHERE scope=?1 AND id IN (SELECT id FROM staged WHERE scope=?1 AND body IS NULL)", [&key])?;
            tx.execute("INSERT INTO nodes(scope,id,body) SELECT scope,id,body FROM staged WHERE scope=?1 AND body IS NOT NULL ON CONFLICT(scope,id) DO UPDATE SET body=excluded.body", [&key])?;
            tx.execute("DELETE FROM staged WHERE scope=?1", [&key])?;
            tx.execute(
                "UPDATE feeds SET cursor=?2,next_cursor=NULL,pending=0,reset=0 WHERE scope=?1",
                params![key, page.checkpoint.cursor().0],
            )?;
        } else {
            tx.execute(
                "UPDATE feeds SET next_cursor=?2 WHERE scope=?1",
                params![key, page.checkpoint.cursor().0],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn cursor(&self, scope: &Scope) -> Result<Option<Cursor>> {
        Ok(self
            .db
            .query_row(
                "SELECT cursor FROM feeds WHERE scope=?1",
                [Self::key(scope)?],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten()
            .map(Cursor))
    }
    pub fn nodes(&self, scope: &Scope) -> Result<Vec<Node>> {
        let mut query = self
            .db
            .prepare("SELECT body FROM nodes WHERE scope=?1 ORDER BY id")?;
        let rows = query.query_map([Self::key(scope)?], |r| r.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }
    pub fn counts(&self) -> Result<(u64, u64)> {
        Ok((
            self.db
                .query_row("SELECT COUNT(*) FROM feeds", [], |r| r.get::<_, i64>(0))?
                as u64,
            self.db
                .query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get::<_, i64>(0))?
                as u64,
        ))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use cirrove_core::{Checkpoint, NodeKind};
    fn scope(account: &str) -> Scope {
        Scope {
            account: account.into(),
            provider: "fixture".into(),
            collection: "drive".into(),
        }
    }
    fn node(id: &str) -> Change {
        Change::Upsert(Node {
            id: id.into(),
            parent_id: None,
            name: id.into(),
            kind: NodeKind::File,
            size: 3,
            etag: None,
            target: None,
        })
    }
    fn page(changes: Vec<Change>, end: bool, cursor: &str) -> ChangePage {
        ChangePage {
            changes,
            checkpoint: if end {
                Checkpoint::Complete(Cursor(cursor.into()))
            } else {
                Checkpoint::Continue(Cursor(cursor.into()))
            },
        }
    }
    #[test]
    fn interrupted_pages_resume_without_exposing_partial_tree() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db");
        let s = scope("a");
        {
            let mut db = Store::open(&path).unwrap();
            db.begin(&s, false).unwrap();
            db.stage(&s, None, &page(vec![node("one")], false, "page2"))
                .unwrap();
            assert!(db.nodes(&s).unwrap().is_empty());
            assert!(db.cursor(&s).unwrap().is_none());
        }
        let mut db = Store::open(path).unwrap();
        let cursor = db.begin(&s, false).unwrap();
        assert_eq!(cursor, Some(Cursor("page2".into())));
        db.stage(
            &s,
            cursor.as_ref(),
            &page(vec![node("two")], true, "delta1"),
        )
        .unwrap();
        assert_eq!(db.nodes(&s).unwrap().len(), 2);
        assert_eq!(db.cursor(&s).unwrap(), Some(Cursor("delta1".into())));
    }
    #[test]
    fn reset_keeps_visible_baseline_until_complete_and_is_account_isolated() {
        let mut db = Store::open(":memory:").unwrap();
        let a = scope("a");
        let b = scope("b");
        for s in [&a, &b] {
            db.begin(s, false).unwrap();
            db.stage(s, None, &page(vec![node("old")], true, "v1"))
                .unwrap();
        }
        db.begin(&a, true).unwrap();
        db.stage(&a, None, &page(vec![node("new")], false, "p2"))
            .unwrap();
        assert_eq!(db.nodes(&a).unwrap()[0].id, "old");
        db.stage(&a, Some(&Cursor("p2".into())), &page(vec![], true, "v2"))
            .unwrap();
        assert_eq!(db.nodes(&a).unwrap()[0].id, "new");
        assert_eq!(db.nodes(&b).unwrap()[0].id, "old");
    }
    #[test]
    fn deletion_last_occurrence_and_out_of_order_protection() {
        let mut db = Store::open(":memory:").unwrap();
        let s = scope("a");
        db.begin(&s, false).unwrap();
        db.stage(&s, None, &page(vec![node("one"), node("two")], true, "v1"))
            .unwrap();
        let cursor = db.begin(&s, false).unwrap();
        assert!(matches!(
            db.stage(&s, None, &page(vec![], true, "bad")),
            Err(StoreError::OutOfOrder)
        ));
        db.stage(
            &s,
            cursor.as_ref(),
            &page(
                vec![node("two"), Change::Delete { id: "two".into() }],
                true,
                "v2",
            ),
        )
        .unwrap();
        assert_eq!(db.nodes(&s).unwrap().len(), 1);
    }
}
