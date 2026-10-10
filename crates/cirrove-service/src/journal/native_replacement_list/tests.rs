#![allow(clippy::unwrap_used)]
use super::*;
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
fn node(id: &str) -> Node {
    Node {
        id: format!("FILE::com.apple.CloudDocs::{id}"),
        parent_id: Some("FOLDER::com.apple.CloudDocs::parent".into()),
        name: "Owned.pages".into(),
        kind: NodeKind::Folder,
        size: 17,
        modified_unix: 0,
        etag: Some("v1".into()),
        content_version: None,
        target: None,
        package: true,
    }
}
// Persist exact historical row fixtures; these tests exercise discovery, not
// admission or remote evidence production (covered by actual handoff tests).
fn append(
    j: &mut UploadJournal,
    index: usize,
    native: bool,
    state: UploadState,
    escaped: bool,
) -> UploadRecord {
    let mut r = j
        .enqueue(
            scope(),
            UploadIntent::Create {
                parent: "root".into(),
                name: format!("fixture-{index}"),
            },
            b"sealed-local-bytes".as_slice(),
        )
        .unwrap();
    if native {
        let mut original = node(&format!("original-{index}"));
        if escaped {
            original.id = format!("FILE::com.apple.CloudDocs::{}-{index}", "\"".repeat(4000));
            original.parent_id = Some(format!(
                "FOLDER::com.apple.CloudDocs::{}",
                "\"".repeat(4000)
            ));
            original.etag = Some("\"".repeat(4096));
        }
        r.intent = UploadIntent::Replace {
            item: original.id.clone(),
            expected_etag: original.etag.clone().unwrap(),
        };
        r.representation = UploadRepresentation::PackageReplacementArchive {
            expected_root: "Source.pages".into(),
            semantic: semantic(),
            original: Box::new(original.clone()),
            original_semantic: semantic(),
        };
        if state == UploadState::Uploaded {
            let mut current = original.clone();
            current.id = format!(
                "FILE::com.apple.CloudDocs::{}-{index}",
                if escaped {
                    "\\".repeat(4000)
                } else {
                    "new".into()
                }
            );
            let mut backup = original.clone();
            backup.parent_id = Some("FOLDER::com.apple.CloudDocs::TRASH_ROOT".into());
            r.remote = Some(current);
            r.package_completion = Some(semantic());
            r.identity_handoff=Some(serde_json::from_value(serde_json::json!({"recovery_object":Uuid::new_v4(),"old_item":original.id,"recovery_name":"recovery.pages","trash_parent":"FOLDER::com.apple.CloudDocs::TRASH_ROOT","backup":backup})).unwrap());
        }
    }
    r.state = state;
    save(j, &r);
    r
}
fn save(j: &UploadJournal, r: &UploadRecord) {
    j.db.execute(
        "UPDATE uploads SET state=?2,body=?3 WHERE id=?1",
        params![
            r.id.to_string(),
            serde_json::to_value(r.state).unwrap().as_str().unwrap(),
            serde_json::to_string(r).unwrap()
        ],
    )
    .unwrap();
}
#[test]
fn native_replacement_list_is_scoped_typed_and_readonly_across_states() {
    let t = temp();
    let mut j = UploadJournal::open(t.path(), "owned", 1024 * 1024).unwrap();
    append(&mut j, 0, false, UploadState::Pending, false);
    let mut ids = vec![];
    for (i, state) in [
        UploadState::Pending,
        UploadState::VerifyRequired,
        UploadState::Conflict,
        UploadState::Failed,
        UploadState::Uploaded,
    ]
    .into_iter()
    .enumerate()
    {
        ids.push(append(&mut j, i + 1, true, state, false).id)
    }
    let mut foreign = append(&mut j, 9, true, UploadState::Pending, false);
    foreign.scope.collection = "other".into();
    save(&j, &foreign);
    let mut foreign_account = append(&mut j, 10, true, UploadState::Pending, false);
    foreign_account.scope.account = "other".into();
    save(&j, &foreign_account);
    let plan:String=j.db.query_row("EXPLAIN QUERY PLAN SELECT body FROM uploads INDEXED BY native_package_replacement_operations_v21 WHERE sequence>0 AND json_extract(body,'$.representation.kind') IN ('package_replacement_archive','flat_numbers_replacement_archive') ORDER BY sequence LIMIT 3",[],|r|r.get(3)).unwrap();
    assert!(plan.contains("native_package_replacement_operations_v21"));
    let version: u32 =
        j.db.pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
    assert_eq!(version, JOURNAL_SCHEMA);
    drop(j);
    let db = std::fs::read(t.path().join("uploads.db")).unwrap();
    let recovery = RecoveryJournal::open(t.path(), "owned").unwrap();
    let page = recovery
        .native_replacement_list(&scope(), None, 100)
        .unwrap();
    assert_eq!(
        page.operations
            .iter()
            .map(|x| x.operation)
            .collect::<Vec<_>>(),
        ids
    );
    for r in &page.operations[..4] {
        assert!(!r.handoff_receipt_recorded);
        assert!(r.current.is_none() && r.recovery.is_none())
    }
    let last = page.operations.last().unwrap();
    assert!(last.handoff_receipt_recorded);
    assert_eq!(last.original.item, last.recovery.as_ref().unwrap().item);
    assert_ne!(last.original.item, last.current.as_ref().unwrap().item);
    let mut wrong = scope();
    wrong.account = "other".into();
    assert!(recovery.native_replacement_list(&wrong, None, 1).is_err());
    wrong = scope();
    wrong.provider = "onedrive".into();
    assert!(recovery.native_replacement_list(&wrong, None, 1).is_err());
    for limit in [0, 101] {
        assert!(
            recovery
                .native_replacement_list(&scope(), None, limit)
                .is_err()
        )
    }
    assert!(
        recovery
            .native_replacement_list(&scope(), Some(u64::MAX), 1)
            .is_err()
    );
    drop(recovery);
    assert_eq!(std::fs::read(t.path().join("uploads.db")).unwrap(), db);
}
#[test]
fn native_replacement_list_legacy_empty_pages_and_byte_overflow_never_skip_rows() {
    for indexed in [true, false] {
        let t = temp();
        let mut j = UploadJournal::open(t.path(), "owned", 1024 * 1024).unwrap();
        append(&mut j, 0, false, UploadState::Pending, false);
        let ids = (1..31)
            .map(|i| append(&mut j, i, true, UploadState::Uploaded, true).id)
            .collect::<Vec<_>>();
        if !indexed {
            j.db.execute_batch("DROP INDEX native_package_replacement_operations_v21; DROP INDEX native_package_replacement_operations_flat_pages_v21;")
                .unwrap();
        }
        drop(j);
        let before = std::fs::read(t.path().join("uploads.db")).unwrap();
        let ro = RecoveryJournal::open(t.path(), "owned").unwrap();
        if !indexed {
            let first = ro.native_replacement_list(&scope(), None, 1).unwrap();
            assert!(first.operations.is_empty());
            assert!(first.next.is_some());
        }
        let mut after = None;
        let mut found = vec![];
        let mut pages = 0;
        loop {
            let p = ro.native_replacement_list(&scope(), after, 100).unwrap();
            assert!(serde_json::to_vec(&p).unwrap().len() <= 512 * 1024);
            found.extend(p.operations.iter().map(|v| v.operation));
            pages += 1;
            if p.next.is_none() {
                break;
            }
            assert!(p.next > after);
            after = p.next;
            assert!(pages < 40);
        }
        assert!(pages > 1);
        assert_eq!(found, ids);
        drop(ro);
        assert_eq!(std::fs::read(t.path().join("uploads.db")).unwrap(), before);
    }
}
#[test]
fn native_replacement_list_refuses_malformed_receipts_and_row_bindings() {
    for corrupt in [
        "semantic",
        "backup",
        "name",
        "sequence",
        "state",
        "body_limit",
        "intent",
        "old_item",
        "trash_parent",
    ] {
        let t = temp();
        let mut j = UploadJournal::open(t.path(), "owned", 1024 * 1024).unwrap();
        let mut row = append(&mut j, 1, true, UploadState::Uploaded, false);
        match corrupt {
            "semantic" => row.package_completion.as_mut().unwrap().sha256 = "b".repeat(64),
            "backup" => {
                row.identity_handoff
                    .as_mut()
                    .unwrap()
                    .backup
                    .as_mut()
                    .unwrap()
                    .id = "FILE::com.apple.CloudDocs::foreign".into()
            }
            "name" => row.remote.as_mut().unwrap().name = "Other.pages".into(),
            "sequence" => row.sequence += 1,
            "state" => {}
            "old_item" | "trash_parent" => {
                let mut reservation =
                    serde_json::to_value(row.identity_handoff.as_ref().unwrap()).unwrap();
                reservation[corrupt] = serde_json::json!("wrong");
                row.identity_handoff = Some(serde_json::from_value(reservation).unwrap());
            }
            "intent" => {
                row.intent = UploadIntent::Create {
                    parent: "root".into(),
                    name: "Wrong.pages".into(),
                }
            }
            _ => row.sha256 = "a".repeat(RECORD_BYTES as usize),
        }
        save(&j, &row);
        if corrupt == "state" {
            j.db.execute(
                "UPDATE uploads SET state='pending' WHERE id=?1",
                [row.id.to_string()],
            )
            .unwrap();
        }
        assert!(
            j.native_replacement_list(&scope(), None, 100).is_err(),
            "{corrupt}"
        );
    }
}

