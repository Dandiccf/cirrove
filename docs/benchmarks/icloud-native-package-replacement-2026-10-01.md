# Native iCloud package replacement foundation — 2026-10-01

## Scope

This foundation binds an explicit validated replacement archive, the selected
original revision, and both semantic content identities. It reuses the existing
upload worker, identity-handoff reservation and metadata publication outbox.
Explicit replacement now has account-bound socket/CLI submission and retained
operation discovery/watch. Ordinary FUSE package saves remain disabled. No real
cloud package replacement has been attempted by this change.

The replacement creates a distinct provider identity and retains the original in
recoverable Trash. It does not preserve the original sharing link or history.
Remote logical size is distinct from the sealed archive byte count.

## Persistence compatibility

The new persisted representation and receipt require journal schema17. Every
writable journal opened by this version migrates to17, including ordinary-only
accounts. Older writers refuse it; there is no supported automatic downgrade.
The installed daemon and account journals have not been migrated by these tests.
Synthetic tests preserve schema16 ordinary payloads through migration and verify
read-only recovery. This is not execution of an old binary against a new journal.

## Synthetic results

Six journal tests passed in `native-package-handoff-foundation-recovery-fixed`:
exact two-ID acknowledgement/recovery ownership, forged receipts, explicit
admission/conflicting owners, account/cancellation refusal, actual shared worker
lost-reply/restart reconciliation with one commit, and schema compatibility.
The initial admission test incorrectly expected a retry to become a fresh write;
it now requires no ordinary claim and an explicit verification claim instead.
Production restart behavior was unchanged by that correction.

The `native-package-negative-semantic` arm removed only the new-content semantic
comparison. The forged-receipt test failed at fault1 (exit101). Source was restored
in a finally block; the full positive check remains required after restoration.

Five transport tests passed in `native-package-handoff-transport-tests`: legacy
proof decoding, account/cancellation boundaries, both content proofs before
mutation, diverged originals, and already-staged Trash/rename transport with
lost-response read-only reconciliation. These use synthetic HTTP, not Apple.

Each arm retains its command, source hashes, private disk temporary filesystem,
process status and test log under `.local-state/icloud-access-<arm>-2026-10-01/`.

## Remaining integration

The durable package staging-to-handoff coordinator is integrated and routed
through the explicit native replacement service path.
Journal admission does not independently establish remote ancestry or original
contents; the final admission and provider adapter must do so. Publication tests
prove durable scheduling, not mounted alias convergence or held-reader behavior.
Valid edited Pages/Numbers/Keynote live application tests and installed acceptance
remain open. No release gate closes on this foundation alone.

## Coordinator evidence

`native-package-coordinator-tests` passed six tests: actual synthetic HTTPS
allocation/body/registration through old-package Trash and new-package rename,
remote logical/archive size separation, lost replies at all three mutation
phases with fresh reconstruction and inspection-only recovery, exact request and
proof binding, changed originals before allocation, and bounded checkpoint
serialization. This direct coordinator test is not the complete service worker.

`native-package-coordinator-negative-proof` removed only the staged semantic
comparison. The binding test failed with `accepted staged_semantic` (exit101).
The source was restored before further validation. The full shared-worker test
currently uses a synthetic provider, so actual coordinator/worker integration
and cancellation must still be demonstrated before enabling routing.

The first full check stopped on an old-schema publication fixture that tried to
remove old trigger names from a current-schema database. The private fixture now
removes the version17 triggers/index before explicitly modeling schema15; no
production migration behavior was relaxed. Full check will be rerun on the
combined foundation and coordinator.

## Combined project validation

The complete `scripts/check.sh` passed at 2026-10-01T16:03:07.552987+00:00
(arm `native-package-coordinator-fullcheck`), including all workspace/feature
tests, actual FUSE tests, scripts, ledger and docs. Both negative-control source
changes were restored; the corresponding positive tests passed in this run.
Native DATA admission regression tests also passed. The existing rustdoc link
warning remains. No graphical window scenarios or live cloud package replacement
were performed. The installed daemon and journal remain untouched.

## Admission and kernel publication follow-up

The internal explicit replacement admission now captures the local validated
archive and independently reads the selected original's semantic identity before
rechecking account/mount/route/revision/frontier and durably enqueuing. Four
`native_replace_` tests passed, including a deterministic pause after durable
enqueue but before the outer result. `native-replace-negative-late-cancel`
inserted a cancellation refusal after that pause and failed with "durable
operation lost to late cancellation". Restored code passed all59 native service
regressions (one ignored kernel test) in
`native-replace-admission-restored-and-trash-regression`.

