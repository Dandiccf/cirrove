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
