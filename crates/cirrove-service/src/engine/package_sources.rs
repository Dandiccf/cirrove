//! Revalidate generated snapshots only after their source actually changes in
//! the local index. This is independent of the optional first-open policy.
use super::*;
use sha2::{Digest, Sha256};

fn fingerprint(node: &Node) -> Result<[u8; 32], ProviderError> {
    let bytes = serde_json::to_vec(&(
        &node.id,
        &node.parent_id,
        &node.name,
        &node.kind,
        node.size,
        node.content_revision(),
        node.package,
        &node.target,
    ))
    .map_err(|_| ProviderError::Unavailable)?;
    Ok(Sha256::digest(bytes).into())
}
impl Engine {
    async fn package_source_state(
        &self,
        scope: &Scope,
        parent: &str,
    ) -> Result<Option<cirrove_store::DirectorySourceState>, ProviderError> {
        let db = self.db.clone();
        let scope = scope.clone();
        let parent = parent.to_owned();
        self.tasks
            .spawn_blocking(move || Store::open(db)?.directory_source_state(&scope, &parent))
            .await
            .map_err(|_| ProviderError::Unavailable)?
            .map_err(|_| ProviderError::Unavailable)
    }
    fn changed_package_source(
        &self,
        state: Option<cirrove_store::DirectorySourceState>,
    ) -> Result<Option<Node>, ProviderError> {
        let Some(state) = state else {
            return Ok(None);
        };
        if !state.changed {
            return Ok(None);
        }
        let opted_in = state
            .bound
            .as_ref()
            .or(state.legacy_classification.as_ref())
            .or(state.current.as_ref())
            .is_some_and(|node| self.provider.retry_package_source_on_version_change(node));
        if !opted_in {
            return Ok(None);
        }
        let current = state.current.ok_or(ProviderError::NotFound)?;
        if !current.package
            || current.kind != cirrove_core::NodeKind::Folder
            || current.target.is_some()
            || current.content_revision().is_none()
            || !self
                .provider
                .retry_package_source_on_version_change(&current)
            || state
                .bound
                .as_ref()
                .or(state.legacy_classification.as_ref())
                .is_some_and(|old| old.id != current.id)
        {
            return Err(ProviderError::VersionChanged);
        }
        Ok(Some(current))
    }
    pub(super) async fn recheck_observed_package_source(
        self: &Arc<Self>,
        scope: &Scope,
        parent: &str,
    ) -> Result<(), ProviderError> {
        if self.cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        if self
            .changed_package_source(self.package_source_state(scope, parent).await?)?
            .is_none()
        {
            return Ok(());
        }
        let key =
            serde_json::to_string(&(scope, parent)).map_err(|_| ProviderError::Unavailable)?;
        let gate = self.directory_gate(&key)?;
        let _guard = tokio::select! {
            biased;
            _ = self.cancel.cancelled() => return Err(ProviderError::Cancelled),
            guard = gate.lock() => guard,
        };
        let Some(current) =
            self.changed_package_source(self.package_source_state(scope, parent).await?)?
        else {
            return Ok(());
        };
        let source = fingerprint(&current)?;
        // Failed online revalidation must not redownload on each repeated READDIR.
        // Activity refresh still retries independently; a new observed source gets
        // a new foreground attempt. The completed old snapshot remains explicit
        // offline fallback, never a new source binding.
        if self
            .package_source_failures
            .lock()
            .map_err(|_| ProviderError::Unavailable)?
            .get(&key)
            == Some(&source)
        {
            return Ok(());
        }
        let result = self.fetch_directory(scope, parent).await;
        self.activity.touch(scope, parent);
        self.activity.observed(
            &crate::activity::DirectoryJob {
                scope: scope.clone(),
                parent: parent.into(),
            },
            &result,
        );
        match result {
            Ok(()) => {
                self.package_source_failures
                    .lock()
                    .map_err(|_| ProviderError::Unavailable)?
                    .remove(&key);
                Ok(())
            }
            Err(ProviderError::Unavailable | ProviderError::Throttled(_)) => {
                // Never hide source disappearance or a second source change
                // behind an availability failure from the previous attempt.
                let state = self.package_source_state(scope, parent).await?;
                if let Some(current) = self.changed_package_source(state)? {
                    if fingerprint(&current)? != source {
                        return Err(ProviderError::VersionChanged);
                    }
                    let mut failed = self
                        .package_source_failures
                        .lock()
                        .map_err(|_| ProviderError::Unavailable)?;
                    if failed.len() >= 256 {
                        failed.clear();
                    }
                    failed.insert(key, source);
                }
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
}
