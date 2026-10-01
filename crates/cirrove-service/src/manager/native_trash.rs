//! Account- and mount-bound explicit native document removal.
use super::native_import::{eligible, status_allows};
use super::*;
use crate::native_trash::{NativeTrashInput, NativeTrashStatus};
use cirrove_core::mutation::{MutationIntent, MutationReceipt};
impl Manager {
    pub async fn enqueue_native_trash(
        self: &Arc<Self>,
        engine: Arc<Engine>,
        input: NativeTrashInput,
        cancel: CancellationToken,
    ) -> Result<uuid::Uuid> {
        if !crate::native_trash::validate(&input) || engine.account.id != input.expected_account_id
        {
            bail!("native Trash requires the exact selected account and document");
        }
        let permit = self
            .native_import_slots
            .clone()
            .try_acquire_owned()
            .context("another native document action is active")?;
        let control = self.native_import_control(&engine).await?;
        let selection = tokio::select! { biased;
            _ = cancel.cancelled() => bail!("native Trash cancelled before enqueue"),
            _ = engine.cancel.cancelled() => bail!("native Trash account stopped"),
            value = tokio::time::timeout(std::time::Duration::from_secs(60), control.native_trash_selection(&engine, &input, &cancel)) => value.context("native Trash selection check timed out")??,
        };
        let manager = self.clone();
        tokio::task::spawn_blocking(move || -> Result<_> {
            let _permit = permit;
            let status = manager.status.blocking_read();
            let engines = manager.engines.blocking_read();
            let writers = manager.writers.blocking_read();
            if !eligible(&engine)
                || !status_allows(&status, &engine)
                || engine.account.id != input.expected_account_id
                || engines
                    .get(&engine.account.id)
                    .is_none_or(|e| !Arc::ptr_eq(e, &engine))
                || writers
                    .get(&engine.account.id)
                    .is_none_or(|w| !w.same_mount(&control))
                || !control.belongs_to(&engine)
            {
                bail!("native Trash mount changed before enqueue");
            }
            let operation = control.enqueue_native_trash(selection, &cancel)?;
            // Cancellation after durable queue publication cannot discard its ID.
            Ok(operation)
        })
        .await
        .context("native Trash enqueue stopped")?
    }
    /// Only observes the exact operation; never queues, retries or sends provider IO.
    pub async fn native_trash_status(
        &self,
        engine: &Arc<Engine>,
        expected_account_id: &str,
        operation: uuid::Uuid,
    ) -> Result<NativeTrashStatus> {
        if engine.account.id != expected_account_id {
            bail!("native Trash account changed");
        }
        let control = self.native_import_control(engine).await?;
        let record = control.native_trash_record(operation).await?;
        let MutationIntent::TrashNativeDocument { before } = &record.request.intent else {
            bail!("operation is not native Trash");
        };
        if record.request.scope != engine.scope(&engine.account.drive.id) {
            bail!("native Trash operation belongs to another account");
        }
        let removal_confirmed = record.state == crate::journal::MutationState::Applied
            && matches!(&record.receipt, Some(MutationReceipt::Removed { item }) if item == &before.id);
        let metadata_removed = removal_confirmed
            && matches!(
                control.native_trash_publication(operation).await?,
                crate::journal::PackagePublicationStatus::Absent
            );
        self.native_import_same_control(engine, &control).await?;
        Ok(NativeTrashStatus {
            operation,
            state: record.state,
            removal_confirmed,
            metadata_removed,
        })
    }
}

impl Manager {
    /// Local-only historical discovery, also available after reconnecting read-only.
    pub async fn list_native_trash(
        &self,
        engine: &Arc<Engine>,
        expected_account_id: &str,
        after: Option<u64>,
        limit: u32,
    ) -> Result<crate::native_trash::NativeTrashListing> {
        if engine.account.id != expected_account_id
            || !engine.account.enabled
            || engine.cancel.is_cancelled()
            || engine.account.registration.provider_id() != "icloud"
            || !(1..=100).contains(&limit)
        {
            bail!("native Trash listing account or page changed");
        }
        {
            let engines = self.engines.read().await;
            if engines
                .get(expected_account_id)
                .is_none_or(|e| !Arc::ptr_eq(e, engine))
            {
                bail!("native Trash listing account changed");
            }
        }
        let control = self.recovery_control(engine.clone()).await?;
        let page = control
            .native_trash_list(engine.scope(&engine.account.drive.id), after, limit)
            .await?;
        let engines = self.engines.read().await;
        if engine.cancel.is_cancelled()
            || engines
                .get(expected_account_id)
                .is_none_or(|e| !Arc::ptr_eq(e, engine))
        {
            bail!("native Trash listing account changed");
        }
        Ok(page)
    }
}
