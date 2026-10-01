#![allow(clippy::unwrap_used)]
use super::*;
pub(super) fn temp() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/var/tmp")
        .unwrap()
}
pub(super) fn bytes(value: &[u8]) -> Vec<u8> {
    crate::native_import::synthetic_package_archive("Owned.pages/Document", value)
}
pub(super) fn binding(t: &tempfile::TempDir, data: &[u8]) -> NativeArchiveBinding {
    let path = t.path().join(format!("{}.zip", Uuid::new_v4()));
    std::fs::write(&path, data).unwrap();
    let archive = crate::native_import::ValidatedPackageArchive::capture(
        &path,
        t.path(),
        "Owned.pages",
        &CancellationToken::new(),
    )
    .unwrap();
    let (_, repr, _, sha) = archive.into_parts();
    let UploadRepresentation::PackageArchive { semantic, .. } = repr else {
        panic!("package")
    };
    let scope = Scope {
        account: "native-working".into(),
        provider: "icloud".into(),
        collection: "drive".into(),
    };
    let source = Node {
        id: "FILE::com.apple.CloudDocs::original".into(),
        parent_id: Some("FOLDER::com.apple.CloudDocs::owned".into()),
        name: "Owned.pages".into(),
        kind: NodeKind::Folder,
        size: 17,
        modified_unix: 0,
        etag: Some("v1".into()),
        content_version: None,
        target: None,
        package: true,
    };
    let archive = Node {
        id: format!("icloud-artifact:{}", source.id),
        parent_id: Some(source.id.clone()),
        name: source.name.clone(),
        kind: NodeKind::File,
        size: data.len() as u64,
        modified_unix: 0,
        etag: None,
        content_version: Some(format!(
            "icloud-artifact-v2:{}",
            serde_json::json!({"source_etag":"v1","source_size":17,"source_parent":source.parent_id,"sha256":sha})
        )),
        target: None,
        package: false,
    };
    NativeArchiveBinding {
        scope,
        source,
        archive,
        semantic,
    }
}
pub(super) fn journal(t: &tempfile::TempDir) -> UploadJournal {
    UploadJournal::open(
        &t.path().join("journal"),
        "native-working",
        16 * 1024 * 1024,
    )
    .unwrap()
}
pub(super) fn publish(j: &mut UploadJournal, b: NativeArchiveBinding, data: &[u8]) -> WorkingFile {
    let mut hydrate = j.reserve_native_working(b).unwrap();
    hydrate.write_chunk(data).unwrap();
    let ready = hydrate.validate(&CancellationToken::new()).unwrap();
    j.publish_native_working(ready).unwrap()
}
pub(super) fn edit(j: &mut UploadJournal, id: Uuid, data: &[u8]) {
    j.truncate_working(id, 0).unwrap();
    j.write_working(id, 0, data).unwrap();
}
pub(super) fn count(j: &UploadJournal) -> i64 {
    j.db.query_row("SELECT count(*) FROM uploads", [], |r| r.get(0))
        .unwrap()
}
#[test]
fn bound_hydration_reopens_and_first_seal_uses_native_owner_only() {
    let t = temp();
    let original = bytes(b"original contents");
    let expected = binding(&t, &original);
    let mut j = journal(&t);
    let f = publish(&mut j, expected.clone(), &original);
    assert!(f.native);
    assert!(!f.dirty);
    assert_eq!(j.namespace_objects().unwrap().len(), 2);
    drop(j);
    let mut j = journal(&t);
    let edited = bytes(b"new contents");
    edit(&mut j, f.id, &edited);
    assert!(matches!(j.seal_working(f.id), Err(JournalError::Intent)));
    let snapshot = j
        .capture_native_working(f.id)
        .unwrap()
        .capture(&CancellationToken::new())
        .unwrap();
    let row = j
        .seal_captured_native_working(snapshot, &CancellationToken::new())
        .unwrap();
    assert_eq!(row.working_file, None);
    assert_eq!(row.size, edited.len() as u64);
    assert_eq!(count(&j), 1);
    let UploadRepresentation::PackageReplacementArchive {
        original,
        original_semantic,
        ..
    } = &row.representation
    else {
        panic!("native replacement")
    };
    assert_eq!(original.as_ref(), &expected.source);
    assert_eq!(original_semantic, &expected.semantic);
    let working = j.working_file(f.id).unwrap();
    assert!(!working.dirty);
    assert_eq!(working.latest, Some(row.id));
    let owner = j.namespace_for_operation(row.id).unwrap().unwrap();
    assert_eq!(owner.node.kind, NodeKind::Folder);
    assert!(owner.node.package);
    assert_eq!(owner.working_file, None);
    edit(&mut j, f.id, &bytes(b"second save retained"));
    let captured = j
        .capture_native_working(f.id)
        .unwrap()
        .capture(&CancellationToken::new())
        .unwrap();
    let second = j
        .seal_captured_native_working(captured, &CancellationToken::new())
        .unwrap();
    assert_eq!(second.base.as_ref().unwrap().predecessor, row.id);
    assert_eq!(count(&j), 2);
    edit(&mut j, f.id, &bytes(b"third save remains dirty"));
    drop(j);
    let ro = RecoveryJournal::open(&t.path().join("journal"), "native-working").unwrap();
    assert_eq!(ro.working_recovery_list(None, 20).unwrap().0.len(), 1);
    let export = ro.local_export_source(row.id).unwrap();
    let target = t.path().join("saved.zip");
    export
        .copy_to(&target, &CancellationToken::new(), |_| {})
        .unwrap();
    assert_eq!(std::fs::read(target).unwrap(), edited);
}
#[test]
fn stale_copy_cannot_seal_or_clear_later_bytes() {
    for mutate_after_copy in [false, true] {
        let t = temp();
        let data = bytes(b"original contents");
        let mut j = journal(&t);
        let f = publish(&mut j, binding(&t, &data), &data);
        edit(&mut j, f.id, &bytes(b"edit one"));
        let capture = j.capture_native_working(f.id).unwrap();
        let newer = bytes(b"edit two is authoritative local generation");
        let snapshot = if mutate_after_copy {
            let s = capture.capture(&CancellationToken::new()).unwrap();
            edit(&mut j, f.id, &newer);
            s
        } else {
            edit(&mut j, f.id, &newer);
            match capture.capture(&CancellationToken::new()) {
                Ok(s) => s,
                Err(_) => {
                    assert_eq!(count(&j), 0);
                    assert!(j.working_file(f.id).unwrap().dirty);
                    continue;
                }
            }
        };
        assert!(matches!(
            j.seal_captured_native_working(snapshot, &CancellationToken::new()),
            Err(JournalError::Stale)
        ));
        assert_eq!(count(&j), 0);
        assert!(j.working_file(f.id).unwrap().dirty);
        assert_eq!(j.read_working(f.id, 0, 4096).unwrap(), newer);
    }
}
#[test]
fn wrong_binding_missing_binding_and_cancel_never_enqueue() {
    let t = temp();
    let data = bytes(b"original contents");
    let b = binding(&t, &data);
    let mut j = journal(&t);
    for field in 0..5 {
        let mut bad = b.clone();
        match field {
            0 => bad.scope.account = "other".into(),
            1 => bad.archive.id.push('x'),
            2 => bad.source.etag = Some("other".into()),
            3 => bad.archive.parent_id = Some("other".into()),
            _ => bad.semantic.sha256 = "0".repeat(64),
        };
        match j.reserve_native_working(bad) {
            Err(_) => {}
            Ok(mut h) => {
                h.write_chunk(&data).unwrap();
                assert!(h.validate(&CancellationToken::new()).is_err());
            }
        }
    }
    let f = publish(&mut j, b, &data);
    edit(&mut j, f.id, &bytes(b"edit"));
    let capture = j.capture_native_working(f.id).unwrap();
    let token = CancellationToken::new();
    token.cancel();
    assert!(capture.capture(&token).is_err());
    let captured = j
        .capture_native_working(f.id)
        .unwrap()
        .capture(&CancellationToken::new())
        .unwrap();
    assert!(j.seal_captured_native_working(captured, &token).is_err());
    j.db.execute(
        "DELETE FROM native_working_bindings WHERE working=?1",
        [f.id.to_string()],
    )
    .unwrap();
    assert!(j.capture_native_working(f.id).is_err());
    assert!(j.seal_working(f.id).is_err());
    assert_eq!(count(&j), 0);
    assert!(j.working_file(f.id).unwrap().dirty);
}
#[test]
fn malformed_native_bytes_stay_recoverable_without_parser_or_writer_on_reopen() {
    let t = temp();
    let data = bytes(b"original contents");
    let mut j = journal(&t);
    let f = publish(&mut j, binding(&t, &data), &data);
    edit(&mut j, f.id, b"partial zip bytes");
    assert!(
        j.capture_native_working(f.id)
            .unwrap()
            .capture(&CancellationToken::new())
            .is_err()
    );
    assert_eq!(count(&j), 0);
    // Simulate the already-existing journal unlinked recovery state; no cloud
    // unlink route or namespace action is added by this component.
    j.db.execute(
        "UPDATE working_files SET body=json_set(body,'$.unlinked',json('true')) WHERE id=?1",
        [f.id.to_string()],
    )
    .unwrap();
    drop(j);
    let ro = RecoveryJournal::open(&t.path().join("journal"), "native-working").unwrap();
    let rows = ro.working_recovery_list(None, 20).unwrap().0;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].unlinked);
    let target = t.path().join("retained.bin");
    let prepared = ro
        .working_export_source(f.id, rows[0].generation)
        .unwrap()
        .prepare_copy(&target, &CancellationToken::new(), |_| {})
        .unwrap();
    ro.verify_working_export(prepared)
        .unwrap()
        .publish(&CancellationToken::new())
        .unwrap();
    assert_eq!(std::fs::read(target).unwrap(), b"partial zip bytes");
}
#[test]
fn schema17_migration_preserves_ordinary_rows_and_native_quota_refusal_is_local() {
    let t = temp();
    let mut j = journal(&t);
    let data = bytes(b"original contents");
    let b = binding(&t, &data);
    let ordinary = j
        .enqueue(
            b.scope.clone(),
            UploadIntent::Create {
                parent: "ordinary-parent".into(),
                name: "ordinary.txt".into(),
            },
            &b"ordinary retained"[..],
        )
        .unwrap();
    j.db.execute_batch("DROP TABLE native_working_bindings; PRAGMA user_version=17;")
        .unwrap();
    drop(j);
    let mut j = journal(&t);
    assert_eq!(
        j.db.pragma_query_value::<u32, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        18
    );
    assert_eq!(j.get(ordinary.id).unwrap().sha256, ordinary.sha256);
    let f = publish(&mut j, b, &data);
    edit(&mut j, f.id, &data);
    j.quota = j.retained_bytes().unwrap();
    assert!(matches!(
        j.capture_native_working(f.id),
        Err(JournalError::Quota)
    ));
    assert!(j.working_file(f.id).unwrap().dirty);
    assert_eq!(count(&j), 1);
}

