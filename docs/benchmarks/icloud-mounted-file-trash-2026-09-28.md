# Mounted iCloud file Trash, 2026-09-28

## Registered before the live arm

Question: Can a separate application call `unlink` through the real FUSE mount
on the Cirrove-created `Mounted Create.txt` in the isolated
`Cirrove Write Validation-37fc6800-2a7f-4f43-9a6d-fd1d4f2c5938` folder, and
can the shared mutation worker move that exact file to recoverable Apple Trash?
The folder's other test child was already removed and verified in
`icloud-mounted-empty-folder-trash-2026-09-28.md`. The normal iCloud
connection, settings and installed daemon remain untouched.

Arm: reopen that fixture with `cirrove-icloud-mounted-write-probe --remove-file`
and its UUID. Before sending Trash, the provider must match the file's account,
collection, parent, item ID, name, ETag, size and full SHA-256 against the
successful upload journal and an independent current iCloud read. It must then
send the conditional Trash request for that exact ID and ETag. The only cloud
mutation permitted by this arm is the recoverable deletion of this one
Cirrove-owned test file.

Endpoint: the separate `os.unlink` process succeeds; the mutation journal has
an applied `RemoveFile` receipt with the exact upload item ID; a complete
independent iCloud parent listing lacks both already-removed test children; the
adapter observes the exact file ID in a complete Trash listing before success.
The isolated FUSE mount must be absent after exit. An uncertain outcome retains
the journal and fixture for inspection, without inferring success from a name
or missing parent entry.

Prediction: the FUSE `unlink` path emits a durable `RemoveFile` intent, the
journal-backed adapter accepts only the already uploaded file and reaches
`Applied`. If FUSE or Apple refuses the operation, the arm fails without
claiming a deletion. One successful arm is functional evidence only; it cannot
establish reliability under network interruption or concurrent edits.

## Result

The live arm passed. The separate process completed `os.unlink`, the shared
worker reached `Applied`, and the validator's independent complete parent
listing lacked both test children. The third mutation receipt named the exact
file ID from the successful upload record. Read-only SQLite reopening found
one upload row and three mutation rows; the mount was absent after exit.
The private manifest is
`.local-state/icloud-mounted-file-trash-control-7df9149b-aca0-4e82-9f8a-82ddccf86276/manifest.json`:
PID 2819348, binary SHA-256
`251c817efb3aaf0568a7879f062413152cf065a65a760b86da2c59ad7faad1cd`,
start `2026-09-28T11:24:00Z`, expected window 360 seconds, with btrfs-backed
`TMPDIR` and `SQLITE_TMPDIR`. No completion timestamp was recorded, so this
supports no duration claim.

## Post-removal remount registered before execution

Question: Can a fresh process reconstruct that both owned test children were
removed, show neither through FUSE or an independent iCloud listing, and leave
the same one-upload/three-mutation journal unchanged? The arm runs
`--resume-after-file-remove` for the same UUID. Its endpoint requires no file
or folder in the mount or independent parent listing, three applied mutation
receipts including the exact removed file ID, and no mount at exit.

Prediction: replaying the applied removal receipts leaves the owned-ID set
empty and permits the fixture root to mount read-only without another cloud
mutation. This is a recovery check, not a repeated deletion attempt.

The post-removal remount passed. The separate application found neither child
through FUSE, and an independent complete iCloud listing found neither child.
The journal still held one upload and three mutations, with no new intent; the
mount was absent after exit. The private manifest is
`.local-state/icloud-mounted-post-file-trash-control-f7f4b560-ffb0-4f4b-a2c0-0914bed53109/manifest.json`:
PID 2823327, binary SHA-256
`43a3c7882f8e4eaea832c8c64f2dcdd682495ed925ebce179de07a6222d36d35`,
start `2026-09-28T11:28:03Z`, expected window 360 seconds, with btrfs-backed
temporary directories. This does not establish multi-client or long-lived
reliability.
