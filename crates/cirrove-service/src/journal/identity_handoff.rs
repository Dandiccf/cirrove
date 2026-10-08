//! Reserve and publish both provider identities of a staged replacement.
//! A hidden recovery object owns the former item after a verified handoff;
//! no provider request runs while either SQLite transaction is held.
use super::*;
use cirrove_core::mutation::{MutationIntent, MutationReceipt};
use cirrove_core::upload::{PackageHandoffReceipt, RecoveryLocation};
use rusqlite::Transaction;

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Reservation {
    recovery_object: Uuid,
    old_item: String,
    /// Full validated pre-provider identity for local metadata CAS only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    metadata_original: Option<Box<Node>>,
    recovery_name: String,
    #[serde(default)]
    trash_parent: Option<String>,
    #[serde(default)]
    pub(super) backup: Option<Node>,
    /// Exact pending atomic takeover which authorized its earlier source save.
    /// Old reservation bodies carry no such authority and deserialize as None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) atomic_source: Option<Uuid>,
}

impl UploadRecord {
    /// The persisted naming contract reserved before provider activity. This
    /// accessor grants no new authority; callers must validate the operation.
    pub(crate) fn reserved_recovery_location(&self) -> Option<RecoveryLocation> {
        self.identity_handoff
            .as_ref()
            .map(|reservation| match &reservation.trash_parent {
                Some(parent) => RecoveryLocation::Trash {
                    local_name: reservation.recovery_name.clone(),
                    parent: parent.clone(),
                },
                None => RecoveryLocation::Sibling {
                    name: reservation.recovery_name.clone(),
                },
            })
    }
}

/// Local directory IDs are stable across provider confirmation. A nested
/// file's local parent therefore differs from the exact provider parent;
/// walk only confirmed folder bindings, never infer an owner from a path.
pub(super) fn confirmed_parent_route(
    db: &Connection,
    scope: &Scope,
    local_parent: Option<&str>,
    remote_parent: Option<&str>,
) -> Result<bool> {
    let (Some(local), Some(remote)) = (local_parent, remote_parent) else {
        return Ok(false);
    };
    let (mut local, mut remote) = (local.to_owned(), remote.to_owned());
    let mut visited = std::collections::HashSet::new();
    for _ in 0..128 {
        if local == remote {
            return Ok(true);
        }
        if !visited.insert(local.clone()) {
            return Ok(false);
        }
        let Some(folder) = namespace::by_local(db, scope, &local)? else {
            return Ok(false);
        };
        let Some(bound) = folder.remote.as_ref() else {
            return Ok(false);
        };
        if folder.scope != *scope
            || folder.unlinked
            || !folder.remote_owned
            || folder.node.id != local
            || folder.node.kind != NodeKind::Folder
            || folder.node.target.is_some()
            || bound.id != remote
            || bound.kind != NodeKind::Folder
            || bound.target.is_some()
            || folder.node.name != bound.name
        {
            return Ok(false);
        }
        if let Some(latest) = folder.latest {
            let body: String = db.query_row(
                "SELECT body FROM mutations WHERE id=?1",
                [latest.to_string()],
                |row| row.get(0),
            )?;
            let mutation: MutationRecord = serde_json::from_str(&body)?;
            if mutation.state != MutationState::Applied
                || !matches!(mutation.receipt, Some(MutationReceipt::Upsert(ref node)) if node == bound)
            {
                return Ok(false);
            }
        }
        let (Some(next_local), Some(next_remote)) =
            (folder.node.parent_id.as_deref(), bound.parent_id.as_deref())
        else {
            return Ok(false);
        };
        local = next_local.to_owned();
        remote = next_remote.to_owned();
    }
    Ok(false)
}

impl UploadJournal {
    /// Reserve the old item before the first remote handoff mutation. This
    /// requires a proven current owner or a durable editor replacement whose
    /// victim still owns the old ID. Successors resolve only from its receipt.
    pub fn reserve_identity_handoff(
        &mut self,
        id: Uuid,
        attempt: Uuid,
        location: RecoveryLocation,
    ) -> Result<Uuid> {
        let (recovery_name, trash_parent) = match location {
            RecoveryLocation::Sibling { name } => (name, None),
            RecoveryLocation::Trash { local_name, parent } => (local_name, Some(parent)),
        };
        let mut record = self.active_attempt(id, attempt)?;
        let native = package_replacement::original(&record.representation);
        if native.is_some() {
            package_replacement::validate(&record.scope, &record.intent, &record.representation)?;
        }
        if !record.representation.is_file_bytes() && native.is_none() {
            return Err(JournalError::Intent);
        }
        if native.is_some()
            && trash_parent.as_deref() != Some("FOLDER::com.apple.CloudDocs::TRASH_ROOT")
        {
            return Err(JournalError::Intent);
        }
        let UploadIntent::Replace {
            item,
            expected_etag,
        } = &record.intent
        else {
            return Err(JournalError::Intent);
        };
        let owner = self
            .namespace_for_operation(id)?
            .ok_or(JournalError::Stale)?;
        let victim = replacements::handoff_victim(&self.db, &record, &owner)?;
        let unlinked_remove =
            victim.is_none() && ordinary_remove_successor(&self.db, &record, &owner)?;
        let atomic_source = if victim.is_none() {
            ordinary_atomic_source_prerequisite(&self.db, &record, &owner)?
        } else {
            None
        };
        if victim.is_none()
            && ordinary_atomic_source_required(&self.db, &record, &owner)?
            && atomic_source.is_none()
        {
            return Err(JournalError::Stale);
        }
        if record.identity_handoff.is_some()
            && record.representation.is_file_bytes()
            && victim.is_none()
            && !unlinked_remove
            && atomic_source.is_none()
            && owner.latest != Some(id)
            && !ordinary_linear_successor(&self.db, &record, &owner)?
        {
            return Err(JournalError::Stale);
        }
        if let Some(reservation) = &record.identity_handoff {
            let recovery = self.namespace_object(reservation.recovery_object)?;
            if (owner.unlinked && victim.is_none() && !unlinked_remove)
                || reservation.old_item != *item
                || reservation.recovery_name != recovery_name
                || reservation.trash_parent != trash_parent
                || reservation.backup.is_some()
                || recovery.scope != record.scope
                || !recovery.unlinked
                || recovery.remote_owned
                || recovery.remote.as_ref().is_none_or(|node| node.id != *item)
            {
                return Err(JournalError::Stale);
            }
            let recovery_object = reservation.recovery_object;
            if atomic_source.is_some() && reservation.atomic_source != owner.latest {
                record
                    .identity_handoff
                    .as_mut()
                    .ok_or(JournalError::Corrupt)?
                    .atomic_source = owner.latest;
                if self.db.execute(
                    "UPDATE uploads SET body=?2 WHERE id=?1",
                    params![id.to_string(), serde_json::to_string(&record)?],
                )? != 1
                {
                    return Err(JournalError::Missing);
                }
            }
            return Ok(recovery_object);
        }
        if record.state != UploadState::Uploading
            && !(record.state == UploadState::Verifying
                && record.session_key.is_none()
                && record.transferred_bytes == 0)
        {
            return Err(JournalError::Stale);
        }
        let old_owner = victim.as_ref().unwrap_or(&owner);
        let old = old_owner.remote.as_ref().ok_or(JournalError::Stale)?;
        let parent_matches = confirmed_parent_route(
            &self.db,
            &record.scope,
            atomic_source
                .as_ref()
                .unwrap_or(&owner.node)
                .parent_id
                .as_deref(),
            old.parent_id.as_deref(),
        )?;
        if owner.scope != record.scope
            || (owner.unlinked && victim.is_none() && !unlinked_remove)
            || owner.follows_remote
            || !owner.remote_owned
            || (owner.latest != Some(id)
                && !unlinked_remove
                && atomic_source.is_none()
                && !(native.is_some()
                    && working::native::successors::permits_reservation(
                        &self.db, &owner, &record,
                    )?))
            || old.id != *item
            || old.etag.as_deref() != Some(expected_etag)
            || if native.is_some() {
                old.kind != NodeKind::Folder || !old.package || native != Some(old)
            } else {
                old.kind != NodeKind::File || old.package
            }
            || old.target.is_some()
            || old.parent_id.is_none()
            || (owner.node.name != old.name && atomic_source.is_none())
            || !parent_matches
        {
            return Err(JournalError::Stale);
        }
        UploadIntent::Create {
            parent: old.parent_id.clone().ok_or(JournalError::Corrupt)?,
            name: recovery_name.clone(),
        }
        .validate()
        .map_err(|_| JournalError::Intent)?;
        if recovery_name == old.name {
            return Err(JournalError::Intent);
        }
        if trash_parent
            .as_ref()
            .is_some_and(|parent| parent.is_empty() || old.parent_id.as_ref() == Some(parent))
        {
            return Err(JournalError::Intent);
        }
        let count: i64 =
            self.db
                .query_row("SELECT count(*) FROM namespace_objects", [], |row| {
                    row.get(0)
                })?;
        if count >= 10_000 {
            return Err(JournalError::Quota);
        }
        let recovery_object = Uuid::new_v4();
        let mut recovery_node = old.clone();
        recovery_node.id = format!("local-recovery-{recovery_object}");
        recovery_node.name = recovery_name.clone();
        let recovery = NamespaceObject {
            native_archive: None,
            id: recovery_object,
            scope: owner.scope.clone(),
            names: owner.names,
            node: recovery_node,
            remote: Some(old.clone()),
            remote_owned: false,
            remote_sequence: old_owner.remote_sequence,
            working_file: None,
            latest: None,
            revision: 0,
            follows_remote: false,
            unlinked: true,
        };
        record.identity_handoff = Some(Reservation {
            recovery_object,
            old_item: item.clone(),
            metadata_original: Some(Box::new(old.clone())),
            recovery_name,
            trash_parent,
            backup: None,
            atomic_source: atomic_source.as_ref().and(owner.latest),
        });
        let tx = self.db.transaction()?;
        namespace::save(&tx, &recovery)?;
        if tx.execute(
            "UPDATE uploads SET body=?2 WHERE id=?1",
            params![id.to_string(), serde_json::to_string(&record)?],
        )? != 1
        {
            return Err(JournalError::Missing);
        }
        tx.commit()?;
        Ok(recovery_object)
    }

