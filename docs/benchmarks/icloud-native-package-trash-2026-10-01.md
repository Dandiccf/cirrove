# Native package conditional Trash: preparation and validation

Status: synthetic coordinator and service guards validated; first live bootstrap stopped before document upload. Native Trash live acceptance remains pending.
This is a metadata-revision experiment on a fresh sacrificial synthetic Pages
package. It does not establish native-content edit conflict safety, rename CAS,
atomic replacement, or preserved document history/sharing.

## Live question and bounds

Does Apple reject Trash of an exact owned package with its previous metadata
revision, then accept its current revision and preserve a semantically identical
recoverable package? A fresh preregistration must precede creation of the new
owned folder/source/import. Existing user documents and the successful public
import `ec7f82e1-3c33-4a1f-bf01-d6e3f2ca80cc` are excluded.

Prediction: rename exposes a different ETag; old-ETag Trash receives an explicit
precondition rejection, and current-ETag Trash leaves the exact document readable
in recovery. Only an explicit HTTP 412 currently qualifies as precondition
refusal. Generic 400/404/409 or item refusal is insufficient evidence and stops
the experiment. An unchanged ETag also stops it. No permanent deletion exists.

The source is a previously verified Cirrove-owned synthetic Pages archive, copied
only into a newly created owned fixture. Require exact account, source digest,
package semantics, scope, fresh operation, parent and allocated target identities.
The service must enforce this provenance before exposing the executor; the core
helper alone does not establish preregistration-before-import.

Each potentially-sent boundary is durably sealed before dispatch. Failed or lost
responses permit read-only inspection only. A consuming executor and retained
create-new marker prevent replay. Verification uses private anonymous disk files
with explicit mode 0600, bounded to 64 MiB, and no raw provider bodies or signed
URLs are logged. No build or other measurement may overlap a live arm.

## Synthetic evidence

The first compile exposed an incorrect fixture-certificate relative path; it was
corrected to reuse the existing package-create fixture. All 13 coordinator tests
then passed in `icloud-access-native-trash-coordinator2-2026-10-01`.
Tests exercise actual execute-path HTTPS requests, persisted Armed phases,
failed/paused vault writes, cancellation before dispatch, lost responses,
inspect-only recovery, exact identities, semantic downloads, recovered parents,
and distinct owned state-directory identities.

Three negative controls failed as required:

- `trash-parent-negative`: discarding the parent recovery marker allowed execution
  and failed the refusal assertion.
- `trash-refusal-negative`: accepting generic refusal as stale-precondition proof
  let the workflow succeed and failed the expected-error assertion.
- `trash-cancel-negative`: removing the post-persistence cancellation check sent
  an HTTP mutation and failed the zero-dispatch assertion.

All modified production guards were restored in a finally block. Evidence is in
`.local-state/icloud-access-<arm>-2026-10-01`; these are synthetic correctness
checks, not Apple compatibility or performance measurements. The restored test
run and later live result must be recorded separately.

After restoration, all 13 coordinator tests passed again in
`icloud-access-native-trash-restored-2026-10-01`. No Apple requests or cloud
mutations were made by these fixtures.

## Preregistered live arm

Fresh run: `9c17134a-ef0e-4a04-8227-bbd015193780`.
Retained archive source: `ac9e5456-bd10-4b7d-9215-21bbb85dde69` only.
The harness creates `Cirrove Package Validation <run>` at the root, a new
`Cirrove Package Source <run>.pages`, and a new `Cirrove Package Import <run>.pages`.
It independently downloads/verifies the fresh source and import before the Trash
coordinator can rename or trash the new import. The source stays active; the
successful sacrificial target stays recoverable. ec7 and existing documents are
excluded. Fixed run selection and create-new state refuse a second execution.

The private preregistration binds source hash/semantic identity and exact account
before folder creation. Every bootstrap stage has a durable create-new phase
receipt before dispatch; source/target import identities come from their journal
or sealed allocation receipts. Lost/uncertain outcomes stop. Restart uses the
separate inspect command, never reconstructs workers or resumes a mutation.
An independent code review found no concrete provenance/replay blocker; service
guard tests do not substitute for the core execute-path ordering tests.

Run only after focused tests and a fresh probe build. Record command/binary hash,
source state, process and btrfs TMPDIR/SQLITE_TMPDIR before execution; outer limit
30 minutes. No builds or other measurements overlap. The result must distinguish
explicit precondition failure from sanitized Conflict/Rejected outcomes. No live
result is claimed here yet.

## First live outcome: local bootstrap refusal

The fresh probe build passed, as did the 13 final core tests, three service guard
tests, and probe Clippy with warnings denied. The `native-trash-live` arm then
terminated with exit 1 after `02-folder-created.json`, before SourceArmed.
Apple root metadata reads took 30.865 and 29.592 seconds; these are individual
observations, not a performance result. The exact newly owned folder receipt is
retained in the run directory. No source/import document upload or Trash dispatch
was reached.

