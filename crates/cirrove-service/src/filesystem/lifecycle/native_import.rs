use super::*;
use crate::native_import::{ImportParent, ValidatedPackageArchive};

fn unavailable() -> std::io::Error {
    std::io::Error::other("native import destination is unavailable or changed")
}
fn plain(node: &Node) -> bool {
    node.kind == NodeKind::Folder
        && !node.package
        && node.target.is_none()
        && node.id.starts_with("FOLDER::com.apple.CloudDocs::")
}
impl WriteControl {
    pub(crate) async fn native_import_publication(
        &self,
        id: uuid::Uuid,
    ) -> std::io::Result<crate::journal::PackagePublicationStatus> {
        self.writer
            .native_import_publication(id)
            .await
            .map_err(|_| unavailable())
    }

    pub(crate) fn same_mount(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
    pub(crate) fn native_import_open(&self) -> std::io::Result<()> {
        let _admitted = self.inner.edits.admit().map_err(|_| unavailable())?;
        if self.inner.cancel.is_cancelled() {
            return Err(unavailable());
        }
        Ok(())
    }
    pub(crate) async fn native_import_parent(
        &self,
        engine: &Arc<Engine>,
        path: &str,
        cancel: &CancellationToken,
    ) -> std::io::Result<ImportParent> {
        self.native_import_open()?;
        if !self.belongs_to(engine) || cancel.is_cancelled() {
            return Err(unavailable());
        }
        let parts: Vec<_> = if path.is_empty() {
            Vec::new()
        } else {
            path.split('/').collect()
        };
        if parts.len() > 32
            || parts
                .iter()
                .any(|p| p.is_empty() || *p == "." || *p == ".." || p.contains('\0'))
        {
            return Err(unavailable());
        }
        let scope = engine.scope(&engine.account.drive.id);
        let frontier = self
            .writer
            .native_import_frontier()
            .await
            .map_err(|_| unavailable())?;
        let mut node = engine
            .refresh_node(&scope, &engine.account.root_id)
            .await
            .map_err(|_| unavailable())?;
        if !plain(&node) {
            return Err(unavailable());
        }
        let mut route = vec![node.clone()];
        for name in parts {
            if cancel.is_cancelled() {
                return Err(unavailable());
            }
            engine
                .fetch_directory(&scope, &node.id)
                .await
                .map_err(|_| unavailable())?;
            let children = engine
                .children(&scope, &node.id)
                .await
                .map_err(|_| unavailable())?;
            let children = self
                .writer
                .overlay(&scope, &node.id, children)
                .map_err(|_| unavailable())?;
            let child = children
                .into_iter()
                .find(|n| n.name == name)
                .ok_or_else(unavailable)?;
            if child.kind != NodeKind::Folder || child.package || child.target.is_some() {
                return Err(unavailable());
            }
            let remote = self
                .writer
                .directory_identity(&scope, &child.id)
                .map_err(|_| unavailable())?
                .ok_or_else(unavailable)?;
            let observed = engine
                .refresh_node(&scope, &remote)
                .await
                .map_err(|_| unavailable())?;
            if !plain(&observed) || observed.parent_id.as_deref() != Some(&node.id) {
                return Err(unavailable());
            }
            node = observed;
            route.push(node.clone());
        }
        // Browsing deliberately serves cached children. Admission needs a new
        // complete provider observation: another client may have removed or
        // occupied the destination since that snapshot was published.
        engine
            .fetch_directory(&scope, &node.id)
            .await
            .map_err(|_| unavailable())?;
        let children = engine
            .children(&scope, &node.id)
            .await
            .map_err(|_| unavailable())?;
        if self
            .writer
            .native_import_frontier()
            .await
            .map_err(|_| unavailable())?
            != frontier
        {
            return Err(unavailable());
        }
        self.native_import_open()?;
        Ok(ImportParent {
            scope,
            route,
            parent: node,
            children,
            frontier,
        })
    }
    /// Blocking publication. Caller holds manager registry leases and slot here.
    pub(crate) fn enqueue_native_import(
        &self,
        parent: ImportParent,
        name: String,
        archive: ValidatedPackageArchive,
        cancel: &CancellationToken,
    ) -> std::io::Result<crate::journal::UploadRecord> {
        let _admitted = self.inner.edits.admit().map_err(|_| unavailable())?;
        if self.inner.cancel.is_cancelled() || cancel.is_cancelled() {
            return Err(unavailable());
        }
        let record = self
            .writer
            .enqueue_native_import(parent, name, archive, cancel)
            .map_err(|error| std::io::Error::from_raw_os_error(error.code()))?;
        self.writer.wake.notify_one();
        self.inner.engine.changed.notify_waiters();
        Ok(record)
    }
    pub(crate) async fn native_import_record(
        &self,
        id: uuid::Uuid,
    ) -> std::io::Result<crate::journal::UploadRecord> {
        self.writer
            .native_import_record(id)
            .await
            .map_err(|_| unavailable())
    }
}

impl WriteControl {
    pub(crate) async fn native_import_list(
        &self,
        scope: Scope,
        after: Option<u64>,
        limit: u32,
    ) -> std::io::Result<crate::journal::NativeImportListing> {
        self.writer
            .native_import_list(scope, after, limit)
            .await
            .map_err(|_| unavailable())
    }
}
