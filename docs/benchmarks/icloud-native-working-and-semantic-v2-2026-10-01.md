# Native working generations and versioned package identity

Registered before the integration tests. This is implementation validation, not
live Apple reliability or permission to replay an existing conflicted operation.

## Questions and predictions

The owned replacement diagnostic established equal file paths, lengths and hashes
but three additional directory entries in Apple's returned archive. An explicit
version-2 identity should compare those representations equally while retaining
empty directories and refusing changed content, paths, kinds and extra roots.
Version-1 proofs must keep their exact original interpretation.

Native working bytes should survive restart independently of cloud confirmation.
Consecutive sealed generations must wait for the predecessor's typed replacement
receipt and bind its exact new identity, revision and original-content proof.
Acknowledging A must preserve queued B and dirty C. A failed acknowledgement
transaction must preserve source identity, recovery namespace, receipt state and
all accepted bytes together.

## Arms and endpoints

- Versioned ZIP identity tests and core structural validation: v1 unchanged;
  v2 canonical root/ancestor directories, explicit empty directories, exact
  file identity, 10,000 canonical-entry ceiling, cancellation and wrong-root
  refusal. Negative controls remove closure or canonical-budget enforcement.
- Synthetic HTTPS package create/verification: v2 survives directory expansion
  and a lost registration reply without a second mutation; changed bytes remain
  refused; retained v1 directory differences remain Conflict.
- Native working foundation/successor/recovery tests: hydrate, edit, seal,
  restart, A/B/C ordering, receipt rebinding, dirty/sealed byte export, malformed
  or missing binding, generation races, quota and transactional rollback.
  Negative controls remove generation or durable-binding rechecks.
- Existing migration/future-schema rejection and public replacement socket
  tests: schema18 rejects older writable readers, retains ordinary data, and
  public replies remain identity/status-only rather than exposing new proof
  formats. Fresh admission uses v2 explicitly; retained proofs select their
  stored version, including independently versioned original/new proofs.
- Full `scripts/check.sh` after the focused checks and restoration of all
  negative controls. Display-dependent and live installed acceptance remain
  separate.

One parent runs builds/tests serially with private disk-backed TMPDIR and
SQLITE_TMPDIR, per-arm manifests and the isolated worktree's Storage target.
No cloud mutation is part of these arms. Source is frozen during each run.

## Compatibility and remaining scope

Journal schema18 is one unreleased transition for versioned proofs and native
working bindings. Existing schema17 data migrates only through the current
writable implementation; compatible read-only recovery remains available.
Never lower user_version manually. This does not establish arbitrary old-binary
recovery support or authorize installed-daemon replacement.

The generic default semantic function remains v1. Fresh import/resolution selects
v2 explicitly; hydration and later seals preserve their recorded stream version.
Old conflicts, immutable archives and encrypted checkpoints are not rewritten.
Socket jobs/listings expose Nodes/IDs/revisions/flags, not semantic proofs, so
their existing wire version remains compatible.

The initial journal integration alone does not enable ordinary mounted package
saves. Derived archive projection, atomic replacement, real Pages/Numbers/Keynote
application acceptance, installed account transitions and Strata preservation
remain required for full iCloud support.

## Initial integration results and corrections

The first semantic suite passed 22 tests and failed one new fixture expectation:
the existing ZIP layout parser already refuses file ancestors in v1. The new test
incorrectly expected that invalid shape to pass v1. Corrected to retain refusal
under both versions, all 23 semantic tests passed. Removing directory closure
made the normalization regression fail; removing its canonical-entry limit made
the budget regression fail. Exact source was restored after each negative.
`semantic-v2-provider-restored` then passed all 331 feature-enabled provider tests,
including v2 normalization/lost-reply recovery, changed-content refusal and
retained-v1 directory conflict.

Native working tests initially exposed test-only compile errors (non-Clone typed
receipt, SQLite signed-integer reads and a deliberately non-Debug representation).
The fixtures were corrected without broadening production types. All 16 native
working/successor/recovery tests passed. Removing the shared generation comparison
or both durable-binding comparisons made their dedicated regressions fail;
both controls were restored. These are synthetic invariants, not live save proof.

## Derived archive projection: controls registered before execution

The first six projection tests were exploratory integration checks and all passed.
The following controls are registered now, before running them. Prediction: a
source acknowledgement without paired child refresh must fail exact artifact
identity/publication assertions; removing the source-owner Head exclusion must
allow an unrelated explicit replacement that its regression rejects. Restore
both guards before the complete service library regression suite.

Hydration establishes source owner, working bytes, Head and derived child in one
transaction. Only the source owns cloud operations; the child must remain a
non-owning file with an exact typed role. Dirty bytes and paired identity changes
publish together, stale batches cannot roll back the pair, and malformed role,
scope or alias cannot gain authority. Ordinary writable admission is still closed
through the existing admission check before the existing-working shortcut.

