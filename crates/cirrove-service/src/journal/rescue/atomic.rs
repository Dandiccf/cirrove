//! Rescue a refused editor replacement without releasing its old cleanup early.
use super::*;
use cirrove_core::mutation::MutationIntent;

pub(super) struct AtomicRescue {
    record: ReplacementRecord,
    victim: NamespaceObject,
    cleanup_object: NamespaceObject,
    cleanup: MutationRecord,
}
impl AtomicRescue {
    pub fn cleanup_id(&self) -> Uuid {
        self.cleanup.id
    }
}

pub(super) fn prepare(
    journal: &UploadJournal,
    upload: &UploadRecord,
    object: Option<&NamespaceObject>,
) -> Result<Option<AtomicRescue>> {
    prepare_bound(journal, upload, object, &[upload.id], None)
}

pub(super) fn prepare_bound(
    journal: &UploadJournal,
    upload: &UploadRecord,
    object: Option<&NamespaceObject>,
    allowed: &[Uuid],
    predecessor: Option<&NamespaceObject>,
) -> Result<Option<AtomicRescue>> {
    let Some(object) = object else {
        return Ok(None);
    };
    let mut query = journal
        .db
        .prepare("SELECT body FROM file_replacements WHERE id=?1 OR source=?2 OR victim=?2")?;
    let records = query.query_map(params![upload.id.to_string(), object.id.to_string()], |r| {
        r.get::<_, String>(0)
    })?;
    let mut active = None;
    for row in records {
        let record: ReplacementRecord = serde_json::from_str(&row?)?;
        if record.id == upload.id {
            active = Some(record);
        } else if !allowed.contains(&record.id)
            && (!(record.remote_applied || record.rescued_as.is_some())
                || journal.mutation(record.cleanup)?.state != MutationState::Applied)
        {
            // Another takeover still owns one of these bindings.
            return Err(JournalError::Stale);
        }
    }
    let Some(record) = active else {
        return Ok(None);
    };
    if record.rescued_as.is_some() || !record.local_ready || record.remote_applied {
        return Err(JournalError::Stale);
    }
    let victim = if let Some(previous) = predecessor {
        if record.source != object.id
            || record.victim != previous.id
            || previous.scope != upload.scope
            || !previous.unlinked
            || !previous.remote_owned
            || previous.node.package
            || previous.node.target.is_some()
            || previous.node.kind != NodeKind::File
            || previous
                .remote
                .as_ref()
                .is_none_or(|r| object.remote.as_ref().is_none_or(|n| n.id == r.id))
        {
            return Err(JournalError::Stale);
        }
        previous.clone()
    } else {
        replacements::handoff_victim(&journal.db, upload, object)?.ok_or(JournalError::Stale)?
    };
    let cleanup_object = journal.namespace_object(record.cleanup_object)?;
    let cleanup = journal.mutation(record.cleanup)?;
    let remote = object.remote.as_ref().ok_or(JournalError::Stale)?;
    if cleanup.state != MutationState::Pending
        || cleanup.attempt.is_some()
        || cleanup.receipt.is_some()
        || cleanup.prepared_item.is_some()
        || cleanup.failed_attempts != 0
        || cleanup.working_file.is_some()
        || !cleanup.local_ready
        || cleanup.base.as_ref().is_some_and(|base| !base.resolved)
        || cleanup.request.scope != upload.scope
        || !matches!(&cleanup.request.intent, MutationIntent::RemoveFile { before } if before == remote)
        || cleanup_object.scope != upload.scope
        || !cleanup_object.unlinked
        || cleanup_object.remote_owned
        || cleanup_object.working_file.is_some()
        || cleanup_object.latest != Some(cleanup.id)
        || journal.operation_prerequisites(cleanup.id)? != vec![upload.id]
    {
        return Err(JournalError::Stale);
    }
    let dependent: bool = journal.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM write_prerequisites WHERE predecessor=?1
        UNION ALL SELECT 1 FROM write_successors WHERE predecessor=?1)",
        [cleanup.id.to_string()],
        |r| r.get(0),
    )?;
    if dependent {
        return Err(JournalError::Stale);
    }
    Ok(Some(AtomicRescue {
        record,
        victim,
        cleanup_object,
        cleanup,
    }))
}

pub(super) fn commit(
    tx: &rusqlite::Transaction<'_>,
    saved: &AtomicRescue,
    source: &NamespaceObject,
    copy: &UploadRecord,
) -> Result<Node> {
    let mut victim = namespace::by_id(tx, saved.victim.id)?;
    let mut cleanup_object = namespace::by_id(tx, saved.cleanup_object.id)?;
    if victim.revision != saved.victim.revision
        || cleanup_object.revision != saved.cleanup_object.revision
    {
        return Err(JournalError::Stale);
    }
    let original = victim.remote.clone().ok_or(JournalError::Stale)?;
    let temporary = source.remote.clone().ok_or(JournalError::Stale)?;
    // Keep the detached victim stream for existing readers, but free its cloud
    // binding for a fresh alias. The source's binding was freed by rescue::commit.
    victim.remote_owned = false;
    victim.revision = victim.revision.checked_add(1).ok_or(JournalError::Quota)?;
    namespace::save(tx, &victim)?;

    // The old operation has never reached a provider. Resolve it explicitly;
    // pretending it was Applied would invent a deletion receipt.
    let mut old = saved.cleanup.clone();
    old.state = MutationState::Resolved;
    if tx.execute(
        "UPDATE mutations SET state='resolved',body=?2 WHERE id=?1 AND state='pending'",
        params![old.id.to_string(), serde_json::to_string(&old)?],
    )? != 1
    {
        return Err(JournalError::Stale);
    }
    mutations::queue_complete(tx, old.id, true)?;
    tx.execute(
        "DELETE FROM namespace_operations WHERE operation=?1",
        [old.id.to_string()],
    )?;

    // Append instead of resequencing history: every prerequisite must point to
    // an older operation. No cleanup is eligible until the rescue is confirmed.
    let mut cleanup = saved.cleanup.clone();
    cleanup.id = Uuid::new_v4();
    cleanup.base = None;
    cleanup.retry_at = 0;
    cleanup.sequence = mutations::queue_insert(
        tx,
        cleanup.id,
        mutations::mutation_resources(&cleanup.request)?,
    )?;
    barriers::insert(tx, cleanup.id, cleanup.sequence, &copy.scope, &[copy.id])?;
    tx.execute(
        "INSERT INTO mutations VALUES(?1,?2,'pending',?3)",
        params![
            cleanup.sequence as i64,
            cleanup.id.to_string(),
            serde_json::to_string(&cleanup)?
        ],
    )?;
    let local_id = cleanup_object.node.id.clone();
    cleanup_object.node = temporary.clone();
    cleanup_object.node.id = local_id;
    directories::localize_parent(tx, &copy.scope, &mut cleanup_object.node)?;
    cleanup_object.remote = Some(temporary);
    cleanup_object.remote_owned = true;
    cleanup_object.remote_sequence = source.remote_sequence;
    cleanup_object.latest = Some(cleanup.id);
    cleanup_object.revision = cleanup_object
        .revision
        .checked_add(1)
        .ok_or(JournalError::Quota)?;
    namespace::save(tx, &cleanup_object)?;
    let mut record = saved.record.clone();
    record.cleanup = cleanup.id;
    record.rescued_as = Some(copy.id);
    replacements::save(tx, &record)?;
    Ok(original)
}
