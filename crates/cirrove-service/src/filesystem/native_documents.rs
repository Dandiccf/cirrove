//! A confirmed iWork package is one mounted document. The provider's protected
//! container and generated archive remain separate internal identities.
use super::*;

impl View {
    pub(super) fn native_document(&self) -> bool {
        !self.native_archive_directories
            && self.scope.provider == "icloud"
            && self.scope.collection == "drive"
            && self.package
            && self.kind == NodeKind::Folder
            && !self.reference
            && self.alias.is_empty()
            && (self.id.starts_with("FILE::com.apple.CloudDocs::")
                || self
                    .id
                    .strip_prefix("local-native-")
                    .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok()))
            && cirrove_core::upload::native_package_suffix(&self.name).is_some()
    }
}

impl Inner {
    pub(super) async fn document_archive(&self, source: &View) -> Result<View, ProviderError> {
        if !source.native_document() {
            return Ok(source.clone());
        }
        let expected = self.node(source).await?;
        if self.writeback.is_none() {
            let current = self.engine.node(&source.scope, &source.id).await?;
            if current.content_revision() != expected.content_revision()
                || current.size != expected.size
            {
                return Err(ProviderError::VersionChanged);
            }
        }
        let mut nodes = self.children(source).await?.into_iter().filter(|node| {
            node.kind == NodeKind::File
                && !node.package
                && node.target.is_none()
                && node.name == source.name.as_ref()
        });
        let archive = nodes.next().ok_or(ProviderError::Unavailable)?;
        if nodes.next().is_some() {
            return Err(ProviderError::VersionChanged);
        }
        self.insert(source, archive).await
    }

    pub(super) async fn unlink_document(
        self: &Arc<Self>,
        parent: &View,
        source: &Node,
    ) -> Result<(), Errno> {
        let control = CloudFs {
            inner: self.clone(),
        }
        .write_control()
        .map_err(|_| Errno::EROFS)?;
        let writer = self.writeback.as_ref().ok_or(Errno::EROFS)?;
        let item_id = writer
            .remote_identity(&parent.scope, &source.id)?
            .unwrap_or_else(|| source.id.clone());
        let mut parts = vec![source.name.clone()];
        let mut route = parent.clone();
        let mut seen = std::collections::HashSet::new();
        while route.inode != ROOT_INODE {
            if route.reference
                || route.package
                || !route.alias.is_empty()
                || route.scope != parent.scope
                || !seen.insert(route.inode)
                || parts.len() > 32
            {
                return Err(Errno::EOPNOTSUPP);
            }
            parts.push(route.name.to_string());
            route = self.view(route.parent).map_err(|e| errno(&e))?;
        }
        parts.reverse();
        let input = crate::native_trash::NativeTrashInput {
            expected_account_id: self.engine.account.id.clone(),
            path: parts.join("/"),
            item_id,
            etag: source.etag.clone().ok_or(Errno::ESTALE)?,
        };
        if !crate::native_trash::validate(&input) {
            return Err(Errno::EOPNOTSUPP);
        }
        let selection = tokio::time::timeout(
            Duration::from_secs(60),
            control.native_trash_selection(&self.engine, &input, &self.cancel),
        )
        .await
        .map_err(|_| Errno::ETIMEDOUT)?
        .map_err(|_| Errno::ESTALE)?;
        control
            .enqueue_native_trash(selection, &self.cancel)
            .map_err(|_| Errno::EBUSY)?;
        Ok(())
    }
}
