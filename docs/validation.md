# Validation record

Date: 2026-09-06. Local environment: Arch Linux, kernel 7.1.9-arch1-2,
Rust 1.98.1, FUSE 3.18.2.
This is evidence for a **read-only development preview**, not completed real-provider
acceptance for roadmap stages 1–3.

## Local checks

- Formatting and strict Clippy cover all crates and test targets.
- Current default workspace suite: **104 tests passed**. Nine kernel-FUSE tests
  run separately; subprocess fixture entry points and the optional performance
  fixture remain excluded from the default suite.
- Built both binaries and generated workspace Rustdoc.
- Executable smoke test passed: synthetic staging, status, competing ownership,
  recovery after SIGKILL, SIGTERM and socket cleanup. Two observer-script tests pass.
- The systemd user-unit template was verified with the locally built daemon path.
  Actual desktop installation and live account evidence are tracked privately.
- Real desktop Secret Service test passed: 64 updates to one uniquely named
  synthetic checkpoint, independent reads through fresh encrypted sessions, and
  removal. Run it explicitly with
  `cargo test -p cirrove-auth --test desktop_vault --locked -- --ignored`.
  No existing credentials are read or changed by this fixture.

## Authentication and Graph transport

Synthetic OIDC tests require a valid RSA signature, issuer, audience, nonce and
expiry. Callback tests reject wrong state, host, method and duplicate parameters.
A 32-caller expiry test causes exactly one refresh, persists the rotated grant and
keeps a delayed old-token 401 from expiring the newer token. A keyring-save failure
prevents publishing the replacement grant. The RSA fixture key is intentionally
public and used only for offline tests.

Local HTTP fixtures test delta translation, tombstones and linked-drive identities;
410 recovery signalling; 429 cooldown; stalled-request cancellation; rejected
foreign continuations and authenticated redirects. Content checks verify exact
ranges, reject version changes and oversized/incorrect bodies, and assert that
Graph bearer headers never reach signed-content requests. Download throttling is
shared with metadata calls. Package fixtures verify that OneNote-style and future
package types do not abort a delta or children listing. cTag tests allow metadata-only
changes during a download and reject changed content, retaining eTag fallback coverage.

These fixtures do not authenticate to Microsoft or establish real token-refresh,
consent-policy, CDN or SharePoint behavior.

## Storage and actual filesystem tests

Storage checks preserve old indexes until a staged refresh finishes, reopen
interrupted pagination, isolate accounts, apply final occurrences/tombstones and
reject out-of-order pages. Foreground observations newer than a feed round survive
that round and yield to the next one. Stable inode batches survive database reopen.
Shortcut ancestry queries exclude unrelated subtrees and terminate parent cycles.

The content tests check 32-reader coalescing, offsets beyond 2 GiB in a synthetic
3 GiB file, offline cache reuse after restart, corruption recovery, interrupted
publication cleanup, quota enforcement and shared-failure retry suppression.

Nine tests ran against **real kernel FUSE mounts** in temporary directories:

- Normal file reads, linked-library projection, duplicate-alias inodes, deep
  traversal, large seeks, EROFS on writes, stable inodes/cache after restart,
  ejection/remount and folder navigation during 96 stalled application reads, all
  released by shutdown with ENODEV.
- The actual account manager's automatic remount, persisted disable/enable,
  refusal to obscure a new local file, shutdown cleanup and restart.
- Refusal to claim another account's mount at the same path.
- Recovery after killing a synthetic mount process, with matching account UUID
  ownership and readable content through a newly started manager.
- Shared read-only and private copy-on-write Python mappings; a private modification
  does not change shared bytes. A synthetic 3 GiB mapping can read beyond 2 GiB while
  the application process's peak RSS stays below 128 MiB.
- A content change through the provider exposes a new regular-file inode, including
  file shortcuts, while open old mappings retain their bytes. A following rename
  with the same cTag reuses both inode and cached content with no new range request.
- A burst of 96 simultaneous 64 KiB reads from distinct 3,100,000-byte files completes
  while cached folder listings retain independent capacity. The fixture adds 250 ms
  per content request and the cache makes 96 range calls, without retries.
- Provider change hints expose a new file and subsequent rename through actual
  mounted paths, including a previously cached negative lookup. The timer is set
  to one hour so polling cannot satisfy the test.

An early stalled-read run exposed an EINTR retry loop during shutdown. The stopped
mount now returns ENODEV; the corrected test passed. Its old fixture process and
mount were cleaned up. No existing cloud mounts or data were changed.