`native-replacement-kernel-publication` and `native-replacement-kernel-restored`
passed the real FUSE replacement publication fixture: warm view resolves to the
new package without duplicate names, new opens read new generated bytes, a held
old reader keeps old bytes, and interrupted publication resumes after journal
and mount reopening without upload calls. The test injects a validated typed
handoff receipt to isolate publication; its bytes model immutable generated
artifacts and are not Apple renderer or cloud protocol evidence. It does not
claim a descriptor survives unmount.

The negative arm `native-replacement-kernel-negative-publication` omitted only
the durable publication acknowledgement. It failed after10 seconds specifically
because the operation was not durably acknowledged, even though independent
metadata refresh could make the path readable. Source was restored; the positive
kernel test then passed again in1.70 seconds. No production publication fix was
needed for this fixture. The internal admission and new kernel test still need
the next complete project check before commit; public submission remains absent.

## Actual worker and router integration

`native-package-worker-transport` passed both actual TransferWorker + native
coordinator synthetic HTTPS tests. Lost registration, Trash and rename replies
reopened encrypted checkpoints and retained exact source bytes; all five remote
mutation counters remained one. Task abort and cancellation after server-side
Trash also resumed without replay. This is synthetic process-state reconstruction,
not a real process crash or Apple evidence. Test wrapping keys use an injected
memory vault; real encrypted files are exercised without desktop keyring access.

`native-package-router-fixed` passed six routing tests. The initial run had one
fixture failure: the ancestor arm marked the trusted root as a package, although
the existing ancestry contract excludes that root. The fixture now uses a genuine
non-root intermediate package ancestor. Production guards were not weakened.

Next negative control: suppress only checkpoint persistence for MoveOld while
retaining journal references. Prediction: the abort/cancellation worker test must
reject the durable checkpoint phase after the remote Trash pause. Restore source
before the positive repeat. This evaluates checkpoint ordering, not performance.

The MoveOld checkpoint negative failed exactly at the expected durable phase
assertion (`null` instead of `move_old`), not during compilation. Source restored
in a finally block; `native-package-worker-restored` passed both worker tests.

`native-replace-observer` passed all12 selected tests: four admission tests, three
router tests, four observer tests and one job state test. Watches require durable
publication, retain the operation on stop, refuse wrong accounts/withdrawn mounts
and reject forged semantic/identity receipts. Rejoin constructs a new Manager
while retaining Engine/journal; this is not full process restart evidence. The
UI labels backup information as a historical Trash receipt, because a later
external restore/deletion can change its current availability. Public socket/CLI
submission and live native replacement remain pending.

## Retained operation discovery

`native-replacement-retained-list` passed three tests for scoped historical
operation discovery, bounded pages without dropped overflow rows, read-only
database preservation and malformed receipt rejection. Review added consistency
checks for the reservation's original identity and Trash parent.

Next negative arm deliberately advances the cursor past an unreturned budget
overflow row. Prediction: the exact operation sequence test fails because one
operation is omitted; source will be restored before the full project check.

The cursor negative failed at the exact UUID sequence assertion: budget-overflow
rows disappeared as predicted. Original source was restored. The complete
project check now repeats the positive cases on the combined admission, real
worker, router, observer, retained discovery and kernel publication changes.

## Selected archive resolution

`native-archive-resolution-import-fixed` passed eight tests of exact generated
archive/source binding, fresh revision and representation checks, cached-byte
integrity, protected ancestry and bounded cancellation. Initial compilation
needed a missing UUID test import; an incorrect import placement was corrected
before execution. This capability returns read evidence only and does not yet
authorize ordinary FUSE writes.

Next negative arm deliberately holds the resolver permit in the async waiter
instead of its blocking parser. Prediction: cancelling the waiter prematurely
releases capacity and the deterministic concurrency assertion fails. Restore
source before positive validation.

The initial resolver permit negative was inconclusive: it failed on a synthetic
server timeout rather than the intended capacity assertion. Review found a
queued third waiter could consume an incorrectly released slot before the
assertion. The fixture now drops that waiter before cancellation and handles
bounded peer connections independently. Repeat the same negative mutation;
require the explicit still-running parser capacity assertion to fail.

The combined full check passed Rust workspace/feature and actual kernel tests,
then stopped because three UI strings were absent from the POT template. The
catalogues were regenerated (303 German translations). The public surface then
passed nine library and one CLI tests after moving its tests into the correct
test module. A new public worker lifecycle test found raw cached membership
was unsuitable for subsequent selected operations; its red regression reproduces
the selection refusal. Canonical selection is being corrected while preserving
exact revision/identity/owner checks. No full-check success is claimed yet.

