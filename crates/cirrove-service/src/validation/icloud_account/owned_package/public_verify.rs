//! Read-only verification of one completed public CLI import, from its journal receipt.
use super::*;
use crate::journal::{RecoveryJournal, UploadRecord};
use crate::validation::icloud_account::package_download::{BoundNativeVerification, verify_bound};
use cirrove_core::upload::{PackageSemanticIdentity, UploadRepresentation};

const RUN: &str = "ec7f82e1-3c33-4a1f-bf01-d6e3f2ca80cc";
const SOURCE: &str = "ac9e5456-bd10-4b7d-9215-21bbb85dde69";
const PUBLIC: &str = "/var/tmp/cirrove-public-native-ec7f82e1";
const LABEL: &str = "iCloudPublicNativeValidation";
#[derive(serde::Deserialize)]
struct Ready {
    run: Uuid,
    account: String,
    label: String,
    session_resealed: bool,
    cloud_contacted: bool,
}
fn receipt_binding<'a>(
    row: &'a UploadRecord,
    account: &Account,
    plan: &OwnedPackagePlan,
    semantic: &PackageSemanticIdentity,
) -> Result<&'a Node> {
    let name = format!("Cirrove Public Import {RUN}.pages");
    ensure!(
        row.scope == scope(account)
            && row.state == UploadState::Uploaded
            && row.intent
                == UploadIntent::Create {
                    parent: plan.parent.clone(),
                    name: name.clone()
                }
            && row.representation
                == UploadRepresentation::PackageArchive {
                    expected_root: plan.source.display_name(),
                    semantic: semantic.clone()
                }
            && row.package_completion.as_ref() == Some(semantic)
            && row.size == plan.archive_size
            && row.sha256 == plan.archive_sha256
            && row.base.is_none()
            && row.working_file.is_none(),
        "public package journal receipt binding mismatch"
    );
    let node = row
        .remote
        .as_ref()
        .context("public package receipt missing identity")?;
    ensure!(
        node.kind == NodeKind::Folder
            && node.package
            && node.target.is_none()
            && node.parent_id.as_deref() == Some(plan.parent.as_str())
            && node.name == name
            && node.id.starts_with("FILE::com.apple.CloudDocs::")
            && node.id != plan.source.drivewsid
            && node.etag.as_ref().is_some_and(|v| !v.is_empty())
            // Current source folders have no independent content tag. Accept
            // the exact ETag alias only for already retained older receipts.
            && (node.content_version.is_none() || node.content_version == node.etag),
        "public package receipt identity mismatch"
    );
    Ok(node)
}
fn exact_entry(entries: &[DriveEntry], node: &Node) -> Result<DriveEntry> {
    let mut matches = entries.iter().filter(|entry| entry.drivewsid == node.id);
    let entry = matches
        .next()
        .context("public imported identity not visible")?;
    ensure!(
        matches.next().is_none()
            && entries
                .iter()
                .filter(|entry| entry.display_name() == node.name)
                .count()
                == 1
            && entry.kind == "FILE"
            && entry.zone == "com.apple.CloudDocs"
            && Some(entry.parent_id.as_str()) == node.parent_id.as_deref()
            && entry.display_name() == node.name
            && entry.size == node.size
            && Some(entry.etag.as_str()) == node.etag.as_deref(),
        "public imported metadata changed or ambiguous"
    );
    Ok(entry.clone())
}

