//! Ordinary editor temp-file cleanup must wait for its sealed staged successor.
//! All provider callbacks are synthetic; fixture files are deliberately retained.
use super::{LockProbe, Vault, journal};
use async_trait::async_trait;
use cirrove_core::mutation::{
    MutationIntent, MutationProvider, MutationReceipt, MutationReconciliation, MutationRequest,
    Result as MutationResult,
};
use cirrove_core::upload::{
    Reconciliation, RecoveryLocation, Result as UploadResult, UploadIntent, UploadProvider,
    UploadRequest, UploadStep,
};
use cirrove_core::{CancellationToken, Node, NodeKind, Scope};
use cirrove_service::{
    journal::{JournalError, MutationState, UploadJournal, UploadRecord, UploadState},
    mutations::MutationWorker,
    transfers::TransferWorker,
};
use rusqlite::{Connection, params};
use secrecy::SecretString;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::Read,
    path::PathBuf,
    sync::{Arc, Mutex},
};

const CONTENT: &[u8] = b"sealed-editor-document";

fn scope() -> Scope {
    Scope {
        account: "fixture".into(),
        provider: "fixture".into(),
        collection: "drive".into(),
    }
}

/// Separate actual journal fixture for admission/confirmation corruption arms.
/// Its predecessor is created and acknowledged through public journal APIs;
/// only the named hostile control mutates private synthetic SQL afterward.
struct PendingRemove {
    root: PathBuf,
    journal: UploadJournal,
    upload: UploadRecord,
    owner: uuid::Uuid,
    working: uuid::Uuid,
    remove: uuid::Uuid,
}

impl PendingRemove {
    fn new(reserve_before_unlink: bool) -> Self {
        let root = tempfile::Builder::new()
            .prefix("cirrove-unlinked-handoff-control-")
            .tempdir()
            .unwrap()
            .keep()
            .join("journal");
        let mut journal = UploadJournal::open(&root, "fixture", 1024 * 1024).unwrap();
        let working = journal
            .create_working(scope(), old_temp(), true, &b""[..])
            .unwrap();
        journal.seal_working(working.id).unwrap().unwrap();
        let predecessor = journal.claim_next().unwrap().unwrap();
        journal
            .acknowledge(predecessor.id, predecessor.attempt.unwrap(), old_temp())
            .unwrap();
        journal.write_working(working.id, 0, CONTENT).unwrap();
        journal.seal_working(working.id).unwrap().unwrap();
        let upload = journal.claim_next().unwrap().unwrap();
        assert!(upload.base.as_ref().unwrap().resolved);
        let owner = journal.namespace_for_operation(upload.id).unwrap().unwrap();
        if reserve_before_unlink {
            journal
                .reserve_identity_handoff(upload.id, upload.attempt.unwrap(), location(upload.id))
                .unwrap();
        }
        let unlinked = journal
            .unlink_namespace_file(owner.id, owner.revision, false)
            .unwrap();
        assert_eq!(
            unlinked.mutation.base.as_ref().unwrap().predecessor,
            upload.id
        );
        assert_eq!(unlinked.mutation.working_file, Some(working.id));
        Self {
            root,
            journal,
            upload,
            owner: owner.id,
            working: working.id,
            remove: unlinked.mutation.id,
        }
    }

    fn db(&self) -> Connection {
        Connection::open(self.root.join("uploads.db")).unwrap()
    }