#[test]
fn write_during_private_copy_cannot_authorize_an_old_generation() {
    let t = temp();
    let data = bytes(b"original contents");
    let mut j = journal(&t);
    let f = publish(&mut j, binding(&t, &data), &data);
    edit(&mut j, f.id, &bytes(b"edit A"));
    let capture = j.capture_native_working(f.id).unwrap();
    let newer = bytes(b"edit B");
    let mut ran = false;
    let copied = capture
        .capture_observed(&CancellationToken::new(), |_| {
            if !ran {
                edit(&mut j, f.id, &newer);
                ran = true;
            }
        })
        .unwrap();
    assert!(ran);
    assert!(matches!(
        j.seal_captured_native_working(copied, &CancellationToken::new()),
        Err(JournalError::Stale)
    ));
    assert_eq!(count(&j), 0);
    assert_eq!(j.read_working(f.id, 0, 4096).unwrap(), newer);
    assert!(j.working_file(f.id).unwrap().dirty);
}

#[test]
fn generation_fence_is_required_even_when_all_visible_metadata_matches() {
    let t = temp();
    let data = bytes(b"original contents");
    let mut j = journal(&t);
    let f = publish(&mut j, binding(&t, &data), &data);
    edit(&mut j, f.id, &data);
    let copied = j
        .capture_native_working(f.id)
        .unwrap()
        .capture(&CancellationToken::new())
        .unwrap();
    j.write_working(f.id, 0, &data).unwrap();
    // Model two writes in the same timestamp tick explicitly, independent of
    // test scheduler delays. The byte generation is the sole changed field.
    let mut later = j.working_file(f.id).unwrap();
    later.node.modified_unix = copied.record.node.modified_unix;
    j.save_working(&later).unwrap();
    assert_eq!(later.node, copied.record.node);
    assert!(matches!(
        j.seal_captured_native_working(copied, &CancellationToken::new()),
        Err(JournalError::Stale)
    ));
    assert_eq!(count(&j), 0);
    assert!(j.working_file(f.id).unwrap().dirty);
}
#[test]
fn changed_durable_binding_cannot_reuse_validated_capture() {
    let t = temp();
    let data = bytes(b"original contents");
    let mut j = journal(&t);
    let f = publish(&mut j, binding(&t, &data), &data);
    edit(&mut j, f.id, &data);
    let copied = j
        .capture_native_working(f.id)
        .unwrap()
        .capture(&CancellationToken::new())
        .unwrap();
    let mut changed = j.native_binding(f.id).unwrap();
    changed.semantic.sha256 = "1".repeat(64);
    j.db.execute(
        "UPDATE native_working_bindings SET body=?2 WHERE working=?1",
        params![f.id.to_string(), serde_json::to_string(&changed).unwrap()],
    )
    .unwrap();
    assert!(matches!(
        j.seal_captured_native_working(copied, &CancellationToken::new()),
        Err(JournalError::Stale)
    ));
    assert_eq!(count(&j), 0);
    assert!(j.working_file(f.id).unwrap().dirty);
}

