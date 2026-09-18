//! What does `notify_inval_entry` cost against concurrent lookups?
//!
//! Shedding namespace views means telling the kernel to drop dentries so FORGET
//! reclaims them. The only cost evidence Cirrove has for that is a cgroup
//! experiment in which the *kernel* shed dentries off its own LRU under memory
//! pressure, taking `d_lock` spinlocks with no FUSE upcall at all. This is a
//! different operation with a different lock: `fuse_reverse_inval_entry` takes
//! the parent directory's `i_rwsem` exclusively, while `lookup_slow` holds the
//! same lock shared across an entire FUSE round trip. Against a writer-preferring
//! rwsem and a single-threaded dispatcher, one queued invalidation can block every
//! subsequent stat on that parent until the in-flight lookup returns.
//!
//! Zero information transfers between the two, so this measures the real one, in
//! isolation, outside Cirrove: a trivial server, stat workers running while
//! invalidations are driven at a fixed rate.
//!
//! 2026-09-17: extended with more than one parent, which the first run's own
//! limits section named as untested -- "invalidations spread across many parents
//! would contend less". That matters because a resident ceiling sheds the OLDEST
//! views, which are in directories the traversal has already left. `i_rwsem` is
//! per-inode, so whether the recorded 107 per second is the traversal case or the
//! worst case turns entirely on this. `disjoint` puts the invalidations in
//! directories no worker is looking at; `same` puts them where the workers are,
//! and with one directory reproduces the original arms exactly.
//!
//! The bar was set by the workload it serves: a 500,000-file traversal resolving
//! about 750,000 views in roughly 530 seconds needs about 1,400 sheds per second
//! for a ceiling to bind during it. Two things have moved since. The traversal now
//! takes 96 seconds per round rather than 530, which makes that bar 7,800; and a
//! ceiling does not have to shed at the arrival rate if it may also SLOW the
//! arrivals, which is what ADR 0012 says a filesystem under pressure does -- wait,
//! never refuse. On that reading the rate is not pass-or-fail. It decides how much
//! slower a traversal runs on a machine at its ceiling, and 107 per second against
//! 1,400 is the difference between two hours of added time and two minutes.
//!
//! Deliberately outside the workspace: it is a measurement instrument, not a
//! feature, and the workspace forbids adding abstractions for unimplemented work.
use std::ffi::OsStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

const DEFAULT_ENTRIES: u64 = 250_000;
const ROOT: u64 = 1;
const TTL: Duration = Duration::from_secs(1);
/// Directory `d` is inode `2 + d`; its entries start above every directory.
const FIRST_FILE: u64 = 1_000_000;

fn name_of(index: u64) -> String {
    format!("entry-{index:08}")
}
fn directory_name(index: u64) -> String {
    format!("dir-{index:06}")
}

struct Directory {
    /// Injected per-reply delay, standing in for a server that has to think.
    /// Not charged during the warm-up, which is filling the cache rather than
    /// being measured.
    delay: Duration,
    warming: Arc<std::sync::atomic::AtomicBool>,
    /// How many parents the entries are spread over. One reproduces the
    /// original single-directory arms byte for byte.
    directories: u64,
    per_directory: u64,
    lookups: Arc<Mutex<Vec<u64>>>,
    /// Invalidating a dentry is only useful if FORGET follows: that is what lets
    /// the server drop the view. A rate with no forgets behind it frees nothing.
    forgotten: Arc<AtomicU64>,
}

fn attr(inode: u64, directory: bool) -> fuser::FileAttr {
    fuser::FileAttr {
        ino: fuser::INodeNo(inode),
        size: 0,
        blocks: 0,
        atime: SystemTime::UNIX_EPOCH,
        mtime: SystemTime::UNIX_EPOCH,
        ctime: SystemTime::UNIX_EPOCH,
        crtime: SystemTime::UNIX_EPOCH,
        kind: if directory {
            fuser::FileType::Directory
        } else {
            fuser::FileType::RegularFile
        },
        perm: if directory { 0o755 } else { 0o444 },
        nlink: if directory { 2 } else { 1 },
        uid: unsafe { libc::getuid() },
        gid: unsafe { libc::getgid() },
        rdev: 0,
        blksize: 512,
        flags: 0,
    }
}

