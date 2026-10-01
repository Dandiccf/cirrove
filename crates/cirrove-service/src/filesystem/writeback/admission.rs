//! Representation admission binds a metadata-only provider check to the exact
//! journal identity observed before it. No journal or projection lock crosses IO.
use super::*;

#[derive(Clone, PartialEq)]
struct Binding {
    id: Uuid,
    revision: u64,
    remote_owned: bool,
    remote: Option<Node>,
    unlinked: bool,
}
impl From<&NamespaceObject> for Binding {
    fn from(object: &NamespaceObject) -> Self {
        Self {
            id: object.id,
            revision: object.revision,
            remote_owned: object.remote_owned,
            remote: object.remote.clone(),
            unlinked: object.unlinked,
        }
    }
}
#[derive(Clone)]
pub(super) struct WriteAdmission {
    scope: Scope,
    item: String,
    binding: Option<Binding>,
}
fn object(
    journal: &UploadJournal,
    scope: &Scope,
    item: &str,
) -> crate::journal::Result<Option<NamespaceObject>> {
    if let Some(local) = journal.namespace_by_local(scope, item)? {
        return Ok(Some(local));
    }
    journal.namespace_by_remote(scope, item)
}
impl WriteAdmission {
    pub(super) fn materialized(object: &NamespaceObject) -> Self {
        Self {
            scope: object.scope.clone(),
            item: object.node.id.clone(),
            binding: Some(Binding::from(object)),
        }
    }
    pub(super) fn recheck(&self, journal: &UploadJournal) -> crate::journal::Result<()> {
        let current = object(journal, &self.scope, &self.item)?;
        if current.as_ref().map(Binding::from) != self.binding {
            return Err(JournalError::Stale);
        }
        Ok(())
    }
}
impl Writeback {
    pub(super) async fn admit_write(
        &self,
        engine: &Engine,
        scope: &Scope,
        node: &Node,
        cancel: &CancellationToken,
    ) -> Result<WriteAdmission> {
        let scope = scope.clone();
        let node = node.clone();
        let (admission, remote) = self
            .local(move |journal| {
                let known = object(journal, &scope, &node.id)?;
                if known
                    .as_ref()
                    .is_some_and(|o| o.unlinked || !o.remote_owned)
                {
                    return Err(JournalError::Stale);
                }
                let remote = match &known {
                    Some(known) => known.remote.clone(),
                    None => Some(node.clone()),
                };
                // Only a journal-owned object with no remote backing is exempt.
                // An unknown/local-looking string must still reach provider validation.
                let admission = WriteAdmission {
                    scope,
                    item: node.id,
                    binding: known.as_ref().map(Binding::from),
                };
                Ok((admission, remote))
            })
            .await?;
        if let Some(remote) = remote {
            tokio::select! { biased;
                _ = cancel.cancelled() => return Err(Errno::ENODEV),
                result = tokio::time::timeout(
                    std::time::Duration::from_secs(60),
                    engine.provider.validate_write_target(&admission.scope, &remote, cancel),
                ) => result.map_err(|_| Errno::ETIMEDOUT)?.map_err(|e| errno(&e))?,
            }
        }
        Ok(admission)
    }
}
