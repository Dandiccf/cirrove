//! Publication tickets order network observations against completed metadata
//! changes. They neither hold a transaction during I/O nor rely on wall clocks.
use super::*;
mod directory_publication;
pub use directory_publication::{DirectoryPublication, DirectoryPublicationResult};
use rusqlite::Transaction;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug)]
pub struct ObservationTicket {
    scope: Scope,
    target: Target,
    database: String,
    revision: i64,
}
#[derive(Clone, Debug)]
enum Target {
    Node(String),
    Directory(String),
}
#[derive(Debug)]
pub enum ObservationResult<T> {
    Published {
        value: T,
        changed: bool,
    },
    /// The caller must use this committed view, or retry when it is unknown.
    /// It must never serve the discarded network result directly.
    Superseded(Option<T>),
}
#[derive(Debug)]
pub enum AbsenceResult {
    Published { changed: bool },
    Superseded(Option<Node>),
}

pub(super) fn migrate(tx: &Transaction<'_>, version: u32) -> Result<()> {
    if version < 4 {
        tx.execute_batch("CREATE TABLE IF NOT EXISTS metadata_clock(singleton INTEGER PRIMARY KEY CHECK(singleton=1),
            identity TEXT NOT NULL, revision INTEGER NOT NULL CHECK(revision>=0));
            INSERT OR IGNORE INTO metadata_clock SELECT 1,hex(randomblob(16)),max(
                coalesce((SELECT max(seen) FROM observed),0),
                coalesce((SELECT max(seen) FROM directories),0),
                coalesce((SELECT max(started) FROM rounds),0));
            CREATE TABLE IF NOT EXISTS metadata_versions(scope TEXT NOT NULL, kind INTEGER NOT NULL,
                identity TEXT NOT NULL, revision INTEGER NOT NULL, PRIMARY KEY(scope,kind,identity));")?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS observed_absent(scope TEXT NOT NULL,id TEXT NOT NULL,
            source_revision INTEGER NOT NULL, PRIMARY KEY(scope,id));",
        )?;
        for table in ["observed", "directories"] {
            let exists: bool=tx.query_row(&format!("SELECT EXISTS(SELECT 1 FROM pragma_table_info('{table}') WHERE name='source_revision')"),[],|r|r.get(0))?;
            if !exists {
                tx.execute_batch(&format!(
                    "ALTER TABLE {table} ADD COLUMN source_revision INTEGER NOT NULL DEFAULT 0;
                    UPDATE {table} SET source_revision=seen;"
                ))?;
            }
        }
        tx.execute_batch("CREATE INDEX IF NOT EXISTS observed_parent ON observed(scope,json_extract(body,'$.parent_id'),source_revision);")?;
    }
    Ok(())
}
pub(super) fn validate(db: &Connection) -> Result<()> {
    clock(db)?;
    db.prepare("SELECT scope,id,source_revision FROM observed_absent LIMIT 0")?;
    db.prepare("SELECT scope,kind,identity,revision FROM metadata_versions LIMIT 0")?;
    db.prepare("SELECT o.source_revision,d.source_revision FROM observed o JOIN directories d ON d.scope=o.scope LIMIT 0")?;
    Ok(())
}
pub(super) fn clock(db: &Connection) -> Result<(String, i64)> {
    Ok(db.query_row(
        "SELECT identity,revision FROM metadata_clock WHERE singleton=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?)
}
pub(super) fn advance(tx: &Transaction<'_>) -> Result<i64> {
    let revision = clock(tx)?.1.checked_add(1).ok_or(StoreError::OutOfOrder)?;
    tx.execute(
        "UPDATE metadata_clock SET revision=?1 WHERE singleton=1",
        [revision],
    )?;
    Ok(revision)
}
fn mark(tx: &Transaction<'_>, scope: &str, kind: i32, identity: &str, revision: i64) -> Result<()> {
    tx.execute(
        "INSERT INTO metadata_versions VALUES(?1,?2,?3,?4)
        ON CONFLICT(scope,kind,identity) DO UPDATE SET revision=excluded.revision",
        params![scope, kind, identity, revision],
    )?;
    Ok(())
}
/// Call before replacing indexed rows so both old and new parents are known.
pub(super) fn publish_delta(tx: &Transaction<'_>, key: &str, reset: bool) -> Result<()> {
    let any: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM staged WHERE scope=?1)",
        [key],
        |r| r.get(0),
    )?;
    if !reset && !any {
        return Ok(());
    }
    let revision = advance(tx)?;
    if reset {
        // A replacement baseline supersedes every earlier ticket for this scope.
        tx.execute("DELETE FROM metadata_versions WHERE scope=?1", [key])?;
        return mark(tx, key, 0, "", revision);
    }
    tx.execute(
        "INSERT INTO metadata_versions(scope,kind,identity,revision)
        SELECT scope,1,id,?2 FROM staged WHERE scope=?1
        ON CONFLICT(scope,kind,identity) DO UPDATE SET revision=excluded.revision",
        params![key, revision],
    )?;
    tx.execute("INSERT INTO metadata_versions(scope,kind,identity,revision)
        SELECT ?1,2,parent,?2 FROM (
            SELECT id AS parent FROM staged WHERE scope=?1
            UNION SELECT json_extract(body,'$.parent_id') FROM staged WHERE scope=?1
            UNION SELECT json_extract((SELECT body FROM nodes WHERE scope=?1 AND id=s.id),'$.parent_id') FROM staged s WHERE scope=?1
            UNION SELECT json_extract((SELECT body FROM observed WHERE scope=?1 AND id=s.id),'$.parent_id') FROM staged s WHERE scope=?1
        ) WHERE parent IS NOT NULL
        ON CONFLICT(scope,kind,identity) DO UPDATE SET revision=excluded.revision",params![key,revision])?;
    Ok(())
}
fn changed_after(db: &Connection, scope: &str, kind: i32, id: &str, revision: i64) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM metadata_versions
        WHERE scope=?1 AND kind=?2 AND identity=?3 AND revision>?4)",
        params![scope, kind, id, revision],
        |r| r.get(0),
    )?)
}
fn current(db: &Connection, ticket: &ObservationTicket, nodes: &[Node]) -> Result<bool> {
    if clock(db)?.0 != ticket.database {
        return Err(StoreError::OutOfOrder);
    }
    let scope = Store::key(&ticket.scope)?;
    if changed_after(db, &scope, 0, "", ticket.revision)? {
        return Ok(false);
    }
    let (kind, target) = match &ticket.target {
        Target::Node(id) => (1, id),
        Target::Directory(id) => (2, id),
    };
    if changed_after(db, &scope, kind, target, ticket.revision)? {
        return Ok(false);
    }
    if matches!(ticket.target, Target::Directory(_))
        && changed_after(db, &scope, 1, target, ticket.revision)?
    {
        return Ok(false);
    }
    // A deleted or moved item may have had no indexed parent when its delta
    // arrived. Checking returned item IDs prevents an old cold listing restoring it.
    let ids = serde_json::to_string(&nodes.iter().map(|n| &n.id).collect::<Vec<_>>())?;
    let newer: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM metadata_versions WHERE scope=?1
        AND kind=1 AND revision>?2 AND identity IN (SELECT value FROM json_each(?3)))",
        params![scope, ticket.revision, ids],
        |r| r.get(0),
    )?;
    if newer {
        return Ok(false);
    }
    if matches!(ticket.target, Target::Node(_))
        && let Some(parent) = nodes.first().and_then(|n| n.parent_id.as_deref())
        && changed_after(db, &scope, 2, parent, ticket.revision)?
    {
        return Ok(false);
    }
    Ok(true)
}
fn changed_items(
    tx: &Transaction<'_>,
    scope: &Scope,
    nodes: &[Node],
    removed: &[Node],
    parent: Option<&str>,
) -> Result<()> {
    let key = Store::key(scope)?;
    let revision = advance(tx)?;
    let mut parents = HashSet::new();
    if let Some(parent) = parent {
        parents.insert(parent.to_owned());
    }
    for node in nodes.iter().chain(removed) {
        mark(tx, &key, 1, &node.id, revision)?;
        if let Some(parent) = &node.parent_id {
            parents.insert(parent.clone());
        }
        if let Some(old) = Store::node_on(tx, scope, &node.id)?
            && let Some(parent) = old.parent_id
        {
            parents.insert(parent);
        }
    }
    for parent in parents {
        mark(tx, &key, 2, &parent, revision)?;
    }
    Ok(())
}
fn write_node(
    tx: &Transaction<'_>,
    scope: &Scope,
    node: &Node,
    seen: i64,
    source_revision: i64,
) -> Result<()> {
    tx.execute(
        "DELETE FROM observed_absent WHERE scope=?1 AND id=?2",
        params![Store::key(scope)?, node.id],
    )?;
    tx.execute(
        "INSERT INTO observed(scope,id,body,seen,source_revision) VALUES(?1,?2,?3,?4,?5)
        ON CONFLICT(scope,id) DO UPDATE SET body=excluded.body,seen=excluded.seen,source_revision=excluded.source_revision",
        params![
            Store::key(scope)?,
            node.id,
            serde_json::to_string(node)?,
            seen,
            source_revision
        ],
    )?;
    Ok(())
}
fn write_directory(
    tx: &Transaction<'_>,
    scope: &Scope,
    parent: &str,
    nodes: &[Node],
    source_revision: i64,
) -> Result<bool> {
    let previous = Store::children_on(tx, scope, parent)?;
    let changed = previous.as_deref() != Some(nodes);
    let previous = previous.as_deref().unwrap_or_default();
    let old = previous
        .iter()
        .map(|n| (n.id.as_str(), n))
        .collect::<HashMap<_, _>>();
    let updated = nodes
        .iter()
        .filter(|n| old.get(n.id.as_str()).copied() != Some(*n))
        .cloned()
        .collect::<Vec<_>>();
    let ids = serde_json::to_string(&nodes.iter().map(|n| &n.id).collect::<Vec<_>>())?;
    let mut removed = HashMap::new();
    for table in ["observed", "nodes"] {
        let mut query = tx.prepare(&format!(
            "SELECT body FROM {table} WHERE scope=?1 AND json_extract(body,'$.parent_id')=?2
            AND id NOT IN (SELECT value FROM json_each(?3))"
        ))?;
        let rows = query.query_map(params![Store::key(scope)?, parent, ids], |r| {
            r.get::<_, String>(0)
        })?;
        for row in rows {
            let node: Node = serde_json::from_str(&row?)?;
            removed.insert(node.id.clone(), node);
        }
    }
    // Even an unchanged complete listing confirms absence/presence. It must
    // supersede an older reply that would reintroduce a different snapshot.
    changed_items(
        tx,
        scope,
        &updated,
        &removed.values().cloned().collect::<Vec<_>>(),
        Some(parent),
    )?;
    // Keep the committed delta baseline intact. Negative observations hide
    // absent cached children until a later observation or feed supersedes them.
    // A known move to another parent must retain that newer location.
    for node in removed.values() {
        if Store::node_on(tx, scope, &node.id)?
            .is_none_or(|n| n.parent_id.as_deref() == Some(parent))
        {
            tx.execute(
                "DELETE FROM observed WHERE scope=?1 AND id=?2",
                params![Store::key(scope)?, node.id],
            )?;
            tx.execute(
                "INSERT INTO observed_absent VALUES(?1,?2,?3)
                ON CONFLICT(scope,id) DO UPDATE SET source_revision=excluded.source_revision",
                params![Store::key(scope)?, node.id, source_revision],
            )?;
        }
    }
    let seen = timestamp();
    for node in nodes {
        write_node(tx, scope, node, seen, source_revision)?;
    }
    directories::write_snapshot(tx, scope, parent, nodes, seen, source_revision)?;
    Ok(changed)
}
impl Store {
    /// Record a provider's NotFound with the same ordering as positive replies.
    /// This hides cached metadata without rewriting the completed delta baseline.
    pub fn publish_absence(&mut self, ticket: &ObservationTicket) -> Result<AbsenceResult> {
        let Target::Node(id) = &ticket.target else {
            return Err(StoreError::OutOfOrder);
        };
        let gate = self.gate.clone();
        let _write = hold(&gate);
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let old = Self::node_on(&tx, &ticket.scope, id)?;
        if !current(&tx, ticket, old.as_slice())? {
            return Ok(AbsenceResult::Superseded(old));
        }
        let key = Self::key(&ticket.scope)?;
        let absent: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM observed_absent WHERE scope=?1 AND id=?2)",
            params![key, id],
            |r| r.get(0),
        )?;
        let revision = advance(&tx)?;
        mark(&tx, &key, 1, id, revision)?;
        mark(&tx, &key, 2, id, revision)?;
        if let Some(parent) = old.as_ref().and_then(|n| n.parent_id.as_deref()) {
            mark(&tx, &key, 2, parent, revision)?;
        }
        tx.execute(
            "DELETE FROM observed WHERE scope=?1 AND id=?2",
            params![key, id],
        )?;
        tx.execute(
            "INSERT INTO observed_absent VALUES(?1,?2,?3)
            ON CONFLICT(scope,id) DO UPDATE SET source_revision=excluded.source_revision",
            params![key, id, ticket.revision],
        )?;
        tx.commit()?;
        Ok(AbsenceResult::Published {
            changed: old.is_some() || !absent,
        })
    }
    pub fn node_observation(&mut self, scope: &Scope, item: &str) -> Result<ObservationTicket> {
        self.observation(scope, Target::Node(item.into()))
    }
    pub fn directory_observation(
        &mut self,
        scope: &Scope,
        parent: &str,
    ) -> Result<ObservationTicket> {
        self.observation(scope, Target::Directory(parent.into()))
    }
    fn observation(&mut self, scope: &Scope, target: Target) -> Result<ObservationTicket> {
        let gate = self.gate.clone();
        let _write = hold(&gate);
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let database = clock(&tx)?.0;
        let revision = advance(&tx)?;
        tx.commit()?;
        Ok(ObservationTicket {
            scope: scope.clone(),
            target,
            database,
            revision,
        })
    }
    pub fn publish_node(
        &mut self,
        ticket: &ObservationTicket,
        node: &Node,
    ) -> Result<ObservationResult<Node>> {
        let Target::Node(id) = &ticket.target else {
            return Err(StoreError::OutOfOrder);
        };
        if id != &node.id {
            return Err(StoreError::OutOfOrder);
        }
        let gate = self.gate.clone();
        let _write = hold(&gate);
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if !current(&tx, ticket, std::slice::from_ref(node))? {
            return Ok(ObservationResult::Superseded(Self::node_on(
                &tx,
                &ticket.scope,
                id,
            )?));
        }
        let changed = Self::node_on(&tx, &ticket.scope, id)?.as_ref() != Some(node);
        changed_items(&tx, &ticket.scope, std::slice::from_ref(node), &[], None)?;
        write_node(&tx, &ticket.scope, node, timestamp(), ticket.revision)?;
        tx.commit()?;
        Ok(ObservationResult::Published {
            value: node.clone(),
            changed,
        })
    }
    pub fn publish_directory(
        &mut self,
        ticket: &ObservationTicket,
        nodes: &[Node],
    ) -> Result<ObservationResult<Vec<Node>>> {
        let Target::Directory(parent) = &ticket.target else {
            return Err(StoreError::OutOfOrder);
        };
        let gate = self.gate.clone();
        let _write = hold(&gate);
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if !current(&tx, ticket, nodes)? {
            return Ok(ObservationResult::Superseded(Self::children_on(
                &tx,
                &ticket.scope,
                parent,
            )?));
        }
        let changed = write_directory(&tx, &ticket.scope, parent, nodes, ticket.revision)?;
        tx.commit()?;
        Ok(ObservationResult::Published {
            value: nodes.to_vec(),
            changed,
        })
    }
    /// Immediate publication for already available metadata. For a network read,
    /// capture node_observation before starting I/O and finish with publish_node.
    pub fn observe_node(&mut self, scope: &Scope, node: &Node) -> Result<()> {
        let gate = self.gate.clone();
        let _write = hold(&gate);
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        changed_items(&tx, scope, std::slice::from_ref(node), &[], None)?;
        let source_revision = advance(&tx)?;
        write_node(&tx, scope, node, timestamp(), source_revision)?;
        tx.commit()?;
        Ok(())
    }
    /// Immediate publication. Network callers use a ticket captured before I/O.
    pub fn observe_directory(
        &mut self,
        scope: &Scope,
        parent: &str,
        nodes: &[Node],
    ) -> Result<bool> {
        let gate = self.gate.clone();
        let _write = hold(&gate);
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let source_revision = advance(&tx)?;
        let changed = write_directory(&tx, scope, parent, nodes, source_revision)?;
        tx.commit()?;
        Ok(changed)
    }
}
