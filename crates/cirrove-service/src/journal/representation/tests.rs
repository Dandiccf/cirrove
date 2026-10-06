#![allow(clippy::unwrap_used)]
use super::*;
use cirrove_core::upload::UploadRequest;
use std::io::Read;

fn private_tempdir() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    temp
}

fn scope() -> Scope {
    Scope {
        account: "owned".into(),
        provider: "icloud".into(),
        collection: "drive".into(),
    }
}
fn intent() -> UploadIntent {
    UploadIntent::Create {
        parent: "root".into(),
        name: "Import.pages".into(),
    }
}
fn semantic() -> PackageSemanticIdentity {
    PackageSemanticIdentity {
        version: 1,
        sha256: "a".repeat(64),
        entries: 2,
        files: 1,
        expanded_bytes: 17,
    }
}
fn node() -> Node {
    Node {
        id: "allocated".into(),
        parent_id: Some("root".into()),
        name: "Import.pages".into(),
        kind: NodeKind::Folder,
        size: 17,
        modified_unix: 0,
        etag: Some("revision".into()),
        content_version: None,
        target: None,
        package: true,
    }
}
fn enqueue(j: &mut UploadJournal) -> UploadRecord {
    j.enqueue_package_archive(
        scope(),
        intent(),
        "Source.pages".into(),
        semantic(),
        b"archive bytes".as_slice(),
    )
    .unwrap()
}
fn receipt() -> PackageUploadReceipt {
    PackageUploadReceipt {
        remote: node(),
        semantic: semantic(),
    }
}
fn payload(j: &UploadJournal, id: Uuid) -> Vec<u8> {
    let mut bytes = Vec::new();
    j.payload(id).unwrap().read_to_end(&mut bytes).unwrap();
    bytes
}

#[test]
fn package_representation_legacy_json_defaults_and_unknown_formats_fail_closed() {
    let temp = private_tempdir();
    let mut journal = UploadJournal::open(temp.path(), "owned", 1024 * 1024).unwrap();
    let row = journal
        .enqueue(scope(), intent(), b"ordinary".as_slice())
        .unwrap();
    let mut json = serde_json::to_value(&row).unwrap();
    json.as_object_mut().unwrap().remove("representation");
    json.as_object_mut().unwrap().remove("package_completion");
    let legacy: UploadRecord = serde_json::from_value(json.clone()).unwrap();
    assert!(legacy.representation.is_file_bytes());
    assert!(legacy.package_completion.is_none());
    let request =
        serde_json::json!({"scope":scope(), "intent":intent(), "size":8, "sha256":row.sha256});
    assert!(
        serde_json::from_value::<UploadRequest>(request)
            .unwrap()
            .representation
            .is_file_bytes()
    );
    json["representation"] = serde_json::json!({"kind":"unknown_archive"});
    assert!(serde_json::from_value::<UploadRecord>(json.clone()).is_err());
    json["representation"] = serde_json::json!({"kind":"package_archive","expected_root":"Source.pages","semantic":semantic()});
    json["representation"]["semantic"]["version"] = 3.into();
    assert!(serde_json::from_value::<UploadRecord>(json).is_err());
}

#[test]
fn package_representation_schema_upgrade_and_readonly_old_new_recovery_preserve_bytes() {
    let temp = private_tempdir();
    let mut journal = UploadJournal::open(temp.path(), "owned", 1024 * 1024).unwrap();
    let row = journal
        .enqueue(scope(), intent(), b"legacy bytes".as_slice())
        .unwrap();
    journal
        .db
        .execute(
            "UPDATE uploads SET body=json_remove(body,'$.representation','$.package_completion')",
            [],
        )
        .unwrap();
    journal.db.pragma_update(None, "user_version", 14).unwrap();
    drop(journal);
    let before = std::fs::read(temp.path().join("uploads.db")).unwrap();
    {
        let ro = RecoveryJournal::open(temp.path(), "owned").unwrap();
        assert_eq!(ro.list(0, 100).unwrap().len(), 1);
    }
    assert_eq!(
        std::fs::read(temp.path().join("uploads.db")).unwrap(),
        before
    );
    let journal = UploadJournal::open(temp.path(), "owned", 1024 * 1024).unwrap();
    assert!(journal.get(row.id).unwrap().representation.is_file_bytes());
    assert_eq!(payload(&journal, row.id), b"legacy bytes");
    let version: u32 = journal
        .db
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, JOURNAL_SCHEMA);
    // Source-level check against the previous version > 14 opening gate.
    // This fixture does not execute an older binary.
    assert!(version > 14);
    drop(journal);
    {
        assert_eq!(
            RecoveryJournal::open(temp.path(), "owned")
                .unwrap()
                .list(0, 100)
                .unwrap()
                .len(),
            1
        );
    }
    let db = Connection::open(temp.path().join("uploads.db")).unwrap();
    db.pragma_update(None, "user_version", JOURNAL_SCHEMA + 1)
        .unwrap();
    drop(db);
    assert!(matches!(
        UploadJournal::open(temp.path(), "owned", 1024 * 1024),
        Err(JournalError::Schema)
    ));
    assert!(matches!(
        RecoveryJournal::open(temp.path(), "owned"),
        Err(JournalError::Schema)
    ));
}