Regression evidence before these fixes: shared mmap returned ENODEV; a package
aborted its containing listing; 64 of 96 burst reads returned EAGAIN. All three
corrected cases passed. The burst's 20 cached directory listings measured p50
1.10 ms and p95 3.00 ms locally. This is simulated thumbnail-style I/O, not a run of
Nautilus's actual thumbnailers or a real-provider performance claim. The sample
directory contains one entry; the separate benchmark below covers 525 entries.
The repeat command and measurements are in
[synthetic-thumbnail-burst.json](synthetic-thumbnail-burst.json).

The optional metadata content-tag field is backward-readable from prior preview
databases. The revised cache/inode keys can cause a one-time cold cache and changed
regular-file inodes on upgrade. Old cache blocks remain quota-accounted and evictable;
no cloud data is written. Mappings still cannot demand an uncached historical version
after it changes remotely; see [FUSE semantics](architecture.md#fuse-and-lifecycle).

## Local performance fixture

The isolated debug-build benchmark performs 20 cold/warm reads of 3,100,000-byte
files and 20 listings of a 525-entry cached directory. Its synthetic provider adds
50 ms latency; it performs no Graph, OAuth or internet traffic.

| Operation | p50 | p95 |
| --- | ---: | ---: |
| Cold content read, metadata already indexed | 123.07 ms | 142.97 ms |
| Warm content read | 1.45 ms | 1.96 ms |
| Cached directory listing | 10.16 ms | 11.12 ms |

There were 20 provider range calls for the cold reads and zero for warm reads.
Process peak RSS was 76,616 KiB, including fixture generation and the FUSE service
in one process. This is not an isolated daemon-memory measurement. The native Graph
adapter additionally checks metadata before and after uncached blocks, so these
figures must not be presented as real OneDrive latency.

Machine-readable evidence and the repeat command are in
[synthetic-performance.json](synthetic-performance.json).

## GitHub CI

The [foundation CI run](https://github.com/Dandiccf/cirrove/actions/runs/34014890227)
passed at `fa22f21`. The [read-only preview CI run](https://github.com/Dandiccf/cirrove/actions/runs/34018513735)
passed on Ubuntu 24.04 at `c4a877eb17e1539c42d4199ac78cebcf035b8e3b`.
Formatting, strict Clippy, all 37 default tests, both explicit kernel-FUSE lifecycle
tests, binary builds, executable smoke and Rustdoc completed successfully. The
keyring check and benchmark remain separate local checks described above.

The [mapping and concurrent-read CI run](https://github.com/Dandiccf/cirrove/actions/runs/34020162248)
passed on Ubuntu 24.04 at `1e301dc541ecad23622409ab4d9aed524628a7d1`.
All 39 default tests and five explicit kernel-FUSE tests passed, including the
memory-mapping and 96-reader cases. Formatting, strict Clippy, builds, executable
smoke and Rustdoc also passed. The kernel advertised direct-I/O mmap support on
both the local Arch session and this Ubuntu runner.

## Upload components under development

The default suite now includes ten local HTTP upload fixtures, eleven journal
tests and eight transfer-worker tests. They cover exact ranges, private session
checkpoints, name collisions, conditional commit conflicts, zero-byte files,
content-hash reconciliation, persisted backoff, cancellation and secret-store
failures. Empty or malformed saved checkpoints trigger remote-content reconciliation
instead of repeated parsing failures. An old journal schema migrates while retaining
pending bytes and ordering.

Opt-in write-consent tests verify that existing grants remain read-only and that
refresh preserves the chosen permission mode. The developer write command rejects
read-only grants, enabled accounts and competing account owners before cloud access.
See [isolated write validation](write-validation.md) for the prepared live workflow.
These synthetic results do not establish Microsoft final-commit semantics or
writable FUSE correctness. An isolated business-drive run additionally passed
the basic write command with generated files: upload/readback, name collision,
empty file, conditional replacement, stale revision, and a competing edit before
final commit. The competing edit remained intact and the staged upload became a
conflict. This is limited fixture evidence, not a general concurrency guarantee;
personal-drive commits and broader real recovery checks remain unverified.
Account identifiers and live logs are retained privately. Ordinary mounts remain
read-only.

## Namespace components under development

Five additional HTTP fixtures cover conditional PATCH/DELETE, Unicode names,
collision/stale ETag rejection, lost responses, remote type checks and conservative
missing-item reconciliation. Seven journal/worker tests cover shared upload ordering,
schema migration, stale attempts, retained indeterminate outcomes, cancellation and
actual child-process SIGKILL before/after acknowledgement. The crash tests exposed
and fixed a receipt serialization ambiguity before live validation.

An isolated business-drive fixture passed folder creation, file rename/move with
byte readback, name collision, stale rename, confirmed file removal and stale-delete
protection. A following run also passed folder rename and move while retaining
and reading back an existing child. Account identities, real item IDs and logs
remain private. These checks
do not establish writable FUSE save semantics or the full provider matrix.

## Change notification implementation

Five transport fixtures cover signed endpoint parsing, Graph bearer isolation,
background permit release, real loopback WebSocket handshakes, Socket.IO namespace
acknowledgement, event ACKs, heartbeat expiry, oversized messages, renewal and
cancellation. Three service fixtures cover push-triggered deltas, retained hints
during active work, burst coalescing, Retry-After, reconnect catch-up and stable
polling for adapters without notifications. The actual FUSE check above verifies
that these changes reach mounted paths.

An isolated business-drive test subscribed through the official Graph Socket.IO
endpoint and observed its uniquely generated folder, then two conditional renames,
in deltas requested because of notifications. The test uses no polling to satisfy
that condition. Provider
delivery can still be delayed; this result is not a low-latency guarantee. Further
personal-account, linked-library and long-session checks remain open. Actual
identifiers and timings remain in private local evidence. See
[the notification decision](adr/0003-change-notifications.md) for the acceptance
plan and repeat command.

## Recently used directory freshness

Eight additional default tests cover bounded activity leases, scheduling fairness,
error backoff, cached and cold navigation while another listing stalls, activity
that cannot bypass throttling, and fresh observations surviving empty/unrelated
deltas. Repeated identical observations do not emit another filesystem reload.

The initial activity commit passed one CI run but failed the concurrent cold-listing
fixture in another. A four-writer store regression reproduced SQLITE_BUSY during
the new metadata comparison's deferred read-to-write transaction upgrade. Directory
observation and delta staging now start immediate transactions before their
read/modify/write work, so writer admission uses the bounded busy timeout. The
regression passes, including mixed observation/delta writers, and verifies every
worker's final listing. Network work remains outside the transaction.

A ninth kernel-FUSE test verifies new, renamed (including Unicode) and deleted
entries while a content read remains blocked. It disables push and sets the delta
timer to one hour; the bounded directory worker must make each change visible.
The existing push test stalls the activity listing so push remains its only route
to fresh metadata. Both mechanisms therefore have independent mounted-path checks.

One full local FUSE run encountered a transient busy mount in the existing
ejection fixture; its isolated repetition passed. The fixture now requires an
ordinary unmount within two seconds, retrying only EBUSY and never forcing a detach.
The full nine-test run then passed. The source of the transient busy state was not
established; this is not proof of a fixed production mount-lifecycle defect.

A subsequent CI run passed the metadata tests but encountered Busy when an existing
upload-journal fixture reopened after dropping its owner. Parallel crash tests can
briefly retain inherited flock descriptions between fork and exec. The test-only
reopen helper permits up to one second for Busy; other errors return immediately,
and held-owner exclusion remains an immediate assertion. This does not change
production ownership or prove a process trace of that CI failure.

An isolated business-drive check additionally passed actual Graph listings through
a kernel mount: creation of one generated child folder and two conditional Unicode
renames appeared in the mounted directory. A validation-only root baseline and
disabled push excluded Graph delta/notifications from satisfying the check. Each
sample followed an already completed directory listing; acknowledgement-to-mount
visibility and page counts are recorded privately. The temporary mount stopped
cleanly after success and the generated fixture was retained. The installed daemon
and its ordinary read-only mount were not changed.

This establishes only the small generated-folder case on the selected business
drive. Graph listing-to-desktop latency, multiple large active directories, request
cost, content reads and indefinitely visible windows remain separate validation
gates. No installed-runtime upgrade is implied by building or merging these changes.
See [the repeat command](write-validation.md#check-directory-freshness-through-an-actual-mount).

## Open application handles during shutdown

A mounted desktop upgrade detached its read-only filesystem but the daemon did
not exit before the service stop timeout. A new kernel regression reproduced that
behavior by retaining both an application file descriptor and a directory
descriptor: the old session join finished only after those descriptors closed.
The original process's exact retained descriptors were not traced.

The session now retains its own FUSE connection-control descriptor when mounting,
unmounts before disconnecting the connection and joins its request threads. Missing
control access rejects the mount explicitly. The regression passes with handles
still open and checks that a second mount with the same account identity remains
readable. A separate subprocess test sends SIGTERM to a synthetic account manager
while the parent retains both handles; the whole child process exits successfully,
the mount disappears and the account state can be owned again.

All eleven local kernel-FUSE tests pass, including read/mmap consistency, lazy
ejection recovery, thumbnail contention and the two new lifecycle checks. These
are synthetic fixtures with no cloud credentials. They do not prove every possible
shutdown failure, writable-save draining, or a successful installed desktop upgrade.

## Actual notification renewal

An isolated business-drive run of the notification validator with `--check-renewal`
completed its real approximately 50-minute renewal, established the replacement
connection, and observed a subsequent generated-fixture rename through a fresh
notification-triggered delta. All four generated changes passed and the test
process exited successfully. Event details and provider timings remain private.
This closes that particular business-drive renewal check, not the wider personal
account, suspend/outage, delivery-latency or 24-hour acceptance matrix.

## Experimental local application saves

The generation journal and initial writable FUSE API now pass 118 default workspace
tests and fourteen actual kernel-FUSE checks on the local development system.
Formatting, strict workspace clippy, build, service smoke checks, two observer tests
and Rust documentation also pass. The three new kernel checks use only generated
local fixtures, without cloud credentials or changes to installed mounts.

The writable checks cover create, partial overwrite of an existing version,
truncate, repeated fsync, immutable generations during an outstanding upload,
and offline remount. A blocked hydration leaves cached directory browsing and an
independent local save operational. Closing a read-only preview cannot seal an
unfinished write; a regression first reproduced that behavior, then passed after
restricting save sealing to write handles. A subprocess test kills the synthetic
filesystem daemon while a separate application holds a write handle, then checks
both the sealed generation and newer dirty bytes through an offline remount.

Additional journal tests inject metadata and queue-commit failures, exceed storage
quota, reject incomplete hydrated sources, and kill a process after a saved version
and subsequent unsealed edits. A synthetic upload worker resumes an interrupted
first generation and then uploads its successor with the predecessor's confirmed
identity and ETag. Generation eligibility also retains ordering against namespace
operations when remote creation assigns a new item ID.

This is initial mounted-write integration, not completion of milestone 2. The
normal daemon still mounts read-only. Atomic replacement, writable folder/name
operations, conflicts in the desktop UI and broader live-provider application-save
validation remain open. Writable mmap and metadata
changes are explicitly unsupported. Process kills and injected storage failures do
not establish physical power-loss or physical disk-full behavior.

## Automatic uploads and writable-session shutdown

The experimental session now owns its upload workers and stops mutating callback
admission before draining local edits. The updated code passes 121 default workspace
tests and eighteen actual synthetic kernel-FUSE checks. Formatting, strict Clippy,
workspace build, service smoke, two observer tests and Rust documentation pass.

Four additional mounted tests verify:

- Consecutive application saves upload automatically in order; an unsealed edit
  on an open handle is sealed by shutdown and uploaded after restart.
- Stalled provider and keyring futures that ignore cancellation cannot indefinitely
  block shutdown; local snapshots remain exact and require remote reconciliation.
- Shutdown waits for a previously accepted write blocked on local journal storage,
  then preserves its exact bytes before detaching the mount.
- Insufficient snapshot quota returns a shutdown error while retaining the dirty
  working file and releasing the temporary mount.

A new developer command exercises two application saves through a temporary writable
mount backed by Graph. Its separate application process, fixture-only namespace and
allowed upload identities are checked locally. Live execution is recorded separately;
the existence of this command does not itself establish provider-backed success.
Atomic replacement, folder operations, physical disk failure and the ordinary
application compatibility matrix remain open.

## Required before calling stages 1–3 complete

- Cirrove's own Microsoft app registration, real consent and verified work-account,
  tenant and drive selection.
- Real access-token expiry/refresh, revoked consent, keyring locking/unlocking and
  recovery across a desktop/service restart.
- Linked SharePoint folders, folder-only permissions, duplicate shortcuts, removed
  links and revoked targets on real accounts.
- At least 24 hours of read-only operation, including a controlled network outage
  and recovery. No completed soak test is claimed.
- Real-provider cold/warm p50/p95, request counts and ordinary application previews;
  synthetic performance does not satisfy that gate.
- Repeat the recovery checks on another supported system; CI covers the synthetic
  Linux path, while real-account behavior still needs desktop validation.

Writes, pinning, conflict recovery, desktop badges/settings, Google Drive and iCloud
remain later milestones. Unit tests and orderly reopen tests do not simulate actual
power loss, hardware failure or every application behavior.
