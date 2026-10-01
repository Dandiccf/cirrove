//! Source binding for explicitly generated directory snapshots. Old children
//! stay complete until a replacement and its source publish in one transaction.
use super::*;

#[derive(Clone, Debug)]
pub struct DirectorySourceState {
    pub bound: Option<Node>,
    /// Historical package classification retained during migration, NOT proof
    /// that these source bytes produced the old snapshot. Never compare its
    /// revision as though it were a successful generated publication.
    pub legacy_classification: Option<Node>,
    pub current: Option<Node>,
    /// A source change was actually observed after this snapshot. A migrated
    /// unbound snapshot does not manufacture a previously verified source.
    pub changed: bool,
}

pub(super) fn migrate(tx: &rusqlite::Transaction<'_>, version: u32) -> Result<()> {
    if version < 8 {
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS directory_sources(
            scope TEXT NOT NULL,parent TEXT NOT NULL,source TEXT,legacy_source TEXT,
            CHECK((source IS NULL) != (legacy_source IS NULL)),
            PRIMARY KEY(scope,parent),
            FOREIGN KEY(scope,parent) REFERENCES directories(scope,parent) ON DELETE CASCADE)
            WITHOUT ROWID;
            INSERT OR IGNORE INTO directory_sources(scope,parent,legacy_source)
            SELECT d.scope,d.parent,coalesce(o.body,n.body) FROM directories d
            LEFT JOIN observed o ON o.scope=d.scope AND o.id=d.parent
            LEFT JOIN nodes n ON n.scope=d.scope AND n.id=d.parent
            WHERE json_extract(coalesce(o.body,n.body),'$.id')=d.parent
            AND json_extract(coalesce(o.body,n.body),'$.package')=1
            AND json_extract(coalesce(o.body,n.body),'$.kind')='folder'
            AND coalesce(json_type(coalesce(o.body,n.body),'$.target'),'null')='null';",
        )?;
    }
    Ok(())
}
pub(super) fn validate(db: &Connection) -> Result<()> {
    db.prepare("SELECT scope,parent,source,legacy_source FROM directory_sources LIMIT 0")?;
    Ok(())
}
pub(super) fn valid(node: &Node) -> bool {
    node.package
        && node.kind == cirrove_core::NodeKind::Folder
        && node.target.is_none()
        && node.content_revision().is_some()
}
pub(super) fn same(before: &Node, after: &Node) -> bool {
    valid(before)
        && valid(after)
        && before.id == after.id
        && before.parent_id == after.parent_id
        && before.name == after.name
        && before.size == after.size
        && before.content_revision() == after.content_revision()
}
impl Store {
    /// Observe the completed snapshot binding and current source in one local
    /// read transaction. None means there is no completed directory snapshot.
    pub fn directory_source_state(
        &self,
        scope: &Scope,
        parent: &str,
    ) -> Result<Option<DirectorySourceState>> {
        let tx = self.db.unchecked_transaction()?;
        let key = Self::key(scope)?;
        let row: Option<(i64, Option<String>, Option<String>)> = tx
            .query_row(
                "SELECT d.source_revision,b.source,b.legacy_source FROM directories d
            LEFT JOIN directory_sources b ON b.scope=d.scope AND b.parent=d.parent
            WHERE d.scope=?1 AND d.parent=?2",
                params![key, parent],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((revision, body, legacy)) = row else {
            return Ok(None);
        };
        let bound: Option<Node> = body.map(|body| serde_json::from_str(&body)).transpose()?;
        let legacy_classification: Option<Node> =
            legacy.map(|body| serde_json::from_str(&body)).transpose()?;
        let current = Self::node_on(&tx, scope, parent)?;
        let changed = if let Some(bound) = &bound {
            !current.as_ref().is_some_and(|current| same(bound, current))
        } else if legacy_classification.as_ref().is_some_and(|legacy| {
            !current
                .as_ref()
                .is_some_and(|node| valid(node) && node.id == legacy.id)
        }) {
            true
        } else {
            tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM metadata_versions
                WHERE scope=?1 AND ((kind=1 AND identity=?2) OR (kind=0 AND identity='')) AND revision>?3)",
                params![key, parent, revision],
                |r| r.get(0),
            )?
        };
        tx.commit()?;
        Ok(Some(DirectorySourceState {
            bound,
            legacy_classification,
            current,
            changed,
        }))
    }
}

#[cfg(test)]
mod tests;
