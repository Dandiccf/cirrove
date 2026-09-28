# iCloud populated-folder move through the mutation worker, 2026-09-28

## Registered before live arm

Question: Can the shared mutation journal recover a deliberately discarded
response to one conditional move of a Cirrove-owned folder containing one
small file, with both exact identities and full file bytes verified by a new
process before accepting the receipt?

One arm creates two new UUID-named Cirrove validation parents, one nested
`Cirrove Nested Move-*` folder and one `created-by-cirrove.txt` file in the
isolated iCloud validation account. The file contains only generated test
bytes. Before the move, the adapter persists the folder ID/current ETag and
the child's exact ID, document ID, name, size and SHA-256 in the journal's
`Relocate` request. It saves a synced private fixture record and prepares the
exact folder ID. The worker sends one conditional `moveItems` request and
deliberately discards the response, leaving `VerifyRequired`.

A fresh process opens the same journal and a reconciliation-only adapter. It
checks the source/destination parents, exclusive location and identity of the
folder, and the sole child's exact IDs, name, size and full SHA-256. It may
mark `Applied` only if the exact populated tree is at the requested
destination and the original request accepts the receipt. The restarted
adapter cannot send another move. Any divergent child blocks acceptance.

Endpoint: one `Applied` exact-ID receipt after restart, or explicit
unresolved/conflict state. Prediction: the earlier direct populated-folder
move and empty-folder journal recovery outcomes will combine here, with the
same tree verified at destination after a lost-response simulation. This is
one functional arm, not evidence of transport-timeout or long-session
reliability. The ordinary iCloud mount remains read-only.

The private run manifest must record command, binary SHA-256, PID, expected
duration, start/completion time and btrfs-backed private `TMPDIR` and
`SQLITE_TMPDIR`. No compilation or second measurement overlaps it.

## Results

The one mutating arm ran in an isolated new fixture under run
`c3801890-1f3d-4094-8ee3-f28aef413880` and exited 0. The private manifest
records PID 3456415, exact binary SHA-256, command, btrfs-backed private
temporary directory and start/end timestamps. The worker saved the exact
folder ID before one `moveItems` call. After deliberately discarding the
response, the journal recorded operation
`96b8af0b-1219-4329-9cf7-6a89117e4072` as `VerifyRequired`.

A separate process (PID 3458935, also recorded in the private manifest)
reopened the saved session, fixture and journal. The reconciliation-only
adapter found the same folder ID exclusively at the requested destination,
with its only child matching the saved exact file and document IDs, name,
size and full SHA-256. The worker accepted an `Upsert` receipt and recorded
`Applied`, without another move request. Both processes exited 0. The
post-arm code also compares the synced child record to the journal manifest
before reconciliation; this additional guard is covered by compilation and
synthetic checks but was not in the binary used for the live arm.

This is one successful lost-response simulation for one generated file in a
new folder. It does not prove genuine transport-timeout recovery, deeper
subtrees, concurrent edits or repeatability. The ordinary iCloud mount
remains read-only. Elapsed time is context only, not a performance claim.
