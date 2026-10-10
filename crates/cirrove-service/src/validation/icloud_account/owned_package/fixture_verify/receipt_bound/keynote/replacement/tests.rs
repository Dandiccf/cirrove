#![allow(clippy::unwrap_used)]
use super::*;
use crate::journal::UploadJournal;
use cirrove_core::upload::{PackageHandoffReceipt, PackageUploadReceipt, RecoveryLocation};

fn source(root: &Path, name: &str, text: &[u8]) -> Source {
    let path = root.join(name);
    let bytes = crate::native_import::synthetic_package_archive("Source.key/Metadata/data", text);
    std::fs::write(&path, &bytes).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let receipt = PackageDownload {
        size: bytes.len() as u64,
        sha256: hex::encode(Sha256::digest(&bytes)),
    };
    let semantic = cirrove_icloud::package_archive_semantic_identity_versioned(
        &File::open(&path).unwrap(),
        &receipt,
        "Source.key",
        2,
        &CancellationToken::new(),
    )
    .unwrap();
    Source {
        path,
        size: receipt.size,
        sha256: receipt.sha256,
        root: Some("Source.key".into()),
        semantic,
    }
}
fn fixture(post: bool) -> (PathBuf, Plan, Node, Option<Node>) {
    let run = Uuid::new_v4();
    let temp = tempfile::Builder::new()
        .prefix(&format!("cirrove-keynote-replacement-{run}"))
        .rand_bytes(0)
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/var/tmp")
        .unwrap()
        .keep();
    let root = temp.as_path();
    let a = source(root, "source-a.key", b"owned title A");
    let b = source(root, "source-b.key", b"owned edited title B");
    let account = Uuid::new_v4();
    let time = now().unwrap();
    let mut plan = Plan {
        version: 1,
        phase: Phase::Preflight,
        run,
        account,
        label: "iCloudKeynoteReplacementValidation".into(),
        session_directory: root.into(),
        settings_sha256: "a".repeat(64),
        parent_creation: Uuid::new_v4(),
        import: Uuid::new_v4(),
        replacement: None,
        original: Node {
            id: "FILE::com.apple.CloudDocs::original-a".into(),
            parent_id: Some("FOLDER::com.apple.CloudDocs::parent".into()),
            name: format!("Cirrove-Keynote-Replacement-{run}.key"),
            kind: NodeKind::Folder,
            package: true,
            target: None,
            content_version: None,
            etag: Some("a-r1".into()),
            size: a.semantic.expanded_bytes,
            modified_unix: 0,
        },
        source_layout: PackageSourceLayout::Wrapped,
        source_a: a,
        source_b: b,
        started_unix: time,
        deadline_unix: time + 600,
    };
    let parent = Node {
        id: "FOLDER::com.apple.CloudDocs::parent".into(),
        parent_id: Some(cirrove_icloud::ROOT_ID.into()),
        name: plan.parent_name(),
        kind: NodeKind::Folder,
        package: false,
        target: None,
        content_version: None,
        etag: Some("parent-r1".into()),
        size: 0,
        modified_unix: 0,
    };
    let mut journal = UploadJournal::open(
        &root.join("journal"),
        &account.to_string(),
        16 * 1024 * 1024,
    )
    .unwrap();
    let object = journal
        .create_namespace_directory(
            plan.scope(),
            cirrove_icloud::ROOT_ID.into(),
            parent.name.clone(),
        )
        .unwrap();
    let mutation = journal.claim_mutation().unwrap().unwrap();
    plan.parent_creation = mutation.id;
    journal
        .acknowledge_mutation(
            mutation.id,
            mutation.attempt.unwrap(),
            MutationReceipt::Upsert(parent.clone()),
        )
        .unwrap();
    let local = journal.namespace_object(object.id).unwrap();
    journal
        .handoff_namespace(local.id, local.revision, parent.clone())
        .unwrap();
    let archive = crate::native_import::ValidatedPackageArchive::capture(
        &plan.source_a.path,
        root,
        "Source.key",
        &CancellationToken::new(),
    )
    .unwrap();
    plan.import = journal
        .enqueue_validated_package_archive(
            plan.scope(),
            UploadIntent::Create {
                parent: parent.id.clone(),
                name: plan.name(Format::Keynote),
            },
            archive,
            &CancellationToken::new(),
        )
        .unwrap()
        .id;
    let row = journal.claim_next().unwrap().unwrap();
    journal
        .acknowledge_package(
            row.id,
            row.attempt.unwrap(),
            PackageUploadReceipt {
                remote: plan.original.clone(),
                semantic: plan.source_a.semantic.clone(),
            },
        )
        .unwrap();
    let row = journal.get(plan.import).unwrap();
    journal
        .finish_package_publication(
            &row,
            PackagePublicationStatus::Present(plan.original.clone()),
            0,
        )
        .unwrap();
    let mut current = plan.original.clone();
    let mut backup = None;
    if post {
        plan.phase = Phase::Postflight;
        let archive = crate::native_import::ValidatedPackageArchive::capture(
            &plan.source_b.path,
            root,
            "Source.key",
            &CancellationToken::new(),
        )
        .unwrap();
        let row = journal
            .enqueue_validated_package_replacement(
                plan.scope(),
                plan.original.clone(),
                plan.source_a.semantic.clone(),
                archive,
                &CancellationToken::new(),
            )
            .unwrap();
        plan.replacement = Some(row.id);
        let row = journal.claim_next().unwrap().unwrap();
        let attempt = row.attempt.unwrap();
        journal
            .reserve_identity_handoff(
                row.id,
                attempt,
                RecoveryLocation::Trash {
                    local_name: "owned-recovery-A".into(),
                    parent: "FOLDER::com.apple.CloudDocs::TRASH_ROOT".into(),
                },
            )
            .unwrap();
        current.id = "FILE::com.apple.CloudDocs::current-b".into();
        current.etag = Some("b-r1".into());
        current.size = plan.source_b.semantic.expanded_bytes;
        let old = Node {
            parent_id: Some("FOLDER::com.apple.CloudDocs::TRASH_ROOT".into()),
            etag: Some("trash-a-r2".into()),
            ..plan.original.clone()
        };
        journal
            .acknowledge_package_handoff(
                row.id,
                attempt,
                PackageHandoffReceipt {
                    original: plan.original.clone(),
                    current: PackageUploadReceipt {
                        remote: current.clone(),
                        semantic: plan.source_b.semantic.clone(),
                    },
                    backup: PackageUploadReceipt {
                        remote: old.clone(),
                        semantic: plan.source_a.semantic.clone(),
                    },
                },
            )
            .unwrap();
        let row = journal.get(row.id).unwrap();
        journal
            .finish_package_publication(&row, PackagePublicationStatus::Present(current.clone()), 0)
            .unwrap();
        backup = Some(old);
    }
    drop(journal);
    (temp, plan, current, backup)
}
fn bound(plan: &Plan) -> Result<(Node, Node, Option<Node>)> {
    plan.validate_at(now()?, Format::Keynote)?;
    admitted(
        plan,
        &snapshot(
            &plan.session_directory.join("journal/uploads.db"),
            plan.account,
        )?,
        Format::Keynote,
    )
}
#[test]
fn owned_keynote_replacement_actual_ack_binds_current_and_trash_readonly() {
    for post in [false, true] {
        let (_temp, plan, current, backup) = fixture(post);
        let db = plan.session_directory.join("journal/uploads.db");
        let before = std::fs::read(&db).unwrap();
        let owner = RecoveryJournal::open(db.parent().unwrap(), &plan.account.to_string()).unwrap();
        let (_, actual_current, actual_backup) = bound(&plan).unwrap();
        assert_eq!(actual_current, current);
        assert_eq!(actual_backup, backup);
        assert_eq!(std::fs::read(&db).unwrap(), before);
        drop(owner);
        assert_eq!(std::fs::read(&db).unwrap(), before);
    }
}
#[test]
fn owned_keynote_replacement_rejects_scope_layout_and_unchanged_pair() {
    let (_temp, plan, _, _) = fixture(true);
    bound(&plan).unwrap();
    for arm in 0..10 {
        let mut bad = plan.clone();
        match arm {
            0 => bad.account = Uuid::new_v4(),
            1 => bad.original.etag = Some("*".into()),
            2 => bad.source_b.root = Some("Source.numbers".into()),
            3 => bad.source_b.semantic = bad.source_a.semantic.clone(),
            4 => bad.source_b.sha256 = bad.source_a.sha256.clone(),
            5 => bad.replacement = Some(bad.import),
            6 => bad.deadline_unix = bad.started_unix + 601,
            7 => bad.original.parent_id = Some(cirrove_icloud::ROOT_ID.into()),
            8 => bad.phase = Phase::Preflight,
            _ => bad.original.id = "FILE::com.apple.CloudDocs::foreign".into(),
        }
        assert!(bound(&bad).is_err(), "authority arm {arm} accepted");
    }
}
#[test]
fn owned_keynote_replacement_rejects_sql_body_sequence_and_queue_corruption() {
    for arm in 0..6 {
        let (_temp, plan, _, _) = fixture(true);
        bound(&plan).unwrap();
        let path = plan.session_directory.join("journal/uploads.db");
        let db = rusqlite::Connection::open(&path).unwrap();
        let id = plan.replacement.unwrap().to_string();
        match arm {
            0 => {
                db.execute("UPDATE uploads SET sequence=sequence+50 WHERE id=?1", [&id])
                    .unwrap();
            }
            1 => {
                db.execute("UPDATE write_queue SET complete=0 WHERE id=?1", [&id])
                    .unwrap();
            }
            2 => {
                db.execute("DELETE FROM write_queue WHERE id=?1", [&id])
                    .unwrap();
            }
            3 => {
                db.execute(
                    "UPDATE package_metadata_publication SET done=0 WHERE operation=?1",
                    [&id],
                )
                .unwrap();
            }
            4 => {
                db.execute(
                    "INSERT INTO write_queue(id,complete) VALUES(?1,1)",
                    [Uuid::new_v4().to_string()],
                )
                .unwrap();
            }
            _ => {
                let body: String = db
                    .query_row("SELECT body FROM uploads WHERE id=?1", [&id], |r| r.get(0))
                    .unwrap();
                let mut v: serde_json::Value = serde_json::from_str(&body).unwrap();
                v["scope"]["collection"] = "foreign".into();
                db.execute(
                    "UPDATE uploads SET body=?2 WHERE id=?1",
                    [&id, &v.to_string()],
                )
                .unwrap();
            }
        }
        drop(db);
        let before = std::fs::read(&path).unwrap();
        assert!(bound(&plan).is_err(), "SQL arm {arm} accepted");
        assert_eq!(std::fs::read(path).unwrap(), before);
    }
}
#[test]
fn owned_keynote_replacement_rejects_wrong_handoff_identity_revision_and_semantic() {
    let (_temp, plan, _, _) = fixture(true);
    let baseline = snapshot(
        &plan.session_directory.join("journal/uploads.db"),
        plan.account,
    )
    .unwrap();
    admitted(&plan, &baseline, Format::Keynote).unwrap();
    for arm in 0..7 {
        let mut s = snapshot(
            &plan.session_directory.join("journal/uploads.db"),
            plan.account,
        )
        .unwrap();
        let row = s
            .uploads
            .iter_mut()
            .find(|r| Some(r.id) == plan.replacement)
            .unwrap();
        let mut value = serde_json::to_value(&*row).unwrap();
        match arm {
            0 => {
                value["identity_handoff"]["backup"]["id"] =
                    "FILE::com.apple.CloudDocs::foreign".into()
            }
            1 => value["identity_handoff"]["backup"]["parent_id"] = cirrove_icloud::ROOT_ID.into(),
            2 => value["identity_handoff"]["backup"]["etag"] = "*".into(),
            3 => value["remote"]["etag"] = serde_json::Value::Null,
            4 => {
                value["package_completion"] = serde_json::to_value(&plan.source_a.semantic).unwrap()
            }
            5 => {
                value["representation"]["original_semantic"] =
                    serde_json::to_value(&plan.source_b.semantic).unwrap()
            }
            _ => value["identity_handoff"] = serde_json::Value::Null,
        }
        *row = serde_json::from_value(value).unwrap();
        assert!(
            admitted(&plan, &s, Format::Keynote).is_err(),
            "handoff arm {arm} accepted"
        );
    }
}
#[test]
fn owned_keynote_replacement_source_member_tamper_refuses_without_changes() {
    let (_temp, plan, _, _) = fixture(false);
    source_verified(&plan.source_a).unwrap();
    source_verified(&plan.source_b).unwrap();
    let path = &plan.source_b.path;
    let mut bytes = std::fs::read(path).unwrap();
    bytes[0] ^= 1;
    std::fs::write(path, &bytes).unwrap();
    assert!(source_verified(&plan.source_b).is_err());
    assert_eq!(std::fs::read(path).unwrap(), bytes);
    let mut wrong = plan.source_a.clone();
    wrong.semantic = plan.source_b.semantic.clone();
    assert!(source_verified(&wrong).is_err());
}
#[test]
fn owned_keynote_replacement_manifest_requires_explicit_fields_and_original_clock() {
    let (_temp, plan, _, _) = fixture(false);
    let v = serde_json::to_value(&plan).unwrap();
    for key in [
        "source_a",
        "source_b",
        "original",
        "started_unix",
        "deadline_unix",
    ] {
        let mut bad = v.clone();
        bad.as_object_mut().unwrap().remove(key);
        assert!(serde_json::from_value::<Plan>(bad).is_err());
    }
    let mut bad = v;
    bad["provider_secret"] = "untrusted".into();
    assert!(serde_json::from_value::<Plan>(bad).is_err());
    assert!(
        plan.validate_at(plan.started_unix - 1, Format::Keynote)
            .is_err()
    );
    assert!(
        plan.validate_at(plan.deadline_unix, Format::Keynote)
            .is_err()
    );
}
#[tokio::test]
async fn owned_keynote_replacement_public_failure_discards_untrusted_causes() {
    let (_temp, plan, _, _) = fixture(false);
    let path = plan
        .session_directory
        .join("replacement-preflight-registration.json");
    std::fs::write(&path, b"untrusted-token-or-signed-url").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let failure = icloud_owned_keynote_replacement_receipt_verify(&path, &"a".repeat(64))
        .await
        .unwrap_err();
    assert_eq!(
        failure.to_string(),
        "owned Keynote replacement observation refused; no mutation submitted"
    );
    assert_eq!(failure.chain().count(), 1);
    assert!(!format!("{failure:?}").contains("untrusted"));
}

