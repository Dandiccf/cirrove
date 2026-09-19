use crate::accounts;
use anyhow::{Context, Result, bail};
use cirrove_auth::AppRegistration;
use cirrove_core::{CancellationToken, Node, NodeKind, ProviderError, ReadProvider, Scope};
use rusqlite::{Connection, OpenFlags, params};
use serde::Serialize;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

const MAX_FILE_BYTES: u64 = 1024 * 1024;
const READ_CHUNK: u32 = 4 * 1024 * 1024;

#[derive(Default, Serialize)]
struct GoogleReadReport {
    indexed_items: usize,
    compared_files: usize,
    compared_bytes: u64,
    changed_files: usize,
    unavailable_files: usize,
    shortcut_targets_checked: usize,
    shortcut_targets_available: usize,
    shortcut_targets_missing: usize,
    shortcut_targets_outside_my_drive: usize,
    shortcut_targets_unavailable: usize,
}

/// Compare bounded live adapter reads with the mounted view and classify shortcut
/// targets. This is GET-only and deliberately emits aggregates rather than cloud
/// names, identities, hashes or response bodies.
pub async fn google_read(
    state: &Path,
    label: &str,
    max_files: usize,
    max_shortcuts: usize,
) -> Result<()> {
    if max_files == 0 || max_files > 32 || max_shortcuts > 256 {
        bail!("use 1..=32 files and at most 256 shortcut targets");
    }
    let account = accounts::Settings::load(state)?
        .accounts
        .into_iter()
        .find(|account| account.label == label)
        .context("unknown account")?;
    if !matches!(account.registration, AppRegistration::Google { .. }) {
        bail!("this check requires a Google Drive connection");
    }
    let scope = Scope {
        account: account.id.clone(),
        provider: account.registration.provider_id().into(),
        collection: account.drive.id.clone(),
    };
    let database = state.join("accounts").join(&account.id).join("metadata.db");
    let key = serde_json::to_string(&scope)?;
    let nodes = tokio::task::spawn_blocking(move || -> Result<Vec<Node>> {
        let db = Connection::open_with_flags(
            database,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let mut query = db.prepare("SELECT body FROM nodes WHERE scope=?1 ORDER BY id")?;
        let rows = query.query_map(params![key], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    })
    .await??;
    let provider = accounts::provider(&account)?;
    let by_id: HashMap<_, _> = nodes.iter().map(|node| (node.id.as_str(), node)).collect();
    let mut report = GoogleReadReport {
        indexed_items: nodes.len(),
        ..Default::default()
    };
    let cancel = CancellationToken::new();

    let mut candidates: Vec<_> = nodes
        .iter()
        .filter(|node| {
            node.kind == NodeKind::File
                && node.target.is_none()
                && node.size > 0
                && node.size <= MAX_FILE_BYTES
                && !node.name.ends_with(".url")
                && node.content_revision().is_some()
        })
        .filter_map(|node| path_for(&by_id, &scope.collection, node).map(|path| (node, path)))
        .collect();
    candidates.sort_by_key(|(node, _)| (node.size, node.id.as_str()));
    for (node, relative) in candidates.into_iter().take(max_files) {
        match direct_bytes(provider.clone(), &scope, node, &cancel).await {
            Ok(direct) => {
                let mounted = tokio::time::timeout(
                    Duration::from_secs(30),
                    tokio::fs::read(account.mount_path.join(relative)),
                )
                .await;
                match mounted {
                    Ok(Ok(mounted)) if mounted == direct => {
                        report.compared_files += 1;
                        report.compared_bytes += direct.len() as u64;
                    }
                    Ok(Ok(_)) => report.changed_files += 1,
                    _ => report.unavailable_files += 1,
                }
            }
            Err(ProviderError::VersionChanged | ProviderError::NotFound) => {
                report.changed_files += 1;
            }
            Err(_) => report.unavailable_files += 1,
        }
    }

    let mut targets: Vec<_> = nodes
        .iter()
        .filter_map(|node| node.target.as_deref())
        .collect();
    targets.sort_by_key(|target| (&target.collection, &target.item));
    targets.dedup_by(|a, b| a.collection == b.collection && a.item == b.item);
    for target in targets.into_iter().take(max_shortcuts) {
        report.shortcut_targets_checked += 1;
        let target_scope = Scope {
            collection: target.collection.clone(),
            ..scope.clone()
        };
        match provider.node(&target_scope, &target.item, &cancel).await {
            Ok(_) => report.shortcut_targets_available += 1,
            Err(ProviderError::NotFound) => report.shortcut_targets_missing += 1,
            Err(ProviderError::Permission) => report.shortcut_targets_outside_my_drive += 1,
            Err(_) => report.shortcut_targets_unavailable += 1,
        }
    }

    println!("{}", serde_json::to_string_pretty(&report)?);
    if report.compared_files == 0 {
        bail!("no bounded file could be compared through both live read paths");
    }
    Ok(())
}

async fn direct_bytes(
    provider: Arc<dyn ReadProvider>,
    scope: &Scope,
    node: &Node,
    cancel: &CancellationToken,
) -> std::result::Result<Vec<u8>, ProviderError> {
    let mut bytes = Vec::with_capacity(node.size as usize);
    let mut offset = 0;
    while offset < node.size {
        let part = provider
            .read_range(scope, node, offset, READ_CHUNK, cancel)
            .await?;
        if part.is_empty() {
            return Err(ProviderError::Protocol(
                "Google read ended before file size",
            ));
        }
        offset += part.len() as u64;
        bytes.extend_from_slice(&part);
    }
    Ok(bytes)
}

fn path_for(by_id: &HashMap<&str, &Node>, root: &str, node: &Node) -> Option<PathBuf> {
    let mut parts = vec![node.name.as_str()];
    let mut parent = node.parent_id.as_deref()?;
    for _ in 0..256 {
        if parent == root {
            let mut path = PathBuf::new();
            for part in parts.into_iter().rev() {
                if part.is_empty() || matches!(part, "." | "..") || part.contains(['/', '\0']) {
                    return None;
                }
                path.push(part);
            }
            return Some(path);
        }
        let directory = *by_id.get(parent)?;
        if directory.kind != NodeKind::Folder {
            return None;
        }
        parts.push(directory.name.as_str());
        parent = directory.parent_id.as_deref()?;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, parent: Option<&str>, name: &str, kind: NodeKind) -> Node {
        Node {
            id: id.into(),
            parent_id: parent.map(str::to_owned),
            name: name.into(),
            kind,
            size: 1,
            modified_unix: 0,
            etag: None,
            content_version: Some("google-version:1".into()),
            target: None,
            package: false,
        }
    }

    #[test]
    fn indexed_paths_are_bounded_and_never_accept_path_components() {
        let folder = node("folder", Some("root"), "Folder", NodeKind::Folder);
        let file = node("file", Some("folder"), "File [id].txt", NodeKind::File);
        let nodes = [&folder, &file];
        let by_id = nodes.iter().map(|node| (node.id.as_str(), *node)).collect();
        assert_eq!(
            path_for(&by_id, "root", &file).expect("indexed ancestry should resolve"),
            PathBuf::from("Folder/File [id].txt")
        );
        let invalid = node("bad", Some("root"), "../escape", NodeKind::File);
        assert!(path_for(&by_id, "root", &invalid).is_none());
        let orphan = node("orphan", Some("missing"), "file", NodeKind::File);
        assert!(path_for(&by_id, "root", &orphan).is_none());
    }
}
