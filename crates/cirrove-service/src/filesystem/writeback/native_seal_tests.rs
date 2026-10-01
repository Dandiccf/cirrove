#![allow(clippy::unwrap_used)]
use super::*;
use cirrove_core::{reads::NativeArchiveBinding, upload::UploadRepresentation};
use std::os::unix::fs::PermissionsExt;
fn binding(t: &tempfile::TempDir, data: &[u8]) -> NativeArchiveBinding {
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

pub(super) fn fixture() -> (tempfile::TempDir, Writeback, WorkingFile) {
    let temp = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/var/tmp")
        .unwrap();
    let data = crate::native_import::synthetic_package_archive("Owned.pages/Document", b"original");
    let bound = binding(&temp, &data);
    let mut journal = UploadJournal::open(
        &temp.path().join("journal"),
        "native-working",
        16 * 1024 * 1024,
    )
    .unwrap();
    let mut hydrate = journal.reserve_native_working(bound).unwrap();
    hydrate.write_chunk(&data).unwrap();
    let ready = hydrate.validate(&CancellationToken::new()).unwrap();
    let file = journal.publish_native_working(ready).unwrap();
    let writer = Writeback {
        journal: Arc::new(Mutex::new(journal)),
        refusals: Default::default(),
        wake: Default::default(),
        projection: Default::default(),
        hydrating: Default::default(),
        sealing: Default::default(),
        activity: Default::default(),
        maintenance_cursor: Default::default(),
        preserving_cursor: Default::default(),
        maintenance_retries: Default::default(),
        provider: Default::default(),
    };
    (temp, writer, file)
}
#[tokio::test]
async fn native_seal_dispatch_queues_typed_successor_and_clean_fsync_is_idempotent() {
    let (_temp, writer, file) = fixture();
    writer.refresh_projection().await.unwrap();
    for data in [b"first".as_slice(), b"second".as_slice()] {
        let archive = crate::native_import::synthetic_package_archive("Owned.pages/Document", data);
        writer.truncate(file.id, 0).await.unwrap();
        writer.write(file.id, 0, archive, false).await.unwrap();
        writer.seal(file.id).await.unwrap();
        assert!(
            !writer
                .working(&file.scope, &file.node.id)
                .unwrap()
                .unwrap()
                .dirty
        );
        writer.seal(file.id).await.unwrap();
    }
    let journal = writer.journal.lock().unwrap();
    let records = journal.list(0, 10).unwrap();
    assert_eq!(records.len(), 2);
    assert!(records.iter().all(|r| matches!(
        &r.representation,
        UploadRepresentation::PackageReplacementArchive { .. }
    )));
    let newer = records.iter().max_by_key(|r| r.sequence).unwrap();
    let older = records.iter().min_by_key(|r| r.sequence).unwrap();
    assert_eq!(newer.base.as_ref().unwrap().predecessor, older.id);
    assert!(!newer.base.as_ref().unwrap().resolved);
}
#[tokio::test]
async fn native_seal_all_retains_invalid_native_bytes_but_seals_ordinary_file() {
    let (_temp, writer, file) = fixture();
    writer.refresh_projection().await.unwrap();
    writer.truncate(file.id, 0).await.unwrap();
    writer
        .write(file.id, 0, b"unfinished invalid zip".to_vec(), false)
        .await
        .unwrap();
    let ordinary = writer
        .create(
            file.scope.clone(),
            Node {
                id: "".into(),
                parent_id: Some("root".into()),
                name: "ordinary.txt".into(),
                kind: NodeKind::File,
                size: 0,
                modified_unix: 0,
                etag: None,
                content_version: None,
                target: None,
                package: false,
            },
        )
        .await
        .unwrap();
    writer
        .write(ordinary.id, 0, b"ordinary content".to_vec(), false)
        .await
        .unwrap();
    assert!(writer.seal_all().await.is_err());
    assert_eq!(
        writer.read(file.id, 0, 1024).await.unwrap(),
        b"unfinished invalid zip"
    );
    let journal = writer.journal.lock().unwrap();
    assert!(journal.working_file(file.id).unwrap().dirty);
    assert!(!journal.working_file(ordinary.id).unwrap().dirty);
    let records = journal.list(0, 10).unwrap();
    assert_eq!(records.len(), 1);
    assert!(records[0].representation.is_file_bytes());
}
