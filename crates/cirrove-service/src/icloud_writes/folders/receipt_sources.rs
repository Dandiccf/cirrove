//! A completed staged file receipt can precede the read index's new item ID.
//! These fixtures use real journal admission/handoff/resolution APIs only.
//! Temp roots are retained; planning must neither mutate nor load a session.
#![allow(clippy::unwrap_used)]
use super::*;
use crate::icloud_writes::tests::{fixture, folder};
use crate::journal::{MutationRecord, UploadRecord};
use cirrove_core::upload::RecoveryLocation;
use rusqlite::{Connection, params};
use std::io::Read;

const CONTENT: &[u8] = b"owned editor temp contents";

struct ResolvedRemove {
    retained_root: PathBuf,
    provider: ICloudWriteProvider,
    parent: Node,
    current: Node,
    upload: UploadRecord,
    remove: MutationRecord,
    owner: Uuid,
    working: Uuid,
}

impl ResolvedRemove {
    fn new() -> Self {
        let (temp, provider) = fixture();
        let retained_root = temp.keep();
        let parent = Node {
            id: format!("FOLDER::com.apple.CloudDocs::{}", Uuid::new_v4()),
            ..folder(ROOT_ID)
        };
        let empty = Node {
            id: format!("FILE::com.apple.CloudDocs::{}", Uuid::new_v4()),
            parent_id: Some(parent.id.clone()),
            name: format!("editor-{}.tmp", Uuid::new_v4()),
            kind: NodeKind::File,
            size: 0,
            modified_unix: 0,
            etag: Some("empty-revision".into()),
            content_version: Some("empty-content".into()),
            target: None,
            package: false,
        };
        let current = Node {
            id: format!("FILE::com.apple.CloudDocs::{}", Uuid::new_v4()),
            size: CONTENT.len() as u64,
            etag: Some("handoff-revision".into()),
            content_version: Some("handoff-content".into()),
            ..empty.clone()
        };
        let (upload, remove, owner, working) = {
            let mut journal = provider.journal.lock().unwrap();
            let working = journal
                .create_working(provider.scope.clone(), empty.clone(), true, &b""[..])
                .unwrap();
            journal.seal_working(working.id).unwrap().unwrap();
            let created = journal.claim_next().unwrap().unwrap();
            journal
                .acknowledge(created.id, created.attempt.unwrap(), empty.clone())
                .unwrap();
            assert_eq!(
                journal.get(created.id).unwrap().state,
                UploadState::Uploaded
            );
            journal.write_working(working.id, 0, CONTENT).unwrap();
            journal.seal_working(working.id).unwrap().unwrap();
            let upload = journal.claim_next().unwrap().unwrap();
            assert!(upload.representation.is_file_bytes());
            assert!(upload.base.as_ref().unwrap().resolved);
            let owner = journal.namespace_for_operation(upload.id).unwrap().unwrap();
            let unlinked = journal
                .unlink_namespace_file(owner.id, owner.revision, false)
                .unwrap();
            assert_eq!(
                unlinked.mutation.base.as_ref().unwrap().predecessor,
                upload.id
            );
            assert!(!unlinked.mutation.base.as_ref().unwrap().resolved);
            let recovery = journal
                .reserve_identity_handoff(
                    upload.id,
                    upload.attempt.unwrap(),
                    RecoveryLocation::Trash {
                        local_name: format!("recovery-{}.tmp", upload.id),
                        parent: "FOLDER::com.apple.CloudDocs::TRASH_ROOT".into(),
                    },
                )
                .unwrap();
            let backup = Node {
                parent_id: Some("FOLDER::com.apple.CloudDocs::TRASH_ROOT".into()),
                etag: Some("trash-revision".into()),
                ..empty.clone()
            };
            journal
                .acknowledge_identity_handoff(
                    upload.id,
                    upload.attempt.unwrap(),
                    current.clone(),
                    backup.clone(),
                )
                .unwrap();
            let completed = journal.get(upload.id).unwrap();
            assert_eq!(completed.state, UploadState::Uploaded);
            assert_eq!(completed.remote, Some(current.clone()));
            assert!(completed.ordinary_handoff_receipt().is_some());
            assert_eq!(
                journal.namespace_object(recovery).unwrap().remote,
                Some(backup)
            );
            let remove = journal.claim_mutation().unwrap().unwrap();
            assert_eq!(remove.id, unlinked.mutation.id);
            assert_eq!(remove.state, MutationState::Applying);
            assert!(remove.base.as_ref().unwrap().resolved);
            assert_eq!(remove.base.as_ref().unwrap().predecessor, completed.id);
            assert_eq!(remove.working_file, Some(working.id));
            assert_eq!(remove.request.intent.before(), Some(&current));
            let tombstone = journal.namespace_object(owner.id).unwrap();
            assert!(tombstone.unlinked);
            assert_eq!(tombstone.latest, Some(remove.id));
            assert_eq!(tombstone.remote, Some(current.clone()));
            assert_eq!(
                journal
                    .namespace_for_operation(remove.id)
                    .unwrap()
                    .unwrap()
                    .id,
                owner.id
            );
            (completed, remove, owner.id, working.id)
        };
        let mut store = Store::open(&provider.metadata).unwrap();
        store.observe_node(&provider.scope, &parent).unwrap();
        assert!(store.node(&provider.scope, &current.id).unwrap().is_none());
        assert_eq!(
            store.node(&provider.scope, &parent.id).unwrap(),
            Some(parent.clone())
        );
        Self {
            retained_root,
            provider,
            parent,
            current,
            upload,
            remove,
            owner,
            working,
        }
    }

    fn db(&self) -> Connection {
        Connection::open(self.retained_root.join("journal/uploads.db")).unwrap()
    }

