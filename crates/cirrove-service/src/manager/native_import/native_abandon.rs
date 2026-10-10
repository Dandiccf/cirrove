//! Explicit local abandonment after fresh, mutationless Stage evidence.
use super::*;
use crate::native_abandon::{NativeAbandonReceipt, NativeAbandonRequest};
use cirrove_auth::CredentialVault;
use cirrove_core::upload::UploadRequest;
use cirrove_icloud::{NativeReplacementAbandonEvidence, SealedUploadCheckpointVault};
#[cfg(test)]
pub(crate) type TestInspector = dyn Fn(
        UploadRequest,
        uuid::Uuid,
        CancellationToken,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<NativeReplacementAbandonEvidence>> + Send>,
    > + Send
    + Sync;
impl Manager {
    pub(crate) async fn start_native_abandon(
        self: &Arc<Self>,
        engine: Arc<Engine>,
        input: NativeAbandonRequest,
    ) -> Result<crate::jobs::Job> {
        validate(&engine, &input)?;
        let control = self.native_import_control(&engine).await?;
        engine.start_native_abandon_job(self.clone(), control, input)
    }
    pub(crate) async fn native_abandon_receipt(
        &self,
        engine: &Arc<Engine>,
        input: &NativeAbandonRequest,
    ) -> Result<Option<NativeAbandonReceipt>> {
        validate(engine, input)?;
        if !engine.account.enabled
            || engine.cancel.is_cancelled()
            || engine.account.registration.provider_id() != "icloud"
        {
            bail!("native abandonment account unavailable");
        }
        {
            let engines = self.engines.read().await;
            if engines
                .get(&input.expected_account_id)
                .is_none_or(|current| !Arc::ptr_eq(current, engine))
            {
                bail!("native abandonment account changed");
            }
        }
        let control = self.recovery_control(engine.clone()).await?;
        let record = control.native_abandon_record(input.operation).await?;
        let engines = self.engines.read().await;
        if engine.cancel.is_cancelled()
            || engines
                .get(&input.expected_account_id)
                .is_none_or(|current| !Arc::ptr_eq(current, engine))
        {
            bail!("native abandonment account changed");
        }
        record
            .map(|record| NativeAbandonReceipt::bound(record, &engine.account.id, input.operation))
            .transpose()
    }
    pub(crate) async fn abandon_native_stage(
        self: &Arc<Self>,
        engine: Arc<Engine>,
        control: WriteControl,
        input: NativeAbandonRequest,
        cancel: CancellationToken,
    ) -> Result<NativeAbandonReceipt> {
        validate(&engine, &input)?;
        let permit = self
            .native_import_slots
            .clone()
            .try_acquire_owned()
            .context("another native document action is active")?;
        self.native_import_same_control(&engine, &control).await?;
        let prepared = control.prepare_native_abandon(input.operation).await?;
        let request = prepared.request();
        if prepared.operation() != input.operation
            || request.scope != engine.scope(&engine.account.drive.id)
        {
            bail!("native abandonment account or operation changed");
        }
        self.native_import_same_control(&engine, &control).await?;
        let inspect =
            self.inspect_native_abandon(&engine, request, input.operation, cancel.clone());
        let evidence = tokio::select! { biased;
            _=cancel.cancelled()=>bail!("native abandonment cancelled"),
            _=engine.cancel.cancelled()=>bail!("native abandonment account stopped"),
            result=tokio::time::timeout(std::time::Duration::from_secs(190), inspect)=>result.context("native abandonment inspection timed out")??,
        };
        let manager = self.clone();
        let publication = control.clone();
        let record = tokio::task::spawn_blocking(move || -> Result<_> {
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
                bail!("native abandonment mount changed");
            }
            let record = control.commit_native_abandon(prepared, evidence, &cancel)?;
            NativeAbandonReceipt::bound(record, &input.expected_account_id, input.operation)
        })
        .await
        .context("native abandonment commit stopped")??;
        // A publication error must not hide an already committed local outcome.
        // The durable namespace frontier converges on the next refresh/restart.
        let _ = publication.refresh_native_abandon(record.operation).await;
        Ok(record)
    }
    async fn inspect_native_abandon(
        &self,
        engine: &Engine,
        request: UploadRequest,
        operation: uuid::Uuid,
        cancel: CancellationToken,
    ) -> Result<NativeReplacementAbandonEvidence> {
        #[cfg(test)]
        {
            let fixture = self
                .native_abandon_inspector
                .lock()
                .map_err(|_| anyhow::anyhow!("test inspector unavailable"))?
                .clone();
            if let Some(fixture) = fixture {
                return fixture(request, operation, cancel).await;
            }
        }
        // State and credentials come only from the configured current Engine.
        let state = state_root(engine)?;
        let vault = SealedUploadCheckpointVault::new(&state, &engine.account.id)?;
        let checkpoint = vault
            .load(&format!("upload/{operation}"))
            .await?
            .context("native checkpoint unavailable")?;
        let staging = state
            .join("accounts")
            .join(&engine.account.id)
            .join("package-upload-staging");
        let adapter =
            cirrove_icloud::ICloudFileReplace::restore_native_package_from_sealed_checkpoint(
                request.clone(),
                operation,
                cirrove_icloud::ICloudSealedSignIn {
                    apple_id: engine.account.identity.username.clone(),
                    credential_id: engine.account.credential_id.clone(),
                },
                &state,
                &staging,
                &checkpoint,
            )
            .map_err(|_| anyhow::anyhow!("native checkpoint binding refused"))?;
        adapter
            .inspect_native_stage_abandonment(&operation.to_string(), &request, &vault, &cancel)
            .await
            .map_err(|_| anyhow::anyhow!("native Stage evidence unavailable"))
    }
}
fn validate(engine: &Engine, input: &NativeAbandonRequest) -> Result<()> {
    if input.expected_account_id != engine.account.id
        || uuid::Uuid::parse_str(&input.expected_account_id).is_err()
        || input.operation.is_nil()
        || input.label != engine.account.label
    {
        bail!("native abandonment selection changed");
    }
    Ok(())
}
