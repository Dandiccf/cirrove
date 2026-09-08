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
