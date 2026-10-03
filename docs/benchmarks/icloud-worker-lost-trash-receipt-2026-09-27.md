# iCloud mutation-worker Trash response loss — 2026-09-27

Registered before the live run. Both processes use the isolated
`iCloudGuiValidation` session and one new Cirrove-owned folder/file. The
ordinary iCloud mount remains read-only.

Question: after Apple accepts `moveItemsToTrash` but Cirrove deliberately
discards the response, can a fresh mutation worker use its durable prepared
remote ID to confirm the exact item in complete Trash metadata and commit
`Applied(Removed)` without sending a second delete request?

Arm A: create and read back the test file, enqueue `RemoveFile`, let the
shared worker persist the exact prepared item ID, then send one Trash request.
After Apple responds, discard the response before the adapter's normal
confirmation and require durable `VerifyRequired`. Arm B: exit Arm A, reopen
the journal and saved account session in a fresh process, reconstruct only a
read-only fixture handle from the request's exact ID and parent, and run the
worker's verification path. The Arm B adapter explicitly refuses
`mutate_prepared`, so it cannot delete again. No comparative arm.

Prediction: Arm A retains the prepared ID at `VerifyRequired`; Arm B finds
the same ID with a restore path in a complete Trash listing and commits
`Applied` with a matching `Removed` receipt. An absent/ambiguous item or
incomplete listing must remain unresolved. One run per arm has no within-arm
spread or latency claim. Discarding a received reply does not test an
in-flight request, concurrent remote edits, permanent deletion or restore.

Endpoint: durable state, prepared ID and receipt after each process. Separate
private manifests record command, binary SHA-256, PID, expected duration and
disk-backed temporary storage before launch. The fixture remains recoverable
in Trash. No credentials, signed URLs, raw provider bodies or file bytes are
recorded here.

## Observed

Arm A ended with `VerifyRequired` for operation
`b76b614f-b813-4e23-9688-46d2b80e23b4` after discarding an already
received Trash response. The journal retained the exact prepared file ID.
Arm B, in a new process with a read-only reconciliation adapter, found that
same ID with a restore path in the complete Trash listing and committed
`Applied(Removed)` for it. Arm B had no code path that could send another
delete request. The two private run manifests record binary SHA-256
`0b23a499dfb57011681d7575cfe608294e10e7d3a4287c45d3d876b9c89d1aed`
and btrfs temporary storage. This is one owned file in one account, with
one run per arm; it does not establish in-flight timeout safety, concurrency
behavior, latency distribution or release readiness.
