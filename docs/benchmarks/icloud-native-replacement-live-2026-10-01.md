# Native iCloud replacement live validation — 2026-10-01

Run: `42a313de-1e94-488f-aed8-e7c704944fed`. Status: preparing an owned edited Pages source; no Cirrove
replacement submitted yet.

## Registered question and prediction

Can the public explicit replacement command replace an owned Pages PACKAGE with
a separately valid, edited Pages archive while retaining exact original bytes
and a historical Trash receipt, then expose the new document at the same mount
path and open its changed contents in Apple Pages? Prediction: one new identity
and one original recovery identity, independent semantic proofs, no replay after
rejoining the saved operation. This does not prove normal application saves or
sharing/history preservation.

## Preparation scope

Use Apple Pages Duplicate on the owned document
`Cirrove Public Import ec7f82e1-3c33-4a1f-bf01-d6e3f2ca80cc`, then rename only the
new copy to `Cirrove Replacement Source 42a313de-1e94-488f-aed8-e7c704944fed` and edit synthetic text there.
Keep the original import unchanged. Export the owned copy as native Pages. No
other user files, sharing settings or permanent deletion are in scope. Record
actual identity, representation and archive proof before any Cirrove replacement;
if export is DATA instead of a valid PACKAGE source, report it rather than
relabeling the representation. Live command/owner manifest must precede submission.

Pending: duplicate/edit/export, actual source capture, public workflow test,
owned live replacement, independent old/new content and Apple app verification.

## Owned source preparation observed

Apple Pages duplicated the owned import and saved the uniquely named version-B
copy. Its UI document ID is `3EA9FB30-0110-4798-9DF7-C662A3E9833D`. The screenshot
shows the new run marker and original synthetic text. Native Pages export yielded
a flat ZIP: 109812 bytes, 12 entries, 108248 expanded bytes; SHA-256
`2f18fc8fba5c2161150d5cae924343268c40f3d67a5abca3944877b17dc8a13b`. It is retained unchanged alongside a ZIP with the
explicit `Replacement.pages/` root added to each entry, without changing entry
bytes. This structural conversion is not evidence of remote DATA/PACKAGE type.
Artifacts and preparation manifest: `/var/tmp/cirrove-native-replacement-live-42a313de-1e94-488f-aed8-e7c704944fed`.
No Cirrove replacement has been submitted. Next inspect exact remote metadata
and semantic binding, then use only the public replacement route once ready.

## Read-only harness validation

The preflight/postflight harness has no submission or mutation method. Four
focused synthetic tests passed after fixing two compile errors (qualified bail
macro and provider-error conversion). Removing only the exact prepared
representation comparison made `native-replacement-live-negative-receipt` fail
on fault 8 (changed original semantic proof); source was restored in `finally`.
`native-replacement-live-harness-restored` then passed all four tests. The
preflight will independently inspect only the owned parent/original and retain
an immutable validated replacement snapshot before any public submission.

## First live public arm: stopped before submission

`native-replacement-live-preflight` passed with the renewed sealed session:
exact original PACKAGE content matched the retained semantic proof, and the
changed source snapshot was captured and validated. The public mount runner
then resolved the exact original ID, but enumerating the native package folder
returned Linux ESTALE. It stopped before invoking replace-native-package;
`public-run.json` records submitted=false. The isolated daemon exited normally.
No replacement operation or cloud mutation was submitted by this arm. The
regular installed daemon and mounts remained running. Investigation is pending;
this is a failed mounted acceptance observation, not a successful replacement.

## Stale first-open diagnosis and registered correction

Read-only inspection found the exact original in cached observed/directory
metadata with an older ETag than the successful live preflight. The isolated
journal has no namespace owner. Generated package enumeration correctly refuses
that stale source revision, but the engine did not refresh the selected source
and retry. No content mismatch was observed in the independently verified original.

Prediction: one exact-parent metadata refresh and one fresh enumeration, within
the original deadline, will allow this first open without waiting for background
refresh. A second concurrent revision change must still refuse, preserving the
completed old children and old artifact bytes. Partial staged pages/cursors must
be discarded before retry; identity/location/type changes must refuse.

The initial `package-revision-retry-red` fixture run failed all five cases without
the engine fix. Review caught that its first proposed capability flag was not
implemented by ICloudDrive, so that draft would not fix the real adapter. The
correction needs a distinct opt-in for revision-failure retry; it must not enable
the unrelated first-open cached-package recheck policy wholesale. Focused red/green
and a rebuilt live mount check remain pending.