## Public lifecycle and follow-up selection result

`native-replacement-public-socket-restored` passed the complete synthetic public
socket → admission → shared worker → typed acknowledgement → canonical publication
path. It deliberately loses the initial reply, discovers the durable operation,
stops/rejoins observation, clears all jobs and reconstructs Manager/socket, then
observes the same operation without another allocation/body/capture. Engine and
journal remain open; this is not a full process restart claim. The final arm
explicitly submits a second replacement of the newly confirmed identity.

The original fixture incorrectly asserted uniqueness in raw cached remote
membership. It now checks the actual mounted canonical projection. More
substantively, raw membership also blocked follow-up selection;
`native-replacement-followup-red` reproduced that refusal. Canonical selection
plus exact clean-owner translation fixes it. A further diagnostic isolated an
already-following owner incorrectly passed to the candidate-for-handoff clean
predicate. Already-following owners now require no latest/working operation;
other owners still require the original clean predicate. Full exact node/remote,
frontier, revision and busy-resource checks remain.

`native-following-owner-regression` passed actual journal enqueue/claim/typed
ack/publication/handoff, subsequent selection, stale/cancel refusal and rejection
of an overlapping queued successor. Standalone native Trash still refuses a
retained namespace owner; this limitation is not erased by selection success.

The corrected permit negative failed exactly with one prematurely available
slot (`cancelled waiter released a still-running blocking parser slot`).
`native-archive-resolution-restored` then passed all eight positive resolver
tests. Source mutations were restored before this combined project validation.

## Combined checkpoint validation

`native-public-replacement-combined-fullcheck-cli-fixed` ran the complete
`scripts/check.sh` from 2026-10-01 16:55:55 to 17:05:35 UTC and exited 0.
Formatting, clippy, workspace and feature tests, real kernel writable-session
tests, scripts, translations, ledger and documentation checks passed. Rustdoc
reported a broken intra-doc link to `filesystem::writeback::Writeback::retry_stuck`;
the documentation build completed. Display-dependent window scenarios are not
part of this check. This validates the development checkpoint, not installed
acceptance or real-provider replacement reliability.

## Follow-up native Trash admission

The retained-owner restriction is narrowed only for an exact already-following
owner with no latest/working operation, unchanged full scoped node/remote identity
and no unlink. Nonfollowing owners remain refused. The stable owner is retained;
verified absence controls listing. This does not claim a new held-FD kernel run.

The first red/green fixture attempts both failed early because the synthetic
publisher omitted Engine::refresh_node before recording publication. Those are
not proof of the guard change. After matching production publication ordering,
`native-trash-following-corrected-red` failed with the original blanket-owner
refusal; `native-trash-following-corrected-fixed` passed. The test binds the NEW
replacement ID (not original recovery ID), rejects wrong revision/account,
retains owner identity, checks absence projection and refuses overlapping Trash.
Full project validation of this subsequent change is still pending.

The subsequent full check `native-package-retry-and-trash-fullcheck` stopped at
the old public-socket fixture's blanket Trash-refusal assertion. That assertion
encoded the now-corrected limitation, not a security invariant. The fixture now
continues with its second replacement without first queuing Trash; the dedicated
following-owner regression covers successful Trash and overlapping-work refusal.
The full check must be rerun; no success is claimed for this attempt.

`native-package-canonical-fullcheck` also stopped, this time in two desktop
accounts tests: its long disk-backed TMPDIR exceeded Linux Unix-socket path
length. Formatting/clippy passed, but the complete check did not. The temporary
base was shortened on the same ext4 storage device; no production socket path or
desktop behavior was changed. The next full check includes the added read-only
stage diagnostic and must complete before committing this checkpoint.

`native-package-stage-fullcheck` completed the full `scripts/check.sh` with exit 0
from 18:07:46 to 18:17:21 UTC on 2026-10-01. Its private temporary storage is ext4,
with a worktree-specific target on the Storage disk. Formatting, all clippy
variants, workspace/feature tests, real kernel mounts, scripts, desktop integration
checks, ledger and docs passed. The existing broken rustdoc link to
`filesystem::writeback::Writeback::retry_stuck` remains a warning. Display-dependent
window scenarios are outside this command. This checkpoint includes stale
first-open recovery, following-owner Trash admission and read-only diagnosis; it
does not yet correct the confirmed directory-normalization conflict or close
full-iCloud release acceptance.
