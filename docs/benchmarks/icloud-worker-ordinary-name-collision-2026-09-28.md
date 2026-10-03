# iCloud Create collision at an ordinary filename — 2026-09-28

Registered before the live run. The only cloud target is the Cirrove-owned
folder from the preceding ordinary-name trial, containing Cirrove's own
`Résumé 2026 final.txt`. The original exact ID and full bytes are checked
before and after. A second worker uses a separate local journal and tries
to create a different payload under that occupied name. It may allocate
an invisible Apple upload slot; it must not upload content, register a
second file, rename or trash the existing one. No user item, normal mount,
other client state or permanent deletion is touched.

Question: with a newly allocated document ID that differs from the
existing item's ID, does the shared worker report `Conflict` before any
visible file mutation and retain the original exact ID/full bytes?

One arm: load and authenticate the prior owned journal, verify that its
folder contains exactly the original file and bytes, reserve a new
document ID for another Create with the same name, run one worker pass,
then verify the folder still contains exactly the original ID and bytes.
Prediction: `Conflict`, with no current receipt for the second operation
and no new listed file. A surprising or uncertain result is inspected
read-only, not retried. One run has no within-arm spread or latency claim.
It does not prove every collision timing, concurrent external edits,
other parents, account classes or mounted writes.

Endpoint: exact-ID/full-byte folder inspection and durable worker state.
A private manifest records the command, binary SHA-256, PID, expected
duration and disk-backed TMPDIR/SQLITE_TMPDIR before the run. No token,
cursor, signed URL, raw provider body or file content is logged.

## Observed

The second worker allocated a different document ID, then returned
`Conflict` for the occupied ordinary filename. A fresh authenticated
read-only session found exactly the original listed ID and full original
bytes; no new file appeared in the owned folder. An independent read-only
SQLite reopening found the second operation in `conflict` with no remote
receipt and its local payload retained. The private manifest records binary
SHA-256
`63baaea0ca0ed91ec2b75dc182a75f26db5e4dbcc2fcb0940c838372a2f3d307`
and btrfs temporary storage. This one controlled collision has no
within-arm spread or latency claim. It does not cover a name arriving
during an already in-flight Apple registration request.
