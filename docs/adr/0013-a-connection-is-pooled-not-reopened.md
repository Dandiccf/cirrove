# 0013: A connection is pooled, not reopened

## What it costs to open one

`cirrove-service` opens a store once per operation, at 58 call sites. What that
costs shows up in none of them. SQLite reads and re-parses the whole schema on a
connection's first statement -- 17 tables and 23 indexes here, 3,071 characters
of DDL -- and prepares every statement again. On the owner's 564 MB store an
open plus one statement measures 0.10 ms where the same statement on a warm
connection measures 0.002 ms.

Sampling the daemon on 2026-09-16, while a desktop file indexer walked the
mount, gave sixteen computing stacks: five in `Store::open`, four closing a
connection, four in `sqlite3Prepare` and `yy_reduce`, and four in
`Store::with_children` -- the only ones doing what was asked. A file indexer
re-checking the mount cost **1.28 ms of daemon CPU per file**, about a thousand
times what a local disk costs, so a legitimate re-check became a quarter of an
hour of fan noise.

ADR 0006 wrote this down and nobody built it: "A pooled read connection is a
prerequisite, not an optimisation."

## The decision

Connections are pooled per database file and returned when the store holding
them drops. Measured on the owner's own account, with the indexer competing:
200 opens build **1** connection instead of 200; per-entry CPU falls from
**1.28 ms to 0.32 ms**, median of seven walks; a full re-crawl costs **0.9
minutes** of daemon CPU instead of 3.6; and a daemon start settles in **57
seconds at 16 percent** of a core instead of 16 to 25 minutes at 100 percent.

## Idle connections live only as long as a store does

Before pooling, the last `Store` to drop was the last connection to close, and
SQLite retires `-wal` and `-shm` when the last connection closes. A pool that
outlived its stores would leave a write-ahead log beside a cleanly closed
account, which everything downstream reads as a crash. Three tests said so
within a minute of the first pool existing.

So the pool counts live stores per database. Idle connections are held only
while at least one store is alive, and when the count reaches zero they are
closed along with the last one. The old contract is untouched, and the account
keeper in `Engine` -- one connection held for the account's lifetime -- is what
keeps the count above zero while the account is open.

## A connection goes back only in the state a fresh one would be in

This was the hard part, and every case was found by a test rather than by
reading the code:

- **An open transaction.** State the next caller must not inherit.
- **Temporary tables.** `directory_publication` builds six per listing and
  creates them *without* `IF NOT EXISTS`, deliberately: a listing that finds
  them already there has inherited someone else's scratch space and must fail
  rather than publish it. A reused connection handed them straight on.
- **A progress handler.** `capacity::cold` installs one to abandon a fetch. A
  connection carrying it onwards interrupted whoever got it next with
  `SQLITE_INTERRUPT`, which reads as a database failure rather than as the
  cancellation it was.

A connection that still carries any of these is closed instead of shared. An
in-memory database is never pooled at all: every `:memory:` open is a different
database, and much of the test suite depends on that isolation.

## Cost

**A connection that stays open holds its own page cache and its own map of the
file.** The pool is bounded at 24 idle connections per database, which is the
shape of one per blocking worker; past that a connection is cheaper to close
than to keep.

**A future piece of per-connection state will be inherited silently.** Nothing
makes `reset_for_reuse` complete by construction; it knows about transactions,
the temporary schema and the progress handler because those are what this
codebase uses. Anything else added later -- a user-defined function, an
authorizer, a busy handler set per operation -- will travel between callers
until a test catches it, and the test may be far from the cause.

**The measured comparison has one soft edge.** The 1.28 ms figure is a single
walk; the 0.32 ms figure is a median of seven. The factor of four is well
outside the pooled arm's own spread, so the size holds, but a matching spread
for the old arm was never taken.

## What would refute this

A write-ahead log beside a closed account. A store that reads another's rows. A
connection count that rises with operations rather than with worker threads.
