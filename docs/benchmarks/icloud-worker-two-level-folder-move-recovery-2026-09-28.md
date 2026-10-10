# iCloud two-level folder move through the mutation worker, 2026-09-28

## Registered before live arm

Question: Can the shared mutation journal prepare one owned outer-folder ID,
tolerate a deliberately discarded move response, then reconcile a two-level
tree (outer folder → inner folder → file) by exact identities and full file
bytes in a new process without another move?

One arm creates two new UUID-named Cirrove validation parents, a new outer
and inner `Cirrove Nested Move-*` folder and one generated
`created-by-cirrove.txt` file in the isolated iCloud account. The adapter
persists the outer ID/current ETag, inner ID/name/current ETag and the file's
exact ID/document ID/name/ETag/size/SHA-256 inside the journal request. A
separate private fixture record is synced before enqueueing the request.
The worker prepares the outer ID durably, sends one conditional `moveItems`,
and deliberately discards the response, leaving `VerifyRequired`.

A fresh process reloads the saved Apple session, synced fixture and journal.
It compares the fixture and journal identities, then uses a reconciliation-
only adapter to check the exact parents, exclusive outer-folder location,
inner-folder identity/parent and only file's identities, size and full
SHA-256. It may mark `Applied` only at the requested destination with an
accepted `Upsert` receipt. The restarted adapter cannot send a move.

Endpoint: one `Applied` exact-ID receipt or an explicit conflict/unresolved
state. Prediction: the prior direct two-level move and one-level journal
recovery outcomes will combine. This is one generated tree and simulated
lost response, not a real timeout, arbitrary-depth recovery or reliability
proof. Ordinary iCloud mounts remain read-only.

The private run manifest must record command, binary SHA-256, PID, expected
duration, start/completion time and btrfs-backed private `TMPDIR` and
`SQLITE_TMPDIR`. No other measurement or compilation overlaps the live arm.

## Results

The one mutating arm ran 20:16:53–20:19:25 UTC and exited 0. Its private
manifest records PID 3536598, command, exact binary SHA-256 and the private
btrfs-backed temporary directory. The runner synced fixture
`7a560d09-d6b5-497f-8547-05423b28f340`, then durably prepared the outer
folder ID before one conditional `moveItems`. It deliberately discarded the
response; operation `917fa398-7144-4e7a-9be0-ac403d88f434` remained
`VerifyRequired` with the prepared ID.

A separate process (PID 3539011, 20:19:37–20:20:15 UTC, exit 0) reopened
the saved session, fixture and journal. Its reconciliation-only adapter
verified both exact root parents, the outer folder only at the requested
destination, the inner folder's exact ID/name/parent, and the only file's
ID/document ID/name/size and full SHA-256. The worker accepted an `Upsert`
receipt and marked the original operation `Applied` without a second move.
The private manifest records the second process and binary SHA-256.

This is one controlled simulated lost-response result for a generated
two-level tree. It does not establish genuine transport-timeout behavior,
arbitrary depth, concurrent-client safety, repeatability or mounted writes.
Ordinary iCloud accounts remain read-only. Elapsed time is context only.
