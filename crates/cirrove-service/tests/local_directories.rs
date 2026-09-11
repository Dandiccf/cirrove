//! Folder receipts supply destinations, never a child's file identity/version.
#![allow(clippy::unwrap_used)]
use cirrove_core::{
    Node, NodeKind, Scope,
    mutation::{MutationIntent, MutationReceipt},
    upload::UploadIntent,
};
use cirrove_service::journal::{
    JournalError, MutationState, NamespaceObject, UploadJournal, UploadRecord,
};
use std::{
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn scope() -> Scope {
    Scope {
        account: "folders".into(),
        provider: "fixture".into(),
        collection: "drive".into(),
    }
}
fn open(path: &Path) -> UploadJournal {
    let until = Instant::now() + Duration::from_secs(2);
    loop {
        match UploadJournal::open(path, &scope().account, 1024 * 1024) {
            Err(JournalError::Busy) if Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(2))
            }
            r => return r.unwrap(),
        }
    }
}
fn folder(id: &str, parent: &str, name: &str) -> Node {
    Node {
        id: id.into(),
        parent_id: Some(parent.into()),
        name: name.into(),
        kind: NodeKind::Folder,
        size: 0,
        etag: Some(format!("etag-{id}")),
        content_version: None,
        modified_unix: 1,
        target: None,
    }
}
fn child(j: &mut UploadJournal, parent: &str, name: &str) -> UploadRecord {
    let mut node = folder("", parent, name);
    node.kind = NodeKind::File;
    node.etag = None;
    let working = j.create_working(scope(), node, true, &b""[..]).unwrap();
    j.write_working(working.id, 0, b"data").unwrap();
    j.seal_working(working.id).unwrap().unwrap()
}
fn uploaded(j: &mut UploadJournal, record: &UploadRecord, id: &str) -> Node {
    let UploadIntent::Create { parent, name } = &record.intent else {
        panic!("expected create")
    };
    let mut node = folder(id, parent, name);
    node.kind = NodeKind::File;
    node.size = record.size;
    node.content_version = Some(format!("content-{id}"));
    j.acknowledge(record.id, record.attempt.unwrap(), node.clone())
        .unwrap();
    node
}
fn ack_folder(j: &mut UploadJournal, object: &NamespaceObject, id: &str) -> Node {
    let record = j.claim_mutation().unwrap().unwrap();
    assert_eq!(Some(record.id), object.latest);
    let MutationIntent::CreateFolder { parent, name } = &record.request.intent else {
        panic!("expected folder")
    };
    let node = folder(id, parent, name);
    j.acknowledge_mutation(
        record.id,
        record.attempt.unwrap(),
        MutationReceipt::Upsert(node.clone()),
    )
    .unwrap();
    node
}

#[test]
fn nested_directories_and_siblings_share_parent_confirmation_without_sharing_save_bases() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let mut j = open(&root);
    let top = j
        .create_namespace_directory(scope(), "root".into(), "Grüße".into())
        .unwrap();
    let nested = j
        .create_namespace_directory(scope(), top.node.id.clone(), "nested".into())
        .unwrap();
    let first = child(&mut j, &top.node.id, "first.txt");
    let second = child(&mut j, &top.node.id, "second.txt");
    let deep = child(&mut j, &nested.node.id, "deep.txt");
    let independent = child(&mut j, "root", "independent.txt");
    let run = j.claim_next().unwrap().unwrap();
    assert_eq!(run.id, independent.id);
    uploaded(&mut j, &run, "root-file");
    assert!(j.claim_next().unwrap().is_none());
    let remote_top = ack_folder(&mut j, &top, "cloud-top");
    let a = j.claim_next().unwrap().unwrap();
    let b = j.claim_next().unwrap().unwrap();
    assert_eq!(a.id, first.id);
    assert_eq!(b.id, second.id);
    assert!(a.base.is_none() && b.base.is_none());
    assert!(matches!(&a.intent,UploadIntent::Create{parent,..} if parent=="cloud-top"));
    let remote_a = uploaded(&mut j, &a, "file-a");
    uploaded(&mut j, &b, "file-b");
    let remote_nested = ack_folder(&mut j, &nested, "cloud-nested");
    assert_eq!(remote_nested.parent_id.as_deref(), Some("cloud-top"));
    let c = j.claim_next().unwrap().unwrap();
    assert_eq!(c.id, deep.id);
    assert!(matches!(&c.intent,UploadIntent::Create{parent,..} if parent=="cloud-nested"));
    uploaded(&mut j, &c, "file-c");
    let object = j.namespace_by_remote(&scope(), "file-a").unwrap().unwrap();
    assert_eq!(object.node.parent_id.as_ref(), Some(&top.node.id));
    assert_eq!(
        object.remote.as_ref().unwrap().parent_id.as_deref(),
        Some("cloud-top")
    );
    let following = j
        .handoff_namespace(object.id, object.revision, remote_a.clone())
        .unwrap();
    assert_eq!(following.node.parent_id.as_ref(), Some(&top.node.id));
    let listing = j
        .namespace_overlay(&scope(), &top.node.id, vec![remote_a])
        .unwrap();
    let file = listing
        .nodes
        .iter()
        .find(|n| n.name == "first.txt")
        .unwrap();
    assert_eq!(file.id, object.node.id);
    assert_eq!(file.parent_id.as_ref(), Some(&top.node.id));
    let object = j.namespace_object(top.id).unwrap();
    j.handoff_namespace(object.id, object.revision, remote_top)
        .unwrap();
    let newer = j.enqueue_after(first.id, &b"new"[..]).unwrap();
    let claim = j.claim_next().unwrap().unwrap();
    assert_eq!(claim.id, newer.id);
    assert!(matches!(&claim.intent,UploadIntent::Replace{item,..} if item=="file-a"));
}

