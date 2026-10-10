# iCloud old-rename response-loss experiment — 2026-09-27

Registered before the live run. The previous two-process trial crossed a
verified checkpoint. This trial tests the narrower fault window after the
old rename request has reached Apple but before its receipt is committed
locally. Both remote files are newly created Cirrove-owned fixtures in a
fresh isolated folder; the normal service remains read-only.

Arm A: create and hash-check both items; fsync a private account-bound plan;
fsync `old_rename_pending`; send the old item's rename to its unique recovery
name; deliberately discard the response and exit without inspecting or
advancing the checkpoint. Arm B: a new process loads the pending checkpoint,
lists the exact item IDs and reads both full hashes. It may advance to
`old_at_recovery` only if the remote state proves that rename committed.
It must never resend the pending old rename. It then fsyncs
`new_rename_pending`, moves the staged item to the vacated name, verifies
both IDs and hashes and fsyncs `complete`. No comparative arm.

Prediction: the second process will find the old item at its recovery name,
reconcile the lost receipt and complete the second step while preserving both
versions. If the old item is still at the original name or any identity/hash
differs, the second process must stop without replay. This does not test a
network timeout with an Apple request still in flight, power loss during
fsync, or the staged upload before its ID is checkpointed.

The private record and start/resume manifests live under
`.local-state/icloud-lost-old-receipt-validation/`. Each manifest records
command, binary hash, PID, expected duration and disk-backed private temp
filesystem before its process starts. No tokens, signed URLs, raw responses
or file contents are recorded.

## Observed first run

The first process exited normally after 117.5 seconds, leaving a private
`0600` record in `old_rename_pending`. A separate process, using the same
binary hash and sealed account session, completed in 75.7 seconds. It found
the original exact ID at its unique recovery name, checked both full content
hashes, completed the staged rename, and saved `complete`. Both IDs and byte
sequences remained; no temporary checkpoint file remained. These are one
run per arm without a within-arm spread. The response was deliberately
discarded *after* Apple replied; a network timeout with a request still in
flight remains untested.
