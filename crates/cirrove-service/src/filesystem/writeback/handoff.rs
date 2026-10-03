//! Per-object access leases and bounded cleanup of fully acknowledged edits.
use super::*;
use tokio::sync::{OwnedRwLockReadGuard, RwLock};

#[derive(Clone)]
pub(crate) struct FileLease {
    _inner: Arc<Lease>,
}

struct Lease {
    guard: Option<OwnedRwLockReadGuard<()>>,
    wake: Arc<tokio::sync::Notify>,
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.guard.take();
        self.wake.notify_waiters();
    }
}
impl Writeback {
    pub(super) fn activity_gate(&self, identity: EditKey) -> Result<Arc<RwLock<()>>> {
        let mut gates = self.activity.lock().map_err(|_| Errno::EIO)?;
        gates.retain(|_, gate| gate.strong_count() > 0);
        if let Some(gate) = gates.get(&identity).and_then(Weak::upgrade) {
            return Ok(gate);
        }
        let gate = Arc::new(RwLock::new(()));
        gates.insert(identity, Arc::downgrade(&gate));
        Ok(gate)
    }
    pub async fn lease(
        &self,
        scope: &Scope,
        item: &str,
        cancel: &CancellationToken,
    ) -> Result<FileLease> {
        // Callers hold a projected local identity. A provider acknowledgement
        // cannot change this gate or move an existing handle to another object.
        let gate = self.activity_gate(key(scope, item))?;
        let guard = tokio::select! { biased;
            _=cancel.cancelled()=>return Err(Errno::ENODEV),
            guard=gate.read_owned()=>guard,
        };
        Ok(FileLease {
            _inner: Arc::new(Lease {
                guard: Some(guard),
                wake: self.wake.clone(),
            }),
        })
    }
    pub fn remote_identity(&self, scope: &Scope, item: &str) -> Result<Option<String>> {
        let projection = self.projection.lock().map_err(|_| Errno::EIO)?;
        Ok(projection
            .local_object(scope, item)
            .and_then(|o| o.remote.as_ref())
            .map(|r| r.id.clone()))
    }
    pub fn follows_remote(&self, scope: &Scope, item: &str) -> Result<bool> {
        let projection = self.projection.lock().map_err(|_| Errno::EIO)?;
        Ok(projection
            .local_object(scope, item)
            .is_some_and(|o| o.follows_remote))
    }
    /// After publishing pending namespace changes, inspect at most 16 cleanup
    /// candidates and make one provider request. The cursor makes pending/open
    /// objects yield to others. Publication also repairs a missed callback
    /// without replaying the provider operation.
    pub async fn maintain(self: &Arc<Self>, engine: &Engine) -> Result<bool> {
        if self.refresh_projection().await? {
            engine.changed.notify_waiters();
        }
        let prepared = self.prepare_replacement_source(engine).await?;
        if self.preserve_unlinked(engine).await? {
            return Ok(true);
        }
        if prepared {
            return Ok(true);
        }
        let after = *self.maintenance_cursor.lock().map_err(|_| Errno::EIO)?;
        let batch = self
            .local(move |j| {
                j.collect_retired_working(16)?;
                j.collect_uploaded_payloads(16)?;
                j.handoff_candidates(after, 16)?
                    .into_iter()
                    .map(|object| {
                        let clean = j.namespace_is_clean(&object)?;
                        Ok((object, clean))
                    })
                    .collect::<crate::journal::Result<Vec<_>>>()
            })
            .await?;
        if batch.is_empty() {
            *self.maintenance_cursor.lock().map_err(|_| Errno::EIO)? = None;
            return Ok(false);
        }
        for (object, clean) in batch {
            *self.maintenance_cursor.lock().map_err(|_| Errno::EIO)? = Some(object.id);
            if !clean {
                if let Some(progress) = self.maintain_native(engine, object.id).await? {
                    return Ok(progress);
                }
                continue;
            }
            if self
                .maintenance_retries
                .lock()
                .map_err(|_| Errno::EIO)?
                .get(&object.id)
                .is_some_and(|(_, at)| *at > tokio::time::Instant::now())
            {
                continue;
            }
            let gate = self.activity_gate(key(&object.scope, &object.node.id))?;
            // Check idleness before starting network work, but do not block
            // applications during that request. The journal revision is checked
            // again under exclusive access after the reply.
            let Ok(idle) = gate.clone().try_write_owned() else {
                continue;
            };
            drop(idle);
            let expected = object.remote.as_ref().ok_or(Errno::EIO)?.clone();
            let db = engine.db.clone();
            let scope = object.scope.clone();
            let item = expected.id.clone();
            let cached = tokio::task::spawn_blocking(move || Store::open(db)?.node(&scope, &item))
                .await
                .map_err(|_| Errno::EIO)?
                .map_err(|_| Errno::EIO)?;
            // A folder creation receipt can already be in the metadata cache
            // while its provider ETag has settled to a different value. Reusing
            // that receipt would clear `latest` and permit rmdir with the stale
            // precondition. Observe folders independently before handoff.
            let remote = if expected.kind != NodeKind::Folder && cached.as_ref() == Some(&expected)
            {
                expected
            } else {
                let response = tokio::select! {biased;
                    _=engine.cancel.cancelled()=>return Ok(false),
                    result=tokio::time::timeout(Duration::from_secs(30),engine.refresh_node(&object.scope,&expected.id))=> {
                        match result {
                            Ok(Ok(node))=>Ok(node),
                            // refresh_node has committed the ordered absence.
                            // Keep the alias identity, but release this fully
                            // acknowledged local overlay of a deleted remote file.
                            Ok(Err(ProviderError::NotFound))=>Ok(expected),
                            Ok(Err(error))=>Err(errno(&error)),
                            Err(_)=>Err(Errno::ETIMEDOUT),
                        }
                    },
                };
                match response {
                    Ok(node) => node,
                    Err(error) => {
                        let mut retries =
                            self.maintenance_retries.lock().map_err(|_| Errno::EIO)?;
                        let failures = retries
                            .get(&object.id)
                            .map_or(1, |(n, _)| n.saturating_add(1).min(6));
                        retries.insert(
                            object.id,
                            (
                                failures,
                                tokio::time::Instant::now()
                                    + Duration::from_secs((1u64 << failures).min(60)),
                            ),
                        );
                        return Err(error);
                    }
                }
            };
            let remote =
                self.present_observation(object.remote.as_ref().ok_or(Errno::EIO)?, remote)?;
            self.maintenance_retries
                .lock()
                .map_err(|_| Errno::EIO)?
                .remove(&object.id);
            let Ok(idle) = gate.try_write_owned() else {
                return Ok(true);
            };
            let writer = self.clone();
            let result = tokio::task::spawn_blocking(move || {
                let _idle = idle;
                let mut journal = writer.journal.lock().map_err(|_| Errno::EIO)?;
                let following = journal
                    .following_namespace(&object, remote.clone())
                    .map_err(error)?;
                // Reserve and validate the in-memory publication before the
                // durable detach. Once committed, applying it is infallible.
                Self::publish_locked(&journal, &writer.projection)?;
                let mut projection = writer.projection.lock().map_err(|_| Errno::EIO)?;
                if !projection.validate_merge(&following, None)? {
                    return Err(Errno::ESTALE);
                }
                let committed = journal
                    .handoff_namespace(object.id, object.revision, remote)
                    .map_err(error)?;
                projection.apply(committed, None, None);
                Ok(())
            })
            .await
            .map_err(|_| Errno::EIO)?;
            match result {
                Ok(()) => engine.changed.notify_waiters(),
                Err(e) if e == Errno::ESTALE => {}
                Err(e) => return Err(e),
            }
            return Ok(true);
        }
        Ok(false)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use cirrove_auth::{AccessMode, AppRegistration, Identity};
    use cirrove_core::{ChangePage, Cursor, DirectoryPage, MetadataProvider, ReadProvider};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    struct Provider {
        node: Node,
        hold: AtomicBool,
        fail: AtomicBool,
        calls: AtomicUsize,
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }
    #[async_trait]
    impl MetadataProvider for Provider {
        fn provider_id(&self) -> &'static str {
            "fixture"
        }
        async fn changes(
            &self,
            _: &Scope,
            _: Option<&Cursor>,
            _: &CancellationToken,
        ) -> std::result::Result<ChangePage, ProviderError> {
            Err(ProviderError::Unavailable)
        }
    }
    #[async_trait]
    impl ReadProvider for Provider {
        async fn node(
            &self,
            _: &Scope,
            _: &str,
            _: &CancellationToken,
        ) -> std::result::Result<Node, ProviderError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail.load(Ordering::SeqCst) {
                return Err(ProviderError::Unavailable);
            }
            if self.hold.swap(false, Ordering::SeqCst) {
                self.entered.notify_one();
                // Deliberately ignore cancellation: the maintenance boundary
                // must be able to abandon this network future safely.
                self.release.notified().await;
            }
            Ok(self.node.clone())
        }
        async fn children(
            &self,
            _: &Scope,
            _: &str,
            _: Option<&Cursor>,
            _: &CancellationToken,
        ) -> std::result::Result<DirectoryPage, ProviderError> {
            Err(ProviderError::Unavailable)
        }
        async fn read_range(
            &self,
            _: &Scope,
            _: &Node,
            _: u64,
            _: u32,
            _: &CancellationToken,
        ) -> std::result::Result<Vec<u8>, ProviderError> {
            Err(ProviderError::Unavailable)
        }
    }
    struct Fixture {
        _temp: tempfile::TempDir,
        engine: Arc<Engine>,
        writer: Arc<Writeback>,
        journal: Arc<Mutex<UploadJournal>>,
        provider: Arc<Provider>,
        working: WorkingFile,
    }
    impl Fixture {
        async fn new(hold: bool) -> Self {
            let temp = tempfile::tempdir().unwrap();
            let account = crate::accounts::Account {
                id: "handoff".into(),
                label: "fixture".into(),
                registration: AppRegistration::Microsoft {
                    client_id: "00000000-0000-4000-8000-000000000001".into(),
                    authority: "common".into(),
                },
                identity: Identity {
                    tenant_id: "00000000-0000-4000-8000-000000000002".into(),
                    subject: "fixture".into(),
                    username: "fixture@example.invalid".into(),
                    graph_user_id: "fixture".into(),
                    display_name: "fixture".into(),
                },
                credential_id: "fixture".into(),
                access: AccessMode::ReadWrite,
                drive: cirrove_onedrive::DriveInfo {
                    id: "drive".into(),
                    name: "fixture".into(),
                    drive_type: "business".into(),
                    web_url: "https://example.invalid".into(),
                },
                root_id: "root".into(),
                mount_path: temp.path().join("mount"),
                enabled: false,
                poll_seconds: 3600,
                cache_bytes: 8 * 1024 * 1024,
            };
            let node = Node {
                package: false,
                id: "remote".into(),
                parent_id: Some("root".into()),
                name: "file.txt".into(),
                kind: NodeKind::File,
                size: 3,
                modified_unix: 1,
                etag: Some("original".into()),
                content_version: Some("original".into()),
                target: None,
            };
            let provider = Arc::new(Provider {
                node: node.clone(),
                hold: AtomicBool::new(hold),
                fail: AtomicBool::new(false),
                calls: AtomicUsize::new(0),
                entered: tokio::sync::Notify::new(),
                release: tokio::sync::Notify::new(),
            });
            let engine = Engine::new(account, provider.clone(), temp.path().join("state"))
                .await
                .unwrap();
            let mut journal =
                UploadJournal::open(&temp.path().join("journal"), "handoff", 4096).unwrap();
            let working = journal
                .create_working(engine.scope("drive"), node, false, b"old".as_slice())
                .unwrap();
            let journal = Arc::new(Mutex::new(journal));
            let writer = Writeback::new(&engine, journal.clone()).await.unwrap();
            Self {
                _temp: temp,
                engine,
                writer,
                journal,
                provider,
                working,
            }
        }
        fn start(&self) -> tokio::task::JoinHandle<Result<bool>> {
            let writer = self.writer.clone();
            let engine = self.engine.clone();
            tokio::spawn(async move { writer.maintain(&engine).await })
        }
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_new_access_and_save_during_handoff_io_stays_responsive_and_invalidates_detach() {
        for hold_open in [false, true] {
            let f = Fixture::new(true).await;
            let task = f.start();
            tokio::time::timeout(Duration::from_secs(2), f.provider.entered.notified())
                .await
                .unwrap();
            let lease = tokio::time::timeout(
                Duration::from_millis(500),
                f.writer
                    .lease(&f.working.scope, &f.working.node.id, &f.engine.cancel),
            )
            .await
            .unwrap()
            .unwrap();
            f.writer
                .write(f.working.id, 0, b"newer".to_vec(), false)
                .await
                .unwrap();
            let retained = hold_open.then(|| lease.clone());
            drop(lease);
            f.provider.release.notify_one();
            tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let j = f.journal.lock().unwrap();
            assert_eq!(j.read_working(f.working.id, 0, 20).unwrap(), b"newer");
            assert!(j.working_file(f.working.id).unwrap().dirty);
            assert!(
                !j.namespace_by_local(&f.working.scope, &f.working.node.id)
                    .unwrap()
                    .unwrap()
                    .follows_remote
            );
            drop(j);
            drop(retained);
        }
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancellation_abandons_stalled_metadata_without_detaching_clean_bytes() {
        let f = Fixture::new(true).await;
        let task = f.start();
        tokio::time::timeout(Duration::from_secs(2), f.provider.entered.notified())
            .await
            .unwrap();
        f.engine.cancel.cancel();
        assert!(
            !tokio::time::timeout(Duration::from_millis(500), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap()
        );
        assert_eq!(
            f.journal
                .lock()
                .unwrap()
                .read_working(f.working.id, 0, 20)
                .unwrap(),
            b"old"
        );
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn idle_passes_do_not_reset_a_failed_objects_provider_backoff() {
        let f = Fixture::new(false).await;
        f.provider.fail.store(true, Ordering::SeqCst);
        assert!(f.writer.maintain(&f.engine).await.is_err());
        assert!(!f.writer.maintain(&f.engine).await.unwrap());
        assert!(!f.writer.maintain(&f.engine).await.unwrap());
        assert_eq!(f.provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(f.writer.read(f.working.id, 0, 20).await.unwrap(), b"old");
        f.provider.fail.store(false, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(2100)).await;
        assert!(!f.writer.maintain(&f.engine).await.unwrap());
        assert!(f.writer.maintain(&f.engine).await.unwrap());
        assert_eq!(f.provider.calls.load(Ordering::SeqCst), 2);
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failed_durable_handoff_keeps_the_old_projection_and_retries_from_the_journal() {
        let f = Fixture::new(false).await;
        let old = f
            .journal
            .lock()
            .unwrap()
            .namespace_by_local(&f.working.scope, &f.working.node.id)
            .unwrap()
            .unwrap();
        let db = rusqlite::Connection::open(f._temp.path().join("journal/uploads.db")).unwrap();
        db.execute_batch("CREATE TRIGGER deny_handoff BEFORE UPDATE ON namespace_objects BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
        assert!(f.writer.maintain(&f.engine).await.is_err());
        assert!(
            f.writer
                .working(&f.working.scope, &f.working.node.id)
                .unwrap()
                .is_some()
        );
        assert_eq!(f.writer.read(f.working.id, 0, 20).await.unwrap(), b"old");
        db.execute_batch("DROP TRIGGER deny_handoff;").unwrap();
        // The first call wraps the bounded cursor; the second retries this object.
        assert!(!f.writer.maintain(&f.engine).await.unwrap());
        assert!(f.writer.maintain(&f.engine).await.unwrap());
        assert!(
            f.writer
                .working(&f.working.scope, &f.working.node.id)
                .unwrap()
                .is_none()
        );
        // A delayed save callback cannot resurrect the retired working UUID.
        f.writer
            .projection
            .lock()
            .unwrap()
            .merge(old, Some(f.working.clone()))
            .unwrap();
        assert!(
            f.writer
                .working(&f.working.scope, &f.working.node.id)
                .unwrap()
                .is_none()
        );
        f.writer.maintain(&f.engine).await.unwrap();
        assert_eq!(f.journal.lock().unwrap().retained_bytes().unwrap(), 0);
    }

    #[tokio::test]
    async fn folder_handoff_refreshes_a_cached_creation_receipt_before_removal() {
        let f = Fixture::new(false).await;
        f.writer.maintain(&f.engine).await.unwrap();
        let scope = f.engine.scope("drive");
        let receipt = Node {
            id: "new-folder".into(),
            name: "made-and-unmade".into(),
            kind: NodeKind::Folder,
            size: 0,
            content_version: None,
            etag: Some("create-receipt".into()),
            ..f.provider.node.clone()
        };
        let settled = Node {
            etag: Some("settled-folder".into()),
            ..receipt.clone()
        };
        let provider = Arc::new(Provider {
            node: settled.clone(),
            hold: AtomicBool::new(false),
            fail: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let engine = Engine::new(
            f.engine.account.clone(),
            provider.clone(),
            f._temp.path().join("folder-state"),
        )
        .await
        .unwrap();
        let object = {
            let mut journal = f.journal.lock().unwrap();
            let object = journal
                .create_namespace_directory(scope.clone(), "root".into(), receipt.name.clone())
                .unwrap();
            let operation = journal.claim_mutation().unwrap().unwrap();
            journal
                .acknowledge_mutation(
                    operation.id,
                    operation.attempt.unwrap(),
                    cirrove_core::mutation::MutationReceipt::Upsert(receipt.clone()),
                )
                .unwrap();
            object
        };
        // A receipt may reach the index before maintenance runs. Equality with
        // that cache entry is not an independent observation of a settled ETag.
        Store::open(engine.db.clone())
            .unwrap()
            .observe_node(&scope, &receipt)
            .unwrap();
        let writer = Writeback::new(&engine, f.journal.clone()).await.unwrap();
        assert!(writer.maintain(&engine).await.unwrap());
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        let mut journal = f.journal.lock().unwrap();
        let handed = journal.namespace_object(object.id).unwrap();
        assert!(handed.follows_remote && handed.latest.is_none());
        assert_eq!(handed.remote.as_ref(), Some(&settled));
        let current = Store::open(engine.db.clone())
            .unwrap()
            .node(&scope, &receipt.id)
            .unwrap()
            .unwrap();
        let materialized = journal.observe_namespace_file(scope, current).unwrap();
        let removed = journal
            .remove_namespace_directory(materialized.id, materialized.revision)
            .unwrap();
        assert_eq!(
            removed.mutation.request.intent.before().unwrap().etag,
            settled.etag
        );
    }

    struct SettledFolderFixture {
        _base: Fixture,
        fs: crate::filesystem::CloudFs,
        writer: Arc<Writeback>,
        stale: View,
        object: Uuid,
        settled: Node,
        provider: Arc<Provider>,
    }
    impl SettledFolderFixture {
        async fn new() -> Self {
            let f = Fixture::new(false).await;
            f.writer.maintain(&f.engine).await.unwrap();
            let scope = f.engine.scope("drive");
            let receipt = Node {
                id: "new-folder".into(),
                name: "made-and-unmade".into(),
                kind: NodeKind::Folder,
                size: 0,
                content_version: None,
                etag: Some("create-receipt".into()),
                ..f.provider.node.clone()
            };
            let settled = Node {
                etag: Some("settled-folder".into()),
                ..receipt.clone()
            };
            let provider = Arc::new(Provider {
                node: settled.clone(),
                hold: AtomicBool::new(false),
                fail: AtomicBool::new(false),
                calls: AtomicUsize::new(0),
                entered: tokio::sync::Notify::new(),
                release: tokio::sync::Notify::new(),
            });
            let engine = Engine::new(
                f.engine.account.clone(),
                provider.clone(),
                f._temp.path().join("stale-folder-state"),
            )
            .await
            .unwrap();
            let (object, stale_node) = {
                let mut journal = f.journal.lock().unwrap();
                let object = journal
                    .create_namespace_directory(scope.clone(), "root".into(), receipt.name.clone())
                    .unwrap();
                let operation = journal.claim_mutation().unwrap().unwrap();
                journal
                    .acknowledge_mutation(
                        operation.id,
                        operation.attempt.unwrap(),
                        cirrove_core::mutation::MutationReceipt::Upsert(receipt.clone()),
                    )
                    .unwrap();
                let confirmed = journal.namespace_object(object.id).unwrap();
                assert_eq!(confirmed.node.etag, receipt.etag);
                assert!(confirmed.latest.is_some());
                (object.id, confirmed.node)
            };
            Store::open(engine.db.clone())
                .unwrap()
                .observe_node(&scope, &receipt)
                .unwrap();
            let fs = crate::filesystem::CloudFs::new_experimental_writable(
                engine.clone(),
                f.journal.clone(),
            )
            .await
            .unwrap();
            let writer = fs.inner.writeback.as_ref().unwrap().clone();
            let root = fs.inner.view(1).unwrap();
            // Deterministically capture the rmdir view before maintenance finishes.
            let stale = fs.inner.insert(&root, stale_node).await.unwrap();
            assert_eq!(stale.node.as_ref().unwrap().etag, receipt.etag);
            assert!(writer.maintain(&engine).await.unwrap());
            assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
            {
                let journal = f.journal.lock().unwrap();
                let handed = journal.namespace_object(object).unwrap();
                assert!(handed.follows_remote && handed.latest.is_none());
                assert_eq!(handed.remote.as_ref(), Some(&settled));
            }
            Self {
                _base: f,
                fs,
                writer,
                stale,
                object,
                settled,
                provider,
            }
        }
    }
    struct ConditionalFolderRemoval {
        settled: Node,
        attempts: Mutex<Vec<Node>>,
        removed: AtomicBool,
    }
    #[async_trait]
    impl cirrove_core::mutation::MutationProvider for ConditionalFolderRemoval {
        async fn mutate(
            &self,
            request: &cirrove_core::mutation::MutationRequest,
            _: &cirrove_core::CancellationToken,
        ) -> cirrove_core::mutation::Result<cirrove_core::mutation::MutationReceipt> {
            request.validate()?;
            let cirrove_core::mutation::MutationIntent::RemoveFolder { before } = &request.intent
            else {
                return Err(cirrove_core::mutation::MutationError::Unsupported(
                    "only synthetic empty-folder removal",
                ));
            };
            self.attempts.lock().unwrap().push(before.clone());
            if before != &self.settled {
                return Err(cirrove_core::mutation::MutationError::Conflict);
            }
            assert!(
                !self.removed.swap(true, Ordering::SeqCst),
                "folder deletion replayed"
            );
            Ok(cirrove_core::mutation::MutationReceipt::Removed {
                item: before.id.clone(),
            })
        }
        async fn reconcile_mutation(
            &self,
            _: &cirrove_core::mutation::MutationRequest,
            _: &cirrove_core::CancellationToken,
        ) -> cirrove_core::mutation::Result<cirrove_core::mutation::MutationReconciliation>
        {
            Ok(cirrove_core::mutation::MutationReconciliation::Indeterminate)
        }
    }
    #[tokio::test]
    async fn stale_created_folder_view_cannot_restore_creation_etag_after_settled_handoff() {
        let f = SettledFolderFixture::new().await;
        assert_eq!(
            f.stale.node.as_ref().unwrap().etag.as_deref(),
            Some("create-receipt")
        );
        f.writer.rmdir(&f.fs.inner, f.stale.clone()).await.unwrap();
        let provider = Arc::new(ConditionalFolderRemoval {
            settled: f.settled.clone(),
            attempts: Mutex::new(Vec::new()),
            removed: AtomicBool::new(false),
        });
        let worker = crate::mutations::MutationWorker::new(
            f._base.journal.clone(),
            provider.clone(),
            cirrove_core::CancellationToken::new(),
        );
        let result = tokio::time::timeout(Duration::from_secs(5), worker.run_once())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let attempts = provider.attempts.lock().unwrap().clone();
        assert_eq!(attempts.len(), 1);
        let mut shape = attempts[0].clone();
        shape.etag = f.settled.etag.clone();
        assert_eq!(shape, f.settled);
        assert_eq!(
            result.state,
            crate::journal::MutationState::Applied,
            "conditional deletion from a pre-handoff view must use independently observed settled ETag; attempted fixture ETag: {:?}",
            attempts[0].etag
        );
        assert!(result.issue.is_none());
        assert!(provider.removed.load(Ordering::SeqCst));
        assert_eq!(
            provider.attempts.lock().unwrap().as_slice(),
            std::slice::from_ref(&f.settled)
        );
        let journal = f._base.journal.lock().unwrap();
        let rows = journal.list_mutations(0, 10).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter()
                .all(|r| r.state == crate::journal::MutationState::Applied)
        );
        assert!(rows[1].base.is_none());
        assert_eq!(rows[1].request.intent.before(), Some(&f.settled));
    }
    #[tokio::test]
    async fn settled_folder_readoption_refuses_changed_path_kind_shape_and_invalid_identity() {
        for arm in 0..7 {
            let f = SettledFolderFixture::new().await;
            let mut selected = f.stale.node.as_ref().unwrap().as_ref().clone();
            match arm {
                0 => selected.name = "a-different-folder".into(),
                1 => selected.parent_id = Some("different-parent".into()),
                2 => selected.kind = NodeKind::File,
                3 => selected.package = true,
                4 => selected.size = 123,
                5 => selected.id.clear(),
                _ => {
                    selected.target = Some(Box::new(cirrove_core::RemoteRef {
                        collection: "drive".into(),
                        item: "other-folder".into(),
                        kind: Some(NodeKind::Folder),
                    }))
                }
            }
            let before = {
                let journal = f._base.journal.lock().unwrap();
                serde_json::to_value(journal.namespace_object(f.object).unwrap()).unwrap()
            };
            let mut view = f.stale.clone();
            view.remember(&selected, true);
            assert!(
                f.writer.rmdir(&f.fs.inner, view).await.is_err(),
                "changed selected folder path/shape was silently adopted, arm {arm}"
            );
            let journal = f._base.journal.lock().unwrap();
            assert_eq!(
                serde_json::to_value(journal.namespace_object(f.object).unwrap()).unwrap(),
                before
            );
            assert_eq!(journal.list_mutations(0, 10).unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn fresh_scoped_cached_folder_alias_keeps_its_observed_revision_for_rmdir() {
        let f = SettledFolderFixture::new().await;
        let scope = f.stale.scope.as_ref().clone();
        let fresh = Node {
            etag: Some("fresh-observed-E5".into()),
            content_version: Some("fresh-observed-C5".into()),
            modified_unix: 5,
            ..f.settled.clone()
        };
        Store::open(f.fs.inner.engine.db.clone())
            .unwrap()
            .observe_node(&scope, &fresh)
            .unwrap();
        let projected = f
            .writer
            .overlay(&scope, "root", vec![fresh.clone()])
            .unwrap();
        let selected = projected
            .into_iter()
            .find(|n| n.name == fresh.name)
            .unwrap();
        assert_eq!(selected.id, f.stale.id.as_ref());
        assert_ne!(selected.id, fresh.id);
        assert_eq!(selected.etag, fresh.etag);
        let root = f.fs.inner.view(1).unwrap();
        let view = f.fs.inner.insert(&root, selected).await.unwrap();
        assert_eq!(view.node.as_ref().unwrap().etag, fresh.etag);
        assert_eq!(
            f._base
                .journal
                .lock()
                .unwrap()
                .namespace_object(f.object)
                .unwrap()
                .remote
                .as_ref(),
            Some(&f.settled)
        );
        f.writer.rmdir(&f.fs.inner, view).await.unwrap();
        let provider = Arc::new(ConditionalFolderRemoval {
            settled: fresh.clone(),
            attempts: Mutex::new(Vec::new()),
            removed: AtomicBool::new(false),
        });
        let worker = crate::mutations::MutationWorker::new(
            f._base.journal.clone(),
            provider.clone(),
            cirrove_core::CancellationToken::new(),
        );
        let result = tokio::time::timeout(Duration::from_secs(5), worker.run_once())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let attempts = provider.attempts.lock().unwrap().clone();
        assert_eq!(attempts.len(), 1);
        assert_eq!(
            result.state,
            crate::journal::MutationState::Applied,
            "fresh projected local alias must condition on observed E5; attempted fixture ETag: {:?}",
            attempts[0].etag
        );
        assert!(result.issue.is_none());
        assert!(provider.removed.load(Ordering::SeqCst));
        assert_eq!(attempts.as_slice(), std::slice::from_ref(&fresh));
        let journal = f._base.journal.lock().unwrap();
        let rows = journal.list_mutations(0, 10).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter()
                .all(|r| r.state == crate::journal::MutationState::Applied)
        );
        assert!(rows[1].base.is_none());
        assert_eq!(rows[1].request.intent.before(), Some(&fresh));
    }

    #[tokio::test]
    async fn fresh_scoped_cached_renamed_folder_alias_remains_removable() {
        let f = SettledFolderFixture::new().await;
        let scope = f.stale.scope.as_ref().clone();
        let fresh = Node {
            name: "externally-renamed-folder".into(),
            etag: Some("fresh-observed-E5".into()),
            content_version: Some("fresh-observed-C5".into()),
            modified_unix: 5,
            ..f.settled.clone()
        };
        Store::open(f.fs.inner.engine.db.clone())
            .unwrap()
            .observe_node(&scope, &fresh)
            .unwrap();
        let projected = f
            .writer
            .overlay(&scope, "root", vec![fresh.clone()])
            .unwrap();
        let selected = projected
            .into_iter()
            .find(|n| n.name == fresh.name)
            .unwrap();
        assert_eq!(selected.id, f.stale.id.as_ref());
        assert_ne!(selected.id, fresh.id);
        assert_eq!(selected.etag, fresh.etag);
        let root = f.fs.inner.view(1).unwrap();
        let view = f.fs.inner.insert(&root, selected).await.unwrap();
        assert_eq!(view.node.as_ref().unwrap().etag, fresh.etag);
        assert_eq!(
            f._base
                .journal
                .lock()
                .unwrap()
                .namespace_object(f.object)
                .unwrap()
                .remote
                .as_ref(),
            Some(&f.settled)
        );
        f.writer.rmdir(&f.fs.inner, view).await.unwrap();
        let provider = Arc::new(ConditionalFolderRemoval {
            settled: fresh.clone(),
            attempts: Mutex::new(Vec::new()),
            removed: AtomicBool::new(false),
        });
        let worker = crate::mutations::MutationWorker::new(
            f._base.journal.clone(),
            provider.clone(),
            cirrove_core::CancellationToken::new(),
        );
        let result = tokio::time::timeout(Duration::from_secs(5), worker.run_once())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let attempts = provider.attempts.lock().unwrap().clone();
        assert_eq!(attempts.len(), 1);
        assert_eq!(
            result.state,
            crate::journal::MutationState::Applied,
            "fresh projected local alias must condition on observed E5; attempted fixture ETag: {:?}",
            attempts[0].etag
        );
        assert!(result.issue.is_none());
        assert!(provider.removed.load(Ordering::SeqCst));
        assert_eq!(attempts.as_slice(), std::slice::from_ref(&fresh));
        let journal = f._base.journal.lock().unwrap();
        let rows = journal.list_mutations(0, 10).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter()
                .all(|r| r.state == crate::journal::MutationState::Applied)
        );
        assert!(rows[1].base.is_none());
        assert_eq!(rows[1].request.intent.before(), Some(&fresh));
    }

    #[tokio::test]
    async fn scoped_folder_source_refuses_absent_wrong_kind_cancelled_cache_and_snapshot_drift() {
        for arm in 0..4 {
            let f = SettledFolderFixture::new().await;
            let scope = f.stale.scope.as_ref().clone();
            let calls = f.provider.calls.load(Ordering::SeqCst);
            let before = {
                let j = f._base.journal.lock().unwrap();
                serde_json::to_value(j.namespace_object(f.object).unwrap()).unwrap()
            };
            match arm {
                0 => {
                    let mut store = Store::open(f.fs.inner.engine.db.clone()).unwrap();
                    let ticket = store.node_observation(&scope, &f.settled.id).unwrap();
                    store.publish_absence(&ticket).unwrap();
                    assert!(store.node(&scope, &f.settled.id).unwrap().is_none());
                    drop(store);
                    assert!(f.writer.rmdir(&f.fs.inner, f.stale.clone()).await.is_err());
                }
                1 => {
                    let wrong = Node {
                        kind: NodeKind::File,
                        ..f.settled.clone()
                    };
                    Store::open(f.fs.inner.engine.db.clone())
                        .unwrap()
                        .observe_node(&scope, &wrong)
                        .unwrap();
                    assert!(f.writer.rmdir(&f.fs.inner, f.stale.clone()).await.is_err());
                }
                2 => {
                    let cancel = CancellationToken::new();
                    cancel.cancel();
                    assert!(
                        f.writer
                            .observed_folder_source(
                                &f.fs.inner.engine,
                                &scope,
                                f.stale.node.as_ref().unwrap(),
                                &cancel
                            )
                            .await
                            .is_err()
                    );
                }
                _ => {
                    let (_, snapshot) = f
                        .writer
                        .observed_folder_source(
                            &f.fs.inner.engine,
                            &scope,
                            f.stale.node.as_ref().unwrap(),
                            &CancellationToken::new(),
                        )
                        .await
                        .unwrap();
                    let mut j = f._base.journal.lock().unwrap();
                    let concurrent = Node {
                        etag: Some("concurrent-E9".into()),
                        ..f.settled.clone()
                    };
                    j.observe_namespace_file(scope.clone(), concurrent).unwrap();
                    let changed =
                        serde_json::to_value(j.namespace_object(f.object).unwrap()).unwrap();
                    assert!(Writeback::recheck_folder_source(&j, snapshot.as_ref()).is_err());
                    assert_eq!(
                        serde_json::to_value(j.namespace_object(f.object).unwrap()).unwrap(),
                        changed
                    );
                }
            }
            assert_eq!(
                f.provider.calls.load(Ordering::SeqCst),
                calls,
                "source preparation must never fall back to provider IO, arm {arm}"
            );
            let j = f._base.journal.lock().unwrap();
            assert_eq!(j.list_mutations(0, 10).unwrap().len(), 1);
            if arm != 3 {
                assert_eq!(
                    serde_json::to_value(j.namespace_object(f.object).unwrap()).unwrap(),
                    before
                );
            }
        }
    }
    #[tokio::test]
    async fn externally_moved_folder_refuses_old_route_and_accepts_current_projected_route() {
        let f = SettledFolderFixture::new().await;
        let scope = f.stale.scope.as_ref().clone();
        let moved = Node {
            parent_id: Some("other-parent".into()),
            etag: Some("fresh-moved-E5".into()),
            ..f.settled.clone()
        };
        let parent = Node {
            id: "other-parent".into(),
            parent_id: Some("root".into()),
            name: "other-parent".into(),
            etag: Some("parent-E5".into()),
            ..f.settled.clone()
        };
        {
            let mut store = Store::open(f.fs.inner.engine.db.clone()).unwrap();
            store.observe_node(&scope, &parent).unwrap();
            store.observe_node(&scope, &moved).unwrap();
        }
        let parent_object = {
            let mut j = f._base.journal.lock().unwrap();
            let object = j
                .create_namespace_directory(scope.clone(), "root".into(), parent.name.clone())
                .unwrap();
            let operation = j.claim_mutation().unwrap().unwrap();
            j.acknowledge_mutation(
                operation.id,
                operation.attempt.unwrap(),
                cirrove_core::mutation::MutationReceipt::Upsert(parent.clone()),
            )
            .unwrap();
            let current = j.namespace_object(object.id).unwrap();
            assert_ne!(current.node.id, parent.id);
            assert_eq!(current.remote.as_ref(), Some(&parent));
            current
        };
        f.writer.refresh_projection().await.unwrap();
        let before = serde_json::to_value(
            f._base
                .journal
                .lock()
                .unwrap()
                .namespace_object(f.object)
                .unwrap(),
        )
        .unwrap();
        assert!(
            f.writer.rmdir(&f.fs.inner, f.stale.clone()).await.is_err(),
            "old root route must not remove a folder moved into another parent"
        );
        assert_eq!(
            serde_json::to_value(
                f._base
                    .journal
                    .lock()
                    .unwrap()
                    .namespace_object(f.object)
                    .unwrap()
            )
            .unwrap(),
            before
        );
        assert_eq!(
            f._base
                .journal
                .lock()
                .unwrap()
                .list_mutations(0, 10)
                .unwrap()
                .len(),
            2
        );
        let projected = f
            .writer
            .overlay(&scope, &parent_object.node.id, vec![moved.clone()])
            .unwrap();
        let selected = projected
            .into_iter()
            .find(|n| n.id == f.stale.id.as_ref())
            .unwrap();
        assert_eq!(
            selected.parent_id.as_deref(),
            Some(parent_object.node.id.as_str())
        );
        assert_eq!(selected.etag, moved.etag);
        let root = f.fs.inner.view(1).unwrap();
        let parent_view =
            f.fs.inner
                .insert(&root, parent_object.node.clone())
                .await
                .unwrap();
        let view = f.fs.inner.insert(&parent_view, selected).await.unwrap();
        f.writer.rmdir(&f.fs.inner, view).await.unwrap();
        let provider = Arc::new(ConditionalFolderRemoval {
            settled: moved.clone(),
            attempts: Mutex::new(Vec::new()),
            removed: AtomicBool::new(false),
        });
        let worker = crate::mutations::MutationWorker::new(
            f._base.journal.clone(),
            provider.clone(),
            CancellationToken::new(),
        );
        let result = tokio::time::timeout(Duration::from_secs(5), worker.run_once())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(result.state, crate::journal::MutationState::Applied);
        assert!(result.issue.is_none());
        assert!(provider.removed.load(Ordering::SeqCst));
        assert_eq!(
            provider.attempts.lock().unwrap().as_slice(),
            std::slice::from_ref(&moved)
        );
        let j = f._base.journal.lock().unwrap();
        let rows = j.list_mutations(0, 10).unwrap();
        assert_eq!(rows.len(), 3);
        assert!(
            rows.iter()
                .all(|r| r.state == crate::journal::MutationState::Applied)
        );
        assert_eq!(rows[2].request.intent.before(), Some(&moved));
        assert!(rows[2].base.is_none());
    }

    /// An explicit write grant is the opt-in, and it is the only one.
    ///
    /// The guard also required `!enabled` until that clause was removed. The two
    /// together were unsatisfiable in ordinary use, because the daemon does not
    /// mount a disabled account, so writable mounts were unreachable rather than
    /// deliberate. Restoring the clause makes the `(ReadWrite, enabled)` row fail.
    #[tokio::test]
    async fn writes_need_an_explicit_grant_and_nothing_else() {
        for (access, enabled, allowed) in [
            (AccessMode::ReadWrite, false, true),
            (AccessMode::ReadWrite, true, true),
            (AccessMode::ReadOnly, false, false),
            (AccessMode::ReadOnly, true, false),
        ] {
            let f = Fixture::new(false).await;
            let mut account = f.engine.account.clone();
            account.access = access;
            account.enabled = enabled;
            let temp = tempfile::tempdir().unwrap();
            let engine = Engine::new(account, f.provider.clone(), temp.path().join("state"))
                .await
                .unwrap();
            let journal = Arc::new(Mutex::new(
                UploadJournal::open(&temp.path().join("journal"), "handoff", 4096).unwrap(),
            ));
            let result = Writeback::new(&engine, journal).await;
            assert_eq!(
                result.is_ok(),
                allowed,
                "access {access:?} enabled {enabled}"
            );
            if let Err(error) = result {
                assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
            }
        }
    }
}
