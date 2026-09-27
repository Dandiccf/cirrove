# iCloud recoverable delete through the shared mutation worker — 2026-09-27

Registered before the live run. Only a fresh Cirrove-owned test file in the
isolated `iCloudGuiValidation` account may be moved to Trash. The normal
iCloud mount remains read-only.

Question: can the shared durable `MutationWorker` save the exact prepared
remote ID before sending iCloud's recoverable delete and then commit a
`Removed` receipt for that same ID? A focused journal test first failed with
`JournalError::Corrupt`: the journal previously accepted a prepared ID only
with an `Upsert` receipt. The fix accepts `Removed` only when its item equals
the saved prepared ID.

Arm A: create a new UUID-named validation folder and small file, enqueue
`RemoveFile` with account, collection, original parent, ID and ETag in a
private upload/mutation journal, and run one worker. The adapter verifies the
full original bytes and ETag, sends one `moveItemsToTrash`, then verifies the
same ID in complete Trash metadata. If the worker times out after the request,
only its `VerifyRequired` reconciliation path may run; no uncertain request
may be replayed. No comparative arm.

Prediction: the journal finishes `Applied`, retains the exact prepared ID,
and stores `Removed` for that ID. A timeout may instead leave
`VerifyRequired` for further inspection. One run has no within-arm spread
and cannot establish interactive latency, lost-response recovery or broader
account reliability.

Endpoint: final durable mutation state, saved prepared ID and receipt. The
private manifest records command, binary SHA-256, PID, expected duration and
disk-backed temporary storage before execution. The test file remains
recoverable in Trash. No credentials, signed URLs, raw provider bodies or
file bytes are recorded here.

## Observed Arm A

The shared worker saved the exact prepared ID and completed operation
`42a634d3-5ccd-41f9-811d-aefa51fd0595` as `Applied` with a `Removed`
receipt for that ID. The owned file remains recoverable in iCloud Trash.
The private manifest records binary SHA-256
`14f2b7d1de28481d68555a3269e0a2254a77236023a52be7f4e085d98195f25e`
and btrfs temporary storage. No second attempt was needed. This is one
account and one run; it does not test a lost server response or a fresh
process's recovery.
