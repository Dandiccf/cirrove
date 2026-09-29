# General iCloud file-Trash lost-response recovery — 2026-09-29

Question registered before live validation: can the non-feature iCloud
`ICloudFileTrash` adapter, reconstructed from the account's sealed session,
resolve a discarded conditional Trash response through the shared durable
mutation journal without sending a second delete? The previous clean-response
trial checked complete Trash bytes but did not exercise this recovery path.

Prediction: an isolated validator creates a fresh Cirrove-owned folder and
small generated file, saves its exact ID, parent, ETag and full SHA-256, then
prepares that ID in the journal. One conditional Trash request is sent and its
response deliberately discarded. The first process must leave the operation
`VerifyRequired` without a receipt. A separate process must load the sealed
session and the same journal, verify absence at the parent and the exact file
ID, size, restore path, stable Trash ETag and complete saved bytes in Trash,
then record one `Applied` recoverable removal without another request.

Endpoints: one owned fixture only; no ordinary iCloud account settings or
daemon changes. A failure, changed byte, incomplete Trash listing or missing
saved digest must never become `Applied`. Each process receives its own
private btrfs-backed `TMPDIR`/`SQLITE_TMPDIR`, and a manifest records command,
binary SHA-256, PID and expected duration before the arm. No compilation
runs during the live trial. This deliberately lost response is not evidence
for arbitrary real timeouts, concurrent clients or general iCloud writes.

## Result

The generated fixture used recovery run
`5d5eed13-4caf-4957-a234-86053f648606`. Both arms used one built binary,
with SHA-256 and btrfs temporary storage recorded in private manifests under
`.local-state/icloud-general-trash-recovery-live-b303b5d0-2332-4720-a091-89f7185e8fea`.
The discard arm (PID 3956924, 62.3 seconds) created and read back the owned
file, sent one conditional Trash request and dropped its response. The saved
journal row had the exact prepared file ID, state `VerifyRequired` and no
receipt. The fresh reconciliation arm (PID 3957994, 157.1 seconds) loaded
the sealed session and journal, checked the complete Trash-byte SHA-256 and
stable recoverable identity through `ICloudFileTrash`, and changed that same
operation to `Applied` with the exact-ID `Removed` receipt. Its code path has
no send operation; independent exact-ID Trash inspection also succeeded.
Neither arm touched an ordinary account mount or the installed daemon.

This establishes one deliberately discarded response for a small generated
file. It does not prove recovery from a real network timeout, a concurrent
remote edit or an arbitrary user file. Normal iCloud connections remain
read-only.
