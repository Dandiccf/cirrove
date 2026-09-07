//! Publish a committed namespace frontier, including every side of a transfer.
use super::*;
use crate::journal::NamespacePublication;
use std::collections::HashSet;

impl Projection {
    fn publish_batch(&mut self, batch: NamespacePublication) -> Result<bool> {
        if batch.after > self.frontier || batch.through < batch.after {
            return Err(Errno::ESTALE);
        }
        if batch.through <= self.frontier {
            return Ok(false);
        }
        let mut updates = Vec::with_capacity(batch.objects.len());
        let mut ids = HashSet::new();
        let mut locals = HashMap::new();
        let mut remotes = HashMap::new();
        for snapshot in batch.objects {
            let object = &snapshot.object;
            if !ids.insert(object.id) {
                return Err(Errno::EIO);
            }
            if !self.validate_snapshot(object, snapshot.working.as_ref())? {
                continue;
            }
            if locals
                .insert(key(&object.scope, &object.node.id), object.id)
                .is_some()
            {
                return Err(Errno::EIO);
            }
            if object.remote_owned
                && let Some(remote) = &object.remote
                && remotes
                    .insert(key(&object.scope, &remote.id), object.id)
                    .is_some()
            {
                return Err(Errno::EIO);
            }
            updates.push(snapshot);
        }
        let changed: HashSet<_> = updates.iter().map(|s| s.object.id).collect();
        for (identity, owner) in &remotes {
            if let Some(old) = self.remote_bindings.get(identity)
                && old != owner
                && !changed.contains(old)
            {
                return Err(Errno::EIO);
            }
        }
        // Validation above changes nothing. Remove all old bindings before
        // assigning any new one, so UUID/order of the pair cannot affect it.
        for snapshot in &updates {
            if let Some(old) = self.objects.get(&snapshot.object.id)
                && old.remote_owned
                && let Some(remote) = &old.remote
            {
                self.remote_bindings.remove(&key(&old.scope, &remote.id));
            }
        }
        let changed = !updates.is_empty();
        for snapshot in updates {
            self.apply(snapshot.object, snapshot.working);
        }
        self.frontier = batch.through;
        Ok(changed)
    }

    /// The caller owns the journal, then the projection. Only local indexed
    /// metadata reads occur here; no file hydration or provider request.
    pub(super) fn catch_up(&mut self, journal: &UploadJournal) -> Result<bool> {
        self.publish_batch(
            journal
                .namespace_publication(self.frontier)
                .map_err(error)?,
        )
    }
}

