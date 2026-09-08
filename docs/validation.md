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

A later push run at `db9bfe3` timed out during the active burst with 93 of 96
provider reads started; its parallel PR run passed. A local two-CPU reproduction
passed in 7.97 seconds, with 6.34 seconds of user CPU time. This does not establish
the cause of the CI host's delay. Each 64 KiB application read loads a distinct
3,100,000-byte cache block, so this fixture generates and durably caches nearly
300 MB, including eviction under its 16 MiB quota.

The burst now has a 60-second completion bound, allowing the separate bounded
queue and provider phases, without treating 20 seconds of aggregate disk throughput
as a product requirement. Every cached directory request must still finish within
500 ms. Navigation is sampled throughout the entire burst, including later queued
waves and eviction, rather than only at its start. Every read must return correct
bytes without failures or provider retries. Output includes elapsed time, sample
count and maximum navigation latency; the containing sequential CI suite has a
150-second bound. This is a validation correction, not a runtime performance fix
or real-provider latency claim.

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

## Separate local identities and provider lookups

A journal regression now refuses provider metadata whose opaque ID equals an
unrelated, unacknowledged local ID, retaining the local name, bytes and revision
across restart. Created-file receipt checks assert that a provider ID does not
resolve through the local-identity query, and vice versa, before and after restart.
A projection fixture models two identity domains with equal strings, checks that
their working streams remain distinct after a binding update and a delayed callback,
and still rejects two owners of one provider identity. That fixture isolates the
lookup boundary; it does not perform a durable binding transfer or FUSE replacement.

The full local run passed 184 default workspace tests and all 27 actual synthetic
kernel-FUSE tests, plus formatting, strict Clippy, workspace build, executable smoke,
two observer checks and Rustdoc. Existing mounted rename, handoff, unlink and restart
fixtures pass with the separated lookup paths. The journal schema remains 9, and
no live cloud documents or installed service state were changed. Joint binding
transfer, multi-object operation prerequisites and atomic application replacement
remain open.

## Two-object replacement in the journal

Six completion-prerequisite fixtures distinguish the target's content/ETag base
from prior source operations and guarded cleanup. They cover an uncertain target
publication across restart, failed/conflicted prerequisites, independent-file
progress, namespace prerequisites, explicit verification, validation before source
reads, transaction rollback and schema-9 migration. Ordering dependencies never
supply another file's identity or consume its linear content successor.

Nine additional journal fixtures exercise atomic local path takeover with retained
victim streams, later writes to detached bytes, active-binding transfer with upload
acknowledgement, later saves/renames, two pending creates, source cleanup and conflicts.
They also cover rollback after local path changes and after all binding changes,
reconciliation after restart, consecutive pending replacements, snapshot-quota failure,
a 500 GiB victim without local hydration, process-local reader barriers, already
cached source/target files and schema-10 migration. The owner-index check confirms
indexed lookup and refusal of a second active provider binding for one object.

The full local run passed 199 default workspace tests and all 27 existing actual
synthetic kernel-FUSE tests, plus formatting, strict Clippy, workspace build,
executable smoke, two observer checks and Rustdoc. The FUSE checks are regression
coverage for existing mounted behavior; **they do not exercise mounted replacement**.
FUSE still refuses replacement of an occupied path. Atomic in-memory publication,
background preservation of old readers, uncached-source preparation and actual
application/provider replacement acceptance remain open. No live cloud documents
or installed service state were changed.

## Atomic publication of local namespace changes

Journal schema 12 records one coalesced change marker per local object. The mount
publishes the complete changed set at a committed database frontier, including all
sides of ownership transfers. Four journal fixtures exercise repeated writes with
128 unchanged objects, indexed incremental reads, actual schema-11 migration,
restart, missing publication structures, transaction rollback and clock exhaustion.
Four projection fixtures exercise target-binding transfer in adversarial object
order, chained replacements with delayed callbacks, rejected incomplete/corrupt
batches and acknowledgement rollback followed by successful retry. Old, intermediate
and current local streams retain their separate bytes.

The final local run passed 207 default workspace tests and all 27 existing actual
synthetic kernel-FUSE checks, plus formatting, strict Clippy, build, executable
smoke, two observer checks and Rustdoc. SQLite snapshot reads and decoding leave
cached projection lookups available; final index publication holds the projection
lock. These checks do not establish a new latency bound or large-library capacity.

The ordinary service remains read-only, and experimental FUSE rename still refuses
an occupied destination. Replacement-specific reader preservation, deferred
preparation of uncached sources and actual mounted application-save acceptance
remain open. The 10,000-object namespace limit bounds a publication batch; removing
that limit requires bounded transaction groups and retained-history cleanup.
No installed service, credentials or cloud files were changed.

## Mounted regular-file replacement and deferred source capture

Experimental FUSE rename now replaces an occupied regular-file path, including
when its source exists only online. Local namespace acceptance does not wait for
source downloads. Journal schema 13 retains a `Preparing` operation until the
original source version has a complete immutable snapshot. Target publication
and guarded source cleanup remain behind separate receipt and reader barriers.

Nine journal fixtures exercise source/target identity separation, newer edits
during preparation, source-move receipts, quota failure, blocked conflicts with
independent-file progress, schema-12 migration and refusal of missing/future
schemas. They also check attempt fencing, ownership across asynchronous downloads,
restart reader gates, reclamation of identified capture temporaries, adoption after
failed final SQL publication, same-length corruption detection using the durable
checksum, and refusal of delayed hydration after provider-binding transfer.

Four additional actual synthetic kernel-FUSE tests use separate application
processes. They cover two consecutive atomic saves with retained old descriptors,
replacement of an online-only source while its download is held, a held old-target
range read while independent saves continue, and shutdown/remount during source
capture. Old descriptors retain their own bytes and zero link counts. Conditional
source cleanup waits until readers are safe and target publication is acknowledged;
writes through detached descriptors remain local recovery data.

The final local run passed 216 default workspace tests and all 31 actual synthetic
kernel-FUSE checks (15 read-only-suite checks and 16 writable-session checks), plus
formatting, strict Clippy, workspace build, executable smoke, two observer checks
and Rustdoc. The nested helper result in the read-only suite is not counted twice.
These checks use synthetic providers and do not change installed services or live
cloud documents. Writable directories, ordinary editor/office acceptance, live
Graph replacement and cleanup, physical fault testing, detached-data recovery and
bounded retained history remain release gates. Ordinary mounts stay read-only;
all six product milestones remain open.

