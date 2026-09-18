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
#[derive(Default)]
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
/// Read one file through the mount, whole, and say how long it took.
///
/// Opening a file is what a person does after navigating to one, and on a cloud
/// filesystem it is the part they wait for. The path is resolved through the
/// mount like everything else, so this includes finding the file as well as
/// fetching it -- which is what "I opened a document" actually costs.
async fn timed_read(mount: &Path, relative: &str) -> serde_json::Value {
    let target = mount.join(relative);
    let at = std::time::Instant::now();
    let read = tokio::task::spawn_blocking(move || -> std::io::Result<u64> {
        use std::io::Read;
        let mut file = std::fs::File::open(&target)?;
        let mut buffer = vec![0u8; 1 << 20];
        let mut total = 0u64;
        loop {
            let n = file.read(&mut buffer)?;
            if n == 0 {
                return Ok(total);
            }
            total += n as u64;
        }
    })
    .await;
    let seconds = at.elapsed().as_secs_f64();
    match read {
        Ok(Ok(bytes)) => serde_json::json!({
            "path": relative,
            "bytes": bytes,
            "seconds": seconds,
            "mib_per_s": bytes as f64 / 1024.0 / 1024.0 / seconds.max(0.001),
        }),
        Ok(Err(error)) => {
            serde_json::json!({"path": relative, "failed": error.to_string(), "seconds": seconds})
        }
        Err(_) => serde_json::json!({"path": relative, "failed": "the read task did not finish"}),
    }
}

