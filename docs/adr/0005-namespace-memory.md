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
not limit these allocations. The invalidation worker also copies and walks the
retained view map on each coalesced change wake, adding CPU and temporary memory.

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
still collects a Vec, as do foreground publication, writable local-overlay
projection and point/name lookups. Cached read-only OPENDIR now streams, but the
remaining consumers keep the end-to-end paging gate open.

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
The FUSE wrapper's entry/create replies return no delivery outcome. Cancelled or
failed delivery can therefore leave conservative references; accounting must not
guess them away. Interruption and delivery-failure acceptance remain open.

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
   The current cold foreground listing also has a 100,000-entry limit, while a
   complete delta index can contain larger directories. Large-directory acceptance
   must cover both paths rather than simply raising this safety limit.
4. Replace mount-wide invalidation scans with an index of affected, live projections
   and bounded coalesced work. Measure allocation and navigation latency during a
   remote-change burst; avoiding retained views must not lose live invalidations.
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

Compact/budgeted view payloads, full-pipeline paging and
targeted invalidation remain unimplemented. These planned limits must not be
advertised as supported capacity until the tests pass.
