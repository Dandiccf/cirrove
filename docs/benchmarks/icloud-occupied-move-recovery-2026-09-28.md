# iCloud occupied move recovery, 2026-09-28

## Registered before live arm

Question: Can a new process determine the exact result of an occupied-name
`moveItems` request from a durable pre-request fixture record, without sending
another mutation? The [first collision arm](icloud-occupied-move-2026-09-28.md)
showed that Apple can automatically rename the incoming file, but the probe
had not persisted its source and destination identities.

The one arm creates two new UUID-named Cirrove-owned root validation folders,
each with one distinct small file. It confirms their exact IDs and bytes, then
persists account, folder and file IDs, source ETag, sizes and SHA-256 digests in
private local state with a file and directory sync **before** calling the
existing one-shot move probe. No password, token, signed URL or raw provider
body is written. A separate new process loads only that record and the saved
Apple session, lists both exact folders, and reads exact file IDs to classify
unchanged, moved with both byte streams intact, or indeterminate. The second
process has no mutation path for this mode. Neither process touches user files.

Endpoint: one record must exist before the request; the recovery command must
identify both exact file IDs and full digests, including any collision rename,
or report an unresolved state. A nonzero one-shot probe is retained as such;
read-only recovery must not retroactively make it a clean run. One run is
functional evidence only, not a reliability or latency estimate.

Prediction: Apple will again preserve both IDs and auto-rename the incoming
file. The new process should report the observed name and verify both hashes.
The normal iCloud mount remains read-only regardless of this outcome.

The private manifest records command, binary SHA-256, PID, expected duration,
start/completion time and a btrfs-backed private `TMPDIR`/`SQLITE_TMPDIR`.

## Results

The one-shot arm ran from 16:09:18 to 16:11:43 UTC and exited 0. Its private
fixture record `ce77bc15-3a6f-42a8-b9e4-69d059f60ce6` was synced before
the move request. The in-process verifier reported that both IDs and complete
bytes survived and that the incoming file was renamed.

A separate new process invoked `--reconcile-occupied-move` with only that run
ID. It loaded the pre-request record, verified the exact source and destination
folder identities at root, listed both, read the two file IDs by their known
parents and matched their full SHA-256 digests and sizes. The source folder was
empty; the destination held exactly two file IDs. The original occupant kept
`created-by-cirrove.txt`, and the moved file appeared as
`created-by-cirrove 2.txt`. The read-only command exited 0 without issuing a
mutation. This closes the identity-recording gap in the first collision arm.

The one arm establishes a recoverable observed outcome for this fixture. It
does not simulate a lost HTTP response, prove that Apple's collision rename is
stable across accounts, or make a POSIX overwrite safe. The normal iCloud mount
remains read-only. The one-shot arm's elapsed time is recorded for context only;
no timing comparison is made from one sample.
