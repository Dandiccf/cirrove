# Bounding the namespace peak

Status: **rejected by its own measurement.** Both named measurements were run on
2026-09-09. The peak does not move, so the mechanism below is not a route to the
peak criterion. The document stays because the negative result is the useful
part, and because what it rules out narrows what is left.

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

## What the measurements said

[The arms](../benchmarks/namespace-entry-ttl.json), 500,000 files, three rounds
each, one frozen binary, TTL the only difference:

| entry TTL | round-1 peak | navigation |
| ---: | ---: | --- |
| 1000 ms | 492.7 MiB | 13.4 / 13.0 / 11.4 ms |
| 100 ms | 492.2 MiB | 22.9 / 12.6 / 11.8 ms |
| 10 ms | 490.3 MiB | 53.8 / 51.9 / 49.3 ms |

The 1000 ms arm also rose across its three rounds, in peak, residue and traversal
time together, where the short-TTL arms were flat. That looked like a finding and
was not: [four replications](../benchmarks/namespace-g3-replication.json) at
1000 ms, two per binary, are flat in all three, with round-three residue between
15.3 and 17.4 MiB against that arm's 110.4. It was one outlier in eleven runs,
and it is recorded as unexplained rather than explained away.

**The peak does not fall.** A hundredfold reduction in TTL moves it by 0.5
percent. The prediction recorded before the run was that it would fall but stop
short of the ceiling; it was wrong, and wrong in the unfavourable direction.

The reason is a distinction this document did not make. **Expiry is not
eviction.** An expired entry is revalidated on next access, not dropped. The
kernel's dentry shrinker runs under *memory pressure*, and a machine with no
pressure keeps everything however short the TTL. The cgroup experiment that shed
136,000 of 150,138 views for 4.1 percent did so because a cap applied pressure --
not because a TTL made the entries cheap. "Tell the kernel which entries are
cheap to drop" was therefore never the lever; the lever was the pressure, and a
user-level daemon does not have it to give.

The cost question, answered anyway: navigation rises about fourfold and stays
roughly ten times inside its 500 ms bound. The cost was never the obstacle.

The preregistered decision rule said that if the peak did not move, this would be
recorded as a dead end and **not** retried with a shorter TTL until a result
appeared. That is what this section is.

## What it leaves, and one thing worth arguing

Of the three alternatives above, TTL pressure was the cheapest and it is gone.
Reducing bytes per view is measured at 15 to 20 percent, which takes 490 MiB to
about 410 and not to 256. Eviction with reconstruction is larger than shedding
was, and shedding was retired for being unaffordable.

Off-heap payloads look like a fourth option and are not one. ADR 0005 assessed
`feature/paged-view-payloads` when 96 percent of retained RSS was allocator free
arena, and concluded it addressed "the smaller of the two failures". The trim
has since closed the retained failure, so that premise has inverted and the
prototype now aims at the only failure left. It still does not pass, for the
reason the TTL arms just demonstrated: a file-backed mapping's pages are
resident until something reclaims them, and on an unloaded machine nothing does.
Moving 470 MiB of live payload from the heap into a mapping moves which counter
it lands in, not whether it is resident.

That generalises, and it is the useful part of this whole result. Any design
that keeps the payload reachable without a round trip leaves it resident on an
idle machine, whatever it is stored in. The peak criterion as phrased can
therefore only be met by a design that **discards** data and pays to rebuild it
-- which is why eviction with reconstruction is not merely the last candidate
standing but the only category that could ever have worked.

That leaves the argument this document reserved for exactly this outcome: if the
measurement shows a floor well above 256 MiB that no reachable design clears,
make the case explicitly, with the floor measured. The floor is now measured. It
is 490 MiB for 750,438 views, about 657 bytes each, stable to a fraction of a
percent across seven runs and three TTLs.

The case, stated so it can be argued against rather than assumed: the criterion
measures resident bytes on an unloaded machine, and the failure it exists to
prevent is a machine that cannot cope. Those are not the same measurement. Under
an actual cap the kernel sheds the views and the mount keeps working at 4.1
percent -- which is the behaviour "runs on any hardware" is asking about, and it
already passes. A criterion phrased as behaviour under pressure would be
falsifiable, would bind the thing users experience, and would not be met by
quietly raising a number until the implementation fits.

This is not a change anyone should make on their own, and it is not made here.
The budget was chosen to give "runs on any hardware" teeth, and re-phrasing a
criterion after failing it is exactly the move that empties one. It needs a
decision, and the decision needs the person whose product it is.

## Open

Whether the peak criterion stays as an absolute bound, moves to a
behaviour-under-pressure form, or stands unmet with a recorded floor. Nothing
else here is open: the mechanism is measured and rejected.
