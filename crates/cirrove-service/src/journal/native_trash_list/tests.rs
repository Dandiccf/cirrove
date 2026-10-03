#![allow(clippy::unwrap_used)]
use super::*;
use cirrove_core::mutation::MutationRequest;
use std::os::unix::fs::PermissionsExt;
fn scope() -> Scope {
    Scope {
        account: "owned".into(),
        provider: "icloud".into(),
        collection: "drive".into(),
    }
}
fn request(index: u32, native: bool) -> MutationRequest {
    let before = Node {
        id: format!("item-{index}"),
        parent_id: Some("root".into()),
        name: format!("Owned-{index}.pages"),
        kind: if native {
            NodeKind::Folder
        } else {
            NodeKind::File
        },
        package: native,
        size: 0,
        modified_unix: 0,
        etag: Some("E1".into()),
        content_version: None,
        target: None,
    };
    MutationRequest {
        scope: scope(),
        intent: if native {
            MutationIntent::TrashNativeDocument { before }
        } else {
            MutationIntent::RemoveFile { before }
        },
    }
}
#[test]
fn native_trash_list_retains_pending_and_recorded_completion_across_readonly_restart() {
    let temp = tempfile::tempdir_in("/var/tmp").unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut journal = UploadJournal::open(temp.path(), "owned", 1024 * 1024).unwrap();
    let first = journal.enqueue_mutation(request(1, true)).unwrap();
    let claimed = journal.claim_mutation().unwrap().unwrap();
    journal
        .acknowledge_mutation(
            first.id,
            claimed.attempt.unwrap(),
            MutationReceipt::Removed {
                item: "item-1".into(),
            },
        )
        .unwrap();
    let applied = journal.mutation(first.id).unwrap();
    journal
        .finish_native_trash_publication(&applied, PackagePublicationStatus::Absent, 0)
        .unwrap();
    journal.enqueue_mutation(request(2, false)).unwrap();
    let last = journal.enqueue_mutation(request(3, true)).unwrap();
    let plan:String=journal.db.query_row("EXPLAIN QUERY PLAN SELECT body FROM mutations INDEXED BY native_trash_operations WHERE sequence>0 AND json_extract(body,'$.request.intent.kind')='trash_native_document' ORDER BY sequence LIMIT 2",[],|r|r.get(3)).unwrap();
    assert!(plan.contains("native_trash_operations"));
    drop(journal);
    let recovery = RecoveryJournal::open(temp.path(), "owned").unwrap();
    let firstpage = recovery.native_trash_list(&scope(), None, 1).unwrap();
    assert_eq!(firstpage.operations.len(), 1);
    assert_eq!(firstpage.operations[0].operation, first.id);
    assert!(
        firstpage.operations[0].removal_receipt_recorded
            && firstpage.operations[0].metadata_absence_recorded
    );
    let lastpage = recovery
        .native_trash_list(&scope(), firstpage.next, 1)
        .unwrap();
    assert_eq!(lastpage.operations.len(), 1);
    assert_eq!(lastpage.operations[0].operation, last.id);
    assert_eq!(lastpage.operations[0].state, MutationState::Pending);
    assert!(
        !lastpage.operations[0].removal_receipt_recorded
            && !lastpage.operations[0].metadata_absence_recorded
    );
    assert!(lastpage.next.is_none());
    assert!(
        recovery
            .native_trash_list(&scope(), Some(u64::MAX), 1)
            .is_err()
    );
    assert!(recovery.native_trash_list(&scope(), None, 101).is_err());
    let mut foreign = scope();
    foreign.account = "other".into();
    assert!(recovery.native_trash_list(&foreign, None, 1).is_err());
    drop(recovery);
    let connection = Connection::open_with_flags(
        temp.path().join("uploads.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(
        connection
            .query_row::<i64, _, _>("SELECT count(*) FROM mutations", [], |r| r.get(0))
            .unwrap(),
        3
    );
    let pending: String = connection
        .query_row(
            "SELECT state FROM mutations WHERE id=?1",
            [last.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pending, "pending");
}
#[test]
fn native_trash_list_old_readonly_index_fallback_advances_empty_bounded_pages() {
    let temp = tempfile::tempdir_in("/var/tmp").unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut journal = UploadJournal::open(temp.path(), "owned", 1024 * 1024).unwrap();
    let ordinary = journal.enqueue_mutation(request(1, false)).unwrap();
    let native = journal.enqueue_mutation(request(2, true)).unwrap();
    journal
        .db
        .execute("DROP INDEX native_trash_operations", [])
        .unwrap();
    drop(journal);
    let recovery = RecoveryJournal::open(temp.path(), "owned").unwrap();
    let empty = recovery.native_trash_list(&scope(), None, 1).unwrap();
    assert!(empty.operations.is_empty());
    assert_eq!(empty.next, Some(ordinary.sequence));
    let page = recovery.native_trash_list(&scope(), empty.next, 1).unwrap();
    assert_eq!(page.operations[0].operation, native.id);
    assert!(page.next.is_none());
    drop(recovery);
    let connection = Connection::open_with_flags(
        temp.path().join("uploads.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(
        connection
            .query_row::<i64, _, _>(
                "SELECT count(*) FROM sqlite_master WHERE name='native_trash_operations'",
                [],
                |r| r.get(0)
            )
            .unwrap(),
        0
    );
}

#[test]
fn native_trash_list_escaped_metadata_respects_wire_budget_without_skipping_rows() {
    let temp = tempfile::tempdir_in("/var/tmp").unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut journal = UploadJournal::open(temp.path(), "owned", 1024 * 1024).unwrap();
    let mut expected = Vec::new();
    for i in 0..80 {
        let mut value = request(i, true);
        let MutationIntent::TrashNativeDocument { before } = &mut value.intent else {
            unreachable!()
        };
        before.id = format!("item-{i}-{}", "\\".repeat(4080));
        before.etag = Some("\\\"".repeat(2048));
        expected.push(journal.enqueue_mutation(value).unwrap().id);
    }
    for indexed in [true, false] {
        if !indexed {
            journal
                .db
                .execute("DROP INDEX native_trash_operations", [])
                .unwrap();
        }
        let mut after = None;
        let mut found = Vec::new();
        let mut pages = 0;
        loop {
            let page = journal.native_trash_list(&scope(), after, 100).unwrap();
            assert!(serde_json::to_vec(&page).unwrap().len() < 513 * 1024);
            assert!(!page.operations.is_empty());
            found.extend(page.operations.iter().map(|r| r.operation));
            pages += 1;
            match page.next {
                Some(next) => {
                    assert!(next > after.unwrap_or(0));
                    after = Some(next);
                }
                None => break,
            }
            assert!(pages <= 80);
        }
        assert!(pages > 1);
        assert_eq!(found, expected);
    }
}