#[path = "recovery_tests.rs"]
mod recovery_tests;

#[test]
fn stored_native_working_semantic_version_survives_hydration_seal_and_restart() {
    use sha2::Digest;
    for version in [1, 2] {
        let t = temp();
        let original = bytes(b"original own fixture");
        let mut selected = binding(&t, &original);
        let path = t.path().join("version-proof.zip");
        std::fs::write(&path, &original).unwrap();
        selected.semantic = cirrove_icloud::package_archive_semantic_identity_versioned(
            &File::open(path).unwrap(),
            &cirrove_icloud::PackageDownload {
                size: original.len() as u64,
                sha256: hex::encode(sha2::Sha256::digest(&original)),
            },
            &selected.archive.name,
            version,
            &CancellationToken::new(),
        )
        .unwrap();
        let mut j = journal(&t);
        let working = publish(&mut j, selected.clone(), &original);
        edit(&mut j, working.id, &bytes(b"edited own fixture"));
        let captured = j
            .capture_native_working(working.id)
            .unwrap()
            .capture(&CancellationToken::new())
            .unwrap();
        let row = j
            .seal_captured_native_working(captured, &CancellationToken::new())
            .unwrap();
        let UploadRepresentation::PackageReplacementArchive {
            semantic,
            original_semantic,
            ..
        } = &row.representation
        else {
            panic!("replacement")
        };
        assert_eq!(semantic.version, version);
        assert_eq!(original_semantic, &selected.semantic);
        drop(j);
        let ro = RecoveryJournal::open(&t.path().join("journal"), "native-working").unwrap();
        assert!(ro.list(0, 10).unwrap()[0].representation == row.representation);
    }
}
