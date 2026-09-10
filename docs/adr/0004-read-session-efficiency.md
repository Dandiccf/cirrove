# Read-session efficiency and Graph request amplification

Status: shared conditional sessions, bounded streamed windows and transport
diagnostics are implemented experimentally. Ordinary accounts still
use the original conservative reads; full provider/application acceptance remains
outstanding.

## Problem and present evidence

`OneDrive::fetch_range` currently fetches item metadata, downloads one range from
the returned signed URL, and fetches item metadata again before releasing bytes.
`ContentCache` invokes it separately for each cold 4 MiB block. With no retries,
a complete cold 1 GiB read therefore needs 256 content requests and **512 Graph
metadata requests**, excluding listing, open, authentication and background work.
Warm blocks do not repeat these requests. Merely listing a large library does not
trigger these block checks; opening/thumbnailing many cold files does.

The before/after checks compare content revision and size to the opened node. They
detect observed changes, but two metadata observations are not a provider-side lock
or a transactional snapshot. Removing them on the strength of a URL lifetime or a
missed change notification would not establish equivalent version evidence.

This is a release blocker in product milestone 1, not an accepted permanent cost
of using Graph or a generic request to measure performance later.

## Design and acceptance work

1. **Measure separately.** Count foreground Graph metadata, content-origin requests,
   authentication/renewal, retries and background reconciliation. Record first-byte
   and complete-read p50/p95, downloaded bytes, cache hit rate, memory and disk
   staging. Counters must not contain paths, signed URLs, tokens or provider bodies.
2. **Introduce shared version-bound read sessions.** Opening a remote content
   revision establishes one provider-neutral read context keyed by account,
   collection, item and content revision. Concurrent readers and ranges of that
   revision share setup and renewal. Provider handles and signed URLs remain
   adapter-private, short-lived and in memory; the cache stores content identity,
   not transport credentials. A session is not a claim that the provider locks a
   file. Its validation strategy must be explicit and proven.
3. **Prove a lower-request validation path.** Investigate content-origin strong
   validators/conditional ranges and immutable-version downloads where available.
   A Graph eTag/cTag must not be assumed to equal the content server's HTTP ETag.
   Successful ranges must prove the same representation and expected length;
   ignored conditions, weak/missing validators or a changed representation cannot
   silently become accepted bytes for the old cache key. Test the actual OneDrive
   Personal, Business and SharePoint responses before enabling a fast path.
4. **Decouple download windows from cache blocks.** Sequential reads can share a
   bounded contiguous transfer and its validation over several 4 MiB cache blocks.
   Keep the first small/sparse read small; grow subsequent windows only when access
   is sequential. Where before/after validation is still necessary, stage that
   bounded window before publishing its blocks. Stream staging to disk under an
   explicit quota rather than allocating a whole large file in RAM. A 32 MiB
   validation window would reduce the above large sequential read to 64 metadata
   checks instead of 512; that is an intermediate fallback target, not the final
   per-session fast path or a promise about random access.
5. **Preserve recovery boundaries.** Coalesce expired-URL renewal; respect shared
   Retry-After, cancellation and account shutdown. Renewal must verify the same
   content identity before continuing. Remote invalidation prevents obsolete
   publication, while lost notifications must not permit mixed-version content.
   Changed or unverifiable content produces an explicit stale/error result. Cached
   old bytes remain associated with their own revision. Abandoned staging is
   restartably collected without touching pending local edits.

The shared service owns request coordination, cache block publication and budgets.
Each provider supplies its concrete content identity and validation strategy. This
keeps Google Drive or another adapter independent of Microsoft URL conventions;
equivalent immutable-download capabilities must not be presumed for every provider.

## Microsoft constraints checked during planning

