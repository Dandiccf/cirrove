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
// A kernel ESTALE retry may reuse the old lookup view after a local rename.
// A fresh admission snapshot alone would then legitimize that stale pathname.
// Compare lookup location only while the journal owns the visible namespace;
// remote-following nodes may legitimately refresh their provider metadata.
fn check_pathname(
    journal: &UploadJournal,
    scope: &Scope,
    known: Option<&NamespaceObject>,
    pathname: Option<&Node>,
) -> crate::journal::Result<()> {
    let (Some(known), Some(pathname)) = (known, pathname) else {
        return Ok(());
    };
    if known.follows_remote {
        return Ok(());
    }
    let canonical_parent = |parent: &Option<String>| -> crate::journal::Result<Option<String>> {
        parent
            .as_ref()
            .map(|id| Ok(object(journal, scope, id)?.map_or_else(|| id.clone(), |p| p.node.id)))
            .transpose()
    };
    if known.node.name != pathname.name
        || canonical_parent(&known.node.parent_id)? != canonical_parent(&pathname.parent_id)?
    {
        return Err(JournalError::Stale);
    }
    Ok(())
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
        self.admit_write_path(engine, scope, node, None, cancel)
            .await
    }
    pub(super) async fn admit_write_path(
        &self,
        engine: &Engine,
        scope: &Scope,
        node: &Node,
        pathname: Option<Node>,
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
                check_pathname(journal, &scope, known.as_ref(), pathname.as_ref())?;
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn stale_lookup_location_is_refused_even_before_a_new_admission_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let scope = Scope {
            account: "fixture".into(),
            provider: "fixture".into(),
            collection: "drive".into(),
        };
        let mut journal =
            UploadJournal::open(&temp.path().join("journal"), &scope.account, 1024 * 1024).unwrap();
        let parent = Node {
            id: "parent".into(),
            parent_id: Some("root".into()),
            name: "Folder".into(),
            kind: NodeKind::Folder,
            size: 0,
            modified_unix: 0,
            etag: Some("p1".into()),
            content_version: None,
            target: None,
            package: false,
        };
        // Observing a pre-existing folder preserves its remote item ID. Create
        // and acknowledge one through the journal to establish a real local
        // directory ID bound to the distinct provider ID "parent".
        let local_parent = journal
            .create_namespace_directory(scope.clone(), "root".into(), "Folder".into())
            .unwrap();
        let creation = journal.claim_mutation().unwrap().unwrap();
        journal
            .acknowledge_mutation(
                creation.id,
                creation.attempt.unwrap(),
                cirrove_core::mutation::MutationReceipt::Upsert(parent),
            )
            .unwrap();
        let parent = journal.namespace_object(local_parent.id).unwrap();
        assert_ne!(parent.node.id, "parent");
        assert_eq!(parent.remote.as_ref().unwrap().id, "parent");
        let lookup = Node {
            id: "source".into(),
            parent_id: Some("parent".into()),
            name: "source.txt".into(),
            kind: NodeKind::File,
            size: 3,
            modified_unix: 0,
            etag: Some("v1".into()),
            content_version: Some("v1".into()),
            target: None,
            package: false,
        };
        let original = journal
            .observe_namespace_file(scope.clone(), lookup.clone())
            .unwrap();
        assert_ne!(
            original.node.parent_id, lookup.parent_id,
            "exercise remote-to-local parent normalization"
        );
        check_pathname(&journal, &scope, Some(&original), Some(&lookup)).unwrap();
        journal
            .relocate_namespace_item(
                original.id,
                original.revision,
                parent.node.id,
                "raced.txt".into(),
            )
            .unwrap();
        let changed = journal.namespace_object(original.id).unwrap();
        assert!(
            matches!(
                check_pathname(&journal, &scope, Some(&changed), Some(&lookup)),
                Err(JournalError::Stale)
            ),
            "an already-stale lookup was admitted with a fresh journal snapshot"
        );
        check_pathname(&journal, &scope, Some(&changed), Some(&changed.node)).unwrap();
        // Content metadata refresh alone is not a pathname change.
        let mut refreshed = changed.node.clone();
        refreshed.etag = Some("v2".into());
        refreshed.content_version = Some("v2".into());
        refreshed.size = 5;
        check_pathname(&journal, &scope, Some(&changed), Some(&refreshed)).unwrap();
        // A file shortcut follows its target identity, not its target's old name.
        check_pathname(&journal, &scope, Some(&changed), None).unwrap();
        // A remote-following clean object may receive a genuine remote rename.
        let mut remote = changed;
        remote.follows_remote = true;
        check_pathname(&journal, &scope, Some(&remote), Some(&lookup)).unwrap();
    }
}
