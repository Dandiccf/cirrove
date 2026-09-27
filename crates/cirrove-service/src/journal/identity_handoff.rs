//! Reserve and publish both provider identities of a staged replacement.
//! A hidden recovery object owns the former item after a verified handoff;
//! no provider request runs while either SQLite transaction is held.
use super::*;
use rusqlite::Transaction;

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Reservation {
    recovery_object: Uuid,
    old_item: String,
    recovery_name: String,
    #[serde(default)]
    backup: Option<Node>,
}

impl UploadJournal {
    /// Reserve the old item before the first remote handoff mutation. This
    /// deliberately requires a single current replacement with no successor;
    /// later generations cannot silently inherit an unproven two-ID transfer.
    pub fn reserve_identity_handoff(
        &mut self,
        id: Uuid,
        attempt: Uuid,
        recovery_name: String,
    ) -> Result<Uuid> {
        let mut record = self.active_attempt(id, attempt)?;
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
        let old = owner.remote.as_ref().ok_or(JournalError::Stale)?;
        if owner.scope != record.scope
            || owner.unlinked
            || owner.follows_remote
            || !owner.remote_owned
            || owner.latest != Some(id)
            || old.id != *item
            || old.etag.as_deref() != Some(expected_etag)
            || old.kind != NodeKind::File
            || old.target.is_some()
            || old.parent_id.is_none()
            || owner.node.name != old.name
            || owner.node.parent_id != old.parent_id
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
        let replacement: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM file_replacements WHERE id=?1)",
            [id.to_string()],
            |row| row.get(0),
        )?;
        let count: i64 =
            self.db
                .query_row("SELECT count(*) FROM namespace_objects", [], |row| {
                    row.get(0)
                })?;
        if replacement || count >= 10_000 {
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
            remote_sequence: owner.remote_sequence,
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
        let mut record = self.active_attempt(id, attempt)?;
        let reservation = record
            .identity_handoff
            .as_mut()
            .ok_or(JournalError::Intent)?;
        if !matches!(&record.intent, UploadIntent::Replace { item, .. } if item == &reservation.old_item)
            || current.id.is_empty()
            || current.id == reservation.old_item
            || current.kind != NodeKind::File
            || current.target.is_some()
            || current.size != record.size
            || current.content_revision().is_none()
            || backup.id != reservation.old_item
            || backup.name != reservation.recovery_name
            || backup.kind != NodeKind::File
            || backup.target.is_some()
            || backup.content_revision().is_none()
        {
            return Err(JournalError::Corrupt);
        }
        reservation.backup = Some(backup);
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
    let old = owner.remote.as_ref().ok_or(JournalError::Corrupt)?;
    if owner.scope != record.scope
        || recovery.scope != record.scope
        || owner.unlinked
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
        || backup.id != old.id
        || backup.name != reservation.recovery_name
        || backup.parent_id != old.parent_id
        || current.parent_id != old.parent_id
        || current.name != old.name
        || current.id == old.id
        || owner.remote_sequence >= record.sequence
    {
        return Err(JournalError::Corrupt);
    }
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
    recovery.remote = Some(backup.clone());
    recovery.remote_owned = true;
    recovery.remote_sequence = record.sequence;
    recovery.node.name = backup.name.clone();
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
