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
after the last in-flight reader or handle releases them, including process death;
they are not durable state and need no orphan sweep. Builders fail closed on I/O,
quota or cancellation errors. Idle handles keep no SQLite read transaction.
Construction still scans the directory before returning its first entry. The
SQLite inode table has its own persistent lifetime; neither its growth nor the
remaining view payloads are solved by snapshot paging.

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
seconds, so the memory reduction does not solve opening latency. The first
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
