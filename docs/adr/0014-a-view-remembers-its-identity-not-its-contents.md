# 0014: A view remembers its identity, not its contents

**Proposed, and gated on a measurement that has not been taken.** ADR 0005 named
two ways out of the traversal peak and asked for an ADR for the second. This is
that ADR. Two of its predecessors were killed by their own gating measurement,
which is the correct outcome for a proposal that does not work, and this one is
written so the same thing can happen to it.

## The number that will not move

`traversed_with_old_files` holds **492.8 / 490.7 / 504.1 MiB at 750,438 live
views** -- 637 to 657 bytes each, which reproduces to within 0.2 percent while
retained RSS varies by 6 percent. The gate is 256 MiB. Returning pages
afterwards cannot lower a high-water mark reached while the views were held;
only holding less can.

Two designs tried to hold fewer views and both died:

- **Shedding** (`notify_inval_entry` above a ceiling) needed about 1,400 per
  second to bind during a traversal and sustains about 107 under concurrent
  lookups (`inval-entry-rate.json`).
- **A short entry TTL** above a ceiling, so the kernel's own dentry reclamation
  does the work: ADR 0006, rejected by its own measurement. The peak does not
  move.

The third way is to hold *less per view* -- three passes have moved 15 to 20
percent each and will not reach a factor of two.

## What a view holds, and why it holds it

A live view is `scope`, `node`, `name`, `alias`, `ancestry`, two residency
handles, and two inode numbers. The shareable parts are already shared: `scope`,
`alias` and `ancestry` are `Arc`s that siblings hold in common. What is not
shareable is the `Arc<Node>`: every file has its own, and a `Node` is five
heap-allocated strings -- id, name, etag, content version, parent id.

So the 650 bytes are, in the main, **a copy of the file's metadata, held in case
somebody asks about it**.

## The decision

**Hold the identity, fetch the contents.** A live view keeps what only it knows:
its inode, its parent's inode, and the key that names the item in the store.
Everything else is read from the metadata store when an operation actually
arrives.

## Why this is arguable now and was not before

Because the store got fifty times faster today, and that is the whole argument.

Before 2026-09-16 a store lookup meant opening a connection: SQLite read and
re-parsed the entire schema, then prepared every statement again. An open plus
one statement measured **0.10 ms**. Resolving a view per operation at that price
would have turned every `getattr` into a tenth of a millisecond of parsing, and
nobody would have proposed it.

Connections are pooled now (ADR 0013) and the store is read through a memory map
(ADR 0012). The same statement on a warm connection measures **0.002 ms**, and a
directory open fell from 82 read syscalls to 11. A resolution per operation now
costs about two microseconds of CPU and no syscall at all.

That is the change. This design was not rejected earlier; it was not available.

## What it should cost, and the gate it must pass

**Corrected the same evening it was written, by measurement.** The first version
of this section expected **150 to 200 bytes** per identity-only view. That was a
guess, and it was about two and a half times too optimistic.

Lengthening one field of the churn fixture at a time and watching bytes per live
view move (`bytes-per-live-view.json`) says where the 658 bytes actually are. The
name is stored once, not twice as this record first assumed. The id is stored
about 1.3 times. All four strings together occupy about 160 bytes once each
allocation's header and rounding are counted -- which leaves **about 498 bytes of
per-view structure**: the view itself, the two indexes that each hold an entry
per view, and the B-tree's own overhead.

So dropping the node saves its strings (about 160) and its `Arc` allocation
(about 176) and costs a key (about 32): **about 354 bytes per view.** At 750,438
views that is **266 MB against a 268 MB gate**. On the line, not inside it, and a
design that arrives exactly at its bound has not passed it.

This record therefore proposes something necessary and not sufficient. The 498
bytes it does not touch are now the larger half of the problem, and whatever
comes next has to say what it does about them.

### The route that the measurements now allow

Three things together, none of them sufficient alone, all of them measured
rather than estimated:

1. **Drop the node from the view.** 650 to about 354 bytes, as above. 266 MB at
   750,438 views.
2. **Stop keeping a second copy of the item id.** The invalidation index's
   `Key { scope: Arc<Scope>, item: Box<str>, inode: u64 }` holds one per live
   view, which is what the 1.3 slope measured. Sharing it means `Node.id`
   becoming an `Arc<str>`; worth 30 to 50 bytes. **About 310 to 324 bytes, or
   233 to 243 MB** -- under the 256 MiB bound for the first time, with the
   margin a bound needs.
3. **Bound the map, or the gate reads the wrong thing anyway.** The 500,000-file
   run holds 1,839 MiB of RSS against 491 MiB of anonymous PSS, because the
   store's mapped pages become resident (`traversal-peak-gate.json`). A live
   heap of 233 MB does not pass a gate that reads RSS while a 2 GiB map is
   underneath it. Whichever way that is settled -- a smaller map, or ADR 0005
   arguing that the gate should read what the daemon holds -- it has to be
   settled before the other two can be demonstrated.

That is a route rather than a plan: none of it is implemented, and the second
item alone is the size of the three passes already recorded as having moved 15
to 20 percent each. What has changed tonight is that the arithmetic is measured
end to end, so the next person is not starting from an estimate.

**G-peak: `traversed_with_old_files` peak RSS at 500,000 files, three rounds, at
most 256 MiB, with bytes per live view reported beside it.** Registered before
the run. If the peak lands above 256 MiB, or if bytes per view do not fall by at
least a factor of three, this ADR is retired like the two before it and the
remaining path is the one nobody wants: a ceiling on concurrent live views, with
whatever that does to the lookup contract.

**A second gate, because speed is the price being paid.** Navigation during the
first index is already measured
(`navigation-during-the-first-index.json`), and a cold listing must not get
slower than its recorded bound. A design that buys memory with latency has to
say how much latency, out loud, before it is believed.

## What this does not touch

Correctness of FUSE lifetimes. A view is still created on LOOKUP and still lives
until FORGET; what changes is what it remembers, not how long. The residency
handles stay exactly as they are -- they are what keeps the parent chain alive,
and they are not the bytes in question.

## The honest state

Nothing here is implemented. The one thing this record adds that its two dead
predecessors could not have is a reason to believe the cost is now affordable,
and that reason is a measurement taken for an unrelated defect on the same day.