#[test]
fn owned_keynote_replacement_synchronous_publication_overrun_is_not_success() {
    let temp = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/var/tmp")
        .unwrap()
        .keep();
    let target = temp.join("retained-receipt.json");
    let active_end = tokio::time::Instant::now() + Duration::from_millis(10);
    let result = publish_result(active_end, || {
        record(&target, &serde_json::json!({"synthetic":true}))?;
        std::thread::sleep(Duration::from_millis(20));
        Ok(())
    });
    assert!(target.exists(), "original publication did not execute");
    assert!(
        result.is_err(),
        "late retained receipt became successful endpoint"
    );
}

#[test]
fn owned_keynote_replacement_synchronous_work_cannot_dispatch_after_deadline() {
    let active_end = tokio::time::Instant::now() + Duration::from_millis(10);
    std::thread::sleep(Duration::from_millis(20));
    let dispatched = std::cell::Cell::new(false);
    let result = (|| -> Result<()> {
        before_dispatch(active_end)?;
        dispatched.set(true);
        Ok(())
    })();
    assert!(
        result.is_err(),
        "expired synchronous work dispatched provider phase"
    );
    assert!(
        !dispatched.get(),
        "operation was constructed after original deadline"
    );
}

#[test]
fn owned_keynote_replacement_nested_sync_work_cannot_dispatch_after_deadline() {
    let end = tokio::time::Instant::now() + Duration::from_millis(10);
    std::thread::sleep(Duration::from_millis(20));
    let dispatched = std::cell::Cell::new(false);
    let result = (|| -> Result<()> {
        super::super::super::super::fixture_active(Some(end))?;
        dispatched.set(true);
        Ok(())
    })();
    assert!(result.is_err());
    assert!(!dispatched.get());
    // Existing unbounded public generic caller retains its historical policy.
    super::super::super::super::fixture_active(None).unwrap();
}
#[test]
fn owned_keynote_replacement_nested_sync_publication_overrun_is_not_success() {
    let temp = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/var/tmp")
        .unwrap()
        .keep();
    let target = temp.join("nested-receipt.json");
    let end = tokio::time::Instant::now() + Duration::from_millis(10);
    let result = super::super::super::super::fixture_publish(Some(end), || {
        record(&target, &serde_json::json!({"synthetic":true}))?;
        std::thread::sleep(Duration::from_millis(20));
        Ok(())
    });
    assert!(target.exists());
    assert!(
        result.is_err(),
        "nested late receipt became successful endpoint"
    );
}