The local capture rejected the staging directory: its path came from
`CARGO_MANIFEST_DIR/../../.local-state`, whereas the secure descriptor walker
rejects ParentDir components. Mode was verified as 0700. This is a harness path
normalization failure, not evidence of Apple Trash behavior. Preserve the run
and folder; never replay this fixed mutation run. A regression and local
preflight must precede any separately preregistered fresh arm.

Evidence: `.local-state/icloud-access-native-trash-live-2026-10-01/` and
`.local-state/icloud-owned-package-create-9c17134a-ef0e-4a04-8227-bbd015193780/`.

## Local path correction

The harness now walks original directory components without following symlinks,
canonicalizes the already validated private directories and checks their device,
inode and owner identity. The immutable local archive capture occurs before
FolderArmed, so staging or archive admission failures precede cloud mutations.
The fixed run remains consumed; no live replay is authorized by this correction.

The new synthetic regression reproduces the repository `../../` shape with a
real private disk ZIP, requires old direct capture to refuse it, and exercises
corrected capture plus source/staging ancestor and source-file symlink refusal.
Positive and guard-removal runs will be recorded after completion.

The local regression passed in `native-trash-path-positive` (one expected test).
Returning the original unresolved path instead of the canonical path made that
same test fail with the exact staging error in `native-trash-path-negative`.
The finally block restored the implementation, verified by inspecting the source.
Formatting then passed. Full repository validation is in progress; no subsequent
live arm has been started and the consumed 9c run remains untouched.

## Full-check failure under investigation

`icloud-watch-trash-strata-fullcheck` exited 101 in the writable FUSE group:
43 cases passed, but
`real_existing_working_admission_rechecks_open_and_path_truncate_atomically`
accepted a stale pathname operation. A separate exact-test run
`admission-race-reproduce` failed as well, this time in the pathname-truncate arm.
This is not a timeout failure and cannot be treated as a passed full check.
No commit or new cloud experiment follows until the admission behavior is
resolved and the complete check succeeds. The local capture regression had
already passed, including its explicit negative control.

The full-iCloud acceptance ledger now has six explicit unchecked rows in its own
release scope. Historical OneDrive success cannot hide these blockers. All four
new ledger tests passed; removing scope filtering and source/ledger scope binding
separately caused the expected failures. Both guards were restored and the four
tests passed again. These reporting checks do not close any product acceptance
row. Negative outputs are `.local-state/ledger-scope-filter-negative.log` and
`.local-state/ledger-scope-binding-negative.log`.

The admission correction preserves the original lookup location before metadata
refresh. An initial journal check now refuses a stale local pathname, so Linux
ESTALE retries cannot legitimize it with a fresh admission snapshot. Final journal
revision checks remain. The unchanged original failing kernel test and the
unknown-package refusal case both passed in `admission-path-kernel` (two tests).
Broader descriptor/alias/kernel and full-check acceptance is still pending.
The added unit fixture separately exposed setup errors (private journal directory
and an incorrect parent-normalization assumption); neither failed run proves the
new guard and both remain in the evidence history.

The full writable kernel group then passed all 44 selected tests in 95.97 seconds
(`admission-path-kernel-suite`). The same full-device scenario excluded by
`scripts/check.sh` was explicitly skipped. This includes original admission race,
file links, retained descriptors, atomic replacement and recovery scenarios.
It is not yet a successful whole-repository check or installed validation.

## Second preregistered arm, not yet executed

Fresh run `02cd3ca4-b0de-4c43-9431-638e7553ddf2` uses the same verified owned
source archive ac9, prediction, strict HTTP 412 criterion and 30-minute outer
bound from the first preregistration. The fixed harness run ID is rotated to
this new value; previous 9c artifacts and folder remain untouched. This is a
new sacrificial folder/source/import, not replay of any previous mutation.
Local archive capture now precedes cloud creation.

Require the corrected admission/unit fixtures and complete repository check,
then a fresh probe build and an exclusive manifest with exact binary/source
hashes before execution. There is no new success claim or cloud dispatch yet.

The corrected unit fixture creates and acknowledges a parent via journal APIs,
so local/provider IDs genuinely differ. `admission-path-unit3` passed. Disabling
the pathname comparison made the same test fail with “already-stale lookup”
in `admission-path-unit-negative`; the finally block restored production code.
The second complete check is `icloud-watch-trash-strata-fullcheck2`.

The second full check stopped at Clippy because the new unit-test module lacked
the test-only `unwrap_used` allowance used by adjacent fixtures. Added that
module-scoped allowance; production lint policy is unchanged. Full check three
is required; the second run did not reach workspace/kernel/script completion.

The third full `scripts/check.sh` completed successfully at 2026-10-01T12:10:07.286747+00:00.
Formatting, Clippy, workspace/feature tests, real FUSE groups, scripts, release
ledger and docs all completed in that one command. Rustdoc reports a broken
internal link to `Writeback::retry_stuck`; it is a warning, not a clean-warning
claim. Window scenarios and installed validation are outside this command.
No native Trash live success is inferred from this result.