/// One arm: take `budget` first-visit samples, walking into directories this
/// mount has never listed.
///
/// A budget rather than a duration, because the arms share one tree. The first
/// run of the three-arm shape consumed all 1,548 directories in the collection
/// in its two busy arms and left the control arm two samples, which is not a
/// control. Each arm now takes what it needs and leaves the rest.
#[allow(clippy::too_many_arguments)]
async fn take_arm(
    engine: &Arc<Engine>,
    mount: &Path,
    known: &mut Vec<PathBuf>,
    unvisited: &mut Vec<PathBuf>,
    samples: &mut Samples,
    budget: usize,
    deadline: tokio::time::Instant,
) -> Result<()> {
    while samples.latencies_us.len() + samples.errors < budget {
        if tokio::time::Instant::now() >= deadline {
            return Ok(());
        }
        // Only directories never listed here. Re-listing a visited one measures
        // the directory cache, which is the mistake the first exploratory run
        // of this made.
        let Some(target) = unvisited.pop() else {
            return Ok(());
        };
        let indexing = engine
            .health()
            .await
            .iter()
            .any(|h| h.state == "indexing" || h.state == "updating_or_offline");
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
        // A listing that fails is a navigation failure, not a reason to stop
        // measuring. Recording it keeps a run that could not navigate at all
        // from looking like one with excellent latency.
        match entries {
            Ok(_) => {
                samples.latencies_us.push(took);
                samples.visited += 1;
            }
            Err(_) => samples.errors += 1,
        }
        if indexing {
            samples.while_indexing += 1;
        }
        let _ = mount;
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn onedrive_navigation(
    state: &Path,
    label: &str,
    drive: Option<String>,
    seconds: u64,
    idle_seconds: u64,
    per_arm: usize,
    streams: usize,
    item: Option<String>,
    root_id: Option<String>,
    read_first: Option<String>,
    read_later: Option<String>,
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
        let mut known: Vec<PathBuf> = vec![mount.clone()];
        let mut unvisited: Vec<PathBuf> = vec![mount.clone()];
        let mut alone = Samples::default();
        let mut competing = Samples::default();
        let mut idle = Samples::default();
        let ceiling = tokio::time::Instant::now() + Duration::from_secs(seconds.max(60));

        // Before arm one, while the index is at its emptiest: what a person
        // waits for when they open a file. Registered separately in
        // docs/benchmarks/waiting-for-a-file-during-the-first-index.json.
        let mut during_index = serde_json::Value::Null;
        if let Some(relative) = read_first.clone() {
            during_index = timed_read(&mount, &relative).await;
            event(
                &mut log,
                serde_json::json!({"stage":"content_read_during_index","read":during_index}),
            )?;
        }

        // Arm one: indexing, nothing else.
        take_arm(
            &engine, &mount, &mut known, &mut unvisited, &mut alone, per_arm, ceiling,
        )
        .await?;

        // Arm two: the same, with a transfer competing for the connection.
        let mut downloads: Vec<tokio::task::JoinHandle<()>> = Vec::new();
        // What the competing transfer actually moved. "competing_download_started"
        // is not evidence that anything competed: the first three-arm run
        // reported a ratio of 1.02 against 1.36 and 1.37 from two earlier runs,
        // and nothing in the record could say whether the transfer had stalled
        // or the interference had genuinely gone.
        let moved = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let arm_started = tokio::time::Instant::now();
        if let Some(id) = item.clone() {
            event(
                &mut log,
                serde_json::json!({"stage":"competing_download_started","streams":streams}),
            )?;
            for _ in 0..streams.max(1) {
            let id = id.clone();
            let graph = graph.clone();
            let scope = scope.clone();
            let cancel = cancel.clone();
            let moved = moved.clone();
            downloads.push(tokio::spawn(async move {
                // Round and round until the arm ends. A file that is read once
                // and finishes leaves the rest of the arm measuring a daemon
                // with nothing competing, which is the control arm wearing the
                // label of this one.
                while let Ok(node) = graph.node(&scope, &id, &cancel).await {
                    let mut offset = 0u64;
                    while offset < node.size {
                        match graph
                            .read_range(&scope, &node, offset, 1 << 20, &cancel)
                            .await
                        {
                            Ok(bytes) if !bytes.is_empty() => {
                                offset += bytes.len() as u64;
                                moved.fetch_add(
                                    bytes.len() as u64,
                                    std::sync::atomic::Ordering::Relaxed,
                                );
                            }
                            _ => return,
                        }
                    }
                    if cancel.is_cancelled() {
                        return;
                    }
                }
            }));
            }
        }
        take_arm(
            &engine, &mount, &mut known, &mut unvisited, &mut competing, per_arm, ceiling,
        )
        .await?;
        let competed_for = arm_started.elapsed();
        let competed_bytes = moved.load(std::sync::atomic::Ordering::Relaxed);
        for handle in downloads {
            handle.abort();
        }
        event(
            &mut log,
            serde_json::json!({"stage":"competing_download_finished",
                "bytes": competed_bytes,
                "seconds": competed_for.as_secs_f64(),
                "streams": streams,
                "mib_per_s": competed_bytes as f64 / 1024.0 / 1024.0 / competed_for.as_secs_f64().max(0.001)}),
        )?;

        // Arm three: the control. Everything above measures a daemon doing its
        // own work; without a quiet, fully indexed arm on the same machine, the
        // same network and the same collection, those numbers have nothing to
        // be a ratio of -- which is what left this row open with good figures
        // and no way to fail them.
        let mut settled_after = None;
        let mut settled_read = serde_json::Value::Null;
        if idle_seconds > 0 {
            let waiting_from = tokio::time::Instant::now();
            let deadline = waiting_from + Duration::from_secs(2 * 60 * 60);
            loop {
                let busy = engine
                    .health()
                    .await
                    .iter()
                    .any(|h| h.state == "indexing" || h.state == "updating_or_offline");
                if !busy {
                    settled_after = Some(waiting_from.elapsed());
                    break;
                }
                anyhow::ensure!(
                    tokio::time::Instant::now() < deadline,
                    "the collection never finished indexing, so there is no idle arm to compare against"
                );
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            event(
                &mut log,
                serde_json::json!({"stage":"settled","waited_ms":settled_after.map(|d|d.as_millis())}),
            )?;
            // The same wait, on a settled daemon. A second file rather than the
            // same one: this engine's cache still holds the first, so re-reading
            // it would time the cache and not the daemon.
            if let Some(relative) = read_later.clone() {
                let after = timed_read(&mount, &relative).await;
                event(
                    &mut log,
                    serde_json::json!({"stage":"content_read_when_settled","read":after}),
                )?;
                settled_read = after;
            }
            take_arm(
                &engine,
                &mount,
                &mut known,
                &mut unvisited,
                &mut idle,
                per_arm,
                tokio::time::Instant::now() + Duration::from_secs(idle_seconds),
            )
            .await?;
        }
        Ok::<
            (
                Samples,
                Samples,
                Samples,
                Option<Duration>,
                u64,
                f64,
                serde_json::Value,
                serde_json::Value,
            ),
            anyhow::Error,
        >((
            alone,
            competing,
            idle,
            settled_after,
            competed_bytes,
            competed_for.as_secs_f64(),
            during_index,
            settled_read,
        ))
    }
    .await;
    engine.stop().await;
    if let Some(session) = session {
        tokio::task::spawn_blocking(move || session.umount_and_join()).await??;
    }
    let (
        alone,
        competing,
        idle,
        settled_after,
        competed_bytes,
        competed_seconds,
        during_index,
        settled_read,
    ) = result?;
    let alone_report = alone.report("indexing_only");
    let mut competing_report = competing.report("indexing_and_download");
    if let Some(object) = competing_report.as_object_mut() {
        // Without this the arm's only claim to be competing is its name.
        object.insert("download_bytes".into(), serde_json::json!(competed_bytes));
        object.insert("download_streams".into(), serde_json::json!(streams));
        // The interference measure, and the one two earlier runs produced at
        // 1.36 and 1.37: both arms are provider-bound first visits and differ
        // only in whether something else is pulling bytes. The ratio against
        // the idle arm answers a different question -- where the answer comes
        // from -- because a settled daemon serves a first visit from its own
        // index and never asks the provider at all.
        let (mut theirs, mut ours) = (competing.latencies_us.clone(), alone.latencies_us.clone());
        theirs.sort_unstable();
        ours.sort_unstable();
        let base = percentile(&ours, 0.95) as f64;
        object.insert(
            "download_p95_over_indexing".into(),
            serde_json::json!((base > 0.0).then(|| percentile(&theirs, 0.95) as f64 / base)),
        );
        object.insert(
            "download_mib_per_s".into(),
            serde_json::json!(
                competed_bytes as f64 / 1024.0 / 1024.0 / competed_seconds.max(0.001)
            ),
        );
    }
    event(&mut log, alone_report.clone())?;
    event(&mut log, competing_report.clone())?;
    println!("{}", serde_json::to_string_pretty(&alone_report)?);
    println!("{}", serde_json::to_string_pretty(&competing_report)?);
    if !during_index.is_null() || !settled_read.is_null() {
        let waiting = serde_json::json!({
            "phase": "waiting_for_a_file",
            "during_the_first_index": during_index,
            "when_settled": settled_read,
        });
        event(&mut log, waiting.clone())?;
        println!("{}", serde_json::to_string_pretty(&waiting)?);
    }
    if idle_seconds > 0 {
        let mut idle_report = idle.report("idle");
        if let Some(object) = idle_report.as_object_mut() {
            object.insert(
                "settled_after_ms".into(),
                serde_json::json!(settled_after.map(|d| d.as_millis())),
            );
            // The ratios the row is about, computed here rather than by hand
            // afterwards: interference is what the clause names, and a ratio
            // removes the provider and the network, which this project does
            // not control.
            let ratio = |busy: &Samples| -> Option<f64> {
                let mut theirs = busy.latencies_us.clone();
                let mut ours = idle.latencies_us.clone();
                theirs.sort_unstable();
                ours.sort_unstable();
                let base = percentile(&ours, 0.95) as f64;
                (base > 0.0).then(|| percentile(&theirs, 0.95) as f64 / base)
            };
            object.insert(
                "indexing_p95_over_idle".into(),
                serde_json::json!(ratio(&alone)),
            );
            object.insert(
                "indexing_and_download_p95_over_idle".into(),
                serde_json::json!(ratio(&competing)),
            );
        }
        event(&mut log, idle_report.clone())?;
        println!("{}", serde_json::to_string_pretty(&idle_report)?);
        // Not "while_indexing == 0". A settled daemon still polls: its feed
        // flips to updating_or_offline for the length of every delta request,
        // and that is the ordinary life of a quiet daemon rather than an index
        // still being built. What makes this a control is that the first full
        // pass completed, which `settled_after_ms` records. A majority of the
        // arm caught mid-poll would be something else and is refused.
        anyhow::ensure!(
            settled_after.is_some() && idle.while_indexing * 2 < idle.latencies_us.len().max(1),
            "the idle arm spent most of itself with a feed still working; it is not a control"
        );
    }
    anyhow::ensure!(
        alone.while_indexing > 0,
        "no sample was taken while the collection was still indexing; \
         the run did not measure the condition this box names"
    );
    Ok(())
}
