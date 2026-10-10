use super::*;
#[tokio::test]
async fn native_trash_list_manager_readonly_discovers_without_writer_or_provider_io() {
    let f = Fixture::new().await;
    let mut account = f.engine.account.clone();
    account.access = cirrove_auth::AccessMode::ReadOnly;
    let engine = Engine::new(
        account.clone(),
        f.provider.clone(),
        f._temp.path().join("readonly-state"),
    )
    .await
    .unwrap();
    let root = engine.db.parent().unwrap().join("journal");
    let mut journal = crate::journal::UploadJournal::open(&root, &account.id, 1024 * 1024).unwrap();
    let mut before = node(
        "FILE::com.apple.CloudDocs::owned",
        Some(ROOT),
        "Owned.pages",
    );
    before.package = true;
    let record = journal
        .enqueue_mutation(cirrove_core::mutation::MutationRequest {
            scope: engine.scope("drive"),
            intent: cirrove_core::mutation::MutationIntent::TrashNativeDocument { before },
        })
        .unwrap();
    drop(journal);
    let manager = Arc::new(Manager::default());
    manager
        .engines
        .write()
        .await
        .insert(account.id.clone(), engine.clone());
    let before_roots = f.provider.roots.load(Ordering::SeqCst);
    let page = manager
        .list_native_trash(&engine, &account.id, None, 10)
        .await
        .unwrap();
    assert_eq!(page.operations.len(), 1);
    assert_eq!(page.operations[0].operation, record.id);
    assert_eq!(
        page.operations[0].state,
        crate::journal::MutationState::Pending
    );
    assert!(
        !page.operations[0].removal_receipt_recorded
            && !page.operations[0].metadata_absence_recorded
    );
    assert!(manager.writers.read().await.is_empty());
    assert_eq!(f.provider.roots.load(Ordering::SeqCst), before_roots);
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), 0);
    assert!(
        manager
            .list_native_trash(&engine, &uuid::Uuid::new_v4().to_string(), None, 10)
            .await
            .is_err()
    );
    manager.engines.write().await.remove(&account.id);
    assert!(
        manager
            .list_native_trash(&engine, &account.id, None, 10)
            .await
            .is_err()
    );
    let read = crate::journal::RecoveryJournal::open(&root, &account.id).unwrap();
    let unchanged = read
        .native_trash_list(&engine.scope("drive"), None, 10)
        .unwrap();
    assert_eq!(unchanged.operations.len(), 1);
    assert_eq!(
        unchanged.operations[0].state,
        crate::journal::MutationState::Pending
    );
}