The corrected capability red reproduced VersionChanged on the first open.
`package-revision-retry-fixed` passed all five tests. The actual iCloud adapter
contract initially refused the synthetic non-UUID account; corrected to a UUID,
`package-revision-adapter-contract-fixed` passed its one intended test. The
fixture was then aligned with iCloud's first-open-cache policy (false), and
`package-revision-realpolicy-fixed` passed all five tests. No broad first-open
refresh flag was enabled. Already cached generated children after newly observed
source metadata are a separate cache-freshness gap under investigation.

## Second public arm: first open repaired, replacement conflicted

The rebuilt isolated mount enumerated the exact original package successfully
(one visible document, one generated child). This resolves the preceding live
ESTALE observation for this fixture. The runner then submitted exactly once.
Saved operation: `82767008-4553-4f8b-99cc-8a521ad9c5ac`; public job:
`a715eb96-9c6b-4b00-b9c1-8752b5b9fad2`. CLI exited 1 and retained listing reports
Conflict, no typed handoff completion. Local row records all 66268 archive bytes
transferred and a sealed checkpoint plus handoff reservation, but those facts
do not establish registration, original removal or successful replacement.

The isolated daemon stopped normally. Evidence is retained in
`public-run-attempt2.json`, `replacement-jobs-attempt2.json`,
`retained-list-attempt2.json` under the registered private run directory.
No resubmission or automatic retry is authorized by this failure. Next inspect
the exact saved checkpoint and owned remote identities read-only; do not treat
the original being absent/present in a local cache as current cloud proof.

## Read-only inspection of the retained conflict

`native-replacement-conflict-diagnose-canonical` exited 0. The checkpoint remains
`stage-registration-armed`, before a verified handoff plan. Exact metadata reads
found both original and staged identities in the owned active parent. The original
still matches its captured revision, name, logical size and document identity;
neither item has a restore marker. Stage content has not yet been verified.
Journal and encrypted checkpoint hashes remained identical across this diagnostic.
Private summaries are retained under the registered run's
`verify-99fc916d-f56a-40e6-8c68-9335ec063d3f/` directory.

The initial diagnostic refused locally because a historical import content-version
alias remained in the preflight artifact, whereas native replacement admission
canonicalizes that alias to None. A narrowly bound in-memory correction accepts
only the exact retained historical ETag alias; unrelated aliases, identities and
selected revisions remain refused. The original artifact is unchanged. Its
negative control failed without canonicalization; all six focused live-harness
tests passed after restoration. This diagnostic correction does not explain or
change the saved replacement conflict.

The edited source ZIP has 12 file entries and no explicit directory entries.
An earlier original Apple download contained explicit directories. A possible
explanation is Apple's directory normalization versus strict semantic identity v1,
which includes explicit directory entries. This is a hypothesis, not a confirmed
cause or permission to relax verification. The next read-only diagnostic will
compare exact staged relative file paths, sizes and hashes with the retained
source and report directory differences without exposing document content. It
must execute no upload, registration, rename, removal or saved-operation replay.

## Stage content diagnosis: explicit directory entries

`stage-diagnostic-live` exited 0. The exact stage download passed the adapter's
before/after identity, parent, revision and logical-size checks. Verification then
failed at `archive-semantic-equality`. Independent bounded comparison found:
12 source files, 12 downloaded files, exact relative file paths/sizes/hashes equal,
108248 expanded bytes in each. The source has zero explicit directories; Apple's
download has three (15 total entries versus 12). The difference is exclusively
directory entries. No document content or entry names were printed.

Evidence: `verify-21a36c7a-c3a6-46c0-9dea-839b08a8df97/stage-verification-summary.json`
under the registered private run. Original metadata again matched the captured
revision in the owned active parent. Journal, checkpoint and WAL-presence/hash
comparison remained unchanged (`stage-diagnostic-unchanged.json`). This describes
the fenced download, not continued name uniqueness after parsing or a completed
handoff. The retained operation remains Conflict.

The diagnostic comparison test passed, failed as intended when directory-only
difference was forced false, and passed after exact source restoration. All three
synthetic HTTPS package-create roundtrip arms passed, including changed-content
refusal and unchanged allocation/upload/registration counts during diagnosis.

Next correction: explicit version-2 canonical directory closure, preserving
empty directories and rejecting path/kind collisions. Version 1 receipts and
checkpoints retain their strict interpretation. New version admission requires
a durable-state compatibility fence; this diagnosis does not authorize replay
or rewrite of the existing conflict.