    fn edit(&self, table: &str, id: Uuid, f: impl FnOnce(&mut serde_json::Value)) {
        assert!(matches!(
            table,
            "uploads" | "mutations" | "namespace_objects" | "working_files"
        ));
        let db = self.db();
        let raw: String = db
            .query_row(
                &format!("SELECT body FROM {table} WHERE id=?1"),
                [id.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        let mut body: serde_json::Value = serde_json::from_str(&raw).unwrap();
        f(&mut body);
        assert_eq!(
            db.execute(
                &format!("UPDATE {table} SET body=?2 WHERE id=?1"),
                params![id.to_string(), serde_json::to_string(&body).unwrap()],
            )
            .unwrap(),
            1
        );
    }

    fn snapshot(&self) -> (Vec<Vec<String>>, Vec<u8>, Vec<u8>, Vec<Node>) {
        let db = self.db();
        let mut rows: Vec<Vec<String>> = [
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
                .query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap()
        })
        .collect();
        rows.push(Connection::open(&self.provider.metadata).unwrap().prepare(
            "SELECT json_array(scope,id,source_revision) FROM observed_absent ORDER BY scope,id"
        ).unwrap().query_map([], |r| r.get::<_, String>(0)).unwrap()
            .collect::<std::result::Result<Vec<_>, _>>().unwrap());
        let mut payload = Vec::new();
        self.provider
            .journal
            .lock()
            .unwrap()
            .payload(self.upload.id)
            .unwrap()
            .read_to_end(&mut payload)
            .unwrap();
        let working = std::fs::read(
            self.retained_root
                .join("journal/working")
                .join(self.working.to_string()),
        )
        .unwrap();
        let visible = Store::open(&self.provider.metadata)
            .unwrap()
            .visible_nodes(&self.provider.scope)
            .unwrap();
        (rows, payload, working, visible)
    }
}

#[tokio::test]
async fn resolved_remove_uses_completed_file_receipt_before_metadata_refresh() {
    let f = ResolvedRemove::new();
    assert_eq!(
        f.provider
            .mutation_operation(&f.remove.id.to_string(), &f.remove.request)
            .unwrap(),
        f.remove.id
    );
    assert_eq!(f.snapshot().1, CONTENT);
    let before = f.snapshot();
    let result = f
        .provider
        .folder_plan_for_operation(f.remove.id, &f.remove.request)
        .await;
    assert_eq!(
        f.snapshot(),
        before,
        "planning changed owned journal, payload or index"
    );
    let plan = result.expect(
        "completed direct FileBytes receipt must authorize its resolved Remove before listing",
    );
    assert!(plan.request == f.remove.request);
    assert!(plan.destination.is_none() && plan.original_sha256.is_none());
    assert!(
        f.provider.folder_adapter(&plan).is_err(),
        "independent digest preflight remains mandatory"
    );
}

#[tokio::test]
async fn resolved_remove_existing_index_must_match_completed_receipt() {
    let f = ResolvedRemove::new();
    let listed = Node {
        content_version: None,
        ..f.current.clone()
    };
    Store::open(&f.provider.metadata)
        .unwrap()
        .observe_node(&f.provider.scope, &listed)
        .unwrap();
    let before = f.snapshot();
    let plan = f
        .provider
        .folder_plan_for_operation(f.remove.id, &f.remove.request)
        .await
        .unwrap();
    assert!(plan.request == f.remove.request);
    assert_eq!(f.snapshot(), before);
    for changed in [
        Node {
            etag: Some("foreign-revision".into()),
            ..listed.clone()
        },
        Node {
            size: listed.size + 1,
            ..listed.clone()
        },
        Node {
            name: "foreign-name.tmp".into(),
            ..listed.clone()
        },
        Node {
            parent_id: Some(ROOT_ID.into()),
            ..listed.clone()
        },
        Node {
            package: true,
            ..listed.clone()
        },
    ] {
        Store::open(&f.provider.metadata)
            .unwrap()
            .observe_node(&f.provider.scope, &changed)
            .unwrap();
        let before = f.snapshot();
        let result = f
            .provider
            .folder_plan_for_operation(f.remove.id, &f.remove.request)
            .await;
        assert_eq!(f.snapshot(), before);
        assert!(
            result.is_err(),
            "receipt fallback overrode an indexed mismatch"
        );
    }
}

#[tokio::test]
async fn resolved_remove_receipt_source_refuses_foreign_request_parent_and_lineage() {
    use serde_json::json;
    for arm in [
        "before_etag",
        "before_size",
        "before_name",
        "before_parent",
        "foreign_scope",
        "foreign_operation",
        "parent_package",
        "parent_file",
        "parent_disconnected",
        "parent_cycle",
        "parent_no_parent",
        "parent_missing",
        "observed_absent",
        "missing_base",
        "unresolved_base",
        "wrong_predecessor",
        "missing_edge",
        "wrong_edge",
        "missing_remove_owner",
        "wrong_remove_owner",
        "missing_upload_owner",
        "wrong_working",
        "upload_not_completed",
        "upload_scope",
        "upload_remote",
        "owner_remote",
        "remove_state_column",
        "remove_sequence_column",
        "remove_queue_complete",
        "remove_queue_sequence",
        "upload_state_column",
        "upload_sequence_column",
        "upload_queue_incomplete",
        "upload_queue_sequence",
        "upload_package",
        "upload_replace_mismatch",
        "working_native",
        "upload_working",
    ] {
        let f = ResolvedRemove::new();
        let mut request = f.remove.request.clone();
        let mut operation = f.remove.id;
        let foreign = Uuid::new_v4();
        match arm {
            "before_etag" | "before_size" | "before_name" | "before_parent" => {
                let MutationIntent::RemoveFile { before } = &mut request.intent else { panic!("remove") };
                match arm {
                    "before_etag" => before.etag = Some("foreign-revision".into()),
                    "before_size" => before.size += 1,
                    "before_name" => before.name = "foreign-name.tmp".into(),
                    _ => before.parent_id = Some(ROOT_ID.into()),
                }
            }
            "foreign_scope" => request.scope.collection = "foreign-drive".into(),
            "foreign_operation" => operation = foreign,
            "parent_package" | "parent_file" | "parent_disconnected" | "parent_cycle" | "parent_no_parent" => {
                let mut parent = f.parent.clone();
                match arm {
                    "parent_package" => parent.package = true,
                    "parent_file" => parent.kind = NodeKind::File,
                    "parent_disconnected" => parent.parent_id = Some("foreign-root".into()),
                    "parent_cycle" => parent.parent_id = Some(parent.id.clone()),
                    _ => parent.parent_id = None,
                }
                Store::open(&f.provider.metadata).unwrap().observe_node(&f.provider.scope, &parent).unwrap();
            }
            "parent_missing" => { Connection::open(&f.provider.metadata).unwrap().execute("DELETE FROM observed", []).unwrap(); }
            "observed_absent" => {
                let mut store = Store::open(&f.provider.metadata).unwrap();
                let ticket = store.node_observation(&f.provider.scope, &f.current.id).unwrap();
                store.publish_absence(&ticket).unwrap();
                assert!(store.node(&f.provider.scope, &f.current.id).unwrap().is_none());
                assert_eq!(Connection::open(&f.provider.metadata).unwrap().query_row(
                    "SELECT count(*) FROM observed_absent WHERE id=?1", [&f.current.id], |r| r.get::<_, i64>(0)
                ).unwrap(), 1);
            }
            "missing_base" => f.edit("mutations", f.remove.id, |b| b["base"] = json!(null)),
            "unresolved_base" => f.edit("mutations", f.remove.id, |b| b["base"]["resolved"] = json!(false)),
            "wrong_predecessor" => f.edit("mutations", f.remove.id, |b| b["base"]["predecessor"] = json!(foreign)),
            "missing_edge" => { f.db().execute("DELETE FROM write_successors WHERE predecessor=?1", [f.upload.id.to_string()]).unwrap(); }
            "wrong_edge" => { f.db().execute("UPDATE write_successors SET successor=?2 WHERE predecessor=?1", params![f.upload.id.to_string(), foreign.to_string()]).unwrap(); }
            "missing_remove_owner" => { f.db().execute("DELETE FROM namespace_operations WHERE operation=?1", [f.remove.id.to_string()]).unwrap(); }
            "wrong_remove_owner" => { f.db().execute("UPDATE namespace_operations SET object=?2 WHERE operation=?1", params![f.remove.id.to_string(), foreign.to_string()]).unwrap(); }
            "missing_upload_owner" => { f.db().execute("DELETE FROM namespace_operations WHERE operation=?1", [f.upload.id.to_string()]).unwrap(); }
            "wrong_working" => f.edit("mutations", f.remove.id, |b| b["working_file"] = json!(foreign)),
            "upload_not_completed" => {
                f.edit("uploads", f.upload.id, |b| b["state"] = json!("verify_required"));
                f.db().execute("UPDATE uploads SET state='verify_required' WHERE id=?1", [f.upload.id.to_string()]).unwrap();
            }
            "upload_scope" => f.edit("uploads", f.upload.id, |b| b["scope"]["collection"] = json!("foreign-drive")),
            "upload_remote" => f.edit("uploads", f.upload.id, |b| b["remote"]["etag"] = json!("foreign-revision")),
            "owner_remote" => f.edit("namespace_objects", f.owner, |b| b["remote"]["etag"] = json!("foreign-revision")),
            "remove_state_column" => { f.db().execute("UPDATE mutations SET state='verify_required' WHERE id=?1", [f.remove.id.to_string()]).unwrap(); }
            "remove_sequence_column" => { f.db().execute("UPDATE mutations SET sequence=sequence+10 WHERE id=?1", [f.remove.id.to_string()]).unwrap(); }
            "remove_queue_complete" => { f.db().execute("UPDATE write_queue SET complete=1 WHERE id=?1", [f.remove.id.to_string()]).unwrap(); }
            "remove_queue_sequence" => { f.db().execute("UPDATE write_queue SET sequence=sequence+10 WHERE id=?1", [f.remove.id.to_string()]).unwrap(); }
            "upload_state_column" => { f.db().execute("UPDATE uploads SET state='verify_required' WHERE id=?1", [f.upload.id.to_string()]).unwrap(); }
            "upload_sequence_column" => { f.db().execute("UPDATE uploads SET sequence=sequence+10 WHERE id=?1", [f.upload.id.to_string()]).unwrap(); }
            "upload_queue_incomplete" => { f.db().execute("UPDATE write_queue SET complete=0 WHERE id=?1", [f.upload.id.to_string()]).unwrap(); }
            "upload_queue_sequence" => { f.db().execute("UPDATE write_queue SET sequence=sequence+10 WHERE id=?1", [f.upload.id.to_string()]).unwrap(); }
            "upload_package" => f.edit("uploads", f.upload.id, |b| b["representation"] = json!({
                "kind": "package_archive", "expected_root": "Fixture.numbers",
                "semantic": {"version": 2, "sha256": "a".repeat(64), "entries": 2, "files": 1, "expanded_bytes": CONTENT.len()}
            })),
            "upload_replace_mismatch" => f.edit("uploads", f.upload.id, |b| b["intent"]["item"] = json!("FILE::com.apple.CloudDocs::foreign-original-item")),
            "working_native" => f.edit("working_files", f.working, |b| b["native"] = json!(true)),
            "upload_working" => f.edit("uploads", f.upload.id, |b| b["working_file"] = json!(foreign)),
            _ => unreachable!(),
        }
        let before = f.snapshot();
        let result = f
            .provider
            .folder_plan_for_operation(operation, &request)
            .await;
        assert_eq!(
            f.snapshot(),
            before,
            "planning changed hostile fixture {arm}"
        );
        assert!(result.is_err(), "receipt source accepted hostile arm {arm}");
    }
}

#[tokio::test]
async fn resolved_remove_preparation_backoff_preserves_newer_unlinked_bytes() {
    let mut f = ResolvedRemove::new();
    f.remove = {
        let mut journal = f.provider.journal.lock().unwrap();
        let mut row = f.remove.clone();
        for failure in 1..=8 {
            journal
                .defer_mutation_preparation(
                    row.id,
                    row.attempt.unwrap(),
                    std::time::Duration::from_secs(if failure == 8 { 60 } else { 0 }),
                )
                .unwrap();
            row = journal.mutation(row.id).unwrap();
            assert_eq!(row.state, MutationState::Pending);
            assert_eq!(row.failed_attempts, failure);
            assert!(row.attempt.is_none() && row.receipt.is_none() && row.prepared_item.is_none());
            assert!(row.retry_at > 0 && row.base.as_ref().unwrap().resolved);
            if failure < 8 {
                row = journal.claim_mutation().unwrap().unwrap();
                assert_eq!(row.id, f.remove.id);
                assert_eq!(row.state, MutationState::Applying);
            }
        }
        assert!(
            journal.claim_mutation().unwrap().is_none(),
            "backoff must not dispatch a new attempt"
        );
        let before = journal.working_file(f.working).unwrap();
        journal
            .write_working(f.working, 0, b"private-newer-dirty-bytes")
            .unwrap();
        let newer = journal.truncate_working(f.working, 9).unwrap();
        assert!(newer.unlinked && newer.dirty && newer.generation > before.generation);
        assert_eq!(newer.latest, Some(row.id));
        assert_eq!(newer.node.size, 9);
        assert_ne!(newer.node.size, f.current.size);
        assert_eq!(journal.namespace_object(f.owner).unwrap().node, newer.node);
        assert_eq!(row.request.intent.before(), Some(&f.current));
        row
    };
    assert_eq!(
        f.provider
            .mutation_operation(&f.remove.id.to_string(), &f.remove.request)
            .unwrap(),
        f.remove.id
    );
    let before = f.snapshot();
    assert_eq!(before.1, CONTENT);
    assert_eq!(before.2, b"private-n");
    let working_before = serde_json::to_value(
        f.provider
            .journal
            .lock()
            .unwrap()
            .working_file(f.working)
            .unwrap(),
    )
    .unwrap();
    let result = f
        .provider
        .folder_plan_for_operation(f.remove.id, &f.remove.request)
        .await;
    assert_eq!(
        f.snapshot(),
        before,
        "planning changed preparation backoff or newer descriptor bytes"
    );
    let plan = result.expect("a resolved completed receipt survives local preparation backoff and dirty descriptor drift");
    assert!(
        plan.request == f.remove.request
            && plan.destination.is_none()
            && plan.original_sha256.is_none()
    );
    assert!(
        f.provider.folder_adapter(&plan).is_err(),
        "source digest still required before mutation"
    );
    let root = f.retained_root.join("journal");
    let account = f.provider.scope.account.clone();
    let working = f.working;
    let upload = f.upload.id;
    let remove = f.remove.clone();
    drop(f);
    let journal = UploadJournal::open(&root, &account, 1024 * 1024).unwrap();
    assert_eq!(
        serde_json::to_value(journal.working_file(working).unwrap()).unwrap(),
        working_before
    );
    assert_eq!(journal.read_working(working, 0, 128).unwrap(), b"private-n");
    let mut sealed = Vec::new();
    journal
        .payload(upload)
        .unwrap()
        .read_to_end(&mut sealed)
        .unwrap();
    assert_eq!(sealed, CONTENT);
    assert_eq!(
        serde_json::to_value(journal.mutation(remove.id).unwrap()).unwrap(),
        serde_json::to_value(remove).unwrap()
    );
}

#[tokio::test]
async fn resolved_remove_present_leaf_refuses_broken_or_observed_absent_ancestry() {
    for arm in [
        "parent_missing",
        "parent_package",
        "parent_file",
        "parent_cycle",
        "parent_no_parent",
        "leaf_observed_absent",
        "parent_observed_absent",
    ] {
        let f = ResolvedRemove::new();
        let mut store = Store::open(&f.provider.metadata).unwrap();
        let listed = Node {
            content_version: None,
            ..f.current.clone()
        };
        store.observe_node(&f.provider.scope, &listed).unwrap();
        assert!(
            store
                .node_chain_to_root(&f.provider.scope, &listed.id, ROOT_ID)
                .unwrap()
                .is_some()
        );
        match arm {
            "parent_missing" => {
                Connection::open(&f.provider.metadata)
                    .unwrap()
                    .execute("DELETE FROM observed WHERE id=?1", [&f.parent.id])
                    .unwrap();
            }
            "leaf_observed_absent" | "parent_observed_absent" => {
                let item = if arm == "leaf_observed_absent" {
                    &listed.id
                } else {
                    &f.parent.id
                };
                let ticket = store.node_observation(&f.provider.scope, item).unwrap();
                store.publish_absence(&ticket).unwrap();
                assert!(store.node(&f.provider.scope, item).unwrap().is_none());
            }
            _ => {
                let mut parent = f.parent.clone();
                match arm {
                    "parent_package" => parent.package = true,
                    "parent_file" => parent.kind = NodeKind::File,
                    "parent_cycle" => parent.parent_id = Some(parent.id.clone()),
                    _ => parent.parent_id = None,
                }
                store.observe_node(&f.provider.scope, &parent).unwrap();
            }
        }
        drop(store);
        let before = f.snapshot();
        let result = f
            .provider
            .folder_plan_for_operation(f.remove.id, &f.remove.request)
            .await;
        assert_eq!(
            f.snapshot(),
            before,
            "planning changed broken indexed ancestry {arm}"
        );
        assert!(
            result.is_err(),
            "receipt fallback overrode existing broken ancestry {arm}"
        );
    }
}

#[tokio::test]
async fn indexed_remove_after_acknowledged_relocation_needs_no_upload_working_authority() {
    let (temp, provider) = fixture();
    let _retained_root = temp.keep();
    let parent = Node {
        id: format!("FOLDER::com.apple.CloudDocs::{}", Uuid::new_v4()),
        ..folder(ROOT_ID)
    };
    let before = Node {
        id: format!("FILE::com.apple.CloudDocs::{}", Uuid::new_v4()),
        parent_id: Some(parent.id.clone()),
        name: "before-rename.txt".into(),
        kind: NodeKind::File,
        size: CONTENT.len() as u64,
        modified_unix: 0,
        etag: Some("before-rename-revision".into()),
        content_version: Some("unchanged-file-content".into()),
        target: None,
        package: false,
    };
    let current = Node {
        name: "after-rename.txt".into(),
        etag: Some("after-rename-revision".into()),
        ..before.clone()
    };
    let remove = {
        let mut journal = provider.journal.lock().unwrap();
        let renamed = journal
            .enqueue_mutation(MutationRequest {
                scope: provider.scope.clone(),
                intent: MutationIntent::Relocate {
                    before,
                    parent: parent.id.clone(),
                    name: current.name.clone(),
                },
            })
            .unwrap();
        let claimed = journal.claim_mutation().unwrap().unwrap();
        assert_eq!(claimed.id, renamed.id);
        journal
            .acknowledge_mutation(
                claimed.id,
                claimed.attempt.unwrap(),
                MutationReceipt::Upsert(current.clone()),
            )
            .unwrap();
        assert_eq!(
            journal.mutation(renamed.id).unwrap().state,
            MutationState::Applied
        );
        let queued = journal
            .enqueue_mutation_after(
                renamed.id,
                MutationRequest {
                    scope: provider.scope.clone(),
                    intent: MutationIntent::RemoveFile {
                        before: current.clone(),
                    },
                },
            )
            .unwrap();
        assert!(!queued.base.as_ref().unwrap().resolved);
        let remove = journal.claim_mutation().unwrap().unwrap();
        assert_eq!(remove.id, queued.id);
        assert_eq!(remove.base.as_ref().unwrap().predecessor, renamed.id);
        assert!(remove.base.as_ref().unwrap().resolved);
        assert_eq!(remove.request.intent.before(), Some(&current));
        assert!(remove.working_file.is_none());
        assert!(
            journal
                .namespace_for_operation(remove.id)
                .unwrap()
                .is_none()
        );
        remove
    };
    let mut store = Store::open(&provider.metadata).unwrap();
    store.observe_node(&provider.scope, &parent).unwrap();
    store.observe_node(&provider.scope, &current).unwrap();
    let journal_before = serde_json::to_value(
        provider
            .journal
            .lock()
            .unwrap()
            .list_mutations(0, 20)
            .unwrap(),
    )
    .unwrap();
    let index_before = store.visible_nodes(&provider.scope).unwrap();
    let result = provider
        .folder_plan_for_operation(remove.id, &remove.request)
        .await;
    assert_eq!(
        serde_json::to_value(
            provider
                .journal
                .lock()
                .unwrap()
                .list_mutations(0, 20)
                .unwrap()
        )
        .unwrap(),
        journal_before
    );
    assert_eq!(store.visible_nodes(&provider.scope).unwrap(), index_before);
    let plan = result.expect("indexed confirmed relocation must retain ordinary Remove routing without an upload/working stream");
    assert!(
        plan.request == remove.request
            && plan.destination.is_none()
            && plan.original_sha256.is_none()
    );
    assert!(
        provider.folder_adapter(&plan).is_err(),
        "independent source digest remains required"
    );
}

#[cfg(test)]
mod atomic_cleanup_sources {
    //! Completed ordinary atomic cleanup may precede the temp leaf's metadata refresh.
    //! All authority is produced by journal APIs; only negative arms corrupt owned fixtures.
    //! No session/provider callback is reached by folder planning. Every fixture is retained.
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::icloud_writes::tests::{fixture, folder};
    use crate::journal::{MutationRecord, ReplacementRecord, UploadRecord};
    use cirrove_core::upload::RecoveryLocation;
    use rusqlite::{Connection, params};
    use std::io::Read;

