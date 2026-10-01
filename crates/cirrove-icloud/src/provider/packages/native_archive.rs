//! Selected archive resolution only. This module grants no filesystem writes.
use super::*;
use cirrove_core::reads::NativeArchiveBinding;

fn check_archive(node: &Node) -> Outcome<(&str, Revision)> {
    let id = node
        .id
        .strip_prefix(ITEM)
        .ok_or(ProviderError::Permission)?;
    let revision = revision(node)?;
    if !matches!(crate::split_file_id(id), Ok(("com.apple.CloudDocs", _)))
        || node.kind != NodeKind::File
        || node.package
        || node.target.is_some()
        || node.etag.is_some()
        || node.parent_id.as_deref() != Some(id)
        || node.name.is_empty()
        || node.name.len() > 255
        || !node.name.ends_with(".pages")
        || node
            .name
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
        || node.size == 0
        || node.size > 64 * 1024 * 1024
        || node.content_version.as_ref().is_none_or(|v| v.len() > 8192)
        || !revision
            .source_parent
            .starts_with("FOLDER::com.apple.CloudDocs::")
        || revision.source_parent.ends_with("::")
        || revision.source_parent == crate::write_transport::TRASH_ROOT
        || revision.source_etag.len() > 4096
        || revision.source_etag.contains(['*', '\0', '\r', '\n'])
        || revision.source_size > crate::MAX_WRITE_FILE_SIZE
        || !revision
            .sha256
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(ProviderError::Permission);
    }
    Ok((id, revision))
}
fn exact_item(value: serde_json::Value, source: &DriveEntry) -> Outcome<()> {
    if value.get("size").and_then(serde_json::Value::as_u64) != Some(source.size)
        || value.get("restorePath").is_some_and(|v| !v.is_null())
    {
        return Err(ProviderError::VersionChanged);
    }
    let item: DriveEntry =
        serde_json::from_value(value).map_err(|_| ProviderError::VersionChanged)?;
    if item.drivewsid != source.drivewsid
        || item.docwsid != source.docwsid
        || item.zone != source.zone
        || item.kind != "FILE"
        || item.parent_id != source.parent_id
        || item.display_name() != source.display_name()
        || item.size != source.size
        || item.etag != source.etag
    {
        return Err(ProviderError::VersionChanged);
    }
    Ok(())
}
async fn observed_source(
    session: &mut ICloudReadSession,
    id: &str,
    archive: &Node,
    revision: &Revision,
) -> Outcome<DriveEntry> {
    let mut next = revision.source_parent.clone();
    let mut seen = std::collections::HashSet::new();
    let mut selected = None;
    for _ in 0..128 {
        if !next.starts_with("FOLDER::com.apple.CloudDocs::")
            || next.ends_with("::")
            || next == crate::write_transport::TRASH_ROOT
            || !seen.insert(next.clone())
        {
            return Err(ProviderError::Permission);
        }
        let parent = session
            .active_folder_metadata(&next)
            .await
            .map_err(|e| map_read_error(&e))?;
        if parent.drivewsid != next
            || parent.kind != "FOLDER"
            || (parent.zone != "com.apple.CloudDocs"
                && !(next == ROOT_ID && parent.zone.is_empty()))
        {
            return Err(ProviderError::Permission);
        }
        let following = parent.parent_id.clone();
        if selected.is_none() {
            selected = Some(parent);
        }
        if next == ROOT_ID {
            break;
        }
        next = following;
    }
    if next != ROOT_ID {
        return Err(ProviderError::Permission);
    }
    let parent = selected.ok_or(ProviderError::Permission)?;
    let entries: Vec<_> = parent
        .items
        .into_iter()
        .filter(|n| n.drivewsid == id || n.display_name() == archive.name)
        .collect();
    if entries.len() != 1 {
        return Err(ProviderError::VersionChanged);
    }
    let source = entries
        .into_iter()
        .next()
        .ok_or(ProviderError::VersionChanged)?;
    if source.drivewsid != id
        || source.docwsid != id.rsplit("::").next().unwrap_or_default()
        || source.zone != "com.apple.CloudDocs"
        || source.kind != "FILE"
        || source.parent_id != revision.source_parent
        || source.display_name() != archive.name
        || source.etag != revision.source_etag
        || source.size != revision.source_size
    {
        return Err(ProviderError::VersionChanged);
    }
    exact_item(
        session
            .item_details(id)
            .await
            .map_err(|e| map_read_error(&e))?,
        &source,
    )?;
    Ok(source)
}
impl ICloudDrive {
    pub(in crate::provider) async fn resolve_selected_native_archive(
        &self,
        scope: &Scope,
        node: &Node,
        cancel: &CancellationToken,
    ) -> Outcome<Option<NativeArchiveBinding>> {
        self.check_scope(scope)?;
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        if !artifact(node) {
            return Ok(None);
        }
        // Parse only a small complete provider revision, before any network IO.
        if node.content_version.as_ref().is_none_or(|v| v.len() > 8192) {
            return Err(ProviderError::Permission);
        }
        let (id, revision) = check_archive(node)?;
        let packages = self.packages.as_ref().ok_or(ProviderError::Unavailable)?;
        let work = async {
            // Separate from transfer permits: an uncached artifact may need a
            // download while this bounded resolver owns its slot.
            let resolution = packages
                .resolutions
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| ProviderError::Unavailable)?;
            let mut session = self.read_session().await?;
            let source = observed_source(&mut session, id, node, &revision).await?;
            if !matches!(
                session
                    .download_representation(id)
                    .await
                    .map_err(|e| map_read_error(&e))?,
                crate::ContentRepresentation::Package(_)
            ) {
                return Err(ProviderError::Permission);
            }
            // Artifact lookup releases its cache mutex before any await. A
            // refetch validates raw length/hash against this exact archive Node.
            let disk = self.artifact_disk(node, cancel).await?;
            let file = disk.file.clone();
            let receipt = crate::PackageDownload {
                size: node.size,
                sha256: revision.sha256.clone(),
            };
            let expected_root = node.name.clone();
            let token = cancel.clone();
            #[cfg(test)]
            let probe = packages.resolution_probe.lock().await.clone();
            let (semantic, _resolution) = tokio::task::spawn_blocking(move || {
                // The closure owns the permit, including after async timeout or
                // cancellation. Return it to retain the bound through fencing.
                #[cfg(test)]
                if let Some(probe) = probe {
                    probe.pause()?;
                }
                let semantic = crate::package_archive_semantic_identity_versioned(
                    &file,
                    &receipt,
                    &expected_root,
                    2, // Explicit policy for fresh resolution, never a stored proof upgrade.
                    &token,
                )?;
                Ok::<_, ProviderError>((semantic, resolution))
            })
            .await
            .map_err(|_| ProviderError::Unavailable)??;
            // A cached artifact never bypasses fresh source and parent fences.
            let after = observed_source(&mut session, id, node, &revision).await?;
            if after.drivewsid != source.drivewsid
                || after.etag != source.etag
                || after.display_name() != source.display_name()
            {
                return Err(ProviderError::VersionChanged);
            }
            let mut projected = directory_nodes(&revision.source_parent, vec![source])?
                .into_iter()
                .next()
                .ok_or(ProviderError::VersionChanged)?;
            projected.kind = NodeKind::Folder;
            projected.package = true;
            Ok(Some(NativeArchiveBinding {
                scope: scope.clone(),
                archive: node.clone(),
                source: projected,
                semantic,
            }))
        };
        tokio::select! {biased; _=cancel.cancelled()=>Err(ProviderError::Cancelled), result=tokio::time::timeout(Duration::from_secs(360),work)=>result.unwrap_or(Err(ProviderError::Unavailable))}
    }
}
#[cfg(test)]
mod tests;

#[cfg(test)]
#[derive(Default)]
pub(super) struct ParserProbe {
    entered: std::sync::atomic::AtomicUsize,
    notify: tokio::sync::Notify,
    released: std::sync::Mutex<bool>,
    wake: std::sync::Condvar,
}
#[cfg(test)]
impl ParserProbe {
    fn pause(&self) -> Outcome<()> {
        self.entered
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.notify.notify_one();
        let guard = self
            .released
            .lock()
            .map_err(|_| ProviderError::Unavailable)?;
        let (guard, _) = self
            .wake
            .wait_timeout_while(guard, Duration::from_secs(5), |released| !*released)
            .map_err(|_| ProviderError::Unavailable)?;
        if !*guard {
            return Err(ProviderError::Unavailable);
        }
        Ok(())
    }
    fn release(&self) {
        if let Ok(mut released) = self.released.lock() {
            *released = true;
            self.wake.notify_all();
        }
    }
}
