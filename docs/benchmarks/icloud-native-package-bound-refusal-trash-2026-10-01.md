# Preregistered native PACKAGE bound-revision Trash arm

Status: live arm and independent read-only inspection passed; see result below. Run UUID `97be33d2-b216-49bf-9e49-465b1ca85d1d`.
This is a new experiment. Historical strict-HTTP412 runs, including
`e6ec6113-dcce-42da-9836-4afce35e0d28`, remain failed/pending exactly as recorded.
Their markers and cloud items must not be resumed, renamed or trashed by this arm.

## Question, prediction and endpoint

Does a fresh, owned synthetic Pages package refuse a stale metadata revision
through HTTP412 or an exactly bound per-item ETAG_CONFLICT, remain independently
unchanged, then enter recoverable Trash with its current revision and preserve
identical native content?

Prediction: the stale request returns HTTP200 with an ETAG_CONFLICT item bound to
the owned drivewsid, parentId and independently observed new E1. Active metadata
and package semantics remain E1/unchanged. The one current-E1 request then succeeds,
and exact Trash metadata plus package semantic verification proves recovery.
A generic refusal is not success. Missing/wrong receipt binding, changed active
revision/content, uncertain responses, failed persistence or cancellation stop
without replay. One arm is protocol evidence, not a reliability estimate.

Successful endpoint requires a durable Recovered checkpoint, report containing
exact sanitized refusal kind and numeric HTTP status, both independent-proof
booleans true, exact target identity and semantic recovery. No permanent deletion,
restore/replacement, content-edit CAS, sharing/history preservation or general
native-write reliability is claimed.

## Public protocol evidence and stricter binding

Public asset: https://www.icloud.com/applications/iclouddrive/2636Build19/en-us/main.js
SHA256 `28a365b0629aba447bf3d6048bbe45e8c69fc6f1803283dc51f9f0b5484a813d`, 589227 bytes.
The current anonymous application HTML still references this version.
Zero-based UTF-8 byte offsets: module alias at214448; moveItemsToTrash invocation
at216730; ETAG_CONFLICT handling at577560. Module2262 partitions per-item statuses,
associates response drivewsid with the request and compares parentId before
retrying using the returned etag. Cirrove deliberately never copies that retry.
Our typed response requires exactly one matching ID/parent and returned revision
equal to independently observed E1, nonempty/bounded and different from E0.
Only this bound status or HTTP412 qualifies for this new arm. Generic CONFLICT,
400/404/409, unknown status and wrong/missing fields never qualify.

## Fresh ownership and durable sequence

The fixed service harness preregisters before mutation. It uses only the retained
Cirrove-owned synthetic Pages archive from run
`ac9e5456-bd10-4b7d-9215-21bbb85dde69`, validates immutable local capture before cloud
mutation, and creates a new folder/source/import with this new UUID. The public
import ec7 and all older Trash-arm fixtures are excluded. Names, scope, account,
allocated IDs, source raw digest and root-relative semantics remain bound.
Purpose is `owned-native-package-bound-refusal-trash-v2`.

1. Fresh preregistration and existing durable bootstrap markers precede requests.
2. Independently verify the allocated import and rename it using E0; observe E1.
3. Persist StaleTrashArmed before the single E0 request.
4. Persist typed refusal kind/status in schema2 checkpoint while still
   StaleTrashArmed. This alone does not claim independent proof.
5. Independently verify active exact ID/parent/name/E1 and native semantic content.
6. Persist StaleRefused; persist CurrentTrashArmed before the single E1 request.
7. Verify exact recoverable Trash metadata and package semantic identity; persist
   Recovered. Inspection returns recorded refusal evidence without any mutation.

Schema1 checkpoints remain readable as historical evidence, without inventing a
new refusal kind. No execution resumes them. Failed/new runs retain all markers.

## Synthetic validation required before running

Prepared tests must be executed with isolated on-disk TMPDIR/SQLITE_TMPDIR:
- Existing HTTP412 end-to-end success, persistence failure and cancellation tests.
- Bound200 success including persisted checkpoint and fresh read-only inspection.
- Missing/wrong ID, parent, E1, old E0, duplicate items and generic CONFLICT stop.
- Failure saving refusal evidence prevents current Trash.
- Independent active E1 or native-content change prevents current Trash.
- Schema2 requires valid evidence in proved phases; schema1 does not invent it.

Negative controls: omit exact receipt ID check (wrong-ID arm fails); ignore the
evidence save error (failed-save arm fails); skip the independent active check
(changed revision/content arms fail). Tests prepared here are not yet run.

## Execution discipline

Command after successful validation and parent authorization:
`cirrove-icloud-mounted-write-probe --owned-package-trash 97be33d2-b216-49bf-9e49-465b1ca85d1d`

Before running, record actual binary SHA256, exact command, UTC start, private
non-tmpfs TMPDIR/SQLITE_TMPDIR and expected900s upper bound in its run manifest.
No concurrent build/measurement. Only a fresh run is permitted. A failed arm gets
read-only inspection; it is never replayed. No cleanup or personal-cloud mutation.

## Local checks before live execution

Independent review found no state-ordering or receipt-binding blocker.
`trash-bound-refusal-tests` passed all 22 coordinator/vault tests. The evidence
save failure fixture was narrowed to the actual StaleTrashArmed evidence-save
boundary, so a later save cannot mask omission of this required earlier save.
Omitting that save failed `trash-evidence-save-negative`; omitting the independent
active proof failed `trash-active-proof-negative`. Production source was restored.
`identity-bound-refusal-restored` then passed all 261 adapter tests. Earlier exact
response-ID guard removal also failed its foreign-ID assertion. These remain
synthetic evidence; no new live result is claimed here.

## Live result

The fresh 97be arm completed with exit 0 at 2026-10-01T13:07:33.042202+00:00.
`08-complete.json` and the durable report exist. The actual refusal evidence is
`ItemEtagConflict` with HTTP 200, bound to the exact owned item, parent and
independently observed E1. After unchanged active E1/semantic verification, the
single current-revision Trash completed. Final phase is `Recovered`, location
`RecoverableTrash`, with stale-refusal and current-Trash semantic-recovery proof
flags both true.

A separate `native-trash-bound-inspect` process then read the sealed checkpoint
and provider state, downloaded only the exact owned Trash package for semantic
verification, and produced the identical report without replaying any mutation.
The live and inspection manifests, raw/semantic artifacts and checkpoints remain
retained in their private run directories. No other fixtures were removed.

This is one successful native Pages metadata-revision/Trash run. It does not
prove content-edit CAS, restoration to an original parent, native replacement,
Numbers/Keynote behavior, installed application acceptance or long-term
reliability. All historical failed arms retain their original outcomes.

The full `scripts/check.sh` arm `readiness-identity-trash-fullcheck` passed at
2026-10-01T13:17:28.322366+00:00, including workspace/feature tests, actual FUSE groups,
scripts, ledger and docs. The existing rustdoc internal-link warning remains.
Subsequent authentication lifecycle fixes require another complete check before
commit; this result must not be used as evidence for later source changes.
