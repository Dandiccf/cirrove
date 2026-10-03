#![allow(clippy::unwrap_used)]
use super::*;
use std::os::unix::fs::PermissionsExt;
fn scope() -> Scope {
    Scope {
        account: "owned".into(),
        provider: "icloud".into(),
        collection: "drive".into(),
    }
}
fn temp() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/var/tmp")
        .unwrap()
}
fn semantic() -> PackageSemanticIdentity {
    PackageSemanticIdentity {
        version: 1,
        sha256: "a".repeat(64),
        entries: 1,
        files: 1,
        expanded_bytes: 3,
    }
}
fn save(j: &UploadJournal, row: &UploadRecord) {
    j.db.execute(
        "UPDATE uploads SET state=?2,body=?3 WHERE id=?1",
        params![
            row.id.to_string(),
            serde_json::to_value(row.state).unwrap().as_str().unwrap(),
            serde_json::to_string(row).unwrap()
        ],
    )
    .unwrap();
}
// These are retained-row fixtures, not evidence of admission or Apple upload.
fn append(
    j: &mut UploadJournal,
    index: usize,
    native: bool,
    state: UploadState,
    escaped: bool,
) -> UploadRecord {
    let parent = format!(
        "FOLDER::com.apple.CloudDocs::{}",
        if escaped {
            "\"".repeat(4000)
        } else {
            "root".into()
        }
    );
    let name = format!("Owned-{index}.pages");
    let mut row = if native {
        j.enqueue_package_archive(
            scope(),
            UploadIntent::Create {
                parent: parent.clone(),
                name: name.clone(),
            },
            "Source.pages".into(),
            semantic(),
            b"sealed-local-archive".as_slice(),
        )
        .unwrap()
    } else {
        j.enqueue(
            scope(),
            UploadIntent::Create {
                parent: parent.clone(),
                name: format!("ordinary-{index}"),
            },
            b"ordinary-local-bytes".as_slice(),
        )
        .unwrap()
    };
    row.state = state;
    row.session_key = Some(Uuid::new_v4());
    if native && state == UploadState::Uploaded {
        row.remote = Some(Node {
            id: format!(
                "FILE::com.apple.CloudDocs::{}-{index}",
                if escaped {
                    "\\".repeat(4000)
                } else {
                    "imported".into()
                }
            ),
            parent_id: Some(parent),
            name,
            kind: NodeKind::Folder,
            size: 3,
            modified_unix: 1,
            etag: Some("E1".into()),
            content_version: None,
            target: None,
            package: true,
        });
        row.package_completion = Some(semantic());
    }
    save(j, &row);
    row
}
#[test]
fn native_import_list_is_scoped_historical_and_unchanged_under_readonly_recovery() {
    let t = temp();
    let mut j = UploadJournal::open(t.path(), "owned", 1024 * 1024).unwrap();
    append(&mut j, 0, false, UploadState::Pending, false);
    let mut replacement = append(&mut j, 90, false, UploadState::Pending, false);
    let original = Node {
        id: "FILE::com.apple.CloudDocs::original".into(),
        parent_id: Some("FOLDER::com.apple.CloudDocs::root".into()),
        name: "Original.pages".into(),
        kind: NodeKind::Folder,
        size: 3,
        modified_unix: 0,
        etag: Some("E1".into()),
        content_version: None,
        target: None,
        package: true,
    };
    replacement.intent = UploadIntent::Replace {
        item: original.id.clone(),
        expected_etag: "E1".into(),
    };
    replacement.representation = UploadRepresentation::PackageReplacementArchive {
        expected_root: "Source.pages".into(),
        semantic: semantic(),
        original: Box::new(original),
        original_semantic: semantic(),
    };
    assert!(replacement.representation.validate().is_ok());
    save(&j, &replacement);
    let states = [
        UploadState::Pending,
        UploadState::VerifyRequired,
        UploadState::Conflict,
        UploadState::Failed,
        UploadState::Resolved,
        UploadState::Uploaded,
    ];
    let ids: Vec<_> = states
        .into_iter()
        .enumerate()
        .map(|(i, s)| append(&mut j, i + 1, true, s, false).id)
        .collect();
    for (i, field) in ["account", "provider", "collection"]
        .into_iter()
        .enumerate()
    {
        let mut row = append(&mut j, i + 20, true, UploadState::Pending, false);
        match field {
            "account" => row.scope.account = "other".into(),
            "provider" => row.scope.provider = "onedrive".into(),
            _ => row.scope.collection = "other".into(),
        }
        save(&j, &row);
    }
    // Legacy exact ETag alias must remain recognized without rewriting it.
    let mut legacy = append(&mut j, 30, true, UploadState::Uploaded, false);
    legacy.remote.as_mut().unwrap().content_version = Some("E1".into());
    save(&j, &legacy);
    let frozen = serde_json::to_value(j.list(0, 100).unwrap()).unwrap();
    let plan:String=j.db.query_row("EXPLAIN QUERY PLAN SELECT body FROM uploads INDEXED BY native_package_import_operations WHERE sequence>0 AND json_extract(body,'$.representation.kind')='package_archive' ORDER BY sequence LIMIT 3",[],|r|r.get(3)).unwrap();
    assert!(plan.contains("native_package_import_operations"));
    drop(j);
    let before = std::fs::read(t.path().join("uploads.db")).unwrap();
    let ro = RecoveryJournal::open(t.path(), "owned").unwrap();
    let page = ro.native_import_list(&scope(), None, 100).unwrap();
    let mut expected = ids.clone();
    expected.push(legacy.id);
    assert_eq!(
        page.operations
            .iter()
            .map(|r| r.operation)
            .collect::<Vec<_>>(),
        expected
    );
    for row in &page.operations[..5] {
        assert!(!row.completion_receipt_recorded);
        assert!(row.remote_item.is_none());
    }
    for row in &page.operations[5..] {
        assert!(row.completion_receipt_recorded);
        assert!(row.remote_item.is_some());
    }
    let projected = serde_json::to_value(&page).unwrap();
    let keys = projected["operations"][0].as_object().unwrap();
    assert_eq!(keys.len(), 7);
    for field in [
        "archive",
        "source",
        "session_key",
        "checkpoint",
        "representation",
        "sha256",
        "remote",
        "current",
    ] {
        assert!(
            !keys.contains_key(field),
            "private/freshness field {field} leaked"
        );
    }
    assert_eq!(
        serde_json::to_value(ro.list(0, 100).unwrap()).unwrap(),
        frozen
    );
    for field in ["account", "provider", "collection"] {
        let mut wrong = scope();
        match field {
            "account" => wrong.account = "other".into(),
            "provider" => wrong.provider = "onedrive".into(),
            _ => wrong.collection = "other".into(),
        }
        assert!(ro.native_import_list(&wrong, None, 1).is_err(), "{field}");
    }
    for limit in [0, 101] {
        assert!(ro.native_import_list(&scope(), None, limit).is_err());
    }
    assert!(ro.native_import_list(&scope(), Some(u64::MAX), 1).is_err());
    drop(ro);
    assert_eq!(std::fs::read(t.path().join("uploads.db")).unwrap(), before);
}
#[test]
fn native_import_list_legacy_pages_and_escaped_byte_overflow_do_not_skip_operations() {
    for indexed in [true, false] {
        let t = temp();
        let mut j = UploadJournal::open(t.path(), "owned", 1024 * 1024).unwrap();
        append(&mut j, 0, false, UploadState::Pending, false);
        let ids: Vec<_> = (1..51)
            .map(|i| append(&mut j, i, true, UploadState::Uploaded, true).id)
            .collect();
        if !indexed {
            j.db.execute_batch("DROP INDEX native_package_import_operations")
                .unwrap();
        }
        drop(j);
        let before = std::fs::read(t.path().join("uploads.db")).unwrap();
        let ro = RecoveryJournal::open(t.path(), "owned").unwrap();
        if !indexed {
            let first = ro.native_import_list(&scope(), None, 1).unwrap();
            assert!(first.operations.is_empty());
            assert!(first.next.is_some());
        }
        let mut after = None;
        let mut found = vec![];
        let mut pages = 0;
        loop {
            let page = ro.native_import_list(&scope(), after, 100).unwrap();
            assert!(serde_json::to_vec(&page).unwrap().len() <= 512 * 1024);
            found.extend(page.operations.into_iter().map(|r| r.operation));
            pages += 1;
            let Some(next) = page.next else { break };
            assert!(after.is_none_or(|old| next > old));
            after = Some(next);
            assert!(pages < 60);
        }
        assert!(pages > 1);
        assert_eq!(found, ids);
        drop(ro);
        assert_eq!(std::fs::read(t.path().join("uploads.db")).unwrap(), before);
    }
}
#[test]
fn native_import_list_refuses_nonimport_owners_malformed_receipts_and_row_bindings() {
    for arm in [
        "semantic",
        "remote-parent",
        "remote-name",
        "remote-id",
        "remote-kind",
        "remote-package",
        "remote-tag",
        "remote-content",
        "root-format",
        "name-format",
        "intent",
        "working",
        "handoff",
        "base",
        "sequence",
        "column-id",
        "column-state",
        "body-limit",
        "pending-receipt",
    ] {
        let t = temp();
        let mut j = UploadJournal::open(t.path(), "owned", 1024 * 1024).unwrap();
        let mut row = append(&mut j, 1, true, UploadState::Uploaded, false);
        match arm {
            "semantic"=>row.package_completion.as_mut().unwrap().sha256="b".repeat(64),
            "remote-parent"=>row.remote.as_mut().unwrap().parent_id=Some("FOLDER::com.apple.CloudDocs::other".into()),
            "remote-name"=>row.remote.as_mut().unwrap().name="Other.pages".into(),
            "remote-id"=>row.remote.as_mut().unwrap().id="other".into(),
            "remote-kind"=>row.remote.as_mut().unwrap().kind=NodeKind::File,
            "remote-package"=>row.remote.as_mut().unwrap().package=false,
            "remote-tag"=>row.remote.as_mut().unwrap().etag=None,
            "remote-content"=>row.remote.as_mut().unwrap().content_version=Some("other".into()),
            "root-format"=>{let UploadRepresentation::PackageArchive {expected_root,..}=&mut row.representation else {unreachable!()};*expected_root="Source.docx".into();},
            "name-format"=>{let UploadIntent::Create {name,..}=&mut row.intent else {unreachable!()};*name="Other.numbers".into();},
            "intent"=>row.intent=UploadIntent::Replace {item:"other".into(),expected_etag:"E1".into()},
            "working"=>row.working_file=Some(Uuid::new_v4()),
            "handoff"=>row.identity_handoff=Some(serde_json::from_value(serde_json::json!({"recovery_object":Uuid::new_v4(),"old_item":"old","recovery_name":"recovery.pages","trash_parent":"FOLDER::com.apple.CloudDocs::TRASH_ROOT","backup":null})).unwrap()),
            "base"=>row.base=Some(UploadBase {predecessor:Uuid::new_v4(),resolved:false}),
            "sequence"=>row.sequence+=1,
            "body-limit"=>row.sha256="a".repeat(RECORD_BYTES as usize),
            "pending-receipt"=>row.state=UploadState::Pending,
            _=>{}
        }
        save(&j, &row);
        if arm == "column-id" {
            j.db.execute(
                "UPDATE uploads SET id=?2 WHERE id=?1",
                params![row.id.to_string(), Uuid::new_v4().to_string()],
            )
            .unwrap();
        }
        if arm == "column-state" {
            j.db.execute(
                "UPDATE uploads SET state='pending' WHERE id=?1",
                [row.id.to_string()],
            )
            .unwrap();
        }
        assert!(j.native_import_list(&scope(), None, 100).is_err(), "{arm}");
    }
}
