# iCloud populated nested-folder move, 2026-09-28

## Registered before live arm

Question: Does one conditional `moveItems` of a newly created folder preserve
both that folder's exact ID and the exact ID and complete bytes of its only
child file? The preceding folder arm tested an empty child only.

The one arm creates two fresh UUID-named Cirrove validation parents at root,
then one `Cirrove Nested Move-*` child under the source. A narrow owned upload
registers exactly one small `created-by-cirrove.txt` in that child and reads
the complete bytes back by exact ID. The runner re-reads the nested folder's
current ETag, then syncs account, parent, folder and file IDs, names, ETags,
file size and full SHA-256 digest in private local state **before** sending
one `moveItems`. It does not retry. A separate process later lists both
parents and the nested child by exact ID and reads the file bytes. No existing
user item is addressed.

Endpoint: if accepted, the same folder ID appears only at the destination
under its original name and the same file ID and full digest remain inside.
If rejected, folder and file stay intact at the source. Any lost, duplicate,
renamed or byte-changed item is indeterminate. A response status alone does
not establish the result.

Prediction: Apple will preserve this one-level subtree across the move.
One arm is functional evidence only, not a journal-backed or mounted move;
deep subtrees, collisions, lost responses and concurrent clients remain open.
The ordinary iCloud mount remains read-only.

The private manifest records command, binary SHA-256, PID, expected duration,
start/completion time and btrfs-backed `TMPDIR`/`SQLITE_TMPDIR`.

## Results

The one mutating arm ran from 18:39:28 to 18:41:50 UTC and exited 0. It
registered and byte-verified the new file inside the nested folder, then
synced private record `7285054d-b11c-4c8a-bcee-f6571df7d376` before its
one conditional `moveItems` request. Immediate readback found the exact
nested-folder ID only at the destination and the exact file ID and complete
bytes still inside it.

A separate new process loaded that record and the saved Apple session. It
listed both exact root parents and the nested folder, then read the child file
by its recorded ID and matched its complete SHA-256 and size. It found the
folder only under the destination with its name unchanged and exited 0
without mutation.

This is one successful one-level subtree protocol arm. It does not establish
journal-backed restart for populated moves, deep subtree preservation, lost
response handling, concurrent edits, collisions or release reliability. The
normal iCloud mount remains read-only. The elapsed time is context only.