struct NoProviderIo;
#[async_trait::async_trait]
impl MetadataProvider for NoProviderIo {
    fn provider_id(&self) -> &'static str {
        "icloud"
    }
    async fn changes(
        &self,
        _: &Scope,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<ChangePage, ProviderError> {
        panic!("retained discovery called changes")
    }
}
#[async_trait::async_trait]
impl ReadProvider for NoProviderIo {
    async fn node(
        &self,
        _: &Scope,
        _: &str,
        _: &CancellationToken,
    ) -> std::result::Result<Node, ProviderError> {
        panic!("retained discovery called node")
    }
    async fn children(
        &self,
        _: &Scope,
        _: &str,
        _: Option<&Cursor>,
        _: &CancellationToken,
    ) -> std::result::Result<DirectoryPage, ProviderError> {
        panic!("retained discovery called children")
    }
    async fn read_range(
        &self,
        _: &Scope,
        _: &Node,
        _: u64,
        _: u32,
        _: &CancellationToken,
    ) -> std::result::Result<Vec<u8>, ProviderError> {
        panic!("retained discovery downloaded content")
    }
}
#[tokio::test]
async fn native_trash_socket_lost_reply_then_readonly_manager_restart_recovers_operation() {
    use tokio::io::AsyncWriteExt;
    let mut f = Fixture::new().await;
    // Match the normal daemon/recovery journal path rather than the older
    // import-only fixture's deliberately isolated uploads directory.
    let journal = Arc::new(Mutex::new(
        crate::journal::UploadJournal::open(
            &f.engine.db.parent().unwrap().join("journal"),
            &f.engine.account.id,
            1024 * 1024,
        )
        .unwrap(),
    ));
    let fs = CloudFs::new_experimental_writable(f.engine.clone(), journal.clone())
        .await
        .unwrap();
    let control = fs.write_control().unwrap();
    f.control.freeze();
    f.manager
        .writers
        .write()
        .await
        .insert(f.engine.account.id.clone(), control.clone());
    f.control = control;
    f.journal = journal;
    drop(fs);
    let mut target = node(
        "FILE::com.apple.CloudDocs::lost-reply",
        Some(ROOT),
        "Lost reply.pages",
    );
    target.package = true;
    f.provider.nodes.lock().unwrap().push(target.clone());
    let mut account = f.engine.account.clone();
    let request = crate::TrashNativeDocumentRequest {
        label: account.label.clone(),
        expected_account_id: account.id.clone(),
        path: target.name.clone(),
        item_id: target.id.clone(),
        etag: target.etag.clone().unwrap(),
    };
    let socket = f._temp.path().join("first.sock");
    let cancel = CancellationToken::new();
    let server = tokio::spawn(crate::serve_managed(
        f.engine.db.clone(),
        socket.clone(),
        cancel.clone(),
        Some(f.manager.clone()),
    ));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !socket.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // Send the real protocol request, then lose the socket without reading any
    // job/operation response. The next public interaction happens after restart.
    let mut stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
    let line = format!(
        "trash-native-document {}\n",
        serde_json::to_string(&request).unwrap()
    );
    stream.write_all(line.as_bytes()).await.unwrap();
    drop(stream);
    let committed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(row) = f
                .journal
                .lock()
                .unwrap()
                .list_mutations(0, 10)
                .unwrap()
                .into_iter()
                .next()
            {
                break row;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(committed.state, crate::journal::MutationState::Pending);
    f.engine.stop().await;
    cancel.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let Fixture {
        _temp,
        manager,
        engine,
        control,
        journal,
        provider,
        ..
    } = f;
    manager.writers.write().await.clear();
    manager.engines.write().await.clear();
    drop(control);
    drop(journal);
    drop(manager);
    drop(engine);
    drop(provider);
    account.access = cirrove_auth::AccessMode::ReadOnly;
    let restarted = Engine::new(
        account.clone(),
        Arc::new(NoProviderIo),
        _temp.path().join("state"),
    )
    .await
    .unwrap();
    assert!(restarted.jobs.list().is_empty());
    let manager = Arc::new(Manager::default());
    manager
        .engines
        .write()
        .await
        .insert(account.id.clone(), restarted.clone());
    assert!(manager.writers.read().await.is_empty());
    manager.status.write().await.push(AccountStatus {
        account_id: account.id.clone(),
        label: account.label.clone(),
        mount_path: account.mount_path.clone(),
        enabled: true,
        mounted: false,
        ..Default::default()
    });
    let socket = _temp.path().join("restarted.sock");
    let cancel = CancellationToken::new();
    let server = tokio::spawn(crate::serve_managed(
        restarted.db.clone(),
        socket.clone(),
        cancel.clone(),
        Some(manager.clone()),
    ));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !socket.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let list = crate::ListNativeTrashRequest {
        label: account.label.clone(),
        expected_account_id: account.id.clone(),
        after: None,
        limit: 1,
    };
    let reply = crate::list_native_trash(&socket, &list).await.unwrap();
    assert!(reply.refusal.is_none());
    assert_eq!(reply.operations.len(), 1);
    assert!(reply.next.is_none());
    let found = &reply.operations[0];
    assert_eq!(found.operation, committed.id);
    assert_eq!(found.item_id, request.item_id);
    assert_eq!(found.etag, request.etag);
    assert_eq!(found.name, request.path);
    assert_eq!(found.state, crate::journal::MutationState::Pending);
    assert!(!found.removal_receipt_recorded && !found.metadata_absence_recorded);
    assert!(restarted.jobs.list().is_empty());
    assert!(manager.writers.read().await.is_empty());
    // Repeated discovery is still an observer, not a retry or job recreation.
    let again = crate::list_native_trash(&socket, &list).await.unwrap();
    assert_eq!(again.operations.len(), 1);
    assert_eq!(again.operations[0].operation, committed.id);
    let mut wrong = list;
    wrong.expected_account_id = uuid::Uuid::new_v4().to_string();
    let refused = crate::list_native_trash(&socket, &wrong).await.unwrap();
    assert!(refused.refusal.is_some() && refused.operations.is_empty());
    cancel.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    restarted.stop().await;
}
