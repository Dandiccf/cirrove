//! Developer fixture for the two things the read-session acceptance box still
//! wanted after its counters were read off a live mount: that the optimized path
//! serves the same bytes as the conservative one *through a mount*, and that the
//! Graph metadata requests attributable to a read actually fall when it is on.
//!
//! Both arms mount the same file with a fresh engine directory, so each starts
//! with an empty content cache and neither can be served blocks the other
//! fetched. Counters are sampled around the read alone, so the figure compared
//! is the read's own traffic rather than the adapter's lifetime total.
use super::*;
use crate::{engine::Engine, filesystem::CloudFs};
use cirrove_core::ReadProvider;
use sha2::{Digest, Sha256};
use std::io::Read;

struct ArmResult {
    sha256: String,
    bytes: u64,
    graph_gets: u64,
    content_gets: u64,
    setups: u64,
    conditional_windows: u64,
    seconds: f64,
}
#[allow(clippy::too_many_arguments)]
async fn arm(
    directory: &Path,
    account: &accounts::Account,
    collection: &str,
    parent: &Node,
    file: &Node,
    optimized: bool,
) -> Result<ArmResult> {
    let name = if optimized {
        "optimized"
    } else {
        "conservative"
    };
    // A second adapter, built for this arm alone, so its counters describe this
    // arm and the daemon's own traffic cannot leak into them.
    let graph = if optimized {
        accounts::experimental_read_provider(account)?
    } else {
        accounts::provider(account)?
    };
    let mut config = account.clone();
    // Most of this account's content lives in a linked collection rather than in
    // its own drive, so the arm mounts whichever collection holds the file.
    config.drive.id = collection.to_string();
    config.root_id = parent.id.clone();
    config.mount_path = directory.join(format!("mount-{name}"));
    config.poll_seconds = 3600;
    crate::private_dir(&config.mount_path)?;
    let engine = Engine::new(
        config,
        graph.clone(),
        // A fresh engine directory is what makes this arm cold. Sharing one would
        // let the second arm read the first arm's cached blocks and report a read
        // that never reached the provider at all.
        directory.join(format!("engine-{name}")),
    )
    .await?;
    let mut session = None;
    let mount = engine.account.mount_path.clone();
    let target = mount.join(&file.name);
    let result = async {
        engine.start().await?;
        while !engine.health().await.iter().any(|h| h.state == "ready") {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let fs = CloudFs::new(engine.clone())?;
        let at = mount.clone();
        session = Some(tokio::task::spawn_blocking(move || fs.mount(&at)).await??);
        while !tokio::fs::try_exists(&target).await.unwrap_or(false) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let before = graph.read_counters();
        let started = std::time::Instant::now();
        let path = target.clone();
        let (digest, bytes) = tokio::task::spawn_blocking(move || -> Result<(String, u64)> {
            let mut handle = std::fs::File::open(&path)?;
            let mut hash = Sha256::new();
            let mut buffer = vec![0u8; 1 << 20];
            let mut total = 0u64;
            loop {
                let read = handle.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                hash.update(&buffer[..read]);
                total += read as u64;
            }
            Ok((hex::encode(hash.finalize()), total))
        })
        .await??;
        let seconds = started.elapsed().as_secs_f64();
        let after = graph.read_counters();
        Ok::<ArmResult, anyhow::Error>(ArmResult {
            sha256: digest,
            bytes,
            graph_gets: after.graph_get_attempts - before.graph_get_attempts,
            content_gets: after.content_get_attempts - before.content_get_attempts,
            setups: after.setups - before.setups,
            conditional_windows: after.conditional_windows - before.conditional_windows,
            seconds,
        })
    }
    .await;
    engine.stop().await;
    if let Some(session) = session {
        tokio::task::spawn_blocking(move || session.umount_and_join()).await??;
    }
    result
}
pub async fn onedrive_read_bytes(
    state: &Path,
    label: &str,
    item: &str,
    drive: Option<String>,
) -> Result<()> {
    let account = read_only_account(state, label)?;
    let run = uuid::Uuid::new_v4();
    let directory = state.join("read-byte-checks").join(run.to_string());
    crate::private_dir(&directory)?;
    let mut log = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(directory.join("events.jsonl"))?;
    let scope = Scope {
        account: account.id.clone(),
        provider: "onedrive".into(),
        collection: drive.unwrap_or_else(|| account.drive.id.clone()),
    };
    let cancel = CancellationToken::new();
    let _cancel_on_return = cancel.clone().drop_guard();
    let graph = accounts::provider(&account)?;
    let file = graph.node(&scope, item, &cancel).await?;
    let parent = graph
        .node(
            &scope,
            file.parent_id.as_deref().context("item has no parent")?,
            &cancel,
        )
        .await?;
    event(
        &mut log,
        serde_json::json!({"stage":"planned","size":file.size,"private_evidence":directory}),
    )?;
    println!(
        "Reading one file through two cold mounts; private evidence: {}",
        directory.display()
    );

    let conservative = arm(
        &directory,
        &account,
        &scope.collection,
        &parent,
        &file,
        false,
    )
    .await?;
    event(
        &mut log,
        serde_json::json!({"stage":"arm","name":"conservative","bytes":conservative.bytes,
            "graph_gets":conservative.graph_gets,"content_gets":conservative.content_gets,
            "setups":conservative.setups,"conditional_windows":conservative.conditional_windows,
            "seconds":conservative.seconds}),
    )?;
    println!(
        "Conservative: {} bytes, {} Graph GETs, {} content GETs, {:.1} s",
        conservative.bytes,
        conservative.graph_gets,
        conservative.content_gets,
        conservative.seconds
    );

    let optimized = arm(
        &directory,
        &account,
        &scope.collection,
        &parent,
        &file,
        true,
    )
    .await?;
    event(
        &mut log,
        serde_json::json!({"stage":"arm","name":"optimized","bytes":optimized.bytes,
            "graph_gets":optimized.graph_gets,"content_gets":optimized.content_gets,
            "setups":optimized.setups,"conditional_windows":optimized.conditional_windows,
            "seconds":optimized.seconds}),
    )?;
    println!(
        "Optimized:    {} bytes, {} Graph GETs, {} content GETs, {:.1} s",
        optimized.bytes, optimized.graph_gets, optimized.content_gets, optimized.seconds
    );

    anyhow::ensure!(
        conservative.bytes == optimized.bytes && conservative.bytes > 0,
        "arms read different byte counts: {} against {}",
        conservative.bytes,
        optimized.bytes
    );
    anyhow::ensure!(
        conservative.sha256 == optimized.sha256,
        "THE OPTIMIZED PATH SERVED DIFFERENT BYTES THROUGH THE MOUNT"
    );
    anyhow::ensure!(
        optimized.setups > 0 && optimized.conditional_windows > 0,
        "optimized arm never established a bound session ({} setups, {} windows); \
         its byte match would not be evidence about the fast path",
        optimized.setups,
        optimized.conditional_windows
    );
    anyhow::ensure!(
        conservative.setups == 0 && conservative.conditional_windows == 0,
        "conservative arm used the session path; the arms are not distinct"
    );
    event(
        &mut log,
        serde_json::json!({"stage":"finished","passed":true,"sha256_match":true,
            "graph_gets_conservative":conservative.graph_gets,
            "graph_gets_optimized":optimized.graph_gets}),
    )?;
    println!(
        "Byte-identical through the mount. Graph metadata requests for the same read: \
         {} conservative against {} optimized.",
        conservative.graph_gets, optimized.graph_gets
    );
    Ok(())
}