    /// A provider may call this only after independently verifying the new
    /// exact ID/bytes and the old exact ID at its recovery location. The two
    /// bindings and the upload receipt commit as one SQLite transaction.
    pub fn acknowledge_identity_handoff(
        &mut self,
        id: Uuid,
        attempt: Uuid,
        current: Node,
        backup: Node,
    ) -> Result<()> {
        self.acknowledge_handoff_inner(id, attempt, current, backup, None)
    }

    pub fn acknowledge_package_handoff(
        &mut self,
        id: Uuid,
        attempt: Uuid,
        receipt: PackageHandoffReceipt,
    ) -> Result<()> {
        self.acknowledge_handoff_inner(
            id,
            attempt,
            receipt.current.remote,
            receipt.backup.remote,
            Some((
                receipt.original,
                receipt.current.semantic,
                receipt.backup.semantic,
            )),
        )
    }

    fn acknowledge_handoff_inner(
        &mut self,
        id: Uuid,
        attempt: Uuid,
        current: Node,
        backup: Node,
        proof: Option<(Node, PackageSemanticIdentity, PackageSemanticIdentity)>,
    ) -> Result<()> {
        let mut record = self.active_attempt(id, attempt)?;
        let native = match (&record.representation, &proof) {
            (UploadRepresentation::FileBytes, None) => false,
            (
                UploadRepresentation::PackageReplacementArchive {
                    original,
                    semantic,
                    original_semantic,
                    ..
                }
                | UploadRepresentation::FlatNumbersReplacementArchive {
                    original,
                    semantic,
                    original_semantic,
                }
                | UploadRepresentation::FlatPagesReplacementArchive {
                    original,
                    semantic,
                    original_semantic,
                },
                Some((selected, current_semantic, backup_semantic)),
            ) if selected == original.as_ref()
                && current_semantic == semantic
                && backup_semantic == original_semantic =>
            {
                record
                    .representation
                    .validate()
                    .map_err(|_| JournalError::Corrupt)?;
                if backup.size != original.size
                    || backup.name != original.name
                    || current.content_version.is_some()
                    || backup.content_version.is_some()
                    || !current.id.starts_with("FILE::com.apple.CloudDocs::")
                    || current.id.ends_with("::")
                    || [&current, &backup].iter().any(|node| {
                        node.etag.as_ref().is_none_or(|tag| {
                            tag.is_empty()
                                || tag.len() > 4096
                                || tag.contains(['\0', '\r', '\n', '*'])
                        })
                    })
                {
                    return Err(JournalError::Corrupt);
                }
                true
            }
            _ => return Err(JournalError::Intent),
        };
        let atomic_target = if record.representation.is_file_bytes() {
            let owner = self
                .namespace_for_operation(id)?
                .ok_or(JournalError::Stale)?;
            let source = ordinary_atomic_source_prerequisite(&self.db, &record, &owner)?;
            if ordinary_atomic_source_required(&self.db, &record, &owner)? && source.is_none() {
                return Err(JournalError::Stale);
            }
            source.and(owner.latest)
        } else {
            None
        };
        let reservation = record
            .identity_handoff
            .as_mut()
            .ok_or(JournalError::Intent)?;
        if let Some(target) = atomic_target {
            reservation.atomic_source = Some(target);
        }
        if !matches!(&record.intent, UploadIntent::Replace { item, .. } if item == &reservation.old_item)
            || current.id.is_empty()
            || current.id == reservation.old_item
            || current.kind
                != if native {
                    NodeKind::Folder
                } else {
                    NodeKind::File
                }
            || current.package != native
            || current.target.is_some()
            || (!native && current.size != record.size)
            || current.content_revision().is_none()
            || backup.id != reservation.old_item
            || !backup_location_matches(reservation, &backup, current.parent_id.as_ref())
            || backup.kind
                != if native {
                    NodeKind::Folder
                } else {
                    NodeKind::File
                }
            || backup.package != native
            || backup.target.is_some()
            || backup.content_revision().is_none()
        {
            return Err(JournalError::Corrupt);
        }
        reservation.backup = Some(backup);
        if let Some((_, semantic, _)) = proof {
            record.package_completion = Some(semantic);
        }
        record.remote = Some(current);
        record.state = UploadState::Uploaded;
        record.transferred_bytes = record.size;
        record.attempt = None;
        self.save(&record)
    }
}

