# General iCloud file move through the mounted journal — 2026-09-29

Question registered before the live run: can a non-feature, exact-ID
`ICloudFileMove` replace the fixture-only cross-folder move while preserving
the shared mutation journal's prepared-ID and restart behavior?

Prediction: on a fresh Cirrove-owned FUSE fixture, a separate application
creates and fsyncs a file and an empty folder, renames the file twice through
the already validated general rename adapter, then moves that same file into
the confirmed child folder. Before one conditional `moveItems`, the general
move adapter must prove the source file ID, parent, ETag and complete saved
SHA-256 and the destination folder's exact ID and parent. A collision at the
destination cannot be treated as success. After the move, independent Apple
listings must find the file ID only in the destination, with unchanged name
and complete bytes. A new process must remount and reopen it there without
another mutation. HTTP 429/5xx and incomplete move receipts remain uncertain.

Endpoints: exact-ID Applied Upsert receipt, independent source absence and
destination content, then successful fresh-process remount. This only mutates
the newly generated test items; normal iCloud accounts stay read-only. Each
arm records command, binary SHA-256, PID, expected duration and private
non-tmpfs `TMPDIR`/`SQLITE_TMPDIR` in a manifest before it starts. No
compilation runs during the sequence. One clean-response trial cannot prove
real lost-response, concurrency or arbitrary-tree reliability.

## Result

The generated fixture `d28cea2a-2f2b-4613-b826-1a1e5c1e08b4` passed all
five arms. Manifests under
`.local-state/icloud-general-move-run-5b63ea6f-f47c-4d31-80d6-84ca69991cf6`
record the binary SHA-256 and btrfs-backed temporary storage. Create (PID
4020992, 107.8 seconds) reached a confirmed file and child folder. The
first rename (PID 4022604, 14.7 seconds) and second rename (PID 4022879,
44.1 seconds) preserved the file's exact ID and bytes. The move through
`ICloudFileMove` (PID 4023503, 49.1 seconds) reached an Applied exact-ID
Upsert, with independent Apple listing showing the file only in the child
folder and a complete byte read matching the original. A fresh process
(PID 4024269, 34.6 seconds) remounted and reopened it in that folder without
another mutation. All source and destination checks used the same confirmed
file ID, not path equality as identity.

This is one controlled clean-response move of a small generated file.
No live throttling, real timeout, destination collision or concurrent edit
was induced. Normal iCloud connections remain read-only.
