# Reusable native package Create adapter: validation plan

Registered before adapter integration or test execution. Goal: replace the owned
one-shot validator's upload-only path with a reusable UploadProvider that fits
Cirrove's durable journal and can independently verify completion after a lost
response. This is a step toward full native writing, not completion of that goal.

Implementation supplied as three ordered patches: reusable adapter/shared wire
transport, whole HTTPS roundtrip tests and receipt/ownership corrections, then
post-session-restoration cancellation guard with a deterministic dispatch test.
Normal routing and installed accounts remain unchanged during these checks.

The adapter binds account, operation, complete upload request, parent, raw archive
receipt and explicit root-bound semantic identity. Allocation must be durably
armed once. A missing allocated ID after an unknown response remains uncertain;
restart only observes state. A successful registration response is insufficient:
the exact allocated ID must be downloaded as a package, content checked and
metadata reobserved before a Folder+package receipt is returned. Archive size
and remote logical size need not match.

Synthetic arms use private disk staging and a dedicated loopback TLS fixture,
explicit DNS override, disabled proxies and a synthetic test certificate. Signed
URL validation is unchanged. Whole-adapter tests cover success with different
archive wrappers, lost registration response followed by fresh-adapter readback,
and successful registration with wrong downloaded content. Request counts must
show one allocation, one body and one registration, and no mutation in recovery.

Cancellation arm: vault loading cancels in the same poll that returns a valid
session. A cfg(test) immediate dispatch sentinel establishes that no body dispatch
occurs; zero server bytes alone could hide a wrongly entered cancelled request.
Prediction: removing the post-load guard makes dispatch count one. Other negative
controls remove slot-none uncertainty or content verification and must make the
relevant refusal tests fail. Restore each control before further work.

Raw and semantic payload validation occurs before transmitting content. The
allocation callback has no payload descriptor, so production service admission
must validate an immutable private archive before enqueue; caller-provided hashes
are not trusted. This adapter alone does not authorize a normal mount import.

Remaining gates include service admission/router integration, immediate metadata
publication, bounded owned-account live validation, installed acceptance and
existing-document replacement/editing. No claim of same-ID conditional native
replacement or cloud atomicity is made by Create support.

Integrated adapter, HTTPS arms and cancellation guard passed all11 tests at
09:44:26 UTC. Offline Cargo resolution added only the existing tokio-rustls dev
edge and its rustls log feature to Cargo.lock; no new package/version was fetched.
At09:44:48 UTC removing only the post-restore cancellation guard failed the
regression with one dispatch instead of zero. The guard was restored immediately.
Artifacts: `.local-state/icloud-access-native-package-adapter-tests-2026-10-01/`
and `.local-state/icloud-access-package-body-cancel-negative-2026-10-01/`.
These are synthetic adapter results, not installed or live-account acceptance.

After restoration, all11 adapter tests passed again at09:45:11 UTC. Targeted
Clippy identified only the repeated Armed suffix on persisted phase names. Those
names intentionally describe uncertainty rather than completion; a narrow enum
lint allowance now documents that reason without changing checkpoint encoding.

Service routing and typed-parent restoration were integrated next. Three tests
passed at09:47:15 UTC: callback routing, cacheless/credential-free recovery and
large raw checkpoint plus exact journal binding. Removing only the journal's
representation-equality guard made the binding test fail at09:47:50 UTC: a
self-consistent but altered request/checkpoint was incorrectly admitted. The
guard was restored before further testing. Raw package checkpoints remain bounded
at96KiB and are never nested inside the ordinary FILE envelope.
Artifacts: `.local-state/icloud-access-native-package-router-tests-2026-10-01/`
and `.local-state/icloud-access-package-router-binding-negative-2026-10-01/`.
Public admission/CLI and mounted publication remain to be connected; a passing
router test does not constitute user-facing import acceptance.

