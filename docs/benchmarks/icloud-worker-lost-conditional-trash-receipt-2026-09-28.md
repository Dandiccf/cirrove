# Lost conditional iCloud Trash receipt — 2026-09-28

Registered before the live run. Only a new UUID-named Cirrove folder with
two newly uploaded small owned files in the isolated `iCloudGuiValidation`
account may be changed. No ordinary mount or existing user file is touched.
The old test ID is left recoverable in Trash; there is no permanent delete.

Question: if the first process sends a conditional `moveItemsToTrash` for
the saved old ETag but discards Apple's response, can a fresh process
inspect the exact old ID and full bytes, finish the staged rename at most
once, and publish both identities without a second Trash request?

Two sequential arms on **the same fixture**: the first process prepares
and sends the old Trash step but deliberately discards its response,
requiring a saved checkpoint and `VerifyRequired`. The second process
reopens that same journal and session, observes the remote phase, and may
send only the outstanding staged rename. Complete old/new bytes, ETags and
Trash restore metadata are required before `Uploaded`. If the first
request did not commit or the state is uncertain, the second process must
stop without blindly replaying Trash. This is not a latency comparison.

Prediction: the first process stops at `VerifyRequired`; the second finds
`OldAtRecovery` and reaches `Uploaded` with one current staged ID and one
hidden old ID under Trash. A single trial has no within-arm spread and
does not establish repeatability, concurrent-edit safety, account-class
coverage or mounted writes.

Endpoint: exact-ID/full-byte remote checks and a reopened journal with
both bindings. A private manifest for each process records command,
binary SHA-256, PID, expected duration and disk-backed temporary storage.
No token, signed URL, raw provider body or file content is recorded.

## Observed

The first process deliberately discarded the conditional Trash response and
ended `VerifyRequired`. A separate read of its journal confirmed the saved
checkpoint, old-ID recovery reservation and absence of a fabricated current
receipt. The second process reopened that journal, reconciled the exact
old ID in Trash, finished the staged replacement and reached `Uploaded`.
Its code path does not issue another `moveItemsToTrash` after observing
`OldAtRecovery`; the final CLI result also reported no replay. Reopening
SQLite independently showed one current staged-ID binding and exactly one
hidden, owned former-ID binding under the opaque Trash parent.

Both private manifests record binary SHA-256
`be82696b1f975f50f60d0dc634864cc1b74e7b14cb4cc76958e34bbbfb8c2e66`
and btrfs temporary storage. This is one response-loss run on one owned
fixture, with no within-arm spread or latency claim. It does not cover a
lost staged-rename response, a provider timeout during the Trash request,
external concurrent edits, collisions, long-term Trash retention or
ordinary mounted writes.