/// The public daemon must be stopped first: RecoveryJournal retains its exclusive
/// owner lease throughout the read-only verification and refuses an active writer.
pub async fn icloud_public_native_verify(run: Uuid) -> Result<()> {
    ensure!(
        run.to_string() == RUN,
        "run is not the preregistered public native import"
    );
    let directory = Path::new(PUBLIC);
    check_directory(directory)?;
    use std::os::unix::fs::MetadataExt;
    ensure!(
        std::fs::symlink_metadata(directory)?.uid() == std::fs::metadata("/proc/self")?.uid(),
        "public verification directory must be owned"
    );
    let ready: Ready = read_json(&directory.join("bootstrap-ready.json"), 16 * 1024)?;
    let settings = Settings::load(&directory.join("state"))?;
    ensure!(
        settings.accounts.len() == 1,
        "public account inventory changed"
    );
    let account = settings.accounts[0].clone();
    ensure!(
        ready.run == run
            && ready.account == account.id
            && ready.label == LABEL
            && ready.session_resealed
            && !ready.cloud_contacted
            && account.label == LABEL
            && account.enabled
            && account.access == AccessMode::ReadWrite
            && matches!(account.registration, AppRegistration::ICloud)
            && account.mount_path == directory.join("mount"),
        "public bootstrap binding mismatch"
    );
    let (source_dir, old_account, plan) = retained(Uuid::parse_str(SOURCE)?)?;
    super::super::public_bootstrap::source_identity_matches(&account, &old_account)?;
    ensure!(
        account.id != old_account.id && account.credential_id != old_account.credential_id,
        "public account must not reuse retained source credentials"
    );
    let source_file = private_file(&source_dir.join("source.zip"), LIMIT)?;
    let source_receipt = PackageDownload {
        size: plan.archive_size,
        sha256: plan.archive_sha256.clone(),
    };
    let semantic = cirrove_icloud::package_archive_semantic_identity(
        &source_file,
        &source_receipt,
        &plan.source.display_name(),
        &CancellationToken::new(),
    )?;
    let journal = RecoveryJournal::open(
        &directory
            .join("state/accounts")
            .join(&account.id)
            .join("journal"),
        &account.id,
    )
    .context("stop only the isolated public daemon before verification")?;
    let rows = journal.list(0, 2)?;
    ensure!(
        rows.len() == 1,
        "expected exactly one public import journal row"
    );
    let row = &rows[0];
    let node = receipt_binding(row, &account, &plan, &semantic)?.clone();
    let attempt = verification_directory(directory)?;
    manifest(&attempt, run, "public-mounted")?;
    let mut remote = session(directory, &account).await?;
    let roots = remote.list_root().await?;
    ensure!(
        roots
            .iter()
            .filter(|entry| entry.drivewsid == plan.parent)
            .count()
            == 1
            && roots.iter().any(|entry| entry.drivewsid == plan.parent
                && entry.kind == "FOLDER"
                && entry.parent_id == ROOT_ID
                && entry.display_name() == plan.parent_name),
        "owned public import parent changed"
    );
    let entries = remote.list_folder(&plan.parent).await?;
    ensure!(
        entries
            .iter()
            .filter(|entry| entry.drivewsid == plan.source.drivewsid)
            .count()
            == 1
            && entries.iter().any(|entry| entry == &plan.source),
        "retained synthetic source changed"
    );
    let imported = exact_entry(&entries, &node)?;
    let receipt = download(
        &mut remote,
        &plan.parent,
        &imported,
        &attempt.join("imported.zip"),
    )
    .await?;
    ensure!(
        exact_entry(&remote.list_folder(&plan.parent).await?, &node)? == imported,
        "public package revision changed during download"
    );
    let imported_file = private_file(&attempt.join("imported.zip"), LIMIT)?;
    let source_root = plan.source.display_name();
    let destination_root = node.name.clone();
    let output = attempt.join("canonical.package");
    let (comparison, canonical) = tokio::task::spawn_blocking(move || -> Result<_> {
        let comparison = compare_package_archives_with_roots(
            &source_file,
            &source_receipt,
            &source_root,
            &imported_file,
            &receipt,
            &destination_root,
            &CancellationToken::new(),
        )?;
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(output)?;
        let mut imported_file = imported_file;
        std::io::Seek::rewind(&mut imported_file)?;
        std::io::copy(&mut imported_file, &mut file)?;
        let canonical =
            cirrove_icloud::canonical_export(&file, &receipt, &CancellationToken::new())?;
        file.sync_all()?;
        Ok((comparison, canonical))
    })
    .await??;
    let online = session(directory, &account).await?;
    let fresh = session(directory, &account).await?;
    verify_bound(BoundNativeVerification {
        run_dir: &attempt,
        account,
        session: online,
        fresh_session: fresh,
        parent_id: &plan.parent,
        source: &imported,
        artifact: &canonical,
    })
    .await?;
    let final_entries = remote.list_folder(&plan.parent).await?;
    ensure!(
        exact_entry(&final_entries, &node)? == imported
            && final_entries
                .iter()
                .filter(|entry| entry.drivewsid == plan.source.drivewsid)
                .count()
                == 1
            && final_entries.iter().any(|entry| entry == &plan.source),
        "public package or source changed during mounted verification"
    );
    record(
        &attempt.join("public-mounted-verified.json"),
        &serde_json::json!({
            "run": run, "operation": row.id, "scope": row.scope, "remote": node,
            "journal_receipt_bound": true, "semantic": semantic,
            "entries": comparison.entries, "files": comparison.files,
            "expanded_bytes": comparison.expanded_bytes,
            "source_archive_sha256": comparison.source_archive_sha256,
            "imported_archive_sha256": comparison.imported_archive_sha256,
            "canonical_archive_sha256": canonical.sha256,
            "normal_fuse": true, "offline_remount": true, "fresh_refetch": true,
            "native_ui_open_verified": false, "read_only": true
        }),
    )?;
    File::open(&attempt)?.sync_all()?;
    drop(journal);
    println!(
        "Public import receipt, semantic content, normal FUSE, offline remount and fresh refetch verified."
    );
    Ok(())
}

#[cfg(test)]
mod tests;