#[test]
fn package_receipt_preserves_raw_and_logical_sizes_and_survives_restart() {
    let temp = private_tempdir();
    let mut journal = UploadJournal::open(temp.path(), "owned", 1024 * 1024).unwrap();
    let row = enqueue(&mut journal);
    let claimed = journal.claim_next().unwrap().unwrap();
    journal
        .acknowledge_package(row.id, claimed.attempt.unwrap(), receipt())
        .unwrap();
    let saved = journal.get(row.id).unwrap();
    assert_eq!(saved.state, UploadState::Uploaded);
    assert_eq!(saved.size, 13);
    assert_eq!(saved.remote.as_ref().unwrap().size, 17);
    assert_eq!(saved.transferred_bytes, 13);
    assert_eq!(saved.package_completion, Some(semantic()));
    assert_eq!(payload(&journal, row.id), b"archive bytes");
    drop(journal);
    let reopened = UploadJournal::open(temp.path(), "owned", 1024 * 1024).unwrap();
    assert_eq!(
        serde_json::to_value(reopened.get(row.id).unwrap()).unwrap(),
        serde_json::to_value(saved).unwrap()
    );
}

#[test]
fn package_receipt_rejects_cross_kind_mismatch_and_stale_attempt_without_changing_queue() {
    for variant in 0..9 {
        let temp = private_tempdir();
        let mut j = UploadJournal::open(temp.path(), "owned", 1024 * 1024).unwrap();
        let row = enqueue(&mut j);
        let attempt = j.claim_next().unwrap().unwrap().attempt.unwrap();
        let before = serde_json::to_value(j.get(row.id).unwrap()).unwrap();
        let mut r = receipt();
        match variant {
            0 => r.semantic.sha256 = "b".repeat(64),
            1 => r.remote.kind = NodeKind::File,
            2 => r.remote.package = false,
            3 => r.remote.parent_id = Some("elsewhere".into()),
            4 => r.remote.name = "wrong.pages".into(),
            5 => r.remote.etag = None,
            6 => r.remote.id.clear(),
            7 => {
                assert!(j.acknowledge_package(row.id, Uuid::new_v4(), r).is_err());
                continue;
            }
            _ => {
                let mut ordinary = node();
                ordinary.kind = NodeKind::File;
                ordinary.package = false;
                ordinary.size = row.size;
                assert!(j.acknowledge(row.id, attempt, ordinary).is_err());
                continue;
            }
        }
        assert!(j.acknowledge_package(row.id, attempt, r).is_err());
        assert_eq!(
            serde_json::to_value(j.get(row.id).unwrap()).unwrap(),
            before
        );
        assert_eq!(payload(&j, row.id), b"archive bytes");
    }
    let temp = private_tempdir();
    let mut j = UploadJournal::open(temp.path(), "owned", 1024 * 1024).unwrap();
    let row = j
        .enqueue(scope(), intent(), b"ordinary".as_slice())
        .unwrap();
    let attempt = j.claim_next().unwrap().unwrap().attempt.unwrap();
    assert!(matches!(
        j.acknowledge_package(row.id, attempt, receipt()),
        Err(JournalError::Intent)
    ));
    assert!(j.get(row.id).unwrap().package_completion.is_none());
    let mut disguised_package = node();
    disguised_package.kind = NodeKind::File;
    disguised_package.size = row.size;
    assert!(j.acknowledge(row.id, attempt, disguised_package).is_err());
    assert_eq!(j.get(row.id).unwrap().state, UploadState::Uploading);
}

#[test]
fn package_retry_retains_qualifier_and_refuses_ordinary_rescue_successor_and_handoff() {
    let temp = private_tempdir();
    let mut j = UploadJournal::open(temp.path(), "owned", 1024 * 1024).unwrap();
    let row = enqueue(&mut j);
    let attempt = j.claim_next().unwrap().unwrap().attempt.unwrap();
    assert!(
        j.reserve_identity_handoff(
            row.id,
            attempt,
            cirrove_core::upload::RecoveryLocation::Sibling {
                name: "backup".into()
            }
        )
        .is_err()
    );
    j.stop_attempt(row.id, attempt, UploadState::Failed)
        .unwrap();
    let before = serde_json::to_value(j.get(row.id).unwrap()).unwrap();
    assert!(
        j.keep_both(row.id, "root".into(), "copy.pages".into())
            .is_err()
    );
    assert!(j.enqueue_after(row.id, b"new".as_slice()).is_err());
    let mut before_node = node();
    before_node.kind = NodeKind::File;
    before_node.package = false;
    let mutation = cirrove_core::mutation::MutationRequest {
        scope: scope(),
        intent: cirrove_core::mutation::MutationIntent::RemoveFile {
            before: before_node,
        },
    };
    assert!(j.enqueue_mutation_after(row.id, mutation).is_err());
    assert!(j.list_mutations(0, 100).unwrap().is_empty());

    assert_eq!(
        serde_json::to_value(j.get(row.id).unwrap()).unwrap(),
        before
    );
    assert_eq!(j.list(0, 100).unwrap().len(), 1);
    j.request_retry(row.id).unwrap();
    drop(j);
    let j = UploadJournal::open(temp.path(), "owned", 1024 * 1024).unwrap();
    assert!(j.get(row.id).unwrap().representation == row.representation);
    assert_eq!(payload(&j, row.id), b"archive bytes");
}