All three router tests passed again after restoring journal binding at09:48:11
UTC. Admission foundation then integrated: private descriptor-pinned local source,
anonymous read-only validated snapshot and journal raw-receipt verification before
object/row publication. First build stopped on a missing CancellationToken import;
adding that import allowed all seven admission tests to pass at09:49:43 UTC.
Artifact `.local-state/icloud-access-native-import-admission-tests2-2026-10-01/`.
The ENOSPC case injects a write failure; it is not real full-device acceptance.
Manager lifecycle, public socket/CLI and immediate mounted publication remain open.

The admission publication negative removed only expected raw size/hash checking.
It failed at09:50:52 UTC because mismatched bytes were incorrectly accepted; the
check was restored and all seven admission tests passed at09:51:13 UTC.
Artifacts `.local-state/icloud-access-native-import-receipt-negative-2026-10-01/`
and `.local-state/icloud-access-native-import-admission-restored-2026-10-01/`.
The immutable capture is not enough by itself: the exact copied bytes must remain
bound through journal publication, as this control demonstrates.

## Public import and metadata publication integration

The public job must not succeed at journal admission or upload acknowledgement: it must observe the exact queued package receipt and revision-fenced local metadata publication. The publication outbox is durable and independent of upload pumps; metadata failures never repeat the upload. Destination reservations use the indexed active queue and unpublished package receipts, rather than all historical uploads.

Prediction before publication tests: acknowledged packages enter a warm directory without downloading content; interrupted publication resumes; stale observations cannot acknowledge another receipt; failed refresh retains its receipt with bounded cooldown.

Combined native-import tests on 2026-10-01 10:06:47–10:07:19 UTC passed all 15 selected tests. The first combined compile exposed sibling-method visibility and missing equality derives, corrected before this run. These are synthetic admission/job tests, not live-provider or installed acceptance. Publication, CLI, and full workspace checks remain outstanding.

Publication tests passed all six at 10:07:55 UTC. Disabling only the enqueue
triggers made the warm-directory publication test fail at 10:08:22, demonstrating
that it exercises the new durable queue. Restoring the triggers passed all six
again at 10:09:55. The explicit CLI argument test passed at 10:09:34; it rejects
an empty account label before submission as well as a missing source root.
Observer cancellation/deadline and atomic job completion are being checked next.

The blocked-observer cancellation/deadline test passed before negative control.
Replacing its interruptible wait with the previous unconditional read await made
it fail after the test's one-second bound at 10:11:13 UTC. The interruptible
implementation was restored. Atomic job completion is a separate boundary.

Restored observer tests passed both cases at 10:11:59 UTC. Workspace/all-target
Clippy passed at 10:12:22 before the final job completion change. The new accepted
stop regression test with the old two-lock completion failed at 10:12:44
(`Succeeded` versus `Stopped`); the atomic implementation was restored.

### Remaining native replacement protocol evidence

The current create adapter, mounted replacement validator, handoff transport,
and package journal acknowledgement all deliberately exclude package Replace.
`add_package` establishes fresh allocation, not conditional in-place update.
Before extending these guards, validate on disposable owned Pages packages:
version-A semantic backup; separately edited version-B Pages archive; public
create to a staging name; stale-revision Trash refusal and current-revision
Trash preservation with independent package readback; staged rename and exact
identity/semantic verification; interruption recovery at both mutation boundaries.
Do not infer PACKAGE Trash behavior from ordinary-file tests or enable Numbers
and Keynote from Pages evidence. This is a proposed next experiment, not executed
acceptance, and does not establish atomic replacement.

## Full checkpoint validation

All 21 native-import tests passed after restoring the atomic completion fix at
10:13:06 UTC. `scripts/check.sh` then passed in full from 10:13:17 to 10:22:29 UTC
(manifest/log: `.local-state/icloud-access-native-import-public-fullcheck-2026-10-01`).
This includes formatting, all-target Clippy, feature probe checks, workspace and
real FUSE suites, scripts, ledger and docs. Rustdoc emitted the pre-existing
private Writeback::retry_stuck link warning in manager.rs; command exited zero.
Window scenarios and the newly prepared socket-to-mount import fixture were not
part of this checkpoint. No installation or live public import was performed.
