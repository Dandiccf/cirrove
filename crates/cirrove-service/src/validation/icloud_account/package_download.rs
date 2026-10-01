//! Read-only full artifact test; uses the existing isolated session, no writer.
use super::*;
mod cache;
mod mounted;
mod native;
use async_trait::async_trait;
use cirrove_core::{ProviderError, reads::ReadWindowSink};
pub(super) use native::{BoundNativeVerification, verify_bound};
use tokio::io::AsyncWriteExt;

struct DiskSink(tokio::fs::File);
#[async_trait]
impl ReadWindowSink for DiskSink {
    async fn write_chunk(&mut self, bytes: &[u8]) -> std::result::Result<(), ProviderError> {
        if bytes.len() > 64 * 1024 {
            return Err(ProviderError::Protocol("oversized package chunk"));
        }
        self.0
            .write_all(bytes)
            .await
            .map_err(|_| ProviderError::Unavailable)
    }
}

pub async fn icloud_account_package_download(session_run: Uuid, run: Uuid) -> Result<()> {
    run_package(session_run, run, false, false).await
}
pub async fn icloud_account_package_mounted(session_run: Uuid, run: Uuid) -> Result<()> {
    run_package(session_run, run, true, false).await
}
pub async fn icloud_account_package_native(session_run: Uuid, run: Uuid) -> Result<()> {
    run_package(session_run, run, true, true).await
}
async fn run_package(session_run: Uuid, run: Uuid, mount: bool, native: bool) -> Result<()> {
    let (mut session, _, _) = super::trash_lookup::fixture(session_run).await?;
    let parent = "FOLDER::com.apple.Pages::documents";
    let source = session
        .list_folder(parent)
        .await?
        .into_iter()
        .find(|item| item.kind == "FILE" && item.extension == "pages")
        .context("no native Pages sample available")?;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.local-state");
    let run_dir = root.join(format!("icloud-package-artifact-{run}"));
    std::fs::DirBuilder::new().mode(0o700).create(&run_dir)?;
    record(
        &run_dir.join("source.json"),
        &serde_json::json!({
        "drive_id":source.drivewsid,"etag":source.etag,"logical_size":source.size,
        "parent":parent,"session_run":session_run,"run":run}),
    )?;
    let path = run_dir.join("artifact.package");
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    let mut sink = DiskSink(tokio::fs::File::from_std(file));
    let artifact = session
        .download_package(
            parent,
            &source,
            64 * 1024 * 1024,
            &mut sink,
            &CancellationToken::new(),
        )
        .await?;
    sink.0.flush().await?;
    sink.0.sync_all().await?;
    drop(sink);
    let (scope, node, staged) =
        cache::verify_and_cache(&run_dir, run, &source.drivewsid, &artifact).await?;
    if native {
        let input = run_dir.join("artifact.package");
        let output = run_dir.join("canonical.package");
        let expected = cirrove_icloud::PackageDownload {
            size: artifact.size,
            sha256: artifact.sha256.clone(),
        };
        let canonical = tokio::task::spawn_blocking(move || -> Result<_> {
            let mut reader = std::fs::File::open(input)?;
            let mut target = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(output)?;
            std::io::copy(&mut reader, &mut target)?;
            Ok(cirrove_icloud::canonical_export(
                &target,
                &expected,
                &CancellationToken::new(),
            )?)
        })
        .await??;
        native::verify(&run_dir, session_run, run, session, &source, &canonical).await?;
    } else if mount {
        mounted::verify_mount(&run_dir, session_run, run, scope, node, staged, &artifact).await?;
    }
    let size = artifact.size;
    let report = serde_json::json!({"run":run,"artifact_size":size,"source_listing_size":source.size,
        "private_disk_digest_verified":true,"shared_cache_reopened_full_digest_verified":true,"cache_provider_reads":0,"source_revision_checked_before_after":true,
        "read_only":true,"mounted":mount,"offline_remount_full_read":mount,"native_provider":native,"fresh_provider_refetch_verified":native,"canonical_directory_times":native});
    record(&run_dir.join("passed.json"), &report)?;
    println!("{report}");
    Ok(())
}