## Expanded mounted OneDrive acceptance command

The developer-only `validate-onedrive-writable` sequence now includes two
temporary-file atomic replacements and another replacement using a retired,
online-only source after reopening the engine and journal. It demands eight
upload receipts and three conditional source-cleanup receipts, independent
destination-content and source-absence checks, and a complete final listing of
the fresh test folder. Local rename time is separate from cloud completion.

Two new local fixtures check the separate application's old-descriptor/inode/byte
assertions and its reuse of the temporary name, and refusal of foreign scope,
foreign identity, folders, linked targets, root targets, missing/wildcard ETags,
outside-root relocation and mismatched cleanup receipts. The application fixture
uses ordinary local files; it does not establish Graph behavior. All 218 default
workspace tests and the existing 31 actual synthetic kernel-FUSE checks passed,
with formatting, strict Clippy, build, smoke, two observer checks and Rustdoc.

The expanded live sequence has **not yet been executed**. Its launch remains
pending explicit authorization for the new folder, generated uploads and three
conditional deletions of run-created sources. Earlier live evidence remains
limited to the already recorded operations and two basic mounted saves. This
increment does not close the application/provider acceptance matrix or any of
the six release milestones.


## Pending local directories (2026-09-07)

Seven synthetic journal tests cover nested folders and sibling creates, preservation
after an uncertain parent across restart, destination-name ordering after binding,
transaction rollback, migration from schema 13 and refusal of missing/future schema,
rejection of unrelated folder receipts, and both arrival orders of a file's source
upload and destination-folder confirmation.

Two additional actual kernel FUSE fixtures use an in-memory provider that refuses
unknown or nonfolder parent IDs. A held folder creation does not block nested mkdir,
file writes, fsync, reads or an independent root upload. After confirmation, children
have the correct provider parents. Folder inode identity survives cleanup and remount;
a remotely added child can then be read, appended and moved through local folder
aliases. An interrupted folder request enters review after restart, retaining nested
local bytes while an independent file uploads. File/directory name collisions are
refused. The cleanup assertion allows the existing two-second per-object maintenance
interval and its later physical-removal pass; production timing was not changed.

These are synthetic service/filesystem checks, not live OneDrive directory evidence.
Folder rename, safe folder removal, broader application saves, provider concurrency,
physical faults and bounded history/recovery remain acceptance gaps. All six product
milestones remain open, and ordinary mounted drives remain read-only.

The final local run passed 225 default workspace tests, 15 read-only/lifecycle
kernel fixtures and 18 writable-session kernel fixtures, plus formatting, strict
Clippy, build, smoke, two observer-script checks and Rustdoc.


## Retained routes to local changes (2026-09-07)

A journal reproduction first demonstrated that a remotely absent, already handed-off
folder could hide a later local child. Four synthetic journal checks now exercise
that case; source-link/target-collection routes, restart and release after confirmed
child handoff; name reuse, bounded capture and transaction rollback; and preservation
of foreign name occupants as explicit collisions. Captures create no provider intents.

Two added actual kernel fixtures cover existing native folders, a SharePoint-style
link to another collection, and a file link whose target has a different local and
provider ID. They remove the synthetic provider's items, commit a complete replacement
metadata baseline, and verify local bytes and directory traversal before and after
reopening both engine and journal. File-link edits address the target's current local
owner. It also rejects new opens through a dangling link after local target removal,
while an existing descriptor retains its bytes. No automatic cloud recreation is
queued. The provider fixture uses only generated local
data, so these checks do not establish real OneDrive/SharePoint compatibility.

Ancestor routes remain until the associated local objects hand off to remote metadata.
They do not redirect failed uploads, implement a recovery UI, supply content that was
never downloaded, or recover vanished paths absent from older journal records. Folder
rename/removal, large-library retention, live provider/application acceptance and the
other product milestones remain open.

Final local validation passed 229 default workspace tests and 35 actual synthetic
FUSE fixtures (15 read-only/lifecycle and 20 writable-session), plus formatting,
strict Clippy, build, smoke, two observer checks and Rustdoc.


## Namespace capacity baseline

A generated provider and actual temporary FUSE mount measured 500,000 zero-byte
file metadata entries across 500 directories. Metadata arrived in 1,000-entry
pages through the normal refresh/index path; no provider kept a 500k-node in-memory
fixture map. The service was indexed before the first memory baseline. Three
traversals changed every file's content revision between passes. No file contents,
real accounts or keyring credentials were used. Each sample was taken after all
application directory handles had closed.

| Sample | Retained views | Process RSS (MiB) | Open files/directories |
| --- | ---: | ---: | ---: |
| Indexed, before traversal | 1 | 20.6 | 0 / 0 |
| After revision 1 | 500,501 | 896.5 | 0 / 0 |
| After revision 2 | 1,000,501 | 1,763.6 | 0 / 0 |
| After revision 3 | 1,500,501 | 2,071.4 | 0 / 0 |

Peak process RSS reached 2,287.2 MiB. Indexed traversal made no foreground provider
metadata requests and no content reads. The complete debug-build fixture took
111.15 seconds locally. Runtime source was `5b1128e`; the added fixture is compiled
only into service tests. [Raw synthetic measurements](benchmarks/namespace-baseline.json)
include PSS, map capacity and per-phase timings.

This confirms the namespace-retention weakness and **does not pass** the proposed
memory gate. Closed application handles are not a measurement of kernel lookup
references; the later reference-lifetime correction is tested separately below. These numbers depend
on this fixture's names, directory shape, allocator and build. The run does not
measure real Graph latency, one huge directory, deep/duplicate aliases, held
mappings or a 24-hour session. The 10,000-file pilot also showed accumulation
(10,011, 20,011 and 30,011 views across revisions), before the full-size run.

