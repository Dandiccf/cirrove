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
        let mut candidates = parent.children.iter().filter(|n| n.name == name);
        let selected = candidates.next().ok_or_else(unavailable)?;
        if candidates.next().is_some()
            || !crate::native_trash::matches(input, selected, &parent.parent.id)
        {
            return Err(unavailable());
        }
        let target = engine
            .refresh_node(&parent.scope, &input.item_id)
            .await
            .map_err(|_| unavailable())?;
        if !crate::native_trash::matches(input, &target, &parent.parent.id)
            || &target != selected
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
}
