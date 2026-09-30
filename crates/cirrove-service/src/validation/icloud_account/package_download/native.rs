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
        .find(|n| n.id == "FOLDER::com.apple.Pages::documents")
        .context("native package parent unavailable")?;
    let page = provider.children(&scope, &parent.id, None, &cancel).await?;
    let package = page
        .nodes
        .into_iter()
        .find(|n| n.id == source.drivewsid)
        .context("native package unavailable")?;
    ensure!(
        package.kind == NodeKind::Folder && package.package,
        "normal adapter did not confirm a package"
    );
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
    let (fresh_session, _, _) = super::super::trash_lookup::fixture(session_run).await?;
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