#[test]
fn uncertain_parent_survives_restart_and_blocks_children_but_not_other_trees() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let mut j = open(&root);
    let parent = j
        .create_namespace_directory(scope(), "root".into(), "pending".into())
        .unwrap();
    let save = child(&mut j, &parent.node.id, "child.txt");
    let first = j.claim_mutation().unwrap().unwrap();
    assert_eq!(Some(first.id), parent.latest);
    drop(j);
    let mut j = open(&root);
    assert!(j.claim_next().unwrap().is_none());
    let verifying = j.claim_mutation().unwrap().unwrap();
    assert_eq!(verifying.state, MutationState::Verifying);
    j.defer_mutation(
        verifying.id,
        verifying.attempt.unwrap(),
        MutationState::NeedsReview,
        Duration::ZERO,
    )
    .unwrap();
    let independent = child(&mut j, "root", "other.txt");
    let ready = j.claim_next().unwrap().unwrap();
    assert_eq!(ready.id, independent.id);
    assert_eq!(j.get(save.id).unwrap().size, 4);
    assert_eq!(
        j.namespace_overlay(&scope(), &parent.node.id, vec![])
            .unwrap()
            .nodes
            .len(),
        1
    );
    drop(j);
    let mut j = open(&root);
    assert!(j.claim_next().unwrap().is_none());
    assert_eq!(
        j.mutation(first.id).unwrap().state,
        MutationState::NeedsReview
    );
    assert!(j.payload(save.id).is_ok());
}

#[test]
fn destination_rebinding_reserves_real_parent_before_younger_colliding_create() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let mut j = open(&root);
    let parent = j
        .create_namespace_directory(scope(), "root".into(), "parent".into())
        .unwrap();
    let older = child(&mut j, &parent.node.id, "same.txt");
    let younger = j
        .enqueue(
            scope(),
            UploadIntent::Create {
                parent: "cloud-parent".into(),
                name: "same.txt".into(),
            },
            &b"other"[..],
        )
        .unwrap();
    ack_folder(&mut j, &parent, "cloud-parent");
    let claimed = j.claim_next().unwrap().unwrap();
    assert_eq!(claimed.id, older.id);
    assert!(j.claim_next().unwrap().is_none());
    uploaded(&mut j, &claimed, "older-file");
    assert_eq!(j.claim_next().unwrap().unwrap().id, younger.id);
}

#[test]
fn directory_transaction_failure_keeps_existing_tree_and_creates_no_partial_child() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let mut j = open(&root);
    let parent = j
        .create_namespace_directory(scope(), "root".into(), "parent".into())
        .unwrap();
    let db = rusqlite::Connection::open(root.join("uploads.db")).unwrap();
    db.execute_batch("CREATE TRIGGER reject_destination BEFORE INSERT ON write_destinations BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(
        j.create_namespace_directory(scope(), parent.node.id.clone(), "child".into())
            .is_err()
    );
    assert_eq!(j.namespace_objects().unwrap().len(), 1);
    assert_eq!(j.list_mutations(0, 100).unwrap().len(), 1);
    assert!(
        j.namespace_overlay(&scope(), &parent.node.id, vec![])
            .unwrap()
            .nodes
            .is_empty()
    );
    db.execute_batch("DROP TRIGGER reject_destination").unwrap();
    j.create_namespace_directory(scope(), parent.node.id.clone(), "child".into())
        .unwrap();
    assert!(
        j.create_namespace_directory(scope(), parent.node.id.clone(), "CHILD".into())
            .is_err()
    );
    assert_eq!(j.namespace_objects().unwrap().len(), 2);
}

