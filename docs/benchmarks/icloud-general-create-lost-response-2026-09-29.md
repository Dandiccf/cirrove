# iCloud general file Create after a lost registration response — 2026-09-29

Registered before the live run. Scope is one new UUID-named Cirrove-owned
folder and one small generated text file in the isolated
`iCloudGuiValidation` account. Existing user files, account settings and
installed mounts are untouched. The ordinary iCloud account remains read-only.

Question: after the **normal-build** `ICloudFileCreate` adapter sends Apple's
file registration request but the test build discards its response, does the
shared upload worker preserve the reserved document identity and reach
`Uploaded` in a fresh process solely by observing that exact remote item and
its complete SHA-256, without resending registration or creating a duplicate?

One functional arm: enqueue the generated file in a private journal; allocate
and persist the upload slot and content receipt; send registration once and
discard its response; require `VerifyRequired`. In a separate process, use a
reconciliation-only adapter and the retained checkpoint to verify the exact
document ID, parent, size and full bytes, then require `Uploaded`. Prediction:
one visible file with the reserved identity and matching SHA-256, and one
durable uploaded receipt. Any mismatch or missing item leaves the operation
unconfirmed. The endpoints are the persisted journal state and an independent
remote list/read. This single run establishes a recovery path, not general
provider reliability, timing or duplicate-free behavior under every failure.

Before the run a private manifest records command, binary SHA-256, PID,
expected duration and disk-backed `TMPDIR`/`SQLITE_TMPDIR`. No other
measurement or compilation runs concurrently. No token, signed URL, cursor,
raw provider response or file content is logged.

## Observation

The first live arm reached `VerifyRequired` for run
`7401ac59-6a8d-4e34-92a3-1afc1fcf998f` and a separate process reached
`Uploaded` for operation `b25f331b-a0ab-47d4-86cb-05f2a5521650` with the
reserved document identity and full-byte SHA-256. This initial probe checked
one exact-ID match, but did not reject a second item with a different ID in
the folder, so the duplicate check was strengthened before the final arm.

The final arm reached `VerifyRequired` for run
`a1da650c-1b22-49f5-b72b-303d01c1d00a`. A fresh process with
`reconciliation_only()` reached `Uploaded` for operation
`15bcff92-bbf6-474a-be16-e079c2df1935`. Its independent remote listing
contained exactly one entry in the dedicated folder, with the reserved
document identity, expected parent, name and size. A full read of that file
matched the journal's SHA-256. The recovery adapter refuses upload and
registration requests, so this transition did not resend either. Both final
processes exited zero. Their private manifests record btrfs temporary storage
and the same binary SHA-256,
`a2f5309c89bb08bd8d129f176099f3f39085b35f18b6980f029250ed18cd3c83`.
`scripts/check.sh` passed after the stronger assertion was added.

The test uses a small file and one owned folder. It does not establish
ordinary-account write reliability, replacement semantics, or behavior when
Apple accepts a registration but delays visibility beyond the worker's
inspection window.
