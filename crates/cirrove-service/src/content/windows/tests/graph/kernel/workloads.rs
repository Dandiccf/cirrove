//! End-to-end read workload. Real FUSE/syscalls and OneDrive HTTP adapter;
//! deterministic loopback bodies, seeded metadata, no real cloud/desktop decoder.
use super::*;
use cirrove_onedrive::ReadCountersSnapshot;
use tokio::io::{AsyncBufReadExt, BufReader};

#[derive(Clone, Copy, Debug)]
enum Mode {
    Conservative,
    Strong,
    Windows,
}

fn check_counts(
    mode: Mode,
    phase: &str,
    before: &ReadCountersSnapshot,
    after: &ReadCountersSnapshot,
) -> anyhow::Result<()> {
    let graph = after.graph_get_attempts - before.graph_get_attempts;
    let content = after.content_get_attempts - before.content_get_attempts;
    let bytes = after.content_body_bytes - before.content_body_bytes;
    let setups = after.setups - before.setups;
    let renewals = after.renewals - before.renewals;
    let (expected_content, expected_bytes) = match phase {
        "preview" | "concurrent" => (1, 3_100_000),
        "reopen" => (0, 0),
        "sparse" => (4, 4 * BLOCK_SIZE as u64),
        "sequential" => (256, 1024 * 1024 * 1024),
        _ => anyhow::bail!("unknown application phase"),
    };
    ensure!(
        bytes == expected_bytes,
        "{mode:?}/{phase}: incorrect/excess transfer: {bytes}"
    );
    if !matches!(mode, Mode::Conservative) && phase == "sequential" {
        // Adaptation may select smaller windows on a slow disk/runner. Require
        // the acceptance ratio rather than pretending every host picks 64 MiB.
        ensure!(
            content <= 32,
            "window amplification exceeds eightfold target: {content}"
        );
    } else {
        ensure!(
            content == expected_content,
            "{mode:?}/{phase}: duplicate/missing transfer: {content}"
        );
    }
    if matches!(mode, Mode::Strong) {
        ensure!(setups == u64::from(phase != "reopen"));
        ensure!(
            graph == 2 * (setups + renewals),
            "per-block Graph checks on strong session"
        );
        ensure!(
            after.conditional_ranges - before.conditional_ranges + after.conditional_windows
                - before.conditional_windows
                == content - setups - renewals
        );
    } else {
        ensure!(
            graph == 2 * content,
            "conservative Graph validation mismatch"
        );
    }
    Ok(())
}

