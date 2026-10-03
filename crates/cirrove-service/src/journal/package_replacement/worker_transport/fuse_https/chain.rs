//! Three atomic FUSE promotions precede real successive native HTTPS handoffs.
//! Standalone adapters are built after the existing internal resolver; no router claim.
use super::*;

const BODIES: [&[u8]; 4] = [
    b"old owned content",
    b"generation A body",
    b"generation B body",
    b"generation C body",
];
fn bytes(index: usize, root: &str) -> Vec<u8> {
    crate::native_import::synthetic_package_archive(&format!("{root}/Document"), BODIES[index])
}
fn proof(index: usize) -> PackageSemanticIdentity {
    let archive = bytes(index, "Target.pages");
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(&archive).unwrap();
    package_archive_semantic_identity_versioned(
        &file,
        &PackageDownload {
            size: archive.len() as u64,
            sha256: hex::encode(Sha256::digest(&archive)),
        },
        "Target.pages",
        2,
        &CancellationToken::new(),
    )
    .unwrap()
}
fn node(index: usize) -> Node {
    if index == 0 {
        return original();
    }
    Node {
        id: format!(
            "FILE::com.apple.CloudDocs::chain-{}",
            ["a", "b", "c"][index - 1]
        ),
        etag: Some(format!("chain-{index}-installed-v2")),
        ..original()
    }
}
fn native_binding(scope: &Scope, index: usize) -> (NativeArchiveBinding, Arc<ArchiveSnapshot>) {
    let source = node(index);
    let archive_bytes = bytes(index, "Target.pages");
    let artifact = Node {
        id: format!("icloud-artifact:{}", source.id),
        parent_id: Some(source.id.clone()),
        name: source.name.clone(),
        kind: NodeKind::File,
        size: archive_bytes.len() as u64,
        modified_unix: 0,
        etag: None,
        content_version: Some(format!(
            "icloud-artifact-v2:{}",
            json!({"source_etag":source.etag,"source_parent":source.parent_id,"source_size":source.size,"sha256":hex::encode(Sha256::digest(&archive_bytes))})
        )),
        target: None,
        package: false,
    };
    let snapshot = Arc::new(ArchiveSnapshot {
        identity: ReadIdentity::new(scope, &artifact).unwrap(),
        bytes: archive_bytes,
    });
    (
        NativeArchiveBinding {
            scope: scope.clone(),
            source,
            archive: artifact,
            semantic: proof(index),
        },
        snapshot,
    )
}
struct PendingChain {
    bindings: Vec<NativeArchiveBinding>,
    snapshots: Vec<Arc<ArchiveSnapshot>>,
    servers: Mutex<Vec<Arc<Mutex<State>>>>,
    resolutions: AtomicUsize,
}
impl PendingChain {
    fn current_index(&self) -> usize {
        self.servers
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.lock().unwrap().installed)
            .count()
    }
    fn current(&self) -> &NativeArchiveBinding {
        &self.bindings[self.current_index()]
    }
}
#[async_trait::async_trait]
impl MetadataProvider for PendingChain {
    fn provider_id(&self) -> &'static str {
        "icloud"
    }
    async fn changes(
        &self,
        _: &Scope,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<ChangePage, ProviderError> {
        let index = self.current_index();
        let mut changes = self.bindings[..index]
            .iter()
            .map(|b| Change::Delete {
                id: b.source.id.clone(),
            })
            .collect::<Vec<_>>();
        changes.push(Change::Upsert(self.bindings[index].source.clone()));
        Ok(ChangePage {
            changes,
            checkpoint: Checkpoint::Complete(Cursor("synthetic-native-chain-baseline".into())),
        })
    }
}
#[async_trait::async_trait]
impl ReadProvider for PendingChain {
    async fn node(
        &self,
        scope: &Scope,
        id: &str,
        _: &CancellationToken,
    ) -> std::result::Result<Node, ProviderError> {
        if scope != &self.bindings[0].scope {
            return Err(ProviderError::NotFound);
        }
        if id == FOLDER {
            return Ok(parent());
        }
        let binding = self.current();
        if id == binding.source.id {
            return Ok(binding.source.clone());
        }
        if id == binding.archive.id {
            return Ok(binding.archive.clone());
        }
        Err(ProviderError::NotFound)
    }
    async fn children(
        &self,
        scope: &Scope,
        parent_id: &str,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<DirectoryPage, ProviderError> {
        if scope != &self.bindings[0].scope {
            return Err(ProviderError::NotFound);
        }
        let binding = self.current();
        let nodes = if parent_id == FOLDER {
            vec![binding.source.clone()]
        } else if parent_id == binding.source.id {
            vec![binding.archive.clone()]
        } else {
            vec![]
        };
        Ok(DirectoryPage { nodes, next: None })
    }
    async fn read_range(
        &self,
        _: &Scope,
        _: &Node,
        _: u64,
        _: u32,
        _: &CancellationToken,
    ) -> std::result::Result<Vec<u8>, ProviderError> {
        panic!("native chain must use exact staged snapshot")
    }
    async fn resolve_native_archive(
        &self,
        scope: &Scope,
        node: &Node,
        _: &CancellationToken,
    ) -> std::result::Result<Option<NativeArchiveBinding>, ProviderError> {
        self.resolutions.fetch_add(1, Ordering::SeqCst);
        let binding = self.current();
        if scope != &binding.scope || node != &binding.archive {
            return Err(ProviderError::VersionChanged);
        }
        Ok(Some(binding.clone()))
    }
    async fn staged_content_session(
        &self,
        scope: &Scope,
        node: &Node,
        _: &CancellationToken,
    ) -> std::result::Result<Option<Arc<dyn ReadSession>>, ProviderError> {
        if self.resolutions.load(Ordering::SeqCst) == 0 {
            return Ok(None);
        }
        let Some(index) = self.bindings.iter().position(|b| b.archive.id == node.id) else {
            return Ok(None);
        };
        if index > self.current_index()
            || ReadIdentity::new(scope, node)? != self.snapshots[index].identity
        {
            return Ok(None);
        }
        Ok(Some(self.snapshots[index].clone()))
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires synthetic kernel FUSE and loopback TLS; no Apple credentials"]
async fn real_native_fuse_pending_atomic_chain_rebases_https_proofs_and_preserves_unread_readers() {
    // Independently encoded preregistered fixture digests, not provider receipts.
    for (index, expected_raw, expected_semantic) in [
        (
            0,
            "ebf518173b8e146d0a2ac52f14031f6d95d956896573dc8321f47b5eb4419e1e",
            "10cc7a7fb05f4050ed332906def99413e48155ba60e34994ade67104b5dc66b3",
        ),
        (
            1,
            "cd9a6a23e8e557a8b6f5c61034612b24f9cb9fd856713c15a02880b415875505",
            "2b3a1a01daacb9121d835551627359ef1e5291cb70f06086f38b1f058aa0f3de",
        ),
        (
            2,
            "ae02da16c848e2f6714fffcc9ee9f480508591112c12f940ac446e14f06e9255",
            "2bcbbe12679891442eabe0eb51b15a8d898ff47f4b4bac92f63354cec75d5188",
        ),
        (
            3,
            "bd2a1476fc2fd855e2521efc9ccecd5792683d58c865af0c889e35ce22799234",
            "2f221d772a9c4e23de6e422da2700f2c2fcd3354e687057e99e0f19b07a566d7",
        ),
    ] {
        let archive = bytes(index, "Target.pages");
        assert_eq!(archive.len(), 157);
        assert_eq!(hex::encode(Sha256::digest(&archive)), expected_raw);
        let semantic = proof(index);
        assert_eq!(
            (
                semantic.version,
                semantic.entries,
                semantic.files,
                semantic.expanded_bytes
            ),
            (2, 2, 1, 17)
        );
        assert_eq!(semantic.sha256, expected_semantic);
    }
    let root = directory();
    let staging = directory();
    let state = directory();
    let mount = root.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let account = account(&mount);
    let scope = Scope {
        account: account.id.clone(),
        provider: "icloud".into(),
        collection: "drive".into(),
    };
    let (bindings, snapshots): (Vec<_>, Vec<_>) = (0..4).map(|i| native_binding(&scope, i)).unzip();
    assert!(proof(0) != proof(1) && proof(1) != proof(2) && proof(2) != proof(3));
    let metadata = Arc::new(PendingChain {
        bindings,
        snapshots,
        servers: Mutex::new(Vec::new()),
        resolutions: AtomicUsize::new(0),
    });
    let journal_root = root.path().join("journal");
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&journal_root, &account.id, 8 * 1024 * 1024).unwrap(),
    ));
    let engine = Engine::new(
        account.clone(),
        metadata.clone(),
        state.path().to_path_buf(),
    )
    .await
    .unwrap();
    engine.start().await.unwrap();
    let fs = CloudFs::new_experimental_writable(engine.clone(), journal.clone())
        .await
        .unwrap();
    let control = fs.write_control().unwrap();
    let session = tokio::task::spawn_blocking({
        let mount = mount.clone();
        move || fs.mount(&mount)
    })
    .await
    .unwrap()
    .unwrap();
    let path = mount.join("Target.pages").join("Target.pages");
    let held = tokio::task::spawn_blocking({
        let path = path.clone();
        let journal = journal.clone();
        move || {
            let mut held = vec![File::open(&path).unwrap()];
            for index in 1..=3 {
                let temporary = path.parent().unwrap().join(format!(".chain-save-{index}"));
                let mut edit = std::fs::OpenOptions::new()
                    .create_new(true)
                    .read(true)
                    .write(true)
                    .open(&temporary)
                    .unwrap();
                edit.write_all(&bytes(index, "Target.pages")).unwrap();
                edit.sync_all().unwrap();
                assert_eq!(
                    journal.lock().unwrap().list(0, 10).unwrap().len(),
                    index - 1,
                    "temporary fsync must stay local"
                );
                std::fs::rename(&temporary, &path).expect("atomic native FUSE promotion");
                drop(edit);
                assert_eq!(
                    journal.lock().unwrap().list(0, 10).unwrap().len(),
                    index,
                    "one immutable native save per promotion"
                );
                if index < 3 {
                    held.push(File::open(&path).unwrap());
                }
            }
            // Neither O, A nor B has been read before provider completion.
            assert_eq!(std::fs::read(&path).unwrap(), bytes(3, "Target.pages"));
            held
        }
    })
    .await
    .unwrap();
    let queued = {
        let j = journal.lock().unwrap();
        let rows = j.list(0, 10).unwrap();
        assert_eq!(rows.len(), 3);
        for (offset, row) in rows.iter().enumerate() {
            assert_eq!(row.state, UploadState::Pending);
            assert!(
                row.attempt.is_none() && row.remote.is_none() && row.package_completion.is_none()
            );
            assert_eq!(
                row.sha256,
                hex::encode(Sha256::digest(bytes(offset + 1, "Target.pages")))
            );
            let UploadRepresentation::PackageReplacementArchive {
                original: before,
                original_semantic,
                semantic: next,
                ..
            } = &row.representation
            else {
                panic!("native chain downgraded into FileBytes")
            };
            assert_eq!(before.as_ref(), &original());
            assert_eq!(original_semantic, &proof(0));
            assert_eq!(next, &proof(offset + 1));
            if offset == 0 {
                assert!(row.base.is_none());
            } else {
                let base = row.base.as_ref().unwrap();
                assert_eq!(base.predecessor, rows[offset - 1].id);
                assert!(!base.resolved);
            }
        }
        rows
    };
    // C's active working slot and owner must survive every earlier acknowledgement.
    // Capture flags as they are after sealing rather than assuming dirty=true.
    let (newest, newest_owner) = {
        let j = journal.lock().unwrap();
        let active = j
            .working_files()
            .unwrap()
            .into_iter()
            .filter(|f| f.latest == Some(queued[2].id) && !f.unlinked)
            .collect::<Vec<_>>();
        assert_eq!(active.len(), 1);
        let owner = j.namespace_for_operation(queued[2].id).unwrap().unwrap();
        let native_slot: (String, String) =
            j.db.query_row(
                "SELECT working,owner FROM native_working_operations WHERE operation=?1",
                [queued[2].id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            native_slot,
            (active[0].id.to_string(), owner.id.to_string())
        );
        assert_eq!(owner.latest, Some(queued[2].id));
        (active.into_iter().next().unwrap(), owner)
    };
    let assert_newest = |j: &UploadJournal| {
        let current = j.working_file(newest.id).unwrap();
        assert_eq!(
            (
                current.id,
                current.generation,
                current.latest,
                current.dirty,
                current.unlinked
            ),
            (
                newest.id,
                newest.generation,
                newest.latest,
                newest.dirty,
                newest.unlinked
            ),
            "acknowledgement/publication changed newest C working identity or generation"
        );
        assert_eq!(current.node.id, newest.node.id);
        assert_eq!(current.node.size, newest.node.size);
        assert_eq!(
            j.read_working(newest.id, 0, 4096).unwrap(),
            bytes(3, "Target.pages")
        );
        let owner = j.namespace_for_operation(queued[2].id).unwrap().unwrap();
        assert_eq!(
            (owner.id, owner.latest),
            (newest_owner.id, newest_owner.latest)
        );
        assert!(!owner.unlinked);
    };
    assert_newest(&journal.lock().unwrap());
    let keys = Arc::new(WrappingKeys::default());
    let mut servers = Vec::new();
    for index in 1..=3 {
        // Explicit fixture seam: resolve before binding this standalone adapter.
        // The default account router independently constructs after worker claim.
        let dispatch = {
            let mut j = journal.lock().unwrap();
            assert!(j.resolve_ready_generations().unwrap());
            let row = j.get(queued[index - 1].id).unwrap();
            assert_eq!(row.state, UploadState::Pending);
            assert!(row.attempt.is_none());
            assert_eq!(row.sha256, queued[index - 1].sha256);
            assert_eq!(row.size, queued[index - 1].size);
            let UploadRepresentation::PackageReplacementArchive {
                original: before,
                original_semantic,
                semantic: next,
                ..
            } = &row.representation
            else {
                panic!("typed successor lost")
            };
            assert_eq!(before.as_ref(), &node(index - 1));
            assert_eq!(
                original_semantic,
                &proof(index - 1),
                "successor must rebase exact predecessor semantic proof before any HTTPS request"
            );
            assert_eq!(next, &proof(index));
            assert!(
                matches!(&row.intent,UploadIntent::Replace {item,expected_etag} if item==&node(index-1).id&&Some(expected_etag)==node(index-1).etag.as_ref())
            );
            if index > 1 {
                let previous = j.get(queued[index - 2].id).unwrap();
                let (_, current, _) = previous.native_replacement_receipt().unwrap();
                assert_eq!(before.as_ref(), current);
                assert_eq!(
                    original_semantic,
                    previous.package_completion.as_ref().unwrap()
                );
                let base = row.base.as_ref().unwrap();
                assert!(base.resolved);
                assert_eq!(base.predecessor, previous.id);
            }
            row
        };
        let generation = Generation {
            original: node(index - 1),
            current: node(index),
            original_body: BODIES[index - 1].to_vec(),
            current_body: BODIES[index].to_vec(),
            staged_etag: format!("chain-{index}-staged-v1"),
            trash_etag: format!("chain-{index}-trash-v2"),
        };
        let server = Server::start_generation(
            Plan {
                target_name: "Target.pages".into(),
                staged_name: format!("staged-by-cirrove-{}.pages", dispatch.id),
            },
            bytes(index, "Target.pages"),
            generation,
        )
        .await;
        metadata.servers.lock().unwrap().push(server.state.clone());
        assert_eq!(counts(&server), (0, 0, 0, 0, 0));
        let vault = Arc::new(
            SealedUploadCheckpointVault::with_test_key_vault(
                state.path(),
                &account.id,
                keys.clone(),
            )
            .unwrap(),
        );
        let worker = TransferWorker::new(
            journal.clone(),
            provider(&server, staging.path(), &dispatch),
            vault,
            CancellationToken::new(),
        );
        let result = tokio::time::timeout(Duration::from_secs(30), worker.run_once())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(result.id, dispatch.id);
        assert_eq!(result.state, UploadState::Uploaded);
        assert!(result.issue.is_none());
        drop(worker);
        {
            let j = journal.lock().unwrap();
            let saved = j.get(dispatch.id).unwrap();
            let (before, current, backup) = saved
                .native_replacement_receipt()
                .expect("actual HTTPS worker typed handoff");
            assert_eq!(before, &node(index - 1));
            assert_eq!(current, &node(index));
            assert_eq!(backup.id, node(index - 1).id);
            assert_eq!(
                backup.etag.as_deref(),
                Some(format!("chain-{index}-trash-v2").as_str())
            );
            assert_eq!(backup.parent_id.as_deref(), Some(TRASH_ROOT));
            assert_eq!(saved.package_completion, Some(proof(index)));
            let mut payload = Vec::new();
            j.payload(saved.id)
                .unwrap()
                .read_to_end(&mut payload)
                .unwrap();
            assert_eq!(payload, bytes(index, "Target.pages"));
            let rows = j.list(0, 10).unwrap();
            assert_eq!(
                rows.iter().map(|r| r.id).collect::<Vec<_>>(),
                queued.iter().map(|r| r.id).collect::<Vec<_>>()
            );
            for later in rows.iter().skip(index) {
                assert_eq!(later.state, UploadState::Pending);
                assert!(later.attempt.is_none() && later.remote.is_none());
            }
            assert_newest(&j);
        }
        assert_eq!(counts(&server), (1, 1, 1, 1, 1));
        tokio::task::spawn_blocking({
            let path = path.clone();
            move || {
                assert_eq!(
                    std::fs::read(path).unwrap(),
                    bytes(3, "Target.pages"),
                    "earlier completion overwrote newest local C"
                )
            }
        })
        .await
        .unwrap();
        // Publication due selection is UUID-ordered: drain this phase completely
        // before the next operation is acknowledged, so no generation is skipped.
        assert!(control.publish_completed_package().await.unwrap());
        assert!(!control.publish_completed_package().await.unwrap());
        assert_newest(&journal.lock().unwrap());
        assert_eq!(
            journal
                .lock()
                .unwrap()
                .package_publication_status(dispatch.id)
                .unwrap(),
            PackagePublicationStatus::Present(node(index))
        );
        tokio::task::spawn_blocking({
            let path = path.clone();
            move || {
                assert_eq!(
                    std::fs::read(path).unwrap(),
                    bytes(3, "Target.pages"),
                    "intermediate publication overwrote newest local C"
                )
            }
        })
        .await
        .unwrap();
        servers.push(server);
    }
    tokio::task::spawn_blocking({
        let path = path.clone();
        move || {
            for (index, mut reader) in held.into_iter().enumerate() {
                reader.seek(SeekFrom::Start(0)).unwrap();
                let mut retained = Vec::new();
                reader.read_to_end(&mut retained).unwrap();
                assert_eq!(
                    retained,
                    bytes(index, "Target.pages"),
                    "unread original/A/B descriptor followed a successor"
                );
            }
            assert_eq!(std::fs::read(path).unwrap(), bytes(3, "Target.pages"));
        }
    })
    .await
    .unwrap();
    engine.stop().await;
    tokio::task::spawn_blocking(move || session.umount_and_join())
        .await
        .unwrap()
        .unwrap();
    drop(control);
    drop(engine);
    drop(journal);
    let journal = Arc::new(Mutex::new(
        UploadJournal::open(&journal_root, &account.id, 8 * 1024 * 1024).unwrap(),
    ));
    let engine = Engine::new(account.clone(), metadata, state.path().to_path_buf())
        .await
        .unwrap();
    engine.start().await.unwrap();
    let fs = CloudFs::new_experimental_writable(engine.clone(), journal.clone())
        .await
        .unwrap();
    let session = tokio::task::spawn_blocking({
        let mount = mount.clone();
        move || fs.mount(&mount)
    })
    .await
    .unwrap()
    .unwrap();
    tokio::task::spawn_blocking({
        let path = path.clone();
        move || {
            assert_eq!(std::fs::read(&path).unwrap(), bytes(3, "Target.pages"));
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .unwrap();
            file.sync_all().unwrap();
        }
    })
    .await
    .unwrap();
    let retained = {
        let j = journal.lock().unwrap();
        let rows = j.list(0, 10).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows.iter().map(|r| r.id).collect::<Vec<_>>(),
            queued.iter().map(|r| r.id).collect::<Vec<_>>()
        );
        for (offset, row) in rows.iter().enumerate() {
            assert_eq!(row.state, UploadState::Uploaded);
            assert_eq!(row.remote.as_ref(), Some(&node(offset + 1)));
            assert_eq!(row.package_completion, Some(proof(offset + 1)));
        }
        rows
    };
    let vault = Arc::new(
        SealedUploadCheckpointVault::with_test_key_vault(state.path(), &account.id, keys).unwrap(),
    );
    let worker = TransferWorker::new(
        journal.clone(),
        provider(servers.last().unwrap(), staging.path(), &retained[2]),
        vault,
        CancellationToken::new(),
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(5), worker.run_once())
            .await
            .unwrap()
            .unwrap()
            .is_none(),
        "completed chain replayed after remount/clean fsync"
    );
    drop(worker);
    for server in &servers {
        assert_eq!(counts(server), (1, 1, 1, 1, 1));
    }
    engine.stop().await;
    tokio::task::spawn_blocking(move || session.umount_and_join())
        .await
        .unwrap()
        .unwrap();
}
