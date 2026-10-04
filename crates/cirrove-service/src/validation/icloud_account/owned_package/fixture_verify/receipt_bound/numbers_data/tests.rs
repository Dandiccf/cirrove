#![allow(clippy::unwrap_used)]
use super::*;
use crate::journal::UploadJournal;
use cirrove_core::upload::RecoveryLocation;

struct Actual {
    temp: tempfile::TempDir,
    plan: Registration,
    parent: Node,
    original: Node,
}
impl Actual {
    fn path(&self) -> PathBuf {
        self.temp.path().join("journal")
    }
    // Synthetic-fixture mutation only; production RecoveryJournal stays read-only.
    fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.path().join("uploads.db")).unwrap()
    }
    fn writer(&self) -> UploadJournal {
        UploadJournal::open(
            &self.path(),
            &self.plan.account.to_string(),
            8 * 1024 * 1024,
        )
        .unwrap()
    }
    fn check(&self) -> Result<(Node, Node, Option<Node>, Vec<u8>)> {
        let journal = RecoveryJournal::open(&self.path(), &self.plan.account.to_string())?;
        journal_binding(&self.plan, &journal)
    }
    fn retire(&self, current: Node) {
        let mut journal = self.writer();
        let object = journal.namespace_object(self.plan.owner).unwrap();
        assert!(journal.namespace_is_clean(&object).unwrap());
        journal
            .handoff_namespace(object.id, object.revision, current)
            .unwrap();
        journal.collect_retired_working(4).unwrap();
        journal.collect_uploaded_payloads(4).unwrap();
    }
    fn save(&mut self, rehydrate: bool) {
        if rehydrate {
            self.retire(self.original.clone());
        }
        let mut journal = self.writer();
        let working = if rehydrate {
            journal
                .create_truncated_working(self.plan.scope(), self.original.clone())
                .unwrap()
        } else {
            journal.truncate_working(self.plan.working_a, 0).unwrap()
        };
        self.plan.working_b = Some(working.id);
        let bytes = std::fs::read(&self.plan.source_b.path).unwrap();
        journal.write_working(working.id, 0, &bytes).unwrap();
        let row = journal.seal_working(working.id).unwrap().unwrap();
        self.plan.save = Some(row.id);
        self.plan.phase = Phase::SavedB;
        self.plan.created_proof_sha256 = Some("f".repeat(64));
        let claimed = journal.claim_next().unwrap().unwrap();
        assert_eq!(claimed.id, row.id);
        journal
            .reserve_identity_handoff(
                row.id,
                claimed.attempt.unwrap(),
                RecoveryLocation::Trash {
                    local_name: format!("recovery-by-cirrove-{}.txt", row.id),
                    parent: "FOLDER::com.apple.CloudDocs::TRASH_ROOT".into(),
                },
            )
            .unwrap();
        let current = Node {
            id: "FILE::com.apple.CloudDocs::current-b".into(),
            size: bytes.len() as u64,
            etag: Some("b-revision".into()),
            content_version: Some("b-revision".into()),
            ..self.original.clone()
        };
        let backup = Node {
            parent_id: Some("FOLDER::com.apple.CloudDocs::TRASH_ROOT".into()),
            etag: Some("trash-a-revision".into()),
            content_version: Some("trash-a-revision".into()),
            ..self.original.clone()
        };
        journal
            .acknowledge_identity_handoff(row.id, claimed.attempt.unwrap(), current, backup)
            .unwrap();
    }
}
fn actual() -> Actual {
    let run = Uuid::new_v4();
    let temp = tempfile::Builder::new()
        .prefix(&format!("cirrove-numbers-data-{run}"))
        .rand_bytes(0)
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/var/tmp")
        .unwrap();
    let source = |name: &str, bytes: &[u8]| {
        let path = temp.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        RawSource {
            path,
            size: bytes.len() as u64,
            sha256: hex::encode(Sha256::digest(bytes)),
        }
    };
    let mut plan = Registration {
        version: 1,
        run,
        account: Uuid::new_v4(),
        label: "iCloudNumbersDataValidation".into(),
        session_directory: temp.path().to_owned(),
        settings_sha256: "a".repeat(64),
        parent_creation: Uuid::nil(),
        create: Uuid::nil(),
        save: None,
        owner: Uuid::nil(),
        working_a: Uuid::nil(),
        working_b: None,
        phase: Phase::CreatedA,
        created_proof_sha256: None,
        source_a: source("source-a.numbers", b"synthetic raw Numbers A"),
        source_b: source("source-b.numbers", b"synthetic raw Numbers replacement B"),
    };
    let path = temp.path().join("journal");
    let mut journal =
        UploadJournal::open(&path, &plan.account.to_string(), 8 * 1024 * 1024).unwrap();
    let local = journal
        .create_namespace_directory(
            plan.scope(),
            cirrove_icloud::ROOT_ID.into(),
            plan.parent_name(),
        )
        .unwrap();
    let mutation = journal.claim_mutation().unwrap().unwrap();
    plan.parent_creation = mutation.id;
    let parent = Node {
        id: "FOLDER::com.apple.CloudDocs::owned-parent".into(),
        parent_id: Some(cirrove_icloud::ROOT_ID.into()),
        name: plan.parent_name(),
        kind: NodeKind::Folder,
        size: 0,
        modified_unix: 0,
        etag: Some("folder-revision".into()),
        content_version: None,
        target: None,
        package: false,
    };
    journal
        .acknowledge_mutation(
            mutation.id,
            mutation.attempt.unwrap(),
            MutationReceipt::Upsert(parent.clone()),
        )
        .unwrap();
    let local = journal.namespace_object(local.id).unwrap();
    journal
        .handoff_namespace(local.id, local.revision, parent.clone())
        .unwrap();
    let node = Node {
        id: "unallocated-local-file".into(),
        parent_id: Some(local.node.id.clone()),
        name: plan.name(),
        kind: NodeKind::File,
        ..parent.clone()
    };
    let working = journal
        .create_working(plan.scope(), node, true, &b""[..])
        .unwrap();
    plan.working_a = working.id;
    let bytes = std::fs::read(&plan.source_a.path).unwrap();
    journal.write_working(working.id, 0, &bytes).unwrap();
    let row = journal.seal_working(working.id).unwrap().unwrap();
    plan.create = row.id;
    plan.owner = journal.namespace_for_operation(row.id).unwrap().unwrap().id;
    let claimed = journal.claim_next().unwrap().unwrap();
    let original = Node {
        id: "FILE::com.apple.CloudDocs::original-a".into(),
        parent_id: Some(parent.id.clone()),
        name: plan.name(),
        kind: NodeKind::File,
        size: plan.source_a.size,
        etag: Some("a-revision".into()),
        content_version: Some("a-revision".into()),
        ..parent.clone()
    };
    journal
        .acknowledge(row.id, claimed.attempt.unwrap(), original.clone())
        .unwrap();
    drop(journal);
    Actual {
        temp,
        plan,
        parent,
        original,
    }
}
#[test]
fn owned_numbers_data_registration_binds_exact_phase_scope() {
    let actual = actual();
    actual.plan.validate().unwrap();
    for arm in 0..7 {
        let mut plan = actual.plan.clone();
        match arm {
            0 => plan.account = Uuid::nil(),
            1 => plan.run = Uuid::new_v4(),
            2 => plan.label = "foreign".into(),
            3 => plan.create = plan.parent_creation,
            4 => plan.save = Some(Uuid::new_v4()),
            5 => plan.source_a.path = plan.source_b.path.clone(),
            _ => plan.source_b.sha256 = plan.source_a.sha256.clone(),
        }
        assert!(plan.validate().is_err(), "scope arm {arm} accepted");
    }
    let mut schema = serde_json::to_value(&actual.plan).unwrap();
    schema["semantic"] = serde_json::json!({"version":2});
    assert!(serde_json::from_value::<Registration>(schema).is_err());
}
#[test]
fn owned_numbers_data_actual_create_active_and_retired() {
    let actual = actual();
    let (parent, current, backup, _) = actual.check().unwrap();
    assert_eq!(parent, actual.parent);
    assert_eq!(current, actual.original);
    assert!(backup.is_none());
    actual.retire(current);
    actual.check().unwrap();
}
#[test]
fn owned_numbers_data_actual_save_chained_and_rehydrated() {
    for rehydrate in [false, true] {
        let mut actual = actual();
        actual.check().unwrap();
        actual.save(rehydrate);
        let (_, current, backup, _) = actual.check().unwrap();
        let backup = backup.unwrap();
        assert_ne!(current.id, actual.original.id);
        assert_eq!(backup.id, actual.original.id);
        assert_eq!(backup.size, actual.plan.source_a.size);
        actual.retire(current);
        actual.check().unwrap();
    }
}
#[test]
fn owned_numbers_data_actual_association_refuses_foreign_owner() {
    let actual = actual();
    actual.check().unwrap();
    let journal = actual.writer();
    let parent = journal
        .namespace_by_remote(&actual.plan.scope(), &actual.parent.id)
        .unwrap()
        .unwrap();
    actual
        .db()
        .execute(
            "UPDATE namespace_operations SET object=?2 WHERE operation=?1",
            rusqlite::params![actual.plan.create.to_string(), parent.id.to_string()],
        )
        .unwrap();
    drop(journal);
    assert!(
        actual.check().is_err(),
        "foreign ordinary create association accepted"
    );
}
#[test]
fn owned_numbers_data_receipts_refuse_foreign_revision_representation_and_source() {
    for arm in 0..8 {
        let mut actual = actual();
        actual.save(false);
        actual.check().unwrap();
        let journal = actual.writer();
        let mut row = journal.get(actual.plan.save.unwrap()).unwrap();
        match arm {
            0 => row.scope.account = Uuid::new_v4().to_string(),
            1 => {
                row.intent = UploadIntent::Replace {
                    item: actual.original.id.clone(),
                    expected_etag: "foreign-revision".into(),
                }
            }
            2 => row.sha256 = actual.plan.source_a.sha256.clone(),
            3 => row.remote.as_mut().unwrap().package = true,
            4 => row.working_file = Some(Uuid::new_v4()),
            5 => row.base.as_mut().unwrap().resolved = false,
            6 => row.remote.as_mut().unwrap().id = actual.original.id.clone(),
            _ => {
                let mut value =
                    serde_json::to_value(row.identity_handoff.as_ref().unwrap()).unwrap();
                value["recovery_object"] = serde_json::json!(Uuid::new_v4());
                row.identity_handoff = Some(serde_json::from_value(value).unwrap());
            }
        }
        actual
            .db()
            .execute(
                "UPDATE uploads SET body=?2 WHERE id=?1",
                rusqlite::params![row.id.to_string(), serde_json::to_string(&row).unwrap()],
            )
            .unwrap();
        drop(journal);
        assert!(actual.check().is_err(), "receipt arm {arm} accepted");
    }
}
#[test]
fn owned_numbers_data_actual_frontier_refuses_dirty_extra_and_unpublished() {
    for arm in 0..5 {
        let actual = actual();
        actual.check().unwrap();
        let mut journal = actual.writer();
        match arm {
            0 => {
                journal
                    .write_working(actual.plan.working_a, 0, b"dirty")
                    .unwrap();
            }
            1 => {
                journal
                    .create_namespace_directory(
                        actual.plan.scope(),
                        cirrove_icloud::ROOT_ID.into(),
                        "unregistered".into(),
                    )
                    .unwrap();
            }
            2 => {
                actual
                    .db()
                    .execute(
                        "INSERT INTO write_queue(sequence,id,complete) VALUES(999,?1,1)",
                        [Uuid::new_v4().to_string()],
                    )
                    .unwrap();
            }
            3 => {
                actual
                    .db()
                    .execute(
                        "INSERT INTO package_metadata_publication(operation,done) VALUES(?1,1)",
                        [actual.plan.create.to_string()],
                    )
                    .unwrap();
            }
            _ => {
                let mut owner = journal.namespace_object(actual.plan.owner).unwrap();
                owner.remote_sequence = 0;
                actual
                    .db()
                    .execute(
                        "UPDATE namespace_objects SET body=?2 WHERE id=?1",
                        rusqlite::params![
                            owner.id.to_string(),
                            serde_json::to_string(&owner).unwrap()
                        ],
                    )
                    .unwrap();
            }
        }
        drop(journal);
        assert!(actual.check().is_err(), "frontier arm {arm} accepted");
    }
}
#[test]
fn owned_numbers_data_sources_refuse_raw_digest_size_and_private_path() {
    let actual = actual();
    actual.plan.sources().verify().unwrap();
    let mut source = actual.plan.source_a.clone();
    source.sha256 = actual.plan.source_b.sha256.clone();
    assert!(source.verify().is_err());
    source = actual.plan.source_a.clone();
    source.size += 1;
    assert!(source.verify().is_err());
    let alias = actual.temp.path().join("alias.numbers");
    std::os::unix::fs::symlink(&actual.plan.source_a.path, &alias).unwrap();
    source = actual.plan.source_a.clone();
    source.path = alias;
    assert!(source.verify().is_err());
    let bytes = serde_json::to_vec(&actual.plan.sources()).unwrap();
    let digest = hex::encode(Sha256::digest(&bytes));
    decode::<Sources>(&bytes, &digest).unwrap();
    let registration = actual.temp.path().join("source-registration.json");
    std::fs::write(&registration, &bytes).unwrap();
    std::fs::set_permissions(&registration, std::fs::Permissions::from_mode(0o600)).unwrap();
    let proof = icloud_owned_numbers_data_source_verify(&registration, &digest).unwrap();
    assert_eq!(proof["raw_source_identity_verified"], true);
    assert_eq!(proof["representation_observed"], false);
    assert_eq!(proof["cloud_mutated"], false);
    let mut changed = bytes;
    changed.push(b' ');
    assert!(decode::<Sources>(&changed, &digest).is_err());
}
fn prior_artifacts(actual: &mut Actual) -> serde_json::Value {
    let mut prior = actual.plan.clone();
    prior.phase = Phase::CreatedA;
    prior.save = None;
    prior.working_b = None;
    prior.created_proof_sha256 = None;
    let registration = serde_json::to_vec(&prior).unwrap();
    std::fs::write(
        actual.temp.path().join("created-registration.json"),
        &registration,
    )
    .unwrap();
    let fixture = b"synthetic independent DATA fixture registration";
    std::fs::write(actual.temp.path().join("created-fixture.json"), fixture).unwrap();
    let fixture_sha = hex::encode(Sha256::digest(fixture));
    let proof = serde_json::json!({"run":prior.run,"account":prior.account,"phase":"created_a",
        "parent_creation":prior.parent_creation,"create":prior.create,"save":null,"owner":prior.owner,
        "working_a":prior.working_a,"working_b":null,"created_proof_sha256":null,
        "registration_sha256":hex::encode(Sha256::digest(registration)),"fixture_manifest_sha256":fixture_sha,
        "current":{"run":prior.run,"account":prior.account,"parent":actual.parent.id,
            "item":actual.original.id,"etag":actual.original.etag,"representation":"data","format":"numbers",
            "size":prior.source_a.size,"sha256":prior.source_a.sha256,"semantic":null,
            "manifest_sha256":fixture_sha,"content_identity_verified":true,"gui_fidelity_verified":false,"cloud_mutated":false},
        "trash":null,"ordinary_data_create_verified":true,"ordinary_data_save_verified":false,
        "cloud_mutated":false,"gui_fidelity_verified":false});
    let bytes = serde_json::to_vec(&proof).unwrap();
    std::fs::write(actual.temp.path().join("created-verified.json"), &bytes).unwrap();
    for name in [
        "created-registration.json",
        "created-fixture.json",
        "created-verified.json",
    ] {
        std::fs::set_permissions(
            actual.temp.path().join(name),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
    actual.plan.created_proof_sha256 = Some(hex::encode(Sha256::digest(bytes)));
    proof
}
#[test]
fn owned_numbers_data_prior_create_proof_refuses_absent_digest_identity_and_representation() {
    let mut actual = actual();
    actual.save(false);
    let value = prior_artifacts(&mut actual);
    prior_created(&actual.plan, &actual.original, &actual.parent).unwrap();
    let mut absent = actual.plan.clone();
    absent.created_proof_sha256 = None;
    assert!(prior_created(&absent, &actual.original, &actual.parent).is_err());
    let mut wrong = actual.plan.clone();
    wrong.created_proof_sha256 = Some("e".repeat(64));
    assert!(prior_created(&wrong, &actual.original, &actual.parent).is_err());
    for arm in 0..8 {
        let mut changed = value.clone();
        match arm {
            0 => changed["run"] = serde_json::json!(Uuid::new_v4()),
            1 => changed["account"] = serde_json::json!(Uuid::new_v4()),
            2 => {
                changed["current"]["item"] = serde_json::json!("FILE::com.apple.CloudDocs::foreign")
            }
            3 => changed["current"]["etag"] = serde_json::json!("other-revision"),
            4 => changed["current"]["sha256"] = serde_json::json!(actual.plan.source_b.sha256),
            5 => changed["current"]["representation"] = serde_json::json!("package"),
            6 => changed["current"]["size"] = serde_json::json!(actual.plan.source_a.size + 1),
            _ => changed["owner"] = serde_json::json!(Uuid::new_v4()),
        }
        let bytes = serde_json::to_vec(&changed).unwrap();
        let mut plan = actual.plan.clone();
        plan.created_proof_sha256 = Some(hex::encode(Sha256::digest(&bytes)));
        assert!(
            created_proof_binding(&plan, &actual.original, &actual.parent, &bytes).is_err(),
            "prior proof arm {arm} accepted"
        );
    }
    std::fs::write(
        actual.temp.path().join("created-fixture.json"),
        b"changed registration",
    )
    .unwrap();
    assert!(prior_created(&actual.plan, &actual.original, &actual.parent).is_err());
}
#[test]
fn owned_numbers_data_actual_readonly_observation_preserves_journal() {
    let mut actual = actual();
    actual.save(true);
    {
        let _journal = actual.writer();
        actual
            .db()
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .unwrap();
    }
    let path = actual.path().join("uploads.db");
    let before = std::fs::read(&path).unwrap();
    let first = actual.check().unwrap();
    let second = actual.check().unwrap();
    assert_eq!(first, second);
    assert_eq!(std::fs::read(path).unwrap(), before);
}

#[test]
fn owned_numbers_data_actual_queue_and_columns_refuse_substitution() {
    for arm in 0..5 {
        let actual = actual();
        actual.check().unwrap();
        let db = actual.db();
        match arm {
            0 => {
                db.execute(
                    "UPDATE write_queue SET id=?2 WHERE id=?1",
                    rusqlite::params![actual.plan.create.to_string(), Uuid::new_v4().to_string()],
                )
                .unwrap();
            }
            1 => {
                db.execute(
                    "UPDATE uploads SET state='failed' WHERE id=?1",
                    [actual.plan.create.to_string()],
                )
                .unwrap();
            }
            2 => {
                db.execute(
                    "UPDATE uploads SET id=?2 WHERE id=?1",
                    rusqlite::params![actual.plan.create.to_string(), Uuid::new_v4().to_string()],
                )
                .unwrap();
            }
            3 => {
                db.execute(
                    "UPDATE uploads SET sequence=999 WHERE id=?1",
                    [actual.plan.create.to_string()],
                )
                .unwrap();
            }
            _ => {
                db.execute(
                    "UPDATE mutations SET state='pending' WHERE id=?1",
                    [actual.plan.parent_creation.to_string()],
                )
                .unwrap();
            }
        }
        drop(db);
        if arm == 0 {
            assert!(
                actual.check().is_err(),
                "same-count queue substitution accepted"
            );
        } else {
            assert!(
                actual.check().is_err(),
                "SQL/body column substitution arm {arm} accepted"
            );
        }
    }
}

#[test]
fn owned_numbers_data_actual_namespace_operation_inventory_refuses_substitution() {
    let mut rejected = Vec::new();
    for arm in 0..3 {
        let actual = actual();
        actual.check().unwrap();
        let db = actual.db();
        let changed = match arm {
            0 => db.execute(
                "INSERT INTO namespace_operations(operation,object) VALUES(?1,?2)",
                rusqlite::params![Uuid::new_v4().to_string(), actual.plan.owner.to_string()],
            ),
            1 => db.execute(
                "DELETE FROM namespace_operations WHERE operation=?1",
                [actual.plan.parent_creation.to_string()],
            ),
            _ => db.execute(
                "UPDATE namespace_operations SET object=?2 WHERE operation=?1",
                rusqlite::params![
                    actual.plan.parent_creation.to_string(),
                    actual.plan.owner.to_string()
                ],
            ),
        }
        .unwrap();
        assert_eq!(changed, 1);
        drop(db);
        rejected.push(actual.check().is_err());
    }
    assert_eq!(
        rejected,
        vec![true; 3],
        "complete namespace operation inventory accepted malformed pairs"
    );
}

#[test]
fn owned_numbers_data_retired_etag_token_omission_preserves_exact_revision() {
    let mut accepted = Vec::new();
    for saved in [false, true] {
        let mut actual = actual();
        if saved {
            actual.save(false);
        }
        actual.check().unwrap();
        let current = {
            let journal = actual.writer();
            journal
                .get(actual.plan.save.unwrap_or(actual.plan.create))
                .unwrap()
                .remote
                .unwrap()
        };
        assert_eq!(current.content_version, current.etag);
        let observed = Node {
            content_version: None,
            ..current
        };
        // Genuine clean handoff stores the metadata observation, not the receipt.
        actual.retire(observed);
        accepted.push(actual.check().is_ok());
    }
    assert_eq!(
        accepted,
        vec![true; 2],
        "retired DATA A/B metadata token omission refused"
    );
    for arm in 0..8 {
        let mut actual = actual();
        if arm == 7 {
            actual.save(false);
        }
        actual.check().unwrap();
        if arm >= 6 {
            let changed = actual.db().execute(
                "UPDATE namespace_objects SET body=json_set(body,'$.remote.content_version',NULL) WHERE id=?1",
                [if arm == 6 {
                    actual.plan.owner.to_string()
                } else {
                    let journal = actual.writer();
                    journal.get(actual.plan.save.unwrap()).unwrap()
                        .ordinary_validation_recovery_owner().unwrap().to_string()
                }],
            ).unwrap();
            assert_eq!(changed, 1);
        } else {
            let mut observed = Node {
                content_version: None,
                ..actual.original.clone()
            };
            match arm {
                0 => observed.content_version = Some("different-content-token".into()),
                1 => observed.etag = Some("different-revision".into()),
                2 => observed.size += 1,
                3 => observed.modified_unix += 1,
                4 => observed.parent_id = Some("FOLDER::com.apple.CloudDocs::other-parent".into()),
                _ => observed.name = "other.numbers".into(),
            }
            actual.retire(observed);
        }
        assert!(
            actual.check().is_err(),
            "token omission guard arm {arm} accepted"
        );
    }
}
