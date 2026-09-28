# Mounted iCloud large replacement, 2026-09-28

## Registered before live work

Question: Can the isolated Cirrove FUSE mount create a 5 MiB owned file and
replace it with a 6 MiB version, then reconcile the two exact IDs through the
shared journal while reading and hashing in bounded 4 MiB ranges?

Arm A creates a new UUID-named `Cirrove Write Validation-*` folder, writes a
5 MiB deterministic file through a separate process, and creates a sibling
folder. Arm B reopens only that owned fixture, truncates and fsyncs the file
with 6 MiB of different deterministic bytes, then waits for the shared
replacement worker. Arm C is a read-only fresh-process remount of the same
fixture. The application and independent iCloud reader compare full SHA-256
digests; the old exact ID must appear once in a complete Trash listing with a
restore path. No existing user item or normal account mount is changed.

Endpoint: A has one `Uploaded` file and one `Applied` folder receipt. B has two
`Uploaded` file receipts, the second a Replace bound to the first exact ID and
ETag, plus the same folder receipt; the current exact ID and full 6 MiB digest
match an independent range read, and the old ID is recoverable in Trash. C
must reproduce the new content through FUSE and independent ranges without
adding another upload or mutation. Each arm ends with its FUSE mount detached.
Any uncertainty leaves the fixture and journal intact; do not repeat a
possibly accepted remote mutation.

Prediction: A may expose a larger mounted Create or upload limit that the
earlier 43-byte run did not. If A passes, B should exercise at least two read
ranges per file and may exceed the worker deadline because its recovery checks
read both versions and the Trash backup. C should pass only if the exact-ID
handoff receipt is durable. One sequence is functional evidence, not a
reliability or throughput result; no timing claim will be made without start
and completion timestamps and repeated arms.

The private run manifests record the command, binary SHA-256, PID, expected
duration, btrfs-backed `TMPDIR` and `SQLITE_TMPDIR`, and outcome. Results and
any corrections belong below in this same artifact.

## Results

Arm A passed. A separate FUSE application fsynced the 5 MiB file and created
the sibling folder. The validator found one `Uploaded` file and one `Applied`
folder receipt, then independently listed their exact IDs and read all file
bytes in 4 MiB ranges with the expected SHA-256. The mount was detached at
exit. The fixture UUID is `8c421e71-3c23-4a29-b16a-18aebd9f9032`. Private
manifest:
`.local-state/icloud-large-create-bb7a92af-3a8f-4595-97e6-f3835fad8827/manifest.json`.
It records binary SHA-256
`2fb051711a182f72b291d4eaf0b53f890c32154e3ea7a840d72d11945c8b0142`,
PID 3027554, btrfs-backed temporary directories, start
`2026-09-28T14:05:09.672894+00:00`, completion
`2026-09-28T14:06:30.442612+00:00`, and exit 0. This is one functional
observation; its duration is not a performance result.

Arm B passed. The separate application truncated and fsynced 6 MiB through
FUSE. The worker recorded a second `Uploaded` receipt whose Replace intent
was bound to the first exact ID and ETag. The new receipt has a different ID
at the same parent and name. An independent session listed that ID, verified
the complete 6 MiB SHA-256 by bounded ranges, and found the old exact ID
uniquely recoverable in Trash. The mount was detached at exit. Private
manifest:
`.local-state/icloud-large-replace-df5c4b7a-8030-4a18-841d-3bef2befb1ca/manifest.json`.
It records the same binary hash, PID 3029810, btrfs-backed temporary
directories, start `2026-09-28T14:07:07.672111+00:00`, completion
`2026-09-28T14:15:27.455506+00:00`, and exit 0. The 8m20s elapsed time is
one observation, not a throughput or latency claim. This large path performs
multiple full-content checks and is presently slow.

Arm C passed. A fresh process restored only the journal-confirmed IDs,
mounted the same fixture, read every replacement byte through FUSE, and
independently checked the exact current ID and complete 6 MiB digest in
bounded ranges. It again found the old exact ID recoverable in Trash. The
journal remained at two `Uploaded` file rows and one `Applied` folder row;
no new upload or mutation was added. The mount was detached at exit. Private
manifest:
`.local-state/icloud-large-remount-f37341cb-484b-4cdb-bb8f-db603ce6277c/manifest.json`.
It records the same binary hash, PID 3038822, btrfs-backed temporary
directories, start `2026-09-28T14:16:03.734469+00:00`, completion
`2026-09-28T14:16:43.295539+00:00`, and exit 0.

Together these arms establish one functional 5-to-6-MiB mounted replacement
of a Cirrove-owned file and a read-only fresh-process recovery. They do not
establish repeatability, general writable iCloud, handling of edits by another
client, or behavior after an actual network interruption. The slow B arm
should be investigated before treating this path as usable for normal files.
