//! Snapshot already traversed ancestors before changing local data or names.
use super::*;
impl Writeback {
    pub fn reference_view(
        &self,
        scope: &Scope,
        provider_item: &str,
    ) -> Result<(Option<String>, bool)> {
        let p = self.projection.lock().map_err(|_| Errno::EIO)?;
        let object = p
            .remote_bindings
            .get(&key(scope, provider_item))
            .and_then(|id| p.objects.get(id));
        if object.is_some_and(|o| o.unlinked) {
            return Err(Errno::ENOENT);
        }
        let local = object.map(|o| o.node.id.clone());
        Ok((local, p.retained().directory(scope, provider_item)))
    }
    pub async fn capture_ancestors(&self, entries: Vec<(Scope, Node)>) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        self.local(move |j| j.capture_namespace_ancestors(entries))
            .await?;
        self.refresh_projection().await?;
        Ok(())
    }
    pub fn retains_directory(&self, scope: &Scope, item: &str) -> Result<bool> {
        Ok(self
            .projection
            .lock()
            .map_err(|_| Errno::EIO)?
            .retained()
            .directory(scope, item))
    }
}
impl Inner {
    pub(in crate::filesystem) async fn capture_ancestors(&self, view: &View) -> Result<()> {
        let Some(writer) = &self.writeback else {
            return Ok(());
        };
        let entries = {
            let views = self.views.lock().map_err(|_| Errno::EIO)?;
            let mut current = view.clone();
            let mut visited = std::collections::HashSet::new();
            let mut entries = Vec::new();
            while current.inode != 1 {
                if !visited.insert(current.inode) || visited.len() > 128 {
                    return Err(Errno::ELOOP);
                }
                let parent = views.get(&current.parent).ok_or(Errno::ESTALE)?;
                if let Some(entry) = &current.entry {
                    let mut node = entry.clone();
                    node.parent_id = Some(parent.node.id.clone());
                    entries.push((parent.scope.clone(), node));
                } else if current.node.kind == NodeKind::Folder {
                    entries.push((current.scope.clone(), current.node.clone()));
                }
                current = parent.clone();
            }
            entries.reverse();
            entries
        };
        writer.capture_ancestors(entries).await
    }
}
