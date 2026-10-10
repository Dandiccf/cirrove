# iCloud conflict recovery with newer pending saves

Registered before the live run on 2026-09-30.

## Question and prediction

Can a conflicted ordinary iCloud replacement be rescued after an application has
saved newer versions, without losing the newest bytes or replaying blocked saves
over the changed cloud original?

Prediction: two newer saves queue while the original replacement is in flight.
When a controlled competing edit changes the cloud original, the original upload
becomes Conflict and both successors remain Pending. After remount, keep-both
uses the last successor's digest and retains every superseded payload. All three
superseded operations become Resolved in one transaction, with only one new Create
eligible to upload. The cloud original keeps its changed bytes and identity.
Both mounted versions remain correct after another remount.

## Registered procedure

The feature-gated `--account-mounted-competing-autosaves <fresh UUID>` arm reuses
the owned competing-edit preflight and independent full-content verification.
Once its one-shot marker proves the replacement has reached prepared MoveOld,
a separate Python application makes two different fsync'd saves through FUSE.
The arm checks that the head attempt is still Uploading/Verifying and both new
saves are Pending with no provider session. No synthetic Conflict is injected.

After the normal adapter refuses the changed cloud revision, read the newest
local bytes and independently hash the original. Restart the isolated mount,
invoke ordinary keep-both, require all three old saves Resolved, independently
hash the rescue Create receipt against the newest bytes, and read both paths
after another restart. Audit the durable journal read-only. Retain the remote
staging object and all failed evidence; cleanup remains a separate gate.

Only this run's UUID-owned tiny text fixture may be changed. Existing accounts,
services and mounts remain intact. One run at a time; no compilation alongside
it. The runner records PID, binary SHA-256 and private disk-backed temporary paths
before launch. A failed fixture is never reused.

## Synthetic regression evidence

Before the change, both newest-sealed and newer-unsealed journal tests failed with
Stale. The new kernel-mount autosave test failed because keep-both returned zero.
The tail-resolution fault test also failed its positive reachability check under
the old code. With the fix, the rescue uses the newest payload, remount retains
both paths, and a fault resolving the later generation rolls back the earlier
resolution, namespace move and rescue queue row together. Other-object dependents
remain refused before dirty bytes are sealed or any reservation released.

Dirty bytes are sealed as a preparatory durable generation; if rescue publication
then fails, that generation remains Pending behind the untouched conflict. The
rescue publication itself is atomic. No uncertain/in-flight successor is absorbed.
Atomic editor replacement records and intervening namespace operations still need
separate ownership-aware recovery support.

## Live result

Run `e7b2662f-f41f-4402-ac94-525c9285bdfe` passed (exit 0) in 279.807 seconds,
with binary SHA-256
`3cdad0e4480a7aeb038ad417b0bc36185d7229bd1cb8048c489ba4d6dffb816f` and a detached
isolated mount. The marker and journal confirmed two Pending saves were sealed
while the head replacement was still in flight. The normal adapter then returned
Conflict. After remount, keep-both resolved all three superseded saves and uploaded
only the newest bytes as a distinct Create. Independent remote digests and another
remount passed.

The read-only journal audit found Uploaded, Resolved, Resolved, Resolved, Uploaded;
all three superseded local payload files were reread and matched their recorded
sizes and SHA-256 values. The rescue's digest equals the last pending generation,
not the original conflicted generation. Both cloud identities have distinct local
owners; no unfinished queue reservations remain.

- [Live result](icloud-mounted-competing-autosaves-e7b2662f-f41f-4402-ac94-525c9285bdfe-live.json)
- [Independent journal/payload audit](icloud-mounted-competing-autosaves-e7b2662f-f41f-4402-ac94-525c9285bdfe-journal.json)

This is one correctness arm, not a latency/repeatability result; no within-arm
spread is available. Remote staging cleanup, atomic-editor conflict chains,
intervening renames and uncertain successors remain separate work. Normal iCloud
write settings and the installed service were not changed.

## Full code validation

`scripts/check.sh` passed at 2026-09-30T19:30:27Z, including clippy, workspace and
feature-gated iCloud tests, kernel mounts, scripts, ledger and documentation.
The retained local manifest is `.local-state/icloud-autosaves-check-2026-09-30/run.json`
(exit 0; btrfs TMPDIR/SQLITE_TMPDIR). Display-dependent window scenarios were not
run; this change does not alter widgets. The existing rustdoc `retry_stuck` link
warning remains. The live arm was complete and detached before compilation began.
