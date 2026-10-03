# Mounted iCloud recoverable removal inside an owned folder — 2026-09-29

Question: can a file newly created inside a Cirrove-owned child folder be
unlinked through FUSE, moved to iCloud's recoverable Trash by the shared
mutation worker, and remain absent after a fresh process remounts? The
feature-gated deletion adapter previously required the fixture root parent.

Prediction registered before the live arm: the journal-confirmed nested
upload and applied child-folder receipt should authorize only the exact
file ID, current ETag and complete byte digest. The worker must persist that
file ID before the conditional Trash request. An independent parent listing
should lack both the ID and name, while iCloud Trash retains that exact ID
with a restore path. The other file in the child folder should retain its ID
and complete bytes. A fresh remount must keep the removed name absent without
sending another mutation.

Arms and endpoints:

1. Synthetic guards require exact child ancestry and a prepared file ID; a
   reconciliation-only adapter refuses another Trash request. A journal test
   restores the nested upload, then excludes its ID only after an applied
   exact-ID removal receipt. These are safety checks, not Apple-side proof.
2. Reuse only generated test run
   `a01359b8-9644-4917-84f7-455228b2068b`. A separate Python process
   unlinks `Nested Created.txt` in its own `Mounted Folder`. Endpoint: one
   additional `Applied` mutation, exact prepared ID, recoverable Trash
   receipt, independent source absence and neighbouring file unchanged.
3. A fresh process restores the private journal and remounts FUSE. It checks
   the same source absence, Trash ID and neighbouring bytes without a new
   mutation.

Before each live arm, record command, binary SHA-256, PID, expected duration
and private btrfs-backed `TMPDIR`/`SQLITE_TMPDIR` in its manifest. Run no
compilation concurrently. This is functional validation, not a performance
measurement. Only the generated Cirrove-owned file may be removed. Ordinary
iCloud connections stay read-only; one controlled removal does not establish
general permissions, real timeout recovery or concurrent-client safety.

## Results

The synthetic guards and journal restore test passed. The mounted removal
arm (PID 3837964, binary SHA-256
`238d25b8ce63a14128cbd2e42c287b912b2f0978cc44064554322b0cf876969a`)
exited 0 after 151.4 seconds. The shared worker recorded an applied removal
with the nested upload's prepared exact file ID. An independent iCloud
session found neither that ID nor its name in the child folder and found the
same ID with a restore path in recoverable Trash. It also verified the other
file's ID and complete bytes. A new process (PID 3841239, same binary) exited
0 after 33.0 seconds, remounted FUSE, kept the removed name absent and
repeated the independent identity and byte checks without another mutation.
Both private manifests record btrfs-backed temporary storage and preserve
the run logs.

This is one controlled removal of a generated file. It does not establish
arbitrary nested-tree deletion, recovery from a real lost Trash response or
concurrent-client safety. Normal iCloud connections remain read-only.