/// An unlinked ordinary stream may finish its already sealed save before its
/// exact pending Remove. Descriptor writes after unlink remain local: only the
/// mutable size/mtime may differ from the node captured by that Remove.
fn ordinary_remove_successor(
    db: &Connection,
    record: &UploadRecord,
    owner: &NamespaceObject,
) -> Result<bool> {
    if !record.representation.is_file_bytes()
        || !owner.unlinked
        || owner.follows_remote
        || !owner.remote_owned
        || owner.native_archive.is_some()
        || owner.scope != record.scope
        || owner.node.kind != NodeKind::File
        || owner.node.package
        || owner.node.target.is_some()
        || owner.remote_sequence >= record.sequence
        || record.base.as_ref().is_some_and(|base| !base.resolved)
    {
        return Ok(false);
    }
    let (Some(remove_id), Some(working_id), Some(old)) =
        (owner.latest, record.working_file, owner.remote.as_ref())
    else {
        return Ok(false);
    };
    if owner.working_file != Some(working_id)
        || !matches!(&record.intent, UploadIntent::Replace { item, expected_etag }
            if item == &old.id && old.etag.as_ref() == Some(expected_etag))
        || old.kind != NodeKind::File
        || old.package
        || old.target.is_some()
    {
        return Ok(false);
    }
    let row: Option<(i64, String, String)> = db
        .query_row(
            "SELECT sequence,state,body FROM mutations WHERE id=?1",
            [remove_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((sequence, state, body)) = row else {
        return Ok(false);
    };
    let remove: MutationRecord = serde_json::from_str(&body)?;
    let MutationIntent::RemoveFile { before } = &remove.request.intent else {
        return Ok(false);
    };
    if remove.id != remove_id
        || sequence <= 0
        || sequence as u64 != remove.sequence
        || remove.sequence <= record.sequence
        || state != "pending"
        || remove.state != MutationState::Pending
        || remove.request.scope != record.scope
        || remove.working_file != Some(working_id)
        || remove.attempt.is_some()
        || remove.receipt.is_some()
        || remove.verified_content.is_some()
        || remove.prepared_item.is_some()
        || remove.failed_attempts != 0
        || remove.retry_at != 0
        || remove
            .base
            .as_ref()
            .is_none_or(|base| base.resolved || base.predecessor != record.id)
        || before.size != record.size
    {
        return Ok(false);
    }
    let mut captured = owner.node.clone();
    captured.size = before.size;
    captured.modified_unix = before.modified_unix;
    if *before != captured {
        return Ok(false);
    }
    let body: Option<String> = db
        .query_row(
            "SELECT body FROM working_files WHERE id=?1",
            [working_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    let Some(body) = body else { return Ok(false) };
    let working: WorkingFile = serde_json::from_str(&body)?;
    if working.id != working_id
        || working.native
        || working.scope != record.scope
        || !working.unlinked
        || working.latest != Some(remove_id)
        || working.node != owner.node
    {
        return Ok(false);
    }
    let state = serde_json::to_value(record.state)?;
    let upload_state = state.as_str().ok_or(JournalError::Corrupt)?;
    let bound: bool = db.query_row(
        "SELECT
        EXISTS(SELECT 1 FROM write_successors WHERE predecessor=?1 AND successor=?2)
        AND (SELECT count(*) FROM namespace_operations
             WHERE operation IN (?1,?2) AND object=?3)=2
        AND EXISTS(SELECT 1 FROM write_queue WHERE id=?2 AND sequence=?4 AND complete=0)
        AND EXISTS(SELECT 1 FROM uploads u JOIN write_queue q ON q.id=u.id
             WHERE u.id=?1 AND u.sequence=?5 AND u.state=?6
             AND q.sequence=u.sequence AND q.complete=?7)",
        params![
            record.id.to_string(),
            remove_id.to_string(),
            owner.id.to_string(),
            sequence,
            record.sequence as i64,
            upload_state,
            record.state == UploadState::Uploaded,
        ],
        |row| row.get(0),
    )?;
    Ok(bound)
}

/// An existing ordinary reservation may outlive later sealed saves or a
/// Relocate. These are linear content successors of the same owner/stream;
/// an atomic source takeover instead points to a cleanup Remove and cannot
/// masquerade as this route if its replacement metadata is missing.
fn ordinary_linear_successor(
    db: &Connection,
    record: &UploadRecord,
    owner: &NamespaceObject,
) -> Result<bool> {
    let Some(latest) = owner.latest else {
        return Ok(false);
    };
    if !record.representation.is_file_bytes()
        || owner.scope != record.scope
        || owner.unlinked
        || owner.follows_remote
        || !owner.remote_owned
        || owner.native_archive.is_some()
        || owner.working_file != record.working_file
        || owner.node.kind != NodeKind::File
        || owner.node.package
        || owner.node.target.is_some()
    {
        return Ok(false);
    }
    let mut previous = record.id;
    let mut previous_sequence = record.sequence;
    for _ in 0..128 {
        let next: Option<String> = db
            .query_row(
                "SELECT successor FROM write_successors WHERE predecessor=?1",
                [previous.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        let Some(next) = next else { return Ok(false) };
        let next = Uuid::parse_str(&next).map_err(|_| JournalError::Corrupt)?;
        let upload: Option<(i64, String, String)> = db
            .query_row(
                "SELECT sequence,state,body FROM uploads WHERE id=?1",
                [next.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let (sequence, valid) = if let Some((sequence, state, body)) = upload {
            let row: UploadRecord = serde_json::from_str(&body)?;
            (
                sequence,
                row.id == next
                    && sequence > 0
                    && sequence as u64 == row.sequence
                    && state == "pending"
                    && row.state == UploadState::Pending
                    && row.scope == record.scope
                    && row.representation.is_file_bytes()
                    && row.working_file == record.working_file
                    && row.attempt.is_none()
                    && row.remote.is_none()
                    && row.identity_handoff.is_none()
                    && row
                        .base
                        .as_ref()
                        .is_some_and(|base| !base.resolved && base.predecessor == previous),
            )
        } else {
            let row: Option<(i64, String, String)> = db
                .query_row(
                    "SELECT sequence,state,body FROM mutations WHERE id=?1",
                    [next.to_string()],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            let Some((sequence, state, body)) = row else {
                return Ok(false);
            };
            let row: MutationRecord = serde_json::from_str(&body)?;
            (
                sequence,
                row.id == next
                    && sequence > 0
                    && sequence as u64 == row.sequence
                    && state == "pending"
                    && row.state == MutationState::Pending
                    && row.request.scope == record.scope
                    && row.working_file == record.working_file
                    && row.attempt.is_none()
                    && row.receipt.is_none()
                    && matches!(&row.request.intent, MutationIntent::Relocate { before, .. }
                    if before.kind == NodeKind::File && !before.package && before.target.is_none()
                        && before.id == owner.node.id)
                    && row
                        .base
                        .as_ref()
                        .is_some_and(|base| !base.resolved && base.predecessor == previous),
            )
        };
        let bound: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM namespace_operations WHERE operation=?1 AND object=?2)
             AND EXISTS(SELECT 1 FROM write_queue WHERE id=?1 AND sequence=?3 AND complete=0)
             AND NOT EXISTS(SELECT 1 FROM file_replacements WHERE id=?1 OR cleanup=?1)
             AND NOT EXISTS(SELECT 1 FROM native_working_operations WHERE operation=?1 OR owner=?2)",
            params![next.to_string(),owner.id.to_string(),sequence], |row| row.get(0),
        )?;
        if !valid || !bound || sequence as u64 <= previous_sequence {
            return Ok(false);
        }
        if next == latest {
            return Ok(true);
        }
        previous = next;
        previous_sequence = sequence as u64;
    }
    Ok(false)
}

/// Require revalidation only for a retained atomic-source latch or an actual
/// pending takeover of this owner; ordinary later saves/Relocate keep their rules.
fn ordinary_atomic_source_required(
    db: &Connection,
    record: &UploadRecord,
    owner: &NamespaceObject,
) -> Result<bool> {
    if !record.representation.is_file_bytes() {
        return Ok(false);
    }
    if record
        .identity_handoff
        .as_ref()
        .is_some_and(|reservation| reservation.atomic_source.is_some())
    {
        return Ok(true);
    }
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM file_replacements r LEFT JOIN uploads u ON u.id=r.id
         WHERE r.id!=?2 AND (r.source=?1 OR json_extract(r.body,'$.source')=?1)
           AND (json_extract(r.body,'$.remote_applied')!=1 OR coalesce(u.state,'')!='uploaded'))",
        params![owner.id.to_string(), record.id.to_string()],
        |row| row.get(0),
    )?)
}

/// A path takeover waits for the source's sealed save. It changes only that
/// source's visible slot, not the receipt used by its earlier upload. Prove the
/// complete pending takeover and cleanup before allowing that upload to finish.
fn ordinary_atomic_source_prerequisite(
    db: &Connection,
    record: &UploadRecord,
    owner: &NamespaceObject,
) -> Result<Option<Node>> {
    let (Some(target_id), Some(working_id), Some(old), Some(base)) = (
        owner.latest,
        record.working_file,
        owner.remote.as_ref(),
        record.base.as_ref(),
    ) else {
        return Ok(None);
    };
    if record
        .identity_handoff
        .as_ref()
        .and_then(|reservation| reservation.atomic_source)
        .is_some_and(|target| target != target_id)
        || target_id == record.id
        || !record.representation.is_file_bytes()
        || !base.resolved
        || owner.unlinked
        || owner.follows_remote
        || !owner.remote_owned
        || owner.native_archive.is_some()
        || owner.scope != record.scope
        || owner.working_file != Some(working_id)
        || owner.node.kind != NodeKind::File
        || owner.node.package
        || owner.node.target.is_some()
        || old.kind != NodeKind::File
        || old.package
        || old.target.is_some()
        || old.content_revision().is_none()
        || !matches!(&record.intent, UploadIntent::Replace { item, expected_etag }
            if item == &old.id && old.etag.as_ref() == Some(expected_etag))
    {
        return Ok(None);
    }
    let row: Option<(String, String, String, String)> = db
        .query_row(
            "SELECT source,victim,cleanup,body FROM file_replacements WHERE id=?1",
            [target_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((source_id, victim_id, cleanup_id, body)) = row else {
        return Ok(None);
    };
    let replacement: ReplacementRecord = serde_json::from_str(&body)?;
    if replacement.id != target_id
        || replacement.source != owner.id
        || source_id != owner.id.to_string()
        || victim_id != replacement.victim.to_string()
        || cleanup_id != replacement.cleanup.to_string()
        || replacement.source == replacement.victim
        || replacement.source == replacement.cleanup_object
        || replacement.victim == replacement.cleanup_object
        || !replacement.local_ready
        || replacement.remote_applied
        || replacement.rescued_as.is_some()
    {
        return Ok(None);
    }
    let (sequence, state, body): (i64, String, String) = db.query_row(
        "SELECT sequence,state,body FROM uploads WHERE id=?1",
        [target_id.to_string()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let target: UploadRecord = serde_json::from_str(&body)?;
    if target.id != target_id
        || sequence <= 0
        || sequence as u64 != target.sequence
        || target.sequence <= record.sequence
        || state != "pending"
        || target.state != UploadState::Pending
        || target.scope != record.scope
        || !target.representation.is_file_bytes()
        || target.working_file != Some(working_id)
        || target.attempt.is_some()
        || target.remote.is_some()
        || target.session_key.is_some()
        || target.identity_handoff.is_some()
        || target.package_completion.is_some()
        || target.transferred_bytes != 0
        || target.failed_attempts != 0
        || target.retry_at != 0
        || target.size != record.size
        || target.sha256 != record.sha256
    {
        return Ok(None);
    }
    let victim = namespace::by_id(db, replacement.victim)?;
    let cleanup = namespace::by_id(db, replacement.cleanup_object)?;
    let Some(victim_remote) = victim.remote.as_ref() else {
        return Ok(None);
    };
    if victim.scope != record.scope
        || !victim.unlinked
        || !victim.remote_owned
        || victim.follows_remote
        || victim.native_archive.is_some()
        || victim.node.kind != NodeKind::File
        || victim.node.package
        || victim.node.target.is_some()
        || victim_remote.kind != NodeKind::File
        || victim_remote.package
        || victim_remote.target.is_some()
        || victim_remote.content_revision().is_none()
        || victim_remote.id == old.id
        || owner.node.name != victim.node.name
        || owner.node.parent_id != victim.node.parent_id
        || cleanup.scope != record.scope
        || !cleanup.unlinked
        || cleanup.remote_owned
        || cleanup.follows_remote
        || cleanup.native_archive.is_some()
        || cleanup.working_file.is_some()
        || cleanup.latest != Some(replacement.cleanup)
        || match cleanup.remote.as_ref() {
            Some(captured) => {
                captured != old
                    || cleanup.remote_sequence != owner.remote_sequence
                    || replacement.source_unconfirmed_create.is_some()
            }
            None => {
                cleanup.remote_sequence != 0
                    || cleanup.revision != 0
                    || replacement.source_unconfirmed_create != Some(base.predecessor)
            }
        }
    {
        return Ok(None);
    }
    // The target's content base belongs to the victim, never to this source.
    if let Some(victim_base) = &target.base {
        let (seq, state, body): (i64, String, String) = db.query_row(
            "SELECT sequence,state,body FROM uploads WHERE id=?1",
            [victim_base.predecessor.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let prior: UploadRecord = serde_json::from_str(&body)?;
        let intent_matches = if victim_base.resolved {
            matches!(&target.intent, UploadIntent::Replace { item, expected_etag }
                if item == &victim_remote.id && victim_remote.etag.as_ref() == Some(expected_etag))
        } else {
            target.intent == prior.intent
        };
        let bound: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM write_successors WHERE predecessor=?1 AND successor=?2)
             AND EXISTS(SELECT 1 FROM write_queue WHERE id=?1 AND sequence=?3 AND complete=1)
             AND EXISTS(SELECT 1 FROM namespace_operations WHERE operation=?1 AND object=?4)",
            params![
                prior.id.to_string(),
                target_id.to_string(),
                seq,
                victim.id.to_string()
            ],
            |row| row.get(0),
        )?;
        if prior.id != victim_base.predecessor
            || seq <= 0
            || seq as u64 != prior.sequence
            || prior.sequence >= target.sequence
            || state != "uploaded"
            || prior.state != UploadState::Uploaded
            || prior.scope != record.scope
            || !prior.representation.is_file_bytes()
            || prior.remote.as_ref() != Some(victim_remote)
            || victim.latest != Some(prior.id)
            || !intent_matches
            || !bound
        {
            return Ok(None);
        }
    } else if !matches!(&target.intent, UploadIntent::Replace { item, expected_etag }
        if item == &victim_remote.id && victim_remote.etag.as_ref() == Some(expected_etag))
    {
        return Ok(None);
    }
    let (cleanup_sequence, state, body): (i64, String, String) = db.query_row(
        "SELECT sequence,state,body FROM mutations WHERE id=?1",
        [replacement.cleanup.to_string()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let remove: MutationRecord = serde_json::from_str(&body)?;
    let MutationIntent::RemoveFile { before } = &remove.request.intent else {
        return Ok(None);
    };
    if remove.id != replacement.cleanup
        || cleanup_sequence <= sequence
        || cleanup_sequence as u64 != remove.sequence
        || state != "pending"
        || remove.state != MutationState::Pending
        || remove.request.scope != record.scope
        || remove.attempt.is_some()
        || remove.receipt.is_some()
        || remove.verified_content.is_some()
        || remove.prepared_item.is_some()
        || remove.working_file.is_some()
        || !remove.local_ready
        || remove.failed_attempts != 0
        || remove.retry_at != 0
        || remove
            .base
            .as_ref()
            .is_none_or(|base| base.resolved || base.predecessor != record.id)
        || before.size != record.size
        || before.name != old.name
    {
        return Ok(None);
    }
    // Descriptor writes can change size/mtime after the path takeover; the two
    // immutable uploads still identify the sealed bytes. Do not rewrite them.
    let mut captured = owner.node.clone();
    captured.name = before.name.clone();
    captured.parent_id = before.parent_id.clone();
    captured.size = before.size;
    captured.modified_unix = before.modified_unix;
    if captured != *before {
        return Ok(None);
    }
    let mut cleanup_node = before.clone();
    cleanup_node.id = format!("local-{}", cleanup.id);
    if cleanup.node != cleanup_node
        || !confirmed_parent_route(
            db,
            &record.scope,
            before.parent_id.as_deref(),
            old.parent_id.as_deref(),
        )?
    {
        return Ok(None);
    }
    let body: String = db.query_row(
        "SELECT body FROM working_files WHERE id=?1",
        [working_id.to_string()],
        |row| row.get(0),
    )?;
    let working: WorkingFile = serde_json::from_str(&body)?;
    if working.id != working_id
        || working.native
        || working.scope != record.scope
        || working.unlinked
        || working.latest != Some(target_id)
        || working.node != owner.node
    {
        return Ok(None);
    }
    let (base_sequence, state, body): (i64, String, String) = db.query_row(
        "SELECT sequence,state,body FROM uploads WHERE id=?1",
        [base.predecessor.to_string()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let prior: UploadRecord = serde_json::from_str(&body)?;
    if prior.id != base.predecessor
        || base_sequence <= 0
        || base_sequence as u64 != prior.sequence
        || prior.sequence >= record.sequence
        || state != "uploaded"
        || prior.state != UploadState::Uploaded
        || prior.scope != record.scope
        || !prior.representation.is_file_bytes()
        || prior.working_file != Some(working_id)
        || prior.remote.as_ref() != Some(old)
        || owner.remote_sequence != prior.sequence
        || (cleanup.remote.is_none()
            && (prior.base.is_some()
                || prior.identity_handoff.is_some()
                || prior.package_completion.is_some()
                || prior.size != 0
                || prior.transferred_bytes != 0
                || old.size != 0
                || !matches!(&prior.intent, UploadIntent::Create { parent, name }
                    if Some(parent) == old.parent_id.as_ref() && name == &old.name)))
    {
        return Ok(None);
    }
    let state = serde_json::to_value(record.state)?;
    let upload_state = state.as_str().ok_or(JournalError::Corrupt)?;
    let bound: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM write_successors WHERE predecessor=?1 AND successor=?2)
         AND EXISTS(SELECT 1 FROM write_successors WHERE predecessor=?2 AND successor=?4)
         AND EXISTS(SELECT 1 FROM write_prerequisites WHERE operation=?3 AND predecessor=?2)
         AND EXISTS(SELECT 1 FROM write_prerequisites WHERE operation=?4 AND predecessor=?3)
         AND (SELECT count(*) FROM namespace_operations WHERE operation IN (?1,?2,?3) AND object=?5)=3
         AND EXISTS(SELECT 1 FROM namespace_operations WHERE operation=?4 AND object=?6)
         AND EXISTS(SELECT 1 FROM write_queue WHERE id=?1 AND sequence=?7 AND complete=1)
         AND EXISTS(SELECT 1 FROM write_queue WHERE id=?2 AND sequence=?8 AND complete=?9)
         AND EXISTS(SELECT 1 FROM uploads WHERE id=?2 AND sequence=?8 AND state=?10)
         AND EXISTS(SELECT 1 FROM write_queue WHERE id=?3 AND sequence=?11 AND complete=0)
         AND EXISTS(SELECT 1 FROM write_queue WHERE id=?4 AND sequence=?12 AND complete=0)
         AND NOT EXISTS(SELECT 1 FROM file_replacements WHERE id=?2 OR cleanup=?2
             OR ((source=?5 OR victim=?5) AND id!=?3
                 AND (json_extract(body,'$.remote_applied')!=1
                     OR EXISTS(SELECT 1 FROM uploads u WHERE u.id=file_replacements.id AND u.state!='uploaded'))))
         AND NOT EXISTS(SELECT 1 FROM native_working_operations WHERE operation IN (?1,?2,?3,?4)
             OR owner IN (?5,?6,?14) OR working=?13)
         AND NOT EXISTS(SELECT 1 FROM native_working_heads WHERE working=?13
             OR json_extract(body,'$.owner') IN (?5,?6,?14))
         AND NOT EXISTS(SELECT 1 FROM native_working_bindings WHERE working=?13)
         AND NOT EXISTS(SELECT 1 FROM native_temporary_streams WHERE working=?13 OR owner IN (?5,?6,?14))
         AND NOT EXISTS(SELECT 1 FROM native_detached_streams WHERE working=?13 OR owner IN (?5,?6,?14))",
        params![prior.id.to_string(),record.id.to_string(),target_id.to_string(),remove.id.to_string(),
            owner.id.to_string(),cleanup.id.to_string(),base_sequence,record.sequence as i64,
            record.state == UploadState::Uploaded,upload_state,sequence,cleanup_sequence,
            working_id.to_string(),victim.id.to_string()], |row| row.get(0),
    )?;
    Ok(bound.then(|| before.clone()))
}

pub(super) fn confirm(tx: &Transaction<'_>, record: &UploadRecord, current: &Node) -> Result<()> {
    let reservation = record
        .identity_handoff
        .as_ref()
        .ok_or(JournalError::Corrupt)?;
    let backup = reservation.backup.as_ref().ok_or(JournalError::Corrupt)?;
    let owner_id: String = tx.query_row(
        "SELECT object FROM namespace_operations WHERE operation=?1",
        [record.id.to_string()],
        |row| row.get(0),
    )?;
    let mut owner = namespace::by_id(
        tx,
        Uuid::parse_str(&owner_id).map_err(|_| JournalError::Corrupt)?,
    )?;
    let mut recovery = namespace::by_id(tx, reservation.recovery_object)?;
    let victim = replacements::handoff_victim(tx, record, &owner)?;
    let old_owner = victim.as_ref().unwrap_or(&owner);
    let old = old_owner.remote.as_ref().ok_or(JournalError::Corrupt)?;
    let unlinked_remove = victim.is_none() && ordinary_remove_successor(tx, record, &owner)?;
    let atomic_source = if victim.is_none() {
        ordinary_atomic_source_prerequisite(tx, record, &owner)?
    } else {
        None
    };
    if victim.is_none()
        && ordinary_atomic_source_required(tx, record, &owner)?
        && atomic_source.is_none()
    {
        return Err(JournalError::Corrupt);
    }
    if record.representation.is_file_bytes()
        && victim.is_none()
        && !unlinked_remove
        && atomic_source.is_none()
        && owner.latest != Some(record.id)
        && !ordinary_linear_successor(tx, record, &owner)?
    {
        return Err(JournalError::Corrupt);
    }
    if owner.scope != record.scope
        || recovery.scope != record.scope
        || (owner.unlinked && victim.is_none() && !unlinked_remove)
        || !owner.remote_owned
        || !recovery.unlinked
        || recovery.remote_owned
        || recovery.working_file.is_some()
        || recovery.latest.is_some()
        || recovery
            .remote
            .as_ref()
            .is_none_or(|node| node.id != old.id)
        || old.id != reservation.old_item
        || package_replacement::original(&record.representation).is_some_and(|before| before != old)
        || backup.id != old.id
        || !backup_location_matches(reservation, backup, old.parent_id.as_ref())
        || current.parent_id != old.parent_id
        || current.name != old.name
        || current.id == old.id
        || owner.remote_sequence >= record.sequence
    {
        return Err(JournalError::Corrupt);
    }
    if victim.is_some() {
        replacements::confirm_handoff(tx, record, current, &old.id)?;
    } else {
        owner.remote = Some(current.clone());
        owner.remote_sequence = record.sequence;
        if owner.working_file.is_none() {
            owner.node.etag = current.etag.clone();
            owner.node.content_version = current.content_version.clone();
            owner.node.size = current.size;
            owner.node.modified_unix = current.modified_unix;
        }
        owner.revision = owner.revision.checked_add(1).ok_or(JournalError::Quota)?;
        namespace::save(tx, &owner)?;
    }
    recovery.remote = Some(backup.clone());
    recovery.remote_owned = true;
    recovery.remote_sequence = record.sequence;
    if reservation.trash_parent.is_none() {
        recovery.node.name = backup.name.clone();
    }
    recovery.node.size = backup.size;
    recovery.node.etag = backup.etag.clone();
    recovery.node.content_version = backup.content_version.clone();
    recovery.node.modified_unix = backup.modified_unix;
    recovery.revision = recovery
        .revision
        .checked_add(1)
        .ok_or(JournalError::Quota)?;
    namespace::save(tx, &recovery)
}

fn backup_location_matches(
    reservation: &Reservation,
    backup: &Node,
    original_parent: Option<&String>,
) -> bool {
    match &reservation.trash_parent {
        Some(parent) => {
            backup.parent_id.as_ref() == Some(parent)
                && original_parent != Some(parent)
                && !backup.name.is_empty()
        }
        None => {
            backup.parent_id.as_ref() == original_parent && backup.name == reservation.recovery_name
        }
    }
}

impl UploadRecord {
    /// Historical ordinary two-identity acknowledgment, independent of later
    /// namespace changes. This does not inspect or replay the provider.
    pub(crate) fn ordinary_handoff_receipt(&self) -> Option<(&Node, &Node)> {
        if self.state != UploadState::Uploaded || !self.representation.is_file_bytes() {
            return None;
        }
        let UploadIntent::Replace { item, .. } = &self.intent else {
            return None;
        };
        let reservation = self.identity_handoff.as_ref()?;
        let current = self.remote.as_ref()?;
        let backup = reservation.backup.as_ref()?;
        let ordinary = |node: &Node| {
            node.kind == NodeKind::File
                && !node.package
                && node.target.is_none()
                && node.content_revision().is_some()
        };
        (reservation.old_item == *item
            && !current.id.is_empty()
            && current.id != *item
            && ordinary(current)
            && current.size == self.size
            && backup.id == *item
            && ordinary(backup)
            && backup_location_matches(reservation, backup, current.parent_id.as_ref()))
        .then_some((current, backup))
    }
    /// Feature-only exact owner of a confirmed ordinary recovery identity.
    #[cfg(feature = "icloud-write-probe")]
    pub(crate) fn ordinary_validation_recovery_owner(&self) -> Option<Uuid> {
        self.ordinary_handoff_receipt()?;
        Some(self.identity_handoff.as_ref()?.recovery_object)
    }
}

impl UploadRecord {
    /// Recorded typed acknowledgment only, not a fresh provider observation.
    pub(crate) fn native_replacement_receipt(&self) -> Option<(&Node, &Node, &Node)> {
        let (original, semantic, original_semantic) = match &self.representation {
            UploadRepresentation::PackageReplacementArchive {
                original,
                semantic,
                original_semantic,
                ..
            }
            | UploadRepresentation::FlatNumbersReplacementArchive {
                original,
                semantic,
                original_semantic,
            }
            | UploadRepresentation::FlatPagesReplacementArchive {
                original,
                semantic,
                original_semantic,
            } => (original, semantic, original_semantic),
            _ => return None,
        };
        let handoff = self.identity_handoff.as_ref()?;
        let backup = handoff.backup.as_ref()?;
        let current = self.remote.as_ref()?;
        let native = |n: &Node| {
            n.kind == NodeKind::Folder
                && n.package
                && n.target.is_none()
                && n.content_version.is_none()
                && n.etag.as_ref().is_some_and(|s| !s.is_empty())
        };
        (self.representation.validate().is_ok()&&original_semantic.validate().is_ok()&&self.state==UploadState::Uploaded&&self.package_completion.as_ref()==Some(semantic)
        &&matches!(&self.intent,UploadIntent::Replace{item,expected_etag} if item==&original.id&&original.etag.as_ref()==Some(expected_etag))
        &&handoff.old_item==original.id&&handoff.trash_parent.as_deref()==Some("FOLDER::com.apple.CloudDocs::TRASH_ROOT")
        &&native(current)&&native(backup)&&current.id!=original.id&&current.id.starts_with("FILE::com.apple.CloudDocs::")&&current.parent_id==original.parent_id&&current.name==original.name
        &&backup.id==original.id&&backup.parent_id==handoff.trash_parent&&backup.name==original.name&&backup.size==original.size).then_some((original,current,backup))
    }
}

impl Reservation {
    /// Validate persisted recovery identity without exposing private reservation fields.
    pub(super) fn matches_native_replacement_backup(&self, original: &Node) -> bool {
        self.old_item == original.id
            && self.trash_parent.as_deref() == Some("FOLDER::com.apple.CloudDocs::TRASH_ROOT")
            && self.backup.as_ref().is_some_and(|backup| {
                backup.id == original.id && backup.parent_id == self.trash_parent
            })
    }
}

impl Reservation {
    pub(super) fn unconfirmed_native(&self, before: &Node) -> Option<(Uuid, &str)> {
        (self.old_item == before.id
            && self.trash_parent.as_deref() == Some("FOLDER::com.apple.CloudDocs::TRASH_ROOT")
            && self.backup.is_none())
        .then_some((self.recovery_object, self.recovery_name.as_str()))
    }
}

/// Durable immutable receipt, independent of later visible heads.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct OrdinaryHandoffMetadata {
    /// Existing ordinary JSON remains byte-compatible. Native jobs require
    /// their own complete package receipt and captured original authority.
    #[serde(default, skip_serializing_if = "metadata_is_ordinary")]
    pub(crate) native: bool,
    pub(crate) operation: Uuid,
    sequence: u64,
    pub(crate) scope: Scope,
    owner: Uuid,
    size: u64,
    sha256: String,
    pub(crate) original: Node,
    pub(crate) current: Node,
    pub(crate) backup: Node,
}
fn metadata_is_ordinary(native: &bool) -> bool {
    !native
}
fn metadata_owner(db: &Connection, id: Uuid) -> Result<NamespaceObject> {
    let owner: String = db.query_row(
        "SELECT object FROM namespace_operations WHERE operation=?1",
        [id.to_string()],
        |r| r.get(0),
    )?;
    let id = Uuid::parse_str(&owner).map_err(|_| JournalError::Corrupt)?;
    let owner = namespace::by_id(db, id)?;
    if owner.id != id {
        return Err(JournalError::Corrupt);
    }
    Ok(owner)
}
fn metadata_receipt(row: &UploadRecord, owner: Uuid) -> Result<Option<OrdinaryHandoffMetadata>> {
    if row.identity_handoff.is_none() {
        return Ok(None);
    }
    let reservation = row.identity_handoff.as_ref().ok_or(JournalError::Corrupt)?;
    let Some(original) = reservation.metadata_original.as_deref() else {
        return Ok(None);
    };
    if !row.representation.is_file_bytes() {
        let Some((selected, current, backup)) = row.native_replacement_receipt() else {
            return Err(JournalError::Corrupt);
        };
        package_replacement::validate(&row.scope, &row.intent, &row.representation)?;
        cirrove_core::upload::UploadRequest {
            scope: row.scope.clone(),
            intent: row.intent.clone(),
            representation: row.representation.clone(),
            size: row.size,
            sha256: row.sha256.clone(),
        }
        .validate()
        .map_err(|_| JournalError::Corrupt)?;
        if original != selected
            || row.transferred_bytes != row.size
            || current.id.ends_with("::")
            || row.scope.account.is_empty()
        {
            return Err(JournalError::Corrupt);
        }
        return Ok(Some(OrdinaryHandoffMetadata {
            native: true,
            operation: row.id,
            sequence: row.sequence,
            scope: row.scope.clone(),
            owner,
            size: row.size,
            sha256: row.sha256.clone(),
            original: original.clone(),
            current: current.clone(),
            backup: backup.clone(),
        }));
    }
    let (current, backup) = row
        .ordinary_handoff_receipt()
        .ok_or(JournalError::Corrupt)?;
    if row.scope.account.is_empty()
        || row.scope.collection.is_empty()
        || row.state != UploadState::Uploaded
        || row.transferred_bytes != row.size
        || original.id != reservation.old_item
        || !matches!(&row.intent,UploadIntent::Replace{item,expected_etag}
            if item==&original.id && original.etag.as_ref()==Some(expected_etag))
        || original.kind != NodeKind::File
        || original.package
        || original.target.is_some()
        || original.content_revision().is_none()
        || original.parent_id.is_none()
        || current.parent_id != original.parent_id
        || current.name != original.name
        || backup.size != original.size
    {
        return Err(JournalError::Corrupt);
    }
    Ok(Some(OrdinaryHandoffMetadata {
        native: false,
        operation: row.id,
        sequence: row.sequence,
        scope: row.scope.clone(),
        owner,
        size: row.size,
        sha256: row.sha256.clone(),
        original: original.clone(),
        current: current.clone(),
        backup: backup.clone(),
    }))
}
pub(super) fn migrate_metadata_publication(db: &mut Connection) -> Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS ordinary_metadata_publication(
        operation TEXT PRIMARY KEY,body TEXT NOT NULL,done INTEGER NOT NULL DEFAULT 0 CHECK(done IN(0,1)),
        failures INTEGER NOT NULL DEFAULT 0,retry_after INTEGER NOT NULL DEFAULT 0);
        CREATE INDEX IF NOT EXISTS ordinary_metadata_due_v20 ON ordinary_metadata_publication(retry_after,operation) WHERE done=0;")?;
    Ok(()) // No ordinary legacy backfill or synthesized original.
}
/// Same transaction as the already validated full handoff acknowledgment.
pub(super) fn enqueue_metadata_publication(tx: &Transaction<'_>, row: &UploadRecord) -> Result<()> {
    if row.identity_handoff.is_none() {
        return Ok(());
    }
    let owner = metadata_owner(tx, row.id)?;
    if let Some(proof) = metadata_receipt(row, owner.id)? {
        tx.execute(
            "INSERT OR IGNORE INTO ordinary_metadata_publication(operation,body) VALUES(?1,?2)",
            params![row.id.to_string(), serde_json::to_string(&proof)?],
        )?;
    }
    Ok(())
}
fn metadata_job(db: &Connection, account: &str, id: Uuid) -> Result<OrdinaryHandoffMetadata> {
    let (body, upload, sequence, state, complete, qsequence, qid): (
        String,
        String,
        i64,
        String,
        bool,
        i64,
        String,
    ) = db.query_row(
        "SELECT p.body,u.body,u.sequence,u.state,q.complete,q.sequence,q.id
         FROM ordinary_metadata_publication p JOIN uploads u ON u.id=p.operation
         JOIN write_queue q ON q.id=u.id WHERE p.operation=?1",
        [id.to_string()],
        |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        },
    )?;
    if body.len() > 256 * 1024 || upload.len() > 256 * 1024 {
        return Err(JournalError::Corrupt);
    }
    let proof: OrdinaryHandoffMetadata = serde_json::from_str(&body)?;
    let row: UploadRecord = serde_json::from_str(&upload)?;
    let owner = metadata_owner(db, id)?;
    if row.id != id
        || row.state != UploadState::Uploaded
        || row.scope != proof.scope
        || proof.operation != id
        || state != "uploaded"
        || !complete
        || u64::try_from(sequence).ok() != Some(row.sequence)
        || qsequence != sequence
        || qid != id.to_string()
        || proof.scope.account != account
        || owner.scope != proof.scope
        || metadata_receipt(&row, owner.id)?.as_ref() != Some(&proof)
    {
        return Err(JournalError::Stale);
    }
    // Newer dirty saves/Relocates/unlink/retirement do not cancel historical jobs.
    Ok(proof)
}
#[derive(Clone, Copy, Default)]
pub(crate) struct NativeMetadataScan {
    after: i64,
    highwater: Option<i64>,
}
/// Bounded read-only reconciliation of existing completed ordinary history.
/// The fixed startup highwater never admits jobs added during this scan.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CompletedOrdinaryMetadataScan {
    after: i64,
    highwater: Option<i64>,
}
impl CompletedOrdinaryMetadataScan {
    pub(crate) fn retry(&mut self, previous: Self) {
        self.after = previous.after;
    }
}
impl UploadJournal {
    pub(crate) fn completed_ordinary_metadata(
        &self,
        scan: &mut CompletedOrdinaryMetadataScan,
    ) -> Result<Option<OrdinaryHandoffMetadata>> {
        let tx = self.db.unchecked_transaction()?;
        let highwater = match scan.highwater {
            Some(value) => value,
            None => {
                let value = tx.query_row(
                    "SELECT coalesce(max(sequence),0) FROM uploads WHERE sequence>=0",
                    [],
                    |r| r.get::<_, i64>(0),
                )?;
                scan.highwater = Some(value);
                value
            }
        };
        if scan.after >= highwater {
            tx.commit()?;
            return Ok(None);
        }
        let rows = {
            let mut query = tx.prepare(
                "SELECT u.sequence,p.operation FROM uploads u
                 JOIN ordinary_metadata_publication p ON p.operation=u.id
                 WHERE u.sequence>?1 AND u.sequence<=?2 AND p.done=1
                 ORDER BY u.sequence LIMIT 16",
            )?;
            query
                .query_map(params![scan.after, highwater], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        for (sequence, id) in &rows {
            // Poison history advances this in-memory cursor, never its done,
            // failure, upload or queue fields, so it cannot starve later jobs.
            scan.after = *sequence;
            let id = Uuid::parse_str(id).map_err(|_| JournalError::Corrupt)?;
            let proof = metadata_job(&tx, &self.account, id)?;
            if !proof.native {
                tx.commit()?;
                return Ok(Some(proof));
            }
        }
        if rows.len() < 16 {
            scan.after = highwater;
        }
        tx.commit()?;
        Ok(None)
    }
    pub(crate) fn validate_completed_ordinary_metadata(
        &self,
        expected: &OrdinaryHandoffMetadata,
    ) -> Result<()> {
        let tx = self.db.unchecked_transaction()?;
        let done: bool = tx.query_row(
            "SELECT done FROM ordinary_metadata_publication WHERE operation=?1",
            [expected.operation.to_string()],
            |r| r.get(0),
        )?;
        if !done
            || expected.native
            || metadata_job(&tx, &self.account, expected.operation)? != *expected
        {
            return Err(JournalError::Stale);
        }
        tx.commit()?;
        Ok(())
    }
}
fn queue_native_history(db: &Connection, account: &str, id: &str, body: &str) -> Result<()> {
    let present: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM ordinary_metadata_publication WHERE operation=?1)",
        [id],
        |r| r.get(0),
    )?;
    if present {
        return Ok(());
    }
    if body.len() > 256 * 1024 {
        db.execute(
            "INSERT OR IGNORE INTO ordinary_metadata_publication(operation,body) VALUES(?1,'null')",
            [id],
        )?;
        return Ok(());
    }
    let value: serde_json::Value = serde_json::from_str(body)?;
    if !matches!(
        value["representation"]["kind"].as_str(),
        Some(
            "package_replacement_archive"
                | "flat_numbers_replacement_archive"
                | "flat_pages_replacement_archive"
        )
    ) || !value["identity_handoff"]["metadata_original"].is_object()
    {
        return Ok(());
    }
    let proof = (|| -> Result<OrdinaryHandoffMetadata> {
        if body.len() > 256 * 1024 {
            return Err(JournalError::Corrupt);
        }
        let id = Uuid::parse_str(id).map_err(|_| JournalError::Corrupt)?;
        let row: UploadRecord = serde_json::from_str(body)?;
        let owner = metadata_owner(db, id)?;
        let proof = metadata_receipt(&row, owner.id)?.ok_or(JournalError::Corrupt)?;
        if row.id != id
            || !proof.native
            || proof.scope.account != account
            || owner.scope != proof.scope
        {
            return Err(JournalError::Stale);
        }
        Ok(proof)
    })();
    let queue_body = match proof {
        Ok(proof) => serde_json::to_string(&proof)?,
        Err(_) => "null".into(),
    };
    db.execute(
        "INSERT OR IGNORE INTO ordinary_metadata_publication(operation,body) VALUES(?1,?2)",
        params![id, queue_body],
    )?;
    Ok(())
}
/// A fixed startup range, with at most 16 indexed package rows inspected per
/// pass. Imports/queued rows advance the same cursor; an exhausted scan stops.
fn backfill_native_metadata(
    db: &Connection,
    account: &str,
    scan: &mut NativeMetadataScan,
) -> Result<()> {
    let version: u32 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version != 21 {
        return Ok(());
    }
    let highwater = match scan.highwater {
        Some(value) => value,
        None => {
            let value = db.query_row(
                "SELECT coalesce(max(sequence),0) FROM uploads WHERE sequence>=0",
                [],
                |r| r.get::<_, i64>(0),
            )?;
            if value < 0 {
                return Err(JournalError::Corrupt);
            }
            scan.highwater = Some(value);
            value
        }
    };
    if scan.after >= highwater {
        return Ok(());
    }
    let mut query = db.prepare(
        "SELECT u.sequence,u.id,u.body FROM uploads u WHERE u.sequence>?1 AND u.sequence<=?2 AND u.state='uploaded'
         AND json_extract(u.body,'$.representation.kind') IN('package_archive','package_replacement_archive','flat_numbers_archive','flat_numbers_replacement_archive','flat_pages_archive','flat_pages_replacement_archive')
         AND json_type(u.body,'$.package_completion')='object'
         ORDER BY u.sequence LIMIT 16")?;
    let rows = query
        .query_map(params![scan.after, highwater], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (sequence, id, body) in &rows {
        queue_native_history(db, account, id, body)?;
        scan.after = *sequence;
    }
    if rows.len() < 16 {
        scan.after = highwater;
    }
    Ok(())
}
impl UploadJournal {
    pub(crate) fn ordinary_metadata_due(
        &self,
        now: u64,
    ) -> Result<Option<OrdinaryHandoffMetadata>> {
        let tx = self.db.unchecked_transaction()?;
        let mut scan = self.native_metadata_scan.get();
        backfill_native_metadata(&tx, &self.account, &mut scan)?;
        let selected: Option<(String, String)> = tx
            .query_row(
                "SELECT operation,body FROM ordinary_metadata_publication
            WHERE done=0 AND retry_after<=?1 ORDER BY retry_after,operation LIMIT 1",
                [i64::try_from(now).map_err(|_| JournalError::Stale)?],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((id, body)) = selected else {
            tx.commit()?;
            self.native_metadata_scan.set(scan);
            return Ok(None);
        };
        let result = Uuid::parse_str(&id)
            .map_err(|_| JournalError::Corrupt)
            .and_then(|id| metadata_job(&tx, &self.account, id));
        if result.is_err() {
            // Corruption is never acknowledged. Defer only this exact raw local
            // queue entry, so a bad head cannot indefinitely starve valid jobs.
            tx.execute(
                "UPDATE ordinary_metadata_publication SET
                retry_after=?3+min(60,(1 << min(failures+1,6))),failures=min(failures+1,6)
                WHERE operation=?1 AND body=?2 AND done=0",
                params![
                    id,
                    body,
                    i64::try_from(now).map_err(|_| JournalError::Stale)?
                ],
            )?;
        }
        tx.commit()?;
        self.native_metadata_scan.set(scan);
        result.map(Some)
    }

    pub(crate) fn finish_ordinary_metadata(
        &self,
        expected: &OrdinaryHandoffMetadata,
        completed: bool,
        now: u64,
    ) -> Result<()> {
        let tx = self.db.unchecked_transaction()?;
        if metadata_job(&tx, &self.account, expected.operation)? != *expected {
            return Err(JournalError::Stale);
        }
        if expected.native
            && completed
            && tx.query_row(
                "SELECT done FROM ordinary_metadata_publication WHERE operation=?1",
                [expected.operation.to_string()],
                |r| r.get::<_, bool>(0),
            )?
        {
            tx.commit()?;
            return Ok(());
        }
        let now = i64::try_from(now).map_err(|_| JournalError::Stale)?;
        let changed = if completed {
            tx.execute("UPDATE ordinary_metadata_publication SET done=1,retry_after=0 WHERE operation=?1 AND done=0",[expected.operation.to_string()])?
        } else {
            tx.execute("UPDATE ordinary_metadata_publication SET retry_after=?2+min(60,(1 << min(failures+1,6))),failures=min(failures+1,6)
                WHERE operation=?1 AND done=0",params![expected.operation.to_string(),now])?
        };
        if changed != 1 {
            return Err(JournalError::Stale);
        }
        tx.commit()?;
        Ok(())
    }
    /// Native package publication shares the immutable metadata job. A racing
    /// maintenance callback may already have completed this exact same proof.
    pub(crate) fn native_metadata_for_package(&self, id: Uuid) -> Result<OrdinaryHandoffMetadata> {
        let tx = self.db.unchecked_transaction()?;
        let body: String = tx.query_row(
            "SELECT body FROM uploads WHERE id=?1 AND state='uploaded'",
            [id.to_string()],
            |r| r.get(0),
        )?;
        queue_native_history(&tx, &self.account, &id.to_string(), &body)?;
        let result = metadata_job(&tx, &self.account, id);
        tx.commit()?;
        let proof = result?;
        if !proof.native {
            return Err(JournalError::Intent);
        }
        Ok(proof)
    }
    pub(crate) fn finish_native_metadata(&self, expected: &OrdinaryHandoffMetadata) -> Result<()> {
        let tx = self.db.unchecked_transaction()?;
        if !expected.native || metadata_job(&tx, &self.account, expected.operation)? != *expected {
            return Err(JournalError::Stale);
        }
        tx.execute(
            "UPDATE ordinary_metadata_publication SET done=1,retry_after=0 WHERE operation=?1",
            [expected.operation.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod metadata_native_tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn fixture() -> (tempfile::TempDir, UploadJournal, Scope) {
        let root = tempfile::tempdir().unwrap();
        let scope = Scope {
            account: Uuid::new_v4().to_string(),
            provider: "icloud".into(),
            collection: "drive".into(),
        };
        let journal =
            UploadJournal::open(&root.path().join("journal"), &scope.account, 1024 * 1024).unwrap();
        (root, journal, scope)
    }
    fn native_ack(
        root: &Path,
        journal: &mut UploadJournal,
        scope: &Scope,
        label: &str,
    ) -> UploadRecord {
        let staging = root.join(format!("staging-{label}"));
        std::fs::create_dir(&staging).unwrap();
        std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o700)).unwrap();
        let source = root.join(format!("source-{label}.zip"));
        std::fs::write(
            &source,
            crate::native_import::synthetic_package_archive(
                "Source.pages/Document",
                b"new sealed native content",
            ),
        )
        .unwrap();
        let archive = crate::native_import::ValidatedPackageArchive::capture(
            &source,
            &staging,
            "Source.pages",
            &cirrove_core::CancellationToken::new(),
        )
        .unwrap();
        let original = Node {
            id: format!("FILE::com.apple.CloudDocs::old-{label}"),
            parent_id: Some("FOLDER::com.apple.CloudDocs::owned".into()),
            name: format!("Owned-{label}.pages"),
            kind: NodeKind::Folder,
            package: true,
            size: 3,
            modified_unix: 0,
            etag: Some("old-v1".into()),
            content_version: None,
            target: None,
        };
        let old_semantic = PackageSemanticIdentity {
            version: 1,
            sha256: "a".repeat(64),
            entries: 1,
            files: 1,
            expanded_bytes: 3,
        };
        let row = journal
            .enqueue_validated_package_replacement(
                scope.clone(),
                original.clone(),
                old_semantic.clone(),
                archive,
                &cirrove_core::CancellationToken::new(),
            )
            .unwrap();
        let claimed = journal.claim_next().unwrap().unwrap();
        assert_eq!(claimed.id, row.id);
        let attempt = claimed.attempt.unwrap();
        journal
            .reserve_identity_handoff(
                row.id,
                attempt,
                RecoveryLocation::Trash {
                    local_name: format!("recovery-{label}.pages"),
                    parent: "FOLDER::com.apple.CloudDocs::TRASH_ROOT".into(),
                },
            )
            .unwrap();
        let UploadRepresentation::PackageReplacementArchive { semantic, .. } = &row.representation
        else {
            panic!("typed native")
        };
        journal
            .acknowledge_package_handoff(
                row.id,
                attempt,
                PackageHandoffReceipt {
                    original: original.clone(),
                    current: PackageUploadReceipt {
                        remote: Node {
                            id: format!("FILE::com.apple.CloudDocs::new-{label}"),
                            etag: Some("new-v2".into()),
                            size: 25,
                            ..original.clone()
                        },
                        semantic: semantic.clone(),
                    },
                    backup: PackageUploadReceipt {
                        remote: Node {
                            parent_id: Some("FOLDER::com.apple.CloudDocs::TRASH_ROOT".into()),
                            etag: Some("trash-v2".into()),
                            ..original
                        },
                        semantic: old_semantic,
                    },
                },
            )
            .unwrap();
        journal.get(row.id).unwrap()
    }
    fn ordinary_ack(journal: &mut UploadJournal, scope: &Scope) -> UploadRecord {
        let original = Node {
            id: "ordinary-old".into(),
            parent_id: Some("root".into()),
            name: "Owned.txt".into(),
            kind: NodeKind::File,
            package: false,
            size: 3,
            modified_unix: 0,
            etag: Some("old-v1".into()),
            content_version: Some("old-content".into()),
            target: None,
        };
        let working = journal
            .create_working(scope.clone(), original.clone(), false, &b"old"[..])
            .unwrap();
        journal.write_working(working.id, 0, b"new").unwrap();
        let row = journal.seal_working(working.id).unwrap().unwrap();
        let claimed = journal.claim_next().unwrap().unwrap();
        assert_eq!(claimed.id, row.id);
        let attempt = claimed.attempt.unwrap();
        journal
            .reserve_identity_handoff(
                row.id,
                attempt,
                RecoveryLocation::Sibling {
                    name: "recovery-A.txt".into(),
                },
            )
            .unwrap();
        journal
            .acknowledge_identity_handoff(
                row.id,
                attempt,
                Node {
                    id: "ordinary-new".into(),
                    etag: Some("new-v2".into()),
                    content_version: Some("new-content".into()),
                    ..original.clone()
                },
                Node {
                    name: "recovery-A.txt".into(),
                    etag: Some("backup-v2".into()),
                    ..original
                },
            )
            .unwrap();
        journal.get(row.id).unwrap()
    }
    fn bodies(journal: &UploadJournal) -> Vec<(String, String)> {
        journal
            .db
            .prepare("SELECT id,body FROM uploads ORDER BY sequence")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    #[test]
    fn metadata_native_qualifier_preserves_exact_ordinary_json_wire() {
        let (_root, mut journal, scope) = fixture();
        let row = ordinary_ack(&mut journal, &scope);
        let proof = journal.ordinary_metadata_due(0).unwrap().unwrap();
        assert_eq!(proof.operation, row.id);
        assert!(!proof.native);
        let body: String = journal
            .db
            .query_row(
                "SELECT body FROM ordinary_metadata_publication WHERE operation=?1",
                [row.id.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        let wire: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(wire.get("native").is_none());
        assert_eq!(serde_json::to_string(&proof).unwrap(), body);
        assert_eq!(
            serde_json::from_str::<OrdinaryHandoffMetadata>(&body).unwrap(),
            proof
        );
        // A fixed startup range inspects 16 indexed receipts per pass, even
        // when all earlier rows already have jobs. New acknowledgments enqueue
        // directly; the exhausted history cursor never revisits that prefix.
        for index in 0..17 {
            native_ack(
                _root.path(),
                &mut journal,
                &scope,
                &format!("bounded-{index}"),
            );
        }
        journal
            .db
            .execute(
                "DELETE FROM ordinary_metadata_publication WHERE operation!=?1",
                [row.id.to_string()],
            )
            .unwrap();
        let before = bodies(&journal);
        let mut scan = NativeMetadataScan::default();
        backfill_native_metadata(&journal.db, &scope.account, &mut scan).unwrap();
        assert_eq!(journal.db.query_row("SELECT count(*) FROM ordinary_metadata_publication WHERE json_extract(body,'$.native')=1", [], |r| r.get::<_,i64>(0)).unwrap(), 16);
        assert!(scan.after < scan.highwater.unwrap());
        backfill_native_metadata(&journal.db, &scope.account, &mut scan).unwrap();
        assert_eq!(journal.db.query_row("SELECT count(*) FROM ordinary_metadata_publication WHERE json_extract(body,'$.native')=1", [], |r| r.get::<_,i64>(0)).unwrap(), 17);
        assert_eq!(scan.after, scan.highwater.unwrap());
        assert_eq!(bodies(&journal), before);
        let fresh = native_ack(_root.path(), &mut journal, &scope, "after-highwater");
        assert!(
            journal
                .native_metadata_for_package(fresh.id)
                .unwrap()
                .native
        );
        journal
            .db
            .execute(
                "DELETE FROM ordinary_metadata_publication WHERE operation=?1",
                [fresh.id.to_string()],
            )
            .unwrap();
        backfill_native_metadata(&journal.db, &scope.account, &mut scan).unwrap();
        assert!(
            !journal
                .db
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM ordinary_metadata_publication WHERE operation=?1)",
                    [fresh.id.to_string()],
                    |r| r.get::<_, bool>(0)
                )
                .unwrap()
        );
        let restored = journal.native_metadata_for_package(fresh.id).unwrap();
        journal.finish_native_metadata(&restored).unwrap();
        journal.finish_native_metadata(&restored).unwrap();
        journal
            .finish_ordinary_metadata(&restored, true, 0)
            .unwrap();
    }

    #[test]
    fn metadata_native_invalid_history_is_retained_and_valid_sibling_is_not_starved() {
        for arm in [
            "foreign_scope",
            "missing_owner",
            "owner_body_id",
            "incomplete_queue",
            "invalid_native_proof",
        ] {
            let (root, mut journal, scope) = fixture();
            let bad = native_ack(root.path(), &mut journal, &scope, "bad");
            let good = native_ack(root.path(), &mut journal, &scope, "good");
            journal
                .db
                .execute("DELETE FROM ordinary_metadata_publication", [])
                .unwrap();
            match arm {
                "foreign_scope" => {
                    journal.db.execute("UPDATE uploads SET body=json_set(body,'$.scope.account','foreign') WHERE id=?1", [bad.id.to_string()]).unwrap();
                }
                "missing_owner" => {
                    journal
                        .db
                        .execute(
                            "DELETE FROM namespace_operations WHERE operation=?1",
                            [bad.id.to_string()],
                        )
                        .unwrap();
                }
                "owner_body_id" => {
                    journal.db.execute("UPDATE namespace_objects SET body=json_set(body,'$.id',?2) WHERE id=(SELECT object FROM namespace_operations WHERE operation=?1)", params![bad.id.to_string(), Uuid::new_v4().to_string()]).unwrap();
                }
                "incomplete_queue" => {
                    journal
                        .db
                        .execute(
                            "UPDATE write_queue SET complete=0 WHERE id=?1",
                            [bad.id.to_string()],
                        )
                        .unwrap();
                }
                _ => {
                    journal.db.execute("UPDATE uploads SET body=json_set(body,'$.identity_handoff.backup.id','foreign') WHERE id=?1", [bad.id.to_string()]).unwrap();
                }
            }
            let before = bodies(&journal);
            let payloads: Vec<_> = [&bad, &good]
                .iter()
                .map(|row| std::fs::read(journal.objects.join(row.id.to_string())).unwrap())
                .collect();
            let mut scan = NativeMetadataScan::default();
            backfill_native_metadata(&journal.db, &scope.account, &mut scan).unwrap();
            journal.native_metadata_scan.set(scan);
            journal
                .db
                .execute(
                    "UPDATE ordinary_metadata_publication SET retry_after=1 WHERE operation=?1",
                    [good.id.to_string()],
                )
                .unwrap();
            assert!(journal.ordinary_metadata_due(0).is_err(), "{arm}");
            let proof = journal.ordinary_metadata_due(1).unwrap().unwrap();
            assert!(proof.native);
            assert_eq!(proof.operation, good.id, "{arm}");
            journal.finish_ordinary_metadata(&proof, true, 0).unwrap();
            assert_eq!(journal.db.query_row("SELECT done,failures FROM ordinary_metadata_publication WHERE operation=?1", [bad.id.to_string()], |r| Ok((r.get::<_,bool>(0)?,r.get::<_,i64>(1)?))).unwrap(), (false, 1));
            assert_eq!(bodies(&journal), before, "{arm}");
            assert_eq!(
                [&bad, &good]
                    .iter()
                    .map(|row| std::fs::read(journal.objects.join(row.id.to_string())).unwrap())
                    .collect::<Vec<_>>(),
                payloads,
                "{arm}"
            );
        }
    }

    #[test]
    fn metadata_native_and_ordinary_missing_capture_are_not_promoted_on_history_read() {
        let (root, mut journal, scope) = fixture();
        let native = native_ack(root.path(), &mut journal, &scope, "legacy");
        let ordinary = ordinary_ack(&mut journal, &scope);
        journal
            .db
            .execute("DELETE FROM ordinary_metadata_publication", [])
            .unwrap();
        for row in [&native, &ordinary] {
            journal.db.execute("UPDATE uploads SET body=json_remove(body,'$.identity_handoff.metadata_original') WHERE id=?1", [row.id.to_string()]).unwrap();
        }
        let before = bodies(&journal);
        assert!(journal.ordinary_metadata_due(0).unwrap().is_none());
        assert!(journal.native_metadata_for_package(native.id).is_err());
        assert_eq!(
            journal
                .db
                .query_row(
                    "SELECT count(*) FROM ordinary_metadata_publication",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert_eq!(bodies(&journal), before);
    }
}
