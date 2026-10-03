# Native atomic journal and completed working-copy retirement

Register before execution. Question: can native temporary streams transfer canonical
ownership atomically while immutable operation lineage survives A/B/C acknowledgements,
and can completed clean working bytes retire only after fresh provider metadata and
reader/seal exclusions, then rehydrate safely into the same dormant slot?

Run serialized focused filters: native_atomic_, native_retirement_,
native_reactivation_, native_maintenance_, native_projection_. Predictions:
- exact generation/Head/owner CAS rejects stale captures and uncertain predecessors;
- temporary and detached late-written bytes export through read-only recovery;
- fresh provider observations occur outside locks; held users prevent reclamation;
- temporary-owner guard blocks retirement until ownership is settled;
- paired publication refuses half transitions and stale reactivation.

Meaningful controls remove lineage edge validation, transaction recheck, fresh
observation, held-reader exclusions, and temporary-owner exclusion individually.
Each must make its named synthetic test fail before restored acceptance. No cloud
mutation or installed daemon is used. Full scripts/check.sh before commit.

Combined patch includes atomic journal prerequisite, retirement/reactivation,
maintenance and temporary-owner guard. Mounted temporary-create/rename remains
closed until separately integrated wiring and actual-kernel proof. Same-ID changed
revision admission is under review; direct journal proof alone is insufficient.
No real-account schema18 deployment or native application compatibility claimed.

Initial integrated build found a rusqlite u64 decoding error for namespace_clock.
Decode its SQLite signed integer as i64, then checked-convert to u64; a negative
clock is corruption, not a wrapped publication cursor.

All ten atomic journal tests passed. Two retirement tests incorrectly expected
a second collector call to reclaim one intent after writable reopen; open already
calls collect_retired_working. Corrected fixtures assert absent bytes and zero
retained intents after reopen, then idempotent explicit collection returns zero.
Production collection behavior was unchanged.

Focused positive results: atomic10, retirement5, reactivation3, maintenance4,
projection7 tests passed. The new dormant pathname test failed before its fix
with Stale at changed-revision admission. Applied the narrow dormant identity
selection fix; fresh independent semantic proof and final CAS remain required.

Dormant pathname admission test passed after the exact-identity fix. Integrated
direct native temp-to-canonical mounted wiring. Register projection pair test
then actual-kernel direct temp fsync/rename and pending A/B/C restart tests.
Prediction: temp fsync and detached old-descriptor fsync are local-only; complete
validated archives enqueue exactly one typed replacement, stale/incomplete roles
refuse, old handles retain their own bytes across canonical identity transfer.
Receipts are synthetic, not Apple transport evidence. Backup-first saves and
temporary local lifecycle are not covered by this direct rename-over slice.

Mounted integration build corrections: supplied explicit local-role arguments to
ordinary projection apply callers, filled dormant snapshot roles, and removed
duplicate fixture fields from rebase. Paired atomic projection test passed.
First integration-test build called a crate-private receipt helper; the fixture
now asserts public package_completion and remote are both absent before any
worker submission. No production receipt API was widened for the test.

Both actual-kernel positive tests passed: native-atomic-kernel-first-corrected
(temporary create/write/fsync/direct rename-over, old descriptor isolation) and
native-atomic-kernel-chain (three pending saves, real unmount/journal reopen/remount,
zero provider submissions). Synthetic receipts only; these do not prove Apple
transport or general editor compatibility. Negative controls and full check remain.

Mounted negative controls passed: omitting complete-pair validation accepted an
incomplete publication, and disabling local-only native stream fsync caused EIO
on the temporary stream. Restored paired projection plus all seven native
actual-kernel tests passed. Retirement temporary-owner guard and pre-observation
reader exclusion controls each failed their named assertion when disabled.
Restored six retirement and four maintenance tests passed. No faulted binary
was run against a real account. Remaining fine-grained controls and editor
variants are not implied by this evidence; full repository validation follows.

Full scripts/check.sh passed in native-atomic-retirement-fullcheck-corrected: formatting, clippy, workspace/features, kernel mounts, scripts, Strata/Dolphin and ledger/docs. First run stopped on collapsible_if; condition simplified without semantic change. Existing rustdoc retry_stuck link warning remains; display window scenarios not included. No installation or cloud mutation.
