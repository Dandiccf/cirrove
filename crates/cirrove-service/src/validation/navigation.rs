//! Developer fixture for navigation latency while a large library is being
//! indexed for the first time, and while a download competes with it.
//!
//! The existing evidence for this is fixtures: cached navigation stays under
//! 500 ms while committed changes are applied. That is a claim about a fixture's
//! library. This one mounts a real collection cold, so the index really is being
//! built from nothing while the directory is listed, and it records the health
//! state beside every sample so a run that finished indexing early cannot be
//! mistaken for one that measured the condition the box names.
use super::*;
use crate::{engine::Engine, filesystem::CloudFs};
use cirrove_core::ReadProvider;
use std::path::PathBuf;

fn percentile(sorted: &[u128], p: f64) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[rank]
}
struct Samples {
    latencies_us: Vec<u128>,
    while_indexing: usize,
    errors: usize,
    visited: usize,
}
impl Samples {
    fn report(&self, name: &str) -> serde_json::Value {
        let mut sorted = self.latencies_us.clone();
        sorted.sort_unstable();
        serde_json::json!({
            "phase": name,
            "samples": sorted.len(),
            "while_indexing": self.while_indexing,
            "p50_ms": percentile(&sorted, 0.50) as f64 / 1000.0,
            "p95_ms": percentile(&sorted, 0.95) as f64 / 1000.0,
            "max_ms": sorted.last().copied().unwrap_or(0) as f64 / 1000.0,
            "over_500ms": sorted.iter().filter(|us| **us > 500_000).count(),
            "failed_listings": self.errors,
            "directories_visited": self.visited,
        })
    }
}
pub async fn onedrive_navigation(
    state: &Path,
    label: &str,
    drive: Option<String>,
    seconds: u64,
    item: Option<String>,
    root_id: Option<String>,
) -> Result<()> {
    let account = read_only_account(state, label)?;
    let run = uuid::Uuid::new_v4();
    let directory = state.join("navigation-checks").join(run.to_string());
    crate::private_dir(&directory)?;
    let mut log = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(directory.join("events.jsonl"))?;
    let collection = drive.unwrap_or_else(|| account.drive.id.clone());
    let scope = Scope {
        account: account.id.clone(),
        provider: "onedrive".into(),
        collection: collection.clone(),
    };
    let cancel = CancellationToken::new();
    let _cancel_on_return = cancel.clone().drop_guard();
    let graph = accounts::provider(&account)?;
    // The account's own root id does not exist inside a linked collection, and
    // an engine given one it cannot resolve never serves a listing at all. When a
    // collection is named, its root has to be named with it.
    let root_id = root_id.unwrap_or_else(|| account.root_id.clone());
    let root = graph
        .node(&scope, &root_id, &cancel)
        .await
        .context("root item is not reachable in the named collection")?;

    let mut config = account.clone();
    config.drive.id = collection.clone();
    config.root_id = root.id.clone();
    config.mount_path = directory.join("mount");
    crate::private_dir(&config.mount_path)?;
    // A fresh engine directory is the point: the index is built from nothing
    // while the navigation loop below is already listing the directory.
    let engine = Engine::new(config, graph.clone(), directory.join("engine")).await?;
    let mount = engine.account.mount_path.clone();
    event(
        &mut log,
        serde_json::json!({"stage":"planned","collection":collection,"seconds":seconds,"competing_download":item.is_some()}),
    )?;
    println!(
        "Cold mount of a real collection; private evidence: {}",
        directory.display()
    );

    let mut session = None;
    let result = async {
        engine.start().await?;
        let fs = CloudFs::new(engine.clone())?;
        let at = mount.clone();
        session = Some(tokio::task::spawn_blocking(move || fs.mount(&at)).await??);
        let started = tokio::time::Instant::now();
        let competing_at = Duration::from_secs(seconds / 3);
        let mut alone = Samples {
            latencies_us: Vec::new(),
            while_indexing: 0,
            errors: 0,
            visited: 0,
        };
        let mut competing = Samples {
            latencies_us: Vec::new(),
            while_indexing: 0,
            errors: 0,
            visited: 0,
        };
        // A cold mount is not listable the instant it is mounted. How long that
        // takes is worth a number of its own, and sampling before it would
        // measure mount setup rather than navigation.
        let first_listing = {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
            loop {
                let target = mount.clone();
                let ok = tokio::task::spawn_blocking(move || std::fs::read_dir(&target).is_ok())
                    .await
                    .unwrap_or(false);
                if ok {
                    break started.elapsed();
                }
                anyhow::ensure!(
                    tokio::time::Instant::now() < deadline,
                    "cold mount never became listable"
                );
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        };
        event(
            &mut log,
            serde_json::json!({"stage":"first_listing","after_mount_ms":first_listing.as_millis()}),
        )?;
        let mut download: Option<tokio::task::JoinHandle<()>> = None;
        let mut known: Vec<PathBuf> = vec![mount.clone()];
        let mut unvisited: Vec<PathBuf> = vec![mount.clone()];
        let mut downloaded_bytes = 0u64;
        while started.elapsed() < Duration::from_secs(seconds) {
            if download.is_none()
                && started.elapsed() >= competing_at
                && let Some(id) = item.clone()
            {
                {
                    let graph = graph.clone();
                    let scope = scope.clone();
                    let cancel = cancel.clone();
                    event(
                        &mut log,
                        serde_json::json!({"stage":"competing_download_started"}),
                    )?;
                    download = Some(tokio::spawn(async move {
                        if let Ok(node) = graph.node(&scope, &id, &cancel).await {
                            let mut offset = 0u64;
                            while offset < node.size {
                                match graph
                                    .read_range(&scope, &node, offset, 1 << 20, &cancel)
                                    .await
                                {
                                    Ok(bytes) if !bytes.is_empty() => offset += bytes.len() as u64,
                                    _ => break,
                                }
                            }
                        }
                    }));
                    downloaded_bytes = 1;
                }
            }
            let indexing = engine
                .health()
                .await
                .iter()
                .any(|h| h.state == "indexing" || h.state == "updating_or_offline");
            // Walk into directories rather than re-listing the root. A root that
            // is listed every 250 ms stays active and therefore warm, which would
            // measure a hot directory rather than navigation. Each sample takes a
            // directory discovered so far, preferring ones not visited yet.
            let target = match unvisited.pop() {
                Some(next) => next,
                None => {
                    known.rotate_left(1);
                    known.first().cloned().unwrap_or_else(|| mount.clone())
                }
            };
            let listing = target.clone();
            let at = std::time::Instant::now();
            let entries = tokio::task::spawn_blocking(move || -> std::io::Result<Vec<PathBuf>> {
                let mut directories = Vec::new();
                for entry in std::fs::read_dir(&listing)? {
                    let entry = entry?;
                    if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        directories.push(entry.path());
                    }
                }
                Ok(directories)
            })
            .await?;
            let took = at.elapsed().as_micros();
            if let Ok(found) = &entries {
                for directory in found {
                    if !known.contains(directory) {
                        known.push(directory.clone());
                        unvisited.push(directory.clone());
                    }
                }
            }
            let bucket = if download.is_some() {
                &mut competing
            } else {
                &mut alone
            };
            // A listing that fails is a navigation failure, not a reason to stop
            // measuring. Recording it keeps a run that could not navigate at all
            // from looking like one with excellent latency.
            match entries {
                Ok(_) => {
                    bucket.latencies_us.push(took);
                    bucket.visited += 1;
                }
                Err(_) => bucket.errors += 1,
            }
            if indexing {
                bucket.while_indexing += 1;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        if let Some(handle) = download {
            handle.abort();
        }
        let _ = downloaded_bytes;
        Ok::<(Samples, Samples), anyhow::Error>((alone, competing))
    }
    .await;
    engine.stop().await;
    if let Some(session) = session {
        tokio::task::spawn_blocking(move || session.umount_and_join()).await??;
    }
    let (alone, competing) = result?;
    let alone_report = alone.report("indexing_only");
    let competing_report = competing.report("indexing_and_download");
    event(&mut log, alone_report.clone())?;
    event(&mut log, competing_report.clone())?;
    println!("{}", serde_json::to_string_pretty(&alone_report)?);
    println!("{}", serde_json::to_string_pretty(&competing_report)?);
    anyhow::ensure!(
        alone.while_indexing > 0,
        "no sample was taken while the collection was still indexing; \
         the run did not measure the condition this box names"
    );
    Ok(())
}
