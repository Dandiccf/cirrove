//! One explicit owned pause after the SDK's final install observation. This
//! feature-only guard does not renew the original arm clock or authorize replay.
use super::*;

impl Guard {
    #[cfg(test)]
    pub(crate) fn install_probe_for_test<'a>(
        &'a self,
        operation: &'a str,
        request: &'a UploadRequest,
        checkpoint: &'a SecretString,
    ) -> impl cirrove_icloud::NativeInstallPreflightProbe + 'a {
        InstallProbe {
            guard: self,
            operation,
            request,
            checkpoint,
        }
    }
    #[cfg(test)]
    pub(crate) fn install_pause_was_used_for_test(&self) -> bool {
        self.install_pause_used.load(Ordering::SeqCst)
    }
    pub(crate) fn with_install_preflight_pause(
        mut self,
        active_deadline: tokio::time::Instant,
        pause_limit: Duration,
    ) -> cirrove_core::upload::Result<Self> {
        let remaining = active_deadline.saturating_duration_since(tokio::time::Instant::now());
        if !matches!(self.mode, Mode::Lose)
            || self.pre_trash_deadline.is_some()
            || remaining.is_zero()
            || remaining > Duration::from_secs(600)
            || !(Duration::from_secs(1)..=Duration::from_secs(120)).contains(&pause_limit)
        {
            return Err(UploadError::Invalid);
        }
        self.install_pause_deadline = Some(active_deadline);
        self.install_pause_limit = pause_limit;
        Ok(self)
    }
}

pub(super) struct InstallProbe<'a> {
    pub guard: &'a Guard,
    pub operation: &'a str,
    pub request: &'a UploadRequest,
    pub checkpoint: &'a SecretString,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallRelease {
    version: u32,
    run: Uuid,
    operation: Uuid,
    attempt: Uuid,
    checkpoint_sha256: String,
    phase: String,
    original_id: String,
    staged_id: String,
    parent_id: String,
    scope: Scope,
}
#[async_trait::async_trait]
impl cirrove_icloud::NativeInstallPreflightProbe for InstallProbe<'_> {
    async fn after_final_preflight(
        &self,
        original_id: &str,
        staged_id: &str,
        parent_id: &str,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let g = self.guard;
        let id = g.operation(self.operation, self.request)?;
        let original = match &self.request.representation {
            cirrove_core::upload::UploadRepresentation::PackageReplacementArchive {
                original,
                ..
            }
            | cirrove_core::upload::UploadRepresentation::FlatNumbersReplacementArchive {
                original,
                ..
            } => original,
            _ => return Err(UploadError::Invalid.into()),
        };
        ensure!(
            original_id == original.id
                && Some(parent_id) == original.parent_id.as_deref()
                && staged_id != original_id
                && staged_id.starts_with("FILE::com.apple.CloudDocs::")
                && staged_id.len() <= 4096,
            "install probe identity refused"
        );
        // Claim once before any marker or wait. A duplicate call cannot send.
        ensure!(
            !g.install_pause_used.swap(true, Ordering::SeqCst),
            "install probe repeated"
        );
        let row = g
            .journal
            .lock()
            .map_err(|_| UploadError::Uncertain)?
            .get(id)?;
        let attempt = row.attempt.ok_or(UploadError::CheckpointInvalid)?;
        ensure!(
            row.state == UploadState::Uploading && row.session_key == Some(id),
            "install frontier refused"
        );
        let frozen = serde_json::to_vec(&row)?;
        let checkpoint_sha256 = hash(self.checkpoint.expose_secret().as_bytes());
        let until = g
            .install_pause_deadline
            .ok_or(UploadError::Invalid)?
            .min(tokio::time::Instant::now() + g.install_pause_limit);
        let fence = || -> Result<()> {
            ensure!(
                !cancel.is_cancelled() && tokio::time::Instant::now() < until,
                "install probe window closed"
            );
            Ok(())
        };
        fence()?;
        let release_path = g.directory.join("install-preflight-release.json");
        match std::fs::symlink_metadata(&release_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err(UploadError::CheckpointInvalid.into()),
        }
        record(
            &g.directory.join("install-preflight-paused.json"),
            &serde_json::json!({
                "version":1,"run":g.run,"operation":id,"attempt":attempt,
                "checkpoint_sha256":checkpoint_sha256,"phase":"after-final-install-preflight",
                "rename_called":false,"original_id":original_id,"staged_id":staged_id,"parent_id":parent_id,"scope":self.request.scope
            }),
        )?;
        loop {
            // Recheck after durable publication and every polling iteration.
            fence()?;
            match OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&release_path)
            {
                Ok(mut file) => {
                    let meta = file.metadata()?;
                    let uid = std::fs::metadata("/proc/self")?.uid();
                    ensure!(
                        meta.is_file()
                            && meta.uid() == uid
                            && meta.nlink() == 1
                            && meta.permissions().mode() & 0o7777 == 0o400
                            && meta.len() <= 2048,
                        "install release file refused"
                    );
                    let mut bytes = Vec::new();
                    std::io::Read::by_ref(&mut file)
                        .take(2049)
                        .read_to_end(&mut bytes)?;
                    ensure!(bytes.len() <= 2048, "install release too large");
                    let end = file.metadata()?;
                    let named = std::fs::symlink_metadata(&release_path)?;
                    let stamp = |m: &std::fs::Metadata| {
                        (
                            m.dev(),
                            m.ino(),
                            m.mode(),
                            m.uid(),
                            m.gid(),
                            m.nlink(),
                            m.len(),
                            m.mtime_nsec(),
                            m.ctime_nsec(),
                            m.mtime(),
                            m.ctime(),
                        )
                    };
                    ensure!(
                        stamp(&meta) == stamp(&end) && stamp(&end) == stamp(&named),
                        "install release changed"
                    );
                    let release: InstallRelease = serde_json::from_slice(&bytes)?;
                    ensure!(
                        release.version == 1
                            && release.run == g.run
                            && release.operation == id
                            && release.attempt == attempt
                            && release.checkpoint_sha256 == checkpoint_sha256
                            && release.phase == "after-final-install-preflight"
                            && release.original_id == original_id
                            && release.staged_id == staged_id
                            && release.parent_id == parent_id
                            && release.scope == self.request.scope,
                        "install release tuple refused"
                    );
                    g.operation(self.operation, self.request)?;
                    let current = g
                        .journal
                        .lock()
                        .map_err(|_| UploadError::Uncertain)?
                        .get(id)?;
                    ensure!(
                        serde_json::to_vec(&current)? == frozen,
                        "install frontier changed"
                    );
                    fence()?;
                    return Ok(());
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(UploadError::CheckpointInvalid.into()),
            }
            tokio::select! {
                _ = cancel.cancelled() => return Err(UploadError::Uncertain.into()),
                _ = tokio::time::sleep(Duration::from_millis(50)) => {},
            }
        }
    }
}
