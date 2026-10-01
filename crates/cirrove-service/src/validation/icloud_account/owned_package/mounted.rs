//! Read-only mounted, offline, and fresh-adapter verification of the exact
//! checkpoint-owned import. This does not enable native package writes.
use super::*;
use crate::validation::icloud_account::package_download::{BoundNativeVerification, verify_bound};

/// Rechecks ownership and semantic content before exposing an imported package
/// through the normal read-only provider. Every invocation retains its own proof.
pub async fn icloud_owned_package_mounted(run: Uuid) -> Result<()> {
    let (dir, account, plan) = retained(run)?;
    let attempt = verification_directory(&dir)?;
    manifest(&attempt, run, "mounted")?;
    let found =
        OwnedPackageCreate::inspect(session(&dir, &account).await?, &plan, &dir.join("state"))
            .await?;
    let imported = found
        .observed
        .context("allocated package identity not visible; no mutation retried")?;
    let mut remote = session(&dir, &account).await?;
    let entries = remote.list_folder(&plan.parent).await?;
    ensure!(
        entries.len() == 2
            && entries.iter().any(|entry| entry == &plan.source)
            && entries.iter().any(|entry| entry == &imported)
            && imported.parent_id == plan.parent
            && imported.display_name() == plan.destination,
        "owned package source, import or inventory changed"
    );
    let receipt = download(
        &mut remote,
        &plan.parent,
        &imported,
        &attempt.join("imported.zip"),
    )
    .await?;
    let source_receipt = PackageDownload {
        size: plan.archive_size,
        sha256: plan.archive_sha256.clone(),
    };
    let source_root = plan.source.display_name();
    let imported_root = plan.destination.clone();
    let source_file = private_file(&dir.join("source.zip"), LIMIT)?;
    let mut imported_file = private_file(&attempt.join("imported.zip"), LIMIT)?;
    let output = attempt.join("canonical.package");
    let (comparison, canonical) = tokio::task::spawn_blocking(move || -> Result<_> {
        // Verify the retained source digest, independently downloaded import,
        // and both exact ZIP wrapper roots BEFORE normalization or mounting.
        let comparison = compare_package_archives_with_roots(
            &source_file,
            &source_receipt,
            &source_root,
            &imported_file,
            &receipt,
            &imported_root,
            &CancellationToken::new(),
        )?;
        let mut canonical_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(output)?;
        std::io::Seek::rewind(&mut imported_file)?;
        std::io::copy(&mut imported_file, &mut canonical_file)?;
        let canonical =
            cirrove_icloud::canonical_export(&canonical_file, &receipt, &CancellationToken::new())?;
        canonical_file.sync_all()?;
        Ok((comparison, canonical))
    })
    .await??;
    // Both fresh sessions are reconstructed from this exact account's sealed
    // session, never a generic GUI account or the old personal Pages fixture.
    let online = session(&dir, &account).await?;
    let fresh = session(&dir, &account).await?;
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
    record(
        &attempt.join("mounted-verified.json"),
        &serde_json::json!({
            "run":run,
            "allocated_identity_verified":true,
            "source_unchanged":true,
            "semantic_content_verified":true,
            "expected_package_roots_bound":true,
            "entries":comparison.entries,
            "files":comparison.files,
            "expanded_bytes":comparison.expanded_bytes,
            "source_archive_sha256":comparison.source_archive_sha256,
            "imported_archive_sha256":comparison.imported_archive_sha256,
            "canonical_archive_size":canonical.size,
            "canonical_archive_sha256":canonical.sha256,
            "normal_provider_mounted_full_digest_verified":true,
            "offline_remount_full_digest_verified":true,
            "fresh_provider_refetch_full_digest_verified":true,
            "native_ui_open_verified":false,
            "read_only":true
        }),
    )?;
    File::open(&attempt)?.sync_all()?;
    println!(
        "Owned package verified through the normal read-only mount, offline remount and fresh adapter refetch; native application open remains."
    );
    Ok(())
}
