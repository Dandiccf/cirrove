//! Reserve and publish both provider identities of a staged replacement.
//! A hidden recovery object owns the former item after a verified handoff;
//! no provider request runs while either SQLite transaction is held.
use super::*;
use cirrove_core::mutation::MutationReceipt;
use cirrove_core::upload::{PackageHandoffReceipt, RecoveryLocation};
use rusqlite::Transaction;

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Reservation {
    recovery_object: Uuid,
    old_item: String,
    recovery_name: String,
    #[serde(default)]
    trash_parent: Option<String>,
    #[serde(default)]
    pub(super) backup: Option<Node>,
}

impl UploadJournal {
    /// Local directory IDs are stable across provider confirmation. A nested
    /// file's local parent therefore differs from the exact provider parent;
    /// walk only confirmed folder bindings, never infer an owner from a path.
    fn confirmed_parent_route(
        &self,
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
            let Some(folder) = namespace::by_local(&self.db, scope, &local)? else {
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
                let mutation = self.mutation(latest)?;
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
        if let Some(reservation) = &record.identity_handoff {
            let recovery = self.namespace_object(reservation.recovery_object)?;
            if reservation.old_item != *item
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
            return Ok(reservation.recovery_object);
        }
        if record.state != UploadState::Uploading
            && !(record.state == UploadState::Verifying
                && record.session_key.is_none()
                && record.transferred_bytes == 0)
        {
            return Err(JournalError::Stale);
        }
        let owner = self
            .namespace_for_operation(id)?
            .ok_or(JournalError::Stale)?;
        let victim = replacements::handoff_victim(&self.db, &record, &owner)?;
        let old_owner = victim.as_ref().unwrap_or(&owner);
        let old = old_owner.remote.as_ref().ok_or(JournalError::Stale)?;
        let parent_matches = self.confirmed_parent_route(
            &record.scope,
            owner.node.parent_id.as_deref(),
            old.parent_id.as_deref(),
        )?;
        if owner.scope != record.scope
            || (owner.unlinked && victim.is_none())
            || owner.follows_remote
            || !owner.remote_owned
            || owner.latest != Some(id)
            || old.id != *item
            || old.etag.as_deref() != Some(expected_etag)
            || if native.is_some() {
                old.kind != NodeKind::Folder || !old.package || native != Some(old)
            } else {
                old.kind != NodeKind::File || old.package
            }
            || old.target.is_some()
            || old.parent_id.is_none()
            || owner.node.name != old.name
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
            recovery_name,
            trash_parent,
            backup: None,
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
        let reservation = record
            .identity_handoff
            .as_mut()
            .ok_or(JournalError::Intent)?;
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
    if owner.scope != record.scope
        || recovery.scope != record.scope
        || (owner.unlinked && victim.is_none())
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
