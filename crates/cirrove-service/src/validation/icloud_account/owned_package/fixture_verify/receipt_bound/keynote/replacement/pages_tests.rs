#![allow(clippy::unwrap_used)]
use super::*;
use crate::journal::UploadJournal;
use cirrove_core::upload::{PackageHandoffReceipt, PackageUploadReceipt, RecoveryLocation};

fn source_for(root: &Path, name: &str, text: &[u8], layout: PackageSourceLayout) -> Source {
    let path = root.join(name);
    let bytes = match layout {
        PackageSourceLayout::Wrapped => {
            crate::native_import::synthetic_package_archive("Source.pages/Metadata/data", text)
        }
        PackageSourceLayout::FlatPages => local_size_mirror(
            crate::native_import::synthetic_package_archive("Metadata/data", text),
        ),
        _ => panic!("unsupported synthetic source layout"),
    };
    std::fs::write(&path, &bytes).unwrap();
    std::fs::set_permissions(
        &path,
        std::fs::Permissions::from_mode(if layout == PackageSourceLayout::FlatPages {
            0o400
        } else {
            0o600
        }),
    )
    .unwrap();
    let receipt = PackageDownload {
        size: bytes.len() as u64,
        sha256: hex::encode(Sha256::digest(&bytes)),
    };
    let semantic = match layout {
        PackageSourceLayout::Wrapped => {
            cirrove_icloud::package_archive_semantic_identity_versioned(
                &File::open(&path).unwrap(),
                &receipt,
                "Source.pages",
                2,
                &CancellationToken::new(),
            )
            .unwrap()
        }
        PackageSourceLayout::FlatPages => {
            cirrove_icloud::package_flat_archive_semantic_identity_v2(
                &File::open(&path).unwrap(),
                &receipt,
                &CancellationToken::new(),
            )
            .unwrap()
        }
        _ => panic!("unsupported synthetic source layout"),
    };
    Source {
        path,
        size: receipt.size,
        sha256: receipt.sha256,
        root: (layout == PackageSourceLayout::Wrapped).then(|| "Source.pages".into()),
        semantic,
    }
}
fn fixture(post: bool) -> (PathBuf, Plan, Node, Option<Node>) {
    fixture_for(post, PackageSourceLayout::Wrapped)
}
fn fixture_for(post: bool, layout: PackageSourceLayout) -> (PathBuf, Plan, Node, Option<Node>) {
    let run = Uuid::new_v4();
    let temp = tempfile::Builder::new()
        .prefix(&format!("cirrove-pages-replacement-{run}"))
        .rand_bytes(0)
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/var/tmp")
        .unwrap()
        .keep();
    let root = temp.as_path();
    let a = source_for(root, "source-a.pages", b"owned title A", layout);
    let b = source_for(root, "source-b.pages", b"owned edited title B", layout);
    let account = Uuid::new_v4();
    let time = now().unwrap();
    let mut plan = Plan {
        version: 1,
        phase: Phase::Preflight,
        run,
        account,
        label: "iCloudPagesReplacementValidation".into(),
        session_directory: root.into(),
        settings_sha256: "a".repeat(64),
        parent_creation: Uuid::new_v4(),
        import: Uuid::new_v4(),
        replacement: None,
        original: Node {
            id: "FILE::com.apple.CloudDocs::original-a".into(),
            parent_id: Some("FOLDER::com.apple.CloudDocs::parent".into()),
            name: format!("Cirrove-Pages-Replacement-{run}.pages"),
            kind: NodeKind::Folder,
            package: true,
            target: None,
            content_version: None,
            etag: Some("a-r1".into()),
            size: a.semantic.expanded_bytes,
            modified_unix: 0,
        },
        source_layout: layout,
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
    let archive = crate::native_import::ValidatedPackageArchive::capture_with_source_layout(
        &plan.source_a.path,
        root,
        layout,
        plan.source_a.root.as_deref(),
        &CancellationToken::new(),
    )
    .unwrap();
    plan.import = journal
        .enqueue_validated_package_archive(
            plan.scope(),
            UploadIntent::Create {
                parent: parent.id.clone(),
                name: plan.name(Format::Pages),
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
        let archive = crate::native_import::ValidatedPackageArchive::capture_with_source_layout(
            &plan.source_b.path,
            root,
            layout,
            plan.source_b.root.as_deref(),
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
    plan.validate_at(now()?, Format::Pages)?;
    admitted(
        plan,
        &snapshot(
            &plan.session_directory.join("journal/uploads.db"),
            plan.account,
        )?,
        Format::Pages,
    )
}

#[test]
fn owned_pages_replacement_actual_ack_binds_current_and_trash_readonly() {
    for post in [false, true] {
        let (_retained, plan, current, backup) = fixture(post);
        source_verified(&plan.source_a).unwrap();
        source_verified(&plan.source_b).unwrap();
        let db = plan.session_directory.join("journal/uploads.db");
        let before = std::fs::read(&db).unwrap();
        let a = std::fs::read(&plan.source_a.path).unwrap();
        let b = std::fs::read(&plan.source_b.path).unwrap();
        let owner = RecoveryJournal::open(db.parent().unwrap(), &plan.account.to_string()).unwrap();
        let (_, actual_current, actual_backup) = bound(&plan).unwrap();
        assert_eq!(actual_current, current);
        assert_eq!(actual_backup, backup);
        assert_eq!(std::fs::read(&db).unwrap(), before);
        drop(owner);
        assert_eq!(std::fs::read(&db).unwrap(), before);
        assert_eq!(std::fs::read(&plan.source_a.path).unwrap(), a);
        assert_eq!(std::fs::read(&plan.source_b.path).unwrap(), b);
    }
}

#[test]
fn owned_pages_replacement_rejects_other_format_scope_and_unchanged_pair() {
    let (_retained, plan, _, _) = fixture(true);
    bound(&plan).unwrap();
    assert!(plan.validate_at(now().unwrap(), Format::Keynote).is_err());
    for arm in 0..14 {
        let mut bad = plan.clone();
        match arm {
            0 => bad.account = Uuid::new_v4(),
            1 => bad.label = "iCloudKeynoteReplacementValidation".into(),
            2 => {
                bad.session_directory =
                    PathBuf::from(format!("/var/tmp/cirrove-keynote-replacement-{}", bad.run))
            }
            3 => bad.source_b.root = Some("Source.key".into()),
            4 => bad.source_b.path = bad.session_directory.join("source-b.key"),
            5 => bad.source_b.semantic = bad.source_a.semantic.clone(),
            6 => bad.source_b.sha256 = bad.source_a.sha256.clone(),
            7 => bad.replacement = Some(bad.import),
            8 => bad.original.name = format!("Cirrove-Keynote-Replacement-{}.key", bad.run),
            9 => bad.original.parent_id = Some(cirrove_icloud::ROOT_ID.into()),
            10 => bad.original.id = "FILE::com.apple.CloudDocs::foreign".into(),
            11 => bad.original.etag = Some("*".into()),
            12 => {
                bad.original.kind = NodeKind::File;
                bad.original.package = false;
            }
            _ => bad.phase = Phase::Preflight,
        }
        assert!(bound(&bad).is_err(), "Pages authority arm {arm} accepted");
    }
}

#[test]
fn owned_pages_replacement_sql_sequence_cannot_borrow_completed_receipt() {
    let (_retained, plan, _, _) = fixture(true);
    bound(&plan).unwrap();
    let path = plan.session_directory.join("journal/uploads.db");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute(
        "UPDATE uploads SET sequence=sequence+50 WHERE id=?1",
        [plan.replacement.unwrap().to_string()],
    )
    .unwrap();
    drop(db);
    let retained = std::fs::read(&path).unwrap();
    assert!(
        bound(&plan).is_err(),
        "SQL sequence borrowed the genuine completed body/queue authority"
    );
    assert_eq!(std::fs::read(&path).unwrap(), retained);
}

#[test]
fn owned_pages_replacement_rejects_unfinished_queue_and_publication() {
    for arm in 0..5 {
        let (_retained, plan, _, _) = fixture(true);
        bound(&plan).unwrap();
        let path = plan.session_directory.join("journal/uploads.db");
        let db = rusqlite::Connection::open(&path).unwrap();
        let id = plan.replacement.unwrap().to_string();
        match arm {
            0 => {
                db.execute("UPDATE write_queue SET complete=0 WHERE id=?1", [&id])
                    .unwrap();
            }
            1 => {
                db.execute("DELETE FROM write_queue WHERE id=?1", [&id])
                    .unwrap();
            }
            2 => {
                db.execute(
                    "UPDATE package_metadata_publication SET done=0 WHERE operation=?1",
                    [&id],
                )
                .unwrap();
            }
            3 => {
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
                let mut value: serde_json::Value = serde_json::from_str(&body).unwrap();
                value["scope"]["collection"] = "foreign".into();
                db.execute(
                    "UPDATE uploads SET body=?2 WHERE id=?1",
                    [&id, &value.to_string()],
                )
                .unwrap();
            }
        }
        drop(db);
        let retained = std::fs::read(&path).unwrap();
        assert!(
            bound(&plan).is_err(),
            "Pages queue/publication arm {arm} accepted"
        );
        assert_eq!(std::fs::read(&path).unwrap(), retained);
    }
}

#[test]
fn owned_pages_replacement_rejects_changed_current_and_trash_identity() {
    let (_retained, plan, _, _) = fixture(true);
    let path = plan.session_directory.join("journal/uploads.db");
    admitted(
        &plan,
        &snapshot(&path, plan.account).unwrap(),
        Format::Pages,
    )
    .unwrap();
    for arm in 0..8 {
        let mut state = snapshot(&path, plan.account).unwrap();
        let row = state
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
                value["remote"]["name"] =
                    format!("Cirrove-Keynote-Replacement-{}.key", plan.run).into()
            }
            5 => {
                value["package_completion"] = serde_json::to_value(&plan.source_a.semantic).unwrap()
            }
            6 => {
                value["representation"]["original_semantic"] =
                    serde_json::to_value(&plan.source_b.semantic).unwrap()
            }
            _ => value["identity_handoff"] = serde_json::Value::Null,
        }
        *row = serde_json::from_value(value).unwrap();
        assert!(
            admitted(&plan, &state, Format::Pages).is_err(),
            "Pages handoff arm {arm} accepted"
        );
    }
}

#[test]
fn owned_pages_replacement_recomputed_raw_receipt_cannot_hide_member_tamper() {
    let (_retained, plan, _, _) = fixture(false);
    source_verified(&plan.source_b).unwrap();
    let original_a = std::fs::read(&plan.source_a.path).unwrap();
    let changed = crate::native_import::synthetic_package_archive(
        "Source.pages/Metadata/data",
        b"foreign complete valid archive member",
    );
    std::fs::write(&plan.source_b.path, &changed).unwrap();
    let mut changed_source = plan.source_b.clone();
    changed_source.size = changed.len() as u64;
    changed_source.sha256 = hex::encode(Sha256::digest(&changed));
    assert!(
        source_verified(&changed_source).is_err(),
        "valid raw receipt bypassed the frozen full semantic identity"
    );
    assert_eq!(std::fs::read(&plan.source_b.path).unwrap(), changed);
    assert_eq!(std::fs::read(&plan.source_a.path).unwrap(), original_a);
}

#[test]
fn owned_pages_replacement_manifest_requires_explicit_tuple_and_original_window() {
    let (_retained, plan, _, _) = fixture(false);
    let value = serde_json::to_value(&plan).unwrap();
    for key in [
        "source_a",
        "source_b",
        "original",
        "started_unix",
        "deadline_unix",
    ] {
        let mut missing = value.clone();
        missing.as_object_mut().unwrap().remove(key);
        assert!(serde_json::from_value::<Plan>(missing).is_err());
    }
    let mut injected = value;
    injected["format"] = "pages".into();
    assert!(
        serde_json::from_value::<Plan>(injected).is_err(),
        "caller-selected format entered the explicit observer wire"
    );
    assert!(
        plan.validate_at(plan.started_unix - 1, Format::Pages)
            .is_err()
    );
    assert!(plan.validate_at(plan.deadline_unix, Format::Pages).is_err());
    let mut long = plan.clone();
    long.deadline_unix = long.started_unix + 601;
    assert!(long.validate_at(long.started_unix, Format::Pages).is_err());
}

#[tokio::test]
async fn owned_pages_replacement_public_failure_discards_untrusted_causes() {
    let (_retained, plan, _, _) = fixture(false);
    let path = plan
        .session_directory
        .join("replacement-preflight-registration.json");
    std::fs::write(&path, b"untrusted-token-or-signed-url").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let error = super::super::super::pages::icloud_owned_pages_replacement_receipt_verify(
        &path,
        &"a".repeat(64),
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "owned Pages replacement observation refused; no mutation submitted"
    );
    assert_eq!(error.chain().count(), 1);
    assert!(!format!("{error:?}").contains("untrusted"));
}

fn local_size_mirror(mut bytes: Vec<u8>) -> Vec<u8> {
    let end = bytes.len() - 22;
    let central = u32::from_le_bytes(bytes[end + 16..end + 20].try_into().unwrap());
    let name = u16::from_le_bytes(bytes[26..28].try_into().unwrap()) as usize;
    let size = u32::from_le_bytes(bytes[22..26].try_into().unwrap());
    assert_eq!(u32::from_le_bytes(bytes[18..22].try_into().unwrap()), size);
    let mut extra = vec![1, 0, 16, 0];
    extra.extend(u64::from(size).to_le_bytes());
    extra.extend(u64::from(size).to_le_bytes());
    bytes[28..30].copy_from_slice(&20u16.to_le_bytes());
    bytes.splice(30 + name..30 + name, extra);
    bytes[end + 36..end + 40].copy_from_slice(&(central + 20).to_le_bytes());
    bytes
}

#[test]
fn owned_pages_flat_replacement_actual_ack_preserves_original_bytes_and_exact_handoff() {
    for post in [false, true] {
        let (_retained, plan, current, backup) = fixture_for(post, PackageSourceLayout::FlatPages);
        let a = std::fs::read(&plan.source_a.path).unwrap();
        let b = std::fs::read(&plan.source_b.path).unwrap();
        assert_eq!(
            &a[30 + u16::from_le_bytes(a[26..28].try_into().unwrap()) as usize..][..4],
            &[1, 0, 16, 0]
        );
        let db = plan.session_directory.join("journal/uploads.db");
        let before = std::fs::read(&db).unwrap();
        let owner = RecoveryJournal::open(db.parent().unwrap(), &plan.account.to_string()).unwrap();
        source_verified_for(&plan.source_a, plan.source_layout).unwrap();
        source_verified_for(&plan.source_b, plan.source_layout).unwrap();
        let (_, observed_current, observed_backup) = bound(&plan).unwrap();
        assert_eq!(observed_current, current);
        assert_eq!(observed_backup, backup);
        let state = snapshot(&db, plan.account).unwrap();
        assert!(
            state
                .uploads
                .iter()
                .find(|r| r.id == plan.import)
                .unwrap()
                .representation
                == UploadRepresentation::FlatPagesArchive {
                    semantic: plan.source_a.semantic.clone()
                },
            "flat Pages import representation changed"
        );
        if post {
            assert!(
                state
                    .uploads
                    .iter()
                    .find(|r| Some(r.id) == plan.replacement)
                    .unwrap()
                    .representation
                    == UploadRepresentation::FlatPagesReplacementArchive {
                        semantic: plan.source_b.semantic.clone(),
                        original: Box::new(plan.original.clone()),
                        original_semantic: plan.source_a.semantic.clone()
                    },
                "flat Pages replacement representation changed"
            );
        }
        drop(owner);
        assert_eq!(std::fs::read(&db).unwrap(), before);
        assert_eq!(std::fs::read(&plan.source_a.path).unwrap(), a);
        assert_eq!(std::fs::read(&plan.source_b.path).unwrap(), b);
        for source in [&plan.source_a, &plan.source_b] {
            assert_eq!(
                std::fs::metadata(&source.path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o400
            );
        }
    }
}

#[test]
fn owned_pages_flat_replacement_layout_is_explicit_null_root_and_v2_only() {
    let (_retained, plan, _, _) = fixture_for(false, PackageSourceLayout::FlatPages);
    bound(&plan).unwrap();
    let wire = serde_json::to_value(&plan).unwrap();
    assert_eq!(wire["source_layout"], "flat_pages");
    assert!(wire["source_a"]["root"].is_null());
    for arm in 0..7 {
        let mut bad = wire.clone();
        match arm {
            0 => {
                bad.as_object_mut().unwrap().remove("source_layout");
            }
            1 => bad["source_layout"] = "wrapped".into(),
            2 => bad["source_layout"] = "flat_numbers".into(),
            3 => bad["source_a"]["root"] = "Source.pages".into(),
            4 => {
                bad["source_b"].as_object_mut().unwrap().remove("root");
            }
            5 => bad["source_a"]["semantic"]["version"] = 1.into(),
            _ => bad["source_b"]["semantic"]["version"] = 1.into(),
        }
        let admitted = serde_json::from_value::<Plan>(bad)
            .ok()
            .is_some_and(|p| p.validate_at(p.started_unix, Format::Pages).is_ok());
        assert!(!admitted, "flat Pages explicit source arm {arm} accepted");
    }
    assert!(
        plan.validate_at(plan.started_unix, Format::Keynote)
            .is_err()
    );
}

#[test]
fn owned_pages_flat_replacement_typed_sql_receipt_cannot_be_relabelled_wrapped() {
    let (_retained, plan, _, _) = fixture_for(true, PackageSourceLayout::FlatPages);
    bound(&plan).unwrap();
    for replacing in [false, true] {
        let mut changed = snapshot(
            &plan.session_directory.join("journal/uploads.db"),
            plan.account,
        )
        .unwrap();
        let row = changed
            .uploads
            .iter_mut()
            .find(|r| {
                r.id == if replacing {
                    plan.replacement.unwrap()
                } else {
                    plan.import
                }
            })
            .unwrap();
        row.representation = if replacing {
            UploadRepresentation::PackageReplacementArchive {
                expected_root: "Source.pages".into(),
                semantic: plan.source_b.semantic.clone(),
                original: Box::new(plan.original.clone()),
                original_semantic: plan.source_a.semantic.clone(),
            }
        } else {
            UploadRepresentation::PackageArchive {
                expected_root: "Source.pages".into(),
                semantic: plan.source_a.semantic.clone(),
            }
        };
        assert!(
            admitted(&plan, &changed, Format::Pages).is_err(),
            "wrapped receipt borrowed flat Pages authority"
        );
    }
}

#[test]
fn owned_pages_flat_replacement_original_current_and_trash_identity_fences_remain_strict() {
    let (_retained, plan, _, _) = fixture_for(true, PackageSourceLayout::FlatPages);
    bound(&plan).unwrap();
    for arm in 0..6 {
        let mut state = snapshot(
            &plan.session_directory.join("journal/uploads.db"),
            plan.account,
        )
        .unwrap();
        let row = state
            .uploads
            .iter_mut()
            .find(|r| Some(r.id) == plan.replacement)
            .unwrap();
        let mut wire = serde_json::to_value(&*row).unwrap();
        match arm {
            0 => wire["scope"]["account"] = Uuid::new_v4().to_string().into(),
            1 => wire["intent"]["expected_etag"] = "foreign-revision".into(),
            2 => {
                wire["representation"]["original"]["id"] =
                    "FILE::com.apple.CloudDocs::foreign-a".into()
            }
            3 => {
                wire["identity_handoff"]["backup"]["parent_id"] =
                    "FOLDER::com.apple.CloudDocs::not-trash".into()
            }
            4 => {
                wire["package_completion"] = serde_json::to_value(&plan.source_a.semantic).unwrap()
            }
            _ => wire["remote"]["etag"] = "*".into(),
        }
        *row = serde_json::from_value(wire).unwrap();
        assert!(
            admitted(&plan, &state, Format::Pages).is_err(),
            "flat Pages identity arm {arm} accepted"
        );
    }
}

#[test]
fn owned_pages_flat_replacement_raw_hash_and_semantic_tamper_refuse_without_rewriting() {
    let (_retained, plan, _, _) = fixture_for(false, PackageSourceLayout::FlatPages);
    source_verified_for(&plan.source_b, plan.source_layout).unwrap();
    let a = std::fs::read(&plan.source_a.path).unwrap();
    let mut wrong_raw = plan.source_b.clone();
    wrong_raw.sha256 = "b".repeat(64);
    assert!(source_verified_for(&wrong_raw, plan.source_layout).is_err());
    let changed = local_size_mirror(crate::native_import::synthetic_package_archive(
        "Metadata/data",
        b"complete foreign content",
    ));
    std::fs::set_permissions(&plan.source_b.path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(&plan.source_b.path, &changed).unwrap();
    std::fs::set_permissions(&plan.source_b.path, std::fs::Permissions::from_mode(0o400)).unwrap();
    let mut changed_source = plan.source_b.clone();
    changed_source.size = changed.len() as u64;
    changed_source.sha256 = hex::encode(Sha256::digest(&changed));
    assert!(source_verified_for(&changed_source, plan.source_layout).is_err());
    assert_eq!(std::fs::read(&plan.source_b.path).unwrap(), changed);
    assert_eq!(std::fs::read(&plan.source_a.path).unwrap(), a);
}

#[test]
fn owned_pages_flat_replacement_queue_sequence_and_completion_cannot_borrow_receipt() {
    for sequence in [false, true] {
        let (_retained, plan, _, _) = fixture_for(true, PackageSourceLayout::FlatPages);
        bound(&plan).unwrap();
        let path = plan.session_directory.join("journal/uploads.db");
        let db = rusqlite::Connection::open(&path).unwrap();
        if sequence {
            db.execute(
                "UPDATE uploads SET sequence=sequence+50 WHERE id=?1",
                [plan.replacement.unwrap().to_string()],
            )
            .unwrap();
        } else {
            db.execute(
                "UPDATE write_queue SET complete=0 WHERE id=?1",
                [plan.replacement.unwrap().to_string()],
            )
            .unwrap();
        }
        drop(db);
        let before = std::fs::read(&path).unwrap();
        assert!(bound(&plan).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[test]
fn owned_pages_flat_replacement_requires_readonly_original_source() {
    let (_retained, plan, _, _) = fixture_for(false, PackageSourceLayout::FlatPages);
    source_verified_for(&plan.source_a, plan.source_layout).unwrap();
    let bytes = std::fs::read(&plan.source_a.path).unwrap();
    std::fs::set_permissions(&plan.source_a.path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(source_verified_for(&plan.source_a, plan.source_layout).is_err());
    assert_eq!(std::fs::read(&plan.source_a.path).unwrap(), bytes);
}

#[test]
fn owned_pages_flat_replacement_preserves_wrapped_default_wire_and_authority() {
    let (_retained, plan, _, _) = fixture(false);
    let wire = serde_json::to_value(&plan).unwrap();
    assert!(wire.get("source_layout").is_none());
    assert_eq!(wire["source_a"]["root"], "Source.pages");
    let decoded: Plan = serde_json::from_value(wire).unwrap();
    bound(&decoded).unwrap();
    source_verified_for(&decoded.source_a, decoded.source_layout).unwrap();
    let mut wrongly_selected = decoded;
    wrongly_selected.source_layout = PackageSourceLayout::FlatPages;
    assert!(bound(&wrongly_selected).is_err());
}