impl fuser::Filesystem for Directory {
    fn lookup(
        &self,
        _req: &fuser::Request,
        parent: fuser::INodeNo,
        name: &OsStr,
        reply: fuser::ReplyEntry,
    ) {
        let started = Instant::now();
        let Some(text) = name.to_str() else {
            reply.error(fuser::Errno::ENOENT);
            return;
        };
        // A directory is resolved without the delay: the workers walk one every
        // time they stat, and charging them the server's thinking time twice
        // would halve the lookup rate for a reason that is not being measured.
        if parent.0 == ROOT {
            let index = text
                .strip_prefix("dir-")
                .and_then(|n| n.parse::<u64>().ok())
                .filter(|index| *index < self.directories);
            match index {
                Some(index) => {
                    reply.entry(&TTL, &attr(2 + index, true), fuser::Generation(0));
                }
                None => reply.error(fuser::Errno::ENOENT),
            }
            return;
        }
        let Some(directory) = parent.0.checked_sub(2).filter(|d| *d < self.directories) else {
            reply.error(fuser::Errno::ENOENT);
            return;
        };
        let Some(index) = text
            .strip_prefix("entry-")
            .and_then(|n| n.parse::<u64>().ok())
            .filter(|index| *index < self.per_directory)
        else {
            reply.error(fuser::Errno::ENOENT);
            return;
        };
        if !self.delay.is_zero() && !self.warming.load(Ordering::Relaxed) {
            std::thread::sleep(self.delay);
        }
        let inode = FIRST_FILE + directory * self.per_directory + index;
        reply.entry(&TTL, &attr(inode, false), fuser::Generation(0));
        // Recorded after the reply so the measurement includes the reply itself,
        // which is where a blocked i_rwsem would show up.
        self.lookups
            .lock()
            .unwrap()
            .push(started.elapsed().as_micros() as u64);
    }

    fn forget(&self, _req: &fuser::Request, _inode: fuser::INodeNo, nlookup: u64) {
        self.forgotten.fetch_add(nlookup, Ordering::Relaxed);
    }

    fn getattr(
        &self,
        _req: &fuser::Request,
        inode: fuser::INodeNo,
        _fh: Option<fuser::FileHandle>,
        reply: fuser::ReplyAttr,
    ) {
        reply.attr(&TTL, &attr(inode.0, inode.0 < FIRST_FILE));
    }
}

fn percentile(sorted: &[u64], fraction: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let index = ((sorted.len() as f64 - 1.0) * fraction).round() as usize;
    sorted[index]
}

