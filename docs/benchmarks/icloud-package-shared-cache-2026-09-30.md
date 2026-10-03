# Generated package sessions through the shared content cache

The complete iCloud package download already writes to private caller-owned disk
staging and returns its actual length/digest after source checks. The shared
engine previously accepted generated content only as a whole `Arc<[u8]>`, so it
could not ingest this disk artifact without another whole-document allocation.

`ReadProvider::staged_content_session` now offers an already validated generated
artifact through the existing version-bound `ReadSession` contract. The default
is None; Google and existing in-memory exporters retain their existing hook.
The engine ingests the session before it publishes the corresponding directory
page. Cache staging checks exact account/collection/item/revision/size identity,
requests at most one 4 MiB block per call, validates returned lengths and uses
the same checksummed block publication path. Two concurrent artifact stagers
bound transfer concurrency. Provider calls occur before cache locks are acquired.
Normal cache eviction and crash-recovery semantics remain unchanged.

The engine regression failed before wiring the hook because it requested the
forbidden whole-memory copy. With the new path, a synthetic 8 MiB + 17-byte
artifact is staged in three requests, and cache-only reads survive engine/cache
reopen. A mid-stream error leaves the derived node absent from visible metadata.
Additional tests refuse foreign account identity before any read, short replies
and cancellation triggered during the final read, without publishing a block.
Seven engine package tests pass. This is not yet native package discovery or
an installed iCloud mount.

## Registered live arm

Question: can a freshly source-checked Pages archive enter the normal block cache
through this streaming path and then be read in full after cache reopen with all
provider reads disabled?
Prediction: all bytes and SHA-256 match the complete-download receipt, and the
offline oracle receives zero content requests. This requires the artifact length,
not the source's larger uncompressed package size.

Use the existing isolated session of fixture
`21d05f56-70c4-47a8-a5e3-577ab68994ac`, a fresh run UUID/private directory and the
same fixed Pages sample selection as the preceding arm. Download at most 64 MiB
through the package primitive, retaining identity/version only in private state.
Independently hash the disk file and each 4 MiB block before presenting an
immutable private-file ReadSession. Each subsequent disk read verifies its block
hash. Stage into a new 64 MiB normal ContentCache, drop the private read session
and cache object, reopen the cache, and read every byte with an Offline provider
that refuses and counts every attempted content access. Compare the final digest
with the provider-checked receipt. Do not print document content, names or IDs.

This arm reads one existing document but does not edit, upload, extract or attach
it. No normal account/service/mount changes occur. Outer timeout 600 seconds;
record binary hash, PID, command and disk-backed private TMPDIR before execution.
No competing local compilation or measurement. The private archive and caches
are retained. This validates the real content cache path, not native package
listing, a FUSE mount, power-failure durability or native editing.

## Result

Run `66d698a7-1091-4192-b0dc-07c6e840719b` passed in 8.689 seconds
(end-to-end single functional arm, not a benchmark claim). The 21,759,419-byte
archive, against a 21,957,877-byte logical source, passed independent disk hashing
and full cache-only hashing after reopen with zero provider reads. Evidence:
`icloud-package-shared-cache-live-2026-09-30.json`. No mount or installed service
was involved. The subsequent mount validator retains the private read-session Arc
for its separate Engine arm; the offline cache oracle still cannot access it.

## Repository validation

`CARGO_TARGET_DIR=.target-icloud-feasibility scripts/check.sh` passed in full,
including workspace and feature tests, real kernel mounts, scripts, acceptance
ledger and documentation. Private log: `.local-state/icloud-staged-session-full-check.log`.
The existing rustdoc `Writeback::retry_stuck` link warning remains. GUI window
scenarios were not run; no desktop behavior changed in this step. Neither this
check nor the captured mount arm installs or enables normal iCloud writes.
