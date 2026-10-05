//! Actual API-driven ordinary source upload preceding an atomic replacement.
//! Synthetic provider callbacks consume real sealed bytes. All fixture files retained.
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
    journal::{JournalError, MutationState, UploadJournal, UploadState},
    mutations::MutationWorker,
    transfers::TransferWorker,
};
use rusqlite::{Connection, OpenFlags, params};
use secrecy::SecretString;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::Read,
    sync::{Arc, Mutex},
};

const ORIGINAL: &[u8] = b"original-final-document-A";
const CONTENT: &[u8] = b"sealed-editor-document-B";
fn scope() -> Scope {
    Scope {
        account: "fixture".into(),
        provider: "fixture".into(),
        collection: "drive".into(),
    }
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
    node(
        "old-empty-temp",
        "editor.tmp",
        "source-parent",
        0,
        "empty-v1",
    )
}
fn current_temp() -> Node {
    node(
        "current-temp-B",
        "editor.tmp",
        "source-parent",
        CONTENT.len() as u64,
        "temp-v2",
    )
}
fn old_final(parent: &str) -> Node {
    node(
        "old-final-A",
        "final.xlsx",
        parent,
        ORIGINAL.len() as u64,
        "final-v1",
    )
}
fn current_final(parent: &str) -> Node {
    node(
        "current-final-B",
        "final.xlsx",
        parent,
        CONTENT.len() as u64,
        "final-v2",
    )
}
fn backup(mut original: Node) -> Node {
    original.parent_id = Some("trash-root".into());
    original.etag = Some(format!("trash-{}", original.id));
    original
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
    destination: String,
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
            let original = if item == &old_temp().id {
                old_temp()
            } else {
                assert_eq!(item, &old_final(&self.destination).id);
                old_final(&self.destination)
            };
            assert_eq!(Some(expected_etag), original.etag.as_ref());
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
                assert_eq!(
                    parent,
                    if name == "editor.tmp" {
                        "source-parent"
                    } else {
                        self.destination.as_str()
                    }
                );
                if name == "editor.tmp" {
                    assert!(calls.data.is_empty());
                    Ok(UploadStep::Complete(old_temp()))
                } else {
                    assert_eq!(name, "final.xlsx");
                    assert_eq!(calls.data, ORIGINAL);
                    Ok(UploadStep::Complete(old_final(&self.destination)))
                }
            }
            UploadIntent::Replace { item, .. } => {
                assert!(calls.reserved_at_begin);
                assert_eq!(calls.data, CONTENT);
                let original = if item == &old_temp().id {
                    old_temp()
                } else {
                    assert_eq!(item, &old_final(&self.destination).id);
                    old_final(&self.destination)
                };
                let current = if item == &old_temp().id {
                    current_temp()
                } else {
                    current_final(&self.destination)
                };
                Ok(UploadStep::HandoffComplete {
                    current,
                    backup: backup(original),
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

/// Two real public API topologies; the same success assertion is the baseline endpoint.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn atomic_source_prerequisite_handoff_preserves_final_name_and_cleans_current_temp() {
    run_atomic_source("source-parent").await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn atomic_source_prerequisite_cross_directory_preserves_both_parent_routes() {
    run_atomic_source("destination-parent").await;
}
async fn run_atomic_source(destination: &str) {
    let root = tempfile::Builder::new()
        .prefix("cirrove-atomic-source-")
        .tempdir()
        .unwrap()
        .keep();
    eprintln!("retained synthetic atomic fixture: {}", root.display());
    let journal_root = root.join("journal");
    let probe = Arc::new(LockProbe::default());
    let journal = journal(&journal_root, &probe);
    let provider = Arc::new(StagedProvider {
        calls: Mutex::new(Calls::default()),
        probe: probe.clone(),
        journal: journal.clone(),
        destination: destination.into(),
    });
    let worker = TransferWorker::new(
        journal.clone(),
        provider.clone(),
        Arc::new(Vault {
            probe,
            ..Default::default()
        }),
        CancellationToken::new(),
    );
    let mutations =
        MutationWorker::new(journal.clone(), provider.clone(), CancellationToken::new());
    // Genuine completed A, detached by the normal retirement API then re-observed.
    // This makes target.base=None, rather than forging a retired victim body.
    let original = {
        let mut j = journal.lock().unwrap();
        let file = j
            .create_working(
                scope(),
                node("", "final.xlsx", destination, 0, "local-A"),
                true,
                &b""[..],
            )
            .unwrap();
        j.write_working(file.id, 0, ORIGINAL).unwrap();
        j.seal_working(file.id).unwrap().unwrap().id
    };
    let initial = worker.run_once().await.unwrap().unwrap();
    assert_eq!(initial.id, original);
    assert_eq!(initial.state, UploadState::Uploaded);
    assert!(initial.issue.is_none());
    let victim = {
        let mut j = journal.lock().unwrap();
        let object = j.namespace_for_operation(original).unwrap().unwrap();
        j.handoff_namespace(object.id, object.revision, old_final(destination))
            .unwrap();
        let observed = j
            .observe_namespace_file(scope(), old_final(destination))
            .unwrap();
        // Genuine hydration also matches the actual editor victim's open stream.
        let hydrated = j
            .create_working(scope(), old_final(destination), false, ORIGINAL)
            .unwrap();
        assert_eq!(hydrated.latest, None);
        let observed = j.namespace_object(observed.id).unwrap();
        assert!(!observed.follows_remote);
        assert_eq!(observed.latest, None);
        assert_eq!(observed.working_file, Some(hydrated.id));
        observed.id
    };
    let (working, empty) = {
        let mut j = journal.lock().unwrap();
        let file = j
            .create_working(scope(), old_temp(), true, &b""[..])
            .unwrap();
        (file.id, j.seal_working(file.id).unwrap().unwrap().id)
    };
    let empty_result = worker.run_once().await.unwrap().unwrap();
    assert_eq!(empty_result.id, empty);
    assert_eq!(empty_result.state, UploadState::Uploaded);
    assert!(empty_result.issue.is_none());
    let (successor, owner, replacement, target_before) = {
        let mut j = journal.lock().unwrap();
        j.write_working(working, 0, CONTENT).unwrap();
        let upload = j.seal_working(working).unwrap().unwrap();
        assert_eq!(upload.base.as_ref().unwrap().predecessor, empty);
        assert!(!upload.base.as_ref().unwrap().resolved);
        let source = j.namespace_for_operation(upload.id).unwrap().unwrap();
        let victim = j.namespace_object(victim).unwrap();
        let replacement = j
            .replace_namespace_file(
                source.id,
                source.revision,
                victim.id,
                victim.revision,
                false,
            )
            .unwrap();
        let target = j.get(replacement.id).unwrap();
        assert!(target.base.is_none());
        assert_eq!(target.working_file, Some(working));
        assert_eq!(target.size, upload.size);
        assert_eq!(target.sha256, upload.sha256);
        assert!(replacement.local_ready && !replacement.remote_applied);
        assert_eq!(replacement.source, source.id);
        assert_eq!(replacement.victim, victim.id);
        let source = j.namespace_object(source.id).unwrap();
        assert_eq!(source.latest, Some(replacement.id));
        assert_eq!(source.node.name, "final.xlsx");
        assert_eq!(source.node.parent_id.as_deref(), Some(destination));
        assert_eq!(source.remote, Some(old_temp()));
        assert!(j.replacement(upload.id).is_err());
        let cleanup = j.mutation(replacement.cleanup).unwrap();
        assert_eq!(cleanup.state, MutationState::Pending);
        assert_eq!(cleanup.base.as_ref().unwrap().predecessor, upload.id);
        assert!(!cleanup.base.as_ref().unwrap().resolved);
        assert!(cleanup.attempt.is_none() && cleanup.receipt.is_none());
        assert!(
            matches!(&cleanup.request.intent, MutationIntent::RemoveFile { before } if before.name == "editor.tmp" && before.parent_id.as_deref() == Some("source-parent"))
        );
        assert!(j.claim_mutation().unwrap().is_none());
        (
            upload.id,
            source.id,
            replacement,
            serde_json::to_value(target).unwrap(),
        )
    };
    // Read-only SQL confirms the actual API committed BOTH distinct dependencies.
    {
        let db = Connection::open_with_flags(
            journal_root.join("uploads.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        for (operation, predecessor) in [
            (replacement.id, successor),
            (replacement.cleanup, replacement.id),
        ] {
            let count: i64 = db.query_row("SELECT count(*) FROM write_prerequisites WHERE operation=?1 AND predecessor=?2", params![operation.to_string(), predecessor.to_string()], |r| r.get(0)).unwrap();
            assert_eq!(count, 1);
        }
        let count: i64 = db
            .query_row(
                "SELECT count(*) FROM write_successors WHERE predecessor=?1 AND successor=?2",
                params![successor.to_string(), replacement.cleanup.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }
    let outcome = worker.run_once().await.unwrap().unwrap();
    assert_eq!(outcome.id, successor);
    {
        let j = journal.lock().unwrap();
        assert_eq!(payload(&j, original), ORIGINAL);
        assert!(payload(&j, empty).is_empty());
        assert_eq!(payload(&j, successor), CONTENT);
        assert_eq!(payload(&j, replacement.id), CONTENT);
        assert_eq!(
            j.read_working(working, 0, CONTENT.len() as u32).unwrap(),
            CONTENT
        );
        assert_eq!(
            serde_json::to_value(j.get(replacement.id).unwrap()).unwrap(),
            target_before
        );
        assert_eq!(j.namespace_object(owner).unwrap().node.name, "final.xlsx");
        if outcome.state == UploadState::VerifyRequired {
            assert_eq!(outcome.issue, Some(JournalError::Stale.to_string()));
            let upload = j.get(successor).unwrap();
            assert_eq!(upload.failed_attempts, 1);
            assert!(upload.session_key.is_none());
            assert_eq!(upload.transferred_bytes, 0);
            let calls = provider.calls.lock().unwrap();
            assert_eq!(calls.staged, 1);
            assert_eq!((calls.begins, calls.streams, calls.commits), (2, 2, 2));
        }
    }
    // Baseline must fail HERE, with real typed lineage and preserved sealed bytes,
    // before any source Replace begin/stream/commit callback is reached.
    assert_eq!(
        outcome.state,
        UploadState::Uploaded,
        "atomic source prerequisite refused before staged handoff"
    );
    assert!(outcome.issue.is_none());
    {
        let mut j = journal.lock().unwrap();
        let source = j.namespace_object(owner).unwrap();
        assert!(!source.unlinked && source.remote_owned);
        assert_eq!(source.latest, Some(replacement.id));
        assert_eq!(source.node.name, "final.xlsx");
        assert_eq!(source.node.parent_id.as_deref(), Some(destination));
        assert_eq!(source.remote, Some(current_temp()));
        let upload = serde_json::to_value(j.get(successor).unwrap()).unwrap();
        assert_eq!(
            upload["identity_handoff"]["backup"],
            serde_json::to_value(backup(old_temp())).unwrap()
        );
        assert!(
            j.claim_mutation().unwrap().is_none(),
            "cleanup must await atomic final receipt"
        );
    }
    let final_result = worker.run_once().await.unwrap().unwrap();
    assert_eq!(final_result.id, replacement.id);
    assert_eq!(final_result.state, UploadState::Uploaded);
    assert!(final_result.issue.is_none());
    {
        let j = journal.lock().unwrap();
        assert!(j.replacement(replacement.id).unwrap().remote_applied);
        assert_eq!(
            j.namespace_object(owner).unwrap().remote,
            Some(current_final(destination))
        );
        assert!(!j.namespace_object(victim).unwrap().remote_owned);
        assert_eq!(
            j.namespace_object(replacement.cleanup_object)
                .unwrap()
                .remote,
            Some(current_temp())
        );
    }
    let cleanup = mutations.run_once().await.unwrap().unwrap();
    assert_eq!(cleanup.id, replacement.cleanup);
    assert_eq!(cleanup.state, MutationState::Applied);
    assert!(cleanup.issue.is_none());
    assert!(mutations.run_once().await.unwrap().is_none());
    assert!(worker.run_once().await.unwrap().is_none());
    {
        let calls = provider.calls.lock().unwrap();
        assert_eq!(
            (calls.staged, calls.begins, calls.streams, calls.commits),
            (2, 4, 4, 4)
        );
        assert_eq!(calls.removed, vec![current_temp()]);
    }
    drop(mutations);
    drop(worker);
    drop(provider);
    drop(journal);
    let j = UploadJournal::open(&journal_root, "fixture", 1024 * 1024).unwrap();
    assert_eq!(payload(&j, original), ORIGINAL);
    assert_eq!(payload(&j, successor), CONTENT);
    assert_eq!(payload(&j, replacement.id), CONTENT);
    let source = j.namespace_object(owner).unwrap();
    assert!(!source.unlinked && source.remote_owned);
    assert_eq!(source.node.name, "final.xlsx");
    assert_eq!(source.node.parent_id.as_deref(), Some(destination));
    assert_eq!(source.remote, Some(current_final(destination)));
    assert_eq!(
        j.read_working(working, 0, CONTENT.len() as u32).unwrap(),
        CONTENT
    );
    assert_eq!(
        j.mutation(replacement.cleanup).unwrap().state,
        MutationState::Applied
    );
}

/// Admission controls start with genuine public journal operations. Only named
/// hostile arms alter retained synthetic SQL after the reservation is durable.
struct AtomicPending {
    root: std::path::PathBuf,
    journal: UploadJournal,
    upload: cirrove_service::journal::UploadRecord,
    original: uuid::Uuid,
    empty: uuid::Uuid,
    target: uuid::Uuid,
    cleanup: uuid::Uuid,
    owner: uuid::Uuid,
    working: uuid::Uuid,
}
impl AtomicPending {
    fn new(reserve_before_takeover: bool) -> Self {
        Self::with_reservation(reserve_before_takeover, !reserve_before_takeover)
    }
    fn with_reservation(reserve_before_takeover: bool, reserve_after_takeover: bool) -> Self {
        let root = tempfile::Builder::new()
            .prefix("cirrove-atomic-authority-")
            .tempdir()
            .unwrap()
            .keep()
            .join("journal");
        let mut j = UploadJournal::open(&root, "fixture", 1024 * 1024).unwrap();
        let victim_file = j
            .create_working(
                scope(),
                node("", "final.xlsx", "source-parent", 0, "local-A"),
                true,
                &b""[..],
            )
            .unwrap();
        j.write_working(victim_file.id, 0, ORIGINAL).unwrap();
        j.seal_working(victim_file.id).unwrap().unwrap();
        let original = j.claim_next().unwrap().unwrap();
        j.acknowledge(
            original.id,
            original.attempt.unwrap(),
            old_final("source-parent"),
        )
        .unwrap();
        let victim = j.namespace_for_operation(original.id).unwrap().unwrap();
        j.handoff_namespace(victim.id, victim.revision, old_final("source-parent"))
            .unwrap();
        let victim = j
            .observe_namespace_file(scope(), old_final("source-parent"))
            .unwrap();
        j.create_working(scope(), old_final("source-parent"), false, ORIGINAL)
            .unwrap();
        let victim = j.namespace_object(victim.id).unwrap();
        let working = j
            .create_working(scope(), old_temp(), true, &b""[..])
            .unwrap();
        j.seal_working(working.id).unwrap().unwrap();
        let empty = j.claim_next().unwrap().unwrap();
        j.acknowledge(empty.id, empty.attempt.unwrap(), old_temp())
            .unwrap();
        j.write_working(working.id, 0, CONTENT).unwrap();
        j.seal_working(working.id).unwrap().unwrap();
        let upload = j.claim_next().unwrap().unwrap();
        assert!(upload.base.as_ref().unwrap().resolved);
        if reserve_before_takeover {
            j.reserve_identity_handoff(
                upload.id,
                upload.attempt.unwrap(),
                control_location(upload.id),
            )
            .unwrap();
        }
        let owner = j.namespace_for_operation(upload.id).unwrap().unwrap();
        let r = j
            .replace_namespace_file(owner.id, owner.revision, victim.id, victim.revision, false)
            .unwrap();
        if reserve_after_takeover {
            j.reserve_identity_handoff(
                upload.id,
                upload.attempt.unwrap(),
                control_location(upload.id),
            )
            .unwrap();
            let value = serde_json::to_value(j.get(upload.id).unwrap()).unwrap();
            assert_eq!(
                value["identity_handoff"]["atomic_source"],
                serde_json::json!(r.id)
            );
        }
        Self {
            root,
            journal: j,
            original: original.id,
            empty: empty.id,
            upload,
            target: r.id,
            cleanup: r.cleanup,
            owner: owner.id,
            working: working.id,
        }
    }
    fn db(&self) -> Connection {
        Connection::open(self.root.join("uploads.db")).unwrap()
    }
    fn edit(&self, table: &str, id: uuid::Uuid, change: impl FnOnce(&mut serde_json::Value)) {
        assert!(matches!(
            table,
            "uploads" | "mutations" | "namespace_objects" | "working_files" | "file_replacements"
        ));
        let db = self.db();
        let body: String = db
            .query_row(
                &format!("SELECT body FROM {table} WHERE id=?1"),
                [id.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        let mut body: serde_json::Value = serde_json::from_str(&body).unwrap();
        change(&mut body);
        assert_eq!(
            db.execute(
                &format!("UPDATE {table} SET body=?2 WHERE id=?1"),
                params![id.to_string(), serde_json::to_string(&body).unwrap()]
            )
            .unwrap(),
            1
        );
    }
    fn corrupt(&self, arm: &str) {
        use serde_json::json;
        let foreign = uuid::Uuid::new_v4();
        match arm {
            "missing_replacement" => {
                self.db()
                    .execute(
                        "DELETE FROM file_replacements WHERE id=?1",
                        [self.target.to_string()],
                    )
                    .unwrap();
            }
            "replacement_source" => self.edit("file_replacements", self.target, |b| {
                b["source"] = json!(foreign)
            }),
            "replacement_cleanup" => self.edit("file_replacements", self.target, |b| {
                b["cleanup"] = json!(foreign)
            }),
            "target_scope" => self.edit("uploads", self.target, |b| {
                b["scope"]["collection"] = json!("foreign-drive")
            }),
            "target_digest" => self.edit("uploads", self.target, |b| {
                b["sha256"] = json!("a".repeat(64))
            }),
            "target_queue_complete" => {
                self.db()
                    .execute(
                        "UPDATE write_queue SET complete=1 WHERE id=?1",
                        [self.target.to_string()],
                    )
                    .unwrap();
            }
            "missing_target_prerequisite" => {
                self.db()
                    .execute(
                        "DELETE FROM write_prerequisites WHERE operation=?1",
                        [self.target.to_string()],
                    )
                    .unwrap();
            }
            "missing_cleanup_successor" => {
                self.db()
                    .execute(
                        "DELETE FROM write_successors WHERE predecessor=?1",
                        [self.upload.id.to_string()],
                    )
                    .unwrap();
            }
            "missing_cleanup_barrier" => {
                self.db()
                    .execute(
                        "DELETE FROM write_prerequisites WHERE operation=?1",
                        [self.cleanup.to_string()],
                    )
                    .unwrap();
            }
            "cleanup_owner" => {
                self.db()
                    .execute(
                        "UPDATE namespace_operations SET object=?2 WHERE operation=?1",
                        params![self.cleanup.to_string(), foreign.to_string()],
                    )
                    .unwrap();
            }
            "cleanup_resolved" => self.edit("mutations", self.cleanup, |b| {
                b["base"]["resolved"] = json!(true)
            }),
            "cleanup_before_parent" => self.edit("mutations", self.cleanup, |b| {
                b["request"]["intent"]["before"]["parent_id"] = json!("foreign-parent")
            }),
            "cleanup_before_name" => self.edit("mutations", self.cleanup, |b| {
                b["request"]["intent"]["before"]["name"] = json!("foreign.tmp")
            }),
            "source_old_etag" => self.edit("namespace_objects", self.owner, |b| {
                b["remote"]["etag"] = json!("foreign-revision")
            }),
            "source_native_stream" => {
                self.edit("working_files", self.working, |b| b["native"] = json!(true))
            }
            _ => panic!("unregistered hostile authority arm"),
        }
    }
    fn snapshot(&self) -> (Vec<Vec<String>>, Vec<Vec<u8>>, Vec<u8>) {
        let db = self.db();
        let rows = [
            "SELECT json_array(sequence,id,state,body) FROM uploads ORDER BY sequence",
            "SELECT json_array(sequence,id,state,body) FROM mutations ORDER BY sequence",
            "SELECT json_array(id,body) FROM namespace_objects ORDER BY id",
            "SELECT json_array(id,body) FROM working_files ORDER BY id",
            "SELECT json_array(id,source,victim,cleanup,body) FROM file_replacements ORDER BY id",
            "SELECT json_array(operation,object) FROM namespace_operations ORDER BY operation",
            "SELECT json_array(identity,object) FROM namespace_remote ORDER BY identity",
            "SELECT json_array(predecessor,successor) FROM write_successors ORDER BY predecessor",
            "SELECT json_array(operation,predecessor) FROM write_prerequisites ORDER BY operation,predecessor",
            "SELECT json_array(sequence,id,complete) FROM write_queue ORDER BY sequence",
        ].into_iter().map(|sql| db.prepare(sql).unwrap().query_map([], |r| r.get::<_, String>(0)).unwrap().collect::<Result<Vec<_>, _>>().unwrap()).collect();
        (
            rows,
            [self.original, self.empty, self.upload.id, self.target]
                .into_iter()
                // A hostile arm changes the expected digest. Snapshot the
                // owned fixture's actual sealed bytes independently of that
                // damaged metadata; the production payload reader must refuse it.
                .map(|id| std::fs::read(self.root.join("objects").join(id.to_string())).unwrap())
                .collect(),
            self.journal
                .read_working(self.working, 0, CONTENT.len() as u32)
                .unwrap(),
        )
    }
}
fn control_location(id: uuid::Uuid) -> RecoveryLocation {
    RecoveryLocation::Trash {
        local_name: format!("recovery-{id}.tmp"),
        parent: "trash-root".into(),
    }
}
const ATOMIC_HOSTILE: &[&str] = &[
    "missing_replacement",
    "replacement_source",
    "replacement_cleanup",
    "target_scope",
    "target_digest",
    "target_queue_complete",
    "missing_target_prerequisite",
    "missing_cleanup_successor",
    "missing_cleanup_barrier",
    "cleanup_owner",
    "cleanup_resolved",
    "cleanup_before_parent",
    "cleanup_before_name",
    "source_old_etag",
    "source_native_stream",
];
#[test]
fn atomic_source_fresh_reservation_refuses_changed_authority_without_changes() {
    for arm in ATOMIC_HOSTILE {
        let mut f = AtomicPending::with_reservation(false, false);
        let body = serde_json::to_value(f.journal.get(f.upload.id).unwrap()).unwrap();
        assert!(body["identity_handoff"].is_null());
        f.corrupt(arm);
        let before = f.snapshot();
        let result = f.journal.reserve_identity_handoff(
            f.upload.id,
            f.upload.attempt.unwrap(),
            control_location(f.upload.id),
        );
        assert!(result.is_err(), "fresh reservation accepted hostile {arm}");
        assert_eq!(
            f.snapshot(),
            before,
            "fresh reservation altered hostile {arm}"
        );
    }
}
#[test]
fn atomic_source_latched_reservation_refuses_changed_authority_without_changes() {
    for arm in ATOMIC_HOSTILE {
        let mut f = AtomicPending::new(false);
        f.corrupt(arm);
        let before = f.snapshot();
        let result = f.journal.reserve_identity_handoff(
            f.upload.id,
            f.upload.attempt.unwrap(),
            control_location(f.upload.id),
        );
        assert!(result.is_err(), "latched reuse accepted hostile {arm}");
        assert_eq!(f.snapshot(), before, "latched reuse altered hostile {arm}");
    }
}
#[test]
fn atomic_source_latched_confirmation_refuses_changed_authority_without_changes() {
    for arm in ATOMIC_HOSTILE {
        let mut f = AtomicPending::new(false);
        f.corrupt(arm);
        let before = f.snapshot();
        let result = f.journal.acknowledge_identity_handoff(
            f.upload.id,
            f.upload.attempt.unwrap(),
            current_temp(),
            backup(old_temp()),
        );
        assert!(
            result.is_err(),
            "latched confirmation accepted hostile {arm}"
        );
        assert_eq!(
            f.snapshot(),
            before,
            "latched confirmation altered hostile {arm}"
        );
    }
}
#[test]
fn atomic_takeover_after_earlier_reservation_cannot_hide_missing_replacement_authority() {
    for confirm in [false, true] {
        let mut f = AtomicPending::new(true);
        // The earlier reservation has no atomic_source latch. A genuine atomic
        // cleanup edge must not become an ordinary newer-save parity exception.
        let value = serde_json::to_value(f.journal.get(f.upload.id).unwrap()).unwrap();
        assert!(value["identity_handoff"]["atomic_source"].is_null());
        f.corrupt("missing_replacement");
        let before = f.snapshot();
        let result = if confirm {
            f.journal.acknowledge_identity_handoff(
                f.upload.id,
                f.upload.attempt.unwrap(),
                current_temp(),
                backup(old_temp()),
            )
        } else {
            f.journal
                .reserve_identity_handoff(
                    f.upload.id,
                    f.upload.attempt.unwrap(),
                    control_location(f.upload.id),
                )
                .map(|_| ())
        };
        assert!(
            result.is_err(),
            "pre-takeover reservation lost authority; confirm={confirm}"
        );
        assert_eq!(f.snapshot(), before);
    }
}
#[test]
fn ordinary_reserved_upload_with_newer_queued_save_still_confirms_its_own_receipt() {
    let root = tempfile::Builder::new()
        .prefix("cirrove-reserved-newer-save-")
        .tempdir()
        .unwrap()
        .keep()
        .join("journal");
    let mut j = UploadJournal::open(&root, "fixture", 1024 * 1024).unwrap();
    let file = j
        .create_working(scope(), old_temp(), true, &b""[..])
        .unwrap();
    j.seal_working(file.id).unwrap().unwrap();
    let empty = j.claim_next().unwrap().unwrap();
    j.acknowledge(empty.id, empty.attempt.unwrap(), old_temp())
        .unwrap();
    j.write_working(file.id, 0, CONTENT).unwrap();
    j.seal_working(file.id).unwrap().unwrap();
    let upload = j.claim_next().unwrap().unwrap();
    let recovery = j
        .reserve_identity_handoff(
            upload.id,
            upload.attempt.unwrap(),
            control_location(upload.id),
        )
        .unwrap();
    let newer = b"newer-open-descriptor-save";
    j.write_working(file.id, 0, newer).unwrap();
    j.truncate_working(file.id, newer.len() as u64).unwrap();
    let next = j.seal_working(file.id).unwrap().unwrap();
    assert_eq!(next.base.as_ref().unwrap().predecessor, upload.id);
    assert!(!next.base.as_ref().unwrap().resolved);
    let owner = j.namespace_for_operation(upload.id).unwrap().unwrap();
    assert_eq!(owner.latest, Some(next.id));
    assert_eq!(
        j.reserve_identity_handoff(
            upload.id,
            upload.attempt.unwrap(),
            control_location(upload.id)
        )
        .unwrap(),
        recovery
    );
    j.acknowledge_identity_handoff(
        upload.id,
        upload.attempt.unwrap(),
        current_temp(),
        backup(old_temp()),
    )
    .unwrap();
    let owner = j.namespace_object(owner.id).unwrap();
    assert_eq!(owner.latest, Some(next.id));
    assert_eq!(owner.remote, Some(current_temp()));
    assert_eq!(j.get(upload.id).unwrap().state, UploadState::Uploaded);
    assert_eq!(j.get(next.id).unwrap().state, UploadState::Pending);
    assert_eq!(payload(&j, upload.id), CONTENT);
    assert_eq!(payload(&j, next.id), newer);
    assert_eq!(
        j.read_working(file.id, 0, newer.len() as u32).unwrap(),
        newer
    );
}

#[test]
fn atomic_source_reservation_survives_exclusive_reopen_without_replay() {
    let f = AtomicPending::new(false);
    let body = serde_json::to_value(f.journal.get(f.upload.id).unwrap()).unwrap();
    let recovery = body["identity_handoff"]["recovery_object"].clone();
    assert_eq!(
        body["identity_handoff"]["atomic_source"],
        serde_json::json!(f.target)
    );
    drop(f.journal);
    let mut j = UploadJournal::open(&f.root, "fixture", 1024 * 1024).unwrap();
    assert_eq!(
        j.get(f.upload.id).unwrap().state,
        UploadState::VerifyRequired
    );
    let verification = j.claim_verification(f.upload.id).unwrap();
    assert_eq!(verification.state, UploadState::Verifying);
    let attempt = verification.attempt.unwrap();
    let reserved = j
        .reserve_identity_handoff(f.upload.id, attempt, control_location(f.upload.id))
        .unwrap();
    assert_eq!(serde_json::json!(reserved), recovery);
    j.acknowledge_identity_handoff(f.upload.id, attempt, current_temp(), backup(old_temp()))
        .unwrap();
    assert_eq!(j.get(f.upload.id).unwrap().state, UploadState::Uploaded);
    assert_eq!(j.get(f.target).unwrap().state, UploadState::Pending);
    let owner = j.namespace_object(f.owner).unwrap();
    assert_eq!(owner.latest, Some(f.target));
    assert_eq!(owner.node.name, "final.xlsx");
    assert_eq!(owner.remote, Some(current_temp()));
    assert_eq!(payload(&j, f.original), ORIGINAL);
    assert_eq!(payload(&j, f.upload.id), CONTENT);
    assert_eq!(payload(&j, f.target), CONTENT);
    assert_eq!(
        j.read_working(f.working, 0, CONTENT.len() as u32).unwrap(),
        CONTENT
    );
}

#[test]
fn atomic_source_and_target_handoffs_preserve_later_dirty_descriptor_bytes() {
    let mut f = AtomicPending::new(false);
    let dirty = b"unsaved descriptor change after path takeover";
    f.journal.write_working(f.working, 0, dirty).unwrap();
    f.journal
        .truncate_working(f.working, dirty.len() as u64)
        .unwrap();
    let before = f.journal.working_file(f.working).unwrap();
    assert!(before.dirty);
    assert_eq!(before.latest, Some(f.target));
    assert_eq!(before.node.name, "final.xlsx");
    f.journal
        .acknowledge_identity_handoff(
            f.upload.id,
            f.upload.attempt.unwrap(),
            current_temp(),
            backup(old_temp()),
        )
        .unwrap();
    assert_eq!(f.journal.working_file(f.working).unwrap().node, before.node);
    let target = f.journal.claim_next().unwrap().unwrap();
    assert_eq!(target.id, f.target);
    let attempt = target.attempt.unwrap();
    f.journal
        .reserve_identity_handoff(target.id, attempt, control_location(target.id))
        .unwrap();
    f.journal
        .acknowledge_identity_handoff(
            target.id,
            attempt,
            current_final("source-parent"),
            backup(old_final("source-parent")),
        )
        .unwrap();
    let cleanup = f.journal.claim_mutation().unwrap().unwrap();
    assert_eq!(cleanup.id, f.cleanup);
    assert!(
        matches!(&cleanup.request.intent, MutationIntent::RemoveFile { before } if before == &current_temp())
    );
    f.journal
        .acknowledge_mutation(
            cleanup.id,
            cleanup.attempt.unwrap(),
            MutationReceipt::Removed {
                item: current_temp().id,
            },
        )
        .unwrap();
    assert_eq!(f.journal.working_file(f.working).unwrap().node, before.node);
    assert_eq!(
        f.journal
            .read_working(f.working, 0, dirty.len() as u32)
            .unwrap(),
        dirty
    );
    assert_eq!(payload(&f.journal, f.upload.id), CONTENT);
    assert_eq!(payload(&f.journal, f.target), CONTENT);
    drop(f.journal);
    let mut j = UploadJournal::open(&f.root, "fixture", 1024 * 1024).unwrap();
    let working = j.working_file(f.working).unwrap();
    assert!(working.dirty);
    assert_eq!(working.node, before.node);
    assert_eq!(
        j.namespace_object(f.owner).unwrap().remote,
        Some(current_final("source-parent"))
    );
    assert_eq!(j.mutation(f.cleanup).unwrap().state, MutationState::Applied);
    assert_eq!(
        j.read_working(f.working, 0, dirty.len() as u32).unwrap(),
        dirty
    );
    assert_eq!(payload(&j, f.original), ORIGINAL);
    assert_eq!(payload(&j, f.upload.id), CONTENT);
    assert_eq!(payload(&j, f.target), CONTENT);
    let later = j.seal_working(f.working).unwrap().unwrap();
    assert_eq!(later.base.as_ref().unwrap().predecessor, f.target);
    assert_eq!(payload(&j, later.id), dirty);
}
