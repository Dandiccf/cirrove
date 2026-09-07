//! Crash-resumable metadata staging. Visible rows and the completed cursor advance
//! in one transaction, only after the last page. This is not an upload journal.
mod directories;
mod metadata_changes;
pub use metadata_changes::{MetadataChange, MetadataChangeKind, MetadataChanges, MetadataPosition};
mod observations;
use cirrove_core::{Change, ChangePage, Cursor, Node, Scope};
pub use observations::{
    AbsenceResult, DirectoryPublication, DirectoryPublicationResult, ObservationResult,
    ObservationTicket,
};
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
    #[error("invalid stored directory snapshot")]
    InvalidDirectorySnapshot,
    #[error("directory staging limit exceeded")]
    DirectoryLimit,
    #[error("directory publication cancelled or expired")]
    Cancelled,
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

fn initial_wal(db: &Connection) -> rusqlite::Result<()> {
    use std::time::{Duration, Instant};
    let timeout = Duration::from_secs(3);
    let deadline = Instant::now() + timeout;
    let result = loop {
        // Concurrent journal-mode transitions can return BUSY immediately to
        // avoid a lock-upgrade deadlock, bypassing SQLite's busy handler. Retry
        // only this autocommit initialization step within one total deadline.
        db.busy_timeout(deadline.saturating_duration_since(Instant::now()))?;
        match db.execute_batch("PRAGMA journal_mode=WAL;") {
            Err(error)
                if error.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseBusy)
                    && db.is_autocommit()
                    && Instant::now() < deadline =>
            {
                std::thread::sleep(
                    Duration::from_millis(10)
                        .min(deadline.saturating_duration_since(Instant::now())),
                );
            }
            result => break result,
        }
    };
    db.busy_timeout(timeout)?;
    result
}
// Start at actual shortcuts and walk their ancestors. Walking every descendant
// of a library root turns an idle discovery poll into a whole-library operation.
const SHORTCUTS_UNDER: &str = "WITH RECURSIVE ancestors(shortcut,id,parent) AS (
    SELECT id,id,json_extract(body,'$.parent_id') FROM nodes
    WHERE scope=?1 AND json_type(body,'$.target')='object'
    UNION
    SELECT a.shortcut,n.id,json_extract(n.body,'$.parent_id') FROM ancestors a
    JOIN nodes n ON n.scope=?1 AND n.id=a.parent WHERE a.id!=?2
) SELECT body FROM nodes WHERE scope=?1 AND id IN (
    SELECT shortcut FROM ancestors WHERE id=?2 OR parent=?2
)";
impl Store {
    /// Call from a blocking worker, never from a filesystem callback or while a
    /// network request is outstanding. One connection per worker.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let mut db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(3))?;
        db.execute_batch("PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")?;
        let version: u32 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > 6 {
            return Err(StoreError::SchemaVersion);
        }
        if version < 6 {
            if version < 2 {
                initial_wal(&db)?;
            }
            let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            // Another connection may have migrated while this one waited for
            // the writer. All schema steps and their version publish together.
            let version: u32 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
            if version > 6 {
                return Err(StoreError::SchemaVersion);
            }
            if version < 2 {
                tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS feeds (
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
",
                )?;
            }
            if version < 3 {
                tx.execute_batch(
                    "CREATE INDEX IF NOT EXISTS node_shortcuts ON nodes(scope,id)
                    WHERE json_type(body,'$.target')='object';",
                )?;
            }
            observations::migrate(&tx, version)?;
            directories::migrate(&tx, version)?;
            metadata_changes::migrate(&tx, version)?;
            // An unusable clock or malformed schema must roll back migration,
            // just like a failure while copying directory entries.
            observations::validate(&tx)?;
            metadata_changes::validate(&tx)?;
            directories::validate(&tx)?;
            if version < 6 {
                tx.pragma_update(None, "user_version", 6)?;
            }
            tx.commit()?;
        }
        observations::validate(&db)?;
        directories::validate(&db)?;
        metadata_changes::validate(&db)?;
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
            tx.execute("INSERT INTO rounds(scope,started) VALUES(?1,?2) ON CONFLICT(scope) DO UPDATE SET started=excluded.started",params![key,observations::advance(&tx)?])?;
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
        // The cursor check and its following writes share a reserved writer;
        // concurrent foreground observations must not invalidate this snapshot.
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
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
            observations::publish_delta(&tx, &key, reset)?;
            // A delta with no relevant changes must not discard a fresher
            // foreground directory snapshot. Invalidate only touched identities
            // and their old/new parents, before replacing the indexed rows.
            if reset {
                tx.execute("DELETE FROM observed_absent WHERE scope=?1 AND source_revision<=(SELECT started FROM rounds WHERE scope=?1)",[&key])?;
                tx.execute("DELETE FROM observed WHERE scope=?1 AND source_revision<=(SELECT started FROM rounds WHERE scope=?1)",[&key])?;
                tx.execute("DELETE FROM directories WHERE scope=?1 AND source_revision<=(SELECT started FROM rounds WHERE scope=?1)",[&key])?;
            } else {
                tx.execute("DELETE FROM directories WHERE scope=?1 AND source_revision<=(SELECT started FROM rounds WHERE scope=?1) AND parent IN (
                    SELECT id FROM staged WHERE scope=?1
                    UNION SELECT json_extract(body,'$.parent_id') FROM staged WHERE scope=?1
                    UNION SELECT json_extract((SELECT body FROM nodes WHERE scope=?1 AND id=s.id),'$.parent_id') FROM staged s WHERE scope=?1
                    UNION SELECT json_extract((SELECT body FROM observed WHERE scope=?1 AND id=s.id),'$.parent_id') FROM staged s WHERE scope=?1
                )",[&key])?;
                tx.execute("DELETE FROM observed_absent WHERE scope=?1 AND source_revision<=(SELECT started FROM rounds WHERE scope=?1) AND id IN (SELECT id FROM staged WHERE scope=?1)",[&key])?;
                tx.execute("DELETE FROM observed WHERE scope=?1 AND source_revision<=(SELECT started FROM rounds WHERE scope=?1) AND id IN (SELECT id FROM staged WHERE scope=?1)",[&key])?;
            }
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
    /// Follow only actual shortcuts' ancestry using the partial shortcut index.
    /// UNION deduplicates corrupt/cyclic parent references for each shortcut.
    pub fn shortcuts_under(&self, scope: &Scope, root: &str) -> Result<Vec<Node>> {
        let mut query = self.db.prepare(SHORTCUTS_UNDER)?;
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
        Self::node_on(&self.db, scope, id)
    }
    fn node_on(db: &Connection, scope: &Scope, id: &str) -> Result<Option<Node>> {
        let key = Self::key(scope)?;
        let body = db
            .query_row(
                "SELECT body FROM (SELECT body FROM observed WHERE scope=?1 AND id=?2
            UNION ALL SELECT body FROM nodes WHERE scope=?1 AND id=?2)
            WHERE NOT EXISTS(SELECT 1 FROM observed_absent WHERE scope=?1 AND id=?2) LIMIT 1",
                params![key, id],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        body.map(|v| Ok(serde_json::from_str(&v)?)).transpose()
    }
    pub fn children(&self, scope: &Scope, parent: &str) -> Result<Option<Vec<Node>>> {
        let tx = self.db.unchecked_transaction()?;
        let nodes = Self::children_on(&tx, scope, parent)?;
        tx.commit()?;
        Ok(nodes)
    }
    /// Find a name in one consistent cached directory view without decoding its
    /// other children. Outer None means unknown; Some(None) means known absent.
    /// The first name/identity-ordered match agrees with the listing API.
    pub fn child(&self, scope: &Scope, parent: &str, name: &str) -> Result<Option<Option<Node>>> {
        let tx = self.db.unchecked_transaction()?;
        let node = directories::child_on(&tx, scope, parent, name)?;
        tx.commit()?;
        Ok(node)
    }
    /// Consume a known directory through a fallible iterator in one consistent
    /// read transaction. Unknown directories return None without calling consume.
    /// The callback must finish local blocking work promptly; never retain this
    /// transaction across network requests or idle open filesystem handles.
    /// Its result is returned unchanged so callers can preserve their own errors.
    pub fn with_children<T>(
        &self,
        scope: &Scope,
        parent: &str,
        consume: impl FnOnce(&mut dyn Iterator<Item = Result<Node>>) -> T,
    ) -> Result<Option<T>> {
        let tx = self.db.unchecked_transaction()?;
        let result = directories::read_on(&tx, scope, parent, consume)?;
        tx.commit()?;
        Ok(result)
    }
    /// Visit a known directory in stable name/identity order within one read
    /// transaction. Returns false if the directory has not been indexed. The
    /// callback must perform only local blocking work, never network I/O. It can
    /// return an error to stop immediately; no metadata writer is reserved.
    /// The visitor retains only one decoded entry; callers control their storage.
    pub fn visit_children(
        &self,
        scope: &Scope,
        parent: &str,
        visit: impl FnMut(Node) -> Result<()>,
    ) -> Result<bool> {
        let tx = self.db.unchecked_transaction()?;
        let known = directories::visit_on(&tx, scope, parent, visit)?;
        tx.commit()?;
        Ok(known)
    }
    fn children_on(db: &Connection, scope: &Scope, parent: &str) -> Result<Option<Vec<Node>>> {
        let mut nodes = Vec::new();
        let known = directories::visit_on(db, scope, parent, |node| {
            nodes.push(node);
            Ok(())
        })?;
        Ok(known.then_some(nodes))
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
        // Inode mappings are immutable. A fully cached directory needs no writer
        // reservation and must stay readable during cache/index publication.
        let mut result = Vec::with_capacity(keys.len());
        {
            let mut query = self.db.prepare("SELECT inode FROM inodes WHERE key=?1")?;
            for key in keys {
                match query.query_row([key], |r| r.get::<_, i64>(0)).optional()? {
                    Some(inode) => result.push(inode as u64),
                    None => break,
                }
            }
        }
        if result.len() == keys.len() {
            return Ok(result);
        }
        // End the read before reserving the writer; never upgrade a stale read
        // transaction. Recheck all mappings after admission because another
        // allocator may already have inserted a formerly missing key.
        result.clear();
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
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
            content_version: None,
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
    fn concurrent_observations_and_delta_commits_preserve_all_completed_listings() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("db");
        Store::open(&path).unwrap();
        let start = std::sync::Barrier::new(4);
        std::thread::scope(|threads| {
            let workers: Vec<_> = (0..4)
                .map(|worker| {
                    let path = &path;
                    let start = &start;
                    threads.spawn(move || -> Result<()> {
                        let mut db = Store::open(path)?;
                        start.wait();
                        let s = scope(&format!("account-{worker}"));
                        let parent = format!("parent-{worker}");
                        for generation in 0..100 {
                            let Change::Upsert(mut item) = node(&format!("item-{worker}")) else {
                                unreachable!()
                            };
                            item.name = format!("revision-{generation}");
                            item.parent_id = Some(parent.clone());
                            if worker % 2 == 0 {
                                assert!(db.observe_directory(&s, &parent, &[item])?);
                            } else {
                                let cursor = db.begin(&s, false)?;
                                db.stage(
                                    &s,
                                    cursor.as_ref(),
                                    &page(
                                        vec![Change::Upsert(item)],
                                        true,
                                        &format!("delta-{generation}"),
                                    ),
                                )?;
                            }
                        }
                        Ok(())
                    })
                })
                .collect();
            let results: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
            assert!(results.iter().all(Result::is_ok), "{results:?}");
        });
        let db = Store::open(path).unwrap();
        for worker in 0..4 {
            let items = db
                .children(
                    &scope(&format!("account-{worker}")),
                    &format!("parent-{worker}"),
                )
                .unwrap()
                .unwrap();
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].name, "revision-99");
        }
    }
    #[test]
    fn unrelated_delta_keeps_fresh_directory_until_an_actual_item_change() {
        let mut db = Store::open(":memory:").unwrap();
        let s = scope("a");
        db.begin(&s, false).unwrap();
        db.stage(&s, None, &page(vec![node("item")], true, "d1"))
            .unwrap();
        let Change::Upsert(mut fresh) = node("item") else {
            unreachable!()
        };
        fresh.name = "fresh name".into();
        fresh.parent_id = Some("root".into());
        assert!(db.observe_directory(&s, "root", &[fresh.clone()]).unwrap());
        // An unchanged background check must not trigger a filesystem reload
        // which could indefinitely renew its own directory-activity lease.
        assert!(!db.observe_directory(&s, "root", &[fresh.clone()]).unwrap());
        for changes in [vec![], vec![node("unrelated")]] {
            let cursor = db.begin(&s, false).unwrap();
            db.stage(&s, cursor.as_ref(), &page(changes, true, "d2"))
                .unwrap();
            assert_eq!(db.node(&s, "item").unwrap(), Some(fresh.clone()));
            assert_eq!(db.children(&s, "root").unwrap(), Some(vec![fresh.clone()]));
        }
        fresh.name = "moved".into();
        fresh.parent_id = Some("other".into());
        let cursor = db.begin(&s, false).unwrap();
        db.stage(
            &s,
            cursor.as_ref(),
            &page(vec![Change::Upsert(fresh.clone())], true, "d3"),
        )
        .unwrap();
        assert!(db.children(&s, "root").unwrap().unwrap().is_empty());
        assert_eq!(db.children(&s, "other").unwrap(), Some(vec![fresh.clone()]));
        assert_eq!(db.node(&s, "item").unwrap(), Some(fresh));
    }
    #[test]
    fn cached_inode_batch_stays_readable_while_another_connection_holds_the_writer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db");
        let keys = vec!["directory".into(), "file".into(), "directory".into()];
        let mut reader = Store::open(&path).unwrap();
        let expected = reader.inodes(&keys).unwrap();
        let writer = Store::open(&path).unwrap();
        writer.db.execute_batch("BEGIN IMMEDIATE").unwrap();
        // The writer stays reserved through the assertion. This is independent
        // of scheduling speed: a cached listing must not request that lock.
        assert_eq!(reader.inodes(&keys).unwrap(), expected);
        assert!(reader.inodes(&[]).unwrap().is_empty());
        writer.db.execute_batch("ROLLBACK").unwrap();
    }

    #[test]
    fn concurrent_inode_batches_keep_cached_and_new_keys_stable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db");
        let cached = Store::open(&path).unwrap().inode("cached").unwrap();
        let start = std::sync::Arc::new(std::sync::Barrier::new(4));
        let workers: Vec<_> = (0..4)
            .map(|worker| {
                let path = path.clone();
                let start = start.clone();
                std::thread::spawn(move || {
                    let mut store = Store::open(path).unwrap();
                    let keys = vec![
                        "cached".into(),
                        "shared".into(),
                        format!("worker-{worker}"),
                        "shared".into(),
                    ];
                    start.wait();
                    let inodes = store.inodes(&keys).unwrap();
                    assert_eq!(store.inodes(&keys).unwrap(), inodes);
                    inodes
                })
            })
            .collect();
        let results: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
        for ids in &results {
            assert_eq!(ids[0], cached);
            assert_eq!(ids[1], results[0][1]);
            assert_eq!(ids[1], ids[3]);
            assert_ne!(ids[1], cached);
            assert_ne!(ids[2], ids[1]);
        }
        assert_eq!(
            results
                .iter()
                .map(|ids| ids[2])
                .collect::<std::collections::HashSet<_>>()
                .len(),
            4
        );
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

    #[test]
    fn shortcut_discovery_work_does_not_scale_with_unrelated_regular_files() {
        let mut db = Store::open(":memory:").unwrap();
        let scope = scope("large-library");
        let mut changes = Vec::new();
        for i in 0..20_000 {
            let Change::Upsert(mut item) = node(&format!("file-{i}")) else {
                unreachable!()
            };
            item.parent_id = Some("root".into());
            changes.push(Change::Upsert(item));
        }
        let Change::Upsert(mut shortcut) = node("link") else {
            unreachable!()
        };
        shortcut.parent_id = Some("root".into());
        shortcut.kind = NodeKind::Shortcut;
        shortcut.target = Some(cirrove_core::RemoteRef {
            collection: "library".into(),
            item: "shared".into(),
            kind: Some(NodeKind::Folder),
        });
        changes.push(Change::Upsert(shortcut));
        db.begin(&scope, false).unwrap();
        db.stage(&scope, None, &page(changes, true, "delta"))
            .unwrap();
        let mut statement = db.db.prepare(SHORTCUTS_UNDER).unwrap();
        let results = statement
            .query_map(params![Store::key(&scope).unwrap(), "root"], |row| {
                row.get::<_, String>(0)
            })
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(results.len(), 1);
        // Count SQLite VM work rather than assert a wall-clock benchmark.
        assert!(statement.get_status(rusqlite::StatementStatus::VmStep) < 2_000);
        assert_eq!(
            statement.get_status(rusqlite::StatementStatus::FullscanStep),
            0
        );
    }

    #[test]
    fn shortcut_index_migration_preserves_existing_metadata_and_cursor() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("metadata.db");
        let scope = scope("migration");
        {
            let mut db = Store::open(&path).unwrap();
            db.begin(&scope, false).unwrap();
            db.stage(&scope, None, &page(vec![node("preserved")], true, "delta"))
                .unwrap();
            db.db
                .execute_batch("DROP INDEX node_shortcuts;
                    ALTER TABLE directories ADD COLUMN body TEXT NOT NULL DEFAULT '[]';
                    DROP TABLE directory_entries; DROP INDEX node_parent_name; DROP INDEX observed_parent_name;
                    PRAGMA user_version=2;")
                .unwrap();
        }
        let db = Store::open(path).unwrap();
        assert_eq!(db.nodes(&scope).unwrap()[0].id, "preserved");
        assert_eq!(db.cursor(&scope).unwrap(), Some(Cursor("delta".into())));
        assert_eq!(
            db.db
                .pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
                .unwrap(),
            6
        );
        assert_eq!(
            db.db
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name='node_shortcuts'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
    }
}