The projection folds into this same unreleased schema18; no live schema18 daemon
or installed upgrade has run. It does not migrate intermediate synthetic scratch
schema18 native streams. Clean native working bytes remain quota-accounted;
retirement, mounted save admission and atomic-save acceptance are still pending.

Both projection negative controls failed at the intended assertions: omitted
child refresh lost the paired child publication; omitted Head exclusion allowed
an unrelated explicit replacement to acquire the native source. Both source
files were restored before the full check. No faulted binary ran against a live
account.

Review of the next mounted-save path identified another prerequisite: current
native sealing calls generic enqueue, which makes a second archive copy under
the journal mutex. The existing private capture is outside the lock, but this
second copy must be replaced by prepared immutable-object publication before
enabling mounted native saves. The staged prerequisite is not presented as a
completed latency or lock-contention fix.

## Whole-library correction and Stage recovery validation

The first full check stopped on clippy replace_box; the unnecessary allocation
was removed. The next full check reached the service library with 382 passing,
four failing and 25 ignored tests. Two worker fixtures expected a fresh v1 proof
although new admission now captures v2; unknown-version rejection still used 2
rather than 3; and a competing-identity fixture collided on the newly reserved
source name before reaching its intended acknowledgement identity collision.
The fixtures now preserve their intended checks. The corrected whole service
library run passed 386 tests, zero failed, 25 ignored. This is not a completed
full scripts/check.sh run.

Next registered question: can an explicit conflicted Stage upload release its
local reservation while preserving the original, retained payload and checkpoint,
and refusing stale evidence or dependent work? Apply the prepared Stage-only
foundation, run provider and journal native_stage_abandonment tests, then remove
the final snapshot equality as a negative control and restore it. Prediction:
the stale-state regression fails without that comparison. No public command or
live abandonment is enabled by this foundation. The existing real conflict is
unchanged; these tests use synthetic accounts and provider responses.

Stage foundation initially failed compilation in one new test because SQLite
counts were read as u64, which rusqlite does not support. Changed that fixture
to i64. The corrected three journal recovery tests passed. Removing the final
captured/current equality made the stale-row arm fail at its intended refusal
assertion; restored the exact source afterward. Independent foundation review
found no concrete blocker, but public manager-owned authority wiring remains
required. No live operation has been abandoned.

The feature-enabled synthetic provider Stage evidence test passed, including
current sealed checkpoint replacement and exact account/operation refusal.

## Prepared publication and bounded native sealing

Register before execution: test exact captured-inode adoption without a second
copy, cancellation/race/orphan retention, typed native seal dispatch and bounded
concurrent seals. Prediction: prepared publication retains the same inode even
at its reservation limit; duplicate same-file seals serialize and a dropped
caller cancels its worker without releasing its slot early. Negative controls
will remove the relevant adoption/permit/cancellation boundary, one at a time,
and must fail the corresponding assertion. These are synthetic resource and
recovery tests, not measured real-application latency or mounted-save acceptance.
Independent review previously rejected an unconstrained blocking dispatcher;
the applied revision owns two per-Writeback slots and a per-working gate through
the blocking worker's actual completion, with caller-drop cancellation.

All three prepared-publication tests and four bounded-seal tests passed. Review
found no remaining implementation blocker and requested a third distinct file
in the limit test; added it and reran all four successfully. The cancellation
negative failed with "abandoned seal did not cancel blocking validation". The
old-copy negative failed with Quota at the full reservation limit. Raising the
slot count to three failed with "third distinct file exceeded the two-worker
limit". Restored the exact intended source after every negative; no faulted
binary was run with a real provider. The full scripts/check.sh run follows on
restored sources before any commit. Mounted native path admission remains closed.

The first combined full check stopped at clippy possible_missing_else in the
new synthetic provider fixture: adjacent independent if statements inside the
HTTP task macro shared one line. Separated them without changing behavior;
restarting the entire check on corrected source.

The corrected combined check passed clippy and reached workspace integration
tests, then stopped at namespace_publication's schema-eleven migration assertion:
it still expected the resulting current journal version to be17, while migration
correctly produced18. Searched all service source/tests for multiline schema
assertions and found the same stale current-version expectation in
namespace_replacement and upload_journal. Updated those three expectations to18;
actual historical schema17 fixtures and future-version refusal arms remain intact.
The preservation and ordering assertions are unchanged. Restarting full check.

## Completed prerequisite check

`native-working-schema18-fullcheck` ran full `scripts/check.sh` from
2026-10-01T18:58:13.564711+00:00 through 2026-10-01T19:06:35.049289+00:00 and exited0. Formatting, clippy,
workspace and feature-enabled iCloud tests, actual synthetic kernel mounts,
script/Strata/Dolphin checks, ledger and documentation completed. The existing
rustdoc retry_stuck link warning remains; display-dependent window scenarios
are not part of this command. No live schema18 account, public abandonment or
ordinary native mounted write was enabled or validated by this result. The
regular installation is unchanged.
