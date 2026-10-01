//! Exact pathname selection for the one derived native archive. This does not
//! authorize sibling creation, ordinary namespace mutation or provider writes.
use super::*;

#[derive(Clone)]
pub(crate) struct NativeEditSelection {
    pub(crate) scope: Scope,
    pub(crate) source: Node,
    pub(crate) archive: Node,
    pub(crate) working: Option<Uuid>,
    frontier: u64,
}
impl NativeEditSelection {
    pub(crate) fn recheck(&self, journal: &UploadJournal) -> Result<()> {
        if !journal.owns_account(&self.scope.account) || clock(journal)? != self.frontier {
            return Err(JournalError::Stale);
        }
        Ok(())
    }
}
fn clock(journal: &UploadJournal) -> Result<u64> {
    let value: i64 = journal
        .db
        .query_row("SELECT value FROM namespace_clock", [], |r| r.get(0))?;
    u64::try_from(value).map_err(|_| JournalError::Corrupt)
}
fn parent(
    journal: &UploadJournal,
    scope: &Scope,
    value: &Option<String>,
) -> Result<Option<String>> {
    value
        .as_ref()
        .map(|id| {
            Ok(namespace::by_remote(&journal.db, scope, id)?
                .map_or_else(|| id.clone(), |o| o.node.id))
        })
        .transpose()
}
fn same_location(journal: &UploadJournal, scope: &Scope, a: &Node, b: &Node) -> Result<bool> {
    Ok(a.name == b.name
        && parent(journal, scope, &a.parent_id)? == parent(journal, scope, &b.parent_id)?)
}
impl UploadJournal {
    pub(crate) fn select_native_edit(
        &self,
        scope: &Scope,
        source: &Node,
        archive: &Node,
        pathname: Option<&Node>,
    ) -> Result<NativeEditSelection> {
        if !self.owns_account(&scope.account) {
            return Err(JournalError::Account);
        }
        if source.kind != NodeKind::Folder
            || !source.package
            || source.target.is_some()
            || archive.kind != NodeKind::File
            || archive.package
            || archive.target.is_some()
            || archive.name != source.name
            || parent(self, scope, &archive.parent_id)?
                != parent(self, scope, &Some(source.id.clone()))?
            || pathname.is_some_and(|p| p.id != archive.id)
        {
            return Err(JournalError::Intent);
        }
        if let Some(pathname) = pathname
            && !same_location(self, scope, archive, pathname)?
        {
            return Err(JournalError::Stale);
        }
        let owner = namespace::by_local(&self.db, scope, &source.id)?
            .or(namespace::by_remote(&self.db, scope, &source.id)?);
        let mut remote_source = source.clone();
        let mut selected_archive = archive.clone();
        let mut working = None;
        if let Some(owner) = owner {
            if owner.unlinked
                || !owner.remote_owned
                || owner.node.kind != NodeKind::Folder
                || !owner.node.package
                || owner.native_archive.is_some()
                || !same_location(self, scope, source, &owner.node)?
            {
                return Err(JournalError::Stale);
            }
            let remote = owner.remote.as_ref().ok_or(JournalError::Stale)?;
            let expected = if source.id == owner.node.id {
                &owner.node
            } else {
                remote
            };
            let mut comparable = source.clone();
            comparable.parent_id = expected.parent_id.clone();
            if &comparable != expected {
                return Err(JournalError::Stale);
            }
            remote_source = remote.clone();
            let id: Option<String> = self.db.query_row(
                "SELECT working FROM native_working_heads WHERE json_extract(body,'$.owner')=?1",
                [owner.id.to_string()], |r| r.get(0),
            ).optional()?;
            if let Some(id) = id {
                let id = Uuid::parse_str(&id).map_err(|_| JournalError::Corrupt)?;
                let child = namespace::by_id(&self.db, id)?;
                projection::validate_child(&self.db, &child)?;
                let binding = self.native_binding(id)?;
                let role = child.native_archive.as_ref().ok_or(JournalError::Corrupt)?;
                if archive.id == child.node.id {
                    if !same_location(self, scope, archive, &child.node)? {
                        return Err(JournalError::Stale);
                    }
                } else {
                    // Only the original exact artifact can still be a cached
                    // first-open pathname. Never remap an old artifact across a
                    // remote identity handoff or adopt unrelated current bytes.
                    selected_archive.parent_id = Some(remote.id.clone());
                    if archive.id != role.artifact
                        || selected_archive != binding.archive
                        || remote != &binding.source
                    {
                        return Err(JournalError::Stale);
                    }
                }
                working = Some(id);
            }
        }
        selected_archive.parent_id = Some(remote_source.id.clone());
        if working.is_none() && archive.id.starts_with("local-") {
            return Err(JournalError::Stale);
        }
        Ok(NativeEditSelection {
            scope: scope.clone(),
            source: remote_source,
            archive: selected_archive,
            working,
            frontier: clock(self)?,
        })
    }
}
#[cfg(test)]
mod tests;
