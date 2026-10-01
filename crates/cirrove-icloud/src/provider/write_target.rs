//! Classify a selected write target without fetching its content. Filename
//! suffixes cannot prove that a remote FILE is an ordinary byte stream.
use super::*;
use std::{collections::VecDeque, time::Instant};

const CAPACITY: usize = 256;
const MAX_AGE: Duration = Duration::from_secs(30);

#[derive(Default)]
pub(super) struct Cache(VecDeque<(Node, Instant)>);

impl Cache {
    fn matches(&mut self, node: &Node) -> bool {
        self.0.retain(|(old, at)| {
            at.elapsed() < MAX_AGE && (old.id != node.id || same_revision(old, node))
        });
        self.0.iter().any(|(old, _)| same_revision(old, node))
    }

    fn insert(&mut self, node: &Node) {
        self.0.retain(|(old, _)| old.id != node.id);
        while self.0.len() >= CAPACITY {
            self.0.pop_front();
        }
        self.0.push_back((node.clone(), Instant::now()));
    }
}

fn same_revision(a: &Node, b: &Node) -> bool {
    a.id == b.id
        && a.parent_id == b.parent_id
        && a.etag == b.etag
        && a.size == b.size
        && a.name == b.name
        && a.kind == b.kind
        && a.package == b.package
        && a.target == b.target
}

fn exact_revision(value: serde_json::Value, node: &Node) -> Result<(), ProviderError> {
    if value
        .get("size")
        .and_then(serde_json::Value::as_u64)
        .is_none()
    {
        return Err(ProviderError::Protocol("iCloud file has no explicit size"));
    }
    if value.get("restorePath").is_some_and(|v| !v.is_null()) {
        return Err(ProviderError::VersionChanged);
    }
    let item: DriveEntry = serde_json::from_value(value)
        .map_err(|_| ProviderError::Protocol("invalid iCloud write target metadata"))?;
    if item.kind != "FILE"
        || item.drivewsid != node.id
        || Some(item.parent_id.as_str()) != node.parent_id.as_deref()
        || Some(item.etag.as_str()) != node.etag.as_deref()
        || item.size != node.size
        || item.display_name() != node.name
    {
        return Err(ProviderError::VersionChanged);
    }
    Ok(())
}

impl ICloudDrive {
    pub(super) async fn validate_ordinary_write_target(
        &self,
        scope: &Scope,
        node: &Node,
        cancel: &CancellationToken,
    ) -> Result<(), ProviderError> {
        self.check_scope(scope)?;
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        if node.package || node.target.is_some() {
            return Err(ProviderError::Permission);
        }
        // The shared ancestry checks protect app-owned/package directories.
        // Only remote FILE streams require a download-representation lookup.
        if node.kind == NodeKind::Folder {
            return Ok(());
        }
        if node.kind != NodeKind::File || crate::split_file_id(&node.id).is_err()
            || node.etag.as_deref().is_none_or(str::is_empty)
            || node.parent_id.as_deref().is_none_or(|p| {
                let parts: Vec<_> = p.split("::").collect();
                !matches!(parts.as_slice(), ["FOLDER", zone, key] if !zone.is_empty() && !key.is_empty())
            })
        { return Err(ProviderError::Permission); }
        let work = async {
            let _permit = self
                .reads
                .acquire()
                .await
                .map_err(|_| ProviderError::Unavailable)?;
            let mut session = {
                let mut state = self.session.lock().await;
                Self::active_session(&mut state).await?.read_only_fork()
            };
            let before = session
                .item_details(&node.id)
                .await
                .map_err(|e| map_read_error(&e))?;
            exact_revision(before, node)?;
            // A cache hit still requires a fresh exact metadata observation.
            // The cache stores no signed URLs and is local to this account scope.
            let cached = self
                .write_targets
                .lock()
                .map_err(|_| ProviderError::Unavailable)?
                .matches(node);
            if cached {
                return Ok(());
            }
            let representation = session
                .download_representation(&node.id)
                .await
                .map_err(|e| map_read_error(&e))?;
            if !matches!(representation, crate::ContentRepresentation::Data(_)) {
                return Err(ProviderError::Permission);
            }
            let after = session
                .item_details(&node.id)
                .await
                .map_err(|e| map_read_error(&e))?;
            exact_revision(after, node)?;
            self.write_targets
                .lock()
                .map_err(|_| ProviderError::Unavailable)?
                .insert(node);
            Ok(())
        };
        let result = tokio::select! { biased;
            _ = cancel.cancelled() => Err(ProviderError::Cancelled),
            result = tokio::time::timeout(Duration::from_secs(30), work) =>
                result.unwrap_or(Err(ProviderError::Unavailable)),
        };
        if result.is_err()
            && let Ok(mut cache) = self.write_targets.lock()
        {
            cache.0.retain(|(old, _)| old.id != node.id);
        }
        result
    }
}

#[cfg(test)]
mod tests;