    fn edit(&self, table: &str, id: uuid::Uuid, change: impl FnOnce(&mut serde_json::Value)) {
        assert!(matches!(
            table,
            "uploads" | "mutations" | "namespace_objects" | "working_files"
        ));
        let db = self.db();
        let body: String = db
            .query_row(
                &format!("SELECT body FROM {table} WHERE id=?1"),
                [id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        let mut body: serde_json::Value = serde_json::from_str(&body).unwrap();
        change(&mut body);
        assert_eq!(
            db.execute(
                &format!("UPDATE {table} SET body=?2 WHERE id=?1"),
                params![id.to_string(), serde_json::to_string(&body).unwrap()],
            )
            .unwrap(),
            1
        );
    }

    fn snapshot(&self) -> (Vec<Vec<String>>, Vec<u8>, Vec<u8>) {
        let db = self.db();
        let rows = [
            "SELECT json_array(sequence,id,state,body) FROM uploads ORDER BY sequence",
            "SELECT json_array(sequence,id,state,body) FROM mutations ORDER BY sequence",
            "SELECT json_array(id,body) FROM namespace_objects ORDER BY id",
            "SELECT json_array(id,body) FROM working_files ORDER BY id",
            "SELECT json_array(operation,object) FROM namespace_operations ORDER BY operation",
            "SELECT json_array(predecessor,successor) FROM write_successors ORDER BY predecessor",
            "SELECT json_array(sequence,id,complete) FROM write_queue ORDER BY sequence",
        ]
        .into_iter()
        .map(|sql| {
            db.prepare(sql)
                .unwrap()
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        })
        .collect();
        (
            rows,
            payload(&self.journal, self.upload.id),
            std::fs::read(self.root.join("working").join(self.working.to_string())).unwrap(),
        )
    }

    fn corrupt(&self, arm: &str) {
        use serde_json::json;
        let foreign = uuid::Uuid::new_v4();
        match arm {
            "missing_edge" => { self.db().execute("DELETE FROM write_successors WHERE predecessor=?1", [self.upload.id.to_string()]).unwrap(); }
            "wrong_edge" => { self.db().execute("UPDATE write_successors SET successor=?2 WHERE predecessor=?1", params![self.upload.id.to_string(), foreign.to_string()]).unwrap(); }
            "missing_remove" => { self.db().execute("DELETE FROM mutations WHERE id=?1", [self.remove.to_string()]).unwrap(); }
            "wrong_remove_owner" => { self.db().execute("UPDATE namespace_operations SET object=?2 WHERE operation=?1", params![self.remove.to_string(), foreign.to_string()]).unwrap(); }
            "missing_upload_owner" => { self.db().execute("DELETE FROM namespace_operations WHERE operation=?1", [self.upload.id.to_string()]).unwrap(); }
            "remove_scope" => self.edit("mutations", self.remove, |b| b["request"]["scope"]["collection"] = json!("foreign-drive")),
            "owner_scope" => self.edit("namespace_objects", self.owner, |b| b["scope"]["collection"] = json!("foreign-drive")),
            "working_scope" => self.edit("working_files", self.working, |b| b["scope"]["collection"] = json!("foreign-drive")),
            "remove_stream" => self.edit("mutations", self.remove, |b| b["working_file"] = json!(foreign)),
            "owner_stream" => self.edit("namespace_objects", self.owner, |b| b["working_file"] = json!(foreign)),
            "working_latest" => self.edit("working_files", self.working, |b| b["latest"] = json!(foreign)),
            "owner_latest" => self.edit("namespace_objects", self.owner, |b| b["latest"] = json!(foreign)),
            "missing_base" => self.edit("mutations", self.remove, |b| b["base"] = json!(null)),
            "wrong_predecessor" => self.edit("mutations", self.remove, |b| b["base"]["predecessor"] = json!(foreign)),
            "resolved_remove" => self.edit("mutations", self.remove, |b| b["base"]["resolved"] = json!(true)),
            "remove_not_pending" => {
                self.edit("mutations", self.remove, |b| b["state"] = json!("verify_required"));
                self.db().execute("UPDATE mutations SET state='verify_required' WHERE id=?1", [self.remove.to_string()]).unwrap();
            }
            "remove_state_column" => { self.db().execute("UPDATE mutations SET state='verify_required' WHERE id=?1", [self.remove.to_string()]).unwrap(); }
            "remove_sequence_column" => { self.db().execute("UPDATE mutations SET sequence=sequence+10 WHERE id=?1", [self.remove.to_string()]).unwrap(); }
            "remove_queue_complete" => { self.db().execute("UPDATE write_queue SET complete=1 WHERE id=?1", [self.remove.to_string()]).unwrap(); }
            "remove_queue_sequence" => { self.db().execute("UPDATE write_queue SET sequence=sequence+10 WHERE id=?1", [self.remove.to_string()]).unwrap(); }
            "attempt_present" => self.edit("mutations", self.remove, |b| b["attempt"] = json!(foreign)),
            "receipt_present" => self.edit("mutations", self.remove, |b| b["receipt"] = serde_json::to_value(MutationReceipt::Removed { item: current_temp().id }).unwrap()),
            "prepared_present" => self.edit("mutations", self.remove, |b| b["prepared_item"] = json!("unrelated-prepared")),
            "failed_remove_attempt" => self.edit("mutations", self.remove, |b| b["failed_attempts"] = json!(1)),
            "remove_retry" => self.edit("mutations", self.remove, |b| b["retry_at"] = json!(1)),
            "captured_id" => self.edit("mutations", self.remove, |b| b["request"]["intent"]["before"]["id"] = json!("other-local-id")),
            "captured_parent" => self.edit("mutations", self.remove, |b| b["request"]["intent"]["before"]["parent_id"] = json!("other-parent")),
            "captured_name" => self.edit("mutations", self.remove, |b| b["request"]["intent"]["before"]["name"] = json!("other.tmp")),
            "captured_etag" => self.edit("mutations", self.remove, |b| b["request"]["intent"]["before"]["etag"] = json!("other-etag")),
            "captured_content_version" => self.edit("mutations", self.remove, |b| b["request"]["intent"]["before"]["content_version"] = json!("other-content")),
            "captured_size" => self.edit("mutations", self.remove, |b| b["request"]["intent"]["before"]["size"] = json!(CONTENT.len() + 1)),
            "working_node" => self.edit("working_files", self.working, |b| b["node"]["size"] = json!(CONTENT.len() + 1)),
            "working_native" => self.edit("working_files", self.working, |b| b["native"] = json!(true)),
            "working_linked" => self.edit("working_files", self.working, |b| b["unlinked"] = json!(false)),
            "owner_not_remote_owned" => self.edit("namespace_objects", self.owner, |b| b["remote_owned"] = json!(false)),
            "owner_follows_remote" => self.edit("namespace_objects", self.owner, |b| b["follows_remote"] = json!(true)),
            "remote_etag" => self.edit("namespace_objects", self.owner, |b| b["remote"]["etag"] = json!("other-remote-etag")),
            "remote_id" => self.edit("namespace_objects", self.owner, |b| b["remote"]["id"] = json!("other-remote-id")),
            "remote_package" => self.edit("namespace_objects", self.owner, |b| b["remote"]["package"] = json!(true)),
            "package_representation" => self.edit("uploads", self.upload.id, |b| b["representation"] = json!({
                "kind": "package_archive", "expected_root": "Fixture.numbers",
                "semantic": {"version": 2, "sha256": "a".repeat(64), "entries": 2, "files": 1, "expanded_bytes": 1}
            })),
            _ => panic!("unknown registered hostile arm"),
        }
    }
}

fn location(id: uuid::Uuid) -> RecoveryLocation {
    RecoveryLocation::Trash {
        local_name: format!("recovery-{id}.tmp"),
        parent: "trash-root".into(),
    }
}

fn backup_temp() -> Node {
    let mut backup = old_temp();
    backup.parent_id = Some("trash-root".into());
    backup.etag = Some("trash-v1".into());
    backup
}

const HOSTILE: &[&str] = &[
    "missing_edge",
    "wrong_edge",
    "missing_remove",
    "wrong_remove_owner",
    "missing_upload_owner",
    "remove_scope",
    "owner_scope",
    "working_scope",
    "remove_stream",
    "owner_stream",
    "working_latest",
    "owner_latest",
    "missing_base",
    "wrong_predecessor",
    "resolved_remove",
    "remove_not_pending",
    "remove_state_column",
    "remove_sequence_column",
    "remove_queue_complete",
    "remove_queue_sequence",
    "attempt_present",
    "receipt_present",
    "prepared_present",
    "failed_remove_attempt",
    "remove_retry",
    "captured_id",
    "captured_parent",
    "captured_name",
    "captured_etag",
    "captured_content_version",
    "captured_size",
    "working_node",
    "working_native",
    "working_linked",
    "owner_not_remote_owned",
    "owner_follows_remote",
    "remote_etag",
    "remote_id",
    "remote_package",
    "package_representation",
];

#[test]
fn unlinked_successor_reservation_refuses_hostile_remove_lineage_without_changes() {
    for arm in HOSTILE {
        let mut fixture = PendingRemove::new(false);
        fixture.corrupt(arm);
        let before = fixture.snapshot();
        let result = fixture.journal.reserve_identity_handoff(
            fixture.upload.id,
            fixture.upload.attempt.unwrap(),
            location(fixture.upload.id),
        );
        assert!(result.is_err(), "reservation accepted hostile arm {arm}");
        assert_eq!(
            fixture.snapshot(),
            before,
            "reservation changed hostile fixture {arm}"
        );
    }
}

#[test]
fn unlinked_existing_reservation_refuses_hostile_remove_lineage_without_changes() {
    for arm in HOSTILE {
        let mut fixture = PendingRemove::new(true);
        fixture.corrupt(arm);
        let before = fixture.snapshot();
        let result = fixture.journal.reserve_identity_handoff(
            fixture.upload.id,
            fixture.upload.attempt.unwrap(),
            location(fixture.upload.id),
        );
        assert!(
            result.is_err(),
            "saved reservation accepted hostile arm {arm}"
        );
        assert_eq!(
            fixture.snapshot(),
            before,
            "saved reservation changed hostile fixture {arm}"
        );
    }
}

#[test]
fn unlinked_successor_confirmation_refuses_hostile_remove_lineage_without_changes() {
    for arm in HOSTILE {
        let mut fixture = PendingRemove::new(true);
        fixture.corrupt(arm);
        let before = fixture.snapshot();
        let result = fixture.journal.acknowledge_identity_handoff(
            fixture.upload.id,
            fixture.upload.attempt.unwrap(),
            current_temp(),
            backup_temp(),
        );
        assert!(result.is_err(), "confirmation accepted hostile arm {arm}");
        assert_eq!(
            fixture.snapshot(),
            before,
            "confirmation changed hostile fixture {arm}"
        );
    }
}

#[test]
fn reservation_before_unlink_reopens_and_confirms_exact_pending_remove_without_resurrection() {
    let fixture = PendingRemove::new(true);
    let PendingRemove {
        root,
        mut journal,
        upload,
        owner,
        working,
        remove,
    } = fixture;
    journal
        .record_session(upload.id, upload.attempt.unwrap(), upload.id, 0)
        .unwrap();
    journal
        .write_working(working, CONTENT.len() as u64, b"new-local-tail")
        .unwrap();
    let newer = journal.read_working(working, 0, 128).unwrap();
    let sealed = payload(&journal, upload.id);
    let original_reservation =
        serde_json::to_value(journal.get(upload.id).unwrap()).unwrap()["identity_handoff"].clone();
    drop(journal);
    let mut journal = UploadJournal::open(&root, "fixture", 1024 * 1024).unwrap();
    assert_eq!(
        journal.get(upload.id).unwrap().state,
        UploadState::VerifyRequired
    );
    let verification = journal.claim_verification(upload.id).unwrap();
    let recovery = journal
        .reserve_identity_handoff(
            upload.id,
            verification.attempt.unwrap(),
            location(upload.id),
        )
        .unwrap();
    assert_eq!(
        serde_json::to_value(journal.get(upload.id).unwrap()).unwrap()["identity_handoff"],
        original_reservation
    );
    journal
        .acknowledge_identity_handoff(
            upload.id,
            verification.attempt.unwrap(),
            current_temp(),
            backup_temp(),
        )
        .unwrap();
    let object = journal.namespace_object(owner).unwrap();
    assert!(object.unlinked);
    assert_eq!(object.latest, Some(remove));
    assert_eq!(object.remote, Some(current_temp()));
    assert_eq!(
        journal.namespace_object(recovery).unwrap().remote,
        Some(backup_temp())
    );
    let removed = journal.claim_mutation().unwrap().unwrap();
    assert_eq!(removed.id, remove);
    assert!(
        matches!(&removed.request.intent, MutationIntent::RemoveFile { before } if before == &current_temp())
    );
    journal
        .acknowledge_mutation(
            remove,
            removed.attempt.unwrap(),
            MutationReceipt::Removed {
                item: current_temp().id,
            },
        )
        .unwrap();
    assert!(journal.namespace_object(owner).unwrap().unlinked);
    let file = journal.working_file(working).unwrap();
    assert!(file.unlinked && file.dirty);
    assert_eq!(journal.read_working(working, 0, 128).unwrap(), newer);
    assert_eq!(payload(&journal, upload.id), sealed);
    assert_eq!(sealed, CONTENT);
    assert!(
        journal
            .namespace_overlay(&scope(), "root", vec![])
            .unwrap()
            .nodes
            .is_empty()
    );
    drop(journal);
}

fn node(id: &str, name: &str, parent: &str, size: u64, revision: &str) -> Node {
    Node {
        id: id.into(),
        parent_id: Some(parent.into()),
        name: name.into(),
        kind: NodeKind::File,
        package: false,
        size,
        modified_unix: 0,
        etag: Some(revision.into()),
        content_version: Some(format!("content-{revision}")),
        target: None,
    }
}

fn old_temp() -> Node {
    node("old-empty-temp", "editor.tmp", "root", 0, "empty-v1")
}

fn current_temp() -> Node {
    node(
        "new-temp",
        "editor.tmp",
        "root",
        CONTENT.len() as u64,
        "temp-v2",
    )
}

fn final_file() -> Node {
    node(
        "final-item",
        "final.xlsx",
        "root",
        CONTENT.len() as u64,
        "final-v1",
    )
}

#[derive(Default)]
struct Calls {
    staged: usize,
    begins: usize,
    streams: usize,
    commits: usize,
    reserved_at_begin: bool,
    data: Vec<u8>,
    removed: Vec<Node>,
}

struct StagedProvider {
    calls: Mutex<Calls>,
    probe: Arc<LockProbe>,
    journal: Arc<Mutex<UploadJournal>>,
}

#[async_trait]
impl UploadProvider for StagedProvider {
    fn staged_recovery_location(
        &self,
        operation: &str,
        request: &UploadRequest,
    ) -> Option<RecoveryLocation> {
        if let UploadIntent::Replace {
            item,
            expected_etag,
        } = &request.intent
        {
            assert_eq!(request.scope, scope());
            assert_eq!(item, &old_temp().id);
            assert_eq!(Some(expected_etag), old_temp().etag.as_ref());
            self.calls.lock().unwrap().staged += 1;
            Some(RecoveryLocation::Trash {
                local_name: format!("recovery-{operation}.tmp"),
                parent: "trash-root".into(),
            })
        } else {
            None
        }
    }

    async fn begin_upload_for_operation(
        &self,
        operation: &str,
        request: &UploadRequest,
        cancel: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        self.probe.check();
        if matches!(request.intent, UploadIntent::Replace { .. }) {
            let id = uuid::Uuid::parse_str(operation).unwrap();
            let record = self.journal.lock().unwrap().get(id).unwrap();
            // The public serialized record exposes the actual durable reservation;
            // the test never writes private journal internals or invents a receipt.
            let body = serde_json::to_value(record).unwrap();
            assert!(!body["identity_handoff"].is_null());
            self.calls.lock().unwrap().reserved_at_begin = true;
        }
        self.begin_upload(request, cancel).await
    }

    async fn begin_upload(
        &self,
        request: &UploadRequest,
        _cancel: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        self.probe.check();
        request.require_file_bytes()?;
        assert_eq!(request.scope, scope());
        let mut calls = self.calls.lock().unwrap();
        calls.begins += 1;
        calls.data.clear();
        Ok(UploadStep::Stream(SecretString::from("fixture-stream")))
    }

    async fn upload_stream(
        &self,
        request: &UploadRequest,
        _checkpoint: &SecretString,
        mut payload: File,
        _cancel: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        self.probe.check();
        let mut data = Vec::new();
        payload.read_to_end(&mut data).unwrap();
        assert_eq!(data.len() as u64, request.size);
        assert_eq!(hex::encode(Sha256::digest(&data)), request.sha256);
        let mut calls = self.calls.lock().unwrap();
        calls.streams += 1;
        calls.data = data;
        Ok(UploadStep::Commit(SecretString::from("fixture-commit")))
    }

    async fn commit_upload(
        &self,
        request: &UploadRequest,
        _checkpoint: &SecretString,
        _cancel: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        self.probe.check();
        let mut calls = self.calls.lock().unwrap();
        calls.commits += 1;
        assert_eq!(hex::encode(Sha256::digest(&calls.data)), request.sha256);
        match &request.intent {
            UploadIntent::Create { parent, name } => {
                assert_eq!(parent, "root");
                if name == "editor.tmp" {
                    assert!(calls.data.is_empty());
                    Ok(UploadStep::Complete(old_temp()))
                } else {
                    assert_eq!(name, "final.xlsx");
                    assert_eq!(calls.data, CONTENT);
                    Ok(UploadStep::Complete(final_file()))
                }
            }
            UploadIntent::Replace { .. } => {
                assert!(calls.reserved_at_begin);
                assert_eq!(calls.data, CONTENT);
                let mut backup = old_temp();
                backup.parent_id = Some("trash-root".into());
                backup.etag = Some("trash-v1".into());
                Ok(UploadStep::HandoffComplete {
                    current: current_temp(),
                    backup,
                })
            }
        }
    }

    async fn inspect_upload(
        &self,
        _request: &UploadRequest,
        _checkpoint: &SecretString,
        _cancel: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        panic!("this fresh successful attempt must not inspect or replay")
    }

    async fn upload_part(
        &self,
        _request: &UploadRequest,
        _checkpoint: &SecretString,
        _offset: u64,
        _bytes: Vec<u8>,
        _cancel: &CancellationToken,
    ) -> UploadResult<UploadStep> {
        panic!("the synthetic provider consumes the actual sealed stream")
    }

    async fn reconcile_upload(
        &self,
        _request: &UploadRequest,
        _checkpoint: Option<&SecretString>,
        _cancel: &CancellationToken,
    ) -> UploadResult<Reconciliation> {
        panic!("this fresh successful attempt must not reconcile or replay")
    }
}

#[async_trait]
impl MutationProvider for StagedProvider {
    async fn mutate(
        &self,
        request: &MutationRequest,
        _cancel: &CancellationToken,
    ) -> MutationResult<MutationReceipt> {
        self.probe.check();
        assert_eq!(request.scope, scope());
        let MutationIntent::RemoveFile { before } = &request.intent else {
            panic!("only the exact pending cleanup is admitted")
        };
        assert_eq!(
            before,
            &current_temp(),
            "cleanup must use the new ID and ETag"
        );
        self.calls.lock().unwrap().removed.push(before.clone());
        Ok(MutationReceipt::Removed {
            item: before.id.clone(),
        })
    }

    async fn reconcile_mutation(
        &self,
        _request: &MutationRequest,
        _cancel: &CancellationToken,
    ) -> MutationResult<MutationReconciliation> {
        panic!("the successful conditional removal must not be replayed")
    }
}

fn payload(journal: &UploadJournal, id: uuid::Uuid) -> Vec<u8> {
    let mut bytes = Vec::new();
    journal
        .payload(id)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    bytes
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sealed_successor_unlinked_before_handoff_completes_then_removes_current_identity() {
    run_unlinked_successor(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn post_unlink_descriptor_writes_and_resize_preserve_newer_local_bytes() {
    run_unlinked_successor(true).await;
}

async fn run_unlinked_successor(dirty_after_unlink: bool) {
    let root = tempfile::Builder::new()
        .prefix("cirrove-unlinked-staged-successor-")
        .tempdir()
        .unwrap()
        .keep();
    eprintln!("retained synthetic handoff fixture: {}", root.display());
    let probe = Arc::new(LockProbe::default());
    let journal = journal(&root.join("journal"), &probe);
    let provider = Arc::new(StagedProvider {
        calls: Mutex::new(Calls::default()),
        probe: probe.clone(),
        journal: journal.clone(),
    });
    let vault = Arc::new(Vault {
        probe,
        ..Default::default()
    });
    let worker = TransferWorker::new(
        journal.clone(),
        provider.clone(),
        vault,
        CancellationToken::new(),
    );
    let mutations =
        MutationWorker::new(journal.clone(), provider.clone(), CancellationToken::new());
    let (working, empty) = {
        let mut j = journal.lock().unwrap();
        let working = j
            .create_working(scope(), old_temp(), true, &b""[..])
            .unwrap();
        let empty = j.seal_working(working.id).unwrap().unwrap();
        (working.id, empty.id)
    };
    let initial = worker.run_once().await.unwrap().unwrap();
    assert_eq!(initial.id, empty);
    assert_eq!(initial.state, UploadState::Uploaded);

    let (successor, final_upload, final_before, final_working, owner, removal) = {
        let mut j = journal.lock().unwrap();
        assert_eq!(j.get(empty).unwrap().remote, Some(old_temp()));
        assert!(payload(&j, empty).is_empty());
        j.write_working(working, 0, CONTENT).unwrap();
        let successor = j.seal_working(working).unwrap().unwrap();
        assert_eq!(successor.base.as_ref().unwrap().predecessor, empty);
        assert!(!successor.base.as_ref().unwrap().resolved);
        let final_working = j
            .create_working(
                scope(),
                node("", "final.xlsx", "root", 0, "local"),
                true,
                &b""[..],
            )
            .unwrap();
        j.write_working(final_working.id, 0, CONTENT).unwrap();
        let final_upload = j.seal_working(final_working.id).unwrap().unwrap();
        let final_before = serde_json::to_value(&final_upload).unwrap();
        let owner = j.namespace_for_operation(successor.id).unwrap().unwrap();
        let unlinked = j
            .unlink_namespace_file(owner.id, owner.revision, false)
            .unwrap();
        assert!(unlinked.object.unlinked);
        assert_eq!(unlinked.object.latest, Some(unlinked.mutation.id));
        assert_eq!(
            unlinked.mutation.base.as_ref().unwrap().predecessor,
            successor.id
        );
        assert!(!unlinked.mutation.base.as_ref().unwrap().resolved);
        assert_eq!(unlinked.mutation.working_file, Some(working));
        assert_eq!(unlinked.mutation.state, MutationState::Pending);
        assert!(j.claim_mutation().unwrap().is_none());
        (
            successor.id,
            final_upload.id,
            final_before,
            final_working.id,
            owner.id,
            unlinked.mutation.id,
        )
    };

    let newer = b"newer-open-descriptor-bytes-after-unlink";
    let expected_working = if dirty_after_unlink {
        let mut j = journal.lock().unwrap();
        j.write_working(working, 0, newer).unwrap();
        // Both growth and truncation are legitimate descriptor-only changes.
        j.truncate_working(working, (newer.len() - 3) as u64)
            .unwrap();
        assert!(j.seal_working(working).unwrap().is_none());
        &newer[..newer.len() - 3]
    } else {
        CONTENT
    };

    let preserved_working =
        serde_json::to_value(journal.lock().unwrap().working_file(working).unwrap()).unwrap();
    let outcome = worker.run_once().await.unwrap().unwrap();
    assert_eq!(outcome.id, successor);
    {
        let j = journal.lock().unwrap();
        assert_eq!(payload(&j, successor), CONTENT);
        assert_eq!(payload(&j, final_upload), CONTENT);
        assert_eq!(
            serde_json::to_value(j.get(final_upload).unwrap()).unwrap(),
            final_before
        );
        assert_eq!(
            j.read_working(working, 0, expected_working.len() as u32)
                .unwrap(),
            expected_working
        );
        assert_eq!(
            j.read_working(final_working, 0, CONTENT.len() as u32)
                .unwrap(),
            CONTENT
        );
        if outcome.state == UploadState::VerifyRequired {
            assert_eq!(outcome.issue, Some(JournalError::Stale.to_string()));
            let record = j.get(successor).unwrap();
            assert_eq!(record.failed_attempts, 1);
            assert!(record.session_key.is_none());
            assert_eq!(record.transferred_bytes, 0);
            let calls = provider.calls.lock().unwrap();
            assert_eq!(calls.staged, 1);
            assert_eq!((calls.begins, calls.streams, calls.commits), (1, 1, 1));
        }
    }
    // Original code reaches this endpoint with VerifyRequired, before begin;
    // preceding assertions prove the actual lineage and preservation, not a
    // handcrafted stale row or a provider failure.
    assert_eq!(
        outcome.state,
        UploadState::Uploaded,
        "sealed ordinary successor was rejected before its staged handoff",
    );
    assert!(outcome.issue.is_none());
    {
        let j = journal.lock().unwrap();
        let object = j.namespace_object(owner).unwrap();
        assert!(object.unlinked);
        assert_eq!(object.latest, Some(removal));
        assert_eq!(object.remote, Some(current_temp()));
        let record = serde_json::to_value(j.get(successor).unwrap()).unwrap();
        let handoff = &record["identity_handoff"];
        assert_eq!(handoff["old_item"], "old-empty-temp");
        let mut backup = old_temp();
        backup.parent_id = Some("trash-root".into());
        backup.etag = Some("trash-v1".into());
        assert_eq!(handoff["backup"], serde_json::to_value(&backup).unwrap());
        let recovery = uuid::Uuid::parse_str(handoff["recovery_object"].as_str().unwrap()).unwrap();
        let recovery = j.namespace_object(recovery).unwrap();
        assert!(recovery.unlinked);
        assert_eq!(recovery.remote, Some(backup));
        let calls = provider.calls.lock().unwrap();
        assert!(calls.reserved_at_begin);
        assert_eq!((calls.begins, calls.streams, calls.commits), (2, 2, 2));
    }
    let final_result = worker.run_once().await.unwrap().unwrap();
    assert_eq!(final_result.id, final_upload);
    assert_eq!(final_result.state, UploadState::Uploaded);
    let removed = mutations.run_once().await.unwrap().unwrap();
    assert_eq!(removed.id, removal);
    assert_eq!(removed.state, MutationState::Applied);
    assert!(mutations.run_once().await.unwrap().is_none());
    assert!(worker.run_once().await.unwrap().is_none());
    {
        let j = journal.lock().unwrap();
        assert_eq!(j.get(final_upload).unwrap().remote, Some(final_file()));
        assert_eq!(payload(&j, final_upload), CONTENT);
        assert!(j.namespace_object(owner).unwrap().unlinked);
        let working = j.working_file(working).unwrap();
        assert!(working.unlinked);
        assert_eq!(working.dirty, dirty_after_unlink);
        assert_eq!(working.node.size, expected_working.len() as u64);
        assert_eq!(
            j.read_working(working.id, 0, expected_working.len() as u32)
                .unwrap(),
            expected_working
        );
        let listing = j
            .namespace_overlay(&scope(), "root", vec![final_file()])
            .unwrap();
        assert!(listing.conflicts.is_empty());
        assert_eq!(listing.nodes.len(), 1);
        assert_eq!(listing.nodes[0].name, "final.xlsx");
        let calls = provider.calls.lock().unwrap();
        assert_eq!(calls.removed, vec![current_temp()]);
        assert_eq!(calls.staged, 1);
        assert_eq!((calls.begins, calls.streams, calls.commits), (3, 3, 3));
    }
    // Close all writer owners before returning; retain every fixture file.
    drop(mutations);
    drop(worker);
    drop(provider);
    drop(journal);
    let reopened = UploadJournal::open(&root.join("journal"), "fixture", 1024 * 1024).unwrap();
    assert_eq!(
        serde_json::to_value(reopened.working_file(working).unwrap()).unwrap(),
        preserved_working
    );
    assert_eq!(
        reopened
            .read_working(working, 0, expected_working.len() as u32)
            .unwrap(),
        expected_working
    );
    assert_eq!(payload(&reopened, successor), CONTENT);
    assert_eq!(payload(&reopened, final_upload), CONTENT);
    let object = reopened.namespace_object(owner).unwrap();
    assert!(object.unlinked);
    assert_eq!(object.latest, Some(removal));
    assert_eq!(object.remote, Some(current_temp()));
    assert_eq!(
        reopened.mutation(removal).unwrap().state,
        MutationState::Applied
    );
    drop(reopened);
}
