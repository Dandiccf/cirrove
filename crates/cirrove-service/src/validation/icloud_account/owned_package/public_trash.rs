//! Read-only verification for a fresh public-service native Trash arm.
//! Neither command starts workers, changes a receipt, or sends a mutation.
use super::super::public_bootstrap::{TRASH_LABEL, source_identity_matches, trash_directory};
use super::public_verify::{exact_entry, named_receipt_binding};
use super::*;
use crate::journal::{MutationState, RecoveryJournal};
use cirrove_core::{mutation::MutationIntent, upload::PackageSemanticIdentity};
const SOURCE: &str = "ac9e5456-bd10-4b7d-9215-21bbb85dde69";
#[derive(serde::Deserialize)]
struct Ready {
    run: Uuid,
    account: String,
    label: String,
    destination: String,
    session_resealed: bool,
    cloud_contacted: bool,
}
#[derive(serde::Serialize, serde::Deserialize)]
struct Selection {
    run: Uuid,
    account: String,
    import: Uuid,
    node: Node,
    semantic: PackageSemanticIdentity,
}
struct Binding {
    dir: PathBuf,
    account: Account,
    plan: OwnedPackagePlan,
    node: Node,
    semantic: PackageSemanticIdentity,
    journal: RecoveryJournal,
}
fn binding(run: Uuid, name: &str, import: Uuid) -> Result<Binding> {
    ensure!(!import.is_nil(), "import operation required");
    let dir = trash_directory(run, name)?;
    check_directory(&dir)?;
    use std::os::unix::fs::MetadataExt;
    ensure!(
        std::fs::symlink_metadata(&dir)?.uid() == std::fs::metadata("/proc/self")?.uid(),
        "validation directory owner changed"
    );
    let ready: Ready = read_json(&dir.join("bootstrap-ready.json"), 16384)?;
    let settings = Settings::load(&dir.join("state"))?;
    ensure!(
        settings.accounts.len() == 1,
        "validation account inventory changed"
    );
    let account = settings.accounts[0].clone();
    ensure!(
        ready.run == run
            && ready.account == account.id
            && ready.label == TRASH_LABEL
            && ready.destination == name
            && ready.session_resealed
            && !ready.cloud_contacted
            && account.label == TRASH_LABEL
            && account.enabled
            && account.access == AccessMode::ReadWrite
            && matches!(account.registration, AppRegistration::ICloud)
            && account.mount_path == dir.join("mount")
            && account.root_id == ROOT_ID,
        "validation account binding changed"
    );
    let (source_dir, old, plan) = retained(Uuid::parse_str(SOURCE)?)?;
    source_identity_matches(&account, &old)?;
    ensure!(
        account.id != old.id && account.credential_id != old.credential_id,
        "validation must use independent credentials"
    );
    let file = private_file(&source_dir.join("source.zip"), LIMIT)?;
    let semantic = cirrove_icloud::package_archive_semantic_identity(
        &file,
        &PackageDownload {
            size: plan.archive_size,
            sha256: plan.archive_sha256.clone(),
        },
        &plan.source.display_name(),
        &CancellationToken::new(),
    )?;
    let journal = RecoveryJournal::open(
        &dir.join("state/accounts").join(&account.id).join("journal"),
        &account.id,
    )
    .context("stop only the isolated validation daemon before read-only verification")?;
    let row = journal.native_validation_upload(import)?;
    let node = named_receipt_binding(&row, &account, &plan, &semantic, name)?.clone();
    Ok(Binding {
        dir,
        account,
        plan,
        node,
        semantic,
        journal,
    })
}
async fn owned_inventory(
    remote: &mut ICloudReadSession,
    plan: &OwnedPackagePlan,
) -> Result<Vec<DriveEntry>> {
    let roots = remote.list_root().await?;
    ensure!(
        roots.iter().filter(|e| e.drivewsid == plan.parent).count() == 1
            && roots.iter().any(|e| e.drivewsid == plan.parent
                && e.kind == "FOLDER"
                && e.parent_id == ROOT_ID
                && e.display_name() == plan.parent_name),
        "owned parent changed"
    );
    let entries = remote.list_folder(&plan.parent).await?;
    ensure!(
        entries
            .iter()
            .filter(|e| e.drivewsid == plan.source.drivewsid)
            .count()
            == 1
            && entries.iter().any(|e| e == &plan.source),
        "owned source changed"
    );
    Ok(entries)
}
fn current_node(entries: &[DriveEntry], receipt: &Node) -> Result<Node> {
    let entry = entries
        .iter()
        .find(|e| e.drivewsid == receipt.id)
        .context("import no longer active")?;
    ensure!(
        !entry.etag.is_empty()
            && entry.etag.len() <= 4096
            && !entry.etag.contains(['\0', '\r', '\n']),
        "invalid current revision"
    );
    ensure!(
        entry.docwsid
            == receipt
                .id
                .strip_prefix("FILE::com.apple.CloudDocs::")
                .context("native identity changed")?,
        "native document identity changed"
    );
    let mut node = receipt.clone();
    node.etag = Some(entry.etag.clone());
    node.content_version = None;
    exact_entry(entries, &node)?;
    Ok(node)
}
fn selection_binding(s: &Selection, b: &Binding, run: Uuid, import: Uuid) -> Result<()> {
    let mut expected = b.node.clone();
    expected.etag = s.node.etag.clone();
    expected.content_version = None;
    ensure!(
        s.run == run
            && s.account == b.account.id
            && s.import == import
            && s.semantic == b.semantic
            && s.node == expected
            && s.node.etag.as_ref().is_some_and(|v| !v.is_empty()
                && v.len() <= 4096
                && !v.contains(['\0', '\r', '\n'])),
        "retained selection binding changed"
    );
    Ok(())
}
pub async fn icloud_public_native_trash_import_verify(
    run: Uuid,
    name: &str,
    import: Uuid,
) -> Result<()> {
    let b = binding(run, name, import)?;
    ensure!(
        !b.dir.join("verified-import-selection.json").try_exists()?,
        "selection already retained; do not replace it"
    );
    let attempt = verification_directory(&b.dir)?;
    manifest(&attempt, run, "public-trash-import-read")?;
    let mut remote = session(&b.dir, &b.account).await?;
    let entries = owned_inventory(&mut remote, &b.plan).await?;
    let node = current_node(&entries, &b.node)?;
    let entry = exact_entry(&entries, &node)?;
    let receipt = download(
        &mut remote,
        &b.plan.parent,
        &entry,
        &attempt.join("imported.zip"),
    )
    .await?;
    let file = private_file(&attempt.join("imported.zip"), LIMIT)?;
    let expected = b.semantic.clone();
    let root = name.to_owned();
    tokio::task::spawn_blocking(move || -> Result<()> {
        ensure!(
            cirrove_icloud::package_archive_semantic_identity(
                &file,
                &receipt,
                &root,
                &CancellationToken::new()
            )? == expected,
            "import semantic contents differ"
        );
        Ok(())
    })
    .await??;
    ensure!(
        exact_entry(&owned_inventory(&mut remote, &b.plan).await?, &node)? == entry,
        "import changed during verification"
    );
    record(
        &b.dir.join("verified-import-selection.json"),
        &Selection {
            run,
            account: b.account.id,
            import,
            node,
            semantic: b.semantic,
        },
    )?;
    println!("Owned public import verified; exact selection retained locally.");
    Ok(())
}
pub async fn icloud_public_native_trash_verify(
    run: Uuid,
    name: &str,
    import: Uuid,
    trash: Uuid,
) -> Result<()> {
    let b = binding(run, name, import)?;
    let selected: Selection = read_json(&b.dir.join("verified-import-selection.json"), 32768)?;
    selection_binding(&selected, &b, run, import)?;
    let row = b.journal.native_validation_mutation(trash)?;
    ensure!(
        trash != import
            && row.request.scope == scope(&b.account)
            && row.request.intent
                == MutationIntent::TrashNativeDocument {
                    before: selected.node.clone()
                }
            && row.state == MutationState::Applied
            && row.base.is_none()
            && row.working_file.is_none()
            && row.receipt.as_ref().is_some_and(|r| row.request.accepts(r))
            && b.journal.native_validation_absence(trash)?,
        "public Trash receipt or publication binding mismatch"
    );
    let attempt = verification_directory(&b.dir)?;
    manifest(&attempt, run, "public-trash-read")?;
    let mut remote = session(&b.dir, &b.account).await?;
    let entries = owned_inventory(&mut remote, &b.plan).await?;
    ensure!(
        !entries.iter().any(|e| e.drivewsid == b.node.id),
        "removed item remains active"
    );
    let request = cirrove_icloud::OwnedPackageTrashRequest {
        apple_account: b.account.identity.username.clone(),
        drive_id: b.node.id.clone(),
        document_id: b
            .node
            .id
            .strip_prefix("FILE::com.apple.CloudDocs::")
            .context("native identity changed")?
            .to_owned(),
        expected_root: name.into(),
        semantic: b.semantic.clone(),
    };
    let staging = private_trash_staging(tempfile::tempfile_in(&attempt)?)?;
    let verified = remote
        .verify_owned_package_in_trash(request, staging, &CancellationToken::new())
        .await?;
    ensure!(
        !owned_inventory(&mut remote, &b.plan)
            .await?
            .iter()
            .any(|e| e.drivewsid == b.node.id),
        "removed item became active during verification"
    );
    record(
        &attempt.join("public-trash-verified.json"),
        &serde_json::json!({"run":run,"account":b.account.id,"import":import,"trash":trash,"item":b.node.id,"semantic":verified.semantic,"archive_size":verified.archive.size,"archive_sha256":verified.archive.sha256,"exact_journal_receipt":true,"metadata_absence_recorded":true,"independent_trash_semantics":true,"cloud_mutation":false,"restore_authorized":false}),
    )?;
    println!("Owned public Trash receipt and independent recoverable package contents verified.");
    Ok(())
}

