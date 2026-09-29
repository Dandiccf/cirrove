# Mounted iCloud replacement in an owned child folder — 2026-09-29

Registered before the live run. This experiment creates only a new UUID-named
Cirrove validation folder, its `Mounted Folder` child and two test files in the
isolated `iCloudGuiValidation` account. The normal account and installed daemon
remain untouched; the normal iCloud connection stays read-only.

Question: can the shared write journal replace a file whose confirmed parent
is one level below the owned fixture root, preserve the old ID in Trash and
recover the new ID and bytes in a fresh process? This tests the version-3
handoff parent binding and the normal folder-aware staging adapter, using
only the feature-gated isolated mount.

Arms, sequentially: create the root fixture and its child folder; create
`Nested Replacement.txt` inside that child; overwrite that file through the
mount; remount in a fresh process and read the result. Each arm must have an
applied journal receipt and an independent Apple session must list the exact
folder and file IDs and verify complete bytes. The overwrite must install a
new ID at the same parent/name and find the former exact ID in recoverable
Trash. Prediction: all four arms exit 0. A failed or uncertain arm stops the
sequence; no blind mutating retry is allowed.

Endpoints: arm exit status, exact journal IDs and parent/name/revision binding,
independent listing, full-byte digest and recoverable Trash ID. One fixture
does not prove arbitrary-folder or real-user-data safety, concurrent-edit
handling, indefinite session reliability or production readiness.

Before each exclusive arm, a private manifest records command, binary SHA-256,
PID, expected duration and disk-backed `TMPDIR`/`SQLITE_TMPDIR`. No compilation
or other measurement overlaps an arm. No token, cursor URL, signed URL or raw
provider body is logged.

## Observation

The first two arms passed for owned run
`4c412f83-6590-41a0-b704-509ca7f3bf45`: the root fixture and nested source
were independently listed and their bytes matched journal receipts. The
initial nested replacement timed out after its 900-second check window. Its
journal retained the replacement as `verify_required`, with 13 failed
attempts, no provider checkpoint, no identity-handoff reservation and no
transferred bytes. The fixture remains available; the normal mount was not
changed.

Read-only journal diagnosis found the exact cause before a cloud handoff: the
replacement owner had a stable local parent ID distinct from the confirmed
iCloud child-folder ID. The reservation required raw string equality even
though all other old-item and revision checks matched. A focused test of that
case failed before the correction and passed after changing the guard to
validate the confirmed local-to-remote folder chain.

Correction to the plan: run one additional exclusive `--finish-nested-replace`
arm against the same durable journal, without a second application write. The
shared worker may now resume the one pending replacement. Prediction: it
reserves the old exact ID, completes the two-ID handoff and verifies the new
bytes and recoverable Trash ID. A later fresh-process read remains required.
The failed arm is not counted as a pass.

The first corrective arm was interrupted by its recorded PID after repeated
`verify_required` observations. It had successfully reserved the old identity,
but still had no upload checkpoint or transferred bytes. Code inspection found
a second restart boundary: a verifying replacement with no checkpoint returned
`CheckpointInvalid` without observing the cloud. The adapter now performs a
read-only exact-folder, exact-old-revision and full-hash observation, requires
both staged/recovery names to be absent, and reports `Uncommitted` only for
that state. A further exclusive `--finish-nested-replace` arm will exercise
that recovery path without another application write. Its prediction is the
same two-ID completion; any different cloud state must stop as conflict or
uncertain. The interrupted arm also remains a failure in this record.

That second corrective arm completed the nested two-ID replacement and an
independent iCloud listing, exact Trash-ID check and full-byte digest all
agreed with the journal. The first fresh-process read failed before mounting:
the isolated fixture restoration still assumed every replacement receipt had
the root as parent. The confirmed replacement is in the owned child folder.
That local restore guard now validates the child folder's journal receipt and
uses its exact provider parent ID. A fresh-process, read-only remount arm is
still required; the failed read is not counted as a pass.

The final fresh-process read exited 0. It opened the replacement through the
isolated FUSE mount and independently verified the confirmed parent and new
file ID, recoverable old Trash ID, complete replacement bytes and uploaded
journal receipt. Private manifests and btrfs temporary directories for every
arm, including the failed/interrupted arms, are under
`.local-state/icloud-nested-replacement-20260929/`. The owned fixture and its
journal remain under
`.local-state/icloud-mounted-write-4c412f83-6590-41a0-b704-509ca7f3bf45/`.

This is one successful nested replacement after the documented corrections,
not a latency or reliability estimate. It does not establish safety for
arbitrary pre-existing user files, simultaneous edits, long sessions or
general-account write enablement.
