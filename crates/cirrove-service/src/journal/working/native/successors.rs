//! Exact native working lineage, using the existing upload dependency queue.
use super::*;
#[derive(Serialize, Deserialize)]
pub(super) struct Head {
    pub(super) owner: Uuid,
    pub(super) current: Node,
    pub(super) semantic: PackageSemanticIdentity,
    pub(super) sequence: u64,
}
pub(crate) fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS native_working_operations(operation TEXT PRIMARY KEY,working TEXT NOT NULL,owner TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS native_working_operation_owner ON native_working_operations(working,operation);
        CREATE TABLE IF NOT EXISTS native_working_heads(working TEXT PRIMARY KEY,body TEXT NOT NULL);
        CREATE UNIQUE INDEX IF NOT EXISTS native_working_head_owner ON native_working_heads(json_extract(body,'$.owner'));")?;
    Ok(())
}
fn association(db: &Connection, operation: Uuid) -> Result<Option<(Uuid, Uuid)>> {
    let row: Option<(String, String)> = db
        .query_row(
            "SELECT working,owner FROM native_working_operations WHERE operation=?1",
            [operation.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    row.map(|(w, o)| {
        Ok((
            Uuid::parse_str(&w).map_err(|_| JournalError::Corrupt)?,
            Uuid::parse_str(&o).map_err(|_| JournalError::Corrupt)?,
        ))
    })
    .transpose()
}
pub(super) fn head(db: &Connection, working: Uuid) -> Result<Head> {
    let body: String = db.query_row(
        "SELECT body FROM native_working_heads WHERE working=?1",
        [working.to_string()],
        |r| r.get(0),
    )?;
    Ok(serde_json::from_str(&body)?)
}
fn upload(db: &Connection, id: Uuid) -> Result<UploadRecord> {
    let body: String = db.query_row(
        "SELECT body FROM uploads WHERE id=?1",
        [id.to_string()],
        |r| r.get(0),
    )?;
    Ok(serde_json::from_str(&body)?)
}
fn check_previous(db: &Connection, working: &WorkingFile) -> Result<(UploadRecord, Uuid)> {
    let previous = working.latest.ok_or(JournalError::Stale)?;
    let (id, owner) = association(db, previous)?.ok_or(JournalError::Stale)?;
    let row = upload(db, previous)?;
    let (active, current) = atomic::active(db, owner)?;
    if active != working.id
        || !atomic::descendant(db, owner, id, working.id)?
        || current.owner != owner
        || row.scope != working.scope
        || package_replacement::original(&row.representation).is_none()
    {
        return Err(JournalError::Stale);
    }
    let object = namespace::by_id(db, owner)?;
    if object.scope != working.scope
        || !object.remote_owned
        || object.unlinked
        || object.node.kind != NodeKind::Folder
        || !object.node.package
        || object.latest != Some(previous)
        || object.remote.as_ref() != Some(&current.current)
    {
        return Err(JournalError::Stale);
    }
    Ok((row, owner))
}
/// A provisional source is never dispatchable until resolve rebases it.
pub(super) fn provisional(
    db: &Connection,
    working: &WorkingFile,
    binding: &Binding,
) -> Result<(Node, PackageSemanticIdentity)> {
    if working.latest.is_none() {
        return Ok((binding.source.clone(), binding.semantic.clone()));
    }
    let (row, _) = check_previous(db, working)?;
    if let Some((_, current, _)) = row.native_replacement_receipt() {
        return Ok((
            current.clone(),
            row.package_completion
                .clone()
                .ok_or(JournalError::Corrupt)?,
        ));
    }
    let UploadRepresentation::PackageReplacementArchive {
        original,
        original_semantic,
        ..
    } = row.representation
    else {
        return Err(JournalError::Stale);
    };
    Ok((*original, original_semantic))
}
/// Only the typed native generation caller may attach to its prior owner.
pub(crate) fn attach(
    tx: &rusqlite::Transaction<'_>,
    row: &UploadRecord,
    commit: &NativeCommit,
) -> Result<bool> {
    let Some(base) = &row.base else {
        return Ok(false);
    };
    if base.resolved || commit.record.latest != Some(base.predecessor) {
        return Err(JournalError::Stale);
    }
    let (previous, owner_id) = check_previous(tx, &commit.record)?;
    if previous.id != base.predecessor
        || previous.sequence >= row.sequence
        || row.scope != commit.record.scope
    {
        return Err(JournalError::Stale);
    }
    // Every unfinished operation touching either old or newly confirmed source
    // must belong to this exact working stream and source owner.
    let owner = namespace::by_id(tx, owner_id)?;
    let current = owner.remote.as_ref().ok_or(JournalError::Corrupt)?;
    let mut intents = vec![row.intent.clone()];
    intents.push(UploadIntent::Replace {
        item: current.id.clone(),
        expected_etag: current.etag.clone().ok_or(JournalError::Corrupt)?,
    });
    for intent in intents {
        for resource in mutations::upload_resources(&row.scope, &intent)? {
            let unrelated: bool = tx.query_row(
                "WITH RECURSIVE ancestors(id,depth) AS (
                    SELECT ?1,0 UNION ALL
                    SELECT t.previous_working,a.depth+1 FROM ancestors a
                    JOIN native_working_transfers t ON t.next_working=a.id
                    JOIN uploads u ON u.id=t.successor
                    JOIN native_working_operations n ON n.operation=t.successor
                        AND n.working=t.next_working AND n.owner=t.owner
                    WHERE t.owner=?2 AND a.depth<10000
                        AND json_extract(u.body,'$.representation.kind')='package_replacement_archive'
                        AND json_extract(u.body,'$.base.predecessor') IS t.predecessor
                ) SELECT EXISTS(
                    SELECT 1 FROM write_resources r JOIN write_queue q ON q.id=r.id
                    LEFT JOIN native_working_operations n ON n.operation=q.id
                    WHERE r.resource=?3 AND q.complete=0 AND q.id!=?4
                        AND (n.owner IS NULL OR n.owner!=?2 OR n.working NOT IN (SELECT id FROM ancestors))
                )",
                params![commit.record.id.to_string(),owner_id.to_string(),resource,row.id.to_string()],
                |r|r.get(0),
            )?;
            if unrelated {
                return Err(JournalError::Stale);
            }
        }
    }
    let mut owner = owner;
    owner.latest = Some(row.id);
    owner.revision = owner.revision.checked_add(1).ok_or(JournalError::Quota)?;
    namespace::save(tx, &owner)?;
    Ok(true)
}
pub(super) fn record(
    tx: &rusqlite::Transaction<'_>,
    working: &WorkingFile,
    row: &UploadRecord,
    binding: &Binding,
) -> Result<()> {
    let owner: String = tx.query_row(
        "SELECT object FROM namespace_operations WHERE operation=?1",
        [row.id.to_string()],
        |r| r.get(0),
    )?;
    let owner = Uuid::parse_str(&owner).map_err(|_| JournalError::Corrupt)?;
    if let Some(previous) = working.latest {
        let (prior, previous_owner) = association(tx, previous)?.ok_or(JournalError::Stale)?;
        if previous_owner != owner
            || !atomic::descendant(tx, owner, prior, working.id)?
            || row.base.as_ref().map(|b| b.predecessor) != Some(previous)
        {
            return Err(JournalError::Stale);
        }
    } else {
        if row.base.is_some()
            || package_replacement::original(&row.representation) != Some(&binding.source)
            || row.intent != working.intent
        {
            return Err(JournalError::Stale);
        }
        let object = namespace::by_id(tx, owner)?;
        let h = head(tx, working.id)?;
        if h.owner != owner
            || h.current != binding.source
            || h.semantic != binding.semantic
            || h.sequence != object.remote_sequence
        {
            return Err(JournalError::Stale);
        }
    }
    if association(tx, row.id)?.is_some_and(|actual| actual != (working.id, owner)) {
        return Err(JournalError::Stale);
    }
    tx.execute(
        "INSERT OR IGNORE INTO native_working_operations VALUES(?1,?2,?3)",
        params![
            row.id.to_string(),
            working.id.to_string(),
            owner.to_string()
        ],
    )?;
    Ok(())
}
pub(crate) fn resolve(journal: &mut UploadJournal, mut row: UploadRecord) -> Result<()> {
    let base = row.base.as_ref().ok_or(JournalError::Corrupt)?;
    if base.resolved {
        return Err(JournalError::Stale);
    }
    let (working, owner) = association(&journal.db, row.id)?.ok_or(JournalError::Stale)?;
    let (prior, prior_owner) =
        association(&journal.db, base.predecessor)?.ok_or(JournalError::Stale)?;
    if prior_owner != owner || !atomic::descendant(&journal.db, owner, prior, working)? {
        return Err(JournalError::Stale);
    }
    let previous = journal.get(base.predecessor)?;
    let (_, current, _) = previous
        .native_replacement_receipt()
        .ok_or(JournalError::Stale)?;
    let (active_working, h) = atomic::active(&journal.db, owner)?;
    if !atomic::descendant(&journal.db, owner, working, active_working)? {
        return Err(JournalError::Stale);
    }
    if previous.scope != row.scope
        || previous.sequence >= row.sequence
        || h.owner != owner
        || h.sequence != previous.sequence
        || h.current != *current
        || previous.package_completion.as_ref() != Some(&h.semantic)
    {
        return Err(JournalError::Stale);
    }
    let object = namespace::by_id(&journal.db, owner)?;
    if object.scope != row.scope
        || object.remote.as_ref() != Some(current)
        || !object.remote_owned
        || object.unlinked
    {
        return Err(JournalError::Stale);
    }
    let UploadRepresentation::PackageReplacementArchive {
        original,
        original_semantic,
        ..
    } = &mut row.representation
    else {
        return Err(JournalError::Intent);
    };
    if original.parent_id != current.parent_id || original.name != current.name {
        return Err(JournalError::Stale);
    }
    **original = current.clone();
    *original_semantic = h.semantic;
    row.intent = UploadIntent::Replace {
        item: current.id.clone(),
        expected_etag: current.etag.clone().ok_or(JournalError::Corrupt)?,
    };
    package_replacement::validate(&row.scope, &row.intent, &row.representation)?;
    row.base.as_mut().ok_or(JournalError::Corrupt)?.resolved = true;
    let tx = journal.db.transaction()?;
    for key in mutations::upload_resources(&row.scope, &row.intent)? {
        tx.execute(
            "INSERT OR IGNORE INTO write_resources(id,resource) VALUES(?1,?2)",
            params![row.id.to_string(), key],
        )?;
    }
    tx.execute(
        "UPDATE uploads SET resource=?2,body=?3 WHERE id=?1",
        params![
            row.id.to_string(),
            resource(&row.intent, &row.scope)?,
            serde_json::to_string(&row)?
        ],
    )?;
    tx.commit()?;
    Ok(())
}
pub(crate) fn confirmed_base(journal: &UploadJournal, row: &UploadRecord) -> Result<Node> {
    let base = row
        .base
        .as_ref()
        .filter(|b| b.resolved)
        .ok_or(JournalError::Stale)?;
    let association_current = association(&journal.db, row.id)?.ok_or(JournalError::Stale)?;
    let (prior, prior_owner) =
        association(&journal.db, base.predecessor)?.ok_or(JournalError::Stale)?;
    if prior_owner != association_current.1
        || !atomic::descendant(&journal.db, prior_owner, prior, association_current.0)?
    {
        return Err(JournalError::Stale);
    }
    let previous = journal.get(base.predecessor)?;
    let (_, current, _) = previous
        .native_replacement_receipt()
        .ok_or(JournalError::Stale)?;
    let UploadRepresentation::PackageReplacementArchive {
        original,
        original_semantic,
        ..
    } = &row.representation
    else {
        return Err(JournalError::Intent);
    };
    if row.scope != previous.scope
        || row.sequence <= previous.sequence
        || original.as_ref() != current
        || previous.package_completion.as_ref() != Some(original_semantic)
        || !matches!(&row.intent,UploadIntent::Replace{item,expected_etag} if item==&current.id && Some(expected_etag)==current.etag.as_ref())
    {
        return Err(JournalError::Stale);
    }
    Ok(current.clone())
}
/// Same transaction as receipt/namespace/recovery/queue completion. Never alters
/// newer local generation, bytes, dirty flag or latest queued operation.
pub(crate) fn acknowledge(tx: &rusqlite::Transaction<'_>, row: &UploadRecord) -> Result<()> {
    let Some((working, owner)) = association(tx, row.id)? else {
        return Ok(());
    };
    let (original, current, _) = row
        .native_replacement_receipt()
        .ok_or(JournalError::Corrupt)?;
    let semantic = row
        .package_completion
        .as_ref()
        .ok_or(JournalError::Corrupt)?;
    let (active_working, mut h) = atomic::active(tx, owner)?;
    if !atomic::descendant(tx, owner, working, active_working)? {
        return Err(JournalError::Stale);
    }
    let object = namespace::by_id(tx, owner)?;
    if h.owner != owner
        || object.scope != row.scope
        || object.remote.as_ref() != Some(current)
        || object.remote_sequence != row.sequence
        || !object.remote_owned
        || object.unlinked
    {
        return Err(JournalError::Corrupt);
    }
    if h.sequence == row.sequence && h.current == *current && h.semantic == *semantic {
        return Ok(());
    }
    let UploadRepresentation::PackageReplacementArchive {
        original_semantic, ..
    } = &row.representation
    else {
        return Err(JournalError::Corrupt);
    };
    if h.sequence >= row.sequence || h.current != *original || h.semantic != *original_semantic {
        return Err(JournalError::Stale);
    }
    h.current = current.clone();
    h.semantic = semantic.clone();
    h.sequence = row.sequence;
    if tx.execute(
        "UPDATE native_working_bindings SET source=?2 WHERE working=?1",
        params![
            active_working.to_string(),
            serde_json::to_string(&(&row.scope, &current.id))?
        ],
    )? != 1
    {
        return Err(JournalError::Corrupt);
    }
    tx.execute(
        "UPDATE native_working_heads SET body=?2 WHERE working=?1",
        params![active_working.to_string(), serde_json::to_string(&h)?],
    )?;
    projection::refresh_child(tx, active_working)?;
    Ok(())
}
#[cfg(test)]
pub(super) mod tests;

/// A queued later save must not prevent the head operation reserving recovery.
/// The exception is native-only and proves the exact bounded dependency path.
pub(crate) fn permits_reservation(
    db: &Connection,
    owner: &NamespaceObject,
    row: &UploadRecord,
) -> Result<bool> {
    let Some(latest) = owner.latest else {
        return Ok(false);
    };
    let Some((working, owned)) = association(db, row.id)? else {
        return Ok(false);
    };
    let (latest_working, latest_owner) = association(db, latest)?.ok_or(JournalError::Stale)?;
    if owned != owner.id
        || owner.scope != row.scope
        || latest_owner != owned
        || !atomic::descendant(db, owned, working, latest_working)?
    {
        return Ok(false);
    }
    let descendant:bool=db.query_row("WITH RECURSIVE chain(id,depth) AS (SELECT ?1,0 UNION ALL SELECT s.successor,c.depth+1 FROM chain c JOIN write_successors s ON s.predecessor=c.id WHERE c.depth<10000) SELECT EXISTS(SELECT 1 FROM chain WHERE id=?2)",params![row.id.to_string(),latest.to_string()],|r|r.get(0))?;
    Ok(descendant)
}
