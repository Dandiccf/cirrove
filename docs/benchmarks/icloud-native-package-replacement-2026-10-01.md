# Native iCloud package replacement foundation — 2026-10-01

## Scope

This foundation binds an explicit validated replacement archive, the selected
original revision, and both semantic content identities. It reuses the existing
upload worker, identity-handoff reservation and metadata publication outbox.
Normal service/FUSE package replacement remains disabled. No real cloud package
replacement has been attempted by this change.

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

The durable package staging-to-handoff coordinator is now integrated but not
routed through the normal service.
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
