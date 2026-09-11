//! Return freed memory to the kernel.
//!
//! glibc keeps what an application frees. After a 500,000-file traversal this
//! service holds about 17 MiB of live heap inside roughly 490 MiB of resident
//! memory: 96 percent of what it occupies has already been freed and simply not
//! given back. A user sees a half-gibibyte process that is doing nothing.
//!
//! `trim_threshold` does not reach it. That tunable governs trimming the top of
//! an arena on `free()`, and the retained pages sit inside long-lived non-main
//! arena heaps rather than at their tops. `malloc_trim` walks every arena and
//! releases free page ranges within each heap, which measured 454 to 466 MiB
//! returned per pass, taking the residue from 476 MiB to 10-14 MiB. See
//! `docs/benchmarks/namespace-malloc-trim.json`.
//!
//! None of this lowers the peak. Bounding what is held *during* a traversal is a
//! separate problem with a separate remedy; this only stops the process sitting
//! on the memory afterwards.
#![allow(unsafe_code)]

// A `configure()` entry point setting M_TRIM_THRESHOLD and M_MMAP_THRESHOLD was
// written here and then removed, because measuring it rejected it. Raising the
// mmap threshold to sit above the content path's four-mebibyte block buffer also
// sets glibc's `no_dyn_threshold`, and everything between 128 KiB and that new
// threshold stops being mmapped: allocations that used to return to the kernel on
// free now land in arenas and stay. Released memory went from 476/467/480 MiB to
// 479/576/633 MiB across three rounds. The block buffer it was protecting is not
// even allocated by that fixture, whose files are zero length.
//
// `malloc_trim` needs no such preparation. The probe that established the effect
// called it from an interposer with no `mallopt` at all.

/// Bytes the allocator holds free across every arena.
///
/// Counts free CHUNKS on the arenas' free lists. Reported because it is the
/// allocator's own view, but it is the wrong signal for deciding to trim, and
/// this is written down because measuring it was the only way to find out:
/// `malloc_trim` returns free PAGES to the kernel and leaves the chunks on the
/// free lists, so this figure does not fall when a trim succeeds. A trigger
/// keyed to it fires forever -- measured on a live daemon at eight trims in
/// eight seconds with the figure flat at 104 MiB.
///
/// `mallinfo2` rather than `mallinfo`, whose fields are `int` and wrap silently
/// above two gibibytes. This service has been measured holding 490 MiB; wrapping
/// is not far away.
#[must_use]
pub fn free_arena_bytes() -> u64 {
    // SAFETY: `mallinfo2` reads the allocator's own counters and returns them by
    // value. It takes no arguments, allocates nothing and touches no caller
    // memory.
    let info = unsafe { libc::mallinfo2() };
    info.fordblks as u64
}

/// This process's resident size, in bytes.
///
/// The figure a reclamation decision is ultimately about, and the only one that
/// moves when a trim succeeds. Reading `/proc/self/status` costs a small read;
/// zero when it cannot be read, so an unknown reads as "nothing to do".
#[must_use]
pub fn resident_bytes() -> u64 {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return 0;
    };
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|kib| kib.parse::<u64>().ok())
        .map(|kib| kib * 1024)
        .unwrap_or(0)
}

/// Resident memory this process holds that is not live heap.
///
/// The figure a reclamation decision actually wants: what the kernel counts as
/// ours minus what the allocator says is in use. That is the memory a trim could
/// give back, and unlike [`free_arena_bytes`] it FALLS when one succeeds,
/// because a successful trim lowers resident size. So it provides its own
/// hysteresis rather than needing one bolted on.
///
/// It is an over-estimate, and by more than "tens of mebibytes and roughly
/// constant", which is what an earlier version of this comment claimed. It
/// includes everything resident that is not malloc'd heap -- on this service the
/// SQLite page cache over a 560 MB index -- so on a real daemon it sits
/// permanently around 90 MiB and never falls after a trim. Reported because it
/// is worth seeing; do not use it to decide whether to trim. Measured in
/// docs/benchmarks/trim-trigger-reaches-reads.json.
///
/// Zero when the status file cannot be read, so a caller treats an unknown as
/// "nothing to do" rather than trimming blindly.
#[must_use]
pub fn retained_bytes() -> u64 {
    // SAFETY: as above.
    let live = unsafe { libc::mallinfo2() }.uordblks as u64;
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return 0;
    };
    let resident = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|kib| kib.parse::<u64>().ok())
        .map(|kib| kib * 1024)
        .unwrap_or(0);
    resident.saturating_sub(live)
}

/// Release free pages held by every arena. Returns whether anything was released.
///
/// This takes each arena's lock in turn, so every allocating thread stalls behind
/// it. Measured at 717 microseconds median around 50 MiB and 20 to 26 milliseconds
/// median, 62 milliseconds maximum, around a gibibyte. Call it from a blocking
/// worker on a quiescent tick, never from a thread that owns a filesystem reply,
/// and never while holding a lock an allocating thread is waiting for.
#[must_use]
pub fn trim() -> bool {
    // SAFETY: `malloc_trim` takes a pad size and releases only memory the
    // allocator already considers free. It cannot invalidate a live pointer.
    unsafe { libc::malloc_trim(0) == 1 }
}
