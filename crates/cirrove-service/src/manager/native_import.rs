//! Explicit PACKAGE capture/queue admission. No job or protocol ownership here.
use super::*;
use crate::{
    filesystem::WriteControl,
    native_import::{NativeImportInput, ValidatedPackageArchive},
};
use cirrove_core::upload::{UploadIntent, UploadRepresentation};

pub(super) fn eligible(engine: &Engine) -> bool {
    engine.account.enabled
        && engine.account.access == cirrove_auth::AccessMode::ReadWrite
        && engine.account.registration.provider_id() == "icloud"
        && !engine.cancel.is_cancelled()
}
pub(super) fn status_allows(status: &[AccountStatus], engine: &Engine) -> bool {
    status
        .iter()
        .any(|s| s.account_id == engine.account.id && s.enabled && s.mounted)
}
fn validate_input(input: &NativeImportInput) -> Result<()> {
    if !input.source.is_absolute()
        || input
            .source
            .components()
            .any(|p| matches!(p, std::path::Component::ParentDir))
        || !input.expected_root.to_ascii_lowercase().ends_with(".pages")
        || !input.name.to_ascii_lowercase().ends_with(".pages")
        || input
            .expected_root
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
        || input.expected_root.is_empty()
        || input.expected_root.len() > 255
        || input.name.len() > 255
        || input
            .name
            .chars()
            .any(|c| c.is_control() || c == '\\' || c == ':')
    {
        bail!("invalid native import request");
    }
    UploadIntent::Create {
        parent: "checked-parent".into(),
        name: input.name.clone(),
    }
    .validate()
    .map_err(|_| anyhow::anyhow!("invalid native import name"))?;
    Ok(())
}
fn state_root(engine: &Engine) -> Result<PathBuf> {
    engine
        .db
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .context("native import state is unavailable")
}
fn source_allowed(source: &Path, excluded: &[PathBuf]) -> Result<()> {
    let resolved = source
        .canonicalize()
        .map_err(|_| anyhow::anyhow!("native import source is unavailable"))?;
    if excluded.iter().any(|root| resolved.starts_with(root)) {
        bail!("native import source must be outside Cirrove mounts and state");
    }
    Ok(())
}
async fn destination(
    control: &WriteControl,
    engine: &Arc<Engine>,
    path: &str,
    cancel: &CancellationToken,
) -> Result<crate::native_import::ImportParent> {
    tokio::select! { biased;
        _ = cancel.cancelled() => bail!("native import cancelled before enqueue"),
        _ = engine.cancel.cancelled() => bail!("native import account stopped"),
        value = tokio::time::timeout(std::time::Duration::from_secs(60), control.native_import_parent(engine, path, cancel)) => {
            Ok(value.context("native import destination check timed out")??)
        }
    }
}
impl Manager {
    /// Observer-only: never capture an archive, enqueue, wake or retry workers.
    pub async fn watch_native_import(
        self: &Arc<Self>,
        request: &crate::WatchNativeImportRequest,
    ) -> Result<crate::jobs::Job> {
        if request.label.is_empty() || uuid::Uuid::parse_str(&request.expected_account_id).is_err()
        {
            bail!("native import watch requires an exact selected account");
        }
        let engine = self.engine(&request.label).await?;
        if engine.account.id != request.expected_account_id {
            bail!("native import watch account changed");
        }
        let row = self
            .native_import_record(&engine, request.operation)
            .await?;
        if !matches!(row.intent, UploadIntent::Create { .. })
            || row.representation.validate().is_err()
            || row.base.is_some()
            || row.identity_handoff.is_some()
            || row.working_file.is_some()
        {
            bail!("saved operation is not an explicit native import");
        }
        engine.start_native_import_watch(self.clone(), row)
    }

    pub(crate) async fn native_import_publication(
        &self,
        engine: &Arc<Engine>,
        operation: uuid::Uuid,
    ) -> Result<crate::journal::PackagePublicationStatus> {
        let control = self.native_import_control(engine).await?;
        let record = control.native_import_record(operation).await?;
        if record.scope != engine.scope(&engine.account.drive.id)
            || !matches!(
                record.representation,
                UploadRepresentation::PackageArchive { .. }
            )
        {
            bail!("native import publication does not belong to this account");
        }
        let publication = control.native_import_publication(operation).await?;
        self.native_import_same_control(engine, &control).await?;
        Ok(publication)
    }