async fn workload(mode: Mode) -> anyhow::Result<serde_json::Value> {
    let delay_ms = std::env::var("CIRROVE_FIXTURE_REQUEST_DELAY_MS")
        .unwrap_or_else(|_| "0".into())
        .parse::<u64>()?;
    ensure!(
        delay_ms <= 100,
        "synthetic delay exceeds workload deadline model"
    );
    let control = Arc::new(Control {
        conservative: matches!(mode, Mode::Conservative),
        strong: matches!(mode, Mode::Strong),
        request_delay: Duration::from_millis(delay_ms),
        files: [
            ("preview".into(), (3_100_000, b'C')),
            ("sparse".into(), (256 * 1024 * 1024, b'D')),
            ("shared".into(), (3_100_000, b'E')),
        ]
        .into(),
        ..Default::default()
    });
    let mut mounted = Mounted::configured(1024 * 1024 * 1024, 512 * 1024 * 1024, control).await?;
    let baseline = rss_bytes();
    let mut peak = baseline;
    let mut child = tokio::process::Command::new("python3")
        .args(["-u", "-c", include_str!("read-workload.py")])
        .arg(mounted.temp.path().join("mount"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn()?;
    let mut input = child.stdin.take().context("application stdin")?;
    let mut lines = BufReader::new(child.stdout.take().context("application stdout")?).lines();
    let mut counts = mounted.server.provider.read_counters();
    let mut phases = Vec::new();
    let start = std::time::Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(180), async {
        for expected in ["preview", "reopen", "sparse", "concurrent", "sequential"] {
            let mut navigation = Vec::new();
            let mut tick = tokio::time::interval(Duration::from_millis(50));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut phase: serde_json::Value = loop {
                tokio::select! {
                    line = lines.next_line() => {
                        break serde_json::from_str(&line?.context("application exited before reporting phase")?)?;
                    }
                    _ = tick.tick() => {
                        navigate(&mounted, &mut navigation).await.with_context(|| format!("{mode:?}/{expected}: cached navigation"))?;
                        peak = peak.max(rss_bytes());
                    }
                }
            };
            ensure!(phase["phase"].as_str() == Some(expected), "unexpected application phase");
            let next = mounted.server.provider.read_counters();
            ensure!(next.graph_get_attempts == mounted.server.graph.load(Ordering::SeqCst) as u64);
            ensure!(next.content_get_attempts == mounted.server.content.load(Ordering::SeqCst) as u64);
            check_counts(mode, expected, &counts, &next)?;
            phase["graph_gets"] = (next.graph_get_attempts-counts.graph_get_attempts).into();
            phase["content_gets"] = (next.content_get_attempts-counts.content_get_attempts).into();
            phase["downloaded_bytes"] = (next.content_body_bytes-counts.content_body_bytes).into();
            phase["renewals"] = (next.renewals-counts.renewals).into();
            navigation.sort_by(f64::total_cmp);
            phase["cached_navigation"] = if navigation.is_empty() {
                serde_json::json!({"samples":0})
            } else {
                serde_json::json!({"samples":navigation.len(),"p50_ms":navigation[(navigation.len()-1)/2],"p95_ms":navigation[(navigation.len()-1)*95/100],"max_ms":navigation.last()})
            };
            phases.push(phase);
            counts = next;
            input.write_all(b"continue\n").await?;
        }
        ensure!(child.wait().await?.success(), "mounted read application failed");
        let stats = mounted.engine.cache.window_stats();
        ensure!(stats.staging_reserved_bytes == 0);
        ensure!(stats.staging_peak_bytes <= 64 * 1024 * 1024);
        // The number belongs in the failure, not only in the report that a
        // failure never reaches: the report is built below these assertions, so
        // a red run used to say "RSS growth" and nothing else -- neither how far
        // over it went nor whether it was close. A limit without a measurement
        // beside it cannot be argued with, only re-run.
        ensure!(
            peak.saturating_sub(baseline) < 256 * 1024 * 1024,
            "whole-file-sized service RSS growth: {:.1} MiB over a {:.1} MiB baseline, limit 256.0 MiB",
            peak.saturating_sub(baseline) as f64 / (1024.0 * 1024.0),
            baseline as f64 / (1024.0 * 1024.0)
        );
        ensure!(!matches!(mode, Mode::Conservative) == (stats.validated_windows > 0));
        Ok::<_, anyhow::Error>(serde_json::json!({
            "mode":format!("{mode:?}"),"build_profile":build_profile(),"phases":phases,
            "synthetic_request_delay_ms":delay_ms,
            "staging":stats,"elapsed_ms":start.elapsed().as_secs_f64()*1000.0,
            "service_and_loopback_sampled_rss_baseline_bytes":baseline,
            "service_and_loopback_sampled_rss_peak_bytes":peak,
            "fixture":"separate Python application, actual FUSE, loopback OneDrive adapter, seeded metadata; no real provider/indexing/desktop decoder"
        }))
    }).await.context("mounted application workload deadline").and_then(|r| r);
    if result.is_err() {
        let _ = child.kill().await;
    }
    let cleanup = mounted.close().await;
    let report = result?;
    cleanup?;
    Ok(report)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires FUSE and loopback HTTP; run explicitly, preferably --release"]
async fn real_mounted_conservative_read_workload() -> anyhow::Result<()> {
    println!(
        "CIRROVE_MOUNTED_READ_WORKLOAD {}",
        workload(Mode::Conservative).await?
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires FUSE and loopback HTTP; run explicitly, preferably --release"]
async fn real_mounted_strong_read_workload() -> anyhow::Result<()> {
    println!(
        "CIRROVE_MOUNTED_READ_WORKLOAD {}",
        workload(Mode::Strong).await?
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires FUSE and loopback HTTP; run explicitly, preferably --release"]
async fn real_mounted_windows_read_workload() -> anyhow::Result<()> {
    println!(
        "CIRROVE_MOUNTED_READ_WORKLOAD {}",
        workload(Mode::Windows).await?
    );
    Ok(())
}
