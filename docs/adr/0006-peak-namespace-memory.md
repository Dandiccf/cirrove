# Bounding the namespace peak

Status: proposed. Nothing here is implemented, and the measurement that would
decide it has not been run.

## Why this exists

[ADR 0005](0005-namespace-memory.md) closed G3: the reclamation tick returns freed
pages once a mount goes quiet, and released memory fell from 477 MiB to 13-16 MiB
against a 256 MiB budget. It did not close the criterion added alongside it. The
traversal peak stays at 490-504 MiB with 750,438 live views, and trimming cannot
lower a high-water mark reached while those views were held.

Whether this service runs on a machine with a gibibyte of usable memory turns on
the peak, not on the residue. That is the whole reason the sibling criterion
exists.

Shedding was the plan, and it is retired. A standalone microbenchmark
([the measurement](../benchmarks/inval-entry-rate.json)) found `notify_inval_entry`
sustains about 107 per second under concurrent lookups on the same parent, against
the roughly 1,400 per second a ceiling binding *during* a traversal would need.
Requesting 4,000 or 12,000 changes nothing. With no concurrent lookups the full
rate is reached, which identifies the cause exactly:
`fuse_reverse_inval_entry` takes the parent's `i_rwsem` exclusively while
`lookup_slow` holds it shared across a whole round trip.

That failure is specific, and it points at the shape of the remedy.

## The proposal: let the kernel decide, and tell it what it needs to know

Shedding fails because it pushes invalidations *up* into a lock the lookup path
already holds. The kernel's own dentry shrinker takes `d_lock` off its LRU with no
FUSE upcall at all, and that path is affordable: under a memory cap it shed about
136,000 of 150,138 views for a 4.1 percent traversal cost, with every fixture
assertion intact.

So do not push. Make the kernel's own reclamation able to do the work, by telling
it which entries are cheap to drop and re-fetch.

Concretely, the entry TTL becomes a function of how much the namespace is holding
rather than the constant one second it is today (`filesystem.rs:35`). Below a
resident ceiling, nothing changes. Above it, entries resolved by bulk traversal
get a short TTL, so the kernel expires them on its own schedule and FORGET
reclaims them through the path that already works. Entries with an open file, a
held directory snapshot or a pending operation keep the long TTL, because those
are the ones a user is actually looking at.

Three things make this different from shedding rather than a variation on it.

**No reverse notification.** The expensive lock is never taken. The kernel drops
what it wants, when it wants, on the path the cgroup experiment already measured.

**No new reference accounting.** A view whose dentry expires is forgotten by the
same FORGET that handles every other case. Shedding's four mandatory corrections
were all about not breaking that accounting; none of them applies here.

**It degrades rather than fails.** Under memory pressure the kernel reclaims more
aggressively on its own, which is exactly when a lower ceiling is wanted. Shedding
had to guess the pressure and act on it.

## What it costs, and what would refute it

A short TTL means re-LOOKUP, and re-LOOKUP is not free:
`filesystem.rs:355` opens a store connection per published view, and `lookup`,
`getattr` and `opendir` gate on a 128-permit semaphore that replies `EAGAIN` when
exhausted. `ls: Resource temporarily unavailable` is the failure a user would see.
A pooled read connection is a prerequisite, not an optimisation, and the churn
fixture needs an assertion that no operation returns `EAGAIN` while the ceiling is
engaged — written before the ceiling, so the failure is a red test.

Two measurements would decide it, and neither has been run.

**Does a short TTL actually lower the peak?** Run the 500,000-file churn fixture
with the entry TTL fixed at 1 second, 100 milliseconds and 10 milliseconds, and
read `peak_rss_kib`. If the peak does not fall materially at 10 milliseconds, the
kernel is not reclaiming on the timescale this depends on and the idea is dead. A
prediction, recorded before the run: the peak falls but not to the ceiling,
because the pinned ancestor spine does not expire — every child holds its parent
through `_parent_residency`, so every ancestor of every resident view has a
strong count above one regardless of its dentry.

**What does it cost in navigation?** The same fixture reports `navigation_ms`
against an asserted 500 ms bound. A short TTL turns cached navigation into
repeated re-lookups, and the 27.6-fold headroom measured for the stall work is the
budget this would spend.

## Alternatives, and why they are not first

**Reduce bytes per view.** Measured three times for 15 to 20 percent each, and the
peak is 490 MiB against a 256 MiB budget. It cannot get there alone, and its own
review found the compact record lossy in two load-bearing places: `parent_id`
cannot be reconstructed across a shortcut boundary, and collapsing `etag` and
`content_version` breaks the If-Match precondition on upload. Worth doing
eventually; not a route to this criterion.

**Eviction with reconstruction.** Needs a reverse inode-to-key lookup
`cirrove-store` does not have, a debt map for outstanding kernel references, a
single-flight gate and a hydration path, and it narrows old-revision behaviour
from serving stale attributes to `ESTALE`. It also makes the never-pruned `inodes`
table load-bearing for correctness, which makes pruning it harder later. It is the
fallback if TTL pressure does not work, not the first attempt.

**Raise the budget.** The 256 MiB figure is not sacred, and a criterion nothing can
meet is worth re-examining rather than working around. But it was chosen to make
"runs on any hardware" mean something, and moving it because the implementation is
inconvenient would empty it. If the TTL measurement shows a floor well above
256 MiB that no reachable design clears, that argument should be made explicitly,
with the floor measured, rather than by quietly adjusting the number.

## Open

Everything. This is a design with a mechanism and two named measurements, not a
result. It is recorded now because the previous plan was retired by measurement
and leaving no successor would make the peak criterion look like an oversight
rather than an open problem with a next step.
