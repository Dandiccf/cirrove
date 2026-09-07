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
        let headers = {
            let views = self.views.lock().map_err(|_| Errno::EIO)?;
            let mut current = view.inode;
            let mut parent = view.parent;
            let mut visited = std::collections::HashSet::new();
            let mut headers = Vec::new();
            while current != 1 {
                if !visited.insert(current) || visited.len() > 128 {
                    return Err(Errno::ELOOP);
                }
                let header = views.get(&parent).ok_or(Errno::ESTALE)?.clone();
                current = header.inode;
                parent = header.parent;
                headers.push(header);
            }
            headers
        };
        let initial = view.clone();
        let entries = tokio::task::spawn_blocking(move || {
            let mut current = initial;
            let mut entries = Vec::new();
            for header in headers {
                let parent = header.load().map_err(|_| Errno::EIO)?;
                if let Some(entry) = &current.entry {
                    let mut node = entry.as_ref().clone();
                    node.parent_id = Some(parent.node.id.clone());
                    entries.push((parent.scope.as_ref().clone(), node));
                } else if current.node.kind == NodeKind::Folder {
                    entries.push((
                        current.scope.as_ref().clone(),
                        current.node.as_ref().clone(),
                    ));
                }
                current = parent;
            }
            entries.reverse();
            Ok::<_, Errno>(entries)
        })
        .await
        .map_err(|_| Errno::EIO)??;
        writer.capture_ancestors(entries).await
    }
}
