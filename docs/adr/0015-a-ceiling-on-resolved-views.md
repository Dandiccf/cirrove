# A ceiling on resolved views

Status: **proposed.** Nothing here is implemented. It exists because
[ADR 0005](0005-namespace-memory.md) named two ways out of the peak criterion,
said the second "needs its own ADR", and did not have one — and because the
first one turns out not to be dead.

## Why this exists now and did not on 2026-09-09

Two things had to be true before this was worth writing, and on 2026-09-17 both
became true in the same night.

**Reducing what a view weighs has run out.** ADR 0014 is implemented and
measured: 489–509 bytes per live view against 637–664, and peak anonymous PSS
over the indexed baseline at 500,000 files is 379.5 / 378.8 / 394.0 MiB against
483.2 / 476.4 / 491.3. Twenty-one percent, matching the three passes before it.
It leaves the gate failing by 1.48 times, and the remainder was decomposed by
structure rather than guessed:
[the peak at five hundred thousand](../benchmarks/the-peak-at-five-hundred-thousand.json).
The largest remaining item was the name in the view, about 87 bytes, and it is
not available — the kernel's entry invalidation is
`fuse_notify_inval_entry(parent, name)`, the case it exists for is deletion, and
by the time the change feed is read the store row is gone, so the live view is
the last place in the process that knows what the deleted item was called. The
other three sum to about 81 bytes and land at 408–428 against a 365 budget.

**Shedding is not dead.** ADR 0006 retired it on a measurement of 107
`notify_inval_entry` per second under concurrent lookups. That measurement's own
limits section named the untested case: *"One parent directory. Invalidations
spread across many parents would contend less."* A ceiling sheds its **oldest**
views, which are in directories the traversal has already left, and
`fuse_reverse_inval_entry` takes the **parent's** `i_rwsem`. Extended
instrument, same machine, same day:
[shedding where nobody is looking](../benchmarks/shedding-where-nobody-is-looking.json).

| | sheds per second | FORGETs | lookups | lookup p99 |
| --- | --- | --- | --- | --- |
| same parent (the September arm, reproduced) | 105.4 | 2,109 | 17,729 | 1.15 ms |
| trailing, 64 parents | 1,400 | 28,000 | 17,861 | 1.11 ms |
| trailing, 64 parents | 24,000 | 480,006 | 18,016 | 1.10 ms |
| trailing, 64 parents | 60,000 | 1,000,000 (supply exhausted) | 18,062 | 1.10 ms |

One FORGET for every invalidation, and the lookups do not notice. No arm has
found the ceiling. The bar is harder than it was — this build traverses 500,000
files in 96 seconds rather than 530, so it resolves 7,800 views per second — and
24,000 clears it.

## The decision

**Hold at most a fixed number of resolved views. Above it, shed the oldest. If
shedding cannot keep up, slow the arrivals rather than refusing them.**

Three parts, and the third is what makes it a bound rather than a hope.

**The ceiling** is a count of live views, derived from the memory budget and the
measured cost of a view: 256 MiB at 499 bytes is about 538,000. It is a count
and not a byte figure because bytes per live view is the thing that reproduces —
to within 0.2 percent across four measurements at two scales — and a count is
what the namespace can actually enforce.

**The shed order is resolution order, and that is not an accident.**
`NamespaceViews` already carries a monotonic `generation` on every entry. A
traversal resolves a directory's children consecutively, so oldest-generation
first *is* "in a directory the sweep has left", with no extra bookkeeping and no
per-view cost — which matters, because per-view cost is the thing being
economised. Shedding sends `notify_inval_entry(parent, name)` and waits for the
FORGET that follows; the reclamation path that FORGET drives is the one already
there.

**Admission closes the gap.** Above the ceiling, a LOOKUP is admitted only as
fast as shedding retires an old view — it *waits*, exactly as the thirteen gates
in [ADR 0012](0012-a-filesystem-waits-instead-of-refusing.md) now wait. This is
not a detail. The one topology where shedding is slow is the single giant
directory, which is the same-parent case at 105 per second, and it is in this
gate's matrix. There, shedding alone is a brake and not a bound. With admission
it is a bound in every topology, and the cost is that a traversal of a
250,000-entry directory on a machine at its ceiling runs slower. That is the
trade a memory bound *is*, and it is the trade the desktop indexer incident says
is acceptable: 7,892 "PDF document is damaged" errors came from answering
`EAGAIN`, and none would have come from answering slowly.

## What this must not do

**It must not refuse a lookup.** POSIX allows `EAGAIN` only on `O_NONBLOCK`, and
ADR 0012 records what callers do with it otherwise. A ceiling that answers
ENOMEM or EAGAIN is a worse product than one that uses 400 MiB.

**It must not shed a view the kernel or a caller still needs.** The residency
accounting is what protects that and it already exists: a view with an open
file, a directory snapshot or an in-flight operation holds a lease, and leases
are what the candidate queue already respects. Shedding adds a second reason to
pass over an entry, not a second set of books.

**It must not let a shed race a lookup of the same entry.** `inval_entry` and a
concurrent LOOKUP on the same name are the one case `i_rwsem` serialises, which
is the mechanism behind the 105 per second — correctness is the kernel's here,
but the rate is not, and a design that sheds where it is looking will measure
105 and conclude the wrong thing.

## The gate that would retire it

Like its two predecessors, and for the same reason.

1. **Peak anonymous PSS at or below 256 MiB over three rounds at 500,000 files,
   in both topologies** — the 2,000-per-directory tree and the single 250,000
   directory. Anonymous, not RSS: since ADR 0012 the store is read through a
   2 GiB map whose clean file-backed pages go resident during a traversal and
   stay, so RSS both flatters a peak (the indexed baseline already contains
   140 MiB of map) and poisons it (695 MiB resident with 26 MiB of anonymous
   memory live). That restatement is owed to ADR 0005 separately and does not
   rescue the peak, which fails on anonymous memory too.
2. **No lookup refused, at any point, in any round.** Asserted, not observed.
3. **The traversal slowdown recorded, in both topologies.** If the tree case
   costs nothing and the giant directory costs an hour, that is the answer and
   it must be written down rather than averaged away.
4. **A cold listing no slower than its recorded bound**, the second gate ADR
   0014 carries.

If 1 holds and 3 is intolerable, this ADR joins the other two rather than
shipping with a footnote.

## What is not claimed

Nothing here is implemented. The instrument is a trivial server whose lookups
are a 1 ms sleep standing in for Cirrove's handler, which resolves a view in
about 1 ms of round trip across eight workers — the arm chosen, but a stand-in.
Cirrove's own candidate queue reached 624,436 entries during the 500,000-file
run without being asked to shed one, so the reclamation path has never been
driven at anything like 24,000 per second. And three wrong versions of the
measurement came before the working one, each of which read like an answer;
[the record](../benchmarks/shedding-where-nobody-is-looking.json) names them,
because the counter that caught two of them — did a FORGET actually arrive —
is the one a reimplementation would leave out.
