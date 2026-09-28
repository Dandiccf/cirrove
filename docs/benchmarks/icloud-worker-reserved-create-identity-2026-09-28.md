# iCloud create receipt bound to a reserved document ID — 2026-09-28

Registered before the live run. Only one new UUID-named Cirrove folder and
one small uniquely named Cirrove-owned file in the isolated
`iCloudGuiValidation` account may be created. No existing user item, normal
mount, other client state or permanent deletion is touched.

Question: can the shared upload worker persist Apple's allocated document ID
before the visible file registration, then resolve a deliberately discarded
registration response in a fresh process using that exact ID and full bytes?
Name and byte equality alone must never authorize a create receipt.

Two sequential arms use the same fixture. The first saves a prepared
checkpoint, asks Apple for an upload slot, saves that slot and document ID
in the local credential vault, uploads the small payload, sends one visible
`add_file` request, and discards the registration response. It must stop
`VerifyRequired` with a saved allocated document ID and no fabricated
success receipt. The second process opens the same journal and credential
vault, runs in reconciliation-only mode, and may read but cannot send
another upload or registration. `Uploaded` requires a returned item whose
document ID equals the saved allocation and whose full bytes match the
sealed journal payload.

Prediction: the first process remains `VerifyRequired`; the second publishes
the exact created ID without replaying either upload or registration. An
unrelated same-name item, even with identical bytes, must be a conflict;
that case is exercised by the red-before-fix synthetic test, not by adding
a colliding cloud file in this run. One live trial has no within-arm spread
or latency/reliability claim. It does not prove arbitrary names, large
files, general folders or ordinary mounted writes.

Endpoint: exact-ID/full-byte remote verification and reopened SQLite state.
Each arm writes a private manifest before starting with command, binary
SHA-256, PID, expected duration and disk-backed TMPDIR/SQLITE_TMPDIR. No
credential, slot URL, cursor, signed URL, raw provider body or file content
enters this artifact or diagnostics.

## Observed

The first process saved an Apple-allocated document ID in the encrypted
worker checkpoint before submitting the visible registration. Its one
registration response was discarded, and the worker stopped
`VerifyRequired` without a success receipt. The second process reopened
the same journal and checkpoint in reconciliation-only mode, matched the
returned remote document ID to the saved allocation, checked the exact
parent and full file bytes, and reached `Uploaded` without another upload
or registration request. An independent read-only SQLite reopening found
the matching operation `uploaded` with a remote receipt and retained local
payload.

The synthetic same-name foreign-document test failed against the previous
name-and-byte receipt rule, then passed after exact document-ID binding;
the full feature-gated iCloud test set passed. Both private manifests record
binary SHA-256
`e7e66028f5cc079a0147a31e75878990124a79a8631b72b5856a3539fd23d988`
and btrfs temporary storage. This is one small owned-file run with no
within-arm spread or latency claim. Arbitrary names, large files, account
classes and mounted writes remain unverified.
