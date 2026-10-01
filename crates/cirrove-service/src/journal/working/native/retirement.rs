//! Bounded dormant native slot and atomic clean-byte retirement. Callers must
//! exclude source/child descriptors, flights and
//! captures, and obtain exact fresh metadata BEFORE entering journal locks.
use super::*;
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct RetiredSlot {
    pub(super) working: Uuid,
    owner: Uuid,
    pub(super) generation: u64,
    scope: Scope,
    current: Node,
}
/// Runtime-only reservation. Persisted dormant history and current namespace
/// owner are separate: the old receipt never certifies freshly hydrated content.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Reactivation {
    pub(super) slot: RetiredSlot,
    owner_snapshot: String,
    child_snapshot: String,
}
fn idle(db: &Connection, owner: &NamespaceObject, current: &Node) -> Result<()> {
    let pending: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM write_queue q INDEXED BY native_retirement_pending CROSS JOIN namespace_operations n ON n.operation=q.id WHERE q.complete=0 AND n.object=?1) OR EXISTS(SELECT 1 FROM package_metadata_publication p INDEXED BY package_metadata_due CROSS JOIN namespace_operations n ON n.operation=p.operation WHERE p.done=0 AND n.object=?1)", [owner.id.to_string()], |r|r.get(0))?;
    if pending {
        return Err(JournalError::Stale);
    }
    let intent = UploadIntent::Replace {
        item: current.id.clone(),
        expected_etag: current.etag.clone().ok_or(JournalError::Stale)?,
    };
    for resource in mutations::upload_resources(&owner.scope, &intent)? {
        let busy:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM write_queue q INDEXED BY native_retirement_pending CROSS JOIN write_resources r ON r.id=q.id WHERE r.resource=?1 AND q.complete=0)",[resource],|r|r.get(0))?;
        if busy {
            return Err(JournalError::Stale);
        }
    }
    Ok(())
}
/// Called only after full fresh archive validation and immediately before
/// attaching bytes, within the same transaction. No provider I/O occurs here.
pub(super) fn prepare_reactivation(
    tx: &rusqlite::Transaction<'_>,
    binding: &Binding,
    selected: &Reactivation,
) -> Result<()> {
    let slot = slot_for_owner(tx, selected.slot.owner)?.ok_or(JournalError::Stale)?;
    let owner = namespace::by_id(tx, slot.owner)?;
    if slot != selected.slot
        || serde_json::to_string(&owner)? != selected.owner_snapshot
        || serde_json::to_string(&namespace::by_id(tx, slot.working)?)? != selected.child_snapshot
    {
        return Err(JournalError::Stale);
    }
    idle(tx, &owner, &binding.source)?;
    // Followed validates SAME remote identity and preserves local owner UUID.
    // This revision is certified by newly validated Binding, never old Head.
    let mut owner = owner.followed(binding.source.clone())?;
    directories::localize_parent(tx, &owner.scope, &mut owner.node)?;
    namespace::save(tx, &owner)
}
pub(super) fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS native_retired_slots(working TEXT PRIMARY KEY, owner TEXT NOT NULL UNIQUE, body TEXT NOT NULL); CREATE INDEX IF NOT EXISTS native_retirement_pending ON write_queue(id) WHERE complete=0;")?;
    Ok(())
}
pub(super) fn slot_for_owner(db: &Connection, owner: Uuid) -> Result<Option<RetiredSlot>> {
    let body: Option<String> = db
        .query_row(
            "SELECT body FROM native_retired_slots WHERE owner=?1",
            [owner.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    body.map(|s| serde_json::from_str(&s).map_err(Into::into))
        .transpose()
}
pub(super) fn validate_retired_child(db: &Connection, child: &NamespaceObject) -> Result<()> {
    let role = child.native_archive.as_ref().ok_or(JournalError::Corrupt)?;
    let slot = slot_for_owner(db, role.source_owner)?.ok_or(JournalError::Corrupt)?;
    let owner = namespace::by_id(db, role.source_owner)?;
    let active: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM working_files WHERE id=?1) OR EXISTS(SELECT 1 FROM native_working_heads WHERE working=?1) OR EXISTS(SELECT 1 FROM native_working_bindings WHERE working=?1)", [child.id.to_string()], |r|r.get(0))?;
    if active
        || slot.owner != role.source_owner
        || slot.working != child.id
        || slot.scope != child.scope
        || !child.valid_native_archive_owner(&owner)
    {
        return Err(JournalError::Stale);
    }
    Ok(())
}
/// Admit a fresh resolver lookup for an exact dormant identity only. This
/// carries no content proof: reserve/validate/publication still bind the fresh
/// source and archive and CAS the persisted marker, owner and child snapshots.
pub(super) fn select_dormant_source(j: &UploadJournal, owner: &NamespaceObject) -> Result<bool> {
    let Some(slot) = slot_for_owner(&j.db, owner.id)? else {
        return Ok(false);
    };
    let remote = owner.remote.as_ref().ok_or(JournalError::Stale)?;
    let child = namespace::by_id(&j.db, slot.working)?;
    validate_retired_child(&j.db, &child)?;
    let pending: bool = j.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM retired_working WHERE id=?1)",
        [slot.working.to_string()],
        |r| r.get(0),
    )?;
    if slot.scope != owner.scope
        || slot.current.id != remote.id
        || !owner.follows_remote
        || owner.latest.is_some()
        || owner.unlinked
        || !owner.remote_owned
        || owner.node.target.is_some()
        || pending
        || j.working.join(slot.working.to_string()).try_exists()?
    {
        return Err(JournalError::Stale);
    }
    idle(&j.db, owner, remote)?;
    Ok(true)
}
pub(super) fn select_slot(j: &UploadJournal, binding: &Binding) -> Result<Option<Reactivation>> {
    let Some(owner) = namespace::by_remote(&j.db, &binding.scope, &binding.source.id)? else {
        return Ok(None);
    };
    let Some(slot) = slot_for_owner(&j.db, owner.id)? else {
        return Ok(None);
    };
    let child = namespace::by_id(&j.db, slot.working)?;
    validate_retired_child(&j.db, &child)?;
    let pending: bool = j.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM retired_working WHERE id=?1)",
        [slot.working.to_string()],
        |r| r.get(0),
    )?;
    if pending
        || j.working.join(slot.working.to_string()).try_exists()?
        || slot.scope != binding.scope
        || slot.current.id != binding.source.id
        || owner
            .remote
            .as_ref()
            .is_none_or(|node| node.id != binding.source.id)
        || !owner.remote_owned
        || owner.node.kind != NodeKind::Folder
        || !owner.node.package
        || owner.node.target.is_some()
        || !owner.follows_remote
        || owner.latest.is_some()
        || owner.unlinked
    {
        return Err(JournalError::Stale);
    }
    idle(&j.db, &owner, &binding.source)?;
    Ok(Some(Reactivation {
        slot,
        owner_snapshot: serde_json::to_string(&owner)?,
        child_snapshot: serde_json::to_string(&child)?,
    }))
}
pub(super) fn recheck_slot(
    j: &UploadJournal,
    binding: &Binding,
    expected: Option<&Reactivation>,
) -> Result<()> {
    if select_slot(j, binding)?.as_ref() != expected {
        return Err(JournalError::Stale);
    }
    Ok(())
}
/// Opaque local snapshot, never provider evidence. The final call separately
/// requires a fresh exact revision observation. Receipt semantic proof remains
/// authoritative only while that complete provider Node is unchanged.
pub(crate) struct NativeRetirementCandidate {
    pub(crate) working: Uuid,
    pub(crate) scope: Scope,
    pub(crate) owner_local: String,
    pub(crate) child_local: String,
    pub(crate) original_archive: String,
    snapshot: Vec<String>,
    pub(crate) current: Node,
}
impl UploadJournal {
    pub(crate) fn native_retirement_for_owner(
        &self,
        owner: Uuid,
    ) -> Result<Option<NativeRetirementCandidate>> {
        let working: Option<String> = self
            .db
            .query_row(
                "SELECT working FROM native_working_heads WHERE json_extract(body,'$.owner')=?1",
                [owner.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        let Some(working) = working else {
            return Ok(None);
        };
        let working = Uuid::parse_str(&working).map_err(|_| JournalError::Corrupt)?;
        match self.native_retirement_candidate(working) {
            Ok(candidate) => Ok(Some(candidate)),
            Err(JournalError::Stale | JournalError::Missing) => Ok(None),
            Err(error) => Err(error),
        }
    }
    pub(crate) fn native_retirement_preview(
        &self,
        selected: &NativeRetirementCandidate,
        observed: &Node,
    ) -> Result<NamespacePublication> {
        let current = self.native_retirement_candidate(selected.working)?;
        if current.snapshot != selected.snapshot || &current.current != observed {
            return Err(JournalError::Stale);
        }
        let mut child = self.namespace_object(selected.working)?;
        let owner = self.namespace_object(
            child
                .native_archive
                .as_ref()
                .ok_or(JournalError::Corrupt)?
                .source_owner,
        )?;
        let owner = self.following_namespace(&owner, observed.clone())?;
        child.unlinked = true;
        child.working_file = None;
        child
            .native_archive
            .as_mut()
            .ok_or(JournalError::Corrupt)?
            .retired = true;
        child.revision = child.revision.checked_add(1).ok_or(JournalError::Quota)?;
        let after: i64 = self.db.query_row(
            "SELECT value FROM namespace_clock WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        let after = u64::try_from(after).map_err(|_| JournalError::Corrupt)?;
        Ok(NamespacePublication {
            after,
            through: after.checked_add(2).ok_or(JournalError::Quota)?,
            objects: vec![
                NamespaceSnapshot {
                    native_local: None,
                    object: owner,
                    working: None,
                },
                NamespaceSnapshot {
                    native_local: None,
                    object: child,
                    working: None,
                },
            ],
        })
    }
    pub(crate) fn native_retirement_candidate(
        &self,
        id: Uuid,
    ) -> Result<NativeRetirementCandidate> {
        let working = self.working_file(id)?;
        let child = self.namespace_object(id)?;
        projection::validate_child(&self.db, &child)?;
        let head = successors::head(&self.db, id)?;
        let owner = self.namespace_object(head.owner)?;
        let temporary: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM native_temporary_streams WHERE owner=?1)",
            [owner.id.to_string()],
            |r| r.get(0),
        )?;
        if temporary {
            return Err(JournalError::Stale);
        }
        let binding = self.native_binding(id)?;
        let latest = working.latest.ok_or(JournalError::Stale)?;
        let row = self.get(latest)?;
        let (_, current, _) = row
            .native_replacement_receipt()
            .ok_or(JournalError::Stale)?;
        if !working.native
            || working.dirty
            || working.unlinked
            || child.unlinked
            || row.id != latest
            || row.scope != working.scope
            || owner.latest != Some(latest)
            || owner.follows_remote
            || owner.unlinked
            || working.scope.account != self.account
            || working.scope != owner.scope
            || current != &head.current
            || row.package_completion.as_ref() != Some(&head.semantic)
            || head.sequence != row.sequence
            || owner.remote_sequence != row.sequence
            || slot_for_owner(&self.db, owner.id)?.is_some()
            || !matches!(self.package_publication_status(latest)?, package_publication::PackagePublicationStatus::Present(ref node) if node == current)
        {
            return Err(JournalError::Stale);
        }
        let linked: bool = self.db.query_row("SELECT EXISTS(SELECT 1 FROM native_working_operations WHERE operation=?1 AND working=?2 AND owner=?3)",params![latest.to_string(),id.to_string(),owner.id.to_string()],|r|r.get(0))?;
        if !linked {
            return Err(JournalError::Stale);
        }
        // Start from pending indices, never scan this owner's completed history.
        let pending: bool = self.db.query_row("SELECT EXISTS(SELECT 1 FROM write_queue q INDEXED BY native_retirement_pending CROSS JOIN namespace_operations n ON n.operation=q.id WHERE q.complete=0 AND n.object=?1) OR EXISTS(SELECT 1 FROM package_metadata_publication p INDEXED BY package_metadata_due CROSS JOIN namespace_operations n ON n.operation=p.operation WHERE p.done=0 AND n.object=?1)", [owner.id.to_string()], |r|r.get(0))?;
        if pending {
            return Err(JournalError::Stale);
        }
        let intent = UploadIntent::Replace {
            item: current.id.clone(),
            expected_etag: current.etag.clone().ok_or(JournalError::Stale)?,
        };
        for resource in mutations::upload_resources(&working.scope, &intent)? {
            let busy: bool = self.db.query_row("SELECT EXISTS(SELECT 1 FROM write_queue q INDEXED BY native_retirement_pending CROSS JOIN write_resources r ON r.id=q.id WHERE r.resource=?1 AND q.complete=0)", [resource], |r|r.get(0))?;
            if busy {
                return Err(JournalError::Stale);
            }
        }
        let snapshot = vec![
            serde_json::to_string(&working)?,
            serde_json::to_string(&child)?,
            serde_json::to_string(&owner)?,
            serde_json::to_string(&head)?,
            serde_json::to_string(&binding)?,
            serde_json::to_string(&row)?,
        ];
        Ok(NativeRetirementCandidate {
            working: id,
            scope: working.scope.clone(),
            owner_local: owner.node.id.clone(),
            child_local: child.node.id.clone(),
            original_archive: binding.archive.id.clone(),
            snapshot,
            current: current.clone(),
        })
    }
    /// Must hold exclusive source/child activity admission through publication.
    /// This API performs no I/O verification; the maintenance caller supplies it.
    pub(crate) fn retire_native_working(
        &mut self,
        selected: &NativeRetirementCandidate,
        freshly_observed: &Node,
    ) -> Result<()> {
        let now = self.native_retirement_candidate(selected.working)?;
        if now.snapshot != selected.snapshot || &now.current != freshly_observed {
            return Err(JournalError::Stale);
        }
        let working = self.working_file(selected.working)?;
        let mut child = self.namespace_object(working.id)?;
        let mut owner = self.namespace_object(
            child
                .native_archive
                .as_ref()
                .ok_or(JournalError::Corrupt)?
                .source_owner,
        )?;
        owner = self.following_namespace(&owner, freshly_observed.clone())?;
        child.unlinked = true;
        child.working_file = None;
        child
            .native_archive
            .as_mut()
            .ok_or(JournalError::Corrupt)?
            .retired = true;
        child.revision = child.revision.checked_add(1).ok_or(JournalError::Quota)?;
        let slot = RetiredSlot {
            working: working.id,
            owner: owner.id,
            generation: working.generation,
            scope: working.scope,
            current: freshly_observed.clone(),
        };
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO native_retired_slots VALUES(?1,?2,?3)",
            params![
                slot.working.to_string(),
                slot.owner.to_string(),
                serde_json::to_string(&slot)?
            ],
        )?;
        tx.execute(
            "INSERT INTO retired_working VALUES(?1)",
            [working.id.to_string()],
        )?;
        for table in [
            "working_files",
            "native_working_bindings",
            "native_working_heads",
        ] {
            let column = if table == "working_files" {
                "id"
            } else {
                "working"
            };
            if tx.execute(
                &format!("DELETE FROM {table} WHERE {column}=?1"),
                [working.id.to_string()],
            )? != 1
            {
                return Err(JournalError::Stale);
            }
        }
        namespace::save(&tx, &owner)?;
        namespace::save(&tx, &child)?;
        tx.commit()?;
        Ok(())
    }
}
#[cfg(test)]
mod tests;
