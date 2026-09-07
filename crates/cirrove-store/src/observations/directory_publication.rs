//! Connection-owned, bounded disk staging for complete foreground listings.
use super::*;
use cirrove_core::{CancellationToken, DirectoryPage};
use std::time::Instant;

const MAX_INPUT_BYTES: usize = 256 * 1024 * 1024;
const MAX_NODE_BYTES: usize = 1024 * 1024;
const MAX_PAGE_BYTES: usize = 16 * 1024 * 1024;
const MAX_PAGES: usize = 65_536;
const MAX_CURSOR_BYTES: usize = 16 * 1024;
const TEMP_PAGES: u32 = 131_072; // 512 MiB, including comparison tables and indexes.

/// Owns its connection so no other operation can reuse its TEMP tables or hook.
/// Page methods return in autocommit mode. Dropping it discards unpublished data.
/// Call every method on a blocking worker, outside filesystem locks.
pub struct DirectoryPublication {
    store: Store,
    ticket: ObservationTicket,
    cancel: CancellationToken,
    deadline: Instant,
    input_bytes: usize,
    pages: usize,
    complete: bool,
}
#[derive(Debug, PartialEq, Eq)]
pub enum DirectoryPublicationResult {
    Published {
        changed: bool,
    },
    /// Reread committed metadata, never the discarded staging rows.
    Superseded {
        known: bool,
    },
}
impl Store {
    pub fn directory_publication(
        mut self,
        scope: &Scope,
        parent: &str,
        cancel: CancellationToken,
        deadline: Instant,
    ) -> Result<DirectoryPublication> {
        check(&cancel, deadline)?;
        let ticket = self.directory_observation(scope, parent)?;
        // SQLite's Linux TEMP database spills to anonymous mode-0600 files.
        // Retain a small page cache; prohibit builds that force TEMP into RAM.
        let memory_only: bool = self.db.query_row(
            "SELECT sqlite_compileoption_used('TEMP_STORE=3')",
            [],
            |r| r.get(0),
        )?;
        if memory_only {
            return Err(StoreError::DirectoryLimit);
        }
        self.db.execute_batch("PRAGMA temp_store=FILE; PRAGMA temp.page_size=4096;
            PRAGMA temp.cache_size=-2048; PRAGMA main.cache_size=-2048;
            PRAGMA mmap_size=0;
            CREATE TEMP TABLE incoming(id TEXT PRIMARY KEY,name TEXT NOT NULL,body TEXT NOT NULL) WITHOUT ROWID;
            CREATE INDEX temp.incoming_name ON incoming(name,id);
            CREATE TEMP TABLE cursors(value TEXT PRIMARY KEY) WITHOUT ROWID;
            CREATE TEMP TABLE previous(id TEXT PRIMARY KEY,body TEXT NOT NULL) WITHOUT ROWID;
            CREATE TEMP TABLE affected(id TEXT PRIMARY KEY,parent TEXT) WITHOUT ROWID;
            CREATE TEMP TABLE removed(id TEXT PRIMARY KEY,parent TEXT) WITHOUT ROWID;")?;
        self.db
            .pragma_update(Some("temp"), "max_page_count", TEMP_PAGES)?;
        let token = cancel.clone();
        self.db.progress_handler(
            4096,
            Some(move || token.is_cancelled() || Instant::now() >= deadline),
        )?;
        Ok(DirectoryPublication {
            store: self,
            ticket,
            cancel,
            deadline,
            input_bytes: 0,
            pages: 0,
            complete: false,
        })
    }
}
fn check(cancel: &CancellationToken, deadline: Instant) -> Result<()> {
    if cancel.is_cancelled() || Instant::now() >= deadline {
        Err(StoreError::Cancelled)
    } else {
        Ok(())
    }
}
impl DirectoryPublication {
    /// Consumes the builder on failure; a partial or malformed page cannot later
    /// be published. Duplicate identities and repeated cursors fail closed.
    pub fn page(mut self, page: DirectoryPage) -> Result<Self> {
        check(&self.cancel, self.deadline)?;
        if self.complete || self.pages >= MAX_PAGES {
            return Err(StoreError::OutOfOrder);
        }
        let Target::Directory(parent) = &self.ticket.target else {
            unreachable!()
        };
        let tx = self.store.db.transaction()?;
        let mut page_bytes = 0usize;
        {
            let mut insert = tx.prepare("INSERT INTO incoming VALUES(?1,?2,?3)")?;
            for node in page.nodes {
                check(&self.cancel, self.deadline)?;
                if node.parent_id.as_deref() != Some(parent) || node.id.is_empty() {
                    return Err(StoreError::InvalidDirectorySnapshot);
                }
                // Bound source strings before serializing potentially escaped JSON.
                let string_bytes = node.id.len()
                    + node.name.len()
                    + parent.len()
                    + node.etag.as_ref().map_or(0, String::len)
                    + node.content_version.as_ref().map_or(0, String::len)
                    + node
                        .target
                        .as_ref()
                        .map_or(0, |t| t.collection.len() + t.item.len());
                if string_bytes > MAX_NODE_BYTES {
                    return Err(StoreError::DirectoryLimit);
                }
                let body = serde_json::to_string(&node)?;
                page_bytes += body.len();
                if body.len() > MAX_NODE_BYTES || page_bytes > MAX_PAGE_BYTES {
                    return Err(StoreError::DirectoryLimit);
                }
                insert.execute(params![node.id, node.name, body])?;
            }
        }
        if let Some(cursor) = &page.next {
            if cursor.0.len() > MAX_CURSOR_BYTES {
                return Err(StoreError::DirectoryLimit);
            }
            page_bytes += cursor.0.len();
            tx.execute("INSERT INTO cursors VALUES(?1)", [&cursor.0])?;
        }
        self.input_bytes += page_bytes;
        if self.input_bytes > MAX_INPUT_BYTES {
            return Err(StoreError::DirectoryLimit);
        }
        check(&self.cancel, self.deadline)?;
        tx.commit()?;
        self.pages += 1;
        self.complete = page.next.is_none();
        Ok(self)
    }
    /// Publish only after the terminal page. The main database's single WAL
    /// transaction owns all visible rows, absence markers and revision marks.
    pub fn publish(mut self) -> Result<DirectoryPublicationResult> {
        check(&self.cancel, self.deadline)?;
        if !self.complete {
            return Err(StoreError::OutOfOrder);
        }
        let tx = self
            .store
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let result = publish(&tx, &self.ticket, &self.cancel, self.deadline)?;
        check(&self.cancel, self.deadline)?;
        tx.commit()?;
        Ok(result)
    }
}
fn publish(
    tx: &Transaction<'_>,
    ticket: &ObservationTicket,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<DirectoryPublicationResult> {
    let Target::Directory(parent) = &ticket.target else {
        return Err(StoreError::OutOfOrder);
    };
    let scope = &ticket.scope;
    let key = Store::key(scope)?;
    let newer: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM incoming i JOIN metadata_versions v
        ON v.scope=?1 AND v.kind=1 AND v.identity=i.id WHERE v.revision>?2)",
        params![key, ticket.revision],
        |r| r.get(0),
    )?;
    if !current(tx, ticket, &[])? || newer {
        let known = directories::read_on(tx, scope, parent, |_| ())?.is_some();
        return Ok(DirectoryPublicationResult::Superseded { known });
    }
    // Canonicalize each visible Node independently. String comparison below is
    // then semantic equality, including defaults missing from legacy JSON bodies.
    let mut insert = tx.prepare("INSERT INTO previous VALUES(?1,?2)")?;
    let known = directories::visit_on(tx, scope, parent, |node| {
        check(cancel, deadline)?;
        insert.execute(params![node.id, serde_json::to_string(&node)?])?;
        Ok(())
    })?;
    drop(insert);
    tx.execute_batch(
        "INSERT INTO affected SELECT i.id,json_extract(i.body,'$.parent_id') FROM incoming i
        LEFT JOIN previous p ON p.id=i.id WHERE p.body IS NULL OR p.body!=i.body;",
    )?;
    let changed = !known || tx.query_row("SELECT EXISTS(SELECT 1 FROM affected)
        OR EXISTS(SELECT 1 FROM previous p WHERE NOT EXISTS(SELECT 1 FROM incoming i WHERE i.id=p.id))", [], |r| r.get::<_,bool>(0))?;
    // Match existing absence semantics, including stale baseline rows belonging
    // to an item that a newer observation moved to a different directory.
    for table in ["observed", "nodes"] {
        tx.execute(
            &format!(
                "INSERT OR REPLACE INTO removed SELECT n.id,json_extract(n.body,'$.parent_id') FROM {table} n
            WHERE n.scope=?1 AND json_extract(n.body,'$.parent_id')=?2
            AND NOT EXISTS(SELECT 1 FROM incoming i WHERE i.id=n.id)"
            ),
            params![key, parent],
        )?;
    }
    tx.execute_batch("INSERT OR REPLACE INTO affected SELECT id,parent FROM removed;")?;
    let revision = advance(tx)?;
    mark(tx, &key, 2, parent, revision)?;
    tx.execute(
        "INSERT INTO metadata_versions SELECT ?1,1,id,?2 FROM affected WHERE true
        ON CONFLICT(scope,kind,identity) DO UPDATE SET revision=excluded.revision",
        params![key, revision],
    )?;
    // Record incoming/removed parents and the currently resolved old parent.
    // Repeating an indexed upsert avoids an unbounded Rust parent set.
    tx.execute(
        "INSERT INTO metadata_versions SELECT ?1,2,parent,?2
        FROM affected WHERE parent IS NOT NULL
        ON CONFLICT(scope,kind,identity) DO UPDATE SET revision=excluded.revision",
        params![key, revision],
    )?;
    tx.execute("INSERT INTO metadata_versions SELECT ?1,2,json_extract(coalesce(o.body,n.body),'$.parent_id'),?2
        FROM affected a LEFT JOIN observed o ON o.scope=?1 AND o.id=a.id
        LEFT JOIN nodes n ON n.scope=?1 AND n.id=a.id
        WHERE NOT EXISTS(SELECT 1 FROM observed_absent x WHERE x.scope=?1 AND x.id=a.id)
        AND json_extract(coalesce(o.body,n.body),'$.parent_id') IS NOT NULL
        ON CONFLICT(scope,kind,identity) DO UPDATE SET revision=excluded.revision",params![key,revision])?;
    // Remove candidates with a currently visible move elsewhere. Evaluate this
    // before changing observed/absence tables, so one update cannot affect another.
    tx.execute(
        "DELETE FROM removed WHERE id IN (SELECT r.id FROM removed r
        LEFT JOIN observed o ON o.scope=?1 AND o.id=r.id
        LEFT JOIN nodes n ON n.scope=?1 AND n.id=r.id
        WHERE NOT EXISTS(SELECT 1 FROM observed_absent x WHERE x.scope=?1 AND x.id=r.id)
        AND coalesce(o.body,n.body) IS NOT NULL
        AND json_extract(coalesce(o.body,n.body),'$.parent_id') IS NOT ?2)",
        params![key, parent],
    )?;
    tx.execute(
        "DELETE FROM observed WHERE scope=?1 AND id IN (SELECT id FROM removed)",
        [&key],
    )?;
    tx.execute(
        "INSERT INTO observed_absent SELECT ?1,id,?2 FROM removed WHERE true
        ON CONFLICT(scope,id) DO UPDATE SET source_revision=excluded.source_revision",
        params![key, ticket.revision],
    )?;
    let seen = timestamp();
    tx.execute(
        "DELETE FROM observed_absent WHERE scope=?1 AND id IN (SELECT id FROM incoming)",
        [&key],
    )?;
    tx.execute("INSERT INTO observed(scope,id,body,seen,source_revision) SELECT ?1,id,body,?2,?3 FROM incoming WHERE true
        ON CONFLICT(scope,id) DO UPDATE SET body=excluded.body,seen=excluded.seen,source_revision=excluded.source_revision",params![key,seen,ticket.revision])?;
    tx.execute("INSERT INTO directories(scope,parent,seen,source_revision) VALUES(?1,?2,?3,?4)
        ON CONFLICT(scope,parent) DO UPDATE SET seen=excluded.seen,source_revision=excluded.source_revision",params![key,parent,seen,ticket.revision])?;
    tx.execute(
        "DELETE FROM directory_entries WHERE scope=?1 AND parent=?2",
        params![key, parent],
    )?;
    tx.execute("INSERT INTO directory_entries(scope,parent,id,name,body) SELECT ?1,?2,id,name,body FROM incoming",params![key,parent])?;
    Ok(DirectoryPublicationResult::Published { changed })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use cirrove_core::NodeKind;
    fn fixture() -> (tempfile::TempDir, DirectoryPublication) {
        let tmp = tempfile::tempdir().unwrap();
        let scope = Scope {
            account: "fixture".into(),
            provider: "fixture".into(),
            collection: "drive".into(),
        };
        let stage = Store::open(tmp.path().join("metadata.db"))
            .unwrap()
            .directory_publication(
                &scope,
                "root",
                CancellationToken::new(),
                Instant::now() + std::time::Duration::from_secs(60),
            )
            .unwrap();
        (tmp, stage)
    }
    fn nodes(count: usize) -> Vec<Node> {
        (0..count)
            .map(|i| Node {
                id: format!("{i:08}"),
                parent_id: Some("root".into()),
                name: "x".repeat(1024),
                kind: NodeKind::File,
                size: 0,
                modified_unix: 0,
                etag: None,
                content_version: None,
                target: None,
            })
            .collect()
    }
    #[test]
    fn temporary_database_is_capped_and_pages_leave_no_transaction() {
        let (_tmp, stage) = fixture();
        let stage = stage
            .page(DirectoryPage {
                nodes: nodes(2048),
                next: Some(Cursor("next".into())),
            })
            .unwrap();
        assert!(stage.store.db.is_autocommit());
        assert_eq!(
            stage
                .store
                .db
                .pragma_query_value(Some("temp"), "page_size", |r| r.get::<_, u32>(0))
                .unwrap(),
            4096
        );
        assert_eq!(
            stage
                .store
                .db
                .pragma_query_value(Some("temp"), "max_page_count", |r| r.get::<_, u32>(0))
                .unwrap(),
            TEMP_PAGES
        );
        let count: u32 = stage
            .store
            .db
            .pragma_query_value(Some("temp"), "page_count", |r| r.get(0))
            .unwrap();
        stage
            .store
            .db
            .pragma_update(Some("temp"), "max_page_count", count)
            .unwrap();
        let mut more = nodes(2048);
        for n in &mut more {
            n.id.push('b');
        }
        assert!(
            stage
                .page(DirectoryPage {
                    nodes: more,
                    next: None
                })
                .is_err()
        );
    }
    #[test]
    fn sql_interrupt_rolls_back_publication_and_releases_writer() {
        let (tmp, stage) = fixture();
        let scope = stage.ticket.scope.clone();
        let stage = stage
            .page(DirectoryPage {
                nodes: nodes(2048),
                next: None,
            })
            .unwrap();
        let token = stage.cancel.clone();
        let observe = token.clone();
        // Cancel during preparation of a bulk write, after revision marks have
        // changed. The production progress hook must interrupt its execution.
        stage
            .store
            .db
            .authorizer(Some(move |context: rusqlite::hooks::AuthContext<'_>| {
                if matches!(
                    context.action,
                    rusqlite::hooks::AuthAction::Insert {
                        table_name: "observed"
                    }
                ) {
                    token.cancel();
                }
                rusqlite::hooks::Authorization::Allow
            }))
            .unwrap();
        assert!(stage.publish().is_err());
        assert!(observe.is_cancelled());
        let mut db = Store::open(tmp.path().join("metadata.db")).unwrap();
        assert!(db.children(&scope, "root").unwrap().is_none());
        assert!(db.node(&scope, "00000000").unwrap().is_none());
        db.observe_directory(&scope, "root", &[]).unwrap();
    }
    #[test]
    #[ignore = "explicit 500k-row cold/unchanged/changed foreground publication memory benchmark"]
    fn directory_publication_capacity() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("metadata.db");
        let scope = Scope {
            account: "fixture".into(),
            provider: "fixture".into(),
            collection: "drive".into(),
        };
        let memory = || {
            let status = std::fs::read_to_string("/proc/self/status").unwrap();
            let value = |key: &str| {
                status
                    .lines()
                    .find(|line| line.starts_with(key))
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .parse::<u64>()
                    .unwrap()
            };
            serde_json::json!({"rss_kib":value("VmRSS:"),"peak_rss_kib":value("VmHWM:")})
        };
        for pass in 0..3 {
            let before = memory();
            let started = Instant::now();
            let mut stage = Store::open(&path)
                .unwrap()
                .directory_publication(
                    &scope,
                    "root",
                    CancellationToken::new(),
                    Instant::now() + std::time::Duration::from_secs(60),
                )
                .unwrap();
            for page in 0..500 {
                let nodes = (page * 1000..(page + 1) * 1000)
                    .map(|i| Node {
                        id: format!("file-{i:08}"),
                        parent_id: Some("root".into()),
                        name: format!("filename-{i:08}"),
                        kind: NodeKind::File,
                        size: if pass == 2 { 1 } else { 0 },
                        modified_unix: 0,
                        etag: None,
                        content_version: None,
                        target: None,
                    })
                    .collect();
                stage = stage
                    .page(DirectoryPage {
                        nodes,
                        next: (page < 499).then(|| Cursor(page.to_string())),
                    })
                    .unwrap();
            }
            let staged_bytes = stage
                .store
                .db
                .pragma_query_value(Some("temp"), "page_count", |r| r.get::<_, i64>(0))
                .unwrap()
                * 4096;
            let tx = stage
                .store
                .db
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .unwrap();
            assert_eq!(
                publish(&tx, &stage.ticket, &stage.cancel, stage.deadline).unwrap(),
                DirectoryPublicationResult::Published { changed: pass != 1 }
            );
            let publication_bytes = tx
                .pragma_query_value(Some("temp"), "page_count", |r| r.get::<_, i64>(0))
                .unwrap()
                * 4096;
            tx.commit().unwrap();
            drop(stage);
            println!(
                "CIRROVE_DIRECTORY_PUBLICATION {}",
                serde_json::json!({"pass":pass,"files":500000,"staged_database_bytes":staged_bytes,"publication_database_bytes":publication_bytes,"seconds":started.elapsed().as_secs_f64(),"memory_before":before,"memory_after":memory()})
            );
        }
    }
    #[test]
    fn cached_readers_keep_the_previous_view_during_final_publication() {
        let (tmp, stage) = fixture();
        let scope = stage.ticket.scope.clone();
        drop(stage);
        let path = tmp.path().join("metadata.db");
        Store::open(&path)
            .unwrap()
            .observe_directory(&scope, "root", &[])
            .unwrap();
        let stage = Store::open(&path)
            .unwrap()
            .directory_publication(
                &scope,
                "root",
                CancellationToken::new(),
                Instant::now() + std::time::Duration::from_secs(60),
            )
            .unwrap()
            .page(DirectoryPage {
                nodes: nodes(10),
                next: None,
            })
            .unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        stage
            .store
            .db
            .authorizer(Some(move |context: rusqlite::hooks::AuthContext<'_>| {
                if matches!(
                    context.action,
                    rusqlite::hooks::AuthAction::Insert {
                        table_name: "directory_entries"
                    }
                ) {
                    entered_tx.send(()).unwrap();
                    release_rx
                        .recv_timeout(std::time::Duration::from_secs(5))
                        .unwrap();
                }
                rusqlite::hooks::Authorization::Allow
            }))
            .unwrap();
        let publication = std::thread::spawn(move || stage.publish());
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let started = Instant::now();
        let db = Store::open(&path).unwrap();
        assert_eq!(db.children(&scope, "root").unwrap(), Some(vec![]));
        assert!(started.elapsed() < std::time::Duration::from_millis(500));
        release_tx.send(()).unwrap();
        assert_eq!(
            publication.join().unwrap().unwrap(),
            DirectoryPublicationResult::Published { changed: true }
        );
        assert_eq!(db.children(&scope, "root").unwrap().unwrap().len(), 10);
    }
    #[test]
    fn temporary_files_are_private_unlinked_and_closed_on_drop() {
        use std::os::unix::fs::MetadataExt;
        if std::env::var_os("CIRROVE_TEMP_FD_CHILD").is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "observations::directory_publication::tests::temporary_files_are_private_unlinked_and_closed_on_drop", "--nocapture"])
                .env("CIRROVE_TEMP_FD_CHILD", "1").output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
            return;
        }
        let (_tmp, stage) = fixture();
        let stage = stage
            .page(DirectoryPage {
                nodes: nodes(4096),
                next: None,
            })
            .unwrap();
        let mut files = Vec::new();
        for entry in std::fs::read_dir("/proc/self/fd").unwrap() {
            let entry = entry.unwrap();
            let Ok(target) = std::fs::read_link(entry.path()) else {
                continue;
            };
            if !target.to_string_lossy().contains("etilqs_") {
                continue;
            }
            let Ok(metadata) = std::fs::metadata(entry.path()) else {
                continue;
            };
            assert_eq!(metadata.mode() & 0o777, 0o600);
            assert_eq!(metadata.nlink(), 0);
            files.push((entry.path(), metadata.ino()));
        }
        assert!(
            !files.is_empty(),
            "fixture must actually spill beyond the TEMP cache"
        );
        drop(stage);
        for (path, inode) in files {
            assert!(!std::fs::metadata(path).is_ok_and(|m| m.ino() == inode));
        }
    }
}
