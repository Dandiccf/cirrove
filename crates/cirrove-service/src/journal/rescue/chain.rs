//! Untouched atomic-editor successors may be rescued together. No provider call.
use super::*;

pub(super) struct Chain {
    pub successors: Vec<UploadRecord>,
    pub object: NamespaceObject,
    atomics: Vec<(atomic::AtomicRescue, NamespaceObject)>,
}
impl Chain {
    pub fn commit(&self, tx: &rusqlite::Transaction<'_>, copy: &UploadRecord) -> Result<Node> {
        // Work backwards: releasing each detached victim's binding makes room
        // for its own temporary-file cleanup. The final source was moved by
        // rescue::commit. Only the oldest victim is restored at the old name.
        let mut original = None;
        for (atomic, source) in self.atomics.iter().rev() {
            original = Some(atomic::commit(tx, atomic, source, copy)?);
        }
        original.ok_or(JournalError::Stale)
    }
}

pub(super) fn prepare(j: &UploadJournal, original: &UploadRecord) -> Result<Option<Chain>> {
    let mut saves = vec![original.clone()];
    loop {
        let previous = saves.last().ok_or(JournalError::Stale)?;
        let next: Option<String> =
            j.db.query_row(
                "SELECT successor FROM write_successors WHERE predecessor=?1",
                [previous.id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        let Some(next) = next else {
            break;
        };
        let next = j.get(Uuid::parse_str(&next).map_err(|_| JournalError::Corrupt)?)?;
        if saves.len() >= 10_000 {
            return Err(JournalError::Quota);
        }
        if next.state != UploadState::Pending
            || next.sequence <= previous.sequence
            || next.scope != original.scope
            || next.intent != original.intent
            || next
                .base
                .as_ref()
                .is_none_or(|b| b.resolved || b.predecessor != previous.id)
            || next.attempt.is_some()
            || next.remote.is_some()
            || next.session_key.is_some()
            || next.identity_handoff.is_some()
            || next.transferred_bytes != 0
            || next.failed_attempts != 0
        {
            return Err(JournalError::Stale);
        }
        saves.push(next);
    }
    if saves
        .windows(2)
        .all(|pair| pair[0].working_file == pair[1].working_file)
    {
        return Ok(None);
    }
    let allowed: Vec<_> = saves.iter().map(|s| s.id).collect();
    let mut objects: Vec<NamespaceObject> = Vec::new();
    let mut atomics = Vec::new();
    let groups: Vec<_> = saves
        .chunk_by(|a, b| a.working_file == b.working_file)
        .collect();
    for (index, group) in groups.iter().enumerate() {
        let save = group.first().ok_or(JournalError::Stale)?;
        let latest = group.last().ok_or(JournalError::Stale)?;
        let object = j
            .namespace_for_operation(save.id)?
            .ok_or(JournalError::Stale)?;
        let working = j.working_file(object.working_file.ok_or(JournalError::Stale)?)?;
        let detached = index + 1 < groups.len();
        if object.unlinked != detached
            || object.follows_remote
            || !object.remote_owned
            || object.scope != original.scope
            || object.latest != Some(latest.id)
            || object.node.kind != NodeKind::File
            || object.node.package
            || object.node.target.is_some()
            || working.unlinked != detached
            || working.latest != Some(latest.id)
            || working.node != object.node
            || working.scope != original.scope
            || save.working_file != Some(working.id)
            || objects.iter().any(|o| o.id == object.id)
        {
            return Err(JournalError::Stale);
        }
        let atomic = atomic::prepare_bound(j, save, Some(&object), &allowed, objects.last())?
            .ok_or(JournalError::Stale)?;
        for member in *group {
            if j.namespace_for_operation(member.id)?.as_ref().map(|o| o.id) != Some(object.id) {
                return Err(JournalError::Stale);
            }
            let dependent:bool = j.db.query_row(
                "SELECT EXISTS(SELECT 1 FROM write_prerequisites WHERE predecessor=?1 AND operation!=?2)",
                params![member.id.to_string(),atomic.cleanup_id().to_string()], |r|r.get(0))?;
            if dependent {
                return Err(JournalError::Stale);
            }
        }
        objects.push(object.clone());
        atomics.push((atomic, object));
    }
    let object = objects.pop().ok_or(JournalError::Stale)?;
    Ok(Some(Chain {
        successors: saves.into_iter().skip(1).collect(),
        object,
        atomics,
    }))
}