    const ORIGINAL: &[u8] = b"owned-final-A";
    const CONTENT: &[u8] = b"owned-atomic-document-B";
    const TRASH: &str = "FOLDER::com.apple.CloudDocs::TRASH_ROOT";

    struct CompletedCleanup {
        root: PathBuf,
        provider: ICloudWriteProvider,
        parent: Node,
        destination: Node,
        current_temp: Node,
        current_final: Node,
        original: UploadRecord,
        create: UploadRecord,
        source: UploadRecord,
        target: UploadRecord,
        replacement: ReplacementRecord,
        remove: MutationRecord,
        working: Uuid,
    }

    fn file(parent: &Node, name: &str, size: u64, etag: &str) -> Node {
        Node {
            id: format!("FILE::com.apple.CloudDocs::{}", Uuid::new_v4()),
            parent_id: Some(parent.id.clone()),
            name: name.into(),
            kind: NodeKind::File,
            size,
            modified_unix: 0,
            etag: Some(etag.into()),
            content_version: Some(format!("content-{etag}")),
            package: false,
            target: None,
        }
    }
    fn backup(original: &Node) -> Node {
        Node {
            parent_id: Some(TRASH.into()),
            etag: Some(format!("trash-{}", original.id)),
            ..original.clone()
        }
    }

