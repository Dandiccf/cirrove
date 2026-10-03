# Exact owned Pages restoration: preregistration

Question: does one conditional putBackItemsFromTrash restore the successfully
trashed native Pages document from run 97be33d2-b216-49bf-9e49-465b1ca85d1d,
preserving its Drive/document IDs and native content?

## Arm and prediction

After review, synthetic tests and serialized build, run the feature-only
`--owned-package-restore 97be33d2-b216-49bf-9e49-465b1ca85d1d` once.
The exact retained source/account/import/Trash completion and sealed checkpoint
are prerequisites. Fresh read-only preflight must reproduce the bounded exact
relative restore path, unchanged native semantics and source-only owned parent.
A separate purpose-sealed restore checkpoint and exclusive permanent marker
precede the one request. RestoreArmed must persist before dispatch. There is no
loaded-checkpoint execute API, retry, rename follow-up or permanent deletion.

Prediction: one conditional restore returns the same document to its original
owned parent; Apple may select a different display name. A receipt is accepted
only for the exact item/document/parent and a valid revision. Missing receipt
fields or a lost reply leave the operation uncertain, even if Apple acted.

The endpoint requires independent target AND source native semantic comparison,
stable resulting metadata, exact two-item destination inventory and durable
Verified checkpoint. A separate process then invokes only
`--owned-package-restore-inspect` with the same run. It never resumes mutation.
Read-only inspection may verify an active document after a lost reply but must
report that no receipt was recorded. A refusal or mismatch is retained, not
replayed or deleted.

This bounded experiment does not prove atomic destination vacancy/no-overwrite
semantics under competing clients. The fixture lives in its own UUID folder;
no user-authored document is a target. Installed services remain untouched.
Launcher manifests retain binary hash, private disk-backed TMPDIR/SQLITE_TMPDIR,
command and duration. No concurrent build or other measurement during live arms.

## Local validation before live execution

Seven `owned_native_restore` tests passed. They exercise execute/inspect with
synthetic HTTP and a prepared owner, not live Secret Service preparation. The
separate encrypted-vault test proves import/Trash/restore same-operation isolation
and AAD transplant refusal. Removing durable RestoreArmed persistence caused the
failed-arm test to fail (`owned-restore-arm-negative`); restored implementation
passed the vault test. Selected service-probe Clippy passed with warnings denied.
The earlier `owned_restore` filter matched only the existing shape test; it is
not counted as executor coverage.

## Initial live outcome: not yet verified

Independent review found no concrete blocker for the exact owned experiment.
The feature binary built successfully. The single `owned-native-restore-live`
arm ended at 2026-10-01T13:48:28.166621Z with exit 1 and static error
`owned restore destination inventory changed`. Execution had passed the strict
restore receipt and active-item checks before reaching this postflight guard;
full destination inventory and native semantic acceptance did not complete.

A fresh process ran only `owned-native-restore-inspect`; it ended at
2026-10-01T13:48:47.261060Z with the same inventory error. No second restore was
sent. Both run manifests/logs remain under their corresponding
`.local-state/icloud-access-<arm>-2026-10-01/` paths; sealed restore history remains
untouched. The next step is a bounded read-only field-comparison diagnostic,
not a replay, rollback, relaxed acceptance, or claim of successful recovery.

## Read-only inventory diagnostic preregistration

A diagnostic-only build keeps the original inventory acceptance unchanged and
adds bounded counts and per-field equality booleans for the exact source/target
identities. No raw values, names, IDs or provider bodies are emitted. The new
synthetic diagnostic/privacy test passed.

Run only the existing restore-inspect command once with this build. Prediction:
the report will distinguish differing list/detail metadata from an altered
source or an unexpected inventory; no cause is assumed before observing it.
The prior restore checkpoint is not rewritten, and no mutation route is invoked.

## Diagnostic result

The read-only diagnostic ended at 2026-10-01T13:52:16.775099+00:00 with exit 1 under
the unchanged guard. Exactly two entries were present. The source matched every
field exactly. The target matched Drive ID, document ID, ETag, size, name,
extension/display name, parent, kind, zone, nested items and item count; only
`item_id` differed between exact-item metadata and the parent listing.

This identifies a list/detail comparison mismatch, not missing content. The
planned fix must retain exact stable identity, revision and semantic verification
while separating the differing transport field. It must first have a failing
synthetic reproduction. No restore replay is justified; semantic acceptance is
still incomplete until the corrected read-only verifier passes.

## Narrow comparison correction and read-only follow-up plan

The actual HTTP regression failed before the change (`restore-item-id-reproduce`)
at the equivalent-target expectation. After excluding only target item_id from
list/detail comparison, all eight owned_native_restore tests passed, including
twelve changed-source/other-target-field refusal cases. Source equality remains
full; exact target drive/document identity, parent, ETag and semantic content
remain mandatory. Public Drive Build2636Build19 main.js (SHA256
28a365b0629aba447bf3d6048bbe45e8c69fc6f1803283dc51f9f0b5484a813d) at byte18469
caches optional item_id by drivewsid and fills absent responses. Existing package
download fencing already uses stable IDs/parent/ETag/size independently of it.

The next arm invokes only restore-inspect with the corrected binary. Prediction:
metadata comparison advances to full native target/source semantic verification.
Any remaining mismatch stops inspection; no restore or other mutation follows.
The original unsuccessful live report and sealed ReceiptObserved history remain
unchanged even if this independent observation establishes restoration.

## Independent restoration verification

The corrected read-only inspection passed at 2026-10-01T13:56:52.042775+00:00 (exit 0).
Its report retains phase ReceiptObserved, receipt-recorded true, same restored
identity/native semantics true, untouched owned source semantics true, and
restored-name-preserved true.
Destination vacancy atomicity remains explicitly unproven.

Report: `.local-state/icloud-owned-package-create-97be33d2-b216-49bf-9e49-465b1ca85d1d/verify-44457727-060c-440e-8324-e8d43038ac59/restore-inspection.json`. No second restore or checkpoint rewrite occurred. This proves
restoration of this one owned Pages document with its original identities and
verified target/source native content, including inspect-only completion after
the original postflight refusal. It does not prove generic collision safety,
native editing/replacement or installed restoration support.
