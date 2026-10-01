//! Synthetic typed-receipt injection: kernel identity/publication, not cloud protocol.
use super::*;
use cirrove_core::upload::{
    PackageHandoffReceipt, PackageSemanticIdentity, PackageUploadReceipt, RecoveryLocation,
    UploadRepresentation,
};
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
fn artifact(parent: &Node, bytes: &[u8]) -> Node {
    Node {
        id: format!("icloud-artifact:{}", parent.id),
        parent_id: Some(parent.id.clone()),
        name: parent.name.clone(),
        kind: NodeKind::File,
        size: bytes.len() as u64,
        modified_unix: 1,
        etag: None,
        content_version: Some(format!(
            "synthetic-verified:{}",
            hex::encode(Sha256::digest(bytes))
        )),
        package: false,
        target: None,
    }
}
fn done(path: &Path, op: uuid::Uuid) -> bool {
    let db =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM package_metadata_publication WHERE operation=?1 AND done=1)",
        [op.to_string()],
        |r| r.get(0),
    )
    .unwrap()
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE"]
async fn real_native_replacement_publication_rebinds_warm_package_and_preserves_held_reader() {
    for interrupted in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let mount = temp.path().join("mount");
        std::fs::create_dir(&mount).unwrap();
        let mut account = account(&mount);
        account.registration = AppRegistration::ICloud;
        account.root_id = "FOLDER::com.apple.CloudDocs::root".into();
        let cloud = Arc::new(Cloud::default());
        cloud.icloud_identity.store(true, Ordering::SeqCst);
        let old = Node {
            id: "FILE::com.apple.CloudDocs::old-native".into(),
            parent_id: Some(account.root_id.clone()),
            name: "Held.pages".into(),
            kind: NodeKind::Folder,
            package: true,
            size: 5,
            modified_unix: 1,
            etag: Some("old-v1".into()),
            content_version: None,
            target: None,
        };
        let current = Node {
            id: "FILE::com.apple.CloudDocs::new-native".into(),
            size: 9,
            etag: Some("new-v1".into()),
            ..old.clone()
        };
        let oldbytes = vec![17u8; 128 * 1024];
        let newbytes = vec![29u8; 96 * 1024];
        let oldartifact = artifact(&old, &oldbytes);
        let newartifact = artifact(&current, &newbytes);
        {
            let mut remote = cloud.remote.lock().unwrap();
            remote.files.insert(old.id.clone(), (old.clone(), vec![]));
            remote.files.insert(
                oldartifact.id.clone(),
                (oldartifact.clone(), oldbytes.clone()),
            );
        }
        let journalpath = temp.path().join("journal");
        let db = journalpath.join("uploads.db");
        let mut journal = Arc::new(Mutex::new(
            UploadJournal::open(&journalpath, &account.id, 1024 * 1024).unwrap(),
        ));
        let state = temp.path().join("state");
        let mut engine = Engine::new(account.clone(), cloud.clone(), state.clone())
            .await
            .unwrap();
        let mut session = WritableSession::mount(
            engine.clone(),
            journal.clone(),
            cloud.clone(),
            Arc::new(Vault::default()),
        )
        .await
        .unwrap();
        let path = mount.join(&old.name).join(&oldartifact.name);
        let held = tokio::task::spawn_blocking({
            let path = path.clone();
            let expected = oldbytes.clone();
            move || {
                let mut f = std::fs::File::open(path).unwrap();
                let mut bytes = vec![];
                f.read_to_end(&mut bytes).unwrap();
                assert_eq!(bytes, expected);
                f
            }
        })
        .await
        .unwrap();
        let scope = engine.scope("drive");
        let source = temp.path().join("source.zip");
        std::fs::write(&source,hex::decode("504b03041400000000006b90415deceb84f4150000001500000015000000536f757263652e70616765732f446f63756d656e7473796e746865746963207265706c6163656d656e74504b010214031400000000006b90415deceb84f41500000015000000150000000000000000000000800100000000536f757263652e70616765732f446f63756d656e74504b0506000000000100010043000000480000000000").unwrap()).unwrap();
        let archive = cirrove_service::native_import::ValidatedPackageArchive::capture(
            &source,
            temp.path(),
            "Source.pages",
            &CancellationToken::new(),
        )
        .unwrap();
        let original_semantic = PackageSemanticIdentity {
            version: 1,
            sha256: "a".repeat(64),
            entries: 1,
            files: 1,
            expanded_bytes: 5,
        };
        let ready = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        *cloud.native_publication_pause.lock().unwrap() = Some(NativePublicationPause {
            item: current.id.clone(),
            ready: ready.clone(),
            release: release.clone(),
        });
        let row = {
            let mut j = journal.lock().unwrap();
            let row = j
                .enqueue_validated_package_replacement(
                    scope.clone(),
                    old.clone(),
                    original_semantic.clone(),
                    archive,
                    &CancellationToken::new(),
                )
                .unwrap();
            let attempt = j.claim_next().unwrap().unwrap();
            assert_eq!(attempt.id, row.id);
            let attempt = attempt.attempt.unwrap();
            j.reserve_identity_handoff(
                row.id,
                attempt,
                RecoveryLocation::Trash {
                    local_name: format!("recovery-by-cirrove-{}.pages", row.id),
                    parent: "FOLDER::com.apple.CloudDocs::TRASH_ROOT".into(),
                },
            )
            .unwrap();
            let UploadRepresentation::PackageReplacementArchive { semantic, .. } =
                &row.representation
            else {
                panic!("package qualifier")
            };
            {
                let mut remote = cloud.remote.lock().unwrap();
                remote.files.remove(&old.id);
                remote.files.remove(&oldartifact.id);
                remote
                    .files
                    .insert(current.id.clone(), (current.clone(), vec![]));
                remote.files.insert(
                    newartifact.id.clone(),
                    (newartifact.clone(), newbytes.clone()),
                );
            }
            j.acknowledge_package_handoff(
                row.id,
                attempt,
                PackageHandoffReceipt {
                    original: old.clone(),
                    current: PackageUploadReceipt {
                        remote: current.clone(),
                        semantic: semantic.clone(),
                    },
                    backup: PackageUploadReceipt {
                        remote: Node {
                            parent_id: Some("FOLDER::com.apple.CloudDocs::TRASH_ROOT".into()),
                            etag: Some("trash-v2".into()),
                            ..old.clone()
                        },
                        semantic: original_semantic,
                    },
                },
            )
            .unwrap();
            j.get(row.id).unwrap()
        };
        let retained = serde_json::to_vec(&row).unwrap();
        tokio::time::timeout(Duration::from_secs(10), ready.notified())
            .await
            .expect("publication did not attempt exact new identity");
        assert!(
            !done(&db, row.id),
            "paused publication falsely recorded done"
        );
        let mut held = Some(
            tokio::task::spawn_blocking(move || {
                let mut held = held;
                held.seek(SeekFrom::Start(0)).unwrap();
                let mut bytes = vec![];
                held.read_to_end(&mut bytes).unwrap();
                assert_eq!(bytes, oldbytes);
                held
            })
            .await
            .unwrap(),
        );
        if interrupted {
            drop(held.take());
            session.shutdown().await.unwrap();
            drop(engine);
            drop(journal);
            *cloud.native_publication_pause.lock().unwrap() = None;
            journal = Arc::new(Mutex::new(
                UploadJournal::open(&journalpath, &account.id, 1024 * 1024).unwrap(),
            ));
            assert_eq!(
                serde_json::to_vec(&journal.lock().unwrap().get(row.id).unwrap()).unwrap(),
                retained
            );
            engine = Engine::new(account, cloud.clone(), state).await.unwrap();
            session = WritableSession::mount(
                engine.clone(),
                journal.clone(),
                cloud.clone(),
                Arc::new(Vault::default()),
            )
            .await
            .unwrap();
        } else {
            *cloud.native_publication_pause.lock().unwrap() = None;
            release.notify_waiters();
        }
        tokio::time::timeout(Duration::from_secs(10), async {
            while !done(&db, row.id) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("replacement publication not durably acknowledged");
        let expected = newbytes.clone();
        let root = mount.clone();
        let expected_name = current.name.clone();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let root = root.clone();
                let path = path.clone();
                let name = expected_name.clone();
                let expected = expected.clone();
                let converged = tokio::task::spawn_blocking(move || {
                    let names: Vec<_> = std::fs::read_dir(root)
                        .unwrap()
                        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                        .collect();
                    names == vec![name] && std::fs::read(path).is_ok_and(|b| b == expected)
                })
                .await
                .unwrap();
                if converged {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("warm package aliases/artifact did not converge to replacement");
        if let Some(mut held) = held {
            tokio::task::spawn_blocking(move || {
                held.seek(SeekFrom::Start(0)).unwrap();
                let mut bytes = vec![];
                held.read_to_end(&mut bytes).unwrap();
                assert_eq!(bytes, vec![17u8; 128 * 1024]);
            })
            .await
            .unwrap();
        }
        let store = cirrove_store::Store::open(&engine.db).unwrap();
        assert!(store.node(&scope, &current.id).unwrap().is_some());
        assert_eq!(
            cloud.write_calls.load(Ordering::SeqCst),
            0,
            "receipt publication/reopen dispatched upload"
        );
        assert_eq!(
            serde_json::to_vec(&journal.lock().unwrap().get(row.id).unwrap()).unwrap(),
            retained
        );
        session.shutdown().await.unwrap();
    }
}
