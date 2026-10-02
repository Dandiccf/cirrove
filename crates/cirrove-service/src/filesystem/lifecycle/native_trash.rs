use super::*;
use crate::native_trash::{NativeTrashAdmission, NativeTrashInput};
fn unavailable() -> std::io::Error {
    std::io::Error::other("native Trash selection is unavailable or changed")
}
impl WriteControl {
    pub(crate) async fn native_trash_selection(
        &self,
        engine: &Arc<Engine>,
        input: &NativeTrashInput,
        cancel: &CancellationToken,
    ) -> std::io::Result<NativeTrashAdmission> {
        let (path, name) = input.path.rsplit_once('/').unwrap_or(("", &input.path));
        let parent = self.native_import_parent(engine, path, cancel).await?;
        let names: Vec<_> = if path.is_empty() {
            vec![]
        } else {
            path.split('/').collect()
        };
        if parent
            .route
            .iter()
            .skip(1)
            .map(|n| n.name.as_str())
            .ne(names)
        {
            return Err(unavailable());
        }
        // Select through the mounted overlay, then map its stable local identity
        // back to the exact provider node. Raw cached membership may still carry
        // the old identity after a completed native handoff.
        let (scope, mut selected) = self
            .resolve_visible_path_mode(engine, &input.path, false)
            .await?;
        if scope != parent.scope || selected.name != name {
            return Err(unavailable());
        }
        // Directory overlays retain their stable local parent identity after
        // creation. Resolve that exact binding before comparing provider nodes;
        // never replace a genuinely different parent with the requested one.
        let selected_parent = selected.parent_id.as_deref().ok_or_else(unavailable)?;
        let provider_parent = self
            .writer
            .directory_identity(&scope, selected_parent)
            .map_err(|_| unavailable())?
            .ok_or_else(unavailable)?;
        if provider_parent != parent.parent.id {
            return Err(unavailable());
        }
        selected.parent_id = Some(provider_parent);
        if !crate::native_trash::matches(input, &selected, &parent.parent.id) {
            return Err(unavailable());
        }
        let target = engine
            .refresh_node(&parent.scope, &input.item_id)
            .await
            .map_err(|_| unavailable())?;
        if !crate::native_trash::matches(input, &target, &parent.parent.id)
            || target != selected
            || cancel.is_cancelled()
        {
            return Err(unavailable());
        }
        self.native_import_open()?;
        Ok(NativeTrashAdmission { parent, target })
    }
    pub(crate) fn enqueue_native_trash(
        &self,
        selection: NativeTrashAdmission,
        cancel: &CancellationToken,
    ) -> std::io::Result<uuid::Uuid> {
        let _admitted = self.inner.edits.admit().map_err(|_| unavailable())?;
        if self.inner.cancel.is_cancelled() || cancel.is_cancelled() {
            return Err(unavailable());
        }
        let id = self
            .writer
            .enqueue_native_trash(selection, cancel)
            .map_err(|e| std::io::Error::from_raw_os_error(e.code()))?;
        self.writer.wake.notify_one();
        self.inner.engine.changed.notify_waiters();
        Ok(id)
    }
    pub(crate) async fn native_trash_record(
        &self,
        id: uuid::Uuid,
    ) -> std::io::Result<crate::journal::MutationRecord> {
        self.writer
            .native_trash_record(id)
            .await
            .map_err(|_| unavailable())
    }
}

impl WriteControl {
    pub(crate) async fn native_trash_list(
        &self,
        scope: Scope,
        after: Option<u64>,
        limit: u32,
    ) -> std::io::Result<crate::native_trash::NativeTrashListing> {
        self.writer
            .native_trash_list(scope, after, limit)
            .await
            .map_err(|_| unavailable())
    }
    pub(crate) async fn native_replacement_list(
        &self,
        scope: Scope,
        after: Option<u64>,
        limit: u32,
    ) -> std::io::Result<crate::journal::NativeReplacementListing> {
        self.writer
            .native_replacement_list(scope, after, limit)
            .await
            .map_err(|_| unavailable())
    }
}
