//! Attribute intermittent navigation stalls without relaxing their deadline.
use super::*;
use std::{sync::atomic::AtomicU64, time::Instant};

const BUDGET: Duration = Duration::from_millis(500);
const DIAGNOSTIC_WAIT: Duration = Duration::from_secs(2);

struct Progress {
    start: Instant,
    stages: [AtomicU64; 4],
}
impl Progress {
    fn mark(&self, index: usize) {
        // Zero means not reached; store elapsed microseconds plus one.
        let micros = self.start.elapsed().as_micros().min(u64::MAX as u128 - 1) as u64;
        self.stages[index].store(micros + 1, Ordering::SeqCst);
    }
    fn snapshot(&self) -> serde_json::Value {
        let elapsed = self.stages.each_ref().map(|value| {
            value
                .load(Ordering::SeqCst)
                .checked_sub(1)
                .map(|micros| micros as f64 / 1000.0)
        });
        serde_json::json!({
            "blocking_worker_started_ms":elapsed[0],
            "opendir_returned_ms":elapsed[1],
            "enumeration_finished_ms":elapsed[2],
            "metadata_finished_ms":elapsed[3],
        })
    }
}

pub(super) async fn navigate(mounted: &Mounted, samples: &mut Vec<f64>) -> anyhow::Result<()> {
    let path = mounted.temp.path().join("mount/folder");
    let progress = Arc::new(Progress {
        start: Instant::now(),
        stages: std::array::from_fn(|_| AtomicU64::new(0)),
    });
    let worker_progress = progress.clone();
    let mut worker = tokio::task::spawn_blocking(move || -> std::io::Result<u64> {
        worker_progress.mark(0);
        let entries = std::fs::read_dir(path)?;
        worker_progress.mark(1);
        let entries = entries.collect::<Result<Vec<_>, _>>()?;
        worker_progress.mark(2);
        if entries.len() != 1 {
            return Err(std::io::Error::other("cached listing changed"));
        }
        let size = std::fs::metadata(entries[0].path())?.len();
        worker_progress.mark(3);
        Ok(size)
    });
    let result = tokio::time::timeout(BUDGET, &mut worker).await;
    let observed = progress.start.elapsed();
    let timed_out = result.is_err();
    if timed_out || observed >= BUDGET {
        let at_failure = progress.snapshot();
        let counts = mounted.server.provider.read_counters();
        let staging = mounted.engine.cache.window_stats();
        let pressure = ["cpu", "io", "memory"].map(|resource| {
            (
                resource,
                std::fs::read_to_string(format!("/proc/pressure/{resource}")).ok(),
            )
        });
        // Observe the SAME syscall task briefly for diagnosis. The operation has
        // already failed its 500 ms bound; late completion never changes that.
        let late_result = if timed_out {
            match tokio::time::timeout(DIAGNOSTIC_WAIT, &mut worker).await {
                Ok(Ok(Ok(_))) => "completed",
                Ok(Ok(Err(_))) => "io_error",
                Ok(Err(_)) => "worker_failed",
                Err(_) => "still_pending",
            }
        } else {
            "completed_before_observation"
        };
        eprintln!(
            "CIRROVE_NAVIGATION_STALL {}",
            serde_json::json!({
                "budget_ms":BUDGET.as_millis(),"observed_ms":observed.as_secs_f64()*1000.0,
                "timeout_fired":timed_out,"at_failure":at_failure,
                "after_diagnostic_wait":progress.snapshot(),"late_result":late_result,
                "diagnostic_wait_limit_ms":DIAGNOSTIC_WAIT.as_millis(),
                "graph_gets_at_failure":counts.graph_get_attempts,
                "content_gets_at_failure":counts.content_get_attempts,
                "downloaded_bytes_at_failure":counts.content_body_bytes,
                "staging_at_failure":staging,"host_pressure_at_failure":pressure,
                "scope":"synthetic fixture diagnostics; host pressure is context, not proof of causation"
            })
        );
        anyhow::bail!(
            "cached navigation exceeded 500 ms (observed {:.3} ms; timer fired: {timed_out})",
            observed.as_secs_f64() * 1000.0
        );
    }
    let size = result.context("cached navigation deadline")???;
    ensure!(size == 16 * 1024, "incorrect cached file metadata");
    samples.push(observed.as_secs_f64() * 1000.0);
    Ok(())
}