// Anonymous tempfile creation follows the process umask. Set the contract on
// the descriptor before handing it to the verifier or writing cloud content.
fn private_trash_staging(file: File) -> Result<File> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() == 0 && metadata.nlink() == 0,
        "Trash verification requires empty anonymous staging"
    );
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn trash_verification_staging_establishes_private_mode_independent_of_umask() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let file = tempfile::tempfile().expect("anonymous fixture");
        // Simulate permissive creation explicitly, without changing the global
        // umask while other tests may create files on separate threads.
        file.set_permissions(std::fs::Permissions::from_mode(0o666))
            .expect("fixture mode");
        let secured = private_trash_staging(file).expect("private staging");
        let metadata = secured.metadata().expect("fixture metadata");
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        assert_eq!(metadata.nlink(), 0);
        assert_eq!(metadata.len(), 0);
    }
    fn fixture() -> (Node, DriveEntry) {
        let entry: DriveEntry = serde_json::from_value(serde_json::json!({"drivewsid":"FILE::com.apple.CloudDocs::own","docwsid":"own","zone":"com.apple.CloudDocs","name":"own","extension":"pages","parentId":"FOLDER::com.apple.CloudDocs::parent","etag":"fresh","type":"FILE","size":55})).unwrap();
        let node = Node {
            id: entry.drivewsid.clone(),
            parent_id: Some(entry.parent_id.clone()),
            name: entry.display_name(),
            kind: NodeKind::Folder,
            size: entry.size,
            modified_unix: 0,
            etag: Some("old".into()),
            content_version: None,
            target: None,
            package: true,
        };
        (node, entry)
    }
    #[test]
    fn owned_trash_current_selection_changes_only_revision() {
        let (node, entry) = fixture();
        let selected = current_node(std::slice::from_ref(&entry), &node).unwrap();
        assert_eq!(selected.etag.as_deref(), Some("fresh"));
        assert_eq!(node.etag.as_deref(), Some("old"));
        assert!(exact_entry(&[entry], &node).is_err());
    }
    #[test]
    fn owned_trash_current_selection_rejects_identity_shape_and_duplicate_changes() {
        let (node, entry) = fixture();
        assert!(current_node(&[entry.clone(), entry.clone()], &node).is_err());
        for field in [
            "drivewsid",
            "docwsid",
            "parentId",
            "name",
            "zone",
            "type",
            "etag",
        ] {
            let mut value = serde_json::to_value(&entry).unwrap();
            value[field] = serde_json::json!("");
            let changed = serde_json::from_value(value).unwrap();
            assert!(current_node(&[changed], &node).is_err(), "{field}");
        }
        let mut changed = entry;
        changed.size += 1;
        assert!(current_node(&[changed], &node).is_err());
    }
}