#[test]
fn package_enqueue_rejects_replace_and_invalid_root_or_identity_without_creating_payload() {
    let temp = private_tempdir();
    let mut j = UploadJournal::open(temp.path(), "owned", 1024 * 1024).unwrap();
    let mut invalid_identity = semantic();
    invalid_identity.version = 3;
    for (intent, root, identity) in [
        (
            UploadIntent::Replace {
                item: "original".into(),
                expected_etag: "v1".into(),
            },
            "Source.pages",
            semantic(),
        ),
        (intent(), "../Source.pages", semantic()),
        (intent(), "Source.pages", invalid_identity),
    ] {
        assert!(
            j.enqueue_package_archive(
                scope(),
                intent,
                root.into(),
                identity,
                b"retained".as_slice()
            )
            .is_err()
        );
    }
    assert!(j.list(0, 100).unwrap().is_empty());
    assert_eq!(
        std::fs::read_dir(temp.path().join("objects"))
            .unwrap()
            .count(),
        0
    );
}

#[derive(Default)]
struct Vault(std::sync::Mutex<Option<secrecy::SecretString>>);
#[async_trait::async_trait]
impl cirrove_auth::CredentialVault for Vault {
    async fn load(&self, _: &str) -> anyhow::Result<Option<secrecy::SecretString>> {
        Ok(self.0.lock().unwrap().clone())
    }
    async fn save(&self, _: &str, value: secrecy::SecretString) -> anyhow::Result<()> {
        *self.0.lock().unwrap() = Some(value);
        Ok(())
    }
    async fn remove(&self, _: &str) -> anyhow::Result<()> {
        *self.0.lock().unwrap() = None;
        Ok(())
    }
}
struct PackageProvider {
    uncertain: bool,
    streams: std::sync::atomic::AtomicUsize,
    reconciliations: std::sync::atomic::AtomicUsize,
}
fn check_request(request: &UploadRequest) {
    assert!(
        request.representation
            == UploadRepresentation::PackageArchive {
                expected_root: "Source.pages".into(),
                semantic: semantic()
            }
    );
    assert_eq!(request.size, 13);
    assert_eq!(
        request.sha256,
        hex::encode(Sha256::digest(b"archive bytes"))
    );
}
#[async_trait::async_trait]
impl cirrove_core::upload::UploadProvider for PackageProvider {
    async fn begin_upload(
        &self,
        r: &UploadRequest,
        _: &cirrove_core::CancellationToken,
    ) -> cirrove_core::upload::Result<cirrove_core::upload::UploadStep> {
        check_request(r);
        Ok(cirrove_core::upload::UploadStep::Stream(
            "sealed-checkpoint".into(),
        ))
    }
    async fn inspect_upload(
        &self,
        r: &UploadRequest,
        _: &secrecy::SecretString,
        _: &cirrove_core::CancellationToken,
    ) -> cirrove_core::upload::Result<cirrove_core::upload::UploadStep> {
        check_request(r);
        Err(cirrove_core::upload::UploadError::Uncertain)
    }
    async fn upload_part(
        &self,
        _: &UploadRequest,
        _: &secrecy::SecretString,
        _: u64,
        _: Vec<u8>,
        _: &cirrove_core::CancellationToken,
    ) -> cirrove_core::upload::Result<cirrove_core::upload::UploadStep> {
        panic!("unexpected part")
    }
    async fn upload_stream(
        &self,
        r: &UploadRequest,
        _: &secrecy::SecretString,
        mut file: File,
        _: &cirrove_core::CancellationToken,
    ) -> cirrove_core::upload::Result<cirrove_core::upload::UploadStep> {
        check_request(r);
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"archive bytes");
        self.streams
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.uncertain {
            Err(cirrove_core::upload::UploadError::Uncertain)
        } else {
            Ok(cirrove_core::upload::UploadStep::PackageComplete(receipt()))
        }
    }
    async fn commit_upload(
        &self,
        _: &UploadRequest,
        _: &secrecy::SecretString,
        _: &cirrove_core::CancellationToken,
    ) -> cirrove_core::upload::Result<cirrove_core::upload::UploadStep> {
        panic!("unexpected commit")
    }
    async fn reconcile_upload(
        &self,
        r: &UploadRequest,
        checkpoint: Option<&secrecy::SecretString>,
        _: &cirrove_core::CancellationToken,
    ) -> cirrove_core::upload::Result<cirrove_core::upload::Reconciliation> {
        use secrecy::ExposeSecret;
        check_request(r);
        assert_eq!(checkpoint.unwrap().expose_secret(), "sealed-checkpoint");
        self.reconciliations
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(cirrove_core::upload::Reconciliation::PackageCommitted(
            receipt(),
        ))
    }
}
#[tokio::test]
async fn package_worker_propagates_qualifier_and_persists_stream_and_reconciled_receipts() {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    for uncertain in [false, true] {
        let temp = private_tempdir();
        let mut journal = UploadJournal::open(temp.path(), "owned", 1024 * 1024).unwrap();
        let row = enqueue(&mut journal);
        let journal = Arc::new(Mutex::new(journal));
        let provider = Arc::new(PackageProvider {
            uncertain,
            streams: AtomicUsize::new(0),
            reconciliations: AtomicUsize::new(0),
        });
        let vault = Arc::new(Vault::default());
        let worker = crate::transfers::TransferWorker::new(
            journal.clone(),
            provider.clone(),
            vault.clone(),
            cirrove_core::CancellationToken::new(),
        );
        let first = worker.run_once().await.unwrap().unwrap();
        if uncertain {
            assert_eq!(first.state, UploadState::VerifyRequired);
            assert!(vault.0.lock().unwrap().is_some());
            journal.lock().unwrap().request_retry(row.id).unwrap();
            assert_eq!(
                worker.run_once().await.unwrap().unwrap().state,
                UploadState::Uploaded
            );
        } else {
            assert_eq!(first.state, UploadState::Uploaded);
        }
        assert_eq!(provider.streams.load(Ordering::SeqCst), 1);
        assert_eq!(
            provider.reconciliations.load(Ordering::SeqCst),
            usize::from(uncertain)
        );
        assert!(vault.0.lock().unwrap().is_none());
        let saved = journal.lock().unwrap().get(row.id).unwrap();
        assert_eq!(saved.package_completion, Some(semantic()));
        assert_eq!(saved.remote.unwrap().size, 17);
        assert_eq!(saved.size, 13);
        assert_eq!(payload(&journal.lock().unwrap(), row.id), b"archive bytes");
    }
}

