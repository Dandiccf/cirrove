//! Internal explicit replacement admission. No public command or FUSE admission.
use super::*;
use crate::native_import::NativeReplaceInput;
use cirrove_core::upload::PackageSemanticIdentity;
impl Manager {
    pub async fn enqueue_native_replacement(
        self: &Arc<Self>,
        engine: Arc<Engine>,
        input: NativeReplaceInput,
        cancel: CancellationToken,
    ) -> Result<crate::journal::UploadRecord> {
        #[cfg(test)]
        {
            let fixture = self
                .native_replace_capture_fixture
                .lock()
                .map_err(|_| anyhow::anyhow!("test capture fixture poisoned"))?
                .clone();
            if let Some(fixture) = fixture {
                if fixture.account != engine.account.id {
                    bail!("test capture account changed");
                }
                return self
                    .enqueue_native_replacement_with(
                        engine,
                        input,
                        cancel,
                        move |node, _, token| async move {
                            if token.is_cancelled() || node != fixture.original {
                                bail!("test original capture binding changed");
                            }
                            fixture
                                .calls
                                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            Ok(fixture.semantic.clone())
                        },
                    )
                    .await;
            }
        }
        let capture_engine = engine.clone();
        self.enqueue_native_replacement_with(
            engine,
            input,
            cancel,
            move |node, stage, token| async move {
                let adapter = cirrove_icloud::ICloudNativeTrash::from_sealed_session(
                    capture_engine.scope(&capture_engine.account.drive.id),
                    capture_engine.account.identity.username.clone(),
                    capture_engine.account.credential_id.clone(),
                    &state_root(&capture_engine)?,
                    node,
                    stage,
                )?;
                Ok(adapter.capture_active_semantic(&token).await?)
            },
        )
        .await
    }
    pub(in crate::manager) async fn enqueue_native_replacement_with<F, Fut>(
        self: &Arc<Self>,
        engine: Arc<Engine>,
        input: NativeReplaceInput,
        cancel: CancellationToken,
        capture: F,
    ) -> Result<crate::journal::UploadRecord>
    where
        F: FnOnce(cirrove_core::Node, PathBuf, CancellationToken) -> Fut,
        Fut: std::future::Future<Output = Result<PackageSemanticIdentity>>,
    {
        use crate::native_import::ReplacementAdmissionError as Phase;
        let mut phase = Phase::Selection;
        let result = async {
        if !crate::native_trash::validate(&input.selected)
            || input.selected.expected_account_id != engine.account.id
        {
            bail!("native replacement selection changed");
        }
        let name = input
            .selected
            .path
            .rsplit('/')
            .next()
            .context("native replacement path missing")?
            .to_owned();
        validate_input(&NativeImportInput {
            source: input.source.clone(),
            expected_root: input.expected_root.clone(),
            parent: String::new(),
            name,
        })?;
        let permit = self
            .native_import_slots
            .clone()
            .try_acquire_owned()
            .context("another native document action is active")?;
        let control = self.native_import_control(&engine).await?;
        let before = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            control.native_trash_selection(&engine, &input.selected, &cancel),
        )
        .await
        .context("native replacement selection timed out")??;
        phase = Phase::Archive;
        let state = state_root(&engine)?;
        let mut excluded = vec![
            state
                .canonicalize()
                .context("native replacement state unavailable")?,
        ];
        for status in self.status.read().await.iter() {
            excluded.push(
                status
                    .mount_path
                    .canonicalize()
                    .unwrap_or_else(|_| status.mount_path.clone()),
            );
        }
        let stage = engine
            .db
            .parent()
            .context("native replacement staging unavailable")?
            .join("native-import");
        let source = input.source;
        let root = input.expected_root;
        let token = cancel.clone();
        let lifetime = control.clone();
        let capture_stage = stage.clone();
        let (archive, permit, _lifetime) = tokio::task::spawn_blocking(move || -> Result<_> {
            source_allowed(&source, &excluded)?;
            crate::private_dir(&stage)?;
            let archive = ValidatedPackageArchive::capture_excluding(
                &source, &stage, &root, &token, &excluded,
            )?;
            Ok((archive, permit, lifetime))
        })
        .await
        .context("native replacement capture stopped")??;
        phase = Phase::Original;
        let semantic = tokio::select! {biased;
            _=cancel.cancelled()=>bail!("native replacement cancelled before enqueue"),
            _=engine.cancel.cancelled()=>bail!("native replacement account stopped"),
            result=tokio::time::timeout(std::time::Duration::from_secs(300),capture(before.target.clone(),capture_stage,cancel.clone()))=>result.context("native original verification timed out")??,
        };
        semantic
            .validate()
            .map_err(|_| anyhow::anyhow!("native original semantic proof invalid"))?;
        phase = Phase::Recheck;
        let current = self.native_import_control(&engine).await?;
        if !current.same_mount(&control) {
            bail!("native replacement mount changed");
        }
        let after = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            control.native_trash_selection(&engine, &input.selected, &cancel),
        )
        .await
        .context("native replacement recheck timed out")??;
        if before.target != after.target
            || before.parent.scope != after.parent.scope
            || before.parent.route != after.parent.route
            || before.parent.frontier != after.parent.frontier
        {
            bail!("native replacement selection changed during capture");
        }
        phase = Phase::Enqueue;
        let manager = self.clone();
        tokio::task::spawn_blocking(move || -> Result<_> {
            let _permit = permit;
            let status = manager.status.blocking_read();
            let engines = manager.engines.blocking_read();
            let writers = manager.writers.blocking_read();
            if !eligible(&engine)
                || !status_allows(&status, &engine)
                || engines
                    .get(&engine.account.id)
                    .is_none_or(|e| !Arc::ptr_eq(e, &engine))
                || writers
                    .get(&engine.account.id)
                    .is_none_or(|w| !w.same_mount(&control))
                || !control.belongs_to(&engine)
            {
                bail!("native replacement mount changed before enqueue");
            }
            // This durable result survives late observer cancellation.
            let record = control.enqueue_native_replace(after, semantic, archive, &cancel)?;
            #[cfg(test)]
            {
                let pause = manager
                    .native_replace_after_enqueue
                    .lock()
                    .map_err(|_| anyhow::anyhow!("test enqueue hook poisoned"))?
                    .take();
                if let Some(pause) = pause {
                    let _ = pause.ready.send(record.id);
                    pause
                        .release
                        .recv_timeout(std::time::Duration::from_secs(5))
                        .context("test enqueue hook timed out")?;
                }
            }
            Ok(record)
        })
        .await
        .context("native replacement enqueue stopped")?
        }.await;
        // Drop dynamic causes at this boundary. A late error is not proof that
        // nothing was queued, and the public message never claims otherwise.
        result.map_err(|_: anyhow::Error| anyhow::Error::new(phase))
    }
}
