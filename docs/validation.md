# Validation record

Date: 2026-09-06. Local environment: Arch Linux, kernel 7.1.9-arch1-2,
Rust 1.98.1, FUSE 3.18.2.
This is evidence for a **read-only development preview**, not completed real-provider
acceptance for roadmap stages 1–3.

## Local checks

- Formatting and strict Clippy cover all crates and test targets.
- Current default workspace suite: **160 tests passed**. Twenty-one kernel-FUSE tests
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

The read-only baseline ran against **real kernel FUSE mounts** in temporary directories:

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

A generated-folder business-drive run also passed two application saves through an
actual writable mount. A separate application process wrote and fsynced two versions
of a Unicode-named file; both generations uploaded automatically, their size/hash
matched the application reports, and independent Graph content readback matched the
final snapshot. The temporary mount shut down successfully. The test used only a
new run-owned folder and retained its synthetic cloud file and private evidence.
Its guarded adapter did not index the account or expose existing files for mutation.
This is one small business-drive fixture, not a latency distribution, large-file
benchmark, personal-account check or desktop-application save matrix.
Atomic replacement, folder operations, physical disk failure and the ordinary
application compatibility matrix remain open.

### Kernel test isolation and memory measurement

The first CI pair for the writable-session change had one passing run and one
failure in existing read-only tests: an application-memory assertion and the
thumbnail-burst deadline. A controlled child-process experiment reproduced a
measurement defect: `ru_maxrss` could include inherited memory before exec even
when the Python application's own address space remained small. The mmap test now
uses that application's `/proc/self/status` `VmHWM`, retaining the 128 MiB limit
and all byte/mapping assertions.

The original CI log did not separate thumbnail file opening from active reads, so
the exact phase responsible for its timeout is unknown. Setup and the subsequent
96-reader burst now each have a bounded deadline; cached-directory requests still
must finish within 500 ms and no content request may fail or be retried. Independent
kernel fixtures run sequentially in CI to avoid competing with these latency
checks. Internal request/account concurrency remains tested. The pre-change suite
also passed a local two-CPU run; that pass does not explain the CI timeout or
establish a runtime performance fix.
The corrected fourteen-test read-only kernel suite passed with two CPUs and
sequential fixtures. The 121 default workspace tests and strict Clippy passed again;
these corrections change the validation harness, not the live-tested runtime.

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


## Save/rename/save lineage and shutdown follow-up

Nine journal fixtures exercise operation chains across uploads and namespace
changes, including restart after a lost rename response, conflicting edits,
cross-kind successor exclusion, schema-5 migration, rollback of a local rename
when its intent transaction fails, and rebinding more than one claim batch.
A worker regression additionally verifies that a reconciled rename containing
another actor's newer file content becomes a conflict and retains the next local
save. A controlled run against the former unconditional acknowledgement failed
that regression; the corrected journal and worker passed. Unknown content lineage
requires review instead of allowing a later upload to adopt it as a safe base.
These are synthetic journal/worker checks. Mounted rename, atomic replacement and
real-provider save/rename/save validation remain outstanding.

A repeated writable-session run exposed a test synchronization error: unrelated
FLUSH callbacks were counted as admission of the intended blocked write. The test
now keeps the application descriptor in a separate Python process and gates its
write after the initial fsync callback drains. Its original shutdown deadline,
retained-byte assertion and requirement to finish with the application's handle
still open remain. Another assertion now permits the retained upload to be pending
or require verification: an early FLUSH can legitimately let the worker claim it
before cancellation. Both cases must retain the exact bytes and resume to the
correct cloud content.

The same investigation exposed intermittent EACCES on the first file creation.
A controlled temporary fault injection invalidated the root inode while its initial
GETATTR response was outstanding and reproduced the failure. Skipping invalidation
of the mount's fixed, synthetic root attributes passed that same injected ordering.
Child entry invalidations remain active; directory handles do not enable kernel
readdir caching. Diagnostic delays and logging were removed afterward. Twenty
consecutive runs of the final four-test writable suite passed (80 test executions).
This is bounded local synthetic evidence, not sustained real-provider acceptance.


## Sparse namespace and actual mounted file relocation

Thirteen additional default fixtures cover metadata-only identity, name/intent
rollback, local collisions, explicit foreign-name conflicts, collection isolation,
case policies, attachment after an old name is reused, stale hydration, zero-byte
truncation, receipt/remote-alias atomicity, schema-6 migration and delayed memory
publication. A renamed 500 GiB metadata object reserves no working bytes under a
1 KiB quota. The projection regression publishes newer snapshots before older
callbacks and checks that both the final name and assigned remote alias survive.

Two additional actual synthetic FUSE tests exercise separate Python applications:

- A 500 GiB online-only file is renamed and moved under a 1 MiB spool quota, with
  stable inode and zero content reads. An occupied destination is preserved. The
  session is shut down with a stalled mutation and its journal is reopened. A lost
  retry response is reconciled without replaying the move, after which a mounted
  truncate/write/fsync follows the confirmed move's ETag and uploads correctly.
- A lost move response is followed by another actor's content change. The next
  mounted save stays pending behind the resulting conflict; local bytes remain
  readable and retained after shutdown, and the foreign cloud content is preserved.

The first mounted truncation attempt failed with ENOSPC because Linux stripped
O_TRUNC from OPEN before a later SETATTR. Requiring FUSE_ATOMIC_O_TRUNC made the same
large-file test pass without hydration. A parallel fixture's immediate journal
reopen also saw Busy while another test fork briefly retained its lease descriptor;
the test now retries only Busy for at most two seconds, without displacing any owner.