    pub(super) async fn native_import_same_control(
        &self,
        engine: &Arc<Engine>,
        previous: &WriteControl,
    ) -> Result<()> {
        // Revalidate together after the journal await; a same-Engine remount
        // still replaces WriteControl. No await follows these guarded checks.
        let status = self.status.read().await;
        let engines = self.engines.read().await;
        let writers = self.writers.read().await;
        if !eligible(engine)
            || !status_allows(&status, engine)
            || engines
                .get(&engine.account.id)
                .is_none_or(|e| !Arc::ptr_eq(e, engine))
            || writers
                .get(&engine.account.id)
                .is_none_or(|w| !w.same_mount(previous))
            || !previous.belongs_to(engine)
        {
            bail!("native import mount changed");
        }
        previous.native_import_open()?;
        Ok(())
    }

    pub(super) async fn native_import_control(&self, engine: &Arc<Engine>) -> Result<WriteControl> {
        if !eligible(engine) || !status_allows(&self.status.read().await, engine) {
            bail!("native import requires an active writable iCloud mount");
        }
        let engines = self.engines.read().await;
        if engines
            .get(&engine.account.id)
            .is_none_or(|current| !Arc::ptr_eq(current, engine))
        {
            bail!("native import account changed");
        }
        let control = self
            .writers
            .read()
            .await
            .get(&engine.account.id)
            .cloned()
            .context("native import writers are unavailable")?;
        if !control.belongs_to(engine) {
            bail!("native import mount changed");
        }
        control.native_import_open()?;
        Ok(control)
    }
    pub async fn enqueue_native_package(
        self: &Arc<Self>,
        engine: Arc<Engine>,
        input: NativeImportInput,
        cancel: CancellationToken,
    ) -> Result<crate::journal::UploadRecord> {
        validate_input(&input)?;
        let permit = self
            .native_import_slots
            .clone()
            .try_acquire_owned()
            .context("another native import is being captured")?;
        let control = self.native_import_control(&engine).await?;
        let before = destination(&control, &engine, &input.parent, &cancel).await?;
        if before.children.iter().any(|n| n.name == input.name) {
            bail!("native import destination already exists");
        }
        let state = state_root(&engine)?;
        let mut excluded = vec![
            state
                .clone()
                .canonicalize()
                .context("native import state is unavailable")?,
        ];
        for status in self.status.read().await.iter() {
            // Refuse all configured mount locations, including ones reconnecting.
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
            .context("native import state is unavailable")?
            .join("native-import");
        let source = input.source.clone();
        let root = input.expected_root.clone();
        let token = cancel.clone();
        let lifetime = control.clone();
        let (archive, permit, _lifetime) = tokio::task::spawn_blocking(move || -> Result<_> {
            let _lifetime = lifetime;
            source_allowed(&source, &excluded)?;
            crate::private_dir(&stage)
                .map_err(|_| anyhow::anyhow!("native import staging is unavailable"))?;
            let archive = ValidatedPackageArchive::capture_excluding(
                &source, &stage, &root, &token, &excluded,
            )?;
            Ok((archive, permit, _lifetime))
        })
        .await
        .context("native import capture stopped")??;
        if cancel.is_cancelled() {
            bail!("native import cancelled before enqueue");
        }
        let current = self.native_import_control(&engine).await?;
        if !current.same_mount(&control) {
            bail!("native import mount changed");
        }
        let after = destination(&control, &engine, &input.parent, &cancel).await?;
        if before.scope != after.scope
            || before.route != after.route
            || before.frontier != after.frontier
        {
            bail!("native import destination changed during capture");
        }
        let manager = self.clone();
        tokio::task::spawn_blocking(move || -> Result<_> {
            let _permit = permit;
            // These registry guards belong to the blocking worker, not its
            // cancellable waiter. Teardown cannot withdraw this accepted mount
            // until local durable enqueue completes. No provider IO occurs here.
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
            {
                bail!("native import mount changed before enqueue");
            }
            let record = control.enqueue_native_import(after, input.name, archive, &cancel)?;
            // Never replace this durable result with a late cancellation error.
            Ok(record)
        })
        .await
        .context("native import enqueue stopped")?
    }
    pub async fn native_import_record(
        &self,
        engine: &Arc<Engine>,
        operation: uuid::Uuid,
    ) -> Result<crate::journal::UploadRecord> {
        let control = self.native_import_control(engine).await?;
        let record = control.native_import_record(operation).await?;
        if record.scope != engine.scope(&engine.account.drive.id)
            || !matches!(
                record.representation,
                UploadRepresentation::PackageArchive { .. }
            )
        {
            bail!("native import receipt does not belong to this account");
        }
        self.native_import_same_control(engine, &control).await?;
        Ok(record)
    }
}

#[cfg(test)]
mod tests;

mod native_replace;

mod native_replace_observer;
