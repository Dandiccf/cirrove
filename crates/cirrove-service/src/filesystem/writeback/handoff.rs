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
    async fn publish_ordinary_metadata(&self, engine: &Engine) -> Result<bool> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let proof = self
            .local(move |journal| journal.ordinary_metadata_due(now))
            .await?;
        let Some(proof) = proof else { return Ok(false) };
        let result = engine.publish_ordinary_metadata(&proof).await;
        let completed = result.as_ref().is_ok_and(|value| *value);
        let completed_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.local(move |journal| {
            journal.finish_ordinary_metadata(&proof, completed, completed_at)
        })
        .await?;
        result.map_err(|error| errno(&error))?;
        Ok(true)
    }
    /// Publish one due metadata job, then inspect at most 16 cleanup candidates.
    /// Publication and retirement may each make one bounded exact-ID read.
    /// The cursor makes pending/open objects yield to others; missed publication
    /// callbacks are repaired without replaying the provider operation.
    pub async fn maintain(self: &Arc<Self>, engine: &Engine) -> Result<bool> {
        let publication_worked = self.publish_ordinary_metadata(engine).await?;
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
            return Ok(publication_worked);
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
        Ok(publication_worked)
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

    // This existing-API regression fails on the old sole-visible-identity
    // assertion after successful handoff, without relying on provider reads.
    #[tokio::test]
    async fn confirmed_ordinary_handoff_retires_only_after_old_metadata_moves_to_backup() {
        let Fixture {
            _temp,
            engine,
            writer,
            journal,
            provider,
            working,
        } = Fixture::new(false).await;
        let root = _temp.keep(); // Retain evidence; no recursive fixture cleanup.
        eprintln!(
            "retained ordinary handoff metadata fixture: {}",
            root.display()
        );
        let scope = engine.scope("drive");
        let original = provider.node.clone();
        let bytes = b"new-body";
        let current = Node {
            id: "zz-new-current".into(),
            size: bytes.len() as u64,
            etag: Some("current-B".into()),
            content_version: Some("current-B".into()),
            ..original.clone()
        };
        let backup = Node {
            parent_id: Some("trash".into()),
            etag: Some("backup-A".into()),
            content_version: Some("backup-A".into()),
            ..original.clone()
        };
        let id = {
            let mut j = journal.lock().unwrap();
            j.write_working(working.id, 0, bytes).unwrap();
            let id = j.seal_working(working.id).unwrap().unwrap().id;
            let row = j.claim_next().unwrap().unwrap();
            assert_eq!(row.id, id);
            j.reserve_identity_handoff(
                id,
                row.attempt.unwrap(),
                cirrove_core::upload::RecoveryLocation::Trash {
                    local_name: "recovery-A".into(),
                    parent: "trash".into(),
                },
            )
            .unwrap();
            j.acknowledge_identity_handoff(
                id,
                row.attempt.unwrap(),
                current.clone(),
                backup.clone(),
            )
            .unwrap();
            let row = j.get(id).unwrap();
            assert_eq!(row.state, crate::journal::UploadState::Uploaded);
            assert_eq!(row.ordinary_handoff_receipt(), Some((&current, &backup)));
            id
        };
        let mut store = Store::open(&engine.db).unwrap();
        store
            .observe_directory(&scope, "root", &[original.clone(), current.clone()])
            .unwrap();
        store.observe_node(&scope, &current).unwrap();
        assert_eq!(store.children(&scope, "root").unwrap().unwrap().len(), 2);
        let before_cursor = store.cursor(&scope).unwrap();
        let delayed_old = store.node_observation(&scope, &original.id).unwrap();
        drop(store);
        assert!(writer.maintain(&engine).await.unwrap());
        {
            let j = journal.lock().unwrap();
            let row = j.get(id).unwrap();
            assert_eq!(row.ordinary_handoff_receipt(), Some((&current, &backup)));
            let owner = j.namespace_for_operation(id).unwrap().unwrap();
            assert!(owner.follows_remote && owner.latest.is_none() && owner.working_file.is_none());
            assert_eq!(owner.remote.as_ref(), Some(&current));
        }
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            0,
            "exact cached B and receipt publication must need no provider replay/read"
        );
        let mut store = Store::open(&engine.db).unwrap();
        assert_eq!(store.cursor(&scope).unwrap(), before_cursor);
        let listed = store.children(&scope, "root").unwrap().unwrap();
        assert_eq!(
            listed,
            vec![current.clone()],
            "confirmed HandoffComplete must not leave old A at the original name after retirement"
        );
        assert_eq!(
            store.node(&scope, &original.id).unwrap(),
            Some(backup.clone())
        );
        assert!(
            matches!(
                store.publish_node(&delayed_old, &original).unwrap(),
                cirrove_store::ObservationResult::Superseded(_)
            ),
            "pre-publication exact-ID observation cannot resurrect old A"
        );
        assert_eq!(
            store.children(&scope, "root").unwrap().unwrap(),
            vec![current]
        );
    }
    // Follow-on history and read-only controls supplement the existing-API
    // regression; their new consumers make no original-baseline RED claim.
    struct OrdinaryMetadataCase {
        root: std::path::PathBuf,
        journal_root: std::path::PathBuf,
        engine: Arc<Engine>,
        writer: Arc<Writeback>,
        journal: Arc<Mutex<UploadJournal>>,
        provider: Arc<Provider>,
        working: Uuid,
        id: Uuid,
        scope: Scope,
        original: Node,
        current: Node,
        backup: Node,
    }
    impl OrdinaryMetadataCase {
        async fn new(account_journal: bool) -> Self {
            let Fixture {
                _temp,
                engine,
                writer: donor_writer,
                journal: donor_journal,
                provider,
                working: donor_working,
            } = Fixture::new(false).await;
            let root = _temp.keep();
            eprintln!("retained ordinary publication fixture: {}", root.display());
            let original = provider.node.clone();
            let scope = engine.scope("drive");
            let journal_root = if account_journal {
                engine.db.parent().unwrap().join("journal")
            } else {
                root.join("journal")
            };
            let (writer, journal, working) = if account_journal {
                drop(donor_writer);
                drop(donor_journal);
                let mut j = UploadJournal::open(&journal_root, "handoff", 4096).unwrap();
                let w = j
                    .create_working(scope.clone(), original.clone(), false, b"old".as_slice())
                    .unwrap();
                let j = Arc::new(Mutex::new(j));
                (Writeback::new(&engine, j.clone()).await.unwrap(), j, w.id)
            } else {
                (donor_writer, donor_journal, donor_working.id)
            };
            let bytes = b"new-body";
            let current = Node {
                id: "zz-new-current".into(),
                size: bytes.len() as u64,
                etag: Some("current-B".into()),
                content_version: Some("current-B".into()),
                ..original.clone()
            };
            let backup = Node {
                parent_id: Some("trash".into()),
                etag: Some("backup-A".into()),
                content_version: Some("backup-A".into()),
                ..original.clone()
            };
            let id = {
                let mut j = journal.lock().unwrap();
                j.write_working(working, 0, bytes).unwrap();
                let id = j.seal_working(working).unwrap().unwrap().id;
                let row = j.claim_next().unwrap().unwrap();
                assert_eq!(row.id, id);
                j.reserve_identity_handoff(
                    id,
                    row.attempt.unwrap(),
                    cirrove_core::upload::RecoveryLocation::Trash {
                        local_name: "recovery-A".into(),
                        parent: "trash".into(),
                    },
                )
                .unwrap();
                j.acknowledge_identity_handoff(
                    id,
                    row.attempt.unwrap(),
                    current.clone(),
                    backup.clone(),
                )
                .unwrap();
                assert_eq!(
                    j.get(id).unwrap().ordinary_handoff_receipt(),
                    Some((&current, &backup))
                );
                id
            };
            let mut store = Store::open(&engine.db).unwrap();
            store
                .observe_directory(&scope, "root", &[original.clone(), current.clone()])
                .unwrap();
            store.observe_node(&scope, &current).unwrap();
            drop(store);
            Self {
                root,
                journal_root,
                engine,
                writer,
                journal,
                provider,
                working,
                id,
                scope,
                original,
                current,
                backup,
            }
        }
    }
    #[tokio::test]
    async fn ordinary_handoff_history_survives_next_sealed_save_and_newer_dirty_bytes() {
        let f = OrdinaryMetadataCase::new(false).await;
        let cbytes = b"third-revision";
        let dirty = b"later-dirty";
        let c = Node {
            id: "zz-third-current".into(),
            size: cbytes.len() as u64,
            etag: Some("current-C".into()),
            content_version: Some("current-C".into()),
            ..f.current.clone()
        };
        let b_backup = Node {
            parent_id: Some("trash".into()),
            etag: Some("backup-B".into()),
            content_version: Some("backup-B".into()),
            ..f.current.clone()
        };
        let next = {
            let mut j = f.journal.lock().unwrap();
            j.write_working(f.working, 0, cbytes).unwrap();
            j.truncate_working(f.working, cbytes.len() as u64).unwrap();
            j.seal_working(f.working).unwrap().unwrap().id
        };
        {
            let mut j = f.journal.lock().unwrap();
            j.write_working(f.working, 0, dirty).unwrap();
            j.truncate_working(f.working, dirty.len() as u64).unwrap();
        }
        let retained = f.journal.lock().unwrap().working_file(f.working).unwrap();
        assert!(retained.dirty && retained.latest == Some(next));
        // B's historical A->Trash job must publish despite latest C and dirty D.
        assert!(f.writer.publish_ordinary_metadata(&f.engine).await.unwrap());
        assert_eq!(
            Store::open(&f.engine.db)
                .unwrap()
                .node(&f.scope, &f.original.id)
                .unwrap(),
            Some(f.backup.clone())
        );
        {
            let mut j = f.journal.lock().unwrap();
            assert_eq!(
                serde_json::to_value(j.working_file(f.working).unwrap()).unwrap(),
                serde_json::to_value(&retained).unwrap()
            );
            let row = j.claim_next().unwrap().unwrap();
            assert_eq!(row.id, next);
            j.reserve_identity_handoff(
                next,
                row.attempt.unwrap(),
                cirrove_core::upload::RecoveryLocation::Trash {
                    local_name: "recovery-B".into(),
                    parent: "trash".into(),
                },
            )
            .unwrap();
            j.acknowledge_identity_handoff(next, row.attempt.unwrap(), c.clone(), b_backup.clone())
                .unwrap();
            assert_eq!(j.read_working(f.working, 0, 4096).unwrap(), dirty);
            assert!(j.working_file(f.working).unwrap().dirty);
        }
        let mut store = Store::open(&f.engine.db).unwrap();
        store.observe_node(&f.scope, &c).unwrap();
        drop(store);
        assert!(f.writer.publish_ordinary_metadata(&f.engine).await.unwrap());
        assert!(!f.writer.publish_ordinary_metadata(&f.engine).await.unwrap());
        let store = Store::open(&f.engine.db).unwrap();
        assert_eq!(store.children(&f.scope, "root").unwrap().unwrap(), vec![c]);
        assert_eq!(
            store.node(&f.scope, &f.original.id).unwrap(),
            Some(f.backup)
        );
        assert_eq!(store.node(&f.scope, &f.current.id).unwrap(), Some(b_backup));
        assert_eq!(f.provider.calls.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn ordinary_handoff_history_survives_completed_relocate_and_unlink() {
        for unlink in [false, true] {
            let f = OrdinaryMetadataCase::new(false).await;
            let before = {
                let mut j = f.journal.lock().unwrap();
                let owner = j.namespace_for_operation(f.id).unwrap().unwrap();
                if unlink {
                    let removed = j
                        .unlink_namespace_file(owner.id, owner.revision, false)
                        .unwrap();
                    assert_eq!(
                        j.namespace_object(owner.id).unwrap().latest,
                        Some(removed.mutation.id)
                    );
                } else {
                    let relocated = j
                        .relocate_namespace_item(
                            owner.id,
                            owner.revision,
                            "root".into(),
                            "renamed.txt".into(),
                        )
                        .unwrap();
                    let claimed = j.claim_mutation().unwrap().unwrap();
                    assert_eq!(claimed.id, relocated.id);
                    let remote = Node {
                        name: "renamed.txt".into(),
                        etag: Some("relocated-B".into()),
                        content_version: Some("relocated-B".into()),
                        ..f.current.clone()
                    };
                    assert_eq!(
                        j.acknowledge_mutation(
                            claimed.id,
                            claimed.attempt.unwrap(),
                            cirrove_core::mutation::MutationReceipt::Upsert(remote)
                        )
                        .unwrap(),
                        crate::journal::MutationState::Applied
                    );
                }
                j.namespace_object(owner.id).unwrap()
            };
            assert_ne!(before.latest, Some(f.id));
            assert!(f.writer.publish_ordinary_metadata(&f.engine).await.unwrap());
            assert_eq!(
                serde_json::to_value(
                    f.journal
                        .lock()
                        .unwrap()
                        .namespace_object(before.id)
                        .unwrap()
                )
                .unwrap(),
                serde_json::to_value(&before).unwrap()
            );
            assert_eq!(
                Store::open(&f.engine.db)
                    .unwrap()
                    .node(&f.scope, &f.original.id)
                    .unwrap(),
                Some(f.backup)
            );
            assert_eq!(f.provider.calls.load(Ordering::SeqCst), 0);
        }
    }
    #[tokio::test]
    async fn ordinary_handoff_ro_startup_repairs_history_without_reconciling_uploads() {
        let f = OrdinaryMetadataCase::new(true).await;
        let pending = {
            let mut j = f.journal.lock().unwrap();
            j.write_working(f.working, 0, b"pending-C").unwrap();
            let next = j.seal_working(f.working).unwrap().unwrap().id;
            let claimed = j.claim_next().unwrap().unwrap();
            assert_eq!(claimed.id, next);
            claimed
        };
        let path = f.journal_root.join("uploads.db");
        let upload_before: String = rusqlite::Connection::open(&path)
            .unwrap()
            .query_row(
                "SELECT body FROM uploads WHERE id=?1",
                [pending.id.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        let OrdinaryMetadataCase {
            root,
            journal_root: _,
            engine,
            writer,
            journal,
            provider,
            working: _,
            id,
            scope,
            original,
            current: _,
            backup,
        } = f;
        let mut account = engine.account.clone();
        account.access = AccessMode::ReadOnly;
        drop(writer);
        drop(journal);
        drop(engine);
        let ro = Engine::new(account, provider.clone(), root.join("state"))
            .await
            .unwrap();
        ro.start().await.unwrap(); // Actual normal RO start registration, no Writeback.
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let done: bool = rusqlite::Connection::open(&path)
                    .unwrap()
                    .query_row(
                        "SELECT done FROM ordinary_metadata_publication WHERE operation=?1",
                        [id.to_string()],
                        |r| r.get(0),
                    )
                    .unwrap();
                if done {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        ro.stop().await;
        let db = rusqlite::Connection::open(&path).unwrap();
        let upload_after: String = db
            .query_row(
                "SELECT body FROM uploads WHERE id=?1",
                [pending.id.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(upload_after, upload_before);
        assert_eq!(pending.state, crate::journal::UploadState::Uploading);
        assert_eq!(
            Store::open(&ro.db)
                .unwrap()
                .node(&scope, &original.id)
                .unwrap(),
            Some(backup)
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn ordinary_handoff_restart_repairs_before_and_after_store_commit_idempotently() {
        for store_committed in [false, true] {
            let f = OrdinaryMetadataCase::new(false).await;
            let proof = f
                .journal
                .lock()
                .unwrap()
                .ordinary_metadata_due(0)
                .unwrap()
                .unwrap();
            let mut store = Store::open(&f.engine.db).unwrap();
            let cursor = store.cursor(&f.scope).unwrap();
            if store_committed {
                assert!(
                    store
                        .publish_ordinary_handoff_backup(
                            &proof.scope,
                            &proof.original,
                            &proof.current,
                            &proof.backup
                        )
                        .unwrap()
                );
            }
            drop(store);
            let OrdinaryMetadataCase {
                journal_root,
                engine,
                writer,
                journal,
                provider,
                id,
                scope,
                current,
                original: _,
                backup: _,
                root: _,
                working: _,
            } = f;
            drop(writer);
            drop(journal); // Exclusive handle restart, no normalizing writer open.
            assert!(
                engine
                    .repair_ordinary_metadata_once_at(journal_root.clone())
                    .await
                    .unwrap()
            );
            let store = Store::open(&engine.db).unwrap();
            assert_eq!(
                store.children(&scope, "root").unwrap().unwrap(),
                vec![current]
            );
            assert_eq!(store.cursor(&scope).unwrap(), cursor);
            drop(store);
            assert!(
                !engine
                    .repair_ordinary_metadata_once_at(journal_root.clone())
                    .await
                    .unwrap()
            );
            let handle =
                crate::journal::MetadataPublicationJournal::open(&journal_root, "handoff").unwrap();
            assert!(handle.due(0).unwrap().is_none());
            drop(handle);
            let db = rusqlite::Connection::open(journal_root.join("uploads.db")).unwrap();
            assert_eq!(db.query_row("SELECT count(*) FROM ordinary_metadata_publication WHERE operation=?1 AND done=1",[id.to_string()],|r|r.get::<_,i64>(0)).unwrap(),1);
            assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        }
    }
    #[tokio::test]
    async fn ordinary_handoff_metadata_preserves_newer_observations_and_absence() {
        for arm in [
            "old_revision",
            "old_location",
            "old_name",
            "absence",
            "current_revision",
            "large_unrelated",
        ] {
            let f = OrdinaryMetadataCase::new(false).await;
            let mut store = Store::open(&f.engine.db).unwrap();
            let mut external = f.original.clone();
            match arm {
                "old_revision" => {
                    external.etag = Some("external".into());
                    external.content_version = Some("external".into());
                    store.observe_node(&f.scope, &external).unwrap();
                }
                "old_location" => {
                    external.parent_id = Some("external".into());
                    store.observe_node(&f.scope, &external).unwrap();
                }
                "old_name" => {
                    external.name = "external.txt".into();
                    store.observe_node(&f.scope, &external).unwrap();
                }
                "absence" => {
                    let ticket = store.node_observation(&f.scope, &external.id).unwrap();
                    store.publish_absence(&ticket).unwrap();
                }
                "current_revision" => {
                    external = f.current.clone();
                    external.etag = Some("external-B".into());
                    external.content_version = Some("external-B".into());
                    store.observe_node(&f.scope, &external).unwrap();
                }
                _ => {
                    let mut nodes = vec![f.original.clone(), f.current.clone()];
                    nodes.extend((0..4096).map(|i| Node {
                        id: format!("unrelated-{i:04}"),
                        ..external.clone()
                    }));
                    store.observe_directory(&f.scope, "root", &nodes).unwrap();
                }
            }
            let cursor = store.cursor(&f.scope).unwrap();
            drop(store);
            assert!(f.writer.publish_ordinary_metadata(&f.engine).await.unwrap());
            let store = Store::open(&f.engine.db).unwrap();
            assert_eq!(store.cursor(&f.scope).unwrap(), cursor);
            if matches!(
                arm,
                "old_revision" | "old_location" | "old_name" | "current_revision"
            ) {
                assert_eq!(store.node(&f.scope, &external.id).unwrap(), Some(external));
            } else if arm == "absence" {
                assert!(store.node(&f.scope, &external.id).unwrap().is_none());
            } else {
                let nodes = store.children(&f.scope, "root").unwrap().unwrap();
                assert_eq!(nodes.len(), 4097);
                assert_eq!(
                    nodes
                        .iter()
                        .filter(|n| n.id.starts_with("unrelated-"))
                        .count(),
                    4096
                );
            }
            assert_eq!(f.provider.calls.load(Ordering::SeqCst), 0);
        }
    }
    #[tokio::test]
    async fn ordinary_handoff_queue_rejects_corruption_and_defers_a_bad_head_without_acknowledging_it()
     {
        for arm in [
            "capture_missing",
            "capture_name",
            "capture_parent",
            "capture_revision",
            "capture_size",
            "current_id",
            "backup_id",
            "operation_map",
            "serialized_state",
            "serialized_scope",
            "native",
            "queue_incomplete",
            "sql_state",
            "negative_sequence",
            "job_scope",
            "job_original",
        ] {
            let f = OrdinaryMetadataCase::new(false).await;
            let db = rusqlite::Connection::open(f.journal_root.join("uploads.db")).unwrap();
            let text: String = db
                .query_row(
                    "SELECT body FROM uploads WHERE id=?1",
                    [f.id.to_string()],
                    |r| r.get(0),
                )
                .unwrap();
            let mut row: serde_json::Value = serde_json::from_str(&text).unwrap();
            match arm {
                "capture_missing" => {
                    row["identity_handoff"]["metadata_original"] = serde_json::Value::Null
                }
                "capture_name" => {
                    row["identity_handoff"]["metadata_original"]["name"] =
                        serde_json::json!("foreign")
                }
                "capture_parent" => {
                    row["identity_handoff"]["metadata_original"]["parent_id"] =
                        serde_json::json!("foreign")
                }
                "capture_revision" => {
                    row["identity_handoff"]["metadata_original"]["etag"] =
                        serde_json::json!("foreign")
                }
                "capture_size" => {
                    row["identity_handoff"]["metadata_original"]["size"] = serde_json::json!(999)
                }
                "current_id" => row["remote"]["id"] = serde_json::json!(f.original.id),
                "backup_id" => {
                    row["identity_handoff"]["backup"]["id"] = serde_json::json!("foreign")
                }
                "serialized_state" => row["state"] = serde_json::json!("verify_required"),
                "serialized_scope" => row["scope"]["collection"] = serde_json::json!("foreign"),
                "native" => {
                    row["representation"] = serde_json::json!({"kind":"package_archive","expected_root":"Owned.numbers","semantic":{"version":2,"sha256":"a".repeat(64),"entries":2,"files":1,"expanded_bytes":8}})
                }
                "operation_map" => {
                    db.execute(
                        "UPDATE namespace_operations SET object=?2 WHERE operation=?1",
                        rusqlite::params![f.id.to_string(), Uuid::new_v4().to_string()],
                    )
                    .unwrap();
                }
                "queue_incomplete" => {
                    db.execute(
                        "UPDATE write_queue SET complete=0 WHERE id=?1",
                        [f.id.to_string()],
                    )
                    .unwrap();
                }
                "negative_sequence" => {
                    db.execute(
                        "UPDATE uploads SET sequence=-1 WHERE id=?1",
                        [f.id.to_string()],
                    )
                    .unwrap();
                }
                "sql_state" => {
                    db.execute(
                        "UPDATE uploads SET state='pending' WHERE id=?1",
                        [f.id.to_string()],
                    )
                    .unwrap();
                }
                _ => {
                    let text: String = db
                        .query_row(
                            "SELECT body FROM ordinary_metadata_publication WHERE operation=?1",
                            [f.id.to_string()],
                            |r| r.get(0),
                        )
                        .unwrap();
                    let mut job: serde_json::Value = serde_json::from_str(&text).unwrap();
                    if arm == "job_scope" {
                        job["scope"]["provider"] = serde_json::json!("foreign");
                    } else {
                        job["original"]["etag"] = serde_json::json!("foreign");
                    }
                    db.execute(
                        "UPDATE ordinary_metadata_publication SET body=?2 WHERE operation=?1",
                        rusqlite::params![f.id.to_string(), job.to_string()],
                    )
                    .unwrap();
                }
            }
            db.execute(
                "UPDATE uploads SET body=?2 WHERE id=?1",
                rusqlite::params![f.id.to_string(), row.to_string()],
            )
            .unwrap();
            let store = Store::open(&f.engine.db).unwrap();
            let before = store.visible_nodes(&f.scope).unwrap();
            let cursor = store.cursor(&f.scope).unwrap();
            assert!(
                f.journal.lock().unwrap().ordinary_metadata_due(0).is_err(),
                "{arm}"
            );
            assert_eq!(
                db.query_row(
                    "SELECT done,failures FROM ordinary_metadata_publication WHERE operation=?1",
                    [f.id.to_string()],
                    |r| Ok((r.get::<_, bool>(0)?, r.get::<_, i64>(1)?))
                )
                .unwrap(),
                (false, 1)
            );
            assert_eq!(store.visible_nodes(&f.scope).unwrap(), before);
            assert_eq!(store.cursor(&f.scope).unwrap(), cursor);
            assert_eq!(f.provider.calls.load(Ordering::SeqCst), 0);
        }
        let f = OrdinaryMetadataCase::new(false).await;
        let db = rusqlite::Connection::open(f.journal_root.join("uploads.db")).unwrap();
        db.execute(
            "INSERT INTO ordinary_metadata_publication(operation,body) VALUES(?1,'malformed')",
            [Uuid::nil().to_string()],
        )
        .unwrap();
        assert!(f.journal.lock().unwrap().ordinary_metadata_due(0).is_err());
        let proof = f
            .journal
            .lock()
            .unwrap()
            .ordinary_metadata_due(0)
            .unwrap()
            .unwrap();
        assert_eq!(proof.operation, f.id);
        assert_eq!(
            db.query_row(
                "SELECT done,failures FROM ordinary_metadata_publication WHERE operation=?1",
                [Uuid::nil().to_string()],
                |r| Ok((r.get::<_, bool>(0)?, r.get::<_, i64>(1)?))
            )
            .unwrap(),
            (false, 1)
        );
    }

    #[tokio::test]
    async fn ordinary_handoff_engine_scope_refuses_foreign_provider_before_store_or_read() {
        let f = OrdinaryMetadataCase::new(false).await;
        let mut proof = f
            .journal
            .lock()
            .unwrap()
            .ordinary_metadata_due(0)
            .unwrap()
            .unwrap();
        proof.scope.provider = "foreign-provider".into();
        let store = Store::open(&f.engine.db).unwrap();
        let visible = store.visible_nodes(&f.scope).unwrap();
        let cursor = store.cursor(&f.scope).unwrap();
        assert!(f.engine.publish_ordinary_metadata(&proof).await.is_err());
        assert_eq!(store.visible_nodes(&f.scope).unwrap(), visible);
        assert_eq!(store.cursor(&f.scope).unwrap(), cursor);
        assert_eq!(f.provider.calls.load(Ordering::SeqCst), 0);
        let db = rusqlite::Connection::open(f.journal_root.join("uploads.db")).unwrap();
        assert!(
            !db.query_row(
                "SELECT done FROM ordinary_metadata_publication WHERE operation=?1",
                [f.id.to_string()],
                |r| r.get::<_, bool>(0)
            )
            .unwrap()
        );
    }

    #[tokio::test]
    async fn ordinary_handoff_later_complete_listing_supersedes_old_absence() {
        for staged in [false, true] {
            let f = OrdinaryMetadataCase::new(false).await;
            let mut store = Store::open(&f.engine.db).unwrap();
            let ticket = store.node_observation(&f.scope, &f.original.id).unwrap();
            store.publish_absence(&ticket).unwrap();
            assert!(store.node(&f.scope, &f.original.id).unwrap().is_none());
            if staged {
                store
                    .directory_publication(
                        &f.scope,
                        "root",
                        CancellationToken::new(),
                        std::time::Instant::now() + Duration::from_secs(5),
                    )
                    .unwrap()
                    .page(DirectoryPage {
                        nodes: vec![f.original.clone(), f.current.clone()],
                        next: None,
                    })
                    .unwrap()
                    .publish()
                    .unwrap();
            } else {
                store
                    .observe_directory(&f.scope, "root", &[f.original.clone(), f.current.clone()])
                    .unwrap();
            }
            let store = Store::open(&f.engine.db).unwrap();
            assert_eq!(store.children(&f.scope, "root").unwrap().unwrap().len(), 2);
            assert_eq!(
                store.node(&f.scope, &f.original.id).unwrap(),
                Some(f.original.clone())
            );
            drop(store);
            assert!(f.writer.publish_ordinary_metadata(&f.engine).await.unwrap());
            assert_eq!(
                Store::open(&f.engine.db)
                    .unwrap()
                    .children(&f.scope, "root")
                    .unwrap()
                    .unwrap(),
                vec![f.current]
            );
            assert_eq!(f.provider.calls.load(Ordering::SeqCst), 0);
        }
    }
    #[tokio::test]
    async fn ordinary_handoff_legacy_missing_capture_is_not_backfilled_on_reopen() {
        let f = OrdinaryMetadataCase::new(false).await;
        let db = rusqlite::Connection::open(f.journal_root.join("uploads.db")).unwrap();
        db.execute(
            "DELETE FROM ordinary_metadata_publication WHERE operation=?1",
            [f.id.to_string()],
        )
        .unwrap();
        db.execute("UPDATE uploads SET body=json_remove(body,'$.identity_handoff.metadata_original') WHERE id=?1",[f.id.to_string()]).unwrap();
        let before: String = db
            .query_row(
                "SELECT body FROM uploads WHERE id=?1",
                [f.id.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        drop(db);
        let OrdinaryMetadataCase {
            journal_root,
            engine,
            writer,
            journal,
            provider,
            id,
            scope,
            root: _,
            working: _,
            original: _,
            current: _,
            backup: _,
        } = f;
        let store = Store::open(&engine.db).unwrap();
        let visible = store.visible_nodes(&scope).unwrap();
        drop(store);
        drop(writer);
        drop(journal);
        let reopened = UploadJournal::open(&journal_root, "handoff", 4096).unwrap();
        assert!(reopened.ordinary_metadata_due(0).unwrap().is_none());
        let db = rusqlite::Connection::open(journal_root.join("uploads.db")).unwrap();
        let after: String = db
            .query_row(
                "SELECT body FROM uploads WHERE id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(after, before);
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM ordinary_metadata_publication",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            Store::open(&engine.db)
                .unwrap()
                .visible_nodes(&scope)
                .unwrap(),
            visible
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    }
}
