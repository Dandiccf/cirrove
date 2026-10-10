# iCloud conditional Trash worker after single-observation change — 2026-09-28

Registered before the run. One new UUID-named folder and two new small
Cirrove-owned files in isolated `iCloudGuiValidation` may be changed. No
existing user file, normal iCloud mount or other client state is touched.
The old test ID may remain recoverable in Trash; no permanent delete or
restore is part of this run.

Question: after removing repeated complete Trash downloads within one
provider deadline, can the native owned-fixture adapter finish the
conditional Trash replacement through the shared worker and publish both
exact IDs without a manual local receipt transaction?

One arm: prepare and verify original and staged bytes, reserve the old ID,
conditionally move it to Trash at its saved ETag, verify its exact ID/full
bytes and complete recovery metadata, rename only the staged ID, then
publish both IDs in the journal. An uncertain result must stay
`VerifyRequired` without blindly resending Trash or rename. The binary
performs at most five worker passes. There is no comparative arm.

Prediction: `Uploaded` with one current staged ID and one hidden, owned old
ID at the opaque Trash parent. A failure will be investigated read-only;
one success is a direction, not repeatability or a latency claim. There is
no within-arm spread. This does not test concurrent edits, name collisions,
long-term Trash retention, general folders or mounted writes.

Endpoint: exact-ID/full-byte receipts and reopened journal state. A private
manifest records command, binary SHA-256, PID, expected duration and a
disk-backed TMPDIR/SQLITE_TMPDIR before the run. No token, signed URL, raw
provider body or file content is recorded.

## Observed

The one owned-fixture run reached `Uploaded` through the shared worker,
without a manual local publication step. The worker reported both exact
identities as a durable replacement. Reopening its SQLite journal in a
separate process confirmed a current receipt, exactly one current binding
to the staged ID, and exactly one hidden, owned binding for the former ID
whose remote parent is the iCloud Trash root. The original and staged full
bytes were checked by the adapter before that receipt.

The private manifest records binary SHA-256
`83a2f341a1a6cad99fa38426903fcc84dbd51758a743f8fee213878187ffca87`
and btrfs temporary storage. This is one run with no within-arm spread or
latency claim. It does not prove repeated reliability, concurrent-edit
protection after the saved ETag check, a lost Trash/rename response, name
collision behavior, Trash retention, folder operations or mounted writes.
