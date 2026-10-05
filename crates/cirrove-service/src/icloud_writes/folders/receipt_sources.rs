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
