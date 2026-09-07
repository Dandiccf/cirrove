//! Row-based directory snapshots and ordered, incremental metadata reads.
use super::*;

pub(super) fn migrate(tx: &rusqlite::Transaction<'_>, version: u32) -> Result<()> {
    if version < 5 {
        tx.execute_batch(
            "CREATE TABLE directory_entries (
                scope TEXT NOT NULL, parent TEXT NOT NULL, id TEXT NOT NULL,
                name TEXT NOT NULL, body TEXT NOT NULL,
                PRIMARY KEY(scope,parent,id),
                FOREIGN KEY(scope,parent) REFERENCES directories(scope,parent) ON DELETE CASCADE
            );
            CREATE INDEX directory_entry_name ON directory_entries(scope,parent,name,id);
            CREATE INDEX IF NOT EXISTS node_parent_name ON nodes(scope,json_extract(body,'$.parent_id'),json_extract(body,'$.name'),id);
            CREATE INDEX IF NOT EXISTS observed_parent_name ON observed(scope,json_extract(body,'$.parent_id'),json_extract(body,'$.name'),id);",
        )?;
        // Legacy arrays are expanded only during this atomic migration. A bad
        // body or duplicate identity aborts without dropping the original data.
        let invalid: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM directories WHERE json_type(body)!='array')",
            [],
            |r| r.get(0),
        )?;
        if invalid {
            return Err(StoreError::InvalidDirectorySnapshot);
        }
        tx.execute_batch(
            "INSERT INTO directory_entries(scope,parent,id,name,body)
                SELECT d.scope,d.parent,json_extract(j.value,'$.id'),json_extract(j.value,'$.name'),j.value
                FROM directories d,json_each(d.body) j;
            ALTER TABLE directories DROP COLUMN body;",
        )?;
    }
    Ok(())
}
pub(super) fn validate(db: &Connection) -> Result<()> {
    db.prepare("SELECT e.body,d.source_revision FROM directory_entries e JOIN directories d ON e.scope=d.scope AND e.parent=d.parent LIMIT 0")?;
    Ok(())
}

#[cfg(test)]
mod tests;

// Both sides of each UNION have matching name/identity indexes. SQLite can
// merge ordered rows instead of constructing an in-memory sort of the directory.
// Unary + prevents the revision filter from selecting the competing revision
// index followed by a full sort. Revisions and their bound parameters are i64;
// dropping SQL affinity here does not change their numeric comparison. Tests
// assert zero SORT and FULLSCAN operations on both listing paths.
const SNAPSHOT_CHILDREN: &str = "SELECT e.body,e.name AS name,e.id AS id FROM directory_entries e
    WHERE e.scope=?1 AND e.parent=?2
    AND NOT EXISTS(SELECT 1 FROM observed o WHERE o.scope=e.scope AND o.id=e.id AND +o.source_revision>?3)
    AND NOT EXISTS(SELECT 1 FROM observed_absent a WHERE a.scope=e.scope AND a.id=e.id AND a.source_revision>?3)
    UNION ALL SELECT o.body,json_extract(o.body,'$.name') AS name,o.id AS id FROM observed o
    WHERE o.scope=?1 AND json_extract(o.body,'$.parent_id')=?2 AND +o.source_revision>?3
    AND NOT EXISTS(SELECT 1 FROM observed_absent a WHERE a.scope=o.scope AND a.id=o.id AND a.source_revision>?3)
    ORDER BY name,id";
const INDEXED_CHILDREN: &str =
    "SELECT n.body,json_extract(n.body,'$.name') AS name,n.id AS id FROM nodes n
    WHERE n.scope=?1 AND json_extract(n.body,'$.parent_id')=?2
    AND NOT EXISTS(SELECT 1 FROM observed o WHERE o.scope=n.scope AND o.id=n.id)
    AND NOT EXISTS(SELECT 1 FROM observed_absent a WHERE a.scope=n.scope AND a.id=n.id)
    UNION ALL SELECT o.body,json_extract(o.body,'$.name') AS name,o.id AS id FROM observed o
    WHERE o.scope=?1 AND json_extract(o.body,'$.parent_id')=?2
    ORDER BY name,id";

pub(super) fn read_on<T>(
    db: &Connection,
    scope: &Scope,
    parent: &str,
    consume: impl FnOnce(&mut dyn Iterator<Item = Result<Node>>) -> T,
) -> Result<Option<T>> {
    let key = Store::key(scope)?;
    let revision = db
        .query_row(
            "SELECT source_revision FROM directories WHERE scope=?1 AND parent=?2",
            params![key, parent],
            |r| r.get::<_, i64>(0),
        )
        .optional()?;
    let sql = if revision.is_some() {
        SNAPSHOT_CHILDREN
    } else {
        let complete = db
            .query_row(
                "SELECT cursor IS NOT NULL FROM feeds WHERE scope=?1",
                [&key],
                |r| r.get::<_, bool>(0),
            )
            .optional()?
            .unwrap_or(false);
        if !complete {
            return Ok(None);
        }
        INDEXED_CHILDREN
    };
    let mut statement = db.prepare(sql)?;
    let mut rows = if let Some(revision) = revision {
        statement.query(params![key, parent, revision])?
    } else {
        statement.query(params![key, parent])?
    };
    let mut done = false;
    let mut iter = std::iter::from_fn(|| {
        if done {
            return None;
        }
        match rows.next() {
            Ok(Some(row)) => Some((|| {
                let body: String = row.get(0)?;
                Ok(serde_json::from_str(&body)?)
            })()),
            Ok(None) => {
                done = true;
                None
            }
            Err(error) => {
                done = true;
                Some(Err(error.into()))
            }
        }
    });
    Ok(Some(consume(&mut iter)))
}

pub(super) fn visit_on(
    db: &Connection,
    scope: &Scope,
    parent: &str,
    mut visit: impl FnMut(Node) -> Result<()>,
) -> Result<bool> {
    Ok(read_on(db, scope, parent, |rows| {
        for node in rows {
            visit(node?)?;
        }
        Ok::<_, StoreError>(())
    })?
    .transpose()?
    .is_some())
}

pub(super) fn write_snapshot(
    tx: &rusqlite::Transaction<'_>,
    scope: &Scope,
    parent: &str,
    nodes: &[Node],
    seen: i64,
    source_revision: i64,
) -> Result<()> {
    let key = Store::key(scope)?;
    tx.execute(
        "INSERT INTO directories(scope,parent,seen,source_revision) VALUES(?1,?2,?3,?4)
        ON CONFLICT(scope,parent) DO UPDATE SET seen=excluded.seen,source_revision=excluded.source_revision",
        params![key,parent,seen,source_revision],
    )?;
    tx.execute(
        "DELETE FROM directory_entries WHERE scope=?1 AND parent=?2",
        params![key, parent],
    )?;
    let mut insert = tx.prepare(
        "INSERT INTO directory_entries(scope,parent,id,name,body) VALUES(?1,?2,?3,?4,?5)",
    )?;
    for node in nodes {
        insert.execute(params![
            key,
            parent,
            node.id,
            node.name,
            serde_json::to_string(node)?
        ])?;
    }
    Ok(())
}
