//! Normal adapter classification and mount, followed by an independent refetch.
use super::*;
use cirrove_core::ReadProvider;
use cirrove_icloud::{DriveEntry, ICloudDrive, ICloudReadSession, PackageDownload};

pub(super) async fn verify(
    run_dir: &Path,
    session_run: Uuid,
    run: Uuid,
    session: ICloudReadSession,
    source: &DriveEntry,
    artifact: &PackageDownload,
) -> Result<()> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local-state")
        .join(format!(
            "icloud-account-mounted-empty-replace-{session_run}"
        ))
        .join("account.json");
    let mut account: Account = serde_json::from_slice(&std::fs::read(path)?)?;
    account.id = run.to_string();
    let (fresh_session, _, _) = super::super::trash_lookup::fixture(session_run).await?;
    verify_bound(BoundNativeVerification {
        run_dir,
        account,
        session,
        fresh_session,
        parent_id: "FOLDER::com.apple.Pages::documents",
        source,
        artifact,
    })
    .await
}

/// All sessions must originate from the supplied isolated account. The caller
/// establishes ownership and independently verifies the canonical receipt.
pub(in crate::validation::icloud_account) struct BoundNativeVerification<'a> {
    pub run_dir: &'a Path,
    pub account: Account,
    pub session: ICloudReadSession,
    pub fresh_session: ICloudReadSession,
    pub parent_id: &'a str,
    pub source: &'a DriveEntry,
    pub artifact: &'a PackageDownload,
}

pub(in crate::validation::icloud_account) async fn verify_bound(
    input: BoundNativeVerification<'_>,
) -> Result<()> {
    let BoundNativeVerification {
        run_dir,
        mut account,
        session,
        fresh_session,
        parent_id,
        source,
        artifact,
    } = input;
    ensure!(
        source.parent_id == parent_id && source.kind == "FILE" && !source.etag.is_empty(),
        "native package source binding unavailable"
    );
    account.enabled = false;
    account.access = AccessMode::ReadOnly;
    account.mount_path = run_dir.join("mount");
    account.root_id = ROOT_ID.into();
    account.drive.id = "drive".into();
    account.poll_seconds = 3600;
    account.cache_bytes = 64 * 1024 * 1024;
    let scope = Scope {
        account: account.id.clone(),
        provider: "icloud".into(),
        collection: "drive".into(),
    };
    let state = run_dir.join("native-state");
    let provider = Arc::new(
        ICloudDrive::on_demand_from_live_session(scope.clone(), session)?
            .with_package_artifacts(&state, account.cache_bytes)?,
    );
    let cancel = CancellationToken::new();
    let root = provider.children(&scope, ROOT_ID, None, &cancel).await?;
    let parent = root
        .nodes
        .into_iter()
        .find(|n| n.id == parent_id)
        .context("native package parent unavailable")?;
    let page = provider.children(&scope, &parent.id, None, &cancel).await?;
    let package = page
        .nodes
        .into_iter()
        .find(|n| n.id == source.drivewsid)
        .context("native package unavailable")?;
    checked_binding(&parent, &package, parent_id, source)?;
    let relative = vec![parent.name, package.name.clone(), package.name];
    mounted::arm(
        account.clone(),
        provider.clone(),
        &state,
        artifact,
        &relative,
    )
    .await?;
    drop(provider);
    let offline = Arc::new(cache::Offline::default());
    mounted::arm(
        account.clone(),
        offline.clone(),
        &state,
        artifact,
        &relative,
    )
    .await?;
    let engine = Engine::new(account.clone(), offline, state.clone()).await?;
    let node = engine
        .node(&scope, &format!("icloud-artifact:{}", source.drivewsid))
        .await?;
    engine.stop().await;
    drop(engine);
    ensure!(
        node.size == artifact.size,
        "native archive published a logical source size"
    );
    // A fresh adapter has neither the first provider's private anonymous file
    // nor its metadata/classification cache. Recover solely from the published
    // node identity, then compare every byte with the independent download.
    let fresh = Arc::new(
        ICloudDrive::on_demand_from_live_session(scope.clone(), fresh_session)?
            .with_package_artifacts(&state, account.cache_bytes)?,
    );
    let read = fresh
        .staged_content_session(&scope, &node, &cancel)
        .await?
        .context("native refetch session unavailable")?;
    let mut hash = Sha256::new();
    let mut offset = 0;
    while offset < node.size {
        let length = (node.size - offset).min(4 * 1024 * 1024) as u32;
        let bytes = read.read_range(offset, length, &cancel).await?;
        ensure!(bytes.len() == length as usize, "native refetch was short");
        hash.update(bytes);
        offset += u64::from(length);
    }
    ensure!(
        hex::encode(hash.finalize()) == artifact.sha256,
        "native refetch digest changed"
    );
    record(&run_dir.join("native-node.json"), &node)?;
    Ok(())
}

