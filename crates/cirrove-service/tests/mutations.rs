//! Namespace ordering, durable recovery and worker faults; no cloud access.
#![allow(clippy::unwrap_used)]
use async_trait::async_trait;
use cirrove_core::mutation::*;
use cirrove_core::{CancellationToken, Node, NodeKind, Scope};
use cirrove_service::journal::{JournalError, MutationState, UploadIntent, UploadJournal};
use cirrove_service::mutations::MutationWorker;
use std::{
    process::{Command, Stdio},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
fn scope() -> Scope {
    Scope {
        account: "fixture".into(),
        provider: "onedrive".into(),
        collection: "drive".into(),
    }
}
fn before() -> Node {
    Node {
        id: "file".into(),
        parent_id: Some("source".into()),
        name: "old.txt".into(),
        kind: NodeKind::File,
        size: 5,
        modified_unix: 0,
        etag: Some("original".into()),
        content_version: Some("content".into()),
        target: None,
    }
}
fn request() -> MutationRequest {
    MutationRequest {
        scope: scope(),
        intent: MutationIntent::Relocate {
            before: before(),
            parent: "destination".into(),
            name: "Grüße.txt".into(),
        },
    }
}
fn receipt(request: &MutationRequest) -> MutationReceipt {
    match &request.intent {
        MutationIntent::Relocate {
            before,
            parent,
            name,
        } => {
            let mut node = before.clone();
            node.parent_id = Some(parent.clone());
            node.name = name.clone();
            node.etag = Some("new".into());
            MutationReceipt::Upsert(node)
        }
        MutationIntent::CreateFolder { parent, name } => {
            let mut node = before();
            node.id = "new-folder".into();
            node.kind = NodeKind::Folder;
            node.parent_id = Some(parent.clone());
            node.name = name.clone();
            MutationReceipt::Upsert(node)
        }
        MutationIntent::RemoveFile { before } | MutationIntent::RemoveFolder { before } => {
            MutationReceipt::Removed {
                item: before.id.clone(),
            }
        }
    }
}
fn journal(root: &std::path::Path) -> UploadJournal {
    // This helper opens fresh/released owners. A concurrent fork can briefly
    // retain a CLOEXEC descriptor until exec; held-owner exclusion is tested
    // separately with direct opens and must still fail immediately.
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match UploadJournal::open(root, "fixture", 1024 * 1024) {
            Err(JournalError::Busy) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(2))
            }
            result => return result.unwrap(),
        }
    }
}
#[test]
fn uploads_and_namespace_changes_share_item_and_destination_ordering() {
    let tmp = tempfile::tempdir().unwrap();
    let mut j = journal(&tmp.path().join("journal"));
    let upload = j
        .enqueue(
            scope(),
            UploadIntent::Replace {
                item: "file".into(),
                expected_etag: "original".into(),
            },
            b"hello".as_slice(),
        )
        .unwrap();
    let mutation = j.enqueue_mutation(request()).unwrap();
    let collision = j
        .enqueue(
            scope(),
            UploadIntent::Create {
                parent: "destination".into(),
                name: "GRÜßE.txt".into(),
            },
            b"hello".as_slice(),
        )
        .unwrap();
    let unrelated = j
        .enqueue(
            scope(),
            UploadIntent::Create {
                parent: "source".into(),
                name: "unrelated.txt".into(),
            },
            b"hello".as_slice(),
        )
        .unwrap();
    assert!(j.claim_mutation().unwrap().is_none());
    let active = j.claim_next().unwrap().unwrap();
    assert_eq!(active.id, upload.id);
    assert_eq!(j.claim_next().unwrap().unwrap().id, unrelated.id);
    assert!(j.claim_next().unwrap().is_none());
    let mut node = before();
    node.etag = Some("uploaded".into());
    j.acknowledge(active.id, active.attempt.unwrap(), node)
        .unwrap();
    let active = j.claim_mutation().unwrap().unwrap();
    assert_eq!(active.id, mutation.id);
    j.acknowledge_mutation(active.id, active.attempt.unwrap(), receipt(&active.request))
        .unwrap();
    assert_eq!(j.claim_next().unwrap().unwrap().id, collision.id);
}
#[test]
fn legacy_queue_migrates_and_stale_mutation_attempts_cannot_acknowledge() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("journal");
    let mut j = journal(&root);
    let upload = j
        .enqueue(
            scope(),
            UploadIntent::Create {
                parent: "source".into(),
                name: "a".into(),
            },
            b"hello".as_slice(),
        )
        .unwrap();
    drop(j);
    let db = rusqlite::Connection::open(root.join("uploads.db")).unwrap();
    db.execute_batch("DROP TABLE write_resources; DROP TABLE write_queue; DROP TABLE mutations; DROP TABLE namespace_operations; DROP TABLE namespace_remote; DROP TABLE namespace_entries; DROP TABLE namespace_objects; DROP TABLE namespace_scopes; PRAGMA user_version=2;").unwrap();
    drop(db);
    let mut j = journal(&root);
    assert_eq!(j.get(upload.id).unwrap().size, 5);
    let mutation = j.enqueue_mutation(request()).unwrap();
    assert!(mutation.sequence > upload.sequence);
    let old = j.claim_mutation().unwrap().unwrap();
    drop(j);
    let mut j = journal(&root);
    assert_eq!(
        j.mutation(old.id).unwrap().state,
        MutationState::VerifyRequired
    );
    let active = j.claim_mutation().unwrap().unwrap();
    assert_ne!(active.attempt, old.attempt);
    assert!(matches!(
        j.acknowledge_mutation(old.id, old.attempt.unwrap(), receipt(&old.request)),
        Err(JournalError::Stale)
    ));
    j.acknowledge_mutation(active.id, active.attempt.unwrap(), receipt(&active.request))
        .unwrap();
    drop(j);
    let j = journal(&root);
    assert_eq!(j.mutation(active.id).unwrap().state, MutationState::Applied);
}
#[test]
fn requests_reject_wildcards_recursive_delete_shortcuts_and_foreign_accounts() {
    let mut r = request();
    if let MutationIntent::Relocate { before, .. } = &mut r.intent {
        before.etag = Some("*".into());
    }
    assert!(r.validate().is_err());
    assert!(
        UploadIntent::Replace {
            item: "file".into(),
            expected_etag: "*".into()
        }
        .validate()
        .is_err()
    );
    let mut folder = before();
    folder.kind = NodeKind::Folder;
    r.intent = MutationIntent::RemoveFile { before: folder };
    assert!(r.validate().is_err());
    r = request();
    if let MutationIntent::Relocate { before, .. } = &mut r.intent {
        before.kind = NodeKind::Shortcut;
    }
    assert!(r.validate().is_err());
    let tmp = tempfile::tempdir().unwrap();
    let mut j = journal(&tmp.path().join("journal"));
    r = request();
    r.scope.account = "other".into();
    assert!(matches!(j.enqueue_mutation(r), Err(JournalError::Account)));
}
struct Provider {
    mode: &'static str,
    mutations: AtomicUsize,
    checks: AtomicUsize,
    journal: Weak<Mutex<UploadJournal>>,
    entered: tokio::sync::Notify,
}
impl Provider {
    fn unlocked(&self) {
        if let Some(j) = self.journal.upgrade() {
            assert!(
                j.try_lock().is_ok(),
                "journal lock held during provider request"
            );
        }
    }
}
#[async_trait]
impl MutationProvider for Provider {
    async fn mutate(&self, r: &MutationRequest, c: &CancellationToken) -> Result<MutationReceipt> {
        self.unlocked();
        self.mutations.fetch_add(1, Ordering::SeqCst);
        if self.mode == "block" {
            self.entered.notify_one();
            c.cancelled().await;
        }
        if self.mode == "conflict" {
            return Err(MutationError::Conflict);
        }
        if self.mode == "success" {
            return Ok(receipt(r));
        }
        Err(MutationError::Uncertain)
    }
    async fn reconcile_mutation(
        &self,
        r: &MutationRequest,
        _: &CancellationToken,
    ) -> Result<MutationReconciliation> {
        self.unlocked();
        self.checks.fetch_add(1, Ordering::SeqCst);
        Ok(if self.mode == "indeterminate" {
            MutationReconciliation::Indeterminate
        } else if self.mode == "changed_content" {
            let MutationReceipt::Upsert(mut node) = receipt(r) else {
                panic!("expected rename fixture")
            };
            node.content_version = Some("external-content".into());
            MutationReconciliation::Applied(MutationReceipt::Upsert(node))
        } else {
            MutationReconciliation::Applied(receipt(r))
        })
    }
}
fn provider(mode: &'static str, j: &Arc<Mutex<UploadJournal>>) -> Arc<Provider> {
    Arc::new(Provider {
        mode,
        mutations: AtomicUsize::new(0),
        checks: AtomicUsize::new(0),
        journal: Arc::downgrade(j),
        entered: tokio::sync::Notify::new(),
    })
}
#[tokio::test]
async fn worker_exposes_content_conflict_after_a_lost_rename_instead_of_acknowledging_a_new_base() {
    let tmp = tempfile::tempdir().unwrap();
    let j = Arc::new(Mutex::new(journal(&tmp.path().join("journal"))));
    let moved = j.lock().unwrap().enqueue_mutation(request()).unwrap();
    let next = j
        .lock()
        .unwrap()
        .enqueue_after(moved.id, b"my saved bytes".as_slice())
        .unwrap();
    let p = provider("changed_content", &j);
    let worker = MutationWorker::new(j.clone(), p.clone(), CancellationToken::new());
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        MutationState::VerifyRequired
    );
    j.lock().unwrap().request_mutation_retry(moved.id).unwrap();
    let result = worker.run_once().await.unwrap().unwrap();
    assert_eq!(result.state, MutationState::Conflict);
    assert!(result.issue.is_some());
    assert!(j.lock().unwrap().claim_next().unwrap().is_none());
    assert_eq!(
        j.lock().unwrap().get(next.id).unwrap().state,
        cirrove_service::journal::UploadState::Pending
    );
    assert_eq!(p.mutations.load(Ordering::SeqCst), 1);
    assert_eq!(p.checks.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn lost_success_reconciles_after_restart_without_a_second_mutation() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("journal");
    let j = Arc::new(Mutex::new(journal(&root)));
    let record = j.lock().unwrap().enqueue_mutation(request()).unwrap();
    let p = provider("lost", &j);
    let worker = MutationWorker::new(j.clone(), p.clone(), CancellationToken::new());
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        MutationState::VerifyRequired
    );
    drop(worker);
    drop(j);
    let j = Arc::new(Mutex::new(journal(&root)));
    j.lock().unwrap().request_mutation_retry(record.id).unwrap();
    let worker = MutationWorker::new(j.clone(), p.clone(), CancellationToken::new());
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        MutationState::Applied
    );
    assert_eq!(p.mutations.load(Ordering::SeqCst), 1);
    assert_eq!(p.checks.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn uncertain_deletion_needs_review_and_does_not_loop_or_block_other_files() {
    let tmp = tempfile::tempdir().unwrap();
    let j = Arc::new(Mutex::new(journal(&tmp.path().join("journal"))));
    let mut r = request();
    r.intent = MutationIntent::RemoveFile { before: before() };
    let record = j.lock().unwrap().enqueue_mutation(r).unwrap();
    let p = provider("indeterminate", &j);
    let worker = MutationWorker::new(j.clone(), p.clone(), CancellationToken::new());
    worker.run_once().await.unwrap();
    j.lock().unwrap().request_mutation_retry(record.id).unwrap();
    assert_eq!(
        worker.run_once().await.unwrap().unwrap().state,
        MutationState::NeedsReview
    );
    assert!(worker.run_once().await.unwrap().is_none());
    let later = j.lock().unwrap().enqueue_mutation(request()).unwrap();
    assert!(worker.run_once().await.unwrap().is_none());
    let mut independent = request();
    if let MutationIntent::Relocate { before, name, .. } = &mut independent.intent {
        before.id = "other".into();
        before.name = "other.txt".into();
        *name = "other.txt".into();
    }
    let other = j.lock().unwrap().enqueue_mutation(independent).unwrap();
    assert_eq!(worker.run_once().await.unwrap().unwrap().id, other.id);
    assert_eq!(
        j.lock().unwrap().mutation(later.id).unwrap().state,
        MutationState::Pending
    );
    assert_eq!(p.mutations.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn cancelled_namespace_call_releases_the_worker_and_retains_uncertainty() {
    let tmp = tempfile::tempdir().unwrap();
    let j = Arc::new(Mutex::new(journal(&tmp.path().join("journal"))));
    j.lock().unwrap().enqueue_mutation(request()).unwrap();
    let p = provider("block", &j);
    let cancel = CancellationToken::new();
    let worker = MutationWorker::new(j, p.clone(), cancel.clone());
    let running = tokio::spawn(async move { worker.run_once().await });
    tokio::time::timeout(Duration::from_secs(3), p.entered.notified())
        .await
        .unwrap();
    cancel.cancel();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), running)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .unwrap()
            .state,
        MutationState::VerifyRequired
    );
}
#[test]
#[ignore = "child process entry point for namespace SIGKILL fixture"]
fn namespace_crash_child() {
    let root = std::env::var("CIRROVE_MUTATION_FIXTURE_ROOT").unwrap();
    let root = std::path::Path::new(&root);
    let mut j = journal(&root.join("journal"));
    let record = j.enqueue_mutation(request()).unwrap();
    let active = j.claim_mutation().unwrap().unwrap();
    let phase = std::env::var("CIRROVE_MUTATION_FIXTURE_PHASE").unwrap();
    if phase == "applied" || phase == "resolved" {
        j.acknowledge_mutation(active.id, active.attempt.unwrap(), receipt(&active.request))
            .unwrap();
    }
    if phase == "resolved" {
        // A successor mutation resolves against its predecessor's receipt, and
        // only the claim that follows performs that resolution. It is the one
        // path through resolve_mutation.
        let mut successor = request();
        if let MutationIntent::Relocate { name, .. } = &mut successor.intent {
            *name = "renamed-again.txt".into();
        }
        j.enqueue_mutation_after(record.id, successor).unwrap();
        let _ = j.claim_mutation().unwrap();
    }
    std::fs::write(
        root.join("reached"),
        cirrove_service::journal::durable::reached().join("\n"),
    )
    .unwrap();
    std::fs::write(root.join("ready"), record.id.to_string()).unwrap();
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}
#[test]
fn actual_sigkill_preserves_pending_outcome_and_acknowledged_receipt() {
    for phase in ["applying", "applied", "resolved"] {
        let tmp = tempfile::tempdir().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "namespace_crash_child", "--ignored"])
            .env("CIRROVE_MUTATION_FIXTURE_ROOT", tmp.path())
            .env("CIRROVE_MUTATION_FIXTURE_PHASE", phase)
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !tmp.path().join("ready").exists() {
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("fixture did not become ready");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        child.kill().unwrap();
        child.wait().unwrap();
        let id = std::fs::read_to_string(tmp.path().join("ready"))
            .unwrap()
            .parse()
            .unwrap();
        let j = journal(&tmp.path().join("journal"));
        let record = j.mutation(id).unwrap();
        assert_eq!(
            record.state,
            if phase == "applying" {
                MutationState::VerifyRequired
            } else {
                MutationState::Applied
            }
        );
        assert_eq!(record.receipt.is_some(), phase != "applying");
    }
}