See [reproduction](development.md#namespace-capacity-baseline) and the
[namespace lifetime decision](adr/0005-namespace-memory.md) for the next correctness,
reclamation and capacity checks. No installed service was replaced by this test.


## Directory-listing lifetime correction

Plain READDIR projections now stay in their directory handle's snapshot and are
not duplicated into the mount-wide resolved-view map. Kernel lookup references
are established by LOOKUP/create, not by returning names from plain READDIR.
This initial correction kept resolved views and old open-file versions retained. The dedicated
3,000-file kernel regression checks the retained-view bound after enumeration,
then keeps an old file open across a remote revision and verifies a distinct new
inode without overwriting the old view. This check also runs in CI.

Repeating the exact 500,000-file, three-revision workload above produced:

| Sample | Retained views | Process RSS (MiB) | Open files/directories |
| --- | ---: | ---: | ---: |
| Indexed, before traversal | 1 | 20.9 | 0 / 0 |
| After revision 1 | 501 | 30.8 | 0 / 0 |
| After revision 2 | 501 | 40.8 | 0 / 0 |
| After revision 3 | 501 | 49.9 | 0 / 0 |

The complete debug fixture took 110.66 seconds. No content or foreground provider
requests occurred. [Raw correction measurements](benchmarks/namespace-listing-lifetime.json)
record the same fields as the baseline. This is a reduction from 2,071.4 to 49.9 MiB
at the final sample for this workload, not a general memory bound or an API-speed
claim. Retained views plateau here, while RSS still increases across revisions;
allocator/SQLite/persistent-index effects need longer-session investigation.

A byte budget, directory reclamation, large-directory paging, scoped invalidation
and 24-hour churn remain open. A file manager that stats or opens every file
exercises a different lifetime from this name-enumeration workload. The subsequent
regular-file reference correction is tested separately below. These RSS results
belong to the listing-only correction, not a new measurement of later changes.
The OneDrive-1.0 namespace memory gate remains open.

## Regular-file reference lifetime

Five focused unit tests cover partial and final FORGET, shared operation/open-file
leases, replacement of an existing inode's path, stale collector generations,
checked underflow/overflow and bounded collection without discarding directory
ancestry. The actual-kernel fixture stats 300 generated files, retains one open
file and publishes another remote revision. All other regular-file views retire
after invalidation. A newly opened revision receives a different inode, while the
old descriptor still sees its original identity. Closing both descriptors and
invalidating their dentries allows the remaining file views to retire.

The fixture uses only generated metadata and a temporary mount, with zero content
reads or foreground provider requests. CI runs it alongside the 3,000-file
listing regression. This does not establish byte-budget compliance, complete
directory lifetimes, interrupted reply delivery or long-session capacity.

The unchanged 500,000-file name-enumeration benchmark was also repeated at
`e779209` after this correction. Resident views were 1 after indexing and 501
after each traversal; RSS was 21.0, 31.3, 41.3 and 50.4 MiB. The full debug fixture
took 113.22 seconds and made no foreground provider/content requests.
[Raw reference-lifetime measurements](benchmarks/namespace-reference-lifetime.json)
record the exact commit and PSS/peak fields. This checks that adding reference
accounting preserves the earlier listing improvement; the benchmark itself does
not exercise mass file lookups or close the memory gate.

At this revision, formatting, strict workspace Clippy, 239 default tests, 37
synthetic actual-kernel mount tests, workspace build, daemon smoke and Rustdoc
passed locally. The actual mount coverage includes old/new memory mappings,
in-flight reads, local saves, unlink, replacement, ancestor recovery and shutdown.

## Streamed foreground directory publication (2026-09-07)

First-time listings and active-directory refreshes now use bounded SQLite TEMP
staging per provider page, followed by one atomic main-database transaction. Store
fixtures compare staged publication with the legacy path through repeated moves
and absence, and exercise duplicate identities/names, invalid parents, repeated
cursors, scope isolation, stale responses, pending baselines and legacy default
fields. Failure during final insertion rolls back visible entries, absence and
supersession markers. An injected cancellation during bulk SQL exercises the
production progress hook and leaves the writer usable. A paused final transaction
allows another connection to read the previous listing within 500 ms.

Tests force TEMP storage beyond its page cache, check private/unlinked descriptors,
verify close cleanup, exhaust its page limit and verify autocommit between pages.
An abandoned Engine fetch discards its pages; a new request starts from the first
page and completes, followed by offline lookup. Actual kernel FUSE tests enumerate
a cold directory, release its anonymous snapshot, revisit offline and remount
without a completed delta baseline or additional provider/content requests.

Separate release processes measure a 500,000-file mounted cold listing and three
500,000-row Store publications (cold, unchanged, changed). Raw measurements and
resource exclusions are in [directory-publication.json](benchmarks/directory-publication.json).
These synthetic checks do not validate provider completeness, real Graph latency,
physical power loss, long-session memory, or writable local-overlay scalability.
The first-entry latency and overall namespace acceptance gates remain open.

The checked source passed formatting, strict workspace Clippy, 321 regular tests,
46 actual-kernel regressions, the workspace build, Rustdoc and service smoke/observer
checks. Builds used the worktree-specific target directory; expected nonzero
filtered test counts were verified.

## Targeted namespace invalidation (2026-09-07)

Metadata schema 6 adds a covering revision index. Tests page committed marks while
another connection replaces rows or resets a scope, without retaining a SQLite read
transaction between pages. Wrong-database positions are rejected. Migration failure
preserves the old version and rows, and large identities trigger the page-byte bound.
Query checks verify index use without full scans or sorts.

Live projection indexes retain both source and target identities for shortcuts,
including distinct aliases, accounts and content revisions. Unit tests cover updates,
retirement during paging and full/scope selection. A watch generation preserves wakes
received during work; local namespace changes explicitly request a full sweep.
Experimental writable mounts also use full sweeps for remote metadata because
their local IDs may differ from provider IDs. Writable handoff and retained-route
fixtures now signal actual metadata changes through that path.

An actual mounted 500,000-file index resolves 3,000 files and holds an old descriptor.
One changed file produces two metadata marks and two notified views, preserving
unrelated cached views and the old descriptor's size. A 100-file delta burst produces
201 marks and 101 notified views while 16 warm metadata calls satisfy the existing
500 ms navigation bound. Those calls may hit kernel caches. Another kernel fixture
checks duplicate shortcut targets, source-link rename, held old versions and target
scope reset. Full recovery releases unused views through actual kernel FORGET.
[Raw measurements](benchmarks/targeted-invalidation.json) include source hashes and
scope limits. These checks do not establish a resident byte budget or close the
combined large-library/24-hour acceptance gate.

The final source passed formatting, strict workspace Clippy, 328 regular tests,
48 actual-kernel regressions, the workspace build, Rustdoc and service smoke/observer
checks. The separate release 500k fixture and focused writable identity-handoff
regression also passed. Every build used the worktree-specific target directory,
and filtered commands were checked for the expected nonzero test counts.

## Shared projection metadata (2026-09-07)

Resolved views now share immutable metadata and route vectors; file siblings reuse
unchanged parent scopes, aliases and ancestry. The reverse index shares target
scopes and bounded notification batches share names. Replacing or detaching a
payload preserves older views; residency and parent leases retain their existing
independent ownership. Tests check unchanged serialized inode keys for ordinary
files, writable identity and shortcuts, distinct aliases, and old content revisions
after a detached edit. Existing retirement and invalidation tests exercise index
ordering by identity value rather than allocation address.

A release representation fixture measures 50,000 live views on three 12-level
routes, plus 32 held clones, with the production projection/index/residency code.
The owned baseline and shared version run in separate processes using the same
fixture. A larger 500k run measures the shared version separately. Both check
retirement to the root; process RSS remains higher afterward and is not a logical
allocation or eviction budget. The fixture does not mount FUSE or use SQLite.
[Raw measurements](benchmarks/shared-projection-payloads.json) include exact source
and fixture hashes, process memory and scope limits.

The final source passed formatting, strict workspace Clippy, 329 regular tests,
48 actual-kernel regressions, workspace build, Rustdoc and smoke/observer checks.
Separate release processes passed the 50k/500k representation fixtures and the
500k-index targeted-invalidation fixture. Builds used the dedicated worktree target,
and filtered commands were verified against their expected nonzero counts.

## Combined namespace workload runner (2026-09-07)

The synthetic kernel runner splits indexed files between primary and shared scopes,
projects the shared tree through two aliases, and traverses 12-level routes. It
keeps 24 old descriptors and three directory snapshots while changing eight file
versions and renaming another entry per collection. Every projected file is statted
in each of three passes, using eight workers and a 128-entry application queue.
Old descriptors retain their inodes, continued snapshots retain the old unbuffered
name, directory/alias inodes remain distinct and stable, and offline remount sees
the committed names without provider calls. Normal kernel invalidation/FORGET
releases all views except the root after each full pass.

An optional timed phase repeats changes and stats in a fixed active group, allowing
normal targeted invalidation to operate without a full sweep after every round.
The initial 65-second trial validates this control path, not a 24-hour gate. The
runner measures reference/index/candidate counts, snapshot storage, RSS/PSS and
metadata database/WAL/inode growth separately. These are not payload-byte accounting
or an allocator/SQLite buffer breakdown. Zero-length files exclude content/mapping
coverage. [Recorded small/short results](benchmarks/namespace-churn-fixture.json)
include exact source hashes and scope limits; large-library and sustained acceptance
remain open. The CI suite's outer timeout accommodates the added full traversal;
the individual 500 ms update/navigation bound remains unchanged.

During the preceding shared-metadata CI, one conservative sequential-read workload
exceeded the unchanged 500 ms cached-navigation limit. The duplicate PR job and
one targeted rerun passed; three fresh local runs of the same release binary
also passed with maximum samples below 3.4 ms. The cause remains unestablished.
The [original CI run](https://github.com/Dandiccf/cirrove/actions/runs/34152465330/attempts/1)
is retained as evidence for the open read-only latency acceptance work; successful
reruns do not establish sustained reliability.

The final runner source passed formatting, strict workspace Clippy, 329 regular
tests, 49 actual-kernel regressions, the workspace build, Rustdoc and smoke/observer
checks. Separate release-small and 65-second debug sustained runs also passed.
Builds used the dedicated worktree target, and filtered tests were checked against
their expected nonzero counts. These checks do not close the large-library gates.

## Compact resident metadata candidate (2026-09-07)

The resident inode map now also supplies ordered full invalidation traversal,
removing the duplicate inode index. Uncommon link targets are boxed, and exactly
equal live alias metadata can share payloads through a bounded search of the
existing identity index. This adds no cache, database state or payload storage.
Tests verify distinct alias lifetimes/inode keys, separation across accounts and
content versions, and unchanged serialized node data.

The measured Rust source passed formatting, strict workspace Clippy, 331 regular
tests, 49 actual-kernel regressions, workspace build, Rustdoc and smoke/observer
checks. Builds used the dedicated worktree target; filtered kernel commands were
checked for 15 read-only, eight capacity, six streamed-window and 20 experimental
write tests. The nested crash-recovery subprocess is not counted twice.

Fresh release processes compare the same representation fixture in three 50k
pairs and one 500k pair. At 500k, populated RSS falls from 457304 to 366124 KiB,
while population time rises from 622.87 to 692.47 ms in that pair. Both processes
retire to one root view but retain high RSS. The fixture excludes kernel, SQLite
and provider work. [Raw samples and source/executable hashes](benchmarks/compact-resident-metadata.json)
record the measured scope. Large combined kernel runs, resident byte accounting
and sustained/provider acceptance remain separate open gates.

## Interrupted namespace clients (2026-09-07)

An actual-kernel fixture pauses a synthetic provider at a known page during cold
LOOKUP or OPENDIR, sends SIGKILL only to the requesting Python client, then permits
the server to complete its original request. Both scenarios return all request
permits, release directory snapshots and retire lookup references to the root alone
through normal kernel cleanup. Completed metadata remains readable offline without
further page or content requests. The existing abandoned-fetch cancellation test
still passes. No production reference rollback or additional state was introduced.

The [CI log](https://github.com/Dandiccf/cirrove/actions/runs/34156972146/job/101850760978)
for Rust source `6307fbce9f6537c12699064e133ca8509b5b5b49` records both scenario
outputs, 331 regular tests and 50 actual-kernel regressions (15 read-only, nine
namespace, six streamed-window and 20 experimental-write tests). Formatting, strict
workspace Clippy, the native-window test, workspace build, smoke/observer checks and
Rustdoc also pass. The nested crash subprocess is not counted twice.

This is narrow signal-interruption coverage on the Ubuntu CI runner. It does not
inject a failed FUSE reply write, interrupt experimental CREATE, or establish
content/mapping integrity under combined large-library load. Those gates remain open.

## Content mappings during namespace churn (2026-09-07)

The content variant of the combined churn fixture passed on source
`24ceedf5cc742e13c18ff9a7bfd336f534b6eee5`. Two 8 KiB files are projected through
three routes. Six old/new memory mappings retain their kernel file lifetimes after
the original descriptors close, while the fixture stats every metadata file. It
compares all bytes, preserves distinct alias/version inodes, and reopens current
paths. Four content versions require eight range fetches; offline remount uses
the same inodes and disk-cached content with zero additional provider calls.

The [Linux CI log](https://github.com/Dandiccf/cirrove/actions/runs/34158229286/job/101854395944)
records 331 regular and 51 kernel tests, plus native-window, formatting, strict
Clippy, build, smoke/observer and Rustdoc checks. This is the small synthetic
fixture; large/sustained mappings, cache-eviction faults, private/COW mappings and
real-provider behavior remain separate gates.

The separate [conservative sequential-read workload](https://github.com/Dandiccf/cirrove/actions/runs/34158229286/job/101854396082)
exceeded its unchanged 500 ms cached-navigation bound. The duplicate push run
passed. This repeats the unresolved earlier navigation failure; it is not explained
by the content fixture passing in a different CI job.

A test-only diagnostic helper at `ce9c298d1d482fcf7d4f5b2da68a992e81169ef0` records
blocking-worker start, directory-open return, enumeration and metadata completion.
A failed check briefly observes the same worker without retrying or accepting late
completion. Aggregate transfer/staging counts and host pressure provide context,
not causal attribution. An explicit wall-clock check also prevents a ready worker
from bypassing the existing bound after delayed timer polling.

All four CI jobs passed for that helper: [Linux checks](https://github.com/Dandiccf/cirrove/actions/runs/34159190419/job/101857258108)
record 331 regular and 51 kernel tests, and the [PR workload job](https://github.com/Dandiccf/cirrove/actions/runs/34159190419/job/101857258236)
and [push workload job](https://github.com/Dandiccf/cirrove/actions/runs/34159106827/job/101857010854)
ran twelve successful mode/delay scenarios in total. Maximum sampled navigation
was 35.033 ms. These passes validate instrumentation; they do not establish a
root cause or a production fix for the earlier stalls.

## Directory prefixes before construction completes (2026-09-07)

Source `82d22db3fb6ee3973b0bb7d8a2d0162bc8d5ca35` publishes a flushed directory
prefix while later inode batches are still being constructed. In the synthetic
actual-kernel fixture, only the first 128 inode mappings are cached and a separate
SQLite writer blocks allocation for the remaining entries. The first meaningful
entry arrives in 4.642 ms and 4.448 ms in the two cases, while that writer is still
held; the unchanged bound is 500 ms. A committed rename cannot change the old
listing, while a fresh listing sees it. Closing during construction cancels the
producer after its blocked batch returns. Both cases retire all views except the
root and release all snapshot storage.

Unit tests verify that unpublished bytes cannot appear, waiting readers wake on
prefix/completion changes, and errors at either file's flush remain errors rather
than false EOF. Abandoned producers wake readers; the last reader closing cancels
further publication, while the writer keeps its reservation until its files close.

The [push Linux job](https://github.com/Dandiccf/cirrove/actions/runs/34160218749/job/101860272100)
passes 334 regular and 52 actual-kernel tests (15 read-only, 11 namespace, six
streamed-window, 20 experimental-write), native window, formatting, strict Clippy,
build, smoke/observer and Rustdoc checks. Both separate workload jobs pass all six
mode/delay scenarios, with maximum navigation 13.063 ms and 8.272 ms.

The [PR Linux job](https://github.com/Dandiccf/cirrove/actions/runs/34160221911/job/101860283655)
failed the existing three-second `ready(engine)` helper in
`real_active_directory_refreshes_during_a_stalled_read_without_push_or_delta`,
before constructing or mounting CloudFs. It passed 334 regular tests and 14 other
read-only tests. The missing ready-feed state was not recorded, so the startup
cause remains unresolved. The passing duplicate is not a cause or repair.
Large-directory first-entry/throughput measurements, sustained construction and
full capacity/provider acceptance remain open; the small kernel fixture establishes
the publication, consistency and close behavior, not those larger gates.

## Compact representation under combined 500k churn (2026-09-07)

Frozen release binaries at baseline `dba5ac8fd218813e3890510a0de2bdaf0eb109cb` and
compact `57752c2ac3c8168283f41a996ad11f44f76c3c1a` each complete three traversals
of 500,000 indexed files projected as 750,000 paths. Every path is statted. The
fixture includes deep paths, duplicate shared aliases, 24 held old descriptors,
three held directory snapshots, version changes/renames and offline remount.
Every completed release and final remount returns to the root view alone, with
zero kernel references, quarantined views or snapshot reservations.

| Topology | Pass | Baseline released RSS (MiB) | Compact released RSS (MiB) | Baseline traversal (s) | Compact traversal (s) |
| --- | ---: | ---: | ---: | ---: | ---: |
| 2,000 files/directory | 1 | 599.0 | 488.2 | 389.41 | 399.44 |
| 2,000 files/directory | 2 | 743.8 | 637.3 | 390.63 | 406.73 |
| 2,000 files/directory | 3 | 770.7 | 651.9 | 410.46 | 408.01 |
| 250,000 files/directory | 1 | 583.9 | 476.2 | 559.03 | 541.88 |
| 250,000 files/directory | 2 | 610.5 | 497.9 | 561.15 | 540.24 |
| 250,000 files/directory | 3 | 613.8 | 498.9 | 541.93 | 541.78 |

Final released RSS is about 15 percent lower with many directories and 18.7 percent
lower with giant directories. All navigation samples remain below the unchanged
500 ms bound: the largest is 8.591 ms for the many-directory pair and 69.482 ms for
the giant pair. Offline remount samples are reported separately in
[the full artifact](benchmarks/compact-namespace-churn.json), alongside source,
executable/log hashes, every memory/reference sample and measurement limits.

The giant pair ran sequentially, with explicit TMPDIR and SQLITE_TMPDIR on Btrfs
verified in the live processes; local builds and competing kernel benchmarks were
kept off the pair. The earlier many-directory compact process used tmpfs, verified
while live; the old baseline's temporary backing was not independently recorded,
and source compilation overlapped later parts of that baseline. Its timing
differences are observational. Comparisons across topologies/filesystems are not
controlled, and one pair does not establish a general speedup.

These are zero-byte metadata fixtures and use binaries from before the mapped-content
variant and early-prefix publication. Process RSS/PSS omit additional kernel/page
cache storage. The remaining high RSS is not proof of live-view leakage or of
reclaimable allocator memory; three passes do not establish a memory plateau.
Resident byte accounting/budgets, sustained 24-hour churn, large mapped-content and
real-provider acceptance remain open.

## Giant-directory first entry with prefix publication (2026-09-08)

A frozen release binary built from `7f64f57ecc8f6da185d74befd78ec3253cc260c5`
(SHA256 `996d4ae4d6f0f6e0ac8290eb76bd5afc859261524b1d8035d7215d05d2ae5ad3`,
Rust sources unchanged since `82d22db`) runs the actual-kernel
`filesystem::capacity::namespace_capacity_baseline` fixture with all 500,000
generated files in a single directory, over three changed-revision passes.

The first entry of that directory arrives in 4.076, 4.918 and 4.369 ms, against
the unchanged 500 ms bound. At that moment the published prefix holds 130 of
500,002 entries and 8,493 logical bytes, so the reader starts while the remaining
entries are still being produced. Each pass ends with every snapshot reservation
released, only the root view retained after invalidation, and no foreground
metadata or content request to the provider.

A controlled pair then measured the same fixture directly. Two frozen release
binaries, differing only in the directory path and built from `ce9c298` and this
source, ran alternately in one session on an idle machine, in the order
baseline / new / new / baseline / baseline / new. The measured fixture is
byte-identical in both sources.

| Measurement | Pre-change `ce9c298` | Prefix publication |
| --- | ---: | ---: |
| First entry, nine samples | 8,802-10,755 ms | 4.33-5.23 ms |
| First entry, median | 10,398 ms | 4.73 ms |
| Whole traversal, median | 10.585 s | 10.232 s |
| Indexing baseline, median | 6.272 s | 6.300 s |

The first-entry distributions do not overlap at any sample, and the median ratio
is 2,199. Whole-pass traversal is unchanged, a ratio of 0.967 with fully
overlapping ranges, so publication of the prefix costs no measurable throughput.
The unrelated indexing baseline agrees within 0.5 percent, which is what makes the
pairing usable at all. The slower traversal seconds against the earlier
[2026-09-07 artifact](benchmarks/directory-snapshot-pages.json) were therefore a
session difference, not streaming overhead; that earlier comparison was never
controlled and is retained only as context.

Post-invalidation RSS still rises across passes, 18.3 / 33.9 / 43.4 MiB, so this
run establishes neither a memory plateau nor the capacity gate. Mapped content,
writable overlays, desktop applications, real-provider acceptance and 24-hour
operation are all untested here.
See [the machine-readable results](benchmarks/early-directory-prefix.json).

## Startup readiness and linked-drive discovery (2026-09-08)

The [PR Linux job](https://github.com/Dandiccf/cirrove/actions/runs/34160221911/job/101860283655)
that failed `real_active_directory_refreshes_during_a_stalled_read_without_push_or_delta`
recorded nothing about the feeds behind its three-second timeout. The helper now
reports the last observed states instead of an anonymous `Elapsed`; the bound is
unchanged.

The recorded log narrows the conditions without settling the cause. That job was
not broadly slow: its whole kernel suite finished in 57.26 s against 59.45 s for
the passing job, and individual tests match within a few tenths of a second, except
the deliberately load-heavy thumbnail burst at 17.91 s against 10.27 s. The failure
happened 4.03 s into the test binary, in the first test of the step, about one
second after a 37.9 s build step ended. Store commits use `synchronous=FULL`, so an
fsync behind a fresh build's writeback is a plausible stall, but no measurement
proves it. Twelve local repetitions of the whole kernel suite on an idle machine
produced no failure.

The investigation did expose an independent recovery gap, now fixed. Discovery
errors were discarded and the worker then waited for another notification, which
only a successful feed poll sends. One transient store failure therefore left a
linked drive unsubscribed for a whole poll interval, an hour in that test account,
and permanently once the feed stopped succeeding. A unit fixture holds a real SQLite
writer until every discovery write exhausts the store's busy timeout, releases it,
and then requires the linked drive to appear with no further notification, while
asserting the sleeping feed never polled again. Without the retry the fixture fails
with the primary feed alone and its next attempt an hour away.

Measuring the bound's slack then made the host-stall explanation implausible.
The helper was temporarily instrumented to report how long the wait actually takes,
and reverted afterwards. On an idle machine both feeds are ready in 10.99 ms median,
11.76 ms worst; pinned to two cores against three competing spinners and two
continuous fsync loops, that rises only to 23.08 ms median and 27.70 ms worst. Heavy
load costs a factor of two, and the worst loaded sample still leaves the bound 108
times of headroom. A runner would have to be about 108 times slower than that
deliberately overloaded configuration for the three seconds to expire, while the same
job's other kernel tests matched the passing job within tenths of a second.

A discarded discovery error instead produces an unbounded wait, because the only feed
then sleeps for an hour. That matches a clean three-second timeout on an otherwise
healthy runner. Three stochastic reproductions found no failure: twelve idle runs of
the whole kernel suite, eight under twelve spinners and two fsync loops, and nine of
the failing test alone pinned to two loaded cores.

This is strong circumstantial evidence, not proof. The feed states at the moment of
the failure were never recorded, so the original failure is retained as a finding
rather than reclassified; the helper now reports them and a recurrence settles it.
See [the margin measurements](benchmarks/startup-readiness-margin.json).

## Cause of the cached-navigation stalls (2026-09-08)

The two recorded workload failures share the conservative mode and the sequential
phase, the only phase that downloads and durably publishes a gibibyte in 256
separate blocks. The failure-only stage diagnostic never fired again, so the
measurement was inverted: every navigation sample was temporarily reported by
stage, then the OPENDIR stage was split further. Both instrumentations were
reverted; no bound or production path was changed for them.

Two earlier reproduction attempts could not have worked. The three local rechecks
recorded as passing all ran with `synthetic_request_delay_ms` 0, while the later CI
failure occurred in the delay-20 iteration. More decisively, `/tmp` on the
development machine is tmpfs, so the fixture placed the database, the content cache
and the snapshot files in RAM; no local run had a filesystem in the path at all.

| Condition | Worst navigation sample |
| --- | ---: |
| tmpfs, no congestion | 3.894 ms |
| Btrfs, no congestion | 5.399 ms |
| Btrfs, four competing fsync writers | 182.998 ms |

Under that congestion the OPENDIR stage dominates: 96.484 ms worst against 2.900 ms
for enumeration, 9.565 ms for metadata and 2.748 ms for the reply. Splitting OPENDIR
separates the components cleanly.

| Component inside OPENDIR | Quiet filesystem | Congested filesystem | Factor |
| --- | ---: | ---: | ---: |
| SQLite connection open | 0.207 ms | 2.453 ms | 12 |
| Snapshot file creation | 0.058 ms | 51.269 ms | 884 |

The navigation path touches the filesystem that the content cache is saturating, and
that contention is what the tail follows. File creation is the component with the
largest amplification: every cached OPENDIR created two anonymous snapshot files even
for a directory holding one entry, and creating them is a filesystem metadata
operation that queues behind the congested transaction. It is not the only touch,
though, and removing it alone does not move the tail; see the
[paired measurement](#resident-directory-listings-and-what-they-do-not-fix-2026-09-08).
Filesystem contention matches every property of the recorded failures: the phase, the
mode, the intermittency, and one failure landing in the first test immediately after a
build step whose writeback was still in flight.

This is not proof. No local run reached 500 ms; the worst was 183.0 ms on a fast
local NVMe, and CI runners use slower shared storage. No CI stall has been captured
since the diagnostic landed, so the attribution rests on the mechanism and its
scaling rather than on a recorded failure. The original failures stay findings. See
[the measurements](benchmarks/navigation-stall-cause.json).

## Resident directory listings, and what they do not fix (2026-09-08)

An ordinary cached listing no longer touches the filesystem. A listing stays
resident until its data and index reach 64 KiB and spills to anonymous files only
beyond that, so a mount holds at most 16 MiB across its 256 snapshots. Readers share
the backing, so a spill stays visible to a handle that already published positions,
while the producer keeps independent writer handles. Byte accounting, the reader
frontier, error semantics and anonymity are unchanged. Starting a listing performs no
filesystem operation, so an unusable state directory is now reported when the listing
spills rather than at open, and a small listing keeps working without one.

Two new unit tests cover the resident and spilled forms and, in particular, a spill
that happens underneath an open reader that has already published positions.

**This does not fix the stalls.** A controlled pair of frozen release binaries
differing only in this change, run alternately under four competing fsync writers:

| | Worst sequential sample | p95 |
| --- | --- | --- |
| Before | 87.1 / 140.7 / 85.6 ms | 28.8 / 22.0 / 26.4 ms |
| After | 81.1 / 79.7 / 183.4 ms | 23.4 / 27.3 / 20.6 ms |
| Median ratio | 1.07 | 1.13 |

Both ratios sit inside the run-to-run spread, and one post-change run reached
183.4 ms. An uncontrolled single-run comparison had suggested 183 ms falling to
130 ms; the paired measurement does not support that, and it is kept only as a
reminder of why the pair was necessary.

The reason is that file creation was one of several disk touches. Navigation still
opens the metadata database and reads its pages from the same filesystem the content
cache saturates, so removing one touch leaves the tail where it was. Meeting the
bound under congestion needs navigation metadata served without touching that
filesystem, less write pressure from content publication, or separate storage for
the two. That is a design decision rather than a local fix, and the recorded
failures stay open. See [the measurements](benchmarks/navigation-stall-cause.json).

## The write-ahead log kept open, and the stall closed (2026-09-08)

CI captured the stall with the stage diagnostic in place:
[the failing job](https://github.com/Dandiccf/cirrove/actions/runs/34190151083/job/101946380643)
observed 501.763 ms against the 500 ms bound. The decomposition is unambiguous:
0.019 ms to admit the blocking worker, **504.947 ms inside LOOKUP and OPENDIR**,
then 0.157 ms to enumerate and 2.888 ms for the metadata lookup. Host pressure at
that moment was 6.61 percent I/O *full*, 0.00 percent CPU full and 0.00 percent
memory full: every runnable task was blocked on storage, not on CPU or memory. The
stall landed 4.5 seconds after a two-minute-58-second release build finished, while
its writeback was still draining, and 367 MB into the download. That binary already
contained the resident-listing change, so no snapshot file was created.

The cause is the SQLite write-ahead log lifetime. Nothing held a connection to
`metadata.db` open, so every `Store::open` was both the first and the last connection
to a WAL database. SQLite therefore creates `metadata.db-wal` and `-shm` on open and,
on close, checkpoints with two fsyncs and unlinks both files: two file creations, two
fsyncs and two unlinks per connection, at two to three connections per navigation,
inside the blocking task the FUSE reply awaits, on the filesystem the content cache is
saturating. The earlier split measured connection open but never connection close,
which is why 2.453 ms plus 51.269 ms accounted for only 54 ms of the 96.484 ms
OPENDIR stage, and why removing the snapshot files did not move the tail.

Measured in isolation on this Btrfs volume, open-and-drop cycles with and without one
idle connection held:

| | Without keeper | With keeper |
| --- | --- | --- |
| Quiet, p50 / p95 / max | 0.139 / 0.150 / 0.379 ms | 0.044 / 0.049 / 0.331 ms |
| Four fsync writers, p50 / p95 / max | 0.224 / 2.849 / 9.220 ms | 0.049 / 0.057 / 0.262 ms |

The decisive property is not the ratio but the decoupling: with the connection held,
congested is statistically identical to quiet, 0.057 against 0.049 ms at p95.

A controlled pair of frozen release binaries differing only in the keeper, run
alternately under four competing fsync writers:

| | Worst sequential sample | p95 |
| --- | --- | --- |
| Before | 237.9 / 66.4 / 46.9 ms | 15.84 / 13.28 / 13.58 ms |
| After | 19.1 / 10.4 / 25.6 ms | 4.51 / 4.41 / 4.58 ms |
| Median ratio | 3.49 | 3.01 |

The distributions do not overlap: every post-change maximum is below every
pre-change maximum. The p95 spread also collapses, from 13.28-15.84 ms to
4.41-4.58 ms. Headroom against the unchanged 500 ms bound rises from 2.1 to 19.5
times, which is what the requirement of holding on unknown user hardware needs.

`engine::persistence::cached_reads_never_create_or_unlink_the_write_ahead_log` is the
regression gate. It asserts the log and its shared index exist continuously across
cached reads and are retired on shutdown, so it measures the mechanism rather than a
duration and is hardware independent. It fails without the keeper.

The content path remains the source of the congestion: about 2,627 fsyncs per
gibibyte, 256 file creations, 256 renames and roughly 1,781 unlinks. Cache blocks are
re-downloadable, so that durability is not required, and reducing it would widen the
margin further on slow storage. See
[the measurements](benchmarks/navigation-stall-cause.json).

## Acceptance under congestion, and what it does and does not settle (2026-09-08)

Twelve alternating runs of the conservative sequential workload on one machine
under four competing fsync writers on the same Btrfs filesystem, six with the
pre-hardening binary and six with the hardened one:

| | Runs | Bound violations | Worst sample per run |
| --- | ---: | ---: | --- |
| Before | 6 | **3** | 44.8 / 85.1 / 181.3 ms, plus three aborted at the bound |
| After | 6 | **0** | 10.1 / 10.9 / 10.9 / 11.4 / 20.3 / 36.6 ms |

The three failures reproduce the recorded CI stall exactly: everything sits in
LOOKUP and OPENDIR, which returned only after 1,959 ms and 1,763 ms in the two
cases where the two-second diagnostic window caught it, while the enumeration that
followed took 0.05 ms. This is the first local reproduction of the failure, and it
makes the workload usable as an acceptance gate rather than a hope.

**What this does not settle.** A completeness review of the whole investigation
raised three objections that hold up.

First, the failing fixture seeds seven nodes, so its metadata database is a few
hundred kilobytes. Any argument from page-cache eviction or from a warm private
page cache — including the measured 22-fold cost of a foreign commit on a 95 MB
index — applies to a real library, not to this test. The block-index split is
retained for users whose index does not fit comfortably in the page cache, and is
not claimed as a cause of the recorded failures.

Second, all three recorded failures land within seconds of a large build
finishing, so the dominant writer at that moment was the build, not Cirrove, which
had written about 375 MB in 4.2 seconds. Every write-side change reduces Cirrove's
share of a congestion it did not dominate. The connection lifetime is a different
kind of fix, and the one the evidence supports: it removes durable filesystem work
from the read path regardless of who is congesting the device.

Third, FUSE dispatch is single-threaded. `fuser` defaults `n_threads` to one and
the mount never overrides it, so every request is dispatched from one thread, and
`lookup`, `getattr` and `opendir` each take the view lock on it before spawning.
During the sequential phase the application issues 16,384 reads through that same
thread. A single 505 ms block followed three milliseconds later by a 2.888 ms
operation of the same class is as consistent with a queue as with per-operation
cost, and no measurement so far distinguishes them, because every stage timestamp
is taken in the client. A server-side measurement of the handler is in progress.

## Concurrent FUSE dispatch is not yet safe (2026-09-08)

Head-of-line blocking behind bulk reads is a structural risk: `fuser` defaults
`n_threads` to one, so every request is decoded and dispatched from a single
thread while an application read of a gibibyte issues 16,384 of them.

Raising it to four does not work today. With four dispatch threads,
`filesystem::capacity::real_combined_namespace_churn_preserves_mapped_content`
fails with three namespace views still alive after the twelve-second settle
window, where one is expected. Twelve seconds is far past any scheduling wobble,
so the views are genuinely not released: concurrent dispatch reorders requests
against the lookup-count bookkeeping that a view's lifetime depends on, and that
bookkeeping is completed in a spawned task rather than before the reply.

The change is therefore reverted and the reason recorded at the mount site.
Making the reference accounting order-independent is a prerequisite for
addressing head-of-line blocking, and it is a change to kernel-facing lifetime
semantics that must not be rushed. Until then, a single slow handler can still
delay every request behind it, which bounds how much the per-operation work
removed elsewhere can guarantee.

A server-side measurement of the OPENDIR handler under four competing fsync
writers, six runs per variant, shows what that work costs today:

| | Handler maximum | Samples above 20 ms per run |
| --- | ---: | ---: |
| Before the hardening | 39.8-85.1 ms | 16-26 |
| After | 21.1-26.2 ms | 0-2 |

Neither variant produced a bound violation during those instrumented runs, so
this measures handler cost, not the extreme tail. Whether a 500 ms event is
handler work or queueing remains unsettled, because no instrumented run captured
one.

## Final acceptance of the complete hardening (2026-09-08)

Twelve alternating runs of the conservative sequential workload under four bounded
competing fsync writers on the same Btrfs filesystem, frozen binaries, six with the
pre-hardening source and six with the complete hardening.

| | Worst sample per run | Median of maxima | Median p95 | Headroom to the bound |
| --- | --- | ---: | ---: | ---: |
| Before | 110.5 / 120.0 / 135.6 / 159.7 / 185.9 / 371.7 ms | 147.7 ms | 38.11 ms | 1.3x |
| After | 7.9 / 10.5 / 12.3 / 12.4 / 13.1 / 18.1 ms | 12.4 ms | 5.02 ms | **27.6x** |

The distributions do not overlap: every hardened run is below every pre-hardening
run, by a factor of six to twenty. That is the number the product requirement turns
on. A machine twenty times slower than this one still holds the 500 ms bound with
the hardening, and does not without it — the pre-hardening worst sample already sat
at 1.3 times the bound on a fast encrypted NVMe.

Neither variant violated the bound in this round. An earlier round of the same
shape produced three violations in six pre-hardening runs and none in six hardened
runs, which remains the sharper result; this one measures the margin rather than
the failure rate.

## A store write-contention fragility, found while landing the stack (2026-09-08)

Updating pull request #30 against the moved `main` produced a red `linux` check on
`cirrove-store`'s `concurrent_observations_and_delta_commits_preserve_all_completed_listings`,
with `SQLITE_BUSY`, "database is locked". The duplicate job for the same commit on
another runner passed, so this is load sensitivity rather than a logic error, and it
is not caused by anything in this branch: pull request #30 predates the hardening.

The mechanism is the same class this branch has been working on. The test runs four
threads, each committing a hundred write transactions to one database at
`synchronous=FULL`, so four hundred fsyncing commits serialise behind SQLite's single
writer while a three-second busy timeout runs. On a congested runner one waiter
exhausts that timeout.

It matters beyond the test. `observe_directory` and the delta commit path return that
error to callers, which map it to `ProviderError::Unavailable` and therefore to an
I/O error at the kernel boundary, so a user writing heavily could see a spurious
failure on a namespace operation rather than a slow success. The test's expectation —
that every concurrent writer succeeds within the busy timeout — is stronger than
SQLite guarantees.

This is recorded as an open finding rather than repaired here, because it predates
this work and its repair changes error semantics across the store. The pull request
was unblocked by re-running the failed job, which does not explain or fix anything.
