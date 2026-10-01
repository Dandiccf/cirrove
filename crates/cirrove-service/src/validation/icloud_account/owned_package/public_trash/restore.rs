//! Single-use feature-only restore of the fresh public-service Trash fixture.
//! This is not general restore admission or evidence of collision atomicity.
use super::*;
use cirrove_core::{
    Scope,
    mutation::{MutationReceipt, MutationReconciliation},
};
use cirrove_icloud::{ICloudNativeRestore, NativeRestoreRequest};
const RUN: &str = "f2dec2e2-b870-4f1f-bfb5-e6d63f008941";
const IMPORT: &str = "c52350d0-8b9d-4278-8c49-578adcf94c8f";
const TRASH: &str = "b715c3e2-702e-475d-ad6d-3fa5feff3253";
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Started {
    version: u8,
    run: Uuid,
    account: String,
    import: Uuid,
    trash: Uuid,
    operation: Uuid,
    attempt: Uuid,
    selected: Node,
    semantic: PackageSemanticIdentity,
}
fn exact_run(run: Uuid) -> Result<(Uuid, Uuid, String)> {
    ensure!(
        run.to_string() == RUN,
        "native restore is restricted to the fresh public Trash run"
    );
    Ok((
        Uuid::parse_str(IMPORT)?,
        Uuid::parse_str(TRASH)?,
        format!("Cirrove Public Trash {run}.pages"),
    ))
}
fn retained_binding(run: Uuid) -> Result<(Binding, Selection, Uuid, Uuid)> {
    let (import, trash, name) = exact_run(run)?;
    let b = binding(run, &name, import)?;
    let selected: Selection = read_json(&b.dir.join("verified-import-selection.json"), 32768)?;
    selection_binding(&selected, &b, run, import)?;
    require_applied_trash(&b, &selected, import, trash)?;
    Ok((b, selected, import, trash))
}
fn attempt_id(path: &Path) -> Result<Uuid> {
    Uuid::parse_str(
        path.file_name()
            .and_then(|p| p.to_str())
            .and_then(|p| p.strip_prefix("verify-"))
            .context("invalid private restore attempt")?,
    )
    .context("invalid private restore attempt")
}
fn claim(dir: &Path, started: &Started) -> Result<()> {
    record(&dir.join("native-restore-started.json"), started)?;
    File::open(dir)?.sync_all()?;
    Ok(())
}
fn check_started(
    s: &Started,
    b: &Binding,
    selected: &Selection,
    run: Uuid,
    import: Uuid,
    trash: Uuid,
) -> Result<()> {
    ensure!(
        s.version == 1
            && s.run == run
            && s.account == b.account.id
            && s.import == import
            && s.trash == trash
            && !s.operation.is_nil()
            && s.operation != trash
            && s.operation != import
            && !s.attempt.is_nil()
            && s.selected == selected.node
            && s.semantic == b.semantic,
        "native restore retained operation binding changed"
    );
    Ok(())
}
fn route_from_owned_parent(parent: &DriveEntry, plan: &OwnedPackagePlan) -> Result<Vec<Node>> {
    ensure!(
        parent.drivewsid == plan.parent
            && parent.parent_id == ROOT_ID
            && parent.kind == "FOLDER"
            && parent.zone == "com.apple.CloudDocs"
            && parent.display_name() == plan.parent_name
            && !parent.etag.is_empty()
            && parent.etag.len() <= 4096
            && !parent.etag.chars().any(char::is_control)
            && !parent.etag.contains('*'),
        "owned restore parent identity changed"
    );
    Ok(vec![
        Node {
            id: ROOT_ID.into(),
            parent_id: None,
            name: "iCloud Drive".into(),
            kind: NodeKind::Folder,
            size: 0,
            modified_unix: 0,
            etag: None,
            content_version: None,
            target: None,
            package: false,
        },
        Node {
            id: parent.drivewsid.clone(),
            parent_id: Some(ROOT_ID.into()),
            name: parent.display_name(),
            kind: NodeKind::Folder,
            size: parent.size,
            modified_unix: 0,
            etag: Some(parent.etag.clone()),
            content_version: None,
            target: None,
            package: false,
        },
    ])
}
fn check_request(
    request: &NativeRestoreRequest,
    started: &Started,
    expected_scope: &Scope,
    parent: &str,
    parent_name: &str,
) -> Result<()> {
    ensure!(
        request.scope == *expected_scope
            && request.removal_operation == started.trash
            && request.before == started.selected
            && request.parent_route.len() == 2
            && request.parent_route[0].id == ROOT_ID
            && request.parent_route[0].parent_id.is_none()
            && request.parent_route[1].id == parent
            && request.parent_route[1].name == parent_name
            && request.parent_route[1].parent_id.as_deref() == Some(ROOT_ID)
            && request.before.parent_id.as_deref() == Some(parent),
        "native restore retained request changed"
    );
    Ok(())
}
fn adapter(
    b: &Binding,
    request: NativeRestoreRequest,
    attempt: &Path,
) -> Result<ICloudNativeRestore> {
    Ok(ICloudNativeRestore::from_sealed_session(
        b.account.identity.username.clone(),
        b.account.credential_id.clone(),
        &b.dir.join("state"),
        request,
        attempt.into(),
    )?)
}
fn confirmed_node(node: &Node, started: &Started) -> Result<()> {
    ensure!(
        node.id == started.selected.id
            && node.parent_id == started.selected.parent_id
            && node.kind == NodeKind::Folder
            && node.package
            && node.target.is_none()
            && node.name.ends_with(".pages")
            && node.etag.as_ref().is_some_and(|s| !s.is_empty()),
        "restore observation does not match selected native identity"
    );
    Ok(())
}
pub async fn icloud_public_native_restore(run: Uuid) -> Result<()> {
    let (b, selected, import, trash) = retained_binding(run)?;
    let attempt = verification_directory(&b.dir)?;
    let started = Started {
        version: 1,
        run,
        account: b.account.id.clone(),
        import,
        trash,
        operation: Uuid::new_v4(),
        attempt: attempt_id(&attempt)?,
        selected: selected.node.clone(),
        semantic: b.semantic.clone(),
    };
    // This permanent create-new marker is installed before session/provider IO.
    // Any error thereafter permits only the distinct inspection command.
    claim(&b.dir, &started)?;
    manifest(&attempt, run, "restore")?;
    let mut remote = session(&b.dir, &b.account).await?;
    let roots = remote.list_root().await?;
    let parents: Vec<_> = roots
        .iter()
        .filter(|e| e.drivewsid == b.plan.parent)
        .collect();
    ensure!(parents.len() == 1, "owned restore parent is ambiguous");
    let route = route_from_owned_parent(parents[0], &b.plan)?;
    let entries = owned_inventory(&mut remote, &b.plan).await?;
    ensure!(
        !entries.iter().any(|e| e.drivewsid == selected.node.id
            || e.display_name().eq_ignore_ascii_case(&selected.node.name)),
        "owned restore destination is occupied"
    );
    let proof = remote
        .verify_owned_package_in_trash(
            cirrove_icloud::OwnedPackageTrashRequest {
                apple_account: b.account.identity.username.clone(),
                drive_id: selected.node.id.clone(),
                document_id: selected
                    .node
                    .id
                    .strip_prefix("FILE::com.apple.CloudDocs::")
                    .context("native identity changed")?
                    .into(),
                expected_root: selected.node.name.clone(),
                semantic: b.semantic.clone(),
            },
            private_trash_staging(tempfile::tempfile_in(&attempt)?)?,
            &CancellationToken::new(),
        )
        .await?;
    ensure!(
        proof.semantic == started.semantic,
        "owned restore source semantic proof changed"
    );
    let request = NativeRestoreRequest {
        scope: scope(&b.account),
        removal_operation: trash,
        before: selected.node,
        parent_route: route,
    };
    check_request(
        &request,
        &started,
        &scope(&b.account),
        &b.plan.parent,
        &b.plan.parent_name,
    )?;
    record(&attempt.join("request.json"), &request)?;
    File::open(&attempt)?.sync_all()?;
    let restore = adapter(&b, request, &attempt)?;
    let cancel = CancellationToken::new();
    restore.prepare(started.operation, &cancel).await?;
    record(
        &attempt.join("prepared.json"),
        &serde_json::json!({"operation":started.operation,"current_trash_semantics_verified":true}),
    )?;
    File::open(&attempt)?.sync_all()?;
    record(
        &attempt.join("execute-armed.json"),
        &serde_json::json!({"operation":started.operation,"single_dispatch_only":true,"automatic_retry":false}),
    )?;
    File::open(&attempt)?.sync_all()?;
    let node = restore.execute(started.operation, &cancel).await?;
    confirmed_node(&node, &started)?;
    record(
        &attempt.join("restore-confirmed.json"),
        &serde_json::json!({"run":run,"operation":started.operation,"trash":trash,"restored":node,"same_identity_and_semantics":true,"general_restore_enabled":false,"destination_vacancy_atomicity_proven":false}),
    )?;
    File::open(&attempt)?.sync_all()?;
    println!("Fresh owned native restore verified; general restore remains disabled.");
    Ok(())
}
pub async fn icloud_public_native_restore_inspect(run: Uuid) -> Result<()> {
    let (b, selected, import, trash) = retained_binding(run)?;
    let started: Started = read_json(&b.dir.join("native-restore-started.json"), 32768)?;
    check_started(&started, &b, &selected, run, import, trash)?;
    let original = b.dir.join(format!("verify-{}", started.attempt));
    check_directory(&original)?;
    let request: NativeRestoreRequest = read_json(&original.join("request.json"), 96 * 1024)?;
    check_request(
        &request,
        &started,
        &scope(&b.account),
        &b.plan.parent,
        &b.plan.parent_name,
    )?;
    let attempt = verification_directory(&b.dir)?;
    manifest(&attempt, run, "native-restore-inspect")?;
    let restore = adapter(&b, request, &attempt)?;
    let result = restore
        .reconcile(started.operation, &CancellationToken::new())
        .await?;
    let (status, node) = match result {
        MutationReconciliation::Applied(MutationReceipt::Upsert(node)) => {
            confirmed_node(&node, &started)?;
            ("same_identity_and_semantics", Some(node))
        }
        MutationReconciliation::Uncommitted => ("prepared_before_dispatch", None),
        MutationReconciliation::Indeterminate => ("uncertain_no_replay", None),
        _ => anyhow::bail!("unexpected native restore reconciliation receipt"),
    };
    record(
        &attempt.join("restore-inspection.json"),
        &serde_json::json!({"run":run,"operation":started.operation,"status":status,"restored":node,"cloud_mutation":false,"automatic_retry":false,"destination_vacancy_atomicity_proven":false}),
    )?;
    File::open(&attempt)?.sync_all()?;
    println!("Owned native restore inspection: {status}; no mutation submitted.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Started, NativeRestoreRequest) {
        let root = Node {
            id: ROOT_ID.into(),
            parent_id: None,
            name: "iCloud Drive".into(),
            kind: NodeKind::Folder,
            size: 0,
            modified_unix: 0,
            etag: None,
            content_version: None,
            target: None,
            package: false,
        };
        let parent = Node {
            id: "FOLDER::com.apple.CloudDocs::owned".into(),
            parent_id: Some(ROOT_ID.into()),
            name: "Owned".into(),
            etag: Some("parent-e1".into()),
            ..root.clone()
        };
        let node = Node {
            id: "FILE::com.apple.CloudDocs::selected".into(),
            parent_id: Some(parent.id.clone()),
            name: "Owned.pages".into(),
            etag: Some("e1".into()),
            package: true,
            ..root.clone()
        };
        let started = Started {
            version: 1,
            run: Uuid::parse_str(RUN).unwrap(),
            account: "account".into(),
            import: Uuid::parse_str(IMPORT).unwrap(),
            trash: Uuid::parse_str(TRASH).unwrap(),
            operation: Uuid::new_v4(),
            attempt: Uuid::new_v4(),
            selected: node.clone(),
            semantic: PackageSemanticIdentity {
                version: 1,
                sha256: "a".repeat(64),
                entries: 1,
                files: 1,
                expanded_bytes: 1,
            },
        };
        let request = NativeRestoreRequest {
            scope: Scope {
                account: "account".into(),
                provider: "icloud".into(),
                collection: ROOT_ID.into(),
            },
            removal_operation: started.trash,
            before: node,
            parent_route: vec![root, parent],
        };
        (started, request)
    }
    #[test]
    fn public_native_restore_claim_is_single_use_and_durable() {
        let dir = tempfile::tempdir().unwrap();
        let (started, _) = fixture();
        claim(dir.path(), &started).unwrap();
        let saved = std::fs::read(dir.path().join("native-restore-started.json")).unwrap();
        assert!(claim(dir.path(), &started).is_err());
        assert_eq!(
            std::fs::read(dir.path().join("native-restore-started.json")).unwrap(),
            saved
        );
        let parsed: Started = serde_json::from_slice(&saved).unwrap();
        assert_eq!(parsed.operation, started.operation);
        assert_eq!(parsed.attempt, started.attempt);
    }
    #[test]
    fn public_native_restore_request_rejects_retargeted_identity_and_route() {
        let (started, request) = fixture();
        let expected = request.scope.clone();
        let check = |r: &NativeRestoreRequest| {
            check_request(
                r,
                &started,
                &expected,
                "FOLDER::com.apple.CloudDocs::owned",
                "Owned",
            )
        };
        check(&request).unwrap();
        for field in 0..6 {
            let mut changed = request.clone();
            match field {
                0 => changed.scope.account = "other".into(),
                1 => changed.removal_operation = Uuid::new_v4(),
                2 => changed.before.id = "FILE::com.apple.CloudDocs::other".into(),
                3 => changed.before.etag = Some("e2".into()),
                4 => changed.parent_route[1].id = "FOLDER::com.apple.CloudDocs::other".into(),
                _ => changed.parent_route[1].name = "Other".into(),
            }
            assert!(
                check(&changed).is_err(),
                "retargeted field {field} accepted"
            );
        }
    }
    #[test]
    fn public_native_restore_excludes_prior_fixture_and_changed_receipt() {
        assert!(exact_run(Uuid::parse_str(RUN).unwrap()).is_ok());
        assert!(
            exact_run(Uuid::parse_str("97be33d2-b216-49bf-9e49-465b1ca85d1d").unwrap()).is_err()
        );
        assert!(exact_run(Uuid::new_v4()).is_err());
        let (started, request) = fixture();
        confirmed_node(&request.before, &started).unwrap();
        let mut wrong = request.before;
        wrong.id = "FILE::com.apple.CloudDocs::other".into();
        assert!(confirmed_node(&wrong, &started).is_err());
        assert!(attempt_id(Path::new("verify-invalid")).is_err());
    }
}