fn main() {
    let mount = std::env::args().nth(1).expect("usage: inval-entry <mount>");
    let rate: u64 = std::env::args()
        .nth(2)
        .and_then(|a| a.parse().ok())
        .unwrap_or(1_400);
    let delay_ms: u64 = std::env::args()
        .nth(3)
        .and_then(|a| a.parse().ok())
        .unwrap_or(0);
    let workers: usize = std::env::args()
        .nth(4)
        .and_then(|a| a.parse().ok())
        .unwrap_or(8);
    let directories: u64 = std::env::args()
        .nth(5)
        .and_then(|a| a.parse().ok())
        .unwrap_or(1)
        .max(1);
    // `same` puts the invalidations where the workers are stat-ing, which is the
    // original arm. `trailing` sweeps the workers forward through every directory
    // and follows them at a fixed lag, which is what a ceiling shedding its
    // oldest views does during a traversal.
    let trailing = std::env::args().nth(6).as_deref() == Some("trailing");
    // How many entries exist. The shed region is half of them, so a high rate
    // needs a large one or it runs out of warmed dentries mid-run and reports a
    // supply limit as though it were a rate limit.
    let entries: u64 = std::env::args()
        .nth(7)
        .and_then(|a| a.parse().ok())
        .unwrap_or(DEFAULT_ENTRIES);
    let per_directory = entries / directories;
    assert!(!trailing || directories >= 2, "trailing needs two directories");
    // The workers hold the upper half, the invalidations walk the lower half:
    // entries that were resolved, whose directories the sweep has left, and
    // which are the oldest -- which is what a resident ceiling sheds.
    let worker_first = if trailing { entries / 2 } else { 0 };
    let worker_span = if trailing { entries / 2 } else { entries };

    let lookups = Arc::new(Mutex::new(Vec::with_capacity(1 << 20)));
    let forgotten = Arc::new(AtomicU64::new(0));
    let warming = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let filesystem = Directory {
        delay: Duration::from_millis(delay_ms),
        warming: warming.clone(),
        directories,
        per_directory,
        lookups: lookups.clone(),
        forgotten: forgotten.clone(),
    };
    let mut config = fuser::Config::default();
    // Single-threaded, matching the dispatcher this is measuring for.
    config.mount_options = vec![
        fuser::MountOption::RO,
        fuser::MountOption::FSName("inval".into()),
    ];
    let session = fuser::Session::new(filesystem, std::path::Path::new(&mount), &config)
        .expect("mount");
    let notifier = session.notifier();
    let mounted = std::thread::spawn(move || session.run());

    std::thread::sleep(Duration::from_millis(200));
    let stop = Arc::new(AtomicU64::new(0));

    // Fill the dentry cache before measuring anything. Without this there is
    // nothing for an invalidation to drop and the rate measures no-ops.
    let warmed = Instant::now();
    let warm_cursor = Arc::new(AtomicU64::new(0));
    let mut warm_threads = Vec::new();
    // Only the region the invalidations will walk. The workers' region stays
    // cold so their lookups reach the server, as a traversal's do.
    let warm_span = if trailing { worker_first } else { 0 };
    for _ in 0..workers.max(1) {
        let root = mount.clone();
        let warm_cursor = warm_cursor.clone();
        warm_threads.push(std::thread::spawn(move || {
            loop {
                let index = warm_cursor.fetch_add(1, Ordering::Relaxed);
                if index >= warm_span {
                    return;
                }
                let path = std::path::Path::new(&root)
                    .join(directory_name(index / per_directory))
                    .join(name_of(index % per_directory));
                let _ = std::fs::metadata(path);
            }
        }));
    }
    for thread in warm_threads {
        let _ = thread.join();
    }
    let warm_seconds = warmed.elapsed().as_secs_f64();
    let warm_forgets = forgotten.load(Ordering::Relaxed);
    warming.store(false, Ordering::Relaxed);
    lookups.lock().unwrap().clear();

    let cursor = Arc::new(AtomicU64::new(0));
    let mut stat_threads = Vec::new();
    for _ in 0..workers {
        let root = mount.clone();
        let stop = stop.clone();
        let cursor = cursor.clone();
        stat_threads.push(std::thread::spawn(move || {
            while stop.load(Ordering::Relaxed) == 0 {
                let index =
                    worker_first + cursor.fetch_add(1, Ordering::Relaxed) % worker_span;
                let path = std::path::Path::new(&root)
                    .join(directory_name(index / per_directory))
                    .join(name_of(index % per_directory));
                let _ = std::fs::metadata(path);
            }
        }));
    }

    // Drive invalidations at the requested rate and record what was achieved,
    // because the interesting failure is the rate silently not being reached.
    let started = Instant::now();
    let interval = Duration::from_nanos(1_000_000_000 / rate.max(1));
    let mut sent = 0u64;
    let mut absent = 0u64;
    let mut failed = 0u64;
    let mut slowest = Duration::ZERO;
    let deadline = started + Duration::from_secs(20);
    while Instant::now() < deadline {
        let call = Instant::now();
        // `trailing` walks the warmed lower half, which the workers have left.
        // `same` walks the same entries the workers are walking.
        let target = if trailing {
            (sent + absent) % worker_first
        } else {
            (sent + absent) % entries
        };
        let name = name_of(target % per_directory);
        let directory = target / per_directory;
        match notifier.inval_entry(fuser::INodeNo(2 + directory), OsStr::new(&name)) {
            Ok(()) => sent += 1,
            // ENOENT means the kernel has no such dentry. That is not a stall,
            // but it is also not work: an arm where most calls are ENOENT is
            // measuring the cost of invalidating nothing, which is why these are
            // counted apart and why `forgets_received` is reported beside them.
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => absent += 1,
            Err(_) => failed += 1,
        }
        slowest = slowest.max(call.elapsed());
        let next = started + interval * u32::try_from(sent + absent + failed).unwrap_or(u32::MAX);
        if let Some(sleep) = next.checked_duration_since(Instant::now()) {
            std::thread::sleep(sleep);
        }
    }
    let elapsed = started.elapsed();
    stop.store(1, Ordering::Relaxed);
    for thread in stat_threads {
        let _ = thread.join();
    }

    let mut samples = lookups.lock().unwrap().clone();
    samples.sort_unstable();
    println!(
        "{}",
        serde_json::json!({
            "entries": entries,
            "directories": directories,
            "per_directory": per_directory,
            "mode": if trailing { "trailing" } else { "same" },
            "warm_up_seconds": warm_seconds,
            "warm_up_forgets": warm_forgets,
            "forgets_per_second": (forgotten.load(Ordering::Relaxed) - warm_forgets) as f64
                / elapsed.as_secs_f64(),
            "requested_rate_per_second": rate,
            "achieved_rate_per_second": (sent + absent) as f64 / elapsed.as_secs_f64(),
            "invalidations_that_dropped_an_entry_per_second": sent as f64 / elapsed.as_secs_f64(),
            "invalidations_sent": sent,
            "invalidations_absent": absent,
            "invalidations_failed": failed,
            "forgets_received": forgotten.load(Ordering::Relaxed) - warm_forgets,
            "slowest_inval_us": slowest.as_micros() as u64,
            "stat_workers": workers,
            "server_delay_ms": delay_ms,
            "lookups": samples.len(),
            "lookup_p50_us": percentile(&samples, 0.50),
            "lookup_p99_us": percentile(&samples, 0.99),
            "lookup_max_us": samples.last().copied().unwrap_or(0),
            "seconds": elapsed.as_secs_f64(),
        })
    );

    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &mount])
        .status();
    let _ = mounted.join();
}
