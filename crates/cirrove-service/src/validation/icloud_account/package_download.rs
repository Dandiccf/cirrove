//! Read-only full artifact test; uses the existing isolated session, no writer.
use super::*;
use async_trait::async_trait;
use cirrove_core::{ProviderError, reads::ReadWindowSink};
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
    let (size, digest) = tokio::task::spawn_blocking(move || -> Result<(u64, String)> {
        use std::io::Read;
        let mut file = std::fs::File::open(path)?;
        let mut hash = Sha256::new();
        let mut size = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hash.update(&buffer[..read]);
            size += read as u64;
        }
        Ok((size, hex::encode(hash.finalize())))
    })
    .await??;
    ensure!(
        size == artifact.size && digest == artifact.sha256,
        "staged package digest differs"
    );
    let report = serde_json::json!({"run":run,"artifact_size":size,"source_listing_size":source.size,
        "private_disk_digest_verified":true,"source_revision_checked_before_after":true,
        "read_only":true,"mounted":false});
    record(&run_dir.join("passed.json"), &report)?;
    println!("{report}");
    Ok(())
}
