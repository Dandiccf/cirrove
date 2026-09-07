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
    fn activity_gate(&self, identity: EditKey) -> Result<Arc<RwLock<()>>> {
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
    /// At most 16 durable objects and one provider request per call. The cursor
    /// makes pending/open objects yield to others. Reloading durable snapshots
    /// also repairs an earlier failed local projection refresh without replaying
    /// the provider operation.
    pub async fn maintain(self: &Arc<Self>, engine: &Engine) -> Result<bool> {
        if self.preserve_unlinked(engine).await? {
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
                        let working = object
                            .working_file
                            .map(|id| j.working_file(id))
                            .transpose()?;
                        Ok((object, working, clean))
                    })
                    .collect::<crate::journal::Result<Vec<_>>>()
            })
            .await?;
        if batch.is_empty() {
            *self.maintenance_cursor.lock().map_err(|_| Errno::EIO)? = None;
            return Ok(false);
        }
        for (object, working, clean) in batch {
            *self.maintenance_cursor.lock().map_err(|_| Errno::EIO)? = Some(object.id);
            let repaired = {
                let mut projection = self.projection.lock().map_err(|_| Errno::EIO)?;
                let changed = projection.validate_merge(&object, working.as_ref())?;
                if changed {
                    projection.apply(object.clone(), working);
                }
                changed
            };
            if repaired {
                engine.changed.notify_waiters();
            }
            if !clean {
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
            let remote = if cached.as_ref() == Some(&expected) {
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
                let following = object.followed(remote.clone()).map_err(error)?;
                // Reserve and validate the in-memory publication before the
                // durable detach. Once committed, applying it is infallible.
                let mut projection = writer.projection.lock().map_err(|_| Errno::EIO)?;
                if !projection.validate_merge(&following, None)? {
                    return Err(Errno::ESTALE);
                }
                let committed = journal
                    .handoff_namespace(object.id, object.revision, remote)
                    .map_err(error)?;
                projection.apply(committed, None);
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
                registration: AppRegistration {
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
}