#[test]
fn package_readonly_recovery_exports_exact_archive_without_changing_pending_or_uncertain_record() {
    // A tiny stored ZIP with Source.pages/content.txt; no archive extraction,
    // provider, upload worker or replay is involved in this recovery fixture.
    let archive = hex::decode("504b03041400000000000000215c717e11c50e0000000e00000018000000536f757263652e70616765732f636f6e74656e742e7478746e61746976652066697874757265504b010214031400000000000000215c717e11c50e0000000e000000180000000000000000000000800100000000536f757263652e70616765732f636f6e74656e742e747874504b0506000000000100010046000000440000000000").unwrap();
    let semantic = PackageSemanticIdentity {
        version: 1,
        sha256: "f4ef32ae50dc5aaec3559c9b0c76db35d2972abd30c04b657d93b62316834b44".into(),
        entries: 1,
        files: 1,
        expanded_bytes: 14,
    };
    let raw_hash = hex::encode(Sha256::digest(&archive));
    assert_ne!(archive.len() as u64, semantic.expanded_bytes);
    for uncertain in [false, true] {
        let temp = private_tempdir();
        let root = temp.path().join("journal");
        let mut j = UploadJournal::open(&root, "owned", 1024 * 1024).unwrap();
        let row = j
            .enqueue_package_archive(
                scope(),
                intent(),
                "Source.pages".into(),
                semantic.clone(),
                archive.as_slice(),
            )
            .unwrap();
        if uncertain {
            let attempt = j.claim_next().unwrap().unwrap().attempt.unwrap();
            // Model durable checkpoint linkage after a lost remote response.
            // Its opaque credential body is neither needed nor loaded by RO export.
            j.record_session(row.id, attempt, row.id, 0).unwrap();
            j.stop_attempt(row.id, attempt, UploadState::VerifyRequired)
                .unwrap();
        }
        let expected = j.get(row.id).unwrap();
        let expected_json = serde_json::to_value(&expected).unwrap();
        drop(j);
        let database_before = std::fs::read(root.join("uploads.db")).unwrap();
        let destination = temp.path().join("recovered.zip");
        {
            let recovery = RecoveryJournal::open(&root, "owned").unwrap();
            let rows = recovery.list(0, 100).unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(serde_json::to_value(&rows[0]).unwrap(), expected_json);
            assert!(rows[0].representation == row.representation);
            assert_eq!(
                rows[0].state,
                if uncertain {
                    UploadState::VerifyRequired
                } else {
                    UploadState::Pending
                }
            );
            assert_eq!(rows[0].session_key, uncertain.then_some(row.id));
            assert!(rows[0].remote.is_none());
            assert!(rows[0].package_completion.is_none());
            let receipt = recovery
                .local_export_source(row.id)
                .unwrap()
                .copy_to(
                    &destination,
                    &cirrove_core::CancellationToken::new(),
                    |_| {},
                )
                .unwrap();
            assert_eq!(receipt.operation, row.id);
            assert_eq!(receipt.size, archive.len() as u64);
            assert_eq!(receipt.sha256, raw_hash);
            assert_eq!(std::fs::read(&destination).unwrap(), archive);
            assert_eq!(
                serde_json::to_value(&recovery.list(0, 100).unwrap()[0]).unwrap(),
                expected_json
            );
        }
        assert_eq!(
            std::fs::read(root.join("uploads.db")).unwrap(),
            database_before
        );
        assert_eq!(
            std::fs::read(root.join("objects").join(row.id.to_string())).unwrap(),
            archive
        );
    }
}

