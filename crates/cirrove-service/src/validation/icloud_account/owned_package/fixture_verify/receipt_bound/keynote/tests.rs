#![allow(clippy::unwrap_used)]
use super::*;
use crate::journal::UploadJournal;
use cirrove_core::upload::PackageUploadReceipt;

fn fixture() -> (
    KeynoteRegistration,
    Node,
    MutationRecord,
    NamespaceObject,
    Vec<UploadRecord>,
) {
    let (old, mut parent, mut mutation, mut local, mut rows) = super::super::tests::fixture();
    let root = PathBuf::from(format!("/var/tmp/cirrove-keynote-import-{}", old.run));
    let mut source = old.source_a.clone();
    source.path = root.join("source-a.key");
    source.root = "Source.key".into();
    let plan = KeynoteRegistration {
        version: 1,
        run: old.run,
        account: old.account,
        label: "iCloudKeynoteImportValidation".into(),
        session_directory: root,
        settings_sha256: old.settings_sha256,
        parent_creation: old.parent_creation,
        import: old.import,
        source,
    };
    parent.name = plan.parent_plan().parent_name();
    mutation.request.intent = MutationIntent::CreateFolder {
        parent: cirrove_icloud::ROOT_ID.into(),
        name: parent.name.clone(),
    };
    mutation.receipt = Some(MutationReceipt::Upsert(parent.clone()));
    local.node.name = parent.name.clone();
    local.remote = Some(parent.clone());
    rows[0].intent = UploadIntent::Create {
        parent: parent.id.clone(),
        name: plan.name(),
    };
    rows[0].representation = UploadRepresentation::PackageArchive {
        expected_root: plan.source.root.clone(),
        semantic: plan.source.semantic.clone(),
    };
    rows[0].remote.as_mut().unwrap().name = plan.name();
    (plan, parent, mutation, local, rows)
}
fn clean() -> ImportFrontier {
    ImportFrontier {
        incomplete: 0,
        working: (0, 0),
        auxiliary: (1, 0, 2),
    }
}
fn check(
    plan: &KeynoteRegistration,
    mutation: &MutationRecord,
    local: &NamespaceObject,
    rows: &[UploadRecord],
) -> Result<(Node, Node)> {
    let published = PackagePublicationStatus::Present(rows[0].remote.clone().unwrap());
    admitted(
        plan,
        std::slice::from_ref(mutation),
        local,
        rows,
        &published,
        &clean(),
    )
}
fn sources() -> (tempfile::TempDir, Sources) {
    let run = Uuid::new_v4();
    let temp = tempfile::Builder::new()
        .prefix(&format!("cirrove-keynote-import-{run}"))
        .rand_bytes(0)
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/var/tmp")
        .unwrap();
    let path = temp.path().join("source-a.key");
    let bytes = crate::native_import::synthetic_package_archive(
        "Source.key/Metadata/data",
        b"owned Keynote fixture",
    );
    std::fs::write(&path, &bytes).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let archive = PackageDownload {
        size: bytes.len() as u64,
        sha256: hex::encode(Sha256::digest(&bytes)),
    };
    let semantic = cirrove_icloud::package_archive_semantic_identity_versioned(
        &File::open(&path).unwrap(),
        &archive,
        "Source.key",
        2,
        &CancellationToken::new(),
    )
    .unwrap();
    let sources = Sources {
        version: 1,
        run,
        session_directory: temp.path().to_owned(),
        source: Source {
            path,
            size: archive.size,
            sha256: archive.sha256,
            root: "Source.key".into(),
            semantic,
        },
    };
    (temp, sources)
}
#[test]
fn owned_keynote_registration_binds_exact_import_scope() {
    let (plan, _, mutation, local, rows) = fixture();
    check(&plan, &mutation, &local, &rows).unwrap();
    assert!(
        plan.parent_plan().validate().is_err(),
        "historical Numbers scope remains strict"
    );
    for arm in 0..5 {
        let mut altered = plan.clone();
        match arm {
            0 => altered.run = Uuid::new_v4(),
            1 => altered.account = Uuid::nil(),
            2 => altered.label = "foreign".into(),
            3 => altered.import = altered.parent_creation,
            _ => altered.session_directory = altered.session_directory.join("other"),
        }
        assert!(
            altered.validate().is_err(),
            "registration arm {arm} accepted"
        );
    }
}
fn actual_journal() -> (tempfile::TempDir, KeynoteRegistration, Node, Node) {
    let (temp, sources) = sources();
    let (mut plan, mut parent, _, _, old_rows) = fixture();
    plan.run = sources.run;
    plan.session_directory = sources.session_directory;
    plan.source = sources.source;
    parent.name = plan.parent_plan().parent_name();
    let path = temp.path().join("journal");
    let mut journal =
        UploadJournal::open(&path, &plan.account.to_string(), 16 * 1024 * 1024).unwrap();
    let local = journal
        .create_namespace_directory(
            plan.parent_plan().scope(),
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
    let local = journal.namespace_object(local.id).unwrap();
    journal
        .handoff_namespace(local.id, local.revision, parent.clone())
        .unwrap();
    let archive = crate::native_import::ValidatedPackageArchive::capture(
        &plan.source.path,
        temp.path(),
        &plan.source.root,
        &CancellationToken::new(),
    )
    .unwrap();
    let imported = journal
        .enqueue_validated_package_archive(
            plan.parent_plan().scope(),
            UploadIntent::Create {
                parent: parent.id.clone(),
                name: plan.name(),
            },
            archive,
            &CancellationToken::new(),
        )
        .unwrap();
    plan.import = imported.id;
    let mut current = old_rows[0].remote.clone().unwrap();
    current.name = plan.name();
    current.size = plan.source.semantic.expanded_bytes;
    let claimed = journal.claim_next().unwrap().unwrap();
    journal
        .acknowledge_package(
            claimed.id,
            claimed.attempt.unwrap(),
            PackageUploadReceipt {
                remote: current.clone(),
                semantic: plan.source.semantic.clone(),
            },
        )
        .unwrap();
    let imported = journal.get(imported.id).unwrap();
    journal
        .finish_package_publication(
            &imported,
            PackagePublicationStatus::Present(current.clone()),
            0,
        )
        .unwrap();
    drop(journal);
    (temp, plan, parent, current)
}
#[test]
fn owned_keynote_actual_journal_binds_import_and_read_only_inventory() {
    let (temp, plan, parent, current) = actual_journal();
    let path = temp.path().join("journal");
    let before = std::fs::read(path.join("uploads.db")).unwrap();
    let journal = RecoveryJournal::open(&path, &plan.account.to_string()).unwrap();
    assert_eq!(
        journal
            .native_validation_import_auxiliary_inventory()
            .unwrap(),
        (1, 0, 2)
    );
    let (actual_parent, actual_current, frozen) = journal_binding(&plan, &journal).unwrap();
    assert_eq!(actual_parent, parent);
    assert_eq!(actual_current, current);
    assert_eq!(journal_binding(&plan, &journal).unwrap().2, frozen);
    drop(journal);
    assert_eq!(std::fs::read(path.join("uploads.db")).unwrap(), before);
}
#[test]
fn owned_keynote_import_refuses_extra_completed_queue_entry() {
    let (temp, plan, _, _) = actual_journal();
    let path = temp.path().join("journal");
    let journal = RecoveryJournal::open(&path, &plan.account.to_string()).unwrap();
    journal_binding(&plan, &journal).unwrap();
    drop(journal);
    // Deliberately orphan one COMPLETED queue entry in this synthetic fixture.
    // It changes neither uploads, mutations nor incomplete queue count.
    let db = rusqlite::Connection::open(path.join("uploads.db")).unwrap();
    db.execute(
        "INSERT INTO write_queue(id,complete) VALUES(?1,1)",
        [Uuid::new_v4().to_string()],
    )
    .unwrap();
    drop(db);
    let before = std::fs::read(path.join("uploads.db")).unwrap();
    let journal = RecoveryJournal::open(&path, &plan.account.to_string()).unwrap();
    assert_eq!(journal.native_validation_incomplete_queue().unwrap(), 0);
    assert_eq!(
        journal
            .native_validation_import_auxiliary_inventory()
            .unwrap(),
        (1, 0, 3)
    );
    assert!(
        journal_binding(&plan, &journal).is_err(),
        "extra completed queue entry accepted"
    );
    drop(journal);
    assert_eq!(std::fs::read(path.join("uploads.db")).unwrap(), before);
}
#[test]
fn owned_keynote_import_refuses_extra_completed_upload() {
    let (plan, _, mutation, local, mut rows) = fixture();
    check(&plan, &mutation, &local, &rows).unwrap();
    let mut extra = rows[0].clone();
    extra.id = Uuid::new_v4();
    extra.sequence += 1;
    extra.remote.as_mut().unwrap().id = "FILE::com.apple.CloudDocs::unregistered".into();
    rows.push(extra);
    assert!(
        check(&plan, &mutation, &local, &rows).is_err(),
        "unregistered completed upload accepted"
    );
}
#[test]
fn owned_keynote_import_refuses_scope_owner_receipt_and_publication_tampering() {
    for arm in 0..13 {
        let (mut plan, _, mut mutation, mut local, mut rows) = fixture();
        match arm {
            0 => plan.account = Uuid::new_v4(),
            1 => rows[0].scope.collection = "foreign".into(),
            2 => rows[0].state = UploadState::Pending,
            3 => rows[0].sha256 = "f".repeat(64),
            4 => rows[0].package_completion = None,
            5 => rows[0].remote.as_mut().unwrap().parent_id = Some(cirrove_icloud::ROOT_ID.into()),
            6 => rows[0].remote.as_mut().unwrap().etag = None,
            7 => rows[0].remote.as_mut().unwrap().package = false,
            8 => local.follows_remote = false,
            9 => mutation.state = MutationState::Pending,
            10 => local.latest = Some(Uuid::new_v4()),
            11 => {
                rows[0].representation = UploadRepresentation::PackageArchive {
                    expected_root: "Source.numbers".into(),
                    semantic: plan.source.semantic.clone(),
                }
            }
            _ => rows[0].remote.as_mut().unwrap().name = "foreign.key".into(),
        }
        assert!(
            check(&plan, &mutation, &local, &rows).is_err(),
            "receipt arm {arm} accepted"
        );
    }
    let (plan, _, mutation, local, rows) = fixture();
    for published in [
        PackagePublicationStatus::Pending,
        PackagePublicationStatus::Absent,
    ] {
        assert!(
            admitted(
                &plan,
                &[mutation.clone()],
                &local,
                &rows,
                &published,
                &clean()
            )
            .is_err()
        );
    }
    let mut foreign = rows[0].remote.clone().unwrap();
    foreign.etag = Some("changed".into());
    assert!(
        admitted(
            &plan,
            &[mutation],
            &local,
            &rows,
            &PackagePublicationStatus::Present(foreign),
            &clean()
        )
        .is_err()
    );
}
#[test]
fn owned_keynote_import_refuses_extra_namespace_association_and_dirty_frontier() {
    let (plan, _, mutation, local, rows) = fixture();
    let published = PackagePublicationStatus::Present(rows[0].remote.clone().unwrap());
    for arm in 0..6 {
        let mut frontier = clean();
        match arm {
            0 => frontier.auxiliary = (2, 0, 2),
            1 => frontier.auxiliary = (1, 1, 2),
            2 => frontier.working = (1, 0),
            3 => frontier.working = (1, 1),
            4 => frontier.incomplete = 1,
            _ => frontier.auxiliary.2 = 3,
        }
        assert!(
            admitted(
                &plan,
                &[mutation.clone()],
                &local,
                &rows,
                &published,
                &frontier
            )
            .is_err(),
            "frontier arm {arm} accepted"
        );
    }
}
#[test]
fn owned_keynote_source_scan_refuses_raw_and_semantic_tampering() {
    let (_temp, sources) = sources();
    sources.verify().unwrap();
    for arm in 0..3 {
        let mut altered = sources.clone();
        match arm {
            0 => altered.source.sha256 = "f".repeat(64),
            1 => altered.source.size += 1,
            _ => altered.source.semantic.sha256 = "f".repeat(64),
        }
        assert!(altered.verify().is_err(), "source arm {arm} accepted");
    }
    std::fs::write(&sources.source.path, b"changed raw source").unwrap();
    assert!(sources.verify().is_err());
}
#[test]
fn owned_keynote_source_scan_refuses_wrong_format_and_root() {
    let (temp, sources) = sources();
    let mut changed = sources.clone();
    changed.source.root = "Source.numbers".into();
    assert!(changed.verify().is_err());
    let bytes = crate::native_import::synthetic_package_archive(
        "Wrong.key/Metadata/data",
        b"owned Keynote fixture",
    );
    std::fs::write(&sources.source.path, &bytes).unwrap();
    changed = sources.clone();
    changed.source.size = bytes.len() as u64;
    changed.source.sha256 = hex::encode(Sha256::digest(bytes));
    assert!(changed.verify().is_err(), "wrong actual ZIP root accepted");
    assert!(temp.path().exists());
}
#[test]
fn owned_keynote_registration_refuses_digest_schema_and_wrong_run() {
    let (temp, sources) = sources();
    let path = temp.path().join("source-registration.json");
    let bytes = serde_json::to_vec(&sources).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let digest = hex::encode(Sha256::digest(&bytes));
    assert_eq!(
        icloud_owned_keynote_source_verify(&path, &digest).unwrap()["offline_only"],
        true
    );
    let mut changed = bytes.clone();
    changed.push(b' ');
    assert!(decode_keynote::<Sources>(&changed, &digest).is_err());
    let mut value = serde_json::to_value(&sources).unwrap();
    value["replacement"] = serde_json::json!(Uuid::new_v4());
    let changed = serde_json::to_vec(&value).unwrap();
    assert!(decode_keynote::<Sources>(&changed, &hex::encode(Sha256::digest(&changed))).is_err());
    let mut altered = sources;
    altered.run = Uuid::new_v4();
    assert!(altered.validate().is_err());
}
