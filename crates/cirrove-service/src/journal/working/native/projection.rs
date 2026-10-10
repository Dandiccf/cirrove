//! Explicit local archive presentation. Source Folder owns every cloud operation;
//! this File owns only the already validated, quota-accounted mutable bytes.
use super::*;
use crate::journal::NativeArchiveRole;

pub(super) fn attach(
    tx: &rusqlite::Transaction<'_>,
    record: &mut WorkingFile,
    binding: &Binding,
) -> Result<()> {
    binding.validate()?;
    if !record.native
        || record.scope != binding.scope
        || record.latest.is_some()
        || record.initial_remote.is_some()
        || record.unlinked
    {
        return Err(JournalError::Stale);
    }
    let mut owner = package_replacement::prepare_owner(
        tx,
        &binding.scope,
        &binding.source,
        None,
        Some(record.id),
    )?;
    owner.follows_remote = false;
    owner.revision = owner.revision.checked_add(1).ok_or(JournalError::Quota)?;
    namespace::save(tx, &owner)?;
    let previous = retirement::slot_for_owner(tx, owner.id)?;
    if previous
        .as_ref()
        .is_some_and(|slot| slot.working != record.id)
    {
        return Err(JournalError::Stale);
    }
    if previous.is_none()
        && tx.query_row("SELECT count(*) FROM namespace_objects", [], |r| {
            r.get::<_, i64>(0)
        })? >= 10_000
    {
        return Err(JournalError::Quota);
    }
    let head = successors::Head {
        owner: owner.id,
        current: binding.source.clone(),
        semantic: binding.semantic.clone(),
        sequence: owner.remote_sequence,
    };
    tx.execute(
        "INSERT INTO native_working_heads VALUES(?1,?2)",
        params![record.id.to_string(), serde_json::to_string(&head)?],
    )?;
    record.node.parent_id = Some(owner.node.id.clone());
    tx.execute(
        "UPDATE working_files SET body=?2 WHERE id=?1",
        params![record.id.to_string(), serde_json::to_string(record)?],
    )?;
    let object = NamespaceObject {
        native_archive: Some(NativeArchiveRole {
            retired: false,
            backed_up: false,
            source_owner: owner.id,
            working: record.id,
            artifact: binding.archive.id.clone(),
        }),
        id: record.id,
        scope: record.scope.clone(),
        names: owner.names,
        node: record.node.clone(),
        remote: None,
        remote_owned: false,
        remote_sequence: 0,
        working_file: Some(record.id),
        latest: None,
        revision: previous.as_ref().map_or(Ok(1), |slot| {
            namespace::by_id(tx, slot.working)?
                .revision
                .checked_add(1)
                .ok_or(JournalError::Quota)
        })?,
        follows_remote: false,
        unlinked: false,
    };
    namespace::save(tx, &object)
}
pub(crate) fn validate_child(db: &Connection, object: &NamespaceObject) -> Result<()> {
    let role = object
        .native_archive
        .as_ref()
        .ok_or(JournalError::Corrupt)?;
    if role.retired {
        return retirement::validate_retired_child(db, object);
    }
    let working: String = db.query_row(
        "SELECT body FROM working_files WHERE id=?1",
        [role.working.to_string()],
        |r| r.get(0),
    )?;
    let working: WorkingFile = serde_json::from_str(&working)?;
    let body: String = db.query_row(
        "SELECT body FROM native_working_bindings WHERE working=?1",
        [role.working.to_string()],
        |r| r.get(0),
    )?;
    let binding: Binding = serde_json::from_str(&body)?;
    binding.validate()?;
    let head = successors::head(db, role.working)?;
    let owner = namespace::by_id(db, role.source_owner)?;
    backup::validate_gap(db, object, &owner)?;
    if !object.valid_native_archive_shape(&working)
        || !object.valid_native_archive_owner(&owner)
        || binding.scope != object.scope
        || head.owner != owner.id
        || owner.remote.as_ref() != Some(&head.current)
        || owner.remote_sequence != head.sequence
    {
        return Err(JournalError::Stale);
    }
    let shared: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM namespace_operations WHERE object=?1)",
        [object.id.to_string()],
        |r| r.get(0),
    )?;
    if shared {
        return Err(JournalError::Corrupt);
    }
    Ok(())
}
pub(super) fn refresh_child(tx: &rusqlite::Transaction<'_>, working: Uuid) -> Result<()> {
    let head = successors::head(tx, working)?;
    let mut child = namespace::by_id(tx, working)?;
    let role = child.native_archive.as_mut().ok_or(JournalError::Corrupt)?;
    if role.source_owner != head.owner || role.working != working {
        return Err(JournalError::Stale);
    }
    role.artifact = format!("icloud-artifact:{}", head.current.id);
    child.revision = child.revision.checked_add(1).ok_or(JournalError::Quota)?;
    namespace::save(tx, &child)
}
#[cfg(test)]
mod tests;