#[test]
fn schema_thirteen_migrates_and_missing_or_future_destination_state_fails_closed() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let mut j = open(&root);
    let save = child(&mut j, "root", "existing.txt");
    drop(j);
    let db = rusqlite::Connection::open(root.join("uploads.db")).unwrap();
    db.execute_batch("DROP TABLE write_destinations; PRAGMA user_version=13;")
        .unwrap();
    drop(db);
    let j = open(&root);
    assert!(j.payload(save.id).is_ok());
    drop(j);
    let db = rusqlite::Connection::open(root.join("uploads.db")).unwrap();
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        14
    );
    db.execute_batch("DROP TABLE write_destinations;").unwrap();
    assert!(UploadJournal::open(&root, &scope().account, 1024 * 1024).is_err());
    db.execute_batch("PRAGMA user_version=15;").unwrap();
    assert!(matches!(
        UploadJournal::open(&root, &scope().account, 1024 * 1024),
        Err(JournalError::Schema)
    ));
}

#[test]
fn unrelated_folder_receipt_cannot_supply_a_child_destination() {
    use cirrove_service::journal::UploadState;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("journal");
    let mut j = open(&root);
    let parent = j
        .create_namespace_directory(scope(), "root".into(), "parent".into())
        .unwrap();
    let unrelated = j
        .create_namespace_directory(scope(), "root".into(), "other".into())
        .unwrap();
    ack_folder(&mut j, &parent, "real-parent");
    ack_folder(&mut j, &unrelated, "unrelated-folder");
    let save = child(&mut j, &parent.node.id, "keep.txt");
    let db = rusqlite::Connection::open(root.join("uploads.db")).unwrap();
    // Even a completed, earlier folder operation in the same scope must actually
    // belong to the captured local parent. Never rebase using unrelated evidence.
    db.execute(
        "UPDATE write_destinations SET predecessor=?1 WHERE operation=?2",
        rusqlite::params![unrelated.latest.unwrap().to_string(), save.id.to_string()],
    )
    .unwrap();
    assert!(j.claim_next().unwrap().is_none());
    assert_eq!(j.get(save.id).unwrap().state, UploadState::Failed);
    assert!(j.payload(save.id).is_ok());
    let independent = child(&mut j, "root", "still-usable.txt");
    assert_eq!(j.claim_next().unwrap().unwrap().id, independent.id);
}

#[test]
fn moving_a_new_file_waits_for_both_its_upload_and_its_destination_folder() {
    for folder_first in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("journal");
        let mut j = open(&root);
        let parent = j
            .create_namespace_directory(scope(), "root".into(), "destination".into())
            .unwrap();
        let save = child(&mut j, "root", "first.txt");
        let object = j.namespace_for_operation(save.id).unwrap().unwrap();
        let movement = j
            .relocate_namespace_file(
                object.id,
                object.revision,
                parent.node.id.clone(),
                "moved.txt".into(),
            )
            .unwrap();
        let later = j
            .enqueue_after(movement.id, &b"edited after moving"[..])
            .unwrap();
        if folder_first {
            ack_folder(&mut j, &parent, "cloud-parent");
        }
        let claimed = j.claim_next().unwrap().unwrap();
        assert_eq!(claimed.id, save.id);
        assert!(j.claim_next().unwrap().is_none());
        uploaded(&mut j, &claimed, "cloud-file");
        if !folder_first {
            ack_folder(&mut j, &parent, "cloud-parent");
        }
        let moved = j.claim_mutation().unwrap().unwrap();
        assert_eq!(moved.id, movement.id);
        let MutationIntent::Relocate {
            before,
            parent,
            name,
        } = &moved.request.intent
        else {
            panic!("expected relocation")
        };
        assert_eq!(before.id, "cloud-file");
        assert_eq!(parent, "cloud-parent");
        let mut remote = before.clone();
        remote.parent_id = Some(parent.clone());
        remote.name = name.clone();
        remote.etag = Some("moved-etag".into());
        j.acknowledge_mutation(
            moved.id,
            moved.attempt.unwrap(),
            MutationReceipt::Upsert(remote),
        )
        .unwrap();
        let claimed = j.claim_next().unwrap().unwrap();
        assert_eq!(claimed.id, later.id);
        assert!(
            matches!(&claimed.intent,UploadIntent::Replace{item,expected_etag} if item=="cloud-file" && expected_etag=="moved-etag")
        );
    }
}