Microsoft documents signed current-content URLs as short-lived and advises using
them immediately. It documents ranged content requests, including the possibility
of a full `200` response when Range is ignored. The current-content endpoint lists
`If-None-Match`; that alone is not proof of version-pinned range downloads. See
[Download driveItem content](https://learn.microsoft.com/en-us/graph/api/driveitem-get-content?view=graph-rest-1.0).

The separate historical-version content API explicitly excludes the current
version. It is therefore a potential historical-read mechanism, not a universal
way to pin every newly opened current file. See
[Download driveItemVersion content](https://learn.microsoft.com/en-us/graph/api/driveitemversion-get-contents?view=graph-rest-1.0).

No fast path is accepted merely because a signed URL stays usable or a synthetic
server honors an unsupported conditional header.

## Transport observations and the next implementation boundary

The developer command `cirrove inspect-onedrive-read` now observes one explicitly
selected file using a normal range GET, deliberately mismatched If-Match/If-Range,
and matching conditions when the content origin supplies a syntactically strong
HTTP ETag. It retains at most 4 KiB per successful sample and does not consume
full-file 200 responses or error bodies. This is a separate diagnostic, never
called by the normal filesystem. Metadata and content operations are read-only;
the configured credential broker may refresh authentication as usual.

```sh
cirrove inspect-onedrive-read --label ACCOUNT --item ITEM_ID \
  --state-dir /absolute/path/to/account-state
# Add --drive DRIVE_ID for an explicitly selected linked collection.
```

The JSON contains statuses, timings and comparison booleans, without file paths,
item IDs, tokens, URLs, raw ETags, byte fingerprints or file contents. It records
logical operations, not wire-request counts: redirects and authentication retries
are excluded. The CLI obtains the selected node before invoking the probe, so its
initial metadata lookup is additional to the two reported validation operations.
A null byte comparison means no complete pair of samples was available.

On 2026-09-07, isolated credentials for one business account observed an existing
synthetic OneDrive file and one existing file in a linked SharePoint library.
Both content origins supplied strong ETags, rejected the deliberately wrong
If-Match with 412, and returned exact 206 ranges with matching ETags and sample
bytes for matching If-Match and If-Range. Wrong If-Range produced 200, whose body
was not consumed. Metadata before and after agreed. Private evidence is retained
locally; these are two file observations, not tenant-wide or provider-wide proof.

These results support implementing a version-bound session prototype using the
**content origin's own strong ETag**, established alongside the current Graph
before/after checks. Later ranges must validate their returned representation,
range and size before cache publication; conditional request success by itself is
insufficient. Setup/renewal must coalesce, bound URL/authorization lifetime and
revalidate the original content revision. Personal accounts, remote replacement,
revocation, expired URLs, redirects and lost notifications still need acceptance.
These observations alone did not enable an optimized path or close an acceptance gate.

## Conditional-session prototype and measured increment

`ReadProvider::open_read_session` now has an optional provider-neutral contract.
The service coalesces creation and limits residency to 64 identities; active entries
cannot be evicted. Entries idle for 60 seconds are pruned on later use. Saturation
uses the existing exact-range contract rather than adding unbounded sessions.
OneDrive selects the session path by ordinary account construction as of
2026-09-10. `with_read_sessions` and `without_read_sessions` name an arm
explicitly, which an A/B measurement needs so that its control does not silently
follow a change to the default; `CIRROVE_CONSERVATIVE_READS=1` is the operational
route back for an account that misbehaves under sessions.

The first requested range performs the existing Graph before/after checks and
binds the content origin's own strong ETag. Later ranges validate both the condition
response and the exact representation before returning bytes. A transport binding
has a 60-second lease. Renewal/rebinding is coalesced and checks the original Graph
content revision/size/identity again; changed content is rejected. Initial missing
or weak validators select conservative validation. The streamed-window increment
below now amortizes that validation for sequential cache misses without a whole-file
allocation. Direct exact-range calls still perform their own before/after pair.

The synthetic adapter fixture streams a 1 GiB file in 64 KiB server chunks and reads
it as 256 individual 4 MiB ranges. It observes **2 Graph metadata requests, 256 content
requests and exactly 1 GiB of content bytes**, with 255 conditional ranges after one
setup. This covers the adapter, not a full 1 GiB kernel/application benchmark.
3.1 MB preview/random-range and 32-reader fixtures cover shared setup; concurrent
expiry/rejected-URL recovery observes one renewal and four Graph requests total.
Other fixtures cover changed versions even when If-Match is ignored, metadata-only
origin-tag changes, wrong identities, invalid/short/oversized/encoded responses,
weak/missing validators, cancellation and shared throttling. Separate cache tests
exercise session reuse, identity separation, bounded residency, cancelled setup,
unrelated-file progress, and persisted blocks after cache restart.

The GET-only `validate-onedrive-read-session` command exercises five ranges of at most
256 KiB through one session, including four concurrent ranges, then compares all
five to independent conservative reads. Its output includes cumulative adapter
counters before/after each phase and elapsed times. Initial CLI node lookup is in
the `before` snapshot; authentication and automatic redirects remain excluded from
these counters. It retains at most five sample buffers plus one comparison, not a
whole arbitrary file. It does not change mount settings or enable ordinary sessions.

An isolated business-file run on 2026-09-07 observed one setup (two Graph calls),
then four conditional content reads with **zero additional Graph calls**. All five
samples matched the conservative comparison. First-range time was 1,199 ms and the
four concurrent ranges took 237 ms together; these are one-run sample timings,
not p50/p95, cold desktop-open measurements or cross-client benchmarks. A linked
SharePoint file larger than 1 MiB passed the same five 256 KiB sample comparisons:
708 ms for setup/first range and 172 ms for the four concurrent ranges together,
again without additional Graph calls after setup. A separate smaller SharePoint
file also passed, but its samples covered the whole file. Real checks of
replacement, revocation, URL renewal and Personal accounts remain open.

## Bounded sequential-window fallback

The shared cache now consumes an optional streamed-window contract on `ReadSession`.
The adapter writes bounded untrusted chunks to a caller-owned sink and reports
success only after validating the whole requested window. OneDrive checks the
original Graph identity/revision/size before and after the streamed HTTP range;
status, range, encoding and exact byte count retain the existing validation rules.
Both fallback and strong-validator sessions now advertise windows after the first
validated read. The fallback keeps the Graph before/after pair; strong windows use
the conditional validation described below without adding per-window Graph calls.

The service starts with a normal 4 MiB cache block. Sequential misses then grow
8/16/32/64 MiB windows, bounded by remaining file size, adapter capability, observed
transfer speed and staging capacity. The sizing target is five seconds at the
observed end-to-end transfer rate; the actual request deadline remains 30 seconds.
Random access and abandoned/failed transfers reset growth. Saturation falls back
to an exact-range read; an unrelated file does not wait for another window's quota.
Overlapping requests share window creation/validation. Already validated blocks
use the same checksum, durable publication and restart behavior as other cache data.

Up to a quarter of the configured cache allowance (4 MiB units, maximum 128 MiB)
is reserved for staging and subtracted from the persisted-block quota. Less than
8 MiB disables windows. Anonymous temporary files disappear when their last handle
closes, including after process exit. Each queued/blocking write keeps the file and
reservation alive even if its async caller is dropped. Source chunks are at most
64 KiB; each staged block has a hash captured during streaming and verified before
publication. Incomplete, invalidated or corrupt windows never become cache blocks.
Kernel ENOSPC, interrupted writes, quota saturation and cancellation have fixtures.
This is not a completion claim for pinned/edit storage or all physical-fault gates.

A test using the **actual OneDrive HTTP adapter, shared session pool and disk cache**
reads a synthetic 1 GiB file with a weak origin ETag. It directly counts 40 Graph
requests and 20 content requests: two single blocks and eighteen streamed windows.
That is a **12.8-fold reduction** from 512 Graph checks. Exactly 1 GiB of content is
transferred; the peak staging reservation is 64 MiB and returns to zero. The last
persisted block remains readable after cache restart without another HTTP request.
The service's development dependency enables a loopback-only synthetic adapter
constructor with fixed fake credentials; ordinary builds do not enable that feature.

One isolated debug-build run on 2026-09-07 recorded 39,690 ms total, 51.25 ms for
the first 4 MiB block, and per-block p50/p95 of 96.78/894.45 ms. RSS sampled after
blocks rose from 21,561,344 to 77,561,856 bytes. The test rejects growth of 256 MiB
or more. These samples do not include all kernel page-cache memory or prove a
production RSS ceiling. They are loopback/cache measurements, not a release-build
throughput result, FUSE desktop measurement or cloud/provider comparison.

```sh
cargo test -p cirrove-service --lib --locked \
  graph_gibibyte_through_shared_disk_cache_uses_forty_metadata_requests -- --nocapture
```

The timing and memory report is JSON and contains no account data. Additional
fixtures cover small/sparse reads, identity separation, shared overlapping reads,
other-file progress, low quota, failed final version checks, invalid bodies,
corruption, cancellation and retaining reservations for in-flight blocking I/O.
The wider kernel/application, real-provider and failure acceptance below remains
open; this increment does not enable optimized ordinary mounts.

## Streamed windows through kernel FUSE

Three actual-kernel fixtures now run ordinary positional file reads through the
OneDrive loopback adapter and shared cache. They pause an 8 MiB window after its
first 4 MiB, then pause the final Graph comparison after the entire body has reached
the staging sink. Two overlapping readers receive no bytes until both transfer and
validation succeed. An independent file and a distant range of the same file remain
readable; repeated cached directory listing plus stat calls retain a 500 ms deadline.

The success case uses one shared window, four total content requests and eight Graph
requests, including the initial block and both independent reads. A same-size version
change at final validation returns ESTALE to both window readers without publishing
their blocks. Cancellation during the partial body returns ENODEV; unmount completes
with the test descriptors still open. Staging reservations return to zero in all
three cases. CI runs these fixtures explicitly and compiles their executables before
starting runtime deadlines.

One local debug run on 2026-09-07 recorded 80 directory samples across these cases,
with per-case p95 values of 1.79–2.01 ms and an overall maximum of 4.32 ms. These are
controlled loopback stalls with transactionally seeded metadata, not live indexing,
desktop thumbnail bursts, first-byte performance or provider latency measurements.
The wider application/load and real-provider gates remain open.

```sh
cargo test -p cirrove-service --lib --locked --no-run
timeout 90s cargo test -p cirrove-service --lib --locked \
  content::windows::tests::graph::kernel::real_ -- --ignored --nocapture --test-threads=1
```

## Acceptance gates

- [x] Deterministic request-count tests for 3.1 MB previews, a 1 GiB sequential
      read, random seeks, repeated opens and concurrent readers of one version.
- [x] With a verified stable representation, additional cache blocks do not each
      trigger a Graph before/after pair; setup is shared per read session and
      renewal is separately counted. Declare measured session limits explicitly.
- [x] Conservative sequential-window fallback reduces Graph metadata calls by at
      least eightfold on the 1 GiB fixture without a whole-file RAM allocation.
- [ ] No mixture of versions after same-size replacement, rename, URL expiry,
      concurrent modification, revoked access, missed push or reconnect.
- [ ] Ignored ranges/conditions, short bodies, throttling, interrupted staging and
      physical quota failure retain correct cache identity and bounded resources.
- [ ] First-byte latency and navigation under thumbnail load do not regress as a
      side effect of aggressive prefetch; report excess downloaded bytes too.
- [ ] Real Personal, Business and linked SharePoint checks verify the chosen
      validation path and API-cost distribution. Synthetic checks alone do not
      close the release gate or establish superiority over another client.

The currently implemented two-check-per-block behavior remains in force until a
replacement passes its correctness and provider gates. The desktop interface work
does not resolve or downgrade this requirement.

## Mounted application workload and measured tradeoffs

The [mounted read workload](../benchmarks/mounted-read-workloads.json) now combines
the actual OneDrive HTTP adapter, shared cache, kernel FUSE and a separate Python
process doing ordinary unbuffered reads. It covers a cold 3,100,000-byte file, three
reopens, four sparse seeks, 32 simultaneous cold opens/reads of one version and a
sequential 1 GiB read using 64 KiB application requests. The fixture origin checks
strong If-Match conditions exactly, including case, and rejects mismatches with 412.
The service counters must agree with the server's HTTP request counters after every
application phase. Each variant runs in a fresh process so allocator retention from
an earlier variant does not contaminate the next measurement.

The original workload baseline at source revision `9a5a789` produced these
**synthetic** sequential results on 2026-09-07:

| Read strategy | Graph GETs | Content GETs | No imposed delay | 20 ms per response |
| --- | ---: | ---: | ---: | ---: |
| Original conservative blocks | 512 | 256 | 2.58 s | 18.64 s |
| Strong conditional session | 2 | 256 | 2.24 s | 7.74 s |
| Conservative streamed windows | 40 | 20 | 3.44 s | 4.85 s |

Every sequential variant downloads exactly 1 GiB. Small-file reopens add zero
requests; 32 concurrent readers share one 3.1 MB download. Sparse reads fetch four
4 MiB blocks for 16 KiB requested: the current block granularity still overfetches
small cold requests. Strong sessions use two Graph requests across those four
blocks, whereas the original and weak-validator paths use eight. Windows do not
speculatively expand this sparse workload. The strong session's existing 60-second
lease remains in force; tests count any renewal separately instead of treating a
slow runner as an indefinitely valid representation.

Cached directory listing/stat remained below 3.71 ms in this run, under the existing
500 ms per-operation bound. The artifact records per-phase navigation and application
read-call p50/p95/max, first open/read timing, transferred bytes, sampled service/test
server RSS and the separate application's peak RSS. These are single local runs,
not a statistically controlled provider benchmark or a total kernel/page-cache
memory ceiling. Metadata is preseeded: live indexing, real thumbnail decoders,
provider throttling and internet conditions are **not** exercised here.

The measurements expose two costs rather than declaring one strategy universally
faster: staging adds local I/O at low latency, and the baseline strong range path
still issued one content request per cache block. The conditional-window increment
below addresses the latter cost while retaining the former. It keeps exact
conditions, bounded staging and safe renewal without appending a restarted transfer
to a partial sink. The cold sparse-read amplification remains visible in subsequent latency work.
Only the request-count and stable-session implementation gates above are closed by
this combined evidence; the real-provider, failure and desktop-load gates remain open.

CI runs the release workload separately from the normal workspace/desktop checks:

```sh
cargo test -p cirrove-service --lib --locked --release --no-run
for delay in 0 20; do
  for mode in conservative strong windows; do
    CIRROVE_FIXTURE_REQUEST_DELAY_MS="$delay" timeout 210s cargo test \
      -p cirrove-service --lib --locked --release "real_mounted_${mode}_read_workload" \
      -- --ignored --nocapture --test-threads=1
  done
done
```

## Conditional streamed windows

Strong content-origin bindings now share the bounded window path. A fresh binding
sends one conditional content request per window, checks the effective resource,
strong ETag, range and encoding before streaming, then checks the exact body length
and binding lease before returning success. Nothing is published while the window
is partial. Request counters distinguish `conditional_ranges` and
`conditional_windows`; Graph setup/renewal counts remain separate.

Ranges and windows share the per-version renewal gate. If a binding has expired or
its headers reject the old context, the requested window itself is downloaded
between the original Graph identity/revision checks. It establishes a replacement
binding without an extra probe or another copy of the body. The binding lease starts
before those checks. A renewed weak validator returns to conservative validation.
Once any bytes have reached the sink, a body/sink failure, cancellation or expired
lease fails the whole window; the service discards it before a later request can
start over. A failed renewal shares its failure with waiting ranges and windows.

Seven additional adapter tests cover large chunked windows, concurrent range/window
renewal, same-size replacement including an origin ignoring If-Match, failed final
renewal validation, malformed/short/oversized bodies, sink failures, throttling,
lease expiry, cancellation and weak-validator fallback. Two additional kernel tests
pause a conditional window mid-body and verify coalescing, independent reads,
cached navigation and cancellation/unmount with held descriptors. The adapter
replacement fixtures do not constitute a real-provider race test.

The [conditional-window measurements](../benchmarks/conditional-read-windows.json)
repeat the same mounted application workload in fresh release processes at zero
and 20 ms imposed response delay. In each, the sequential 1 GiB phase uses **two
Graph and twenty content requests**, with no renewal required during the run.
The earlier strong-range baseline needed 256 content requests. Small previews,
reopens, sparse reads and concurrent readers retain their request counts. The
final local samples took 3.95 s without imposed delay and 4.18 s with 20 ms per
response. These single samples do not isolate a statistically reliable latency
gain. Cached navigation stayed below 3.82 ms, peak reserved staging was 64 MiB, and
all staging reservations were released. The raw artifact retains phase timings
and memory observations for all six repeated strategy/delay combinations.

These runs demonstrate request reduction, not a universal speedup. Staging remains
extra local I/O; the original low-latency strong-range run was faster. Real-provider
window validation, wider desktop load and recovery gates still apply, and normal
accounts continue to use the original conservative construction.


The initial conditional-window CI run also exposed a cached-navigation deadline
failure. Investigation reproduced an independent blocking dependency: batch inode
resolution requested an immediate SQLite writer even when all mappings already
existed. Cached batches now stay on the read path; missing mappings recheck under
the writer before allocation. A regression test failed with DatabaseBusy before
this correction and succeeds while another connection still holds that writer.
An actual FUSE fixture repeats directory listing/stat under a separate process's
held writer, preserving the 500 ms bound. This removes that demonstrated dependency;
it does not attribute every possible slow CI sample to SQLite or establish a
universal latency guarantee. Workload errors now identify their application phase.
