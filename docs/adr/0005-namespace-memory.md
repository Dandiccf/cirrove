# Namespace lifetime and large-library memory

Status: listing lifetime and reference-aware file/directory reclamation implemented; full capacity acceptance remains open.

## Current evidence

`filesystem::Inner.views` originally held a mount-lifetime `HashMap<u64, View>`. Both
lookup and directory listing inserted projected views. A view owns metadata strings,
scope, alias and ancestry vectors and sometimes a second entry node. `releasedir`
dropped the handle's `Arc<Vec<View>>`, but left those entries in the shared view map.
Plain listing projections now live only in the directory snapshot; resolved lookup
and operation views still enter the shared map. The map now tracks kernel lookup
references and shared residency tokens for resolved files and directories. Single
and batched FORGET release counted references; open files, directory snapshots,
in-flight operations and child views protect their dependencies until their tokens
also retire. Each child holds a parent token, retaining the rest of its ancestor
chain through the parent entry. Old and new view clones retain their respective
parent leases after moves. Callbacks capture parent views before async dispatch;
parent-chain validation rejects cycles and missing parents before map changes.
Count errors preserve the affected
entry conservatively. A queued collector checks at most 4,096 candidates per second.

This is growth with projected entries and revisions visited during the mount, not
automatic materialization of every item merely because the index contains 500,000
files. Required ancestor chains remain protected, and kernel-referenced views
cannot yet shed their full metadata payloads. Full namespace lifetime remains a
scalability gap. The content-cache quota does
not limit these allocations. Metadata invalidation now uses revision and live-projection
indexes. Local or recovery sweeps still visit all live entries, but copy only bounded
batches.

Open directories now retain immutable anonymous disk snapshots of compact
inode/kind/name records, with a separate offset index. The read-only cached path
streams from Store through Engine and 128-entry projection batches, without a
complete node or view vector. READDIR uses positioned pages of at most 1,024
entries and 64 KiB of encoded data, plus bounded index/decoded buffers. The
snapshot retains parent/grandparent routes for lifetime safety, not every child
view. It survives concurrent metadata changes and independent cookie readers.

Per mount, at most 256 snapshots and 256 MiB of logical data/index bytes are
reserved before writes. Exhaustion returns EMFILE/ENOSPC. Physical disk blocks,
filesystem metadata and kernel page cache are additional. Anonymous files close
after the producer and last in-flight reader or handle release them, including process death;
they are not durable state and need no orphan sweep. Builders fail closed on I/O,
quota or cancellation errors. Read-only construction now publishes its first
flushed 128-node batch before the complete scan, then extends an immutable prefix
in roughly 1,024-entry batches. A watch frontier lets READDIR wait asynchronously
for more entries. Only successful completion permits EOF; later I/O errors and
abandoned producers are observable failures. The final reader disappearing stops
further building, while the existing reservation remains until writer and reader
descriptors close. The builder continues independently of reader speed, so idle
handles do not keep a read transaction after construction finishes. Writable
overlays retain completed-snapshot semantics. Large-directory latency and sustained
acceptance for this path remain open. The SQLite inode table has its own persistent
lifetime; neither its growth nor remaining view payloads are solved by snapshot paging.