/// Child for the crash test: creates a pending folder with a child waiting on it,
/// stops at the requested durable transition, then waits to be killed.
#[test]
#[ignore = "subprocess fixture; activated only by its parent test"]
fn directories_crash_child() {
    let root = std::env::var("CIRROVE_DIRECTORIES_FIXTURE_ROOT").unwrap();
    let root = Path::new(&root);
    let mut j = open(&root.join("journal"));
    let top = j
        .create_namespace_directory(scope(), "root".into(), "Grüße".into())
        .unwrap();
    let waiting = child(&mut j, &top.node.id, "waiting.txt");
    if std::env::var("CIRROVE_DIRECTORIES_FIXTURE_PHASE").unwrap() == "confirmed" {
        ack_folder(&mut j, &top, "cloud-top");
        // The destination is resolved by the claim that follows confirmation, not
        // by the confirmation itself: another durable transition, reached here.
        let _ = j.claim_next().unwrap();
    }
    std::fs::write(
        root.join("reached"),
        cirrove_service::journal::durable::reached().join("\n"),
    )
    .unwrap();
    std::fs::write(
        root.join("ready"),
        format!("{} {}", top.node.id, waiting.id),
    )
    .unwrap();
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// A killed process must keep a pending folder and whatever is waiting on it
/// together, in whichever state the last durable write left them.
///
/// A local folder exists before the cloud has one, and its children cannot be
/// uploaded until it is confirmed, because their destination is its cloud id.
/// A crash before confirmation must leave the child unclaimable rather than
/// uploading it to a guessed parent; a crash after confirmation must not make the
/// child wait forever for a folder that already exists remotely.
#[test]
fn actual_process_death_keeps_a_pending_folder_and_its_waiting_child_consistent() {
    for phase in ["pending", "confirmed"] {
        let temp = tempfile::tempdir().unwrap();
        let mut child_process = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "directories_crash_child", "--ignored"])
            .env("CIRROVE_DIRECTORIES_FIXTURE_ROOT", temp.path())
            .env("CIRROVE_DIRECTORIES_FIXTURE_PHASE", phase)
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !temp.path().join("ready").exists() {
            if Instant::now() >= deadline {
                child_process.kill().unwrap();
                child_process.wait().unwrap();
                panic!("directories fixture did not become ready");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        child_process.kill().unwrap();
        child_process.wait().unwrap();

        let ready = std::fs::read_to_string(temp.path().join("ready")).unwrap();
        let waiting: uuid::Uuid = ready.split_whitespace().nth(1).unwrap().parse().unwrap();
        let mut j = open(&temp.path().join("journal"));
        if phase == "confirmed" {
            // The child claimed it before dying, so the crash must leave it
            // claimed against the folder's real cloud id rather than losing the
            // destination or reverting to a guess.
            let record = j.get(waiting).unwrap();
            assert!(
                matches!(&record.intent, UploadIntent::Create { parent, .. } if parent == "cloud-top"),
                "the destination was lost or guessed: {:?}",
                record.intent
            );
        } else {
            assert!(
                j.claim_next().unwrap().is_none(),
                "a child was claimable before its folder existed remotely"
            );
        }
    }
}

/// `rmdir` is not recursive, and the journal is the only place that can see a
/// child which exists nowhere else yet.
///
/// The provider cannot help here: a locally created directory or file has no
/// remote identity to list, so an emptiness check against Graph would report the
/// parent empty and the recursive DELETE would take the pending child with it.
/// Dropping the `namespace_objects` scan in `remove_namespace_directory` makes
/// the first two removals below succeed.
#[test]
fn a_directory_is_not_removed_while_a_purely_local_child_still_names_it() {
    let temp = tempfile::tempdir().unwrap();
    let mut j = open(&temp.path().join("journal"));
    let parent = j
        .create_namespace_directory(scope(), "root".into(), "Grüße".into())
        .unwrap();
    let nested = j
        .create_namespace_directory(scope(), parent.node.id.clone(), "nested".into())
        .unwrap();

    // A pending child directory holds the parent.
    assert!(matches!(
        j.remove_namespace_directory(parent.id, parent.revision),
        Err(JournalError::Intent)
    ));

    // The empty child is refused too, for a different reason worth stating: a
    // directory whose creation has not settled cannot carry a conditional
    // removal. Chaining one behind its own creation was tried and is wrong --
    // Graph moves a folder's eTag between the create response and a moment
    // later, so the DELETE loses its precondition and lands in Conflict after
    // rmdir already reported success. `Writeback::rmdir` refuses the whole
    // window with EBUSY rather than letting this surface as a malformed request;
    // `writable_session::real_a_directory_created_and_removed_again_is_gone_from_the_provider`
    // holds the outcome that made the difference visible.
    assert!(nested.remote.is_none());
    assert!(matches!(
        j.remove_namespace_directory(nested.id, nested.revision),
        Err(JournalError::Intent)
    ));

    // A pending child file holds it as well.
    child(&mut j, &parent.node.id, "held.txt");
    assert!(matches!(
        j.remove_namespace_directory(parent.id, parent.revision),
        Err(JournalError::Intent)
    ));

    // Every refusal left the parent alone: a rejected rmdir must not consume the
    // revision it checked, or the retry after the child is gone would be stale.
    let unchanged = j.namespace_object(parent.id).unwrap();
    assert!(!unchanged.unlinked);
    assert_eq!(unchanged.revision, parent.revision);
}
