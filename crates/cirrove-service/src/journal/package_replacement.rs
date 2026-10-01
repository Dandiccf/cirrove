//! Explicit archive replacement admission and sparse ownership. No mount router.
use super::*;
use rusqlite::Transaction;

pub(super) fn original(representation: &UploadRepresentation) -> Option<&Node> {
    match representation {
        UploadRepresentation::PackageReplacementArchive { original, .. } => Some(original),
        _ => None,
    }
}
pub(super) fn validate(
    scope: &Scope,
    intent: &UploadIntent,
    representation: &UploadRepresentation,
) -> Result<()> {
    let before = original(representation).ok_or(JournalError::Intent)?;
    representation
        .validate()
        .map_err(|_| JournalError::Intent)?;
    if scope.provider != "icloud"
        || scope.collection != "drive"
        || !before.id.starts_with("FILE::com.apple.CloudDocs::")
        || before.id.ends_with("::")
        || !before
            .parent_id
            .as_ref()
            .is_some_and(|p| p.starts_with("FOLDER::com.apple.CloudDocs::") && !p.ends_with("::"))
        || before.parent_id.as_deref() == Some("FOLDER::com.apple.CloudDocs::TRASH_ROOT")
        || !before.name.ends_with(".pages")
        || !matches!(intent, UploadIntent::Replace { item, expected_etag } if item == &before.id && before.etag.as_ref() == Some(expected_etag))
    {
        return Err(JournalError::Intent);
    }
    Ok(())
}
impl UploadJournal {
    /// The caller has selected and independently verified this original native
    /// revision. The replacement archive is opaque validated private staging.
    /// Neither extension alone nor an ordinary FUSE save reaches this method.
    pub fn enqueue_validated_package_replacement(
        &mut self,
        scope: Scope,
        before: Node,
        original_semantic: PackageSemanticIdentity,
        archive: crate::native_import::ValidatedPackageArchive,
        cancel: &cirrove_core::CancellationToken,
    ) -> Result<UploadRecord> {
        let (file, representation, size, sha256) = archive.into_parts();
        let UploadRepresentation::PackageArchive {
            expected_root,
            semantic,
        } = representation
        else {
            return Err(JournalError::Intent);
        };
        let intent = UploadIntent::Replace {
            item: before.id.clone(),
            expected_etag: before.etag.clone().ok_or(JournalError::Intent)?,
        };
        let representation = UploadRepresentation::PackageReplacementArchive {
            expected_root,
            semantic,
            original: Box::new(before),
            original_semantic,
        };
        self.enqueue_admitted(
            scope,
            intent,
            WriteOrder::default(),
            None,
            representation::Admission {
                representation,
                receipt: Some(representation::BoundReceipt {
                    size,
                    sha256,
                    cancel: cancel.clone(),
                }),
            },
            file,
        )
    }
}
// Runs in the same enqueue transaction, after resource/sequence allocation and
// before commit. Any refusal rolls back the row/owner; sealed bytes remain an
// orphan retained by the journal's existing publication-failure policy.
pub(super) fn attach(
    tx: &Transaction<'_>,
    record: &UploadRecord,
    native: Option<&working::native::NativeCommit>,
) -> Result<()> {
    let Some(before) = original(&record.representation) else {
        return Ok(());
    };
    validate(&record.scope, &record.intent, &record.representation)?;
    if let Some(native) = native
        && working::native::successors::attach(tx, record, native)?
    {
        return Ok(());
    }
    let mut owner = prepare_owner(
        tx,
        &record.scope,
        before,
        Some(record.id),
        native.map(|commit| commit.working_id()),
    )?;
    owner.latest = Some(record.id);
    owner.follows_remote = false;
    owner.revision = owner.revision.checked_add(1).ok_or(JournalError::Quota)?;
    namespace::save(tx, &owner)
}
#[cfg(test)]
mod tests;

#[cfg(test)]
mod worker_transport;

/// Shared only with validated native hydration. `native` names the exact local
/// stream; an unrelated explicit replacement cannot steal its source owner.
pub(crate) fn prepare_owner(
    tx: &Transaction<'_>,
    scope: &Scope,
    before: &Node,
    except: Option<Uuid>,
    native: Option<Uuid>,
) -> Result<NamespaceObject> {
    let intent = UploadIntent::Replace {
        item: before.id.clone(),
        expected_etag: before.etag.clone().ok_or(JournalError::Intent)?,
    };
    for resource in mutations::upload_resources(scope, &intent)? {
        let busy: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM write_resources r JOIN write_queue q ON q.id=r.id WHERE r.resource=?1 AND q.complete=0 AND (?2 IS NULL OR q.id!=?2))", params![resource,except.map(|id| id.to_string())], |r| r.get(0))?;
        if busy {
            return Err(JournalError::Stale);
        }
    }
    namespace::ensure_legacy_policy(tx, scope)?;
    let owner = match namespace::by_remote(tx, scope, &before.id)? {
        Some(owner) => {
            if owner.unlinked
                || !owner.remote_owned
                || owner.working_file.is_some()
                || owner.remote.as_ref() != Some(before)
                || owner.node.kind != NodeKind::Folder
                || !owner.node.package
                || owner.node.name != before.name
                || owner.node.target.is_some()
            {
                return Err(JournalError::Stale);
            }
            if let Some(latest) = owner.latest {
                let complete: Option<bool> = tx
                    .query_row(
                        "SELECT complete FROM write_queue WHERE id=?1",
                        [latest.to_string()],
                        |r| r.get(0),
                    )
                    .optional()?;
                if complete != Some(true) {
                    return Err(JournalError::Stale);
                }
            }
            owner
        }
        None => {
            let count: i64 =
                tx.query_row("SELECT count(*) FROM namespace_objects", [], |r| r.get(0))?;
            if count >= 10_000 {
                return Err(JournalError::Quota);
            }
            let id = Uuid::new_v4();
            let mut local = before.clone();
            local.id = format!("local-native-{id}");
            directories::localize_parent(tx, scope, &mut local)?;
            NamespaceObject {
                native_archive: None,
                id,
                scope: scope.clone(),
                names: namespace::policy(tx, scope)?,
                node: local,
                remote: Some(before.clone()),
                remote_owned: true,
                remote_sequence: 0,
                working_file: None,
                latest: None,
                revision: 0,
                follows_remote: false,
                unlinked: false,
            }
        }
    };
    let attached: Option<String> = tx
        .query_row(
            "SELECT working FROM native_working_heads WHERE json_extract(body,'$.owner')=?1",
            [owner.id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    if attached.is_some_and(|id| native.map(|n| n.to_string()).as_ref() != Some(&id)) {
        return Err(JournalError::Stale);
    }
    Ok(owner)
}