impl Writeback {
    pub(super) fn publish_locked(
        journal: &UploadJournal,
        projection: &Mutex<Projection>,
    ) -> Result<bool> {
        let after = projection.lock().map_err(|_| Errno::EIO)?.frontier;
        // Keep the journal stable, but let cached directory/stream lookups run
        // while SQLite reads and decodes the changed metadata.
        let batch = journal.namespace_publication(after).map_err(error)?;
        projection
            .lock()
            .map_err(|_| Errno::EIO)?
            .publish_batch(batch)
    }
    pub(super) async fn refresh_projection(&self) -> Result<bool> {
        let journal = self.journal.clone();
        let projection = self.projection.clone();
        tokio::task::spawn_blocking(move || {
            let journal = journal.lock().map_err(|_| Errno::EIO)?;
            Self::publish_locked(&journal, &projection)
        })
        .await
        .map_err(|_| Errno::EIO)?
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::journal::ReplacementRecord;

    fn scope() -> Scope {
        Scope {
            account: "publication".into(),
            provider: "fixture".into(),
            collection: "drive".into(),
        }
    }
    fn node(id: &str, name: &str, tag: &str) -> Node {
        Node {
            id: id.into(),
            parent_id: Some("root".into()),
            name: name.into(),
            kind: NodeKind::File,
            size: 3,
            etag: Some(tag.into()),
            content_version: Some(tag.into()),
            modified_unix: 1,
            target: None,
        }
    }
    fn file(j: &mut UploadJournal, id: &str, name: &str, bytes: &[u8]) -> WorkingFile {
        j.create_working(scope(), node(id, name, "original"), false, bytes)
            .unwrap()
    }
    fn replace(
        j: &mut UploadJournal,
        source: &WorkingFile,
        victim: &WorkingFile,
    ) -> ReplacementRecord {
        let source = j
            .namespace_by_local(&scope(), &source.node.id)
            .unwrap()
            .unwrap();
        let victim = j
            .namespace_by_local(&scope(), &victim.node.id)
            .unwrap()
            .unwrap();
        j.replace_namespace_file(
            source.id,
            source.revision,
            victim.id,
            victim.revision,
            false,
        )
        .unwrap()
    }
    fn ack(j: &mut UploadJournal, replacement: &ReplacementRecord, tag: &str) {
        let upload = j.claim_next().unwrap().unwrap();
        assert_eq!(upload.id, replacement.id);
        j.acknowledge(
            upload.id,
            upload.attempt.unwrap(),
            node("target", "document", tag),
        )
        .unwrap();
    }
    fn owner(p: &Projection, id: &str) -> Option<Uuid> {
        p.remote_bindings.get(&key(&scope(), id)).copied()
    }
    fn bytes(p: &Projection, j: &UploadJournal, local: &WorkingFile) -> Vec<u8> {
        let file = p
            .local_object(&scope(), &local.node.id)
            .unwrap()
            .working_file
            .unwrap();
        j.read_working(file, 0, 32).unwrap()
    }

    #[test]
    fn replacement_publication_transfers_all_bindings_in_any_order_and_retains_old_streams() {
        let tmp = tempfile::tempdir().unwrap();
        let mut j =
            UploadJournal::open(&tmp.path().join("journal"), &scope().account, 4096).unwrap();
        let old = file(&mut j, "target", "document", b"old");
        let new = file(&mut j, "source", "temporary", b"new");
        let mut p = Projection::default();
        p.catch_up(&j).unwrap();
        let source_id = p.local_object(&scope(), &new.node.id).unwrap().id;
        let victim_id = p.local_object(&scope(), &old.node.id).unwrap().id;
        let operation = replace(&mut j, &new, &old);
        let local = j.namespace_publication(p.frontier).unwrap();
        let local_through = local.through;
        assert_eq!(local.objects.len(), 3);
        p.publish_batch(local).unwrap();
        assert!(p.local_object(&scope(), &old.node.id).unwrap().unlinked);
        assert_eq!(
            p.local_object(&scope(), &new.node.id).unwrap().node.name,
            "document"
        );
        ack(&mut j, &operation, "published");
        let mut receipt = j.namespace_publication(p.frontier).unwrap();
        assert_eq!(receipt.objects.len(), 3);
        // This order failed with per-object publication: target is still owned
        // by the victim when the source attempts to acquire it.
        receipt
            .objects
            .sort_by_key(|s| if s.object.id == source_id { 0 } else { 1 });
        p.publish_batch(receipt).unwrap();
        assert_eq!(owner(&p, "target"), Some(source_id));
        assert_eq!(owner(&p, "source"), Some(operation.cleanup_object));
        assert!(!p.objects[&victim_id].remote_owned);
        assert_eq!(bytes(&p, &j, &old), b"old");
        assert_eq!(bytes(&p, &j, &new), b"new");
        let frontier = p.frontier;
        assert!(
            !p.publish_batch(NamespacePublication {
                after: 0,
                through: local_through,
                objects: vec![]
            })
            .unwrap()
        );
        assert_eq!(p.frontier, frontier);
        assert_eq!(owner(&p, "target"), Some(source_id));
        assert!(!p.catch_up(&j).unwrap());
    }

    #[test]
    fn delayed_callbacks_coalesce_chained_replacements_including_the_newest_owner() {
        let tmp = tempfile::tempdir().unwrap();
        let mut j =
            UploadJournal::open(&tmp.path().join("journal"), &scope().account, 4096).unwrap();
        let old = file(&mut j, "target", "document", b"old");
        let first = file(&mut j, "first", "temp-one", b"one");
        let second = file(&mut j, "second", "temp-two", b"two");
        let mut p = Projection::default();
        p.catch_up(&j).unwrap();
        let base = p.frontier;
        let one = replace(&mut j, &first, &old);
        let delayed = j.namespace_publication(base).unwrap();
        let two = replace(&mut j, &second, &first);
        ack(&mut j, &one, "one");
        let middle = j.namespace_publication(base).unwrap();
        ack(&mut j, &two, "two");
        // No callback has published either rename or acknowledgement yet.
        assert!(p.catch_up(&j).unwrap());
        let second_id = p.local_object(&scope(), &second.node.id).unwrap().id;
        assert_eq!(owner(&p, "target"), Some(second_id));
        assert_eq!(owner(&p, "first"), Some(one.cleanup_object));
        assert_eq!(owner(&p, "second"), Some(two.cleanup_object));
        assert!(!p.publish_batch(middle).unwrap());
        assert!(!p.publish_batch(delayed).unwrap());
        assert_eq!(owner(&p, "target"), Some(second_id));
        for (file, expected) in [(&old, b"old"), (&first, b"one"), (&second, b"two")] {
            assert_eq!(bytes(&p, &j, file), expected);
        }
        let listing = project_namespace(p.objects.values(), &scope(), "root", vec![]).unwrap();
        assert_eq!(listing.nodes.len(), 1);
        assert_eq!(listing.nodes[0].id, second.node.id);
    }

    #[test]
    fn incomplete_or_corrupt_publication_changes_neither_aliases_nor_cursor() {
        let tmp = tempfile::tempdir().unwrap();
        let mut j =
            UploadJournal::open(&tmp.path().join("journal"), &scope().account, 4096).unwrap();
        let old = file(&mut j, "target", "document", b"old");
        let new = file(&mut j, "source", "temp", b"new");
        let mut p = Projection::default();
        p.catch_up(&j).unwrap();
        let base = p.frontier;
        let old_owner = owner(&p, "target");
        let source_owner = owner(&p, "source");
        let operation = replace(&mut j, &new, &old);
        ack(&mut j, &operation, "done");
        let mut incomplete = j.namespace_publication(base).unwrap();
        incomplete
            .objects
            .retain(|s| s.object.id != old_owner.unwrap());
        assert_eq!(p.publish_batch(incomplete), Err(Errno::EIO));
        assert_eq!(p.frontier, base);
        assert_eq!(owner(&p, "target"), old_owner);
        assert_eq!(owner(&p, "source"), source_owner);
        let mut corrupt = j.namespace_publication(base).unwrap();
        corrupt
            .objects
            .iter_mut()
            .find(|s| s.object.id == source_owner.unwrap())
            .unwrap()
            .working = None;
        assert_eq!(p.publish_batch(corrupt), Err(Errno::EIO));
        assert_eq!(p.frontier, base);
        assert_eq!(owner(&p, "target"), old_owner);
        let mut duplicate = j.namespace_publication(base).unwrap();
        duplicate.objects.push(duplicate.objects[0].clone());
        assert_eq!(p.publish_batch(duplicate), Err(Errno::EIO));
        assert_eq!(p.frontier, base);
        let mut gap = j.namespace_publication(base).unwrap();
        gap.after = base + 1;
        assert_eq!(p.publish_batch(gap), Err(Errno::ESTALE));
        assert!(p.catch_up(&j).unwrap());
        assert_eq!(owner(&p, "target"), source_owner);
    }

    #[test]
    fn failed_durable_ack_leaves_projection_unchanged_and_retry_publishes_once() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("journal");
        let mut j = UploadJournal::open(&root, &scope().account, 4096).unwrap();
        let old = file(&mut j, "target", "document", b"old");
        let new = file(&mut j, "source", "temp", b"new");
        let mut p = Projection::default();
        let replacement = replace(&mut j, &new, &old);
        p.catch_up(&j).unwrap();
        let frontier = p.frontier;
        let db = rusqlite::Connection::open(root.join("uploads.db")).unwrap();
        db.execute_batch("CREATE TRIGGER deny_pair BEFORE UPDATE ON file_replacements
            WHEN json_extract(NEW.body,'$.remote_applied')=1 BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
        let upload = j.claim_next().unwrap().unwrap();
        assert!(
            j.acknowledge(
                upload.id,
                upload.attempt.unwrap(),
                node("target", "document", "done")
            )
            .is_err()
        );
        assert!(!p.catch_up(&j).unwrap());
        assert_eq!(p.frontier, frontier);
        assert_eq!(owner(&p, "target"), Some(replacement.victim));
        db.execute_batch("DROP TRIGGER deny_pair").unwrap();
        j.acknowledge(
            upload.id,
            upload.attempt.unwrap(),
            node("target", "document", "done"),
        )
        .unwrap();
        assert!(p.catch_up(&j).unwrap());
        assert_eq!(owner(&p, "target"), Some(replacement.source));
        assert!(!p.catch_up(&j).unwrap());
    }
}