#[test]
fn native_import_slot_releases_published_history_but_retains_uncertainty() {
    let temp = private_tempdir();
    let mut journal = UploadJournal::open(temp.path(), "owned", 1024 * 1024).unwrap();
    let row = enqueue(&mut journal);
    assert!(
        journal
            .native_import_destination_reserved(&scope(), "root", "Import.pages")
            .unwrap()
    );
    assert!(
        journal
            .native_import_destination_reserved(&scope(), "root", "IMPORT.PAGES")
            .unwrap()
    );
    assert!(
        !journal
            .native_import_destination_reserved(&scope(), "root", "Other.pages")
            .unwrap()
    );
    let claimed = journal.claim_next().unwrap().unwrap();
    journal
        .acknowledge_package(row.id, claimed.attempt.unwrap(), receipt())
        .unwrap();
    assert!(
        journal
            .native_import_destination_reserved(&scope(), "root", "Import.pages")
            .unwrap()
    );
    let completed = journal.get(row.id).unwrap();
    journal
        .finish_package_publication(
            &completed,
            crate::journal::PackagePublicationStatus::Absent,
            1,
        )
        .unwrap();
    assert!(
        !journal
            .native_import_destination_reserved(&scope(), "root", "Import.pages")
            .unwrap()
    );
    // A new queued package takes the released slot without old receipt replay.
    let second = enqueue(&mut journal);
    assert_ne!(second.id, row.id);
    assert!(
        journal
            .native_import_destination_reserved(&scope(), "root", "Import.pages")
            .unwrap()
    );
}

#[test]
fn native_trash_schema16_preserves_pending_and_uncertain_intents_and_local_recovery() {
    use cirrove_core::mutation::{MutationIntent, MutationRequest};
    for uncertain in [false, true] {
        let temp = private_tempdir();
        let root = temp.path().join("journal");
        let mut journal = UploadJournal::open(&root, "owned", 1024 * 1024).unwrap();
        let retained = journal
            .enqueue(scope(), intent(), b"retained ordinary bytes".as_slice())
            .unwrap();
        // Upgrade an ordinary-only schema15 journal before introducing the new tag.
        journal.db.pragma_update(None, "user_version", 15).unwrap();
        drop(journal);
        let mut journal = UploadJournal::open(&root, "owned", 1024 * 1024).unwrap();
        let version: u32 = journal
            .db
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, JOURNAL_SCHEMA);
        // This is a source-level old-binary guard assertion, not an old binary run.
        assert!(version > 15);
        let mut before = node();
        before.name = "Original.pages".into();
        let request = MutationRequest {
            scope: scope(),
            intent: MutationIntent::TrashNativeDocument { before },
        };
        let removal = journal.enqueue_mutation(request.clone()).unwrap();
        assert!(
            serde_json::to_string(&removal)
                .unwrap()
                .contains("trash_native_document")
        );
        if uncertain {
            let claimed = journal.claim_mutation().unwrap().unwrap();
            assert_eq!(claimed.id, removal.id);
            assert_eq!(claimed.state, MutationState::Applying);
        }
        drop(journal);
        let before_db = std::fs::read(root.join("uploads.db")).unwrap();
        let destination = temp.path().join("retained.txt");
        {
            let recovery = RecoveryJournal::open(&root, "owned").unwrap();
            let receipt = recovery
                .local_export_source(retained.id)
                .unwrap()
                .copy_to(&destination, &CancellationToken::new(), |_| {})
                .unwrap();
            assert_eq!(receipt.operation, retained.id);
            assert_eq!(
                std::fs::read(destination).unwrap(),
                b"retained ordinary bytes"
            );
        }
        assert_eq!(std::fs::read(root.join("uploads.db")).unwrap(), before_db);
        let journal = UploadJournal::open(&root, "owned", 1024 * 1024).unwrap();
        let restored = journal.mutation(removal.id).unwrap();
        assert!(restored.request == request);
        assert!(restored.receipt.is_none());
        assert_eq!(
            restored.state,
            if uncertain {
                MutationState::VerifyRequired
            } else {
                MutationState::Pending
            }
        );
        assert!(restored.attempt.is_none());
    }
}