A small actual-kernel fixture blocks later inode allocation with a separate SQLite
writer while permitting the first cached batch. First entries arrive in 4.448–4.642
ms, before the writer releases. Complete old/fresh listings across a concurrent
rename and closing during construction both preserve their expected behavior and
reclaim snapshot storage. Unit tests exercise late data/index flush failures and
abandoned producers. A separate CI startup-readiness failure remains unresolved. See
[the validation scope](../validation.md#directory-prefixes-before-construction-completes-2026-09-07).

A frozen release binary then runs the same three-pass 500,000-file fixture with
every file in one directory. Its first entry arrives in 4.08-4.92 ms against the
unchanged 500 ms bound, from a published prefix of 130 entries, while the rest of
the listing is still being produced. A controlled pair of frozen release binaries,
differing only in this path and run alternately in one session, gives 8.80-10.76
seconds before the change against 4.33-5.23 ms after it, with no overlapping
sample and a median ratio of 2,199. Whole-pass traversal is unchanged in that
pair, 10.585 against 10.232 seconds, so the earlier apparent throughput cost was a
session difference. Post-invalidation RSS still rises across passes, so sustained
capacity remains unproven. See
[the giant-directory measurements](../validation.md#giant-directory-first-entry-with-prefix-publication-2026-09-08).

The Store read path now uses individual indexed directory-snapshot rows and an
ordered visitor, with one decoded node per callback. It avoids loading a JSON
array and merging a whole directory in a HashMap. Both cached snapshot and delta
index paths have tests for index-ordered output, concurrent publication, early
callback failure and transaction cleanup. Schema upgrades preserve the old data
on failure and serialize concurrent migration attempts. The compatibility API
still collects a Vec, as does writable local-overlay lookup/projection. Foreground
publication now uses bounded page staging and atomic SQL publication. Cached
read-only LOOKUP now restricts the same visibility query
by name, uses the ordered name indexes and decodes only its first matching identity.
Cached read-only OPENDIR now streams, but the
remaining consumers keep the end-to-end paging gate open.

The direct name path has separate actual-kernel coverage: 16 distinct stat
requests plus an absent name in one indexed 500,000-file directory complete in
11.06 ms in a release fixture, without snapshot reservations or provider calls.
Sampled RSS rises from 11,384 to 13,076 KiB, and all 18 lookup views return to the
root after normal invalidation. This avoids full-directory materialization for
these direct requests; it does not accelerate enumerating every filename or
establish long-session capacity. See
[the raw lookup measurement](../benchmarks/indexed-name-lookups.json).

An isolated release-build Store fixture visits 500,000 entries in one directory
in 575–603 ms, retaining zero nodes and showing no additional sampled RSS over
its roughly 8.2 MiB ready baseline. Collecting the same ordered nodes through the
compatibility API retains 500,000 nodes and adds roughly 156.6 MiB RSS. Both paths
use the new schema; this is a comparison of consumers, not old and new binaries.
The fixture excludes FUSE and foreground publication. See
[the raw measurements](../benchmarks/directory-store-streaming.json) and
[reproduction commands](../development.md#namespace-capacity-baseline).

With the cached OPENDIR path streaming all the way to disk snapshots, separate
actual-kernel release runs now traverse 500,000 files both in one directory and
in 500 directories. Both runs complete three changed-revision passes, release
all logical snapshot bytes after close and return to the root alone after
invalidation, without content or foreground metadata requests. The single large
directory holds 33,000,045 logical snapshot bytes; peak process RSS rises by
26,964 KiB over its 11,944 KiB ready baseline. Its first entry takes 5.49–7.31
seconds, so the memory reduction does not solve opening latency; prefix
publication later reduced that first entry to milliseconds. The first
1,000-file directory takes 7.1–9.4 ms in the other variant.

Post-invalidation RSS still rises across passes: 18,340 / 28,876 / 38,404 KiB for
the single directory and 17,820 / 27,356 / 36,224 KiB for many directories. These
measurements do not demonstrate a plateau or close the namespace gate. They are
not a controlled comparison with older binaries. See
[the raw snapshot-page measurements](../benchmarks/directory-snapshot-pages.json).

A synthetic actual-kernel baseline confirmed the original growth: after three traversals
of 500,000 files with new content revisions, 1,500,501 views remained with no open
file or directory handles. Process RSS was about 2,071 MiB, compared with 21 MiB
after indexing. See [the measurement and its limits](../validation.md#namespace-capacity-baseline)
and [machine-readable results](../benchmarks/namespace-baseline.json).

After removing listing-only entries from the mount-wide map, the same three-pass
fixture retained 501 views and ended at 49.9 MiB RSS. The remaining views are the
root and 500 directories resolved during traversal. The fixture creates no file
lookup references merely by reading names, so this improvement does not establish
bounded memory for mass `stat`/open workloads. See
[the correction measurements](../benchmarks/namespace-listing-lifetime.json).

With parent leases, the same three-pass 500k-file fixture now returns from 501
views immediately after traversal to the root alone after normal kernel
invalidation and collector progress. No kernel references are discarded to meet
that count. This release-build run recorded post-invalidation RSS of 21,504,
31,696 and 40,824 KiB: view retirement does not explain or solve the remaining
memory slope. The earlier artifact used a debug build, so timings and absolute
RSS are not a controlled before/after comparison. See
[parent-lifetime measurements](../benchmarks/namespace-parent-lifetime.json).

A separate smaller kernel fixture exercises eight-level paths and two shortcuts
to the same shared tree, each with 2,000 leaves. Held files and a continued
directory snapshot preserve their ancestor chains across invalidation and remote
rename. Closing those users leaves only the root; offline revisit preserves
distinct alias inodes without provider requests. This is not combined 500k-file
deep/alias or long-session coverage.

A separate actual-kernel fixture now stats 300 files, holds an old file open across
a revision change, and observes all other regular-file views retire after kernel
invalidation. Old and new open versions remain distinct; after close and FORGET,
both retire. This proves the tested reference lifetime, not the full memory gate.
The FUSE wrapper's entry/create/open replies return no delivery outcome; accounting
must not guess away references merely because a caller was interrupted. An actual
kernel fixture now kills a client while cold LOOKUP or OPENDIR waits for a provider
page, then lets the server finish its original request. Both cases release request
permits, snapshots and lookup references through normal kernel cleanup, and permit
an offline revisit of the completed metadata. This required no production-state
change. Failed reply writes, interrupted experimental CREATE and broader I/O races
remain outside that fixture and retain their acceptance gap.

Foreground directory publication now stages one provider page at a time in a
connection-owned, disk-backed TEMP database, with atomic publication after the
terminal page. Input/page/node/cursor limits, a 512 MiB TEMP page limit and two
builders per account replace the old full-list/100k-entry boundary. Negative
observations, current moves elsewhere and logical ticket ordering retain their
previous semantics. Canonicalizing visible nodes individually preserves semantic
unchanged detection when legacy JSON omits default fields. Bulk SQL observes
cancellation and the shared fetch deadline; cached readers can retain the old view
while the final writer is paused. Compatibility publication/collection APIs and
writable overlays still retain complete lists.

A separate 500k-row Store run covers cold, unchanged and changed publication. Its
TEMP database grows to approximately 141, 226 and 239 MiB respectively, including
comparison rows and indexes. Process peak RSS stays below 23 MiB in that fixture.
These are different resources: TEMP rollback journals, transient SQL files, the
main database/WAL, filesystem allocation and kernel page cache are additional.
Temporary files on tmpfs consume host memory outside process RSS. This three-pass
fixture does not establish a 24-hour memory plateau. A separate actual-kernel cold
listing fixture verifies 500k entries, offline revisit and remount without a
completed delta index. See [the measured scope and raw results](../benchmarks/directory-publication.json).

Metadata invalidation now consumes schema-6 revision-index pages and selects live
views through target/source identity indexes. Marks and view notifications have
separate bounded page sizes; notifications never hold the namespace lock. Directory
content changes preserve unrelated child dentries, while item/name changes and scope
resets retain their required invalidations. Watch generations keep hints received
during work; local writes, experimental writable projections and recovery still
use paged full sweeps. Unit tests cover revision replacement/reset during pagination,
atomic migration failure, alias/account selection and view retirement. Actual mounts
cover held old versions, duplicate
shortcuts, source-link rename and target-scope reset. See
[the synthetic update/burst measurements](../benchmarks/targeted-invalidation.json).
This does not close resident payload budgeting or the 24-hour gate.

Immutable view payloads now share reference-counted scope, alias/ancestry routes,
node metadata and presentation names. File siblings reuse unchanged parent routes;
folder/shortcut projection detaches only routes it extends. Reverse-index target
scopes and notification names share those allocations too. Local binding/metadata
updates detach their payloads, preserving held older views and parent leases. No
new intern pool retains data after its users close. Persisted inode-key encoding is
unchanged, including aliases and writable local identities.

A separate representation fixture retains 50,000 views across three 12-level routes
(one ordinary route and two shortcuts), including the production live index and
32 held clones. It compares the owned and shared representations in separate release
processes, then retires every view except the root. This isolates duplication; it
has no kernel, SQLite or provider workload. The larger 500k variant and kernel
invalidation regression are recorded separately in
[the shared-payload measurements](../benchmarks/shared-projection-payloads.json).
High RSS after logical retirement still needs allocator/storage attribution. This
correction does not provide evictable byte accounting, reconstructible disk-backed
views, combined 500k kernel churn or 24-hour acceptance.

A combined actual-kernel runner now exercises 12-level paths, duplicate shared-tree
aliases and stat of every projected file, with 24 held old file descriptors and
three stable directory snapshots across changes. It supports three full passes,
optional sustained churn in a fixed active set, and offline remount. Metadata seeds
and application work queues remain bounded. Reference/index/candidate counts and
persistent inode/database growth are recorded separately from RSS/PSS and snapshot
storage. It has small correctness and short-duration validation; the full 500k and
24-hour gates below remain unverified. Zero-length files do not add mapping/content
coverage. See [reproduction and interpretation](../development.md#combined-namespace-traversal-and-churn)
and [the fixture-validation record](../benchmarks/namespace-churn-fixture.json).

## Planned lifetime model

1. Track kernel lookup references, open file/directory leases, in-flight requests
   and dependencies needed by retained local edits separately from cache residency.
   Handle single and batched FORGET with checked counts; preserve the root.
   FORGET alone must not remove an open file, a mapping's version or a protected
   ancestor. Readdir without readdirplus must not manufacture lookup references.
   Follow the [FUSE lifetime contract](https://libfuse.github.io/doxygen/structfuse__lowlevel__ops.html).
2. Use a byte-budgeted cache for unreferenced views, with compact shared identity
   and ancestry storage. Reconstruct evicted projections from persisted metadata
   and explicit projection identity, including account, collection, item, alias
   route and content revision. The existing inode-number mapping alone does not
   store enough information for this. Eviction must not force a cloud request for
   a directory already indexed locally or confuse two links to the same target.
3. Preserve stable directory offsets through bounded pages or disk-backed snapshots
   rather than a complete in-memory `Vec<View>` per open directory. Anonymous
   snapshots and logical admission budgets now implement this for open handles;
   their last descriptor releases storage without restart recovery.
   Keep old directory snapshots stable across concurrent rename/delete operations.
   Bound materialization through the entire store/engine/projection pipeline;
   paging the final snapshot alone leaves the earlier `Vec<Node>` allocation.
   Cold foreground publication now stages pages under byte/storage/page limits
   instead of a 100,000-entry vector limit. The 500k cold fixture covers publication,
   listing and offline remount. Compatibility collectors and writable overlays
   remain outside the streaming path.
4. Metadata invalidation now uses an index of affected live projections and bounded
   coalesced work, including target/source aliases. Local namespace and recovery
   notifications retain a paged full sweep. Extend the current synthetic burst
   measurements to combined deep/alias/held-reference and sustained workloads;
   avoiding retained views must not lose live invalidations.
5. Count resident, evictable, referenced and protected bytes/entries, directory
   snapshot storage, invalidation work and persistent inode/alias history separately.
   Record process RSS/PSS as well as logical cache usage; allocator retention and
   SQLite caches can hide behind an apparently bounded entry count.

An application can keep arbitrarily many references open. Do not discard live
state to claim a constant total RAM ceiling: use compact/disk-backed records and
document admission limits for new work. Pending or conflicted local edits and their
recovery paths are never ordinary cache-eviction candidates. Persistent inode and
journal-history collection requires separate identity/recovery checks.

## Release acceptance

This is an explicit **milestone-1 / OneDrive-1.0 blocker**, independent of the
[Graph read-efficiency gate](0004-read-session-efficiency.md).

- [ ] Repeat three full traversal/close/revisit rounds over a synthetic 500,000-file
      library, including deep paths, duplicate SharePoint links and changing file
      revisions. Test both many directories and one very large directory.
- [ ] With kernel references released and a fixed active working set, resident
      view counts and memory plateau instead of following all previously visited
      paths/revisions. Proposed benchmark budget: 64 MiB of evictable namespace
      data; logical evictable bytes must stay within it.
- [ ] For the read-only traversal fixture with downloads disabled, at most 32 held
      files/mappings and eight held directory handles, target additional process
      RSS below 256 MiB over the ready indexed baseline. Record peak/steady RSS,
      PSS, SQLite buffers and snapshot storage. This is a proposed acceptance
      target, not a measurement or a universal limit for arbitrary applications.
- [ ] Keep open handles/mappings, stable directory cookies, old file versions and
      protected local-edit ancestors correct through eviction, unlink, replacement,
      remote invalidation, interrupted I/O and remount. Include explicit FORGET
      races and checked reference-count underflow tests.
- [ ] Revisit cached metadata offline without additional provider requests.
      Preserve distinct aliases, inode identities and local recovery routes.
- [ ] Measure bounded invalidation batches during change storms on that library;
      cached navigation must remain within the existing 500 ms synthetic bound.
- [ ] Complete a 24-hour namespace-churn run and a representative real-library
      check. Report workload, reference counts and memory slope; a mount that sits
      idle for 24 hours does not close this gate.

Shared immutable payloads reduce duplication, but byte budgets, further compaction/
reconstruction and streaming of remaining compatibility/writable consumers remain
unimplemented. Targeted invalidation has narrow synthetic coverage;
combined and sustained acceptance remains open. Planned limits must not be advertised
as supported capacity until their tests pass.

## Compact resident representation

The simpler resident path uses one ordered inode map for lookup and full invalidation
traversal, avoiding a separate all-inode index and a box per map entry. Reverse-index
item strings have no unused Arc header. Node boxes its uncommon remote target; JSON
and persisted inode keys are unchanged. Publication examines at most eight already
indexed projections for an exactly equal scope/node, reusing their immutable payload
and equal name. Aliases, old versions and parent/reference lifetimes remain separate.
No additional cache, payload file, database or intern registry is introduced.

Fresh release processes compare the same three-route, twelve-level fixture with
32 held clones. At 500k projections, populated RSS is 457304 KiB before these changes
and 366124 KiB afterward; population takes 622.87 ms and 692.47 ms respectively. Three 50k
pairs record similar memory reduction. This is a representation measurement, not
kernel throughput or real-provider capacity. Logical retirement still leaves high
RSS; the change does not establish a memory plateau or a byte budget. Raw results,
source hashes and executable hashes are in
[the comparison](../benchmarks/compact-resident-metadata.json).

The 256 MiB additional-RSS budget above remains a proposed benchmark target. Selecting
further storage machinery requires measured benefit across realistic and extreme
workloads, with the same correctness and latency checks. Simplicity does not close
those gates, and passing narrow tests does not justify extra state on its own.

The compact representation now also has separate full kernel comparisons with
500k indexed/750k projected files, in both many-directory and giant-directory
topologies. Each variant passes three complete stat traversals and offline remount,
retiring all views except the root and releasing snapshots. Final released RSS is
about 15 percent lower for many directories and 18.7 percent lower for giant
directories. High retained RSS and the unmeasured 24-hour plateau remain open.
The giant pair used explicit Btrfs temporary storage without competing local builds;
the earlier many-directory timing/backing limitations and full raw records are in
[the comparison](../benchmarks/compact-namespace-churn.json). These zero-byte
fixtures predate the mapped-content and early-prefix changes; they do not validate
those variants at scale or establish a total-host-memory or resident byte ceiling.

## Direct attribution of the memory gate (2026-09-08)

The gate has two distinct failures that earlier records treated as one. A frozen
release binary carrying the compact representation ran
`real_combined_namespace_churn` at `CIRROVE_CHURN_FILES=500000` under an
`LD_PRELOAD` `mallinfo2` interposer sampling on every fixture marker, with the
temporary directory on btrfs rather than tmpfs so that no bytes hide outside
process RSS. See [the attribution run](../benchmarks/namespace-memory-attribution.json).

**The traversal peak is live application data.** At 750,438 live views the
allocator reports 495.0-510.3 MB allocated across three rounds, that is 637-657
bytes per view over a 17.3 MB baseline. No allocator setting reduces this;
only a bound on resident view count or on bytes per view does.

**The retained floor is allocator-held.** After release the live heap returns to
17.8-18.1 MB while RSS stays at 489.5-559.8 MB, so 96.4-96.8 percent of retained
RSS is free arena that glibc has not returned. Released RSS tracks the peak to
within 0.3 MB. No view budget reduces this; only trimming does.

Peak RSS rises 38.1 and 32.0 MB between rounds while the live peak stays flat.
The inter-round slope on this fixture is therefore allocator retention, and it is
three to four times the roughly 10 MB per pass recorded on `namespace_capacity_baseline`,
which never exceeds 501 live views and is not representative of the gate.

Released PSS above the indexed baseline is 476.4 / 514.8 / 546.7 MiB against the
256 MiB budget: missed by 1.86x, 2.01x and 2.14x, with the miss growing each round.

This corrects an earlier statement in this document. The heavy fixture does
converge: its traversed peaks decay geometrically rather than rising without
limit. It converges at 476-771 MiB, which is the actual failure. The absence of a
plateau reported for `namespace_capacity_baseline` is the signature of glibc arena
growth under a fixed workload, not of unbounded namespace retention; retained view
counts return to one in every recorded run.

The two remedies are complementary, and neither alone closes the gate. This run
is attribution only: one binary, one run per phase, no before/after control.

## Acceptance criteria, bound to named fixtures (2026-09-08)

Until now this gate has been described in prose and measured on whichever fixture
was to hand. The two fixtures differ by two orders of magnitude in what they
stress, so a change can improve one while doing nothing for the other. These
criteria name the fixture, the metric and the number, so that a result either
passes or does not.

The workspace currently contains exactly two memory assertions:
`filesystem::capacity::cold::…` (capacity/cold.rs:337-344, `peak_rss_kib` minus
`rss_kib` under 256 MiB on cold publication) and a 64 MiB bound in
`cirrove-store/src/directories/tests/capacity.rs:126`. Neither is on the churn
fixture. The namespace memory gate is therefore measured but not asserted
anywhere, which is why it has been possible to record progress against it for
weeks without it ever failing a build.

**Metric.** PSS is the gate. `Pss_Anon`, `mallinfo2` live heap and arena committed
bytes are attribution terms, not gates. `VmHWM` is a separate headroom bound. All
four are already emitted by `capacity.rs:151-168`.

Anonymous PSS must not be the gate on its own: it would let any design that
relocates bytes into a file pass by placing that file on tmpfs. That is the same
objection which already rejected SQLite TEMP for the payload path, and it applies
directly to any anonymous-file scheme in the account state directory.

Unconstrained cgroup `memory.peak` must not be the gate either. It is dominated by
clean page cache, which grows to fill available memory, so it would look worst on
the largest machine — backwards for a requirement about running on any hardware.

| Gate | Fixture | Criterion |
| --- | --- | --- |
| G3 resident bound | `real_combined_namespace_churn`, `CIRROVE_CHURN_FILES=500000`, both topologies | `released` PSS minus `indexed_baseline` PSS at most 256 MiB, every round |
| G2 evictable bound | same | a named `resident_bytes.evictable` counter at most 64 MiB, with its charge formula written here before the counter is built |
| G7 sustained plateau | same, sustained mode | over the final twelve hours of a twenty-four hour run, post-settle PSS must not exceed the hour-two sample by more than X percent |
| Survivability | same, under `MemoryMax` with `MemorySwapMax=0` | no OOM kill; assertions intact |

G3 currently fails at 476.4 / 514.8 / 546.7 MiB, missing by 1.86x to 2.14x.

X in the plateau rule must be fixed from a two-hour pilot on `main` before the
twenty-four hour run, not chosen by intuition. The only sustained datum that
exists is a sixty-five second debug run rising 9.7 percent per round, so a rule
picked blind would fail both arms inside the first hour.

Two prerequisites block G7 as written. `capacity/churn.rs:414` skips
`parents::settle` unless the round is full, and the sustained loop at :491-508
runs only non-full rounds, so no sustained sample is post-invalidation root-only
and there is no series a plateau rule can be applied to. Sustained mode also
carries no memory assertion at all.

**G8, the persistent inode table.** Measured across two 500,000-file runs: rows go
from 1 to 750,495 and the database file from 565.6 to 662.7 MiB. The first pass
adds one row per projected view, about 97 MiB; each round after it adds 27, one
per changed file. An earlier working note put this at 154 MiB per pass, which is
both the wrong figure and the wrong axis.

The axis is revisions, not passes. The key embeds the content revision, so every
revision of every file mints a permanent row, and `crates/` contains no
`DELETE FROM inodes` anywhere. A library churning steadily therefore grows this
table without bound for the life of the account. Because it is file-backed it
contributes nothing to process RSS and would be reclaimed under any cgroup cap,
so every memory criterion above passes while it grows.

**Criterion:** on `real_combined_namespace_churn` at 500,000 files, `inode_rows`
after three rounds must not exceed the projected view count by more than one
percent, and the sustained arm must not add more than one row per changed file per
round. That bounds the shape rather than the size, which is the right bound while
no pruning path exists.

Pruning is not cheap and should not be assumed. The table is the persistent inode
key and the fixture asserts inodes stay stable across remount, so naive deletion
renumbers a user's library on upgrade. Recording the criterion now at least means
a regression in the shape fails a build; a pruning design is separate work that
may end up deferred with a written justification rather than done.

## The paged-payload prototype does not reach the gate's own workload

`feature/paged-view-payloads` (32dc281) moves projection payloads into a per-mount
anonymous file. Its production arena is fixed at one gibibyte:
`Store::new` passes `1 << 30`, and `with_file` rejects anything outside
`512..=1 << 30`, so the size is a constant and not configuration. Extents are
rounded to powers of two with a 512-byte minimum.

That makes capacity a function of encoded record size alone:

| encoded record | extent | records in 1 GiB |
| --- | ---: | ---: |
| up to 512 B | 512 B | 2,097,152 |
| up to 1 KiB | 1 KiB | 1,048,576 |
| up to 2 KiB | 2 KiB | 524,288 |
| up to 4 KiB | 4 KiB | 262,144 |

`real_combined_namespace_churn` at 500,000 files holds 750,438 live views, which
allows at most 1,430 bytes per extent, so every record must encode below one
kibibyte for the prototype to cover the gate's own fixture. A record carries a
36-byte header plus a serialised projection with scope, alias and ancestry
vectors, presentation name and identity keys; deep paths and long shared-link
identifiers make one kibibyte a tight ceiling rather than a comfortable one.

Exhaustion is not graceful. The allocator returns `ENOSPC`, which reaches the
kernel from `lookup`, so the failure mode is a namespace operation failing rather
than a payload being spilled or re-fetched.

The prototype has never been run at 500,000 files or in a release build, and it
has committed no benchmark JSON, so this is a structural reading of its
constants rather than a measured failure. It is recorded because the arithmetic
is decidable without running anything: a fixed one-gibibyte arena cannot be sized
to a library, and the gate names a workload it cannot hold at any record size
above one kibibyte.

The measured attribution above makes the prototype's premise weaker still. Its
16 MiB cache and off-heap records address the traversal peak, but 96 percent of
retained RSS is allocator-held free arena that no relocation of live bytes
reduces. Off-heap payloads therefore address the smaller of the two failures,
and only for as long as the arena holds.

### Replication

A second independent run of the identical configuration, same frozen binary,
separates the two components further. Live heap agrees to within 0.1-0.8 percent
at every phase, and bytes per view at the traversal peak come out 653/637/656
against 653/637/657: the quantity the resident-bound work depends on reproduces
to within 0.2 percent.

Retained RSS does not. Rounds two and three differ by 5.9 percent between runs,
and G3 lands at 477.3/483.5/513.4 MiB against 476.4/514.8/546.7 MiB. All of the
run-to-run variation is in the allocator-retained component, which depends on
thread scheduling and allocation interleaving, and none of it is in the live data.

Two consequences. The live-peak figure is solid enough to justify building a
resident bound on it. The retained-RSS figures must not be quoted to three
significant figures from a single run, and any allocator comparison needs
replicated arms rather than one run per arm. G3 fails in both runs in every
round, by 1.86x to 2.14x.

## Allocator configuration does not reach the gate (2026-09-08)

Every allocator diagnostic before today ran `namespace_capacity_baseline`, which
never exceeds 501 live views. Four runs of the heavy fixture at 500,000 files,
same frozen binary, tested whether any glibc configuration reaches the budget.
See [the arms](../benchmarks/namespace-allocator-arms.json).

| arm | tunables | G3, MiB |
| --- | --- | --- |
| default | none | 476.4 / 514.8 / 546.7 |
| default | none | 477.3 / 483.5 / 513.4 |
| trim | `trim_threshold=131072` | 474.3 / 534.0 / 642.3 |
| full | `arena_max=2` + trim + `mmap_threshold=131072` | 478.3 / 571.0 / 607.8 |

Round one lands at 474.3 to 478.3 MiB in every arm, a spread of 3.9 MiB and about
1.86 times the 256 MiB budget. No configuration comes near it. The pre-registered
rule is therefore settled against the allocator lane: bounding resident views is
required, and tuning cannot substitute for it.

Nothing further should be read into these numbers, and two readings that suggest
themselves are wrong.

**They do not show that tuning is harmful.** The later rounds diverge, but the two
untuned runs differ from each other by 6.2 against 38.4 MiB on the same round step,
so the untuned spread is itself sixfold. The arms also ran sequentially through one
shared temporary directory and are confounded with run order; the only arm with a
private untouched directory recorded the lowest round-one value. Comparing absolute
G3 beyond round one across single runs is not supported.

**`mallinfo2` alone does not measure live heap.** It reports mmap'd chunks in
`hblkhd`, not in `arena` or `uordblks`. Setting either threshold also sets glibc's
`no_dyn_threshold`, after which large allocations stay mmapped: at release the
tuned arms report 1.1 to 1.6 MiB in `uordblks` against 16.9 to 17.3 MiB untuned,
which invites the conclusion that they retain far more. Adding `hblkhd` gives 17.1
to 17.6 MiB in every arm. Live heap is the sum; a ratio built on `uordblks` alone
counts live mmap'd data as allocator-held free memory.

One clean signal did separate. Traversal wall time is 573 to 575 seconds in the
`full` arm against 526 to 550 across the other three, every round within two
seconds, and the fastest run was also the last, so this is not machine drift. That
is `arena_max=2` contending eight stat workers over two arenas, and it is an
argument against shipping `arena_max` on cost rather than on memory.

## malloc_trim reaches what the tunables could not, and that re-scopes the gate

Every allocator arm recorded above configured `GLIBC_TUNABLES`. `trim_threshold`
governs trimming the top of an arena on `free()`. `malloc_trim(0)` is a different
operation: it walks every arena and returns free page ranges within each heap. No
recorded arm had asked that question. See
[the probe](../benchmarks/namespace-malloc-trim.json) and
[its pre-registration](../benchmarks/namespace-malloc-trim-preregistration.md),
written before the run.

| round | G3 before | G3 after `malloc_trim(0)` | returned |
| ---: | ---: | ---: | ---: |
| 1 | 476.4 MiB | **10.2 MiB** | 466.2 MiB |
| 2 | 466.8 MiB | **12.4 MiB** | 454.3 MiB |
| 3 | 479.6 MiB | **14.2 MiB** | 465.4 MiB |

G3 goes from 1.86 times over its budget to twenty-five times under it. The predicted
range was 40 to 150 MiB, so the effect is larger than expected, and the named failure
mode — fragmentation leaving one live object per page — did not occur. Live heap holds
at 17.0 to 17.2 MiB while RSS falls to 23.5 to 27.4 MiB, so RSS is live heap plus
overhead and the pages are genuinely returned rather than merely unaccounted.

**This is not the win it looks like, and the pre-registration said so before the
number existed.** `malloc_trim` does not touch the traversal peak, which stays at
489.0 to 492.6 MiB with 750,438 live views. G3 measures what remains after release;
whether the service runs on modest hardware depends on what it holds during
traversal. A process can now pass this gate while its high-water mark sits at half a
gibibyte.

Two consequences follow.

**Shedding is re-scoped rather than cancelled.** It moves from required-for-G3 to
required for the peak and for survivability under a memory cap. That is a difference
of weeks of work, and it is the honest reading: an idle-gated trim is cheap, and the
resident bound is still the only thing that lowers the high-water mark.

**G3 needs a sibling criterion.** Adding `VmHWM` minus the indexed baseline, and
promoting the cgroup survivability arm from a nice-to-have to a hard gate, restores
what G3 was written to mean. This tightens the acceptance criteria, and it arrived
from a result that superficially reads as a pass. It is recorded as a re-scoping.

The trim itself is not yet shipped. It must be idle-gated — earlier measurements put
it at 20 to 26 milliseconds median and 62 milliseconds maximum at around a gibibyte,
and it takes every arena lock in turn, so it belongs on a quiescent reclamation tick
via a blocking worker and never on a runtime thread that owns a filesystem reply.

## The charge formula for `resident_bytes`

G2 bounds a named `resident_bytes.evictable` counter, and this document requires
its charge formula to be written before the counter exists. This is that formula.
It is recorded first so that a counter which disagrees with measurement is a
falsified model rather than a formula quietly adjusted until it agrees.

A resident view is `Entry` in `NamespaceViews::entries`: a `View`, a generation
and a queued flag, in a `BTreeMap` node. `View` owns seven reference-counted
payloads, and three of them are deliberately shared between siblings by
`ProjectionIndex::matches`: `scope`, `alias` and `ancestry`. `residency` is shared
differently — every child holds its parent's through `_parent_residency`, which is
what keeps an ancestor chain alive.

**Charge each distinct allocation once, not each view that can see it.** Walk the
entries collecting `Arc::as_ptr` for every payload, sort, deduplicate, and sum the
allocation sizes of the survivors. Crediting whichever view was inserted first and
debiting the same stored value on removal drifts the counter below truth by
roughly what the interning saves, which is about a fifth on the measured
representation, and an advertised bound that undercounts is not a bound.

The walk must not allocate while it runs. A pre-reserved `Vec<usize>` with a sort
and dedup, never a `BTreeSet`: 750,438 views times roughly five payloads is about
3.75 million node allocations into the very arenas being measured, which
permanently raises `peak_rss_kib` — one of the four scalars every comparison in
this document depends on.

It is `#[cfg(test)]` and never on a filesystem path. It takes the namespace lock
in front of a deliberately single-threaded dispatcher, so a hundred-millisecond
hold is head-of-line blocking on exactly the lookup path five commits were spent
de-stalling. Each sample records the walk's own duration, so a slow sample is
visible rather than averaged into the result.

**Validate before building anything on it.** Compare the modelled total against
measured `Pss_Anon` at every phase, both topologies. Agreement within 15 percent
accepts the model; more than that means the formula is fiction and no ceiling
resting on it is a bound. The fallback is a view-count ceiling derived from the
measured 637 to 657 bytes per view: weaker, because it cannot see a change in
per-view size, but honest, and it costs a day rather than a phase. That choice is
to be made in the open, not by loosening the tolerance until the model passes.

## Shedding: the design, before the code

Recorded before implementation for the same reason as the charge formula above.
Whether it is built at all depends on a measurement that has not been taken.

**What it is.** Above a resident ceiling, select entries and emit
`notify_inval_entry` for them through the batch machinery in
`filesystem/invalidation.rs`, which already runs on a blocking worker outside the
namespace lock and is already cursor-resumable. The kernel drops the dentry,
queues FORGET, and `NamespaceViews::forget` reclaims exactly as it does today.
Every reference count, lease and FORGET path stays untouched.

**Why not eviction with reconstruction.** The fuller design needs a reverse
inode-to-key lookup that `cirrove-store` does not have, a debt map for outstanding
kernel references, a single-flight gate and a hydration path, and it narrows
old-revision behaviour from serving stale attributes to `ESTALE`. It also makes
the never-pruned `inodes` table load-bearing for correctness, which makes pruning
it strictly harder later. Shedding reaches the same bound with none of that,
because the kernel's own re-lookup path is already proven to cost no provider
request: `foreground_requests == 0` is asserted at the end of the churn fixture.

**Selection.** `inode != 1`, not quarantined, `Arc::strong_count(&residency) == 1`,
ordered by the existing generation counter as a clock hand.

Four corrections are mandatory, each from a specific failure this codebase can
already exhibit.

1. **Backpressure the shed channel; never drop.** `ProjectionIndex::invalidation_batch`
   resolves candidates through `self.get(&inode)` and misses on anything absent,
   so a dropped notify silently removes that entry from every future change sweep.
   That is stale metadata with no error and no counter. Blocking when full also
   caps how fast unresolved state can accumulate.

2. **The pinned class is larger than it looks, and the fixtures are its best case.**
   `Inner::project` sets `_parent_residency` on every child, so every ancestor of
   every resident view has a strong count above one and cannot be shed. The
   ceiling is therefore hard over leaves and soft over the directory spine. The
   500,000-file fixtures have 500 directories, or three in the giant topology,
   where the pinned class is negligible. On a deep, wide library it scales with
   directory count. Report the pinned-class size as a first-class metric and claim
   a bound only where it is measured small, rather than assuming it.

3. **Shedding buys re-lookups, and re-lookup is expensive.** `filesystem.rs` opens
   a store connection per published view, and `lookup`, `getattr` and `opendir`
   gate on a 128-permit semaphore that replies `EAGAIN` when exhausted. A shed
   storm is a `Store::open` storm behind that gate, and `ls: Resource temporarily
   unavailable` is the failure a user would see. Requires hysteresis — shed down
   to 0.9 of the ceiling, never oscillate — a bounded shed rate, a pooled read
   connection, and an assertion in the churn fixture that no operation returns
   `EAGAIN` during a shed cycle. Add that assertion before the shed loop, so the
   failure is a red test rather than a field report.

4. **The kernel-side cost is unmeasured and is the gate on all of this.**
   `fuse_reverse_inval_entry` takes the parent's `i_rwsem` exclusively while
   `lookup_slow` holds it shared across a full round trip, against a
   writer-preferring rwsem and a deliberately single-threaded dispatcher. The
   cgroup experiment that suggested shedding is affordable measured the kernel's
   own dcache shrinker taking `d_lock` off the LRU with no FUSE upcall, and
   transfers no information about this. `benchmarks/inval-entry` measures it
   directly, outside this workspace.

**The bar, and what failing it means.** A 500,000-file traversal resolves about
750,000 views in roughly 530 seconds, so a ceiling that binds during traversal
must sustain about 1,400 sheds per second with concurrent-lookup p99 under 100
milliseconds. If that is not reached, shedding becomes a soft ceiling with a
measured overshoot factor rather than a bound, and may need disabling above some
entry density — that is, disabled exactly where density is highest. Everything in
this section is contingent on that number.

**Every claim here is for a single dispatch thread.** Raising `n_threads` to four
is a recorded failure, and shedding makes order-independent reference accounting a
harder prerequisite for concurrency later, not an easier one.
