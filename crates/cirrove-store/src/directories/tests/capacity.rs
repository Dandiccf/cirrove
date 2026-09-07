//! Run each mode in its own process so retained allocations cannot contaminate
//! the comparison. This measures Store reads, not mounted filesystem capacity.
use super::*;
use std::time::Instant;

fn kib(path: &str, field: &str) -> u64 {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix(field))
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

#[test]
#[ignore = "500k-entry SQLite directory; run stream and collect separately in release mode"]
fn large_directory_read_memory() {
    const COUNT: usize = 500_000;
    const PAGE: usize = 1000;
    let mode = std::env::var("CIRROVE_DIRECTORY_READ_MODE").unwrap();
    assert!(matches!(mode.as_str(), "stream" | "collect"));
    let snapshot = std::env::var("CIRROVE_DIRECTORY_SNAPSHOT").unwrap() == "1";
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("metadata.db");
    let mut db = Store::open(&path).unwrap();
    let scope = scope();
    let mut cursor = db.begin(&scope, true).unwrap();
    // Bounded provider-sized pages keep fixture construction from establishing
    // a full-directory Vec high-water mark before the actual read measurement.
    for start in (0..COUNT).step_by(PAGE) {
        let end = (start + PAGE).min(COUNT);
        let next = Cursor(end.to_string());
        db.stage(
            &scope,
            cursor.as_ref(),
            &ChangePage {
                changes: (start..end).map(|n| Change::Upsert(node(n))).collect(),
                checkpoint: if end == COUNT {
                    Checkpoint::Complete(next.clone())
                } else {
                    Checkpoint::Continue(next.clone())
                },
            },
        )
        .unwrap();
        cursor = Some(next);
    }
    if snapshot {
        // Prepare the alternate persisted listing representation entirely in
        // SQLite. Foreground publication still materializes lists; this fixture
        // deliberately measures the read API, not that separate unsolved path.
        let key = Store::key(&scope).unwrap();
        let tx = db.db.transaction().unwrap();
        tx.execute(
            "INSERT INTO directories(scope,parent,seen,source_revision)
            SELECT ?1,'root',0,revision FROM metadata_clock WHERE singleton=1",
            [&key],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO directory_entries(scope,parent,id,name,body)
            SELECT scope,'root',id,json_extract(body,'$.name'),body FROM nodes WHERE scope=?1",
            [&key],
        )
        .unwrap();
        tx.commit().unwrap();
    }
    db.db
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    drop(db);
    let db = Store::open(&path).unwrap();
    let rss_before = kib("/proc/self/status", "VmRSS:");
    let peak_before = kib("/proc/self/status", "VmHWM:");
    let pss_before = kib("/proc/self/smaps_rollup", "Pss:");
    let started = Instant::now();
    let mut count = 0;
    let mut sizes = 0;
    let mut previous: Option<(String, String)> = None;
    let mut inspect = |n: &Node| {
        if let Some((name, id)) = &previous {
            assert!((name.as_str(), id.as_str()) < (n.name.as_str(), n.id.as_str()));
        }
        previous = Some((n.name.clone(), n.id.clone()));
        count += 1;
        sizes += n.size;
    };
    let retained = if mode == "collect" {
        let nodes = db.children(&scope, "root").unwrap().unwrap();
        for node in &nodes {
            inspect(node);
        }
        Some(nodes)
    } else {
        assert!(
            db.visit_children(&scope, "root", |n| {
                inspect(&n);
                Ok(())
            })
            .unwrap()
        );
        None
    };
    let elapsed_ms = started.elapsed().as_millis();
    let rss_after = kib("/proc/self/status", "VmRSS:");
    let peak_after = kib("/proc/self/status", "VmHWM:");
    let pss_after = kib("/proc/self/smaps_rollup", "Pss:");
    assert_eq!(count, COUNT);
    assert_eq!(sizes, COUNT as u64 * (COUNT as u64 - 1) / 2);
    let record = serde_json::json!({
        "fixture": "SQLite Store single directory; no FUSE, network or foreground publication",
        "mode": mode, "snapshot": snapshot, "entries": count,
        "elapsed_ms": elapsed_ms,
        "rss_before_kib": rss_before, "rss_after_kib": rss_after,
        "peak_before_kib": peak_before, "peak_after_kib": peak_after,
        "pss_before_kib": pss_before, "pss_after_kib": pss_after,
        "database_bytes": std::fs::metadata(&path).unwrap().len(),
        "retained_nodes": retained.as_ref().map_or(0, Vec::len)
    });
    println!("{record}");
    if mode == "stream" {
        assert!(rss_after.saturating_sub(rss_before) < 64 * 1024, "{record}");
        assert!(
            peak_after.saturating_sub(peak_before) < 64 * 1024,
            "{record}"
        );
    }
    std::hint::black_box(&retained);
}