#[test]
fn flat_pages_replacement_receipt_discovery_is_typed_and_readonly_with_legacy_index() {
    for legacy in [false, true] {
        let path = temp().keep();
        let mut j = UploadJournal::open(&path, "owned", 1024 * 1024).unwrap();
        let mut row = append(&mut j, 1, true, UploadState::Uploaded, false);
        let UploadRepresentation::PackageReplacementArchive { original, .. } = &row.representation
        else {
            panic!("native fixture");
        };
        let mut proof = semantic();
        proof.version = 2;
        proof.entries = 2;
        row.representation = UploadRepresentation::FlatPagesReplacementArchive {
            semantic: proof.clone(),
            original: original.clone(),
            original_semantic: proof.clone(),
        };
        row.package_completion = Some(proof);
        j.db.execute(
            "DELETE FROM package_metadata_publication WHERE operation=?1",
            [row.id.to_string()],
        )
        .unwrap();
        save(&j, &row);
        let pending: bool = j.db.query_row("SELECT EXISTS(SELECT 1 FROM package_metadata_publication WHERE operation=?1 AND done=0)", [row.id.to_string()], |r|r.get(0)).unwrap();
        assert!(pending);
        if legacy {
            j.db.execute_batch("DROP INDEX native_package_replacement_operations_flat_pages_v21")
                .unwrap();
        }
        drop(j);
        let before = std::fs::read(path.join("uploads.db")).unwrap();
        let ro = RecoveryJournal::open(&path, "owned").unwrap();
        let page = ro.native_replacement_list(&scope(), None, 100).unwrap();
        assert_eq!(page.operations.len(), 1);
        let actual = &page.operations[0];
        assert_eq!(actual.operation, row.id);
        assert!(actual.handoff_receipt_recorded);
        assert_eq!(actual.original.item, actual.recovery.as_ref().unwrap().item);
        assert_ne!(actual.original.item, actual.current.as_ref().unwrap().item);
        drop(ro);
        assert_eq!(std::fs::read(path.join("uploads.db")).unwrap(), before);
    }
}
