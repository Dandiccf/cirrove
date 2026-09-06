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

fn timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros()
        .min(i64::MAX as u128) as i64
}

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
        if version > 2 {
            return Err(StoreError::SchemaVersion);
        }
        if version < 2 {
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
            CREATE INDEX IF NOT EXISTS node_parent ON nodes(scope,json_extract(body,'$.parent_id'));
            CREATE TABLE IF NOT EXISTS observed (scope TEXT NOT NULL,id TEXT NOT NULL,body TEXT NOT NULL,seen INTEGER NOT NULL,PRIMARY KEY(scope,id));
            CREATE TABLE IF NOT EXISTS directories (scope TEXT NOT NULL,parent TEXT NOT NULL,body TEXT NOT NULL,seen INTEGER NOT NULL,PRIMARY KEY(scope,parent));
            CREATE TABLE IF NOT EXISTS rounds (scope TEXT PRIMARY KEY,started INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS inodes (inode INTEGER PRIMARY KEY AUTOINCREMENT,key TEXT NOT NULL UNIQUE);
            INSERT OR IGNORE INTO inodes(inode,key) VALUES(1,'root');
            CREATE TABLE IF NOT EXISTS health (scope TEXT PRIMARY KEY,body TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS subscriptions (account TEXT NOT NULL,collection TEXT NOT NULL,root TEXT NOT NULL,PRIMARY KEY(account,collection,root));
            CREATE TABLE IF NOT EXISTS cache_blocks (key TEXT PRIMARY KEY,size INTEGER NOT NULL,touched INTEGER NOT NULL);
            PRAGMA user_version=2; COMMIT;",
        )?;
        } else {
            db.execute_batch("PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")?;
        }
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
        let pending: bool =
            tx.query_row("SELECT pending FROM feeds WHERE scope=?1", [&key], |r| {
                r.get(0)
            })?;
        if reset || !pending {
            tx.execute("INSERT INTO rounds(scope,started) VALUES(?1,?2) ON CONFLICT(scope) DO UPDATE SET started=excluded.started",params![key,timestamp()])?;
        }
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
            tx.execute("DELETE FROM observed WHERE scope=?1 AND seen<=(SELECT started FROM rounds WHERE scope=?1)",[&key])?;
            tx.execute("DELETE FROM directories WHERE scope=?1 AND seen<=(SELECT started FROM rounds WHERE scope=?1)",[&key])?;
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
    /// Follow ancestry inside SQLite, retaining only shortcut rows in memory.
    /// UNION deduplicates corrupt/cyclic parent references.
    pub fn shortcuts_under(&self, scope: &Scope, root: &str) -> Result<Vec<Node>> {
        let mut query = self.db.prepare("WITH RECURSIVE reachable(id) AS (SELECT ?2 UNION SELECT n.id FROM nodes n JOIN reachable r ON json_extract(n.body,'$.parent_id')=r.id WHERE n.scope=?1) SELECT body FROM nodes WHERE scope=?1 AND id IN (SELECT id FROM reachable) AND json_type(body,'$.target')='object'")?;
        let rows = query.query_map(params![Self::key(scope)?, root], |r| r.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }
    pub fn directory_age(&self, scope: &Scope, parent: &str) -> Result<Option<u64>> {
        let seen = self
            .db
            .query_row(
                "SELECT seen FROM directories WHERE scope=?1 AND parent=?2",
                params![Self::key(scope)?, parent],
                |r| r.get::<_, i64>(0),
            )
            .optional()?;
        Ok(seen.map(|seen| timestamp().saturating_sub(seen).max(0) as u64 / 1_000_000))
    }
    pub fn node(&self, scope: &Scope, id: &str) -> Result<Option<Node>> {
        let key = Self::key(scope)?;
        let body=self.db.query_row("SELECT body FROM observed WHERE scope=?1 AND id=?2 UNION ALL SELECT body FROM nodes WHERE scope=?1 AND id=?2 LIMIT 1",params![key,id],|r|r.get::<_,String>(0)).optional()?;
        body.map(|v| Ok(serde_json::from_str(&v)?)).transpose()
    }
    pub fn observe_node(&mut self, scope: &Scope, node: &Node) -> Result<()> {
        self.db.execute("INSERT INTO observed(scope,id,body,seen) VALUES(?1,?2,?3,?4) ON CONFLICT(scope,id) DO UPDATE SET body=excluded.body,seen=excluded.seen",params![Self::key(scope)?,node.id,serde_json::to_string(node)?,timestamp()])?;
        Ok(())
    }
    pub fn children(&self, scope: &Scope, parent: &str) -> Result<Option<Vec<Node>>> {
        let key = Self::key(scope)?;
        let body = self
            .db
            .query_row(
                "SELECT body FROM directories WHERE scope=?1 AND parent=?2",
                params![key, parent],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        if let Some(body) = body {
            return Ok(Some(serde_json::from_str(&body)?));
        }
        if self.cursor(scope)?.is_none() {
            return Ok(None);
        }
        let mut query=self.db.prepare("SELECT body FROM nodes WHERE scope=?1 AND json_extract(body,'$.parent_id')=?2 ORDER BY json_extract(body,'$.name')")?;
        let rows = query.query_map(params![key, parent], |r| r.get::<_, String>(0))?;
        Ok(Some(
            rows.map(|r| Ok(serde_json::from_str(&r?)?))
                .collect::<Result<_>>()?,
        ))
    }
    pub fn observe_directory(&mut self, scope: &Scope, parent: &str, nodes: &[Node]) -> Result<()> {
        let key = Self::key(scope)?;
        let tx = self.db.transaction()?;
        let seen = timestamp();
        for node in nodes {
            tx.execute("INSERT INTO observed(scope,id,body,seen) VALUES(?1,?2,?3,?4) ON CONFLICT(scope,id) DO UPDATE SET body=excluded.body,seen=excluded.seen",params![key,node.id,serde_json::to_string(node)?,seen])?;
        }
        tx.execute("INSERT INTO directories(scope,parent,body,seen) VALUES(?1,?2,?3,?4) ON CONFLICT(scope,parent) DO UPDATE SET body=excluded.body,seen=excluded.seen",params![key,parent,serde_json::to_string(nodes)?,seen])?;
        tx.commit()?;
        Ok(())
    }
    pub fn inode(&mut self, key: &str) -> Result<u64> {
        if let Some(inode) = self
            .db
            .query_row("SELECT inode FROM inodes WHERE key=?1", [key], |r| {
                r.get::<_, i64>(0)
            })
            .optional()?
        {
            return Ok(inode as u64);
        }
        self.db
            .execute("INSERT OR IGNORE INTO inodes(key) VALUES(?1)", [key])?;
        Ok(self
            .db
            .query_row("SELECT inode FROM inodes WHERE key=?1", [key], |r| {
                r.get::<_, i64>(0)
            })? as u64)
    }
    pub fn inodes(&mut self, keys: &[String]) -> Result<Vec<u64>> {
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut result = Vec::with_capacity(keys.len());
        for key in keys {
            if let Some(inode) = tx
                .query_row("SELECT inode FROM inodes WHERE key=?1", [key], |r| {
                    r.get::<_, i64>(0)
                })
                .optional()?
            {
                result.push(inode as u64);
                continue;
            }
            tx.execute("INSERT INTO inodes(key) VALUES(?1)", [key])?;
            result.push(tx.last_insert_rowid() as u64);
        }
        tx.commit()?;
        Ok(result)
    }
    pub fn health(&self, scope: &Scope) -> Result<Option<String>> {
        Ok(self
            .db
            .query_row(
                "SELECT body FROM health WHERE scope=?1",
                [Self::key(scope)?],
                |r| r.get(0),
            )
            .optional()?)
    }
    pub fn set_health(&mut self, scope: &Scope, body: &str) -> Result<()> {
        self.db.execute("INSERT INTO health(scope,body) VALUES(?1,?2) ON CONFLICT(scope) DO UPDATE SET body=excluded.body",params![Self::key(scope)?,body])?;
        Ok(())
    }
    pub fn subscribe(&mut self, scope: &Scope, root: &str) -> Result<()> {
        self.db.execute(
            "INSERT OR IGNORE INTO subscriptions(account,collection,root) VALUES(?1,?2,?3)",
            params![scope.account, scope.collection, root],
        )?;
        Ok(())
    }
    pub fn subscriptions(&self, account: &str) -> Result<Vec<(String, String)>> {
        let mut query = self
            .db
            .prepare("SELECT collection,root FROM subscriptions WHERE account=?1")?;
        Ok(query
            .query_map([account], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }
    pub fn replace_subscriptions(
        &mut self,
        account: &str,
        roots: &std::collections::HashSet<(String, String)>,
    ) -> Result<()> {
        let tx = self.db.transaction()?;
        tx.execute("DELETE FROM subscriptions WHERE account=?1", [account])?;
        for (collection, root) in roots {
            tx.execute(
                "INSERT INTO subscriptions(account,collection,root) VALUES(?1,?2,?3)",
                params![account, collection, root],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn touch_block(&mut self, key: &str, size: u64) -> Result<()> {
        self.db.execute("INSERT INTO cache_blocks(key,size,touched) VALUES(?1,?2,?3) ON CONFLICT(key) DO UPDATE SET touched=excluded.touched,size=excluded.size",params![key,size as i64,timestamp()])?;
        Ok(())
    }
    pub fn oldest_blocks(&self) -> Result<Vec<(String, u64)>> {
        let mut query = self
            .db
            .prepare("SELECT key,size FROM cache_blocks ORDER BY touched")?;
        Ok(query
            .query_map([], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u64)))?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }
    pub fn forget_block(&mut self, key: &str) -> Result<()> {
        self.db
            .execute("DELETE FROM cache_blocks WHERE key=?1", [key])?;
        Ok(())
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
            modified_unix: 0,
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
    #[test]
    fn foreground_observations_survive_older_round_then_yield_to_next_complete_feed() {
        let mut db = Store::open(":memory:").unwrap();
        let scope = scope("a");
        db.begin(&scope, false).unwrap();
        let Change::Upsert(mut fresh) = node("item") else {
            unreachable!()
        };
        fresh.name = "new name".into();
        fresh.parent_id = Some("root".into());
        db.observe_directory(&scope, "root", &[fresh.clone()])
            .unwrap();
        db.stage(&scope, None, &page(vec![node("item")], true, "d1"))
            .unwrap();
        assert_eq!(db.node(&scope, "item").unwrap().unwrap().name, "new name");
        assert_eq!(db.children(&scope, "root").unwrap().unwrap(), vec![fresh]);
        let cursor = db.begin(&scope, false).unwrap();
        db.stage(
            &scope,
            cursor.as_ref(),
            &page(vec![Change::Delete { id: "item".into() }], true, "d2"),
        )
        .unwrap();
        assert!(db.node(&scope, "item").unwrap().is_none());
        assert!(db.children(&scope, "root").unwrap().unwrap().is_empty());
    }
    #[test]
    fn persistent_inode_batch_is_stable_and_shortcut_projection_is_scope_isolated() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("db");
        let keys = vec![
            "a/drive/item".into(),
            "a/alias/item".into(),
            "b/drive/item".into(),
        ];
        let first = Store::open(&path).unwrap().inodes(&keys).unwrap();
        assert_eq!(
            first
                .iter()
                .copied()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            3
        );
        assert_eq!(Store::open(&path).unwrap().inodes(&keys).unwrap(), first);
    }
    #[test]
    fn shortcut_discovery_uses_reachable_subtrees_and_stops_at_parent_cycles() {
        let mut db = Store::open(":memory:").unwrap();
        let scope = scope("a");
        let mut changes = vec![];
        for (id, parent, target) in [
            ("root", None, false),
            ("inside", Some("root"), true),
            ("outside", Some("other"), true),
            ("loop-a", Some("loop-b"), true),
            ("loop-b", Some("loop-a"), false),
        ] {
            let Change::Upsert(mut item) = node(id) else {
                unreachable!()
            };
            item.parent_id = parent.map(str::to_owned);
            if target {
                item.kind = NodeKind::Shortcut;
                item.target = Some(cirrove_core::RemoteRef {
                    collection: "library".into(),
                    item: "shared".into(),
                    kind: Some(NodeKind::Folder),
                });
            }
            changes.push(Change::Upsert(item));
        }
        db.begin(&scope, false).unwrap();
        db.stage(&scope, None, &page(changes, true, "d1")).unwrap();
        assert_eq!(
            db.shortcuts_under(&scope, "root")
                .unwrap()
                .iter()
                .map(|n| n.id.as_str())
                .collect::<Vec<_>>(),
            vec!["inside"]
        );
        assert_eq!(db.shortcuts_under(&scope, "loop-a").unwrap().len(), 1);
    }
}