These checks cover regular-file relocation in isolated synthetic mounts. The
ordinary manager remains read-only. Writable directories, open-unlinked handles,
atomic replacement, clean-object retirement, later remote-edit rebasing, full
application compatibility and real-provider mounted rename/save acceptance remain
outstanding. No installed daemon or existing cloud document was changed by these
checks.

The completed local verification run passed 144 default workspace tests, all 20
actual synthetic kernel-FUSE checks, formatting, strict Clippy, workspace build,
executable service smoke, two observer tests and Rustdoc. These counts include the
previous lineage and lifecycle cases; they do not establish real-provider readiness.

## Ordered metadata observations

Two controlled regressions failed before this fix: a held directory response
returned an old name after a newer listing committed, and a held item response
restored an older content revision and size. Both now use the newer committed
metadata, even when the provider goes offline before releasing the old response.
A third engine fixture verifies a bounded retry when the newer observation has
not established a complete directory view.

Thirteen store fixtures cover moves across parents, unrelated scopes, unknown-parent
deletions, empty deltas, replacement baselines, request ordering across paginated
feeds, transaction rollback, schema-3 migration, database-bound tickets, unchanged
observations, coherent cached listings and negative observations. Foreground absence
preserves the committed delta baseline and yields to a later-started feed.

An additional actual kernel-FUSE fixture holds a cold directory response while a
newer delta renames its child. A separate Python application's listing sees only
the new name after the old response is released, with zero content reads. All 160
default tests and 21 actual synthetic FUSE tests passed, including existing navigation
under load and writable-session recovery. Formatting, strict Clippy, workspace build,
executable smoke, two observer checks and Rustdoc also passed. These tests use synthetic providers;
they do not establish real-provider latency or complete the outstanding clean-object
retirement and remote-edit rebasing work in experimental writable mounts.

The PR CI passed this increment, while its parallel push CI found account-lock
contention during immediate synthetic session restart. A local concurrent rerun
reproduced it in a second restart fixture. Both fixtures now assert that the old
Engine has no remaining owner, then permit only WouldBlock for at most two seconds
while forked helpers release inherited lock descriptors at exec. Production locking
is unchanged. The corrected six-test concurrent suite passed ten consecutive runs
(60 executions), and the full 160-test default suite also passed again.

## Releasing acknowledged working copies and following remote changes

A synthetic kernel-mount regression against the preceding implementation reproduced
permanent retention of a closed, acknowledged working copy. The current mount
fixture verifies that acknowledged upload payloads are collected while an open
application still retains its working bytes. After the last handle closes, working
storage is released and a foreign remote edit/rename becomes visible with the same
local inode. A subsequent mounted save uses that newer remote ETag and uploads
successfully. Restart, a further metadata-only rename with no hydration, and remote
deletion also pass. A second deletion case removes the remote file before its last
local handle closes; an ordered NotFound observation removes the cached entry and
permits cleanup without leaving a ghost file.

Eight journal fixtures cover the acknowledged frontier, dirty/pending/in-flight
retention, stable aliases, reactivation, failed detach transactions, interrupted
cleanup and schema-7 migration to journal schema 8. Cleanup refuses symlink targets,
retains unknown spool files and retries acknowledged-payload removal after a failed
metadata checkpoint. Four service unit fixtures cover new access and edits during
a held metadata request, cancellation of an uncooperative provider, failed local
publication and delayed callbacks, and per-object backoff across idle passes.
Three store fixtures and one engine fixture check ordered NotFound publication,
rollback and supersession by newer positive metadata or complete parent listings.

The final local run passed 176 default workspace tests and all 22 actual synthetic
kernel-FUSE tests, plus formatting, strict Clippy, workspace build, executable service
smoke, two observer checks and Rustdoc. These tests do not access live cloud accounts
or replace the installed service. Full application atomic-save behavior,
open-unlinked files, writable directories, alias/history retention at scale and
real-provider mounted acceptance remain open.

## Regular-file unlink and retained open streams

Six additional journal fixtures exercise deleting an online-only file without
reserving its contents, name reuse with independent streams, create/delete ordering,
later descriptor writes, transaction rollback, conflict/uncertainty retention,
schema-8 migration and recovery of a previous process's local-reader barriers.
The deletion still waits for its confirmed remote predecessor; releasing a local
reader barrier cannot bypass that receipt dependency or remove retained bytes.

Five additional actual synthetic FUSE tests cover open-handle reads and truncation
after unlink, zero link counts, name reuse, absence of later orphan uploads, restart,
zero-hydration deletion of a 500 GiB virtual file, a held range request, spool quota
failure, and shutdown with a provider that ignores cancellation. The held-read fixture
initially blocked another application's sibling-file create because preservation
ran inside unlink. Moving preservation behind a persisted background barrier made
the same scenario pass: local unlink and the independent save finish while the
provider read stays held, but the cloud DELETE remains ineligible until readers are
safe. Quota failure keeps the cloud content until the last reader closes; shutdown
cancels an uncooperative range request and restart resumes the retained deletion.

The full local run passed 182 default workspace tests and all 27 actual synthetic
kernel-FUSE tests, formatting, strict Clippy, workspace build, executable smoke,
two observer checks and Rustdoc. No live cloud documents or installed services were
changed. Atomic replacement of open files, detached-data recovery and cleanup,
restoration of the same remote identity and broader provider/application acceptance
remain open.