#[test]
fn native_trash_is_terminal_and_cannot_chain_from_existing_generation() {
    use cirrove_core::mutation::{MutationIntent, MutationReceipt, MutationRequest};
    let temp = private_tempdir();
    let mut journal = UploadJournal::open(temp.path(), "owned", 1024 * 1024).unwrap();
    let created = enqueue(&mut journal);
    let request = MutationRequest {
        scope: scope(),
        intent: MutationIntent::TrashNativeDocument { before: node() },
    };
    assert!(
        journal
            .enqueue_mutation_after(created.id, request.clone())
            .is_err()
    );
    assert!(journal.list_mutations(0, 100).unwrap().is_empty());
    let removal = journal.enqueue_mutation(request.clone()).unwrap();
    assert!(journal.enqueue_mutation_after(removal.id, request).is_err());
    assert_eq!(journal.list_mutations(0, 100).unwrap().len(), 1);
    // Native removal cannot be acknowledged by an unrelated or still-active node.
    assert!(!removal.request.accepts(&MutationReceipt::Removed {
        item: "foreign".into()
    }));
    assert!(!removal.request.accepts(&MutationReceipt::Upsert(node())));
}

#[test]
fn flat_numbers_schema20_migration_preserves_wrapped_history_and_publishes_flat_receipt() {
    use crate::native_import::{PackageSourceLayout, ValidatedPackageArchive};
    let retained = private_tempdir().keep();
    let root = retained.join("journal");
    let mut j = UploadJournal::open(&root, "owned", 1 << 20).unwrap();
    let parent = "FOLDER::com.apple.CloudDocs::owned";
    let wrapped = j
        .enqueue_package_archive(
            scope(),
            UploadIntent::Create {
                parent: parent.into(),
                name: "Legacy.pages".into(),
            },
            "Source.pages".into(),
            semantic(),
            b"retained wrapped bytes".as_slice(),
        )
        .unwrap();
    let wrapped_before = serde_json::to_value(&wrapped).unwrap();
    // Synthetic schema20 uses the actual prior wrapped-only SQL definitions.
    // It is not an invocation of an older producer or an installed-state test.
    j.db.execute_batch("DROP INDEX native_package_import_operations_v21;
        DROP INDEX native_package_replacement_operations_v21;
        DROP INDEX uploaded_package_receipts_v21;
        DROP TRIGGER package_metadata_on_insert_v21;
        DROP TRIGGER package_metadata_on_update_v21;
        CREATE INDEX native_package_import_operations ON uploads(sequence) WHERE json_extract(body,'$.representation.kind')='package_archive';
        CREATE INDEX native_package_replacement_operations ON uploads(sequence) WHERE json_extract(body,'$.representation.kind')='package_replacement_archive';").unwrap();
    j.db.execute_batch(r#"CREATE TABLE IF NOT EXISTS package_metadata_publication(
        operation TEXT PRIMARY KEY, done INTEGER NOT NULL DEFAULT 0 CHECK(done IN(0,1)),
        failures INTEGER NOT NULL DEFAULT 0, retry_after INTEGER NOT NULL DEFAULT 0, observed TEXT);
        CREATE INDEX IF NOT EXISTS package_metadata_due ON package_metadata_publication(done,retry_after,operation);
        CREATE INDEX IF NOT EXISTS uploaded_package_receipts_v17 ON uploads(sequence)
        WHERE state='uploaded' AND json_extract(body,'$.representation.kind') IN('package_archive','package_replacement_archive')
          AND json_type(body,'$.package_completion')='object';
        CREATE TRIGGER IF NOT EXISTS package_metadata_on_update_v17 AFTER UPDATE OF state,body ON uploads
        WHEN NEW.state='uploaded' AND json_extract(NEW.body,'$.representation.kind') IN('package_archive','package_replacement_archive')
          AND json_type(NEW.body,'$.package_completion')='object'
        BEGIN INSERT OR IGNORE INTO package_metadata_publication(operation) VALUES(NEW.id); END;
        CREATE TRIGGER IF NOT EXISTS package_metadata_on_insert_v17 AFTER INSERT ON uploads
        WHEN NEW.state='uploaded' AND json_extract(NEW.body,'$.representation.kind') IN('package_archive','package_replacement_archive')
          AND json_type(NEW.body,'$.package_completion')='object'
        BEGIN INSERT OR IGNORE INTO package_metadata_publication(operation) VALUES(NEW.id); END;
        INSERT OR IGNORE INTO package_metadata_publication(operation)
        SELECT id FROM uploads WHERE state='uploaded'
          AND json_extract(body,'$.representation.kind') IN('package_archive','package_replacement_archive')
          AND json_type(body,'$.package_completion')='object';"#).unwrap();
    j.db.pragma_update(None, "user_version", 20).unwrap();
    drop(j);
    let database_before = std::fs::read(root.join("uploads.db")).unwrap();
    {
        let repair =
            crate::journal::export::MetadataPublicationJournal::open(&root, "owned").unwrap();
        assert!(repair.due(0).unwrap().is_none());
    }
    assert_eq!(
        std::fs::read(root.join("uploads.db")).unwrap(),
        database_before
    );
    let mut j = UploadJournal::open(&root, "owned", 1 << 20).unwrap();
    assert_eq!(
        serde_json::to_value(j.get(wrapped.id).unwrap()).unwrap(),
        wrapped_before
    );
    assert_eq!(payload(&j, wrapped.id), b"retained wrapped bytes");
    let archive = crate::native_import::synthetic_package_archive(
        "Index/Document.iwa",
        b"flat owned Numbers",
    );
    let source = retained.join("source.numbers");
    std::fs::write(&source, &archive).unwrap();
    let captured = ValidatedPackageArchive::capture_with_source_layout(
        &source,
        &retained,
        PackageSourceLayout::FlatNumbers,
        None,
        &CancellationToken::new(),
    )
    .unwrap();
    let row = j
        .enqueue_validated_package_archive(
            scope(),
            UploadIntent::Create {
                parent: parent.into(),
                name: "Flat.numbers".into(),
            },
            captured,
            &CancellationToken::new(),
        )
        .unwrap();
    let UploadRepresentation::FlatNumbersArchive { semantic } = &row.representation else {
        panic!("flat proof missing")
    };
    let semantic = semantic.clone();
    // Pending flat bytes are exportable after a read-only restart. Exporting
    // does not acknowledge the upload or change the retained row/database.
    drop(j);
    let pending_database = std::fs::read(root.join("uploads.db")).unwrap();
    let recovery = RecoveryJournal::open(&root, "owned").unwrap();
    let pending = recovery
        .list(0, 10)
        .unwrap()
        .into_iter()
        .find(|saved| saved.id == row.id)
        .unwrap();
    assert_eq!(
        serde_json::to_value(pending).unwrap(),
        serde_json::to_value(&row).unwrap()
    );
    let exported = retained.join("flat-pending-export.zip");
    let export = recovery
        .local_export_source(row.id)
        .unwrap()
        .copy_to(&exported, &CancellationToken::new(), |_| {})
        .unwrap();
    assert_eq!(export.operation, row.id);
    assert_eq!(export.sha256, row.sha256);
    assert_eq!(std::fs::read(exported).unwrap(), archive);
    drop(recovery);
    assert_eq!(
        std::fs::read(root.join("uploads.db")).unwrap(),
        pending_database
    );
    let mut j = UploadJournal::open(&root, "owned", 1 << 20).unwrap();
    assert_eq!(
        j.native_import_list(&scope(), None, 10)
            .unwrap()
            .operations
            .len(),
        2
    );
    // Complete the older wrapped operation so claim_next reaches this flat row.
    let claimed = j.claim_next().unwrap().unwrap();
    assert_eq!(claimed.id, wrapped.id);
    j.stop_attempt(wrapped.id, claimed.attempt.unwrap(), UploadState::Failed)
        .unwrap();
    let claimed = j.claim_next().unwrap().unwrap();
    assert_eq!(claimed.id, row.id);
    let remote = Node {
        id: "FILE::com.apple.CloudDocs::flat".into(),
        parent_id: Some(parent.into()),
        name: "Flat.numbers".into(),
        kind: NodeKind::Folder,
        size: semantic.expanded_bytes,
        modified_unix: 1,
        etag: Some("flat-v1".into()),
        content_version: None,
        target: None,
        package: true,
    };
    let mut wrong = remote.clone();
    wrong.name = "Flat.pages".into();
    assert!(
        j.acknowledge_package(
            row.id,
            claimed.attempt.unwrap(),
            PackageUploadReceipt {
                remote: wrong,
                semantic: semantic.clone()
            }
        )
        .is_err()
    );
    assert_eq!(payload(&j, row.id), archive);
    j.acknowledge_package(
        row.id,
        claimed.attempt.unwrap(),
        PackageUploadReceipt {
            remote: remote.clone(),
            semantic,
        },
    )
    .unwrap();
    let completed = j.get(row.id).unwrap();
    assert_eq!(
        j.package_publication_due(now_seconds())
            .unwrap()
            .unwrap()
            .id,
        row.id
    );
    j.finish_package_publication(
        &completed,
        PackagePublicationStatus::Present(remote),
        now_seconds(),
    )
    .unwrap();
    assert_eq!(
        j.native_import_list(&scope(), None, 10)
            .unwrap()
            .operations
            .len(),
        2
    );
    assert_eq!(payload(&j, row.id), archive);
    drop(j);
    let database_before = std::fs::read(root.join("uploads.db")).unwrap();
    {
        let repair =
            crate::journal::export::MetadataPublicationJournal::open(&root, "owned").unwrap();
        assert!(repair.due(0).unwrap().is_none());
    }
    assert_eq!(
        std::fs::read(root.join("uploads.db")).unwrap(),
        database_before
    );
    let recovery = RecoveryJournal::open(&root, "owned").unwrap();
    assert_eq!(recovery.list(0, 10).unwrap().len(), 2);
    // Completed uploads are historical receipts, not retained export stages.
    assert!(matches!(
        recovery.local_export_source(row.id),
        Err(JournalError::Stale)
    ));
    drop(recovery);
    let j = UploadJournal::open(&root, "owned", 1 << 20).unwrap();
    assert_eq!(
        serde_json::to_value(j.get(row.id).unwrap()).unwrap(),
        serde_json::to_value(completed).unwrap()
    );
    let version: u32 =
        j.db.pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
    assert_eq!(version, 21);
    // The prior writer's `version > JOURNAL_SCHEMA` fence rejects 21 when its
    // compiled maximum is 20. This is source-level, not an old-binary run.
    assert!(version > 20);
    assert_eq!(payload(&j, wrapped.id), b"retained wrapped bytes");
    assert_eq!(std::fs::read(source).unwrap(), archive);
}

#[test]
fn flat_numbers_publication_rejects_retained_matching_version_one_receipts() {
    use crate::native_import::{PackageSourceLayout, ValidatedPackageArchive};
    for endpoint in ["due", "status", "finish"] {
        let retained = private_tempdir().keep();
        let root = retained.join("journal");
        let mut journal = UploadJournal::open(&root, "owned", 1 << 20).unwrap();
        let bytes = crate::native_import::synthetic_package_archive(
            "Index/Document.iwa",
            b"owned flat publication corruption fixture",
        );
        let source = retained.join("source.numbers");
        std::fs::write(&source, &bytes).unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o400)).unwrap();
        let captured = ValidatedPackageArchive::capture_with_source_layout(
            &source,
            &retained,
            PackageSourceLayout::FlatNumbers,
            None,
            &CancellationToken::new(),
        )
        .unwrap();
        let parent = "FOLDER::com.apple.CloudDocs::owned";
        let record = journal
            .enqueue_validated_package_archive(
                scope(),
                UploadIntent::Create {
                    parent: parent.into(),
                    name: "Owned.numbers".into(),
                },
                captured,
                &CancellationToken::new(),
            )
            .unwrap();
        let UploadRepresentation::FlatNumbersArchive { semantic } = &record.representation else {
            panic!("actual flat capture missing")
        };
        assert_eq!(semantic.version, 2);
        let remote = Node {
            id: "FILE::com.apple.CloudDocs::owned-flat-publication".into(),
            parent_id: Some(parent.into()),
            name: "Owned.numbers".into(),
            kind: NodeKind::Folder,
            size: semantic.expanded_bytes,
            modified_unix: 1,
            etag: Some("owned-revision".into()),
            content_version: None,
            target: None,
            package: true,
        };
        let claimed = journal.claim_next().unwrap().unwrap();
        assert_eq!(claimed.id, record.id);
        journal
            .acknowledge_package(
                record.id,
                claimed.attempt.unwrap(),
                PackageUploadReceipt {
                    remote: remote.clone(),
                    semantic: semantic.clone(),
                },
            )
            .unwrap();
        // The genuine admission/acknowledgment first proves this is a due valid
        // V2 receipt; only the retained local row is then deliberately corrupted.
        assert_eq!(
            journal.package_publication_due(0).unwrap().unwrap().id,
            record.id
        );
        assert_eq!(
            journal.package_publication_status(record.id).unwrap(),
            PackagePublicationStatus::Pending
        );
        journal.db.execute(
            "UPDATE uploads SET body=json_set(body,'$.representation.semantic.version',1,'$.package_completion.version',1) WHERE id=?1",
            [record.id.to_string()],
        ).unwrap();
        let corrupted = journal.get(record.id).unwrap();
        assert!(corrupted.representation.validate().is_err());
        let row_before: String = journal
            .db
            .query_row(
                "SELECT body FROM uploads WHERE id=?1",
                [record.id.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        let publication_before: (i64, i64, i64, Option<String>) = journal.db.query_row(
            "SELECT done,failures,retry_after,observed FROM package_metadata_publication WHERE operation=?1",
            [record.id.to_string()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        ).unwrap();
        assert_eq!(publication_before, (0, 0, 0, None));
        let payload_before = payload(&journal, record.id);
        assert_eq!(payload_before, bytes);
        let outcome = match endpoint {
            "due" => journal.package_publication_due(0).map(|_| ()),
            "status" => journal.package_publication_status(record.id).map(|_| ()),
            "finish" => journal.finish_package_publication(
                &corrupted,
                PackagePublicationStatus::Present(remote),
                0,
            ),
            _ => unreachable!(),
        };
        let row_after: String = journal
            .db
            .query_row(
                "SELECT body FROM uploads WHERE id=?1",
                [record.id.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        let publication_after: (i64, i64, i64, Option<String>) = journal.db.query_row(
            "SELECT done,failures,retry_after,observed FROM package_metadata_publication WHERE operation=?1",
            [record.id.to_string()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        ).unwrap();
        assert_eq!(row_after, row_before, "{endpoint}: no upload transition");
        assert_eq!(
            publication_after, publication_before,
            "{endpoint}: corrupt job cannot complete or back off"
        );
        assert_eq!(payload(&journal, record.id), payload_before);
        assert_eq!(std::fs::read(&source).unwrap(), bytes);
        assert_eq!(
            std::fs::metadata(&source).unwrap().permissions().mode() & 0o777,
            0o400
        );
        // Baseline reaches here on due() with Ok(Some(corrupted)); it must not
        // be described as a setup/compile failure or a live provider failure.
        assert!(
            matches!(outcome, Err(JournalError::Corrupt | JournalError::Stale)),
            "{endpoint}: matching invalid proof versions must be refused"
        );
    }
}
