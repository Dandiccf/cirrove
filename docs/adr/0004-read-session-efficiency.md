# Read-session efficiency and Graph request amplification

Status: proposed; implementation and provider acceptance are outstanding.

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

## Planned implementation

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

## Acceptance gates

- [ ] Deterministic request-count tests for 3.1 MB previews, a 1 GiB sequential
      read, random seeks, repeated opens and concurrent readers of one version.
- [ ] With a verified stable representation, additional cache blocks do not each
      trigger a Graph before/after pair; setup is shared per read session and
      renewal is separately counted. Declare measured session limits explicitly.
- [ ] Conservative sequential-window fallback reduces Graph metadata calls by at
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
