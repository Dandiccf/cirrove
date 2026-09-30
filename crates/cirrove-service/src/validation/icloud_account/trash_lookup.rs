use super::*;

/// Read-only diagnostic confined to the two known predecessors of a successful
/// mounted empty/refill arm. No writer, mount or mutation adapter is constructed.
pub(super) async fn fixture(run: Uuid) -> Result<(ICloudReadSession, Vec<String>, Node)> {
    let run_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local-state")
        .join(format!("icloud-account-mounted-empty-replace-{run}"));
    let passed: serde_json::Value =
        serde_json::from_slice(&std::fs::read(run_dir.join("passed.json"))?)?;
    ensure!(
        passed["run"] == run.to_string()
            && passed["both_predecessors_in_trash"] == true
            && passed["remounted_read"] == true,
        "source arm has not passed"
    );
    let account: Account = serde_json::from_slice(&std::fs::read(run_dir.join("account.json"))?)?;
    let parent: Node = serde_json::from_slice(&std::fs::read(run_dir.join("owned-folder.json"))?)?;
    ensure!(
        matches!(account.registration, AppRegistration::ICloud)
            && !account.enabled
            && parent.name == format!("Cirrove Write Validation-{run}")
            && parent.parent_id.as_deref() == Some(ROOT_ID),
        "invalid owned fixture"
    );
    let mut ids = Vec::new();
    for (file, size) in [("original.json", FIRST.len() as u64), ("empty.json", 0)] {
        let node: Node = serde_json::from_slice(&std::fs::read(run_dir.join(file))?)?;
        ensure!(
            node.parent_id.as_ref() == Some(&parent.id)
                && node.name == NAME
                && node.kind == NodeKind::File
                && node.size == size
                && !node.package
                && node.target.is_none(),
            "invalid owned predecessor"
        );
        ids.push(node.id);
    }
    let state = run_dir.join("state");
    let snapshot = SealedSessionVault::new(&state, &account.id)?
        .load(&account.credential_id)
        .await?
        .context("isolated session unavailable")?;
    let session = ICloudReadSession::from_session_snapshot(&snapshot, &account.identity.username)?;
    let active: Node = serde_json::from_slice(&std::fs::read(run_dir.join("refilled.json"))?)?;
    ensure!(
        active.parent_id.as_ref() == Some(&parent.id)
            && active.name == NAME
            && active.size == SECOND.len() as u64
            && active.kind == NodeKind::File
            && !active.package
            && active.target.is_none(),
        "invalid active owned file"
    );
    Ok((session, ids, active))
}

pub async fn icloud_account_trash_lookup(run: Uuid) -> Result<()> {
    let (mut session, ids, _) = fixture(run).await?;
    for arm in 0..3 {
        let report = session.compare_trash_lookup(&ids).await?;
        println!("{}", serde_json::json!({"arm":arm,"report":report}));
        ensure!(
            report["selected_metadata_stable"] == true,
            "owned metadata changed during comparison"
        );
    }
    Ok(())
}

pub async fn icloud_account_trash_lookup_control(run: Uuid) -> Result<()> {
    let (mut session, ids, active) = fixture(run).await?;
    let report = session
        .compare_trash_lookup_envelopes(&ids, &active)
        .await?;
    println!("{}", serde_json::json!({"control":report}));
    ensure!(
        report["selected_metadata_stable"] == true,
        "owned metadata changed during control"
    );
    Ok(())
}

/// Uses an existing isolated session; only metadata shapes leave the adapter.
pub async fn icloud_account_metadata_shapes(run: Uuid) -> Result<()> {
    let (mut session, _, _) = fixture(run).await?;
    println!("{}", session.document_metadata_shapes().await?);
    Ok(())
}

/// Fresh-provider lookup: no index, writer, mount, or mutation request.
pub async fn icloud_account_cold_node(run: Uuid) -> Result<()> {
    use cirrove_core::{ProviderError, ReadProvider};
    let (session, predecessors, active) = fixture(run).await?;
    let scope = Scope {
        account: run.to_string(),
        provider: "icloud".into(),
        collection: "drive".into(),
    };
    let provider = ICloudDrive::on_demand_from_live_session(scope.clone(), session)?;
    let cancel = CancellationToken::new();
    let node = provider.node(&scope, &active.id, &cancel).await?;
    ensure!(
        node.id == active.id
            && node.parent_id == active.parent_id
            && node.name == active.name
            && node.size == active.size
            && node.etag == active.etag
            && node.kind == NodeKind::File
            && !node.package,
        "cold lookup disagrees with owned receipt"
    );
    for id in predecessors {
        ensure!(
            matches!(
                provider.node(&scope, &id, &cancel).await,
                Err(ProviderError::NotFound)
            ),
            "cold lookup did not exclude owned Trash predecessor"
        );
    }
    println!("cold_active_receipt_match=true cold_trash_predecessors_excluded=2");
    Ok(())
}
