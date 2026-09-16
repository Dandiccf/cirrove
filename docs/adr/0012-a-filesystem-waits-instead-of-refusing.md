# 0012: A filesystem waits instead of refusing

## What happened

On 2026-09-16 the owner updated the operating system and rebooted. `cirroved`
then held about 93 percent of one core for 25 minutes. The machine had no
visible reason to be busy: the account was signed in, indexed and idle, nothing
was downloading, and the counters confirmed it -- `content_gets` stayed at zero
for the whole episode.

Two facts in `/proc/<pid>/io` named the shape of it. `rchar` reached 3.8 TB
against 927,259,060 read calls, about 875,000 per second at just over 4 KiB
each. `read_bytes` was 451 MB. The daemon was not reading a disk. It was
fetching pages the kernel already held, one syscall at a time.

The desktop's own report named the second half. GNOME's `localsearch` had
crawled the mount and recorded **13,589 failures inside it**: 7,892 "PDF
document is damaged" and 604 "Resource temporarily unavailable". The same files
read back perfectly once the machine was quiet -- correct header, correct
`%%EOF`, full length. Nothing was wrong with them.

## Two decisions

### Admission waits. It never refuses.

Thirteen filesystem operations -- `read`, `open`, `lookup`, `getattr`, `write`
and the rest -- took their place in a semaphore with `try_acquire_owned` and
answered `EAGAIN` when it was full. 128 places for `pending`, 32 for `writes`.

POSIX permits `EAGAIN` on a read only when the caller opened the descriptor
`O_NONBLOCK`. A caller that never asked for one is entitled to assume that a
read either succeeds or fails for a reason about the file. Poppler, meeting
`EAGAIN` in the middle of a document, concluded the document was damaged. It
was a reasonable conclusion. We were the ones breaking the contract.

Measured on the mount: at 64 concurrent callers, no failures. At 256, **282 of
2,000 stat calls returned `EAGAIN`**. In the fixture, without this change,
**11,352 of 15,360 directory listings** came back refused.

The permit is now acquired inside the spawned task and awaited. The FUSE
callback still returns immediately, so no session thread blocks. The queue
cannot grow without bound, because the kernel decides how many FUSE requests
are outstanding; the waiters are capped by that, not by the callers behind it.

**ADR 0006 predicted this failure in its own words** -- "`ls: Resource
temporarily unavailable` is the failure a user would see" -- and asked for an
assertion "written before the ceiling, so the failure is a red test". The
assertion was never written. It exists now, and it was shown to fail first.

### The store is read through a memory map.

SQLite's page cache defaults to 2000 pages, about 2 MiB, against a metadata
database that is 564 MB on this machine. Opening one directory cost **82
positional reads, every time**, no cheaper on the tenth repeat than the first.
A pilot against a copy of the real database moved 2,239 reads to 1 with
`PRAGMA mmap_size`; raising `cache_size` to 64 MiB moved 5.6 reads per lookup
to 4.7, which is nothing, because the access pattern is genuinely scattered.

The map removes the syscall without holding the memory. These are file pages:
the kernel reclaims them under pressure, they are shared between connections
that map the same file, and the measured resident file pages after the change
were 18.7 MB against a 256 MiB map.

## Cost

**Corrected 2026-09-16, the same day, by the gate it did not think to run.** This
section measured resident memory on an idle daemon -- 87 MB to 150 MB, reclaimed
to 69 MB -- and concluded the map's cost was bounded. Under a traversal that
touches the whole store, the whole store becomes resident. At 500,000 files the
peak went 516 MiB, then 1,182, then 1,839 across three rounds, while anonymous
PSS stayed at 476 to 491 MiB and the live heap did not move at all; the released
phase of round two held 725.6 MiB of RSS against 20.5 MiB of anonymous PSS.

The pages are file-backed and the kernel reclaims them under pressure, so this
is not a leak. But **two gates in this project measure RSS**, and a bound that
reads RSS cannot tell a mapped page from a held one. Neither can a person
looking at their process list, and this owner has already asked once why the
daemon was using what it was using. `traversal-peak-gate.json` carries the
numbers and the two ways out, neither of which should be chosen without its own
measurement.

**An I/O error on a mapped page arrives as SIGBUS, not as an error return.** A
failing disk takes the mount down instead of reporting a fault. This is the
real price and it is why this is a decision record rather than a commit. It is
worth paying here: the alternative is a daemon that burns a core after every
update, and a user whose fan tells them so.

**Waiting is visible where refusing was not.** The fixture run takes 19.8
seconds where the refusing version took 3.8, because the work is now done
instead of thrown away. That is the correct trade, but it means a slow provider
shows up as a slow filesystem rather than as an error, and an operation that
would have failed instantly can now sit behind others.

**The bound must cover the whole database.** The first attempt set it to
256 MiB, chosen before the size was measured, and it halved the rate instead of
removing it: 875,000 reads per second became 391,989, which is the covered
fraction of a 564 MB file and nothing more. At 2 GiB -- SQLite maps at most the
file's length, so 563 MiB in practice -- the same workload reads about 2,000
times per second, and one directory open costs 11 reads rather than 82. The
eleven that remain are the write-ahead log, which SQLite does not read through
the map. A bound that the database can outgrow re-creates the problem quietly,
so it is set well above any metadata database this has produced and costs
address space rather than memory.

## What would refute this

An `EAGAIN` observed on the mount by any caller that did not open `O_NONBLOCK`.
A queue of waiters that grows beyond what the kernel has in flight. A resident
set that tracks the map size rather than the working set.

## What this does not fix

The daemon burns a core for roughly 16 to 25 minutes after **every** start, not
only after an update, and it did so again after the restart that installed this
change -- with the indexer idle at 0 percent CPU and nothing holding the mount
open. That is a third defect. The crawler made it worse and made it visible; it
did not cause it, and neither of the changes here addresses it.

It is no longer unexplained. Sampling the threads that are actually computing
-- every stack whose leaf is `futex`, `read`, `epoll_wait` or `syscall` has to
be discarded first, or two dozen parked tokio workers drown the one that is
working -- gives 16 computing stacks: **five in `Store::open`**, reading and
initialising the schema; **four closing a connection**, in `sqlite3Close`,
`sqlite3BtreeClose` and `drop_glue<rusqlite::Connection>`; **four in
`sqlite3Prepare` and `yy_reduce`**, parsing SQL; and four in
`Store::with_children`, which is the only one doing the work that was asked for.

**The daemon opens a new SQLite connection per operation.** There are 58 call
sites of `Store::open` in `cirrove-service`. Opening one and running a single
statement costs 0.10 ms where the same statement on a warm connection costs
0.002 ms, because each new connection re-reads and re-parses the whole schema --
17 tables, 23 indexes, 3,071 characters of DDL -- and prepares every statement
again.

This is the other half of the sentence ADR 0006 wrote and nobody acted on: "A
pooled read connection is a prerequisite, not an optimisation." Both halves are
fixed now; the pool is ADR 0013, which measures the same walk at 0.32 ms per
entry and a daemon start settling in 57 seconds rather than 16 to 25 minutes.

Measured and ruled out on the way, so that nobody pays for them twice:
`Engine::refresh_active_directories` holds at most 32 directories two seconds
apart; the recursive shortcut query takes 0.1 ms because a partial index covers
it; and all 116 SELECTs in the tree were planned against the real 564 MB
database, where the worst of the 20 full scans, `COUNT(*) FROM nodes` at 8.6 ms,
runs once every five seconds for the status display.
