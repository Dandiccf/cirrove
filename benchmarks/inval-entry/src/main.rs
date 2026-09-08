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
//! isolation, outside Cirrove: a trivial server, one big directory, stat workers
//! running while invalidations are driven at a fixed rate.
//!
//! The bar is set by the workload it is meant to serve. A 500,000-file traversal
//! resolves about 750,000 views in roughly 530 seconds, so a ceiling that binds
//! during traversal has to shed at about 1,400 entries per second. Below that,
//! shedding is a soft ceiling with an overshoot rather than a bound, and that
//! changes what may be claimed.
//!
//! Deliberately outside the workspace: it is a measurement instrument, not a
//! feature, and the workspace forbids adding abstractions for unimplemented work.
use std::collections::HashMap;
use std::ffi::OsStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

const ENTRIES: u64 = 250_000;
const PARENT: u64 = 1;
const TTL: Duration = Duration::from_secs(1);

fn name_of(index: u64) -> String {
    format!("entry-{index:08}")
}

struct Directory {
    /// Injected per-reply delay, standing in for a server that has to think.
    delay: Duration,
    lookups: Arc<Mutex<Vec<u64>>>,
}

fn attr(inode: u64, directory: bool) -> fuser::FileAttr {
    fuser::FileAttr {
        ino: inode,
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
        &mut self,
        _req: &fuser::Request<'_>,
        parent: u64,
        name: &OsStr,
        reply: fuser::ReplyEntry,
    ) {
        let started = Instant::now();
        if parent != PARENT {
            reply.error(libc::ENOENT);
            return;
        }
        let Some(index) = name
            .to_str()
            .and_then(|n| n.strip_prefix("entry-"))
            .and_then(|n| n.parse::<u64>().ok())
            .filter(|index| *index < ENTRIES)
        else {
            reply.error(libc::ENOENT);
            return;
        };
        if !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
        reply.entry(&TTL, &attr(index + 2, false), 0);
        // Recorded after the reply so the measurement includes the reply itself,
        // which is where a blocked i_rwsem would show up.
        self.lookups
            .lock()
            .unwrap()
            .push(started.elapsed().as_micros() as u64);
    }

    fn getattr(
        &mut self,
        _req: &fuser::Request<'_>,
        inode: u64,
        _fh: Option<u64>,
        reply: fuser::ReplyAttr,
    ) {
        reply.attr(&TTL, &attr(inode, inode == PARENT));
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

    let lookups = Arc::new(Mutex::new(Vec::with_capacity(1 << 20)));
    let filesystem = Directory {
        delay: Duration::from_millis(delay_ms),
        lookups: lookups.clone(),
    };
    let session = fuser::Session::new(
        filesystem,
        std::path::Path::new(&mount),
        // Single-threaded, matching the dispatcher this is measuring for.
        &[fuser::MountOption::RO, fuser::MountOption::FSName("inval".into())],
    )
    .expect("mount");
    let notifier = session.notifier();
    let mounted = std::thread::spawn(move || session.run());

    std::thread::sleep(Duration::from_millis(200));
    let stop = Arc::new(AtomicU64::new(0));
    let stats: Arc<Mutex<HashMap<&'static str, u64>>> = Arc::new(Mutex::new(HashMap::new()));

    let mut stat_threads = Vec::new();
    for worker in 0..workers {
        let root = mount.clone();
        let stop = stop.clone();
        stat_threads.push(std::thread::spawn(move || {
            let mut index = worker as u64;
            while stop.load(Ordering::Relaxed) == 0 {
                let path = std::path::Path::new(&root).join(name_of(index % ENTRIES));
                let _ = std::fs::metadata(path);
                index += workers as u64;
            }
        }));
    }

    // Drive invalidations at the requested rate and record what was achieved,
    // because the interesting failure is the rate silently not being reached.
    let started = Instant::now();
    let interval = Duration::from_nanos(1_000_000_000 / rate.max(1));
    let mut sent = 0u64;
    let mut failed = 0u64;
    let mut slowest = Duration::ZERO;
    let deadline = started + Duration::from_secs(20);
    while Instant::now() < deadline {
        let call = Instant::now();
        let name = name_of(sent % ENTRIES);
        match notifier.inval_entry(PARENT, OsStr::new(&name)) {
            Ok(()) => sent += 1,
            // ENOENT means the kernel had already dropped it, which is success
            // for our purposes and must not be counted as a stall.
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => sent += 1,
            Err(_) => failed += 1,
        }
        slowest = slowest.max(call.elapsed());
        let next = started + interval * u32::try_from(sent + failed).unwrap_or(u32::MAX);
        if let Some(sleep) = next.checked_duration_since(Instant::now()) {
            std::thread::sleep(sleep);
        }
    }
    let elapsed = started.elapsed();
    stop.store(1, Ordering::Relaxed);
    for thread in stat_threads {
        let _ = thread.join();
    }
    stats.lock().unwrap().insert("sent", sent);

    let mut samples = lookups.lock().unwrap().clone();
    samples.sort_unstable();
    println!(
        "{}",
        serde_json::json!({
            "entries": ENTRIES,
            "requested_rate_per_second": rate,
            "achieved_rate_per_second": sent as f64 / elapsed.as_secs_f64(),
            "invalidations_sent": sent,
            "invalidations_failed": failed,
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
