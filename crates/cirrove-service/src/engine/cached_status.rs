//! Status rendering must never hydrate generated exports or ask a provider.
use super::*;

impl Engine {
    pub(crate) async fn status_node(
        self: &Arc<Self>,
        scope: &Scope,
        id: &str,
        cached: bool,
    ) -> Result<Node> {
        if !cached {
            return Ok(self.node(scope, id).await?);
        }
        let (db, scope, id) = (self.db.clone(), scope.clone(), id.to_owned());
        tokio::task::spawn_blocking(move || Store::open(db)?.node(&scope, &id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("metadata is not indexed"))
    }

    pub(crate) async fn status_children(
        self: &Arc<Self>,
        scope: &Scope,
        id: &str,
        cached: bool,
    ) -> Result<Vec<Node>> {
        if !cached {
            return Ok(self.children(scope, id).await?);
        }
        let (db, scope, id) = (self.db.clone(), scope.clone(), id.to_owned());
        // Writable projection needs a complete input view. Refuse exceptionally
        // large views instead of allocating an unbounded directory for a badge.
        tokio::task::spawn_blocking(move || -> Result<Vec<Node>> {
            let store = Store::open(db)?;
            let mut nodes = Vec::new();
            let known = store.visit_children(&scope, &id, |node| {
                if nodes.len() == 10_000 {
                    return Err(cirrove_store::StoreError::DirectoryLimit);
                }
                nodes.push(node);
                Ok(())
            })?;
            anyhow::ensure!(known, "metadata is not indexed");
            Ok(nodes)
        })
        .await?
    }

    pub(crate) async fn cached_path_states(
        self: &Arc<Self>,
        paths: &[String],
    ) -> Result<Vec<crate::PathState>> {
        let (db, root, scope, paths) = (
            self.db.clone(),
            self.account.root_id.clone(),
            self.scope(&self.account.drive.id),
            paths.to_vec(),
        );
        let resolved = tokio::task::spawn_blocking(move || -> Result<Vec<PathResolution>> {
            let store = Store::open(db)?;
            Ok(paths
                .into_iter()
                .map(|path| {
                    let result = cached_resolve(&store, scope.clone(), &root, &path)
                        .map_err(|_| "metadata is not indexed".to_string());
                    (path, result)
                })
                .collect())
        })
        .await??;
        self.path_states_resolved_mode(resolved, true).await
    }
}
fn cached_resolve(
    store: &Store,
    mut scope: Scope,
    root: &str,
    path: &str,
) -> Result<(Scope, Node)> {
    anyhow::ensure!(
        path.len() <= 16384
            && !path.starts_with('/')
            && path.split('/').count() <= 128
            && !path.split('/').any(|p| matches!(p, "." | "..")),
        "invalid path"
    );
    let missing = || anyhow::anyhow!("metadata is not indexed");
    let mut node = store.node(&scope, root)?.ok_or_else(missing)?;
    for name in path.split('/').filter(|s| !s.is_empty()) {
        if let Some(target) = &node.target {
            scope.collection = target.collection.clone();
            node = store.node(&scope, &target.item)?.ok_or_else(missing)?;
        }
        node = store
            .child(&scope, &node.id, name)?
            .flatten()
            .ok_or_else(missing)?;
    }
    if let Some(target) = &node.target {
        scope.collection = target.collection.clone();
        node = store.node(&scope, &target.item)?.ok_or_else(missing)?;
    }
    Ok((scope, node))
}

#[cfg(test)]
mod tests;
