# The inode table nobody deletes from

Status: measured, undecided. The cost is now a number rather than a suspicion;
the pruning path is a sketch with two named checks and neither has been run.

## What was found

No `DELETE FROM inodes` exists anywhere in the repository. On a read-only mount
the inode key embeds the content revision and size
([`filesystem.rs:397`](../../crates/cirrove-service/src/filesystem.rs)), because
each revision needs its own kernel page cache. Every revision of every file a
user navigates to therefore mints a row that is never removed.

[The measurement](../benchmarks/inode-table-growth.json), taken from artifacts
already on disk:

- 750,460 rows cost **234 MB** -- 312 bytes each, table plus its UNIQUE index --
  and are **34 percent of the whole metadata database**. The index is larger
  than the table, because the key is a JSON tuple stored whole.
- The first traversal of a 500,000-file library writes 750,441 rows. Each later
  churn round adds 27, one per changed content revision. That second number is
  the steady-state rate: ordinary remote edits, forever.
- Extrapolated, a 200,000-file library whose files are each edited weekly adds
  about 62 MB of permanent rows a week, roughly 3 GB in a year, to a database
  whose useful content does not grow at all.

It is file-backed, so it costs no RSS. Every namespace memory criterion passes
while it grows. That is precisely why it needed its own measurement: the gates
that exist are structurally unable to see it.

A writable mount drops the revision and size from the key, so it does not have
this growth. The regime with the problem is the one the normal daemon runs.

## What makes pruning safe, and what does not

The schema is `inode INTEGER PRIMARY KEY AUTOINCREMENT`. SQLite keeps a
high-water mark in `sqlite_sequence` and never reissues a number that has been
deleted. The obvious hazard -- deleting a row and later handing its number to a
different object while the kernel still caches the old one -- is closed by the
schema, not by anything anyone has to write.

What is not closed is reachability. `writable_session.rs:1721` asserts that a
file keeps its inode number across a remount, so the identity is a durable
promise and not an in-memory convenience. A pruning pass may therefore remove
only keys the current namespace can no longer resolve: superseded content
revisions of items that still exist, and keys of items that are gone. Deleting a
reachable key would mint a fresh number for the same object on next lookup and
break that promise.

Two things have to hold, and the second is why this is not simply implemented:

1. **Prune only unreachable keys.** A superseded revision is unreachable when no
   view holds it and no newer key for the same item resolves to it. Answering
   that needs a way to go from an item to its keys, which the store cannot do
   today -- the same shape of missing index as the inode-to-key lookup that made
   eviction-with-reconstruction expensive in
   [ADR 0006](0006-peak-namespace-memory.md).
2. **Prune where no kernel reference can exist.** Mount time is the only moment
   with that property, and it is also the moment a user is waiting. A pass over
   750,000 rows on a cold database is not obviously cheap, and a slow mount is a
   worse defect than a large file.

## What would decide it

**What does the pass cost?** Run the reachability delete against the pilot's
750,460-row database, cold, and time it. If it costs more than the mount budget
it belongs in the background after mount, which changes the safety argument and
is worth knowing before writing either version.

**How much would it actually reclaim?** The same database, counting rows whose
item has a newer revision. If the answer on a realistic library is small, the
growth is dominated by first-traversal rows that are all reachable, and pruning
is the wrong remedy -- a narrower key would be the right one, and that is a
different change with different risks.

Neither has been run. Recording the cost without the remedy is deliberate: the
number is what makes the problem arguable, and inventing a mechanism before
knowing what it would reclaim is how the shedding work spent weeks.
