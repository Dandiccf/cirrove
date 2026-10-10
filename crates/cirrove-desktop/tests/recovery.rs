#![allow(clippy::unwrap_used)]
use cirrove_desktop::recovery::{confirmed, eligible};
use cirrove_service::{jobs::Job, recent::LocalChange};
use std::path::Path;

fn save() -> LocalChange {
    serde_json::from_value(serde_json::json!({"operation":"00000000-0000-4000-8000-000000000001", "sequence":7, "name":"a\n'b.txt", "state":"conflict", "size":3})).unwrap()
}
#[test]
fn only_identified_unresolved_sealed_generations_are_offered() {
    let mut save = save();
    for state in [
        "pending",
        "uploading",
        "verify_required",
        "verifyrequired",
        "verifying",
        "conflict",
        "failed",
    ] {
        save.state = state.into();
        assert!(eligible(&save));
    }
    for state in ["preparing", "uploaded", "discarded", "resolved", "unknown"] {
        save.state = state.into();
        assert!(!eligible(&save));
    }
    save.state = "conflict".into();
    save.operation = None;
    assert!(!eligible(&save));
}
#[test]
fn success_requires_an_exact_receipt() {
    let save = save();
    let path = Path::new("/home/test/a\n'b.txt");
    let value = serde_json::json!({"id":"job", "kind":"export_local", "name":"copy", "state":"succeeded", "export":{
        "operation":save.operation, "size":3, "sha256":"a".repeat(64), "destination":path
    }});
    let job: Job = serde_json::from_value(value.clone()).unwrap();
    assert!(confirmed(&job, &save, path));
    for (field, wrong) in [
        (
            "operation",
            serde_json::json!("00000000-0000-4000-8000-000000000002"),
        ),
        ("size", serde_json::json!(4)),
        ("destination", serde_json::json!("/other")),
        ("sha256", serde_json::json!("g".repeat(64))),
    ] {
        let mut bad = value.clone();
        bad["export"][field] = wrong;
        assert!(!confirmed(
            &serde_json::from_value(bad).unwrap(),
            &save,
            path
        ));
    }
    for state in ["running", "stopping", "stopped", "failed", "unknown"] {
        let mut bad = value.clone();
        bad["state"] = state.into();
        assert!(!confirmed(
            &serde_json::from_value(bad).unwrap(),
            &save,
            path
        ));
    }
    let mut no_receipt = job.clone();
    no_receipt.export = None;
    assert!(!confirmed(&no_receipt, &save, path));
    let mut wrong_kind = job;
    wrong_kind.kind = cirrove_service::jobs::JobKind::KeepOffline;
    assert!(!confirmed(&wrong_kind, &save, path));
}

fn working() -> cirrove_service::journal::WorkingRecovery {
    serde_json::from_value(serde_json::json!({"file":"00000000-0000-4000-8000-000000000009",
        "generation":9, "name":"unsaved\n' notes.txt", "size":3, "recorded_size":3, "unlinked":true})).unwrap()
}
#[test]
fn active_pages_keep_saved_versions_when_working_route_is_unavailable() {
    use cirrove_desktop::recovery::{Selection, active};
    let saved = cirrove_service::RecentReply {
        local: vec![save()],
        ..Default::default()
    };
    let page = active::page(active::Cursor::default(), Some(saved), None).unwrap();
    assert!(page.working_unavailable);
    assert!(page.next.is_none());
    assert!(matches!(&page.selections[..], [Selection::Saved(_)]));
}
#[test]
fn active_empty_pages_can_advance_and_do_not_repeat_sealed_history() {
    use cirrove_desktop::recovery::{Selection, active};
    let id = working().file;
    let page = active::page(
        active::Cursor::default(),
        Some(cirrove_service::RecentReply::default()),
        Some(cirrove_service::RecoveryWorkingReply {
            files: vec![],
            next: Some(id),
            refusal: None,
        }),
    )
    .unwrap();
    assert!(page.selections.is_empty());
    let cursor = page.next.unwrap();
    assert!(cursor.saved_done);
    let next = active::page(
        cursor.clone(),
        None,
        Some(cirrove_service::RecoveryWorkingReply {
            files: vec![working()],
            next: None,
            refusal: None,
        }),
    )
    .unwrap();
    assert!(matches!(&next.selections[..], [Selection::Working(_)]));
    assert!(next.next.is_none());
    assert!(
        active::page(
            cursor,
            None,
            Some(cirrove_service::RecoveryWorkingReply {
                files: vec![],
                next: Some(id),
                refusal: None,
            })
        )
        .is_err()
    );
}
#[test]
fn active_working_confirmation_is_bound_to_selected_generation_and_job() {
    use cirrove_desktop::recovery::{Selection, confirmed_selection};
    let working = working();
    let selection = Selection::Working(working.clone());
    let destination = Path::new("/local/copy");
    let initial: Job = serde_json::from_value(
        serde_json::json!({"id":"working-job", "kind":"export_local",
        "name":"copy", "bytes_total":3, "state":"running"}),
    )
    .unwrap();
    let mut completed = initial.clone();
    completed.state = cirrove_service::jobs::JobState::Succeeded;
    completed.working_export = Some(cirrove_service::journal::WorkingExportReceipt {
        source: working.clone(),
        sha256: "a".repeat(64),
        destination: destination.into(),
    });
    assert!(confirmed_selection(
        &initial,
        &completed,
        &selection,
        destination
    ));
    let mut newer = working.clone();
    newer.generation += 1;
    assert!(!confirmed_selection(
        &initial,
        &completed,
        &Selection::Working(newer),
        destination
    ));
    let mut other_job = initial.clone();
    other_job.id = "another".into();
    assert!(!confirmed_selection(
        &other_job,
        &completed,
        &selection,
        destination
    ));
    let mut wrong_size = working;
    wrong_size.size += 1;
    assert!(!confirmed_selection(
        &initial,
        &completed,
        &Selection::Working(wrong_size),
        destination
    ));
    completed.working_export = None;
    assert!(!confirmed_selection(
        &initial,
        &completed,
        &selection,
        destination
    ));
}
#[test]
fn ambiguous_dual_receipts_cannot_confirm_either_source_kind() {
    use cirrove_desktop::recovery::{Selection, confirmed_selection};
    let save = save();
    let path = Path::new("/local/copy");
    let initial: Job = serde_json::from_value(
        serde_json::json!({"id":"job", "name":"copy", "kind":"export_local", "bytes_total":3}),
    )
    .unwrap();
    let mut job = initial.clone();
    job.state = cirrove_service::jobs::JobState::Succeeded;
    job.export = Some(cirrove_service::journal::LocalExportReceipt {
        operation: save.operation.unwrap(),
        size: 3,
        sha256: "a".repeat(64),
        destination: path.into(),
    });
    job.working_export = Some(cirrove_service::journal::WorkingExportReceipt {
        source: working(),
        sha256: "a".repeat(64),
        destination: path.into(),
    });
    assert!(!confirmed(&job, &save, path));
    assert!(!confirmed_selection(
        &initial,
        &job,
        &Selection::Saved(save),
        path
    ));
    assert!(!confirmed_selection(
        &initial,
        &job,
        &Selection::Working(working()),
        path
    ));
}