    impl CompletedCleanup {
        fn new(cross_directory: bool, backed_off: bool) -> Self {
            let (temp, provider) = fixture();
            let root = temp.keep();
            eprintln!(
                "retained synthetic atomic cleanup fixture: {}",
                root.display()
            );
            let parent = Node {
                id: format!("FOLDER::com.apple.CloudDocs::{}", Uuid::new_v4()),
                name: "SourceParent".into(),
                ..folder(ROOT_ID)
            };
            let destination = if cross_directory {
                Node {
                    id: format!("FOLDER::com.apple.CloudDocs::{}", Uuid::new_v4()),
                    name: "DestinationParent".into(),
                    ..folder(ROOT_ID)
                }
            } else {
                parent.clone()
            };
            let old_final = file(&destination, "final.xlsx", ORIGINAL.len() as u64, "final-A");
            let old_temp = file(&parent, "editor.tmp", 0, "empty-temp");
            let current_temp = file(&parent, "editor.tmp", CONTENT.len() as u64, "temp-B");
            let current_final = file(&destination, "final.xlsx", CONTENT.len() as u64, "final-B");
            let (original, create, source, target, replacement, remove, working) = {
                let mut j = provider.journal.lock().unwrap();
                // Normal A completion followed by real retirement and rehydration:
                // the atomic victim has a current receipt, working stream and latest=None.
                let a = j
                    .create_working(
                        provider.scope.clone(),
                        Node {
                            size: 0,
                            ..old_final.clone()
                        },
                        true,
                        &b""[..],
                    )
                    .unwrap();
                j.write_working(a.id, 0, ORIGINAL).unwrap();
                let a_id = j.seal_working(a.id).unwrap().unwrap().id;
                let a = j.claim_next().unwrap().unwrap();
                assert_eq!(a.id, a_id);
                j.acknowledge(a.id, a.attempt.unwrap(), old_final.clone())
                    .unwrap();
                let original = j.get(a.id).unwrap();
                let object = j.namespace_for_operation(original.id).unwrap().unwrap();
                j.handoff_namespace(object.id, object.revision, old_final.clone())
                    .unwrap();
                let victim = j
                    .observe_namespace_file(provider.scope.clone(), old_final.clone())
                    .unwrap();
                let hydrated = j
                    .create_working(provider.scope.clone(), old_final.clone(), false, ORIGINAL)
                    .unwrap();
                assert!(hydrated.latest.is_none());
                let victim = j.namespace_object(victim.id).unwrap();
                assert_eq!(victim.working_file, Some(hydrated.id));
                assert!(victim.latest.is_none() && !victim.follows_remote);

                let w = j
                    .create_working(provider.scope.clone(), old_temp.clone(), true, &b""[..])
                    .unwrap();
                let create_id = j.seal_working(w.id).unwrap().unwrap().id;
                j.write_working(w.id, 0, CONTENT).unwrap();
                let source_id = j.seal_working(w.id).unwrap().unwrap().id;
                let source = j.get(source_id).unwrap();
                assert_eq!(source.base.as_ref().unwrap().predecessor, create_id);
                assert!(!source.base.as_ref().unwrap().resolved);
                let owner = j.namespace_for_operation(source_id).unwrap().unwrap();
                // Genuine order: takeover BEFORE empty Create acknowledgement.
                let replacement = j
                    .replace_namespace_file(
                        owner.id,
                        owner.revision,
                        victim.id,
                        victim.revision,
                        false,
                    )
                    .unwrap();
                assert_eq!(replacement.source_unconfirmed_create, Some(create_id));
                assert!(
                    j.namespace_object(replacement.cleanup_object)
                        .unwrap()
                        .remote
                        .is_none()
                );
                assert!(j.get(replacement.id).unwrap().base.is_none());
                assert!(j.claim_mutation().unwrap().is_none());
                let create = j.claim_next().unwrap().unwrap();
                assert_eq!(create.id, create_id);
                j.acknowledge(create.id, create.attempt.unwrap(), old_temp.clone())
                    .unwrap();
                let create = j.get(create.id).unwrap();
                assert_eq!(create.state, UploadState::Uploaded);

                let source = j.claim_next().unwrap().unwrap();
                assert_eq!(source.id, source_id);
                j.reserve_identity_handoff(
                    source.id,
                    source.attempt.unwrap(),
                    RecoveryLocation::Trash {
                        local_name: format!("recovery-{}.tmp", source.id),
                        parent: TRASH.into(),
                    },
                )
                .unwrap();
                j.acknowledge_identity_handoff(
                    source.id,
                    source.attempt.unwrap(),
                    current_temp.clone(),
                    backup(&old_temp),
                )
                .unwrap();
                let source = j.get(source.id).unwrap();
                assert_eq!(source.state, UploadState::Uploaded);
                assert_eq!(source.ordinary_handoff_receipt().unwrap().0, &current_temp);
                assert!(
                    j.claim_mutation().unwrap().is_none(),
                    "cleanup must await target completion"
                );

                let target = j.claim_next().unwrap().unwrap();
                assert_eq!(target.id, replacement.id);
                assert_eq!(target.working_file, Some(w.id));
                assert_eq!((target.size, &target.sha256), (source.size, &source.sha256));
                j.reserve_identity_handoff(
                    target.id,
                    target.attempt.unwrap(),
                    RecoveryLocation::Trash {
                        local_name: format!("recovery-{}.xlsx", target.id),
                        parent: TRASH.into(),
                    },
                )
                .unwrap();
                j.acknowledge_identity_handoff(
                    target.id,
                    target.attempt.unwrap(),
                    current_final.clone(),
                    backup(&old_final),
                )
                .unwrap();
                let target = j.get(target.id).unwrap();
                let replacement = j.replacement(replacement.id).unwrap();
                assert_eq!(target.state, UploadState::Uploaded);
                assert_eq!(target.ordinary_handoff_receipt().unwrap().0, &current_final);
                assert!(replacement.remote_applied && replacement.local_ready);
                let final_owner = j.namespace_object(replacement.source).unwrap();
                assert_eq!(final_owner.remote, Some(current_final.clone()));
                assert!(!final_owner.unlinked && final_owner.remote_owned);
                assert_eq!(final_owner.node.name, "final.xlsx");
                assert_eq!(final_owner.node.parent_id, Some(destination.id.clone()));
                if backed_off {
                    // Actual normal retirement after the completed target removes
                    // working/latest and can use a freshly observed Node without the
                    // upload-only content annotation. No JSON authority is fabricated.
                    let observed = Node {
                        content_version: None,
                        ..current_final.clone()
                    };
                    let retired = j
                        .handoff_namespace(final_owner.id, final_owner.revision, observed.clone())
                        .unwrap();
                    assert!(
                        retired.follows_remote
                            && retired.working_file.is_none()
                            && retired.latest.is_none()
                    );
                    assert_eq!(retired.remote, Some(observed));
                    assert_ne!(retired.remote, target.remote);
                    assert_eq!(retired.node.name, "final.xlsx");
                    assert_eq!(retired.node.parent_id, Some(destination.id.clone()));
                }

                let mut remove = j.claim_mutation().unwrap().unwrap();
                assert_eq!(remove.id, replacement.cleanup);
                assert_eq!(remove.state, MutationState::Applying);
                assert_eq!(remove.request.intent.before(), Some(&current_temp));
                assert!(remove.working_file.is_none());
                assert!(remove.base.as_ref().unwrap().resolved);
                assert_eq!(remove.base.as_ref().unwrap().predecessor, source.id);
                let cleanup = j.namespace_object(replacement.cleanup_object).unwrap();
                assert!(cleanup.unlinked && cleanup.remote_owned && !cleanup.follows_remote);
                assert!(cleanup.working_file.is_none());
                assert_eq!(cleanup.latest, Some(remove.id));
                assert_eq!(cleanup.remote, Some(current_temp.clone()));
                assert_eq!(cleanup.remote_sequence, source.sequence);
                assert_eq!(
                    j.namespace_for_operation(remove.id).unwrap().unwrap().id,
                    cleanup.id
                );
                if backed_off {
                    for failure in 1..=7 {
                        j.defer_mutation_preparation(
                            remove.id,
                            remove.attempt.unwrap(),
                            std::time::Duration::from_secs(if failure == 7 { 60 } else { 0 }),
                        )
                        .unwrap();
                        remove = j.mutation(remove.id).unwrap();
                        assert_eq!(remove.state, MutationState::Pending);
                        assert_eq!(remove.failed_attempts, failure);
                        assert!(
                            remove.attempt.is_none()
                                && remove.receipt.is_none()
                                && remove.prepared_item.is_none()
                        );
                        if failure < 7 {
                            remove = j.claim_mutation().unwrap().unwrap();
                        }
                    }
                    assert!(
                        j.claim_mutation().unwrap().is_none(),
                        "backoff is not an admission retry"
                    );
                }
                (original, create, source, target, replacement, remove, w.id)
            };
            let mut store = Store::open(&provider.metadata).unwrap();
            store.observe_node(&provider.scope, &parent).unwrap();
            if cross_directory {
                store.observe_node(&provider.scope, &destination).unwrap();
            }
            assert!(
                store
                    .node(&provider.scope, &current_temp.id)
                    .unwrap()
                    .is_none()
            );
            let f = Self {
                root,
                provider,
                parent,
                destination,
                current_temp,
                current_final,
                original,
                create,
                source,
                target,
                replacement,
                remove,
                working,
            };
            f.assert_lineage();
            f
        }
        fn db(&self) -> Connection {
            Connection::open(self.root.join("journal/uploads.db")).unwrap()
        }
        fn assert_lineage(&self) {
            let db = self.db();
            for (operation, predecessor) in [
                (self.target.id, self.source.id),
                (self.remove.id, self.target.id),
            ] {
                assert_eq!(db.query_row("SELECT count(*) FROM write_prerequisites WHERE operation=?1 AND predecessor=?2", params![operation.to_string(), predecessor.to_string()], |r| r.get::<_, i64>(0)).unwrap(), 1);
            }
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM write_successors WHERE predecessor=?1 AND successor=?2",
                    params![self.source.id.to_string(), self.remove.id.to_string()],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                1
            );
            for (id, complete) in [
                (self.create.id, 1),
                (self.source.id, 1),
                (self.target.id, 1),
                (self.remove.id, 0),
            ] {
                assert_eq!(
                    db.query_row(
                        "SELECT complete FROM write_queue WHERE id=?1",
                        [id.to_string()],
                        |r| r.get::<_, i64>(0)
                    )
                    .unwrap(),
                    complete
                );
            }
            let metadata = Connection::open(&self.provider.metadata).unwrap();
            for table in ["nodes", "observed", "observed_absent"] {
                assert_eq!(
                    metadata
                        .query_row(
                            &format!("SELECT count(*) FROM {table} WHERE id=?1"),
                            [&self.current_temp.id],
                            |r| r.get::<_, i64>(0)
                        )
                        .unwrap(),
                    0
                );
            }
        }
        fn edit(&self, table: &str, id: Uuid, change: impl FnOnce(&mut serde_json::Value)) {
            assert!(matches!(
                table,
                "uploads" | "mutations" | "namespace_objects" | "file_replacements"
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
        fn snapshot(&self) -> (Vec<Vec<String>>, Vec<Vec<u8>>, Vec<Node>) {
            let db = self.db();
            let mut rows: Vec<Vec<String>> = [
            "SELECT json_array(sequence,id,state,body) FROM uploads ORDER BY sequence",
            "SELECT json_array(sequence,id,state,body) FROM mutations ORDER BY sequence",
            "SELECT json_array(id,body) FROM namespace_objects ORDER BY id",
            "SELECT json_array(id,body) FROM working_files ORDER BY id",
            "SELECT json_array(id,source,victim,cleanup,body) FROM file_replacements ORDER BY id",
            "SELECT json_array(operation,object) FROM namespace_operations ORDER BY operation",
            "SELECT json_array(predecessor,successor) FROM write_successors ORDER BY predecessor",
            "SELECT json_array(operation,predecessor) FROM write_prerequisites ORDER BY operation,predecessor",
            "SELECT json_array(sequence,id,complete) FROM write_queue ORDER BY sequence",
        ].into_iter().map(|sql| db.prepare(sql).unwrap().query_map([], |r| r.get::<_, String>(0)).unwrap().collect::<std::result::Result<Vec<_>, _>>().unwrap()).collect();
            rows.push(Connection::open(&self.provider.metadata).unwrap().prepare("SELECT json_array(scope,id,source_revision) FROM observed_absent ORDER BY scope,id").unwrap().query_map([], |r| r.get::<_, String>(0)).unwrap().collect::<std::result::Result<Vec<_>, _>>().unwrap());
            let j = self.provider.journal.lock().unwrap();
            let mut bytes = Vec::new();
            for id in [
                self.original.id,
                self.create.id,
                self.source.id,
                self.target.id,
            ] {
                let mut payload = Vec::new();
                j.payload(id).unwrap().read_to_end(&mut payload).unwrap();
                bytes.push(payload);
            }
            bytes.push(
                std::fs::read(
                    self.root
                        .join("journal/working")
                        .join(self.working.to_string()),
                )
                .unwrap(),
            );
            let visible = Store::open(&self.provider.metadata)
                .unwrap()
                .visible_nodes(&self.provider.scope)
                .unwrap();
            (rows, bytes, visible)
        }
    }

    async fn admitted(cross_directory: bool, backed_off: bool) {
        let f = CompletedCleanup::new(cross_directory, backed_off);
        assert_eq!(
            f.provider
                .mutation_operation(&f.remove.id.to_string(), &f.remove.request)
                .unwrap(),
            f.remove.id
        );
        let before = f.snapshot();
        assert_eq!(
            before.1,
            vec![
                ORIGINAL.to_vec(),
                vec![],
                CONTENT.to_vec(),
                CONTENT.to_vec(),
                CONTENT.to_vec()
            ]
        );
        let result = f
            .provider
            .folder_plan_for_operation(f.remove.id, &f.remove.request)
            .await;
        assert_eq!(
            f.snapshot(),
            before,
            "planning changed journal, sealed bytes, working bytes or index"
        );
        if let Err(error) = &result {
            assert!(
                matches!(error, MutationError::Conflict),
                "unexpected setup/failure class"
            );
        }
        let plan = result.expect("completed atomic source and target receipts must authorize their exact cleanup before temp indexing");
        assert!(
            plan.request == f.remove.request
                && plan.destination.is_none()
                && plan.original_sha256.is_none()
        );
        assert!(
            f.provider.folder_adapter(&plan).is_err(),
            "receipt routing must not bypass independent content/revision preflight"
        );
        let j = f.provider.journal.lock().unwrap();
        let final_owner = j.namespace_object(f.replacement.source).unwrap();
        let observed_final = if backed_off {
            Node {
                content_version: None,
                ..f.current_final.clone()
            }
        } else {
            f.current_final.clone()
        };
        assert_eq!(final_owner.remote, Some(observed_final));
        assert_eq!(final_owner.follows_remote, backed_off);
        if backed_off {
            assert!(final_owner.working_file.is_none() && final_owner.latest.is_none());
        }
        assert_eq!(
            j.namespace_object(f.replacement.cleanup_object)
                .unwrap()
                .remote,
            Some(f.current_temp.clone())
        );
        assert!(
            j.namespace_object(f.replacement.cleanup_object)
                .unwrap()
                .unlinked
        );
    }
    #[tokio::test]
    async fn completed_atomic_cleanup_routes_unindexed_temp_from_exact_receipts() {
        admitted(false, false).await;
    }
    #[tokio::test]
    async fn completed_atomic_cleanup_backoff_keeps_cross_directory_source_route() {
        admitted(true, true).await;
    }

    #[tokio::test]
    async fn completed_atomic_cleanup_refuses_foreign_or_incomplete_authority() {
        use serde_json::json;
        for arm in [
            "foreign_operation",
            "before_revision",
            "foreign_scope",
            "remove_body_id",
            "remove_sequence",
            "remove_state",
            "remove_queue_complete",
            "source_incomplete",
            "source_queue_incomplete",
            "source_receipt",
            "target_incomplete",
            "target_queue_incomplete",
            "target_receipt",
            "replacement_missing",
            "replacement_not_applied",
            "replacement_cleanup_column",
            "replacement_source_column",
            "replacement_victim_column",
            "missing_cleanup_pair",
            "foreign_cleanup_pair",
            "missing_source_pair",
            "missing_target_pair",
            "missing_source_edge",
            "missing_target_prerequisite",
            "missing_cleanup_barrier",
            "unresolved_base",
            "cleanup_remote",
            "cleanup_sequence",
            "cleanup_working",
            "source_native",
        ] {
            let f = CompletedCleanup::new(false, false);
            let mut operation = f.remove.id;
            let mut request = f.remove.request.clone();
            let foreign = Uuid::new_v4();
            match arm {
            "foreign_operation" => operation = foreign,
            "before_revision" => { let MutationIntent::RemoveFile { before } = &mut request.intent else { panic!("RemoveFile") }; before.etag = Some("foreign-revision".into()); }
            "foreign_scope" => request.scope.collection = "foreign-collection".into(),
            "remove_body_id" => f.edit("mutations", f.remove.id, |b| b["id"] = json!(foreign)),
            "remove_sequence" => { f.db().execute("UPDATE mutations SET sequence=sequence+10 WHERE id=?1", [f.remove.id.to_string()]).unwrap(); }
            "remove_state" => { f.db().execute("UPDATE mutations SET state='pending' WHERE id=?1", [f.remove.id.to_string()]).unwrap(); }
            "remove_queue_complete" => { f.db().execute("UPDATE write_queue SET complete=1 WHERE id=?1", [f.remove.id.to_string()]).unwrap(); }
            "source_incomplete" | "target_incomplete" => {
                let id = if arm == "source_incomplete" { f.source.id } else { f.target.id };
                f.edit("uploads", id, |b| b["state"] = json!("verify_required"));
                f.db().execute("UPDATE uploads SET state='verify_required' WHERE id=?1", [id.to_string()]).unwrap();
            }
            "source_queue_incomplete" | "target_queue_incomplete" => { let id = if arm == "source_queue_incomplete" { f.source.id } else { f.target.id }; f.db().execute("UPDATE write_queue SET complete=0 WHERE id=?1", [id.to_string()]).unwrap(); }
            "source_receipt" | "target_receipt" => { let id = if arm == "source_receipt" { f.source.id } else { f.target.id }; f.edit("uploads", id, |b| b["identity_handoff"] = json!(null)); }
            "replacement_missing" => { f.db().execute("DELETE FROM file_replacements WHERE id=?1", [f.target.id.to_string()]).unwrap(); }
            "replacement_not_applied" => f.edit("file_replacements", f.target.id, |b| b["remote_applied"] = json!(false)),
            "replacement_cleanup_column" | "replacement_source_column" | "replacement_victim_column" => {
                let column = match arm { "replacement_cleanup_column" => "cleanup", "replacement_source_column" => "source", _ => "victim" };
                f.db().execute(&format!("UPDATE file_replacements SET {column}=?2 WHERE id=?1"), params![f.target.id.to_string(), foreign.to_string()]).unwrap();
            }
            "missing_cleanup_pair" | "missing_source_pair" | "missing_target_pair" => {
                let id = match arm { "missing_cleanup_pair" => f.remove.id, "missing_source_pair" => f.source.id, _ => f.target.id };
                f.db().execute("DELETE FROM namespace_operations WHERE operation=?1", [id.to_string()]).unwrap();
            }
            "foreign_cleanup_pair" => { f.db().execute("UPDATE namespace_operations SET object=?2 WHERE operation=?1", params![f.remove.id.to_string(), foreign.to_string()]).unwrap(); }
            "missing_source_edge" => { f.db().execute("DELETE FROM write_successors WHERE predecessor=?1 AND successor=?2", params![f.source.id.to_string(), f.remove.id.to_string()]).unwrap(); }
            "missing_target_prerequisite" | "missing_cleanup_barrier" => {
                let (operation, predecessor) = if arm == "missing_target_prerequisite" { (f.target.id, f.source.id) } else { (f.remove.id, f.target.id) };
                f.db().execute("DELETE FROM write_prerequisites WHERE operation=?1 AND predecessor=?2", params![operation.to_string(), predecessor.to_string()]).unwrap();
            }
            "unresolved_base" => f.edit("mutations", f.remove.id, |b| b["base"]["resolved"] = json!(false)),
            "cleanup_remote" => f.edit("namespace_objects", f.replacement.cleanup_object, |b| b["remote"]["etag"] = json!("foreign")),
            "cleanup_sequence" => f.edit("namespace_objects", f.replacement.cleanup_object, |b| b["remote_sequence"] = json!(f.source.sequence + 1)),
            "cleanup_working" => f.edit("namespace_objects", f.replacement.cleanup_object, |b| b["working_file"] = json!(f.working)),
            "source_native" => f.edit("uploads", f.source.id, |b| b["representation"] = json!({"kind":"package_archive","expected_root":"Fixture.numbers","semantic":{"version":2,"sha256":"a".repeat(64),"entries":2,"files":1,"expanded_bytes":CONTENT.len()}})),
            _ => unreachable!(),
        }
            let before = f.snapshot();
            let result = f
                .provider
                .folder_plan_for_operation(operation, &request)
                .await;
            assert_eq!(f.snapshot(), before, "planning changed hostile arm {arm}");
            assert!(
                result.is_err(),
                "atomic cleanup accepted hostile authority {arm}"
            );
        }
    }

    #[tokio::test]
    async fn completed_atomic_cleanup_receipt_cannot_override_index_absence_or_mismatch() {
        for arm in [
            "indexed_revision",
            "indexed_name",
            "indexed_parent",
            "observed_absent",
            "parent_missing",
            "parent_cycle",
        ] {
            let f = CompletedCleanup::new(true, false);
            let mut store = Store::open(&f.provider.metadata).unwrap();
            match arm {
                "indexed_revision" | "indexed_name" | "indexed_parent" => {
                    let mut node = f.current_temp.clone();
                    match arm {
                        "indexed_revision" => node.etag = Some("foreign".into()),
                        "indexed_name" => node.name = "foreign.tmp".into(),
                        _ => node.parent_id = Some(f.destination.id.clone()),
                    };
                    store.observe_node(&f.provider.scope, &node).unwrap();
                }
                "observed_absent" => {
                    let ticket = store
                        .node_observation(&f.provider.scope, &f.current_temp.id)
                        .unwrap();
                    store.publish_absence(&ticket).unwrap();
                }
                "parent_missing" => {
                    Connection::open(&f.provider.metadata)
                        .unwrap()
                        .execute("DELETE FROM observed WHERE id=?1", [&f.parent.id])
                        .unwrap();
                }
                "parent_cycle" => {
                    let parent = Node {
                        parent_id: Some(f.parent.id.clone()),
                        ..f.parent.clone()
                    };
                    store.observe_node(&f.provider.scope, &parent).unwrap();
                }
                _ => unreachable!(),
            }
            drop(store);
            let before = f.snapshot();
            let result = f
                .provider
                .folder_plan_for_operation(f.remove.id, &f.remove.request)
                .await;
            assert_eq!(
                f.snapshot(),
                before,
                "planning changed metadata refusal {arm}"
            );
            assert!(
                result.is_err(),
                "receipt overrode actual metadata authority {arm}"
            );
        }
    }
}
