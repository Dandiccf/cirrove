# Namespace lifetime and large-library memory

Status: directory-listing lifetime corrected; full reclamation and capacity acceptance remain open.

## Current evidence

`filesystem::Inner.views` is a mount-lifetime `HashMap<u64, View>`. Originally both
lookup and directory listing inserted projected views. A view owns metadata strings,
scope, alias and ancestry vectors and sometimes a second entry node. `releasedir`
dropped the handle's `Arc<Vec<View>>`, but left those entries in the shared view map.
Plain listing projections now live only in the directory snapshot; resolved lookup
and operation views still enter the shared map.
The filesystem does not override FUSE `forget`/batch-forget to reclaim them.
Repeated remote content revisions can also create additional inode/view identities.

This is growth with projected entries and revisions visited during the mount, not
automatic materialization of every item merely because the index contains 500,000
files. Resolved lookup views still accumulate after applications close their
handles, so full namespace lifetime remains a scalability gap. The content-cache quota does
not limit these allocations. The invalidation worker also copies and walks the
retained view map on each coalesced change wake, adding CPU and temporary memory.

Open directories currently retain complete snapshot vectors. The SQLite inode
table has its own persistent lifetime. Neither problem is solved by putting a
simple LRU around the shared map.

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
   rather than a complete in-memory `Vec<View>` per open directory. Account for
   snapshot disk space separately and collect abandoned snapshots after restart.
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

The implementation still retains resolved lookup/operation views until unmount. These planned
limits must not be advertised as supported capacity until the tests pass.
