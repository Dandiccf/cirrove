# iCloud atomic editor conflict rescue

Registered before the live run on 2026-09-30.

## Question and prediction

Can a refused atomic editor replacement be rescued after remount without
redirecting old descriptors, overwriting the competing cloud version, or deleting
the editor's temporary cloud file before the rescue has been confirmed?

Prediction: the first original and editor temporary uploads finish. A mounted
rename-overwrite queues a conditional replacement and its cleanup. A controlled
competing content edit changes the original at prepared MoveOld; the normal
adapter returns Conflict. After remount, keep-both publishes a new Create and a
fresh cloud alias for the victim. The never-attempted old cleanup becomes
Resolved without a receipt. A newly appended cleanup waits for the rescue Create,
then moves only the editor temporary identity to Trash. The victim stream keeps
its old bytes, the rescue keeps the editor bytes, and the cloud original keeps
its competing bytes. Both named versions remain readable after another remount.

## Procedure and boundary

Use `--account-mounted-competing-atomic <fresh UUID>`, restricted by the existing
owned-fixture guard to a new `Cirrove Write Validation-<UUID>` folder. Create and
independently hash the original and editor temporary file, rename the temporary
through FUSE while holding an original descriptor, then inject the one-shot
competing edit at the already validated replacement boundary. Do not fabricate
Conflict. Verify both full contents, remount, invoke ordinary keep-both, hash the
confirmed rescue, check the exact temporary ID in Trash, and remount/read again.
Audit journal ordering, distinct provider owners and retained payloads read-only.

Do not reuse a failed fixture, permanently delete any item, restart the regular
service or compile during the run. Record the binary SHA, PID, command, deadline
and private disk-backed TMPDIR/SQLITE_TMPDIR before launching. Retain all staging
and failed evidence. The provider's internal replacement stage is distinct from
the editor temporary file; internal stage cleanup remains an open gate.

This is one correctness arm, not a performance or repeatability result. No
within-arm spread is available. This does not enable normal iCloud write settings
or prove multi-step atomic conflict chains and arbitrary namespace recovery.

## Synthetic regression evidence

Against the previous rescue implementation, the atomic journal rescue fails with
Stale; the real FUSE atomic rescue returns zero resolved items. A later ordinary
save after a fully completed atomic replacement also fails with Stale. The
transaction fault test cannot reach its injected failure without the change.
With the implementation, the tests require distinct cloud/victim/rescue bindings,
retained descriptor bytes, restart durability and cleanup blocked until rescue
acknowledgement. The transaction fault rolls back all binding and cleanup changes.
An uncertain or formerly attempted cleanup is refused before sealing dirty bytes.

The first synthetic mount run timed out in its test helper because that helper
required all historical mutations to be Applied. The journal now explicitly
records the never-sent cleanup as Resolved; the helper excludes resolved history
from its expected count. All three keep-both FUSE cases then passed.

## Live result

First run `3a6497ca-3d47-4e2a-bd34-561c25a15792` failed (exit 1) after
227.228 seconds; the isolated mount detached. The normal writer recorded Conflict,
but the competing-edit helper had not recorded independent confirmation. Its
postflight assumed exactly two entries, whereas this arm also has the independently
confirmed editor temporary file. The final attempt failed reading that missing
confirmation marker. This run is not acceptance evidence and is never reused.

The corrected helper validates exactly the original, internal stage and confirmed
editor source identities, plus the editor's parent, name, ETag, size and full bytes
before and after the one-shot edit. Unrelated, missing and duplicate identities
remain refused. The harness reports an explicit missing-confirmation context.

- [Retained failed result](icloud-mounted-competing-atomic-3a6497ca-3d47-4e2a-bd34-561c25a15792-live.json)

Corrected fresh run `f5c61412-5f8a-4660-be49-a8d58623d26f` passed (exit 0) in
485.010 seconds with binary SHA-256
`bef1ab3c4f09cc1627e2bb7adf95a082424ca4f7aa0b535fa3016a3d9afc3b28`.
The isolated mount detached. The controlled competing edit was independently
confirmed, the normal adapter refused the stale version, and keep-both succeeded
after remount. Full remote digests and mounted reads after another remount matched.
The editor temporary identity was independently found in Trash; the original
cloud identity and content remained unchanged by rescue.

The read-only audit found uploads Uploaded, Uploaded, Resolved, Uploaded and
mutations Resolved, Applied. The superseded cleanup had no attempt or receipt.
The new cleanup targets only the editor temporary ID, follows the rescue's queue
sequence and depends on its completion. The replacement remains not remotely
applied and explicitly names its rescue. Cloud and rescue have distinct active
owners and local identities; the detached victim remains separate. The retained
conflicted payload was reread and hashed. SQLite integrity passed, all prerequisite
edges point backward, and no unfinished queue reservations remain.

- [Corrected live result](icloud-mounted-competing-atomic-f5c61412-5f8a-4660-be49-a8d58623d26f-live.json)
- [Independent journal and payload audit](icloud-mounted-competing-atomic-f5c61412-5f8a-4660-be49-a8d58623d26f-journal.json)

One successful arm does not establish repeatability. Internal provider-stage
cleanup, chained atomic conflicts, intervening namespace operations, native
package editing and installed release acceptance remain open.

## Full code validation

`scripts/check.sh` passed at 2026-09-30T20:08:41Z, including formatting, clippy,
workspace and feature-gated iCloud tests, actual synthetic kernel mounts, scripts,
ledger and documentation. All 38 writable-session kernel cases passed. The retained
manifest is `.local-state/icloud-atomic-rescue-check-2026-09-30/run.json` (exit 0;
btrfs TMPDIR/SQLITE_TMPDIR). The live fixture was detached before compilation.
The pre-existing rustdoc `retry_stuck` link warning remains. Display-dependent
window scenarios were not run; this change does not alter widgets. This is isolated
experimental validation, not an installed writable iCloud release.