fn checked_binding(
    parent: &Node,
    package: &Node,
    parent_id: &str,
    source: &DriveEntry,
) -> Result<()> {
    fn segment(name: &str) -> bool {
        !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\0'])
    }
    ensure!(
        parent.id == parent_id
            && parent.parent_id.as_deref() == Some(ROOT_ID)
            && parent.kind == NodeKind::Folder
            && parent.target.is_none()
            && segment(&parent.name)
            && source.parent_id == parent_id
            && package.id == source.drivewsid
            && package.kind == NodeKind::Folder
            && package.package
            && package.target.is_none()
            && package.parent_id.as_deref() == Some(parent_id)
            && package.name == source.display_name()
            && segment(&package.name)
            && package.size == source.size
            && !source.etag.is_empty()
            && package.etag.as_deref() == Some(source.etag.as_str()),
        "normal adapter did not confirm the bound package revision"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn owned_native_mount_binding_refuses_other_identity_revision_or_escaping_path() {
        let source: DriveEntry = serde_json::from_value(serde_json::json!({
            "drivewsid":"FILE::com.apple.CloudDocs::owned",
            "parentId":"FOLDER::com.apple.CloudDocs::owned-parent",
            "type":"FILE", "name":"Owned", "extension":"pages", "etag":"r1", "size":123
        }))
        .expect("source");
        let parent = Node {
            id: source.parent_id.clone(),
            parent_id: Some(ROOT_ID.into()),
            name: "Owned fixture".into(),
            kind: NodeKind::Folder,
            size: 0,
            modified_unix: 0,
            etag: None,
            content_version: None,
            target: None,
            package: false,
        };
        let package = Node {
            id: source.drivewsid.clone(),
            parent_id: Some(parent.id.clone()),
            name: source.display_name(),
            kind: NodeKind::Folder,
            size: source.size,
            modified_unix: 0,
            etag: Some(source.etag.clone()),
            content_version: None,
            target: None,
            package: true,
        };
        checked_binding(&parent, &package, &parent.id, &source).expect("exact bound package");
        let mut changed = package.clone();
        changed.id = "FILE::com.apple.CloudDocs::other".into();
        assert!(checked_binding(&parent, &changed, &parent.id, &source).is_err());
        changed = package.clone();
        changed.etag = Some("r2".into());
        assert!(checked_binding(&parent, &changed, &parent.id, &source).is_err());
        changed = package.clone();
        changed.size += 1;
        assert!(checked_binding(&parent, &changed, &parent.id, &source).is_err());
        changed = package.clone();
        changed.package = false;
        assert!(checked_binding(&parent, &changed, &parent.id, &source).is_err());
        let mut moved = parent.clone();
        moved.parent_id = Some("other-parent".into());
        assert!(checked_binding(&moved, &package, &parent.id, &source).is_err());
        for name in ["", ".", "..", "/escape", "a/b", "a\0b"] {
            moved = parent.clone();
            moved.name = name.into();
            assert!(checked_binding(&moved, &package, &parent.id, &source).is_err());
        }
    }
}
